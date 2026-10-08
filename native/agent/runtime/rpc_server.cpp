// 实现单会话 localabstract RPC 监听、长度前缀帧和顺序请求调度。

#include "rpc_server.h"

#include <arpa/inet.h>
#include <array>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <exception>
#include <fcntl.h>
#include <limits>
#include <optional>
#include <poll.h>
#include <span>
#include <string>
#include <string_view>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/un.h>
#include <thread>
#include <unistd.h>
#include <utility>
#include <vector>

#include "agent_log.h"
#include "protocol/json_codec.h"
#include "protocol/protocol_types.h"
#include "runtime.h"
#include "secure_random.h"
#include "secure_memory.h"

namespace azlw::agent {

namespace {

constexpr std::size_t kUnauthenticatedConnectionsBeforeBackoff =
    runtime_rpc_contract::kUnauthenticatedProbeConnections +
    runtime_rpc_contract::kMaximumHandshakeAttempts;
constexpr int kSocketIoTimeoutSeconds = 35;
constexpr int kUnauthenticatedBackoffMilliseconds = 1000;
static_assert(runtime_rpc_contract::kUnauthenticatedProbeConnections == 1);
static_assert(runtime_rpc_contract::kMaximumHandshakeAttempts > 0);
static_assert(
    kMaximumResponseBytes + secure_channel_contract::kProtectedOverheadBytes <=
    std::numeric_limits<std::uint32_t>::max());

/// 确保认证帧在普通返回和异常展开路径上都执行不可省略的清零。
class SensitiveFrameGuard final {
public:
    explicit SensitiveFrameGuard(std::string* frame) noexcept : frame_(frame) {}
    ~SensitiveFrameGuard() { clear(); }

    SensitiveFrameGuard(const SensitiveFrameGuard&) = delete;
    SensitiveFrameGuard& operator=(const SensitiveFrameGuard&) = delete;

    void clear() noexcept {
        if (frame_ == nullptr) {
            return;
        }
        secure_zero(frame_->data(), frame_->size());
        frame_->clear();
    }

private:
    std::string* frame_;
};

/// 已认证明文在本轮请求处理结束后执行不可省略的清零。
class SensitiveBytesGuard final {
public:
    explicit SensitiveBytesGuard(std::vector<std::uint8_t>* bytes) noexcept : bytes_(bytes) {}
    ~SensitiveBytesGuard() { clear(); }

    SensitiveBytesGuard(const SensitiveBytesGuard&) = delete;
    SensitiveBytesGuard& operator=(const SensitiveBytesGuard&) = delete;

    void clear() noexcept {
        if (bytes_ == nullptr) {
            return;
        }
        secure_zero(bytes_->data(), bytes_->size());
        bytes_->clear();
    }

private:
    std::vector<std::uint8_t>* bytes_;
};

std::string_view string_view_of(std::span<const std::uint8_t> bytes) noexcept {
    return {
        reinterpret_cast<const char*>(bytes.data()),
        bytes.size(),
    };
}

std::span<const std::uint8_t> bytes_of(std::string_view value) noexcept {
    return {
        reinterpret_cast<const std::uint8_t*>(value.data()),
        value.size(),
    };
}

}  // namespace

ShutdownConnectionDisposition shutdown_connection_disposition(
    bool preparation_succeeded,
    bool response_delivered) noexcept {
    // 收据是否送达只影响主机判断，不能撤销已经提交的 Prepared 状态。
    static_cast<void>(response_delivered);
    return preparation_succeeded ? ShutdownConnectionDisposition::ParkWorker
                                 : ShutdownConnectionDisposition::CloseAuthenticated;
}

WorkerParkAction worker_park_action(
    bool stop_requested,
    int poll_result,
    int poll_error) noexcept {
    static_cast<void>(poll_result);
    static_cast<void>(poll_error);
    return stop_requested ? WorkerParkAction::Stop : WorkerParkAction::ContinueWaiting;
}

bool configure_rpc_socket_timeouts(int connection, RpcSocketPhase phase) noexcept {
    const timeval receive_timeout{
        .tv_sec = phase == RpcSocketPhase::Handshake ? kSocketIoTimeoutSeconds : 0,
        .tv_usec = 0,
    };
    const timeval send_timeout{.tv_sec = kSocketIoTimeoutSeconds, .tv_usec = 0};
    return setsockopt(
               connection,
               SOL_SOCKET,
               SO_RCVTIMEO,
               &receive_timeout,
               sizeof(receive_timeout)) == 0 &&
           setsockopt(
               connection,
               SOL_SOCKET,
               SO_SNDTIMEO,
               &send_timeout,
               sizeof(send_timeout)) == 0;
}

namespace {

/// 普通断连仍可接受新握手，关闭准备成功后才转入卸载停驻。
enum class ConnectionResult : std::uint8_t {
    Unauthenticated,
    Authenticated,
    ShutdownPrepared,
    ShutdownFailed,
};

/// 处理短读和信号中断，直到固定长度数据完整到达或连接关闭。
bool read_exact(int descriptor, void* destination, std::size_t size) {
    auto* output = static_cast<std::uint8_t*>(destination);
    std::size_t offset = 0;
    while (offset < size) {
        const ssize_t count = recv(descriptor, output + offset, size - offset, 0);
        if (count == 0) {
            return false;
        }
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            return false;
        }
        offset += static_cast<std::size_t>(count);
    }
    return true;
}

/// 处理短写和信号中断，并禁止断连时向进程发送 SIGPIPE。
bool write_exact(int descriptor, const void* source, std::size_t size) {
    const auto* input = static_cast<const std::uint8_t*>(source);
    std::size_t offset = 0;
    while (offset < size) {
        const ssize_t count = send(descriptor, input + offset, size - offset, MSG_NOSIGNAL);
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            return false;
        }
        offset += static_cast<std::size_t>(count);
    }
    return true;
}

/// 先核对网络字节序长度和调用方上限，再为帧体分配内存。
bool read_frame(int descriptor, std::size_t maximum_bytes, std::string* payload) {
    std::uint32_t encoded_length = 0;
    if (!read_exact(descriptor, &encoded_length, sizeof(encoded_length))) {
        return false;
    }
    const std::uint32_t length = ntohl(encoded_length);
    if (length == 0 || length > maximum_bytes) {
        return false;
    }
    payload->resize(length);
    return read_exact(descriptor, payload->data(), payload->size());
}

/// 写入非空且不超过调用方上限的四字节长度前缀帧。
bool write_frame(int descriptor, std::string_view payload, std::size_t maximum_bytes) {
    if (payload.empty() || payload.size() > maximum_bytes ||
        payload.size() > std::numeric_limits<std::uint32_t>::max()) {
        return false;
    }
    const std::uint32_t encoded_length = htonl(static_cast<std::uint32_t>(payload.size()));
    return write_exact(descriptor, &encoded_length, sizeof(encoded_length)) &&
           write_exact(descriptor, payload.data(), payload.size());
}

/// 将明文密封为当前方向的下一个有序帧，并写入长度前缀传输层。
bool write_protected_frame(
    int descriptor,
    secure_channel::SecureChannel* channel,
    std::string plaintext,
    std::size_t maximum_plaintext_bytes) {
    SensitiveFrameGuard plaintext_guard(&plaintext);
    std::vector<std::uint8_t> protected_frame;
    const secure_channel::Error result = channel->outbound().seal(
        bytes_of(plaintext),
        maximum_plaintext_bytes,
        &protected_frame);
    if (result != secure_channel::Error::Ok) {
        log_error("agent.secure_channel", "密封受保护响应失败");
        return false;
    }
    return write_frame(
        descriptor,
        string_view_of(protected_frame),
        maximum_plaintext_bytes + secure_channel_contract::kProtectedOverheadBytes);
}

/// 读取、验证并解密当前方向的下一个有序帧；失败时不交付任何明文。
bool read_protected_frame(
    int descriptor,
    secure_channel::SecureChannel* channel,
    std::size_t maximum_plaintext_bytes,
    std::vector<std::uint8_t>* plaintext) {
    std::string protected_frame;
    if (!read_frame(
            descriptor,
            maximum_plaintext_bytes + secure_channel_contract::kProtectedOverheadBytes,
            &protected_frame)) {
        return false;
    }
    const secure_channel::Error result = channel->inbound().open(
        bytes_of(protected_frame),
        maximum_plaintext_bytes,
        plaintext);
    if (result != secure_channel::Error::Ok) {
        log_error("agent.secure_channel", "受保护请求认证、序号或长度无效");
        return false;
    }
    return true;
}

/// 构造不会改变当前认证会话的稳定 RPC 错误。
AgentError rpc_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.rpc",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

/// 超限成功响应会被替换为有界错误，避免直接中断已认证连接。
bool write_bounded_response(
    int descriptor,
    secure_channel::SecureChannel* channel,
    std::string_view request_id,
    std::string response) {
    if (response.size() > kMaximumResponseBytes) {
        response = encode_error(
            request_id,
            rpc_error("response_too_large", "agent 响应超过 16 MiB 上限"));
    }
    return write_protected_frame(
        descriptor,
        channel,
        std::move(response),
        kMaximumResponseBytes);
}

/// 同时等待监听连接和显式停止事件，避免依赖跨线程 close 唤醒 accept。
int accept_one(int listener, int stop_event, bool* stopping) {
    while (true) {
        pollfd descriptors[2] = {
            {.fd = listener, .events = POLLIN, .revents = 0},
            {.fd = stop_event, .events = POLLIN, .revents = 0},
        };
        const int poll_result = poll(descriptors, 2, -1);
        if (poll_result < 0) {
            if (errno == EINTR) {
                continue;
            }
            return -1;
        }
        if ((descriptors[1].revents & (POLLIN | POLLERR | POLLHUP | POLLNVAL)) != 0) {
            std::uint64_t ignored = 0;
            while (read(stop_event, &ignored, sizeof(ignored)) < 0 && errno == EINTR) {
            }
            *stopping = true;
            return -1;
        }
        if ((descriptors[0].revents & POLLIN) == 0) {
            errno = EBADF;
            return -1;
        }
        const int connection = accept4(listener, nullptr, nullptr, SOCK_CLOEXEC);
        if (connection >= 0) {
            return connection;
        }
        if (errno != EINTR && errno != EAGAIN && errno != EWOULDBLOCK) {
            return -1;
        }
    }
}

/// 读取 `/proc` 第 22 字段，和 TID 一起排除线程号复用。
bool read_thread_start_time(std::int32_t tid, std::uint64_t* start_time) {
    const std::string path = "/proc/self/task/" + std::to_string(tid) + "/stat";
    const int descriptor = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        return false;
    }
    std::array<char, 1024> buffer{};
    const ssize_t count = read(descriptor, buffer.data(), buffer.size() - 1);
    close(descriptor);
    if (count <= 0) {
        return false;
    }
    const std::string_view stat(buffer.data(), static_cast<std::size_t>(count));
    const std::size_t close_parenthesis = stat.rfind(')');
    if (close_parenthesis == std::string_view::npos || close_parenthesis + 2 >= stat.size()) {
        return false;
    }
    std::size_t cursor = close_parenthesis + 2;
    for (int field = 3; field <= 22; ++field) {
        while (cursor < stat.size() && stat[cursor] == ' ') {
            ++cursor;
        }
        const std::size_t end = stat.find(' ', cursor);
        const std::size_t token_end = end == std::string_view::npos ? stat.size() : end;
        if (cursor >= token_end) {
            return false;
        }
        if (field == 22) {
            const auto parsed = std::from_chars(
                stat.data() + cursor, stat.data() + token_end, *start_time);
            return parsed.ec == std::errc{} && parsed.ptr == stat.data() + token_end &&
                   *start_time != 0;
        }
        cursor = token_end;
    }
    return false;
}

/// 一次业务处理的响应，以及关闭准备是否已经提交。
struct RpcDispatchResult final {
    std::string response;
    bool shutdown = false;
    bool preparation_succeeded = false;
};

/// 已解析请求的业务分派。这里不接触连接，关闭收据是否送达由连接层判断。
RpcDispatchResult dispatch_rpc_request(AgentRuntime* runtime, RpcRequest request) {
    RpcDispatchResult result;
    const std::string request_id = request.request_id;
    if (operation_of(request) != Operation::SnapshotShipCatalog) {
        runtime->release_parked_collection_index();
    }
    switch (operation_of(request)) {
        case Operation::Health:
            result.response = encode_health(
                request_id,
                runtime->identity(),
                runtime->main_thread_ready(), runtime->catalog_generation());
            break;
        case Operation::Capabilities:
            result.response = encode_capabilities(
                request_id,
                runtime->main_thread_ready(),
                runtime->bag_read_ready(),
                runtime->owned_state_read_ready(),
                runtime->ship_details_read_ready(),
                runtime->equipment_configs_read_ready(),
                runtime->compose_recipes_read_ready(),
                runtime->equipment_weapons_read_ready(),
                runtime->skill_effects_read_ready(),
                runtime->equipment_reference_names_read_ready());
            break;
        case Operation::SnapshotBag: {
            const auto& body = std::get<SnapshotBagRpcRequest>(request.body);
            const SnapshotExecution execution =
                runtime->execute_snapshot(body.max_items, body.timeout_ms);
            result.response = execution.success
                                  ? encode_snapshot(request_id, execution.snapshot)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::QueryOwned: {
            const auto& body = std::get<QueryOwnedRpcRequest>(request.body);
            const auto execution = runtime->execute_owned_query(body.query, body.timeout_ms);
            result.response = execution.success ? encode_owned_query(request_id, execution)
                                                : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotResources: {
            const auto& body = std::get<SnapshotResourcesRpcRequest>(request.body);
            const auto execution = runtime->execute_resources_snapshot(body.timeout_ms);
            result.response = execution.success ? encode_resources_snapshot(request_id, execution.player)
                                                : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotOwnedState: {
            const auto& body = std::get<SnapshotOwnedStateRpcRequest>(request.body);
            const OwnedStateExecution execution = runtime->execute_owned_state_snapshot(
                body.max_ships,
                body.max_equipments,
                body.max_items,
                body.timeout_ms);
            result.response = execution.success
                                  ? encode_owned_state_snapshot(request_id, execution.snapshot)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotAccountBefore: {
            const auto& body = std::get<SnapshotAccountBeforeRpcRequest>(request.body);
            const AccountBeforeExecution execution = runtime->execute_account_before_snapshot(
                body.max_ships,
                body.max_equipments,
                body.max_items,
                body.timeout_ms);
            result.response = execution.success
                                  ? encode_account_before_snapshot(
                                        request_id,
                                        execution.owned,
                                        execution.details,
                                        execution.dock_frames)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotShipDetails: {
            const auto& body = std::get<SnapshotShipDetailsRpcRequest>(request.body);
            const ShipDetailsExecution execution =
                runtime->execute_ship_details_snapshot(body.max_ships, body.timeout_ms);
            result.response = execution.success
                                  ? encode_ship_details_snapshot(request_id, execution.snapshot)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotShipCatalog: {
            auto& body = std::get<SnapshotShipCatalogRpcRequest>(request.body);
            const ShipCatalogPageExecution execution = runtime->execute_ship_catalog_page(
                std::move(body.table_key),
                body.start_index,
                body.page_size,
                body.timeout_ms);
            result.response = execution.success
                                  ? encode_ship_catalog_page(request_id, execution.page)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotEquipmentConfigs: {
            const auto& body = std::get<SnapshotEquipmentConfigsRpcRequest>(request.body);
            const EquipmentConfigPageExecution execution = runtime->execute_equipment_config_page(
                body.start_index,
                body.page_size,
                body.timeout_ms, body.ids);
            result.response = execution.success
                                  ? encode_equipment_config_page(request_id, execution.page)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotComposeRecipes: {
            const auto& body = std::get<SnapshotComposeRecipesRpcRequest>(request.body);
            const ComposeRecipePageExecution execution = runtime->execute_compose_recipe_page(
                body.start_index,
                body.page_size,
                body.timeout_ms);
            result.response = execution.success
                                  ? encode_compose_recipe_page(request_id, execution.page)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotEquipmentWeapons: {
            auto& body = std::get<SnapshotEquipmentWeaponsRpcRequest>(request.body);
            const EquipmentWeaponBatchExecution execution =
                runtime->execute_equipment_weapon_batch(
                    std::move(body.weapon_ids),
                    body.timeout_ms);
            result.response = execution.success
                                  ? encode_equipment_weapon_batch(request_id, execution.batch)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotSkillEffects: {
            auto& body = std::get<SnapshotSkillEffectsRpcRequest>(request.body);
            const SkillEffectBatchExecution execution = runtime->execute_skill_effect_batch(
                std::move(body.skills),
                body.timeout_ms);
            result.response = execution.success
                                  ? encode_skill_effect_batch(request_id, execution.batch)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::SnapshotEquipmentReferenceNames: {
            auto& body = std::get<SnapshotEquipmentReferenceNamesRpcRequest>(request.body);
            const EquipmentReferenceNameBatchExecution execution =
                runtime->execute_equipment_reference_name_batch(
                    std::move(body.equipment_type_ids),
                    std::move(body.nation_ids),
                    std::move(body.ship_type_ids),
                    std::move(body.attribute_keys),
                    body.timeout_ms);
            result.response = execution.success
                                  ? encode_equipment_reference_name_batch(request_id, execution.batch)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::ExecuteEquipmentCommand: {
            auto& body = std::get<ExecuteEquipmentCommandRpcRequest>(request.body);
            const EquipmentCommandExecution execution = runtime->execute_equipment_command(
                std::move(body.command),
                body.timeout_ms);
            result.response = execution.success
                                  ? encode_equipment_command_receipt(request_id, execution.receipt)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::QueryEquipmentCommand: {
            const auto& body = std::get<QueryEquipmentCommandRpcRequest>(request.body);
            const EquipmentCommandExecution execution =
                runtime->query_equipment_command(body.command_id);
            result.response = execution.success
                                  ? encode_equipment_command_receipt(request_id, execution.receipt)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::CancelEquipmentCommand: {
            const auto& body = std::get<CancelEquipmentCommandRpcRequest>(request.body);
            const EquipmentCommandExecution execution =
                runtime->cancel_equipment_command(body.command_id);
            result.response = execution.success
                                  ? encode_equipment_command_receipt(request_id, execution.receipt)
                                  : encode_error(request_id, execution.error);
            break;
        }
        case Operation::Shutdown: {
            const auto& body = std::get<ShutdownRpcRequest>(request.body);
            const ShutdownPreparation preparation = runtime->prepare_shutdown(body.timeout_ms);
            result.shutdown = true;
            result.preparation_succeeded = preparation.success;
            result.response = preparation.success
                                  ? encode_shutdown_prepared(
                                        request_id,
                                        runtime->identity(),
                                        preparation)
                                  : encode_error(request_id, preparation.error);
            break;
        }
        case Operation::Unsupported: {
            const auto& body = std::get<UnsupportedRpcRequest>(request.body);
            result.response = encode_error(
                request_id,
                rpc_error(
                    "unsupported_operation",
                    "agent 未登记操作 " + body.raw_operation));
            break;
        }
    }
    return result;
}

/// 每次连接使用新的服务端挑战、连接密钥和消息序号，并顺序处理请求。
ConnectionResult serve_connection(int connection, AgentRuntime* runtime) {
    secure_channel::HandshakeNonce server_nonce{};
    std::string handshake_error;
    if (!fill_secure_random(server_nonce, &handshake_error)) {
        log_error("agent.handshake_random", handshake_error);
        return ConnectionResult::Unauthenticated;
    }
    secure_channel::ServerChallenge challenge;
    const bool challenge_created =
        runtime->create_handshake_challenge(server_nonce, &challenge, &handshake_error);
    secure_zero(server_nonce.data(), server_nonce.size());
    if (!challenge_created) {
        log_error("agent.handshake", handshake_error);
        return ConnectionResult::Unauthenticated;
    }
    if (!write_frame(
            connection,
            string_view_of(challenge.bytes()),
            secure_channel_contract::kServerChallengeBytes)) {
        log_error("agent.handshake_io", "服务端挑战发送失败");
        return ConnectionResult::Unauthenticated;
    }

    std::string frame;
    SensitiveFrameGuard frame_guard(&frame);
    if (!read_frame(
            connection,
            secure_channel_contract::kClientProofBytes,
            &frame)) {
        frame_guard.clear();
        log_error("agent.handshake_io", "客户端证明未完整到达");
        return ConnectionResult::Unauthenticated;
    }
    std::optional<secure_channel::SecureChannel> channel;
    bool authenticated = false;
    {
        secure_channel::ChannelKeyMaterial key_material;
        authenticated = runtime->authenticate_handshake(
            challenge,
            bytes_of(frame),
            &key_material,
            &handshake_error);
        if (authenticated) {
            channel.emplace(key_material, secure_channel::EndpointRole::Server);
        }
    }
    frame_guard.clear();
    if (!authenticated) {
        log_error("agent.handshake", handshake_error);
        return ConnectionResult::Unauthenticated;
    }
    if (!configure_rpc_socket_timeouts(connection, RpcSocketPhase::Authenticated)) {
        log_error("agent.socket_timeout", "切换认证后 RPC 连接超时策略失败");
        return ConnectionResult::Authenticated;
    }
    if (!write_protected_frame(
            connection,
            &*channel,
            encode_handshake_ok(runtime->identity()),
            kMaximumResponseBytes)) {
        log_error("agent.handshake_io", "握手响应发送失败");
        return ConnectionResult::Authenticated;
    }

    std::vector<std::uint8_t> plaintext;
    SensitiveBytesGuard plaintext_guard(&plaintext);
    std::uint64_t last_request = 0;
    while (read_protected_frame(
        connection,
        &*channel,
        kMaximumRequestBytes,
        &plaintext)) {
        RpcRequest request;
        AgentError parse_error;
        bool can_respond = false;
        const bool parsed =
            parse_rpc_request(string_view_of(plaintext), &request, &can_respond, &parse_error);
        plaintext_guard.clear();
        if (!parsed) {
            if (!can_respond || !write_protected_frame(
                    connection,
                    &*channel,
                    encode_error(request.request_id, parse_error),
                    kMaximumResponseBytes)) {
                return ConnectionResult::Authenticated;
            }
            continue;
        }
        if (request.request_number <= last_request) {
            const AgentError error = rpc_error(
                "request_id_not_increasing",
                "request_id 必须在同一连接内严格递增且不可重复");
            if (!write_protected_frame(
                    connection,
                    &*channel,
                    encode_error(request.request_id, error),
                    kMaximumResponseBytes)) {
                return ConnectionResult::Authenticated;
            }
            continue;
        }
        last_request = request.request_number;

        const std::string request_id = request.request_id;
        const RpcDispatchResult dispatched =
            dispatch_rpc_request(runtime, std::move(request));
        const bool response_delivered = write_bounded_response(
            connection,
            &*channel,
            request_id,
            dispatched.response);
        if (dispatched.shutdown) {
            if (!response_delivered) {
                log_error("agent.shutdown_io", "关闭收据发送失败");
            }
            return shutdown_connection_disposition(
                       dispatched.preparation_succeeded,
                       response_delivered) == ShutdownConnectionDisposition::ParkWorker
                       ? ConnectionResult::ShutdownPrepared
                       : ConnectionResult::ShutdownFailed;
        }
        if (!response_delivered) {
            return ConnectionResult::Authenticated;
        }
    }
    return ConnectionResult::Authenticated;
}

}  // namespace

// 析构时中断阻塞 I/O，并回收后台线程资源。
RpcServer::~RpcServer() {
    close_listener();
    if (worker_.joinable()) {
        if (worker_.get_id() == std::this_thread::get_id()) {
            worker_.detach();
        } else {
            worker_.join();
        }
    }
    close_descriptors();
}

// 抽象 socket 名仅由固定长度通道标识构成，不落盘也不复用会话身份。
bool RpcServer::bind_channel(const std::string& channel_id, std::string* error) {
    if (listener_.load(std::memory_order_acquire) >= 0 ||
        stop_event_.load(std::memory_order_acquire) >= 0 || channel_id.size() != 32) {
        *error = "localabstract 通道 ID 无效或 socket 已绑定";
        return false;
    }
    const int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (listener < 0) {
        *error = "创建 localabstract socket 失败: " + std::string(std::strerror(errno));
        return false;
    }
    const int stop_event = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (stop_event < 0) {
        const int event_error = errno;
        close(listener);
        *error = "创建 RPC 停止事件失败: " + std::string(std::strerror(event_error));
        return false;
    }
    stop_requested_.store(false, std::memory_order_release);
    listener_.store(listener, std::memory_order_release);
    stop_event_.store(stop_event, std::memory_order_release);

    const std::string& socket_name = channel_id;
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    if (socket_name.size() + 1 > sizeof(address.sun_path)) {
        *error = "localabstract socket 名称过长";
        close_listener();
        return false;
    }
    address.sun_path[0] = '\0';
    std::memcpy(address.sun_path + 1, socket_name.data(), socket_name.size());
    const socklen_t address_size = static_cast<socklen_t>(
        offsetof(sockaddr_un, sun_path) + 1 + socket_name.size());
    if (bind(listener, reinterpret_cast<const sockaddr*>(&address), address_size) != 0) {
        *error = "绑定 localabstract socket 失败: " + std::string(std::strerror(errno));
        close_listener();
        return false;
    }
    if (listen(listener, 1) != 0) {
        *error = "监听 localabstract socket 失败: " + std::string(std::strerror(errno));
        close_listener();
        return false;
    }
    return true;
}

// 后台线程由对象保持 join 所有权；自线程卸载时只释放 std::thread 句柄。
bool RpcServer::start(AgentRuntime* runtime, std::string* error) {
    if (listener_.load(std::memory_order_acquire) < 0 ||
        stop_event_.load(std::memory_order_acquire) < 0 ||
        stop_requested_.load(std::memory_order_acquire) || runtime == nullptr ||
        worker_.joinable()) {
        *error = "RPC server 尚未绑定或 runtime 为空";
        return false;
    }
    try {
        worker_parked_.store(false, std::memory_order_release);
        worker_ = std::thread([this, runtime] { run(runtime); });
        return true;
    } catch (const std::exception& exception) {
        *error = "创建 RPC server 线程失败: " + std::string(exception.what());
        return false;
    }
}

// 串行服务驻留会话的连接；认证失败限速，显式关闭准备后停驻为唯一卸载载体。
void RpcServer::run(AgentRuntime* runtime) noexcept {
    const auto tid = static_cast<std::int32_t>(syscall(SYS_gettid));
    std::uint64_t start_time = 0;
    if (tid > 0 && read_thread_start_time(tid, &start_time)) {
        worker_tid_.store(tid, std::memory_order_release);
        worker_start_time_.store(start_time, std::memory_order_release);
    } else {
        log_error("agent.worker_identity", "读取 RPC 工作线程身份失败");
    }

    ConnectionResult terminal_result = ConnectionResult::Unauthenticated;
    std::size_t unauthenticated_connections = 0;
    while (true) {
        const int listener = listener_.load(std::memory_order_acquire);
        const int stop_event = stop_event_.load(std::memory_order_acquire);
        if (listener < 0 || stop_event < 0 || stop_requested_.load(std::memory_order_acquire)) {
            break;
        }
        if (unauthenticated_connections >= kUnauthenticatedConnectionsBeforeBackoff) {
            // 连续失败只延缓下一次握手，不耗尽驻留会话；停止事件可立即唤醒退避。
            const auto deadline = std::chrono::steady_clock::now() +
                                  std::chrono::milliseconds(kUnauthenticatedBackoffMilliseconds);
            pollfd descriptor{.fd = stop_event, .events = POLLIN, .revents = 0};
            int poll_result = 0;
            do {
                const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
                    deadline - std::chrono::steady_clock::now()).count();
                if (remaining <= 0) {
                    poll_result = 0;
                    break;
                }
                poll_result = poll(&descriptor, 1, static_cast<int>(remaining));
            } while (poll_result < 0 && errno == EINTR);
            if (poll_result != 0 || stop_requested_.load(std::memory_order_acquire)) {
                if (poll_result < 0) {
                    log_error("agent.handshake_backoff", "等待握手退避失败: " +
                              std::string(std::strerror(errno)));
                }
                break;
            }
        }
        bool stopping = false;
        const int connection = accept_one(listener, stop_event, &stopping);
        if (connection < 0) {
            if (!stopping && !stop_requested_.load(std::memory_order_acquire)) {
                log_error("agent.accept", "接受连接失败: " + std::string(std::strerror(errno)));
            }
            break;
        }
        {
            std::lock_guard connection_lock(connection_mutex_);
            if (stop_requested_.load(std::memory_order_acquire)) {
                close(connection);
                break;
            }
            active_connection_ = connection;
        }
        ConnectionResult result = ConnectionResult::Unauthenticated;
        if (!configure_rpc_socket_timeouts(connection, RpcSocketPhase::Handshake)) {
            log_error("agent.socket_timeout", "设置 RPC 握手连接读写超时失败");
        } else {
            result = serve_connection(connection, runtime);
        }
        {
            std::lock_guard connection_lock(connection_mutex_);
            if (active_connection_ == connection) {
                close(connection);
                active_connection_ = -1;
            }
        }
        runtime->release_parked_collection_index();
        if (result == ConnectionResult::ShutdownPrepared ||
            result == ConnectionResult::ShutdownFailed) {
            terminal_result = result;
            break;
        }
        if (result == ConnectionResult::Authenticated) {
            unauthenticated_connections = 0;
        } else if (unauthenticated_connections < kUnauthenticatedConnectionsBeforeBackoff) {
            ++unauthenticated_connections;
        }
    }
    const int listener = listener_.exchange(-1, std::memory_order_acq_rel);
    if (listener >= 0) {
        close(listener);
    }
    runtime->clear_session_secret();
    if (terminal_result != ConnectionResult::ShutdownPrepared) {
        return;
    }

    worker_parked_.store(true, std::memory_order_release);
    bool unexpected_wait_logged = false;
    while (true) {
        pollfd descriptor{
            .fd = stop_event_.load(std::memory_order_acquire),
            .events = POLLIN,
            .revents = 0,
        };
        errno = 0;
        const int poll_result = descriptor.fd < 0 ? -1 : poll(&descriptor, 1, -1);
        const int poll_error = descriptor.fd < 0 ? EBADF : errno;
        const bool stop_requested = stop_requested_.load(std::memory_order_acquire);
        if (worker_park_action(stop_requested, poll_result, poll_error) ==
            WorkerParkAction::Stop) {
            break;
        }
        const bool unexpected_wait = descriptor.fd < 0 || poll_result > 0 ||
                                     (poll_result < 0 && poll_error != EINTR);
        if (unexpected_wait && !unexpected_wait_logged) {
            log_error(
                "agent.worker_park",
                "RPC 卸载载体在未收到停止请求时被意外唤醒，继续保持停驻");
            unexpected_wait_logged = true;
        }
        if (unexpected_wait) {
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
        }
    }
    worker_parked_.store(false, std::memory_order_release);
}

// 幂等请求停止监听，并在互斥所有权内唤醒活动连接上的阻塞读写。
void RpcServer::close_listener() noexcept {
    stop_requested_.store(true, std::memory_order_release);
    const int stop_event = stop_event_.load(std::memory_order_acquire);
    if (stop_event >= 0) {
        const std::uint64_t signal = 1;
        ssize_t written = 0;
        do {
            written = write(stop_event, &signal, sizeof(signal));
        } while (written < 0 && errno == EINTR);
    }
    {
        std::lock_guard connection_lock(connection_mutex_);
        if (active_connection_ >= 0) {
            shutdown(active_connection_, SHUT_RDWR);
        }
    }
    if (!worker_.joinable()) {
        close_descriptors();
    }
}

void RpcServer::close_descriptors() noexcept {
    const int listener = listener_.exchange(-1, std::memory_order_acq_rel);
    if (listener >= 0) {
        close(listener);
    }
    const int stop_event = stop_event_.exchange(-1, std::memory_order_acq_rel);
    if (stop_event >= 0) {
        close(stop_event);
    }
}

bool RpcServer::worker_parked() const noexcept {
    return worker_parked_.load(std::memory_order_acquire);
}

std::int32_t RpcServer::worker_tid() const noexcept {
    return worker_tid_.load(std::memory_order_acquire);
}

std::uint64_t RpcServer::worker_start_time() const noexcept {
    return worker_start_time_.load(std::memory_order_acquire);
}

}  // namespace azlw::agent

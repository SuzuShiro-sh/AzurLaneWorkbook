// 验证 RPC 顺序重连、认证隔离、失败退避和显式停止的有界回收。

#include <atomic>
#include <cerrno>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <system_error>
#include <thread>
#include <vector>

#include <arpa/inet.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#include "runtime/rpc_server.h"
#include "runtime_rpc_contract.h"
#include "runtime/runtime.h"
#include "secure_channel.h"

namespace azlw::agent {
/// 注入只读采集回调，验证跨调用复用、代次失效和失败不入缓存。
struct AgentRuntimeCacheTest {
    static void verify() {
        auto& runtime = AgentRuntime::instance();
        std::size_t reads = 0;
        auto read = [&] { ++reads; EquipmentConfigPageExecution result; result.success = true; result.page.complete = true; return result; };
        auto size = [](const auto& result) -> std::size_t { return result.success ? 32 : 0; };
        runtime.read_static_cached("test:equipment", read, size);
        runtime.read_static_cached("test:equipment", read, size);
        if (reads != 1) throw std::runtime_error("重复静态请求没有复用结果");
        runtime.catalog_generation_.fetch_add(1);
        runtime.read_static_cached("test:equipment", read, size);
        if (reads != 2) throw std::runtime_error("Lua 代次变化没有使静态缓存失效");
        auto failure = [&] { ++reads; return EquipmentConfigPageExecution{}; };
        runtime.read_static_cached("test:failure", failure, size);
        runtime.read_static_cached("test:failure", failure, size);
        if (reads != 4) throw std::runtime_error("失败结果被缓存");
        runtime.static_cache_bytes_ = 64 * 1024 * 1024;
        runtime.read_static_cached("test:capacity", read, size);
        runtime.read_static_cached("test:capacity", read, size);
        if (reads != 6) throw std::runtime_error("静态缓存超过保留容量");
        runtime.static_cache_.clear();
        runtime.static_cache_bytes_ = 0;
    }
};
}

namespace {

/// 失败时保留稳定测试上下文，避免无说明地终止设备测试进程。
void require(bool condition, const std::string& message) {
    if (!condition) {
        throw std::runtime_error(message);
    }
}

/// 完整发送指定长度，供测试帧在 socket 短写时继续发送。
void send_exact(
    int descriptor,
    const void* buffer,
    std::size_t size,
    const std::string& error) {
    const auto* bytes = static_cast<const std::uint8_t*>(buffer);
    std::size_t offset = 0;
    while (offset < size) {
        const ssize_t count =
            send(descriptor, bytes + offset, size - offset, MSG_NOSIGNAL);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        require(count > 0, error);
        offset += static_cast<std::size_t>(count);
    }
}

/// 完整读取指定长度，供测试帧在 socket 短读时继续收取。
void receive_exact(
    int descriptor,
    void* buffer,
    std::size_t size,
    const std::string& error) {
    auto* bytes = static_cast<std::uint8_t*>(buffer);
    std::size_t offset = 0;
    while (offset < size) {
        const ssize_t count = recv(descriptor, bytes + offset, size - offset, 0);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        require(count > 0, error);
        offset += static_cast<std::size_t>(count);
    }
}

/// 发送四字节网络序长度前缀和非空 JSON 帧。
void write_test_frame(int descriptor, const std::string& payload) {
    require(!payload.empty(), "RPC 测试请求为空");
    std::uint32_t length = htonl(static_cast<std::uint32_t>(payload.size()));
    send_exact(
        descriptor,
        &length,
        sizeof(length),
        "发送 RPC 测试帧长度失败");
    send_exact(
        descriptor,
        payload.data(),
        payload.size(),
        "发送 RPC 测试帧正文失败");
}

/// 读取服务端响应帧，并限制测试夹具可接受的响应大小。
std::string read_test_frame(int descriptor) {
    std::uint32_t encoded_length = 0;
    receive_exact(
        descriptor,
        &encoded_length,
        sizeof(encoded_length),
        "读取 RPC 测试帧长度失败");
    const std::uint32_t length = ntohl(encoded_length);
    require(length > 0 && length <= 4096, "RPC 测试响应长度无效");
    std::string payload(length, '\0');
    receive_exact(
        descriptor,
        payload.data(),
        payload.size(),
        "读取 RPC 测试帧正文失败");
    return payload;
}

/// 读取内核实际采用的 socket 超时，避免测试只覆盖策略分支而没有覆盖系统调用。
timeval socket_timeout(int descriptor, int option) {
    timeval timeout{};
    socklen_t size = sizeof(timeout);
    require(
        getsockopt(descriptor, SOL_SOCKET, option, &timeout, &size) == 0,
        "读取 RPC socket 超时失败");
    require(size == sizeof(timeout), "RPC socket 超时返回长度异常");
    return timeout;
}

/// 握手阶段限制空闲读写；认证完成后只限制写入，不中断正常业务空闲。
void verify_socket_timeout_phases() {
    int descriptors[2] = {-1, -1};
    require(
        socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, descriptors) == 0,
        "创建 RPC 超时策略测试 socketpair 失败");

    require(
        azlw::agent::configure_rpc_socket_timeouts(
            descriptors[0], azlw::agent::RpcSocketPhase::Handshake),
        "设置 RPC 握手超时策略失败");
    const timeval handshake_receive = socket_timeout(descriptors[0], SO_RCVTIMEO);
    const timeval handshake_send = socket_timeout(descriptors[0], SO_SNDTIMEO);
    require(
        handshake_receive.tv_sec == 35 && handshake_receive.tv_usec == 0,
        "RPC 握手接收超时不是 35 秒");
    require(
        handshake_send.tv_sec == 35 && handshake_send.tv_usec == 0,
        "RPC 握手发送超时不是 35 秒");

    require(
        azlw::agent::configure_rpc_socket_timeouts(
            descriptors[0], azlw::agent::RpcSocketPhase::Authenticated),
        "设置 RPC 认证后超时策略失败");
    const timeval authenticated_receive = socket_timeout(descriptors[0], SO_RCVTIMEO);
    const timeval authenticated_send = socket_timeout(descriptors[0], SO_SNDTIMEO);
    require(
        authenticated_receive.tv_sec == 0 && authenticated_receive.tv_usec == 0,
        "RPC 认证后仍存在接收空闲超时");
    require(
        authenticated_send.tv_sec == 35 && authenticated_send.tv_usec == 0,
        "RPC 认证后发送超时不是 35 秒");

    close(descriptors[0]);
    close(descriptors[1]);
}

/// 认证后的无限期接收仍必须能被服务器采用的 shutdown 机制立即唤醒。
void verify_authenticated_idle_read_is_interruptible() {
    int descriptors[2] = {-1, -1};
    require(
        socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, descriptors) == 0,
        "创建 RPC 认证后空闲测试 socketpair 失败");
    require(
        azlw::agent::configure_rpc_socket_timeouts(
            descriptors[0], azlw::agent::RpcSocketPhase::Authenticated),
        "设置 RPC 认证后空闲测试策略失败");

    std::atomic<bool> receive_finished{false};
    ssize_t receive_result = 1;
    std::thread receiver([&] {
        char value = 0;
        receive_result = recv(descriptors[0], &value, sizeof(value), 0);
        receive_finished.store(true, std::memory_order_release);
    });
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
    const bool finished_before_shutdown =
        receive_finished.load(std::memory_order_acquire);
    const int shutdown_result = shutdown(descriptors[0], SHUT_RDWR);
    receiver.join();

    close(descriptors[0]);
    close(descriptors[1]);
    require(!finished_before_shutdown, "RPC 认证后空闲接收在显式停止前意外返回");
    require(shutdown_result == 0, "中断 RPC 认证后空闲接收失败");
    require(receive_result <= 0, "RPC 认证后空闲接收没有被显式停止中断");
}

/// 连接指定抽象 socket，使停止路径同时覆盖活动连接上的阻塞读取。
int connect_abstract(const std::string& session_id) {
    const int descriptor = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    require(descriptor >= 0, "创建 RPC 生命周期客户端失败");
    const std::string socket_name = session_id;
    sockaddr_un address{};
    address.sun_family = AF_UNIX;
    address.sun_path[0] = '\0';
    std::memcpy(address.sun_path + 1, socket_name.data(), socket_name.size());
    const socklen_t address_size = static_cast<socklen_t>(
        offsetof(sockaddr_un, sun_path) + 1 + socket_name.size());
    if (connect(descriptor, reinterpret_cast<const sockaddr*>(&address), address_size) != 0) {
        const int connect_error = errno;
        close(descriptor);
        throw std::system_error(connect_error, std::generic_category(), "连接 RPC 生命周期测试会话失败");
    }
    const timeval receive_timeout{.tv_sec = 2, .tv_usec = 0};
    if (setsockopt(
            descriptor,
            SOL_SOCKET,
            SO_RCVTIMEO,
            &receive_timeout,
            sizeof(receive_timeout)) != 0) {
        close(descriptor);
        throw std::runtime_error("设置 RPC 生命周期客户端接收超时失败");
    }
    return descriptor;
}

/// 使用零初始化运行态夹具完成真实挑战证明，确保服务端已经进入认证后空闲读取。
std::unique_ptr<azlw::secure_channel::SecureChannel> authenticate_fixture_connection(
    int descriptor,
    std::string* recorded_challenge = nullptr,
    std::string* recorded_proof = nullptr) {
    const std::string challenge_frame = read_test_frame(descriptor);
    if (recorded_challenge != nullptr) {
        *recorded_challenge = challenge_frame;
    }
    azlw::secure_channel::SessionId session_id{};
    azlw::secure_channel::SessionSecret session_secret{};
    azlw::secure_channel::VerifiedServerChallenge challenge;
    require(
        azlw::secure_channel::verify_server_challenge(
            session_id,
            session_secret,
            std::span<const std::uint8_t>(
                reinterpret_cast<const std::uint8_t*>(challenge_frame.data()),
                challenge_frame.size()),
            &challenge) == azlw::secure_channel::Error::Ok,
        "RPC 生命周期夹具无法验证服务端挑战");
    azlw::secure_channel::HandshakeNonce client_nonce{};
    client_nonce.fill(0x44);
    azlw::secure_channel::ClientProofBytes proof{};
    azlw::secure_channel::ChannelKeyMaterial keys;
    require(
        azlw::secure_channel::answer_server_challenge(
            challenge,
            session_secret,
            client_nonce,
            &proof,
            &keys) == azlw::secure_channel::Error::Ok,
        "RPC 生命周期夹具无法构造客户端证明");
    const std::string proof_frame(reinterpret_cast<const char*>(proof.data()), proof.size());
    if (recorded_proof != nullptr) {
        *recorded_proof = proof_frame;
    }
    write_test_frame(descriptor, proof_frame);
    auto channel = std::make_unique<azlw::secure_channel::SecureChannel>(
        keys,
        azlw::secure_channel::EndpointRole::Client);
    const std::string protected_response = read_test_frame(descriptor);
    std::vector<std::uint8_t> response_bytes;
    require(
        channel->inbound().open(
            std::span<const std::uint8_t>(
                reinterpret_cast<const std::uint8_t*>(protected_response.data()),
                protected_response.size()),
            4096,
            &response_bytes) == azlw::secure_channel::Error::Ok,
        "RPC 生命周期夹具无法验证握手响应");
    const std::string response(
        reinterpret_cast<const char*>(response_bytes.data()),
        response_bytes.size());
    require(
        response.find(R"("message_type":"handshake_ok")") != std::string::npos,
        "RPC 生命周期夹具没有收到握手成功响应");
    return channel;
}

/// 被拒绝的证明和密文必须关闭当前连接，不能误作为后续连接的请求。
void require_connection_closed(int descriptor) {
    char byte = 0;
    ssize_t received = 0;
    do {
        received = recv(descriptor, &byte, sizeof(byte), 0);
    } while (received < 0 && errno == EINTR);
    require(received == 0 || (received < 0 && errno == ECONNRESET), "无效重放未关闭连接");
}

/// 在真实加密连接上复用首个请求编号；新连接必须重新建立序号空间。
std::string query_fixture_health(int descriptor, azlw::secure_channel::SecureChannel* channel) {
    const std::string request =
        R"({"protocol_version":1,"request_id":"0000000000000001","operation":"health","timeout_ms":5000,"payload":{}})";
    std::vector<std::uint8_t> sealed;
    require(channel->outbound().seal(
        std::span<const std::uint8_t>(reinterpret_cast<const std::uint8_t*>(request.data()), request.size()),
        4096, &sealed) == azlw::secure_channel::Error::Ok, "加密 health 请求失败");
    const std::string frame(reinterpret_cast<const char*>(sealed.data()), sealed.size());
    write_test_frame(descriptor, frame);
    const std::string response = read_test_frame(descriptor);
    std::vector<std::uint8_t> plaintext;
    require(channel->inbound().open(
        std::span<const std::uint8_t>(reinterpret_cast<const std::uint8_t*>(response.data()), response.size()),
        4096, &plaintext) == azlw::secure_channel::Error::Ok, "解密 health 响应失败");
    const std::string json(reinterpret_cast<const char*>(plaintext.data()), plaintext.size());
    require(json.find(R"("status":"ok")") != std::string::npos, "重连后的首个 health 请求失败");
    return frame;
}

/// 发送与宿主一致的认证前超限帧，并等待服务端明确关闭连接。
void probe_oversized_unauthenticated_frame(const std::string& session_id) {
    const int connection = connect_abstract(session_id);
    static_cast<void>(read_test_frame(connection));
    const std::uint32_t oversized_length = htonl(
        static_cast<std::uint32_t>(azlw::runtime_rpc_contract::kMaximumRequestBytes + 1));
    send_exact(
        connection,
        &oversized_length,
        sizeof(oversized_length),
        "发送认证前超限帧头失败");

    char response_byte = 0;
    ssize_t received = -1;
    do {
        received = recv(connection, &response_byte, sizeof(response_byte), 0);
    } while (received < 0 && errno == EINTR);
    const bool closed = received == 0 ||
                        (received < 0 &&
                         (errno == ECONNABORTED || errno == ECONNRESET || errno == EPIPE));
    close(connection);
    require(closed, "Agent 未在认证前超限帧后关闭连接");
}

/// 读取挑战后主动断开，制造宿主允许重试的握手传输失败。
void abandon_handshake_connection(const std::string& session_id) {
    const int connection = connect_abstract(session_id);
    static_cast<void>(read_test_frame(connection));
    close(connection);
}

/// 启动服务并等待工作线程公布内核身份。
void start_server(azlw::agent::RpcServer* server, const std::string& session_id) {
    std::string error;
    require(server->bind_channel(session_id, &error), "绑定 RPC 生命周期测试通道失败: " + error);
    require(
        server->start(&azlw::agent::AgentRuntime::instance(), &error),
        "启动 RPC 生命周期测试线程失败: " + error);
    const auto worker_deadline =
        std::chrono::steady_clock::now() + std::chrono::seconds(2);
    while (server->worker_tid() <= 0 && std::chrono::steady_clock::now() < worker_deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    require(server->worker_tid() > 0, "RPC 生命周期测试线程未公布 TID");
}

/// 认证前诊断连接不得挤占正式握手的完整重试预算。
void verify_probe_preserves_handshake_retry_budget() {
    static_assert(azlw::runtime_rpc_contract::kUnauthenticatedProbeConnections == 1);
    static_assert(azlw::runtime_rpc_contract::kMaximumHandshakeAttempts > 0);

    azlw::agent::RpcServer server;
    const std::string session_id = "102132435465768798a9bacbdcedfe0f";
    start_server(&server, session_id);
    probe_oversized_unauthenticated_frame(session_id);
    for (std::uint32_t attempt = 1;
         attempt < azlw::runtime_rpc_contract::kMaximumHandshakeAttempts;
         ++attempt) {
        abandon_handshake_connection(session_id);
    }
    const int connection = connect_abstract(session_id);
    authenticate_fixture_connection(connection);
    server.close_listener();
    close(connection);
}

/// 已认证断连后保持同一工作线程，旧连接的证明和密文均不能重放。
void verify_reconnect_and_replay_isolation() {
    azlw::agent::RpcServer server;
    const std::string session_id = "112132435465768798a9bacbdcedfe0f";
    start_server(&server, session_id);
    const auto worker_tid = server.worker_tid();
    const auto worker_start = server.worker_start_time();
    int connection = connect_abstract(session_id);
    std::string first_challenge;
    std::string first_proof;
    auto first_channel = authenticate_fixture_connection(connection, &first_challenge, &first_proof);
    const std::string old_request = query_fixture_health(connection, first_channel.get());
    close(connection);

    connection = connect_abstract(session_id);
    require(read_test_frame(connection) != first_challenge, "重连复用了服务端挑战");
    write_test_frame(connection, first_proof);
    require_connection_closed(connection);
    close(connection);

    connection = connect_abstract(session_id);
    auto second_channel = authenticate_fixture_connection(connection);
    // 在本连接首条消息处重放，保证拒绝原因是密钥隔离而不是已消费的序号。
    write_test_frame(connection, old_request);
    require_connection_closed(connection);
    close(connection);

    connection = connect_abstract(session_id);
    auto third_channel = authenticate_fixture_connection(connection);
    query_fixture_health(connection, third_channel.get());
    require(server.worker_tid() == worker_tid && server.worker_start_time() == worker_start,
            "重连改变了 RPC 工作线程身份");
    require(!server.worker_parked(), "普通断连错误进入了卸载停驻");
    server.close_listener();
    close(connection);
}

/// 连续认证失败触发退避后仍能恢复，退避过程中显式停止仍有界完成。
void verify_authentication_failure_backoff() {
    const auto failures = azlw::runtime_rpc_contract::kUnauthenticatedProbeConnections +
                          azlw::runtime_rpc_contract::kMaximumHandshakeAttempts;
    {
        azlw::agent::RpcServer server;
        const std::string session_id = "122132435465768798a9bacbdcedfe0f";
        start_server(&server, session_id);
        for (std::uint32_t index = 0; index <= failures; ++index) {
            probe_oversized_unauthenticated_frame(session_id);
        }
        const int connection = connect_abstract(session_id);
        auto channel = authenticate_fixture_connection(connection);
        query_fixture_health(connection, channel.get());
        server.close_listener();
        close(connection);
    }
    const auto stopped = [&] {
        azlw::agent::RpcServer server;
        const std::string session_id = "132132435465768798a9bacbdcedfe0f";
        start_server(&server, session_id);
        for (std::uint32_t index = 0; index < failures; ++index) {
            probe_oversized_unauthenticated_frame(session_id);
        }
        const auto started = std::chrono::steady_clock::now();
        server.close_listener();
        return started;
    }();
    require(std::chrono::steady_clock::now() - stopped < std::chrono::milliseconds(500),
            "停止事件未能立即中断握手退避");
}

/// 尚未启动的运行态拒绝关闭准备后，服务必须终止而不能重新声明连接可用。
void verify_failed_shutdown_ends_listener() {
    azlw::agent::RpcServer server;
    const std::string session_id = "142132435465768798a9bacbdcedfe0f";
    start_server(&server, session_id);
    const int connection = connect_abstract(session_id);
    auto channel = authenticate_fixture_connection(connection);
    const std::string request =
        R"({"protocol_version":1,"request_id":"0000000000000001","operation":"shutdown","timeout_ms":5000,"payload":{}})";
    std::vector<std::uint8_t> sealed;
    require(channel->outbound().seal(
        std::span<const std::uint8_t>(reinterpret_cast<const std::uint8_t*>(request.data()), request.size()),
        4096, &sealed) == azlw::secure_channel::Error::Ok, "加密 shutdown 请求失败");
    write_test_frame(connection, std::string(reinterpret_cast<const char*>(sealed.data()), sealed.size()));
    const std::string response = read_test_frame(connection);
    std::vector<std::uint8_t> plaintext;
    require(channel->inbound().open(
        std::span<const std::uint8_t>(reinterpret_cast<const std::uint8_t*>(response.data()), response.size()),
        4096, &plaintext) == azlw::secure_channel::Error::Ok, "解密 shutdown 响应失败");
    const std::string json(reinterpret_cast<const char*>(plaintext.data()), plaintext.size());
    require(json.find(R"("status":"error")") != std::string::npos, "未启动运行态没有拒绝关闭准备");
    require_connection_closed(connection);
    close(connection);
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(1);
    while (true) {
        int retry = -1;
        try {
            retry = connect_abstract(session_id);
        } catch (const std::system_error& error) {
            require(error.code().value() == ECONNREFUSED, "关闭监听检查发生非预期 socket 错误");
            break;
        }
        close(retry);
        require(std::chrono::steady_clock::now() < deadline, "关闭准备失败后仍在监听连接");
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    require(!server.worker_parked(), "关闭准备失败后错误进入卸载停驻");
}

/// 真实制造 socket 写回失败，并确认已提交的关闭准备仍要求工作线程停驻。
void verify_failed_shutdown_receipt_still_parks() {
    int descriptors[2] = {-1, -1};
    require(
        socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, descriptors) == 0,
        "创建关闭收据失败测试 socketpair 失败");
    close(descriptors[1]);
    descriptors[1] = -1;
    const char payload[] = "shutdown-prepared";
    const bool response_delivered =
        send(descriptors[0], payload, sizeof(payload), MSG_NOSIGNAL) >= 0;
    close(descriptors[0]);

    require(!response_delivered, "关闭收据失败测试没有制造出写回失败");
    require(
        azlw::agent::shutdown_connection_disposition(true, response_delivered) ==
            azlw::agent::ShutdownConnectionDisposition::ParkWorker,
        "关闭准备成功后不应因收据写回失败而跳过工作线程停驻");
    require(
        azlw::agent::shutdown_connection_disposition(false, response_delivered) ==
            azlw::agent::ShutdownConnectionDisposition::CloseAuthenticated,
        "关闭准备失败时不应停驻工作线程");
}

/// 意外唤醒、可重试中断和永久 poll 错误都不能让载体在 loader 接管前返回。
void verify_worker_park_requires_explicit_stop() {
    using azlw::agent::WorkerParkAction;
    using azlw::agent::worker_park_action;
    require(
        worker_park_action(false, 1, 0) == WorkerParkAction::ContinueWaiting,
        "意外 poll 事件不应释放卸载载体");
    require(
        worker_park_action(false, -1, EINTR) == WorkerParkAction::ContinueWaiting,
        "EINTR 不应释放卸载载体");
    require(
        worker_park_action(false, -1, EBADF) == WorkerParkAction::ContinueWaiting,
        "poll 永久错误不应释放卸载载体");
    require(
        worker_park_action(true, 0, 0) == WorkerParkAction::Stop,
        "显式停止请求必须允许卸载载体有界回收");
}

}  // namespace

/// 分别验证空闲 listener 和已认证活动连接都能由显式停止事件有界回收。
int main() {
    try {
        azlw::agent::AgentRuntimeCacheTest::verify();
        verify_reconnect_and_replay_isolation();
        verify_authentication_failure_backoff();
        verify_failed_shutdown_ends_listener();
        const auto started = std::chrono::steady_clock::now();
        verify_socket_timeout_phases();
        verify_authenticated_idle_read_is_interruptible();
        verify_failed_shutdown_receipt_still_parks();
        verify_worker_park_requires_explicit_stop();
        verify_probe_preserves_handshake_retry_budget();
        {
            azlw::agent::RpcServer server;
            start_server(&server, "00112233445566778899aabbccddeeff");
            server.close_listener();
        }
        {
            azlw::agent::RpcServer server;
            const std::string session_id = "ffeeddccbbaa99887766554433221100";
            start_server(&server, session_id);
            const int connection = connect_abstract(session_id);
            authenticate_fixture_connection(connection);
            server.close_listener();
            close(connection);
        }
        const auto elapsed = std::chrono::steady_clock::now() - started;
        require(elapsed < std::chrono::seconds(3), "RPC listener 停止和线程回收超过 3 秒");
        std::cout << "PASS agent_rpc_lifecycle_test\n";
        return 0;
    } catch (const std::exception& exception) {
        std::cerr << "FAIL agent_rpc_lifecycle_test: " << exception.what() << '\n';
        return 1;
    }
}

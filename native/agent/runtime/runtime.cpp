// 实现 agent 启动事务、tolua_update Hook、主线程任务队列和装备命令观察。

#include "runtime.h"

#include <algorithm>
#include <array>
#include <cerrno>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <exception>
#include <fcntl.h>
#include <limits>
#include <string>
#include <type_traits>
#include <variant>
#include <sys/types.h>
#include <unistd.h>
#include <utility>

#include "agent_log.h"
#include "lua/lua_reader.h"
#include "bootstrap_validation.h"
#include "protocol/handshake.h"
#include "protocol/json_codec.h"
#include "rpc_server.h"
#include "secure_memory.h"

namespace azlw::agent {
namespace {

/// 冻结 profile 对应的原始 `tolua_update` 函数签名。
using ToluaUpdate = int (*)(lua_State* state, float delta_time, float unscaled_delta_time);

/// 构造不会自动重试且不改变会话状态的队列错误。
AgentError queue_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.queue",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

/// 关闭开始后所有队列请求都必须封闭当前 RPC 会话。
AgentError stopping_error(std::string message) {
    return AgentError{
        .code = "session_stopping",
        .stage = "agent.shutdown",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "must_close",
    };
}

/// 记录每个仍可能执行 Agent 代码或 trampoline 的 Hook 回调。
class ActiveCallback final {
public:
    explicit ActiveCallback(std::atomic<std::uint32_t>* counter) : counter_(counter) {
        counter_->fetch_add(1, std::memory_order_acq_rel);
    }
    ~ActiveCallback() {
        counter_->fetch_sub(1, std::memory_order_acq_rel);
    }

    ActiveCallback(const ActiveCallback&) = delete;
    ActiveCallback& operator=(const ActiveCallback&) = delete;

private:
    std::atomic<std::uint32_t>* counter_;
};

/// 从当前进程 cmdline 读取 Android 包名，用于二次核对注入目标。
bool read_process_name(std::string* name, std::string* error) {
    const int descriptor = open("/proc/self/cmdline", O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        *error = "打开 /proc/self/cmdline 失败: " + std::string(std::strerror(errno));
        return false;
    }
    std::array<char, 256> buffer{};
    const ssize_t count = read(descriptor, buffer.data(), buffer.size() - 1);
    const int close_result = close(descriptor);
    if (count <= 0) {
        *error = "读取 /proc/self/cmdline 失败或为空";
        return false;
    }
    if (close_result != 0) {
        *error = "关闭 /proc/self/cmdline 失败: " + std::string(std::strerror(errno));
        return false;
    }
    name->assign(buffer.data());
    return true;
}

/// 返回 Hook 入口的整数地址，定义位于导出函数之后。
std::uintptr_t hook_entry_address();

}  // namespace

/// 作为机器码跳转目标，将调用转入进程唯一运行态实例。
extern "C" __attribute__((visibility("hidden"))) int azlw_tolua_update_hook(
    lua_State* state,
    float delta_time,
    float unscaled_delta_time) noexcept {
    return AgentRuntime::instance().invoke_tolua_update(state, delta_time, unscaled_delta_time);
}

namespace {

/// 通过字节复制取得函数地址，避免依赖函数指针强制转换行为。
std::uintptr_t hook_entry_address() {
    ToluaUpdate entry = &azlw_tolua_update_hook;
    std::uintptr_t address = 0;
    static_assert(sizeof(entry) == sizeof(address));
    std::memcpy(&address, &entry, sizeof(address));
    return address;
}

}  // namespace

// 函数内静态对象保证进程内只有一个资源所有者。
AgentRuntime& AgentRuntime::instance() {
    static AgentRuntime runtime;
    return runtime;
}

// 先停止并回收 RPC，确保线程不再访问会话凭据、任务队列和命令账本。
AgentRuntime::~AgentRuntime() {
    server_.reset();
    clear_session_secret();
}

// 按身份、模块、符号、socket、Hook 和线程顺序提交启动事务。
AgentStartCode AgentRuntime::start(const BootstrapConfigV2& config) noexcept {
    StartState expected = StartState::NotStarted;
    if (!start_state_.compare_exchange_strong(expected, StartState::Starting)) {
        return AgentStartCode::AlreadyStarted;
    }

    try {
        config_ = config;
        std::string error;
        if (!validate_bootstrap_config(config_, &error)) {
            return fail_start(AgentStartCode::InvalidConfig, "agent.bootstrap", error);
        }
        if (getpid() != config_.target_pid) {
            return fail_start(
                AgentStartCode::IdentityMismatch,
                "agent.identity",
                "当前 PID 与 bootstrap target_pid 不一致");
        }
        std::string process_name;
        if (!read_process_name(&process_name, &error) || process_name != config_.package_name) {
            if (error.empty()) {
                error = "当前进程包名与 bootstrap package_name 不一致";
            }
            return fail_start(AgentStartCode::IdentityMismatch, "agent.identity", error);
        }
        if (!find_loaded_module(config_.module_name, &module_, &error)) {
            return fail_start(AgentStartCode::ModuleMissing, "agent.module", error);
        }
        std::uintptr_t target = 0;
        if (!resolve_exported_function(module_, "tolua_update", &target, &error)) {
            return fail_start(AgentStartCode::SymbolMissing, "agent.lua_symbols", error);
        }
        config_.target_symbol_offset = target - module_.load_bias;
        if (!module_.contains_executable(target, kHookPrologueSize)) {
            return fail_start(
                AgentStartCode::SymbolMissing,
                "agent.profile",
                "tolua_update 不在目标模块可执行段内");
        }
        if (std::memcmp(
                reinterpret_cast<const void*>(target),
                config_.expected_prologue,
                kHookPrologueSize) != 0) {
            return fail_start(
                AgentStartCode::PrologueMismatch,
                "agent.profile",
                "tolua_update 入口指令不满足当前 Hook 的复制条件");
        }
        if (!lua_api_.resolve(module_, &error)) {
            return fail_start(AgentStartCode::SymbolMissing, "agent.lua_symbols", error);
        }

        identity_ = AgentIdentity{
            .session_id = bootstrap_session_id_hex(config_),
            .process_id = config_.target_pid,
            .package_name = config_.package_name,
        };
        server_ = std::make_unique<RpcServer>();
        if (!server_->bind_channel(bootstrap_channel_id_hex(config_), &error)) {
            return fail_start(AgentStartCode::SocketStartFailed, "agent.socket_bind", error);
        }
        if (!hook_.install(
                module_,
                target,
                hook_entry_address(),
                config_.expected_prologue,
                &original_update_,
                &error)) {
            const AgentStartCode code = error.find("prologue") != std::string::npos
                                            ? AgentStartCode::PrologueMismatch
                                            : AgentStartCode::HookInstallFailed;
            return fail_start(code, "agent.hook_install", error);
        }
        if (!server_->start(this, &error)) {
            return fail_start(AgentStartCode::SocketStartFailed, "agent.socket_thread", error);
        }
        start_state_.store(StartState::Ready, std::memory_order_release);
        log_info("agent.start", "专用运行态 agent 已启动");
        return AgentStartCode::Ok;
    } catch (const std::exception& exception) {
        return fail_start(AgentStartCode::SocketStartFailed, "agent.start", exception.what());
    } catch (...) {
        return fail_start(AgentStartCode::SocketStartFailed, "agent.start", "未知本地异常");
    }
}

// 任何失败都关闭监听、回滚 Hook、擦除配置并永久封闭启动状态。
AgentStartCode AgentRuntime::fail_start(
    AgentStartCode code,
    const char* stage,
    const std::string& message) noexcept {
    log_error(stage, message);
    if (server_ != nullptr) {
        server_->close_listener();
    }
    std::string rollback_error;
    if (!hook_.uninstall(&rollback_error)) {
        log_error("agent.hook_rollback", rollback_error);
        code = AgentStartCode::HookInstallFailed;
    }
    secure_zero(&config_, sizeof(config_));
    start_state_.store(StartState::Failed, std::memory_order_release);
    return code;
}

// 原函数必须先完成当前帧更新，随后才允许任务读取或改变稳定状态。
int AgentRuntime::invoke_tolua_update(
    lua_State* state,
    float delta_time,
    float unscaled_delta_time) noexcept {
    ActiveCallback callback(&active_callbacks_);
    const std::uintptr_t original_address = original_update_.load(std::memory_order_acquire);
    if (original_address == 0) {
        return 0;
    }
    ToluaUpdate original = nullptr;
    static_assert(sizeof(original) == sizeof(original_address));
    std::memcpy(&original, &original_address, sizeof(original));
    const int result = original(state, delta_time, unscaled_delta_time);
    after_tolua_update(state);
    return result;
}

// 每帧至多取出一个任务，并在任务之后观察唯一未决装备命令。
void AgentRuntime::after_tolua_update(lua_State* state) noexcept {
    if (state == nullptr) {
        return;
    }
    if (main_state_.exchange(state, std::memory_order_acq_rel) != state) {
        catalog_generation_.fetch_add(1, std::memory_order_release);
    }

    const bool accepting = start_state_.load(std::memory_order_acquire) == StartState::Ready;
    std::shared_ptr<LuaTask> task = tasks_.take_for_execution(accepting);
    if (!accepting) {
        return;
    }
    if (task != nullptr) {
        const int memory_before = lua_api_.memory_kib(state);
        bool continue_task = false;
        bool publish_task = false;
        std::visit(
            [&](auto& body) {
                using Task = std::decay_t<decltype(body)>;
                if constexpr (std::is_same_v<Task, ResourcesLuaTask>) {
                    auto outcome = snapshot_resources(lua_api_, state);
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else if constexpr (std::is_same_v<Task, BagLuaTask>) {
                    SnapshotExecution outcome = snapshot_bag(lua_api_, state, body.max_items);
                    if (outcome.success && outcome.snapshot.complete) {
                        bag_read_ready_.store(true, std::memory_order_release);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else if constexpr (std::is_same_v<Task, OwnedStateLuaTask>) {
                    OwnedStateExecution outcome = snapshot_owned_state(
                        lua_api_,
                        state,
                        body.max_ships,
                        body.max_equipments,
                        body.max_items);
                    if (outcome.success && outcome.snapshot.bag.complete) {
                        bag_read_ready_.store(true, std::memory_order_release);
                    }
                    if (outcome.success && outcome.snapshot.complete) {
                        owned_state_read_ready_.store(true, std::memory_order_release);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else if constexpr (std::is_same_v<Task, OwnedQueryLuaTask>) {
                    const bool finished = advance_owned_query(lua_api_, state, &body.progress, &body.outcome);
                    std::lock_guard task_lock(task->mutex);
                    if (task->state == TaskState::Running) {
                        continue_task = !finished;
                        publish_task = finished;
                    }
                } else if constexpr (std::is_same_v<Task, AccountBeforeLuaTask>) {
                    const AccountBeforeStep step = advance_account_before(
                        lua_api_, state, &body.progress, &body.outcome);
                    std::lock_guard task_lock(task->mutex);
                    if (task->state == TaskState::Canceled) {
                        continue_task = false;
                    } else if (step == AccountBeforeStep::Continue &&
                               task->state == TaskState::Running) {
                        continue_task = true;
                    } else if (task->state == TaskState::Running) {
                        const AccountBeforeReadiness readiness =
                            account_before_readiness(body.outcome);
                        if (readiness.bag) {
                            bag_read_ready_.store(true, std::memory_order_release);
                        }
                        if (readiness.owned_state) {
                            owned_state_read_ready_.store(true, std::memory_order_release);
                        }
                        if (readiness.ship_details) {
                            ship_details_read_ready_.store(true, std::memory_order_release);
                        }
                        publish_task = true;
                    }
                } else if constexpr (std::is_same_v<Task, ShipDetailsLuaTask>) {
                    ShipDetailsExecution outcome = snapshot_ship_details(
                        lua_api_, state, body.max_ships, body.module_sha256);
                    if (outcome.success && outcome.snapshot.complete) {
                        ship_details_read_ready_.store(true, std::memory_order_release);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else if constexpr (std::is_same_v<Task, ShipCatalogLuaTask>) {
                    const ShipCatalogBatchStep step = advance_ship_catalog_batch(
                        lua_api_, state, &body.progress);
                    std::lock_guard task_lock(task->mutex);
                    switch (decide_batch_frame(
                        task->state, step == ShipCatalogBatchStep::Continue)) {
                    case BatchFrameDisposition::Continue:
                        continue_task = true;
                        break;
                    case BatchFrameDisposition::Publish:
                        body.outcome = std::move(body.progress.outcome);
                        publish_task = true;
                        break;
                    case BatchFrameDisposition::Drop:
                        continue_task = false;
                        break;
                    }
                } else if constexpr (std::is_same_v<Task, EquipmentConfigLuaTask>) {
                    const EquipmentBatchStep step =
                        advance_equipment_config_batch(lua_api_, state, &body.progress);
                    std::lock_guard task_lock(task->mutex);
                    switch (decide_batch_frame(
                        task->state, step == EquipmentBatchStep::Continue)) {
                    case BatchFrameDisposition::Continue:
                        continue_task = true;
                        break;
                    case BatchFrameDisposition::Publish:
                        body.outcome = std::move(body.progress.outcome);
                        if (body.outcome.success && body.outcome.page.complete &&
                            !body.outcome.page.configs.empty()) {
                            equipment_configs_read_ready_.store(true, std::memory_order_release);
                        }
                        publish_task = true;
                        break;
                    case BatchFrameDisposition::Drop:
                        continue_task = false;
                        break;
                    }
                } else if constexpr (std::is_same_v<Task, ComposeRecipeLuaTask>) {
                    const EquipmentBatchStep step =
                        advance_compose_recipe_batch(lua_api_, state, &body.progress);
                    std::lock_guard task_lock(task->mutex);
                    switch (decide_batch_frame(
                        task->state, step == EquipmentBatchStep::Continue)) {
                    case BatchFrameDisposition::Continue:
                        continue_task = true;
                        break;
                    case BatchFrameDisposition::Publish:
                        body.outcome = std::move(body.progress.outcome);
                        if (body.outcome.success && body.outcome.page.complete &&
                            !body.outcome.page.recipes.empty()) {
                            compose_recipes_read_ready_.store(true, std::memory_order_release);
                        }
                        publish_task = true;
                        break;
                    case BatchFrameDisposition::Drop:
                        continue_task = false;
                        break;
                    }
                } else if constexpr (std::is_same_v<Task, EquipmentWeaponLuaTask>) {
                    EquipmentWeaponBatchExecution outcome = snapshot_equipment_weapons(
                        lua_api_, state, body.weapon_ids, body.module_sha256);
                    if (outcome.success && outcome.batch.complete && !outcome.batch.weapons.empty()) {
                        equipment_weapons_read_ready_.store(true, std::memory_order_release);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else if constexpr (std::is_same_v<Task, SkillEffectLuaTask>) {
                    SkillEffectBatchExecution outcome = snapshot_skill_effects(
                        lua_api_, state, body.skills, body.module_sha256);
                    if (outcome.success && outcome.batch.complete && !outcome.batch.skills.empty()) {
                        skill_effects_read_ready_.store(true, std::memory_order_release);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else if constexpr (std::is_same_v<Task, EquipmentReferenceNameLuaTask>) {
                    EquipmentReferenceNameBatchExecution outcome =
                        snapshot_equipment_reference_names(
                            lua_api_,
                            state,
                            body.equipment_type_ids,
                            body.nation_ids,
                            body.ship_type_ids,
                            body.attribute_keys,
                            body.module_sha256);
                    if (outcome.success && outcome.batch.complete) {
                        equipment_reference_names_read_ready_.store(true, std::memory_order_release);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                } else {
                    EquipmentCommandDispatchExecution outcome = dispatch_equipment_command(
                        lua_api_,
                        state,
                        EquipmentCommandLedger::command(body.command),
                        body.dispatch_gate.get(),
                        EquipmentCommandLedger::deadline(body.command));
                    if (outcome.write_dispatched) {
                        equipment_commands_.mark_dispatched(body.command, outcome.error);
                    } else {
                        equipment_commands_.remove_undispatched(body.command);
                    }
                    std::lock_guard task_lock(task->mutex);
                    body.outcome = std::move(outcome);
                    publish_task = true;
                }
            },
            task->body);
        // 完整目录会短时生成大量临时表；让回收工作跟随采集分配，避免等待游戏的下一轮 GC。
        // 装备写入由命令账本负责，不在派发之后引入额外的 Lua 操作。
        if (!std::holds_alternative<EquipmentCommandLuaTask>(task->body)) {
            std::string collection_error;
            if (!lua_api_.collect_allocations_since(state, memory_before, &collection_error)) {
                log_error("agent.lua.collect", collection_error);
                std::lock_guard task_lock(task->mutex);
                std::visit([&](auto& body) {
                    using Task = std::decay_t<decltype(body)>;
                    if constexpr (!std::is_same_v<Task, EquipmentCommandLuaTask>) {
                        body.outcome.success = false;
                        if (!body.outcome.error.message.empty()) {
                            body.outcome.error.message += "; " + collection_error;
                        } else {
                            body.outcome.error = make_lua_error("lua_collection_failed", collection_error);
                        }
                    }
                }, task->body);
                if (task->state != TaskState::Canceled) {
                    publish_task = true;
                }
                continue_task = false;
            }
        }
        if (!continue_task) {
            {
                std::lock_guard task_lock(task->mutex);
                if (publish_task && task->state == TaskState::Running) {
                    task->state = TaskState::Completed;
                }
            }
            tasks_.finish_execution();
            task->condition.notify_one();
        }
    }

    if (start_state_.load(std::memory_order_acquire) != StartState::Ready) {
        return;
    }
    const std::optional<EquipmentCommandHandle> active =
        equipment_commands_.active_observation();
    if (!active.has_value()) {
        return;
    }
    EquipmentCommandStateRead state_read = read_equipment_command_state(
        lua_api_,
        state,
        EquipmentCommandLedger::command(*active));
    EquipmentCommandStateMatch match = EquipmentCommandStateMatch::Before;
    AgentError diagnostic;
    if (state_read.success) {
        match = classify_equipment_command_state(
            EquipmentCommandLedger::command(*active),
            state_read.state);
    } else {
        diagnostic = std::move(state_read.error);
    }
    try {
        equipment_commands_.observe(
            *active,
            match,
            std::move(diagnostic),
            EquipmentCommandLedger::Clock::now());
    } catch (const std::exception& exception) {
        log_error(
            "agent.equipment_command_observe",
            "记录装备命令观察结果失败: " + std::string(exception.what()));
    } catch (...) {
        log_error("agent.equipment_command_observe", "记录装备命令观察结果时发生未知异常");
    }
}

// 服务端先证明持有启动密钥，挑战中只包含公开会话身份、随机数和 MAC。
bool AgentRuntime::create_handshake_challenge(
    const secure_channel::HandshakeNonce& server_nonce,
    secure_channel::ServerChallenge* challenge,
    std::string* error) {
    return ::azlw::agent::create_handshake_challenge(config_, server_nonce, challenge, error);
}

// 每次连接独立验证挑战证明并派生密钥；会话凭据保留到显式停止以支持重连。
bool AgentRuntime::authenticate_handshake(
    const secure_channel::ServerChallenge& challenge,
    std::span<const std::uint8_t> frame,
    secure_channel::ChannelKeyMaterial* keys,
    std::string* error) {
    return verify_handshake_proof(challenge, frame, config_, keys, error);
}

// 先封闭队列，再等待唯一运行任务完成；Hook 保持安装直到 loader 冻结全部线程。
ShutdownPreparation AgentRuntime::prepare_shutdown(std::uint32_t timeout_ms) {
    ShutdownPreparation preparation;
    StartState expected = StartState::Ready;
    if (!start_state_.compare_exchange_strong(
            expected,
            StartState::Preparing,
            std::memory_order_acq_rel,
            std::memory_order_acquire)) {
        preparation.error = stopping_error("Agent 已经开始关闭或不处于可关闭状态");
        return preparation;
    }

    EquipmentCommandHandle started_equipment_command;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    const MainThreadTaskQueue::CanceledWaiting drained = tasks_.cancel_waiting();
    if (drained.running != nullptr) {
        if (auto* command = std::get_if<EquipmentCommandLuaTask>(&drained.running->body);
            command != nullptr &&
            cancel_equipment_command_dispatch(command->dispatch_gate.get()) ==
                EquipmentCommandDispatchCancelResult::CallMayHaveStarted) {
            started_equipment_command = command->command;
        }
    }
    if (started_equipment_command != nullptr) {
        equipment_commands_.mark_dispatched(started_equipment_command, AgentError{});
    }
    equipment_commands_.stop();
    for (const auto& task : drained.canceled) {
        task->condition.notify_one();
    }

    if (!tasks_.wait_until_idle(deadline)) {
        start_state_.store(StartState::Failed, std::memory_order_release);
        preparation.error = stopping_error("等待游戏主线程任务排空超时");
    }
    if (!preparation.error.code.empty()) {
        return preparation;
    }
    if (server_ == nullptr || server_->worker_tid() <= 0 ||
        server_->worker_tid() == identity_.process_id || server_->worker_start_time() == 0 ||
        hook_.target() == 0 || hook_.trampoline_start() == 0 || hook_.trampoline_size() == 0) {
        start_state_.store(StartState::Failed, std::memory_order_release);
        preparation.error = stopping_error("卸载收据所需的线程或 Hook 身份不完整");
        return preparation;
    }

    preparation.success = true;
    preparation.worker_tid = server_->worker_tid();
    preparation.worker_start_time = server_->worker_start_time();
    preparation.hook_target = hook_.target();
    preparation.trampoline_start = hook_.trampoline_start();
    preparation.trampoline_size = hook_.trampoline_size();
    start_state_.store(StartState::Prepared, std::memory_order_release);
    log_info("agent.shutdown", "主线程任务已排空，RPC 工作线程将停驻等待 loader 卸载");
    return preparation;
}

// 外部已经冻结线程，因此这里只做无等待检查和无分配 Hook 还原；对象由恢复线程后的 dlclose 析构。
AgentFinalizeCode AgentRuntime::finalize_shutdown() noexcept {
    StartState expected = StartState::Prepared;
    if (!start_state_.compare_exchange_strong(
            expected,
            StartState::Finalizing,
            std::memory_order_acq_rel,
            std::memory_order_acquire)) {
        return AgentFinalizeCode::NotPrepared;
    }
    const auto retryable = [this](AgentFinalizeCode code) {
        start_state_.store(StartState::Prepared, std::memory_order_release);
        return code;
    };
    if (server_ == nullptr || !server_->worker_parked()) {
        return retryable(AgentFinalizeCode::RpcWorkerNotParked);
    }
    if (active_callbacks_.load(std::memory_order_acquire) != 0) {
        return retryable(AgentFinalizeCode::CallbackRunning);
    }
    if (!tasks_.try_confirm_idle()) {
        return retryable(AgentFinalizeCode::RuntimeBusy);
    }

    if (hook_.uninstall_frozen() != HookUninstallStatus::Ok) {
        start_state_.store(StartState::Failed, std::memory_order_release);
        return AgentFinalizeCode::HookUninstallFailed;
    }
    start_state_.store(StartState::Finalized, std::memory_order_release);
    return AgentFinalizeCode::Ok;
}

// 使用不可优化擦除覆盖固定长度密钥字段。
void AgentRuntime::clear_session_secret() {
    secure_zero(config_.session_secret, sizeof(config_.session_secret));
}

// 业务会话身份在启动后保持只读，socket 使用独立通道身份。
const AgentIdentity& AgentRuntime::identity() const noexcept {
    return identity_;
}

// 同时检查启动事务和主线程状态，避免只凭历史 Lua 指针判定就绪。
bool AgentRuntime::main_thread_ready() const noexcept {
    return start_state_.load(std::memory_order_acquire) == StartState::Ready &&
           main_state_.load(std::memory_order_acquire) != nullptr;
}

// 只有主线程可用且至少完成过一份完整快照时，才宣告背包读取就绪。
bool AgentRuntime::bag_read_ready() const noexcept {
    return main_thread_ready() && bag_read_ready_.load(std::memory_order_acquire);
}

// 完整运行态只有在主线程仍可用且成功取得过一致快照后才宣告就绪。
bool AgentRuntime::owned_state_read_ready() const noexcept {
    return main_thread_ready() && owned_state_read_ready_.load(std::memory_order_acquire);
}

// 详情能力只在主线程仍可用且完整解析过当前客户端方法后公布。
bool AgentRuntime::ship_details_read_ready() const noexcept {
    return main_thread_ready() && ship_details_read_ready_.load(std::memory_order_acquire);
}

bool AgentRuntime::equipment_configs_read_ready() const noexcept {
    return main_thread_ready() &&
           equipment_configs_read_ready_.load(std::memory_order_acquire);
}

bool AgentRuntime::compose_recipes_read_ready() const noexcept {
    return main_thread_ready() && compose_recipes_read_ready_.load(std::memory_order_acquire);
}

bool AgentRuntime::equipment_weapons_read_ready() const noexcept {
    return main_thread_ready() && equipment_weapons_read_ready_.load(std::memory_order_acquire);
}

bool AgentRuntime::skill_effects_read_ready() const noexcept {
    return main_thread_ready() && skill_effects_read_ready_.load(std::memory_order_acquire);
}

bool AgentRuntime::equipment_reference_names_read_ready() const noexcept {
    return main_thread_ready() &&
           equipment_reference_names_read_ready_.load(std::memory_order_acquire);
}

bool AgentRuntime::equipment_commands_ready() const noexcept {
    return main_thread_ready() && owned_state_read_ready_.load(std::memory_order_acquire);
}

// 队列只允许一个任务；超时时先从队列移除，再将仍待执行任务标为取消。
bool AgentRuntime::wait_for_task(
    const std::shared_ptr<LuaTask>& task,
    std::uint32_t timeout_ms,
    AgentError* error) {
    return wait_for_task_until(
        task,
        EquipmentCommandLedger::Clock::now() + std::chrono::milliseconds(timeout_ms),
        error);
}

bool AgentRuntime::wait_for_task_until(
    const std::shared_ptr<LuaTask>& task,
    EquipmentCommandLedger::TimePoint deadline,
    AgentError* error) {
    const StartState initial_state = start_state_.load(std::memory_order_acquire);
    if (initial_state != StartState::Ready) {
        *error = initial_state == StartState::Preparing || initial_state == StartState::Prepared
                     ? stopping_error("Agent 正在关闭，不再接受主线程任务")
                     : queue_error("main_thread_not_ready", "尚未捕获游戏主线程 Lua 状态");
        return false;
    }
    if (main_state_.load(std::memory_order_acquire) == nullptr) {
        *error = queue_error("main_thread_not_ready", "尚未捕获游戏主线程 Lua 状态");
        return false;
    }

    if (start_state_.load(std::memory_order_acquire) != StartState::Ready) {
        *error = stopping_error("Agent 正在关闭，不再接受主线程任务");
        return false;
    }
    switch (tasks_.enqueue_and_wait(
        task,
        deadline,
        start_state_.load(std::memory_order_acquire) == StartState::Ready)) {
        case TaskWaitStatus::Completed:
            return true;
        case TaskWaitStatus::Canceled:
            *error = stopping_error("Agent 关闭已取消等待中的主线程任务");
            return false;
        case TaskWaitStatus::Busy:
            *error = queue_error("main_thread_queue_busy", "主线程队列当前已有任务");
            return false;
        case TaskWaitStatus::NotAccepting:
            *error = stopping_error("Agent 正在关闭，不再接受主线程任务");
            return false;
        case TaskWaitStatus::TimedOut:
            *error = queue_error(
                "main_thread_timeout",
                "等待主线程执行 " + std::string(lua_task_operation_name(task->body)) + " 超时");
            return false;
    }
    *error = queue_error("main_thread_timeout", "等待主线程执行任务超时");
    return false;
}

template <typename TaskBody>
auto AgentRuntime::execute_queued(TaskBody body, std::uint32_t timeout_ms) -> decltype(body.outcome) {
    using Outcome = decltype(body.outcome);
    auto task = std::make_shared<LuaTask>(std::move(body));
    Outcome failure{};
    if (!wait_for_task(task, timeout_ms, &failure.error)) {
        return failure;
    }
    return std::move(std::get<TaskBody>(task->body).outcome);
}

SnapshotExecution AgentRuntime::execute_snapshot(
    std::uint32_t max_items,
    std::uint32_t timeout_ms) {
    return execute_queued(BagLuaTask{.max_items = max_items, .outcome = {}}, timeout_ms);
}

AccountBeforeExecution AgentRuntime::execute_account_before_snapshot(
    std::uint32_t max_ships,
    std::uint32_t max_equipments,
    std::uint32_t max_items,
    std::uint32_t timeout_ms) {
    AccountBeforeLuaTask task;
    task.progress.max_ships = max_ships;
    task.progress.max_equipments = max_equipments;
    task.progress.max_items = max_items;
    task.progress.module_sha256 = std::string(config_.module_sha256);
    return execute_queued(std::move(task), timeout_ms);
}

ResourcesExecution AgentRuntime::execute_resources_snapshot(std::uint32_t timeout_ms) {
    return execute_queued(ResourcesLuaTask{}, timeout_ms);
}

OwnedQueryExecution AgentRuntime::execute_owned_query(const OwnedQuery& query, std::uint32_t timeout_ms) {
    OwnedQueryLuaTask task;
    task.progress.query = query;
    return execute_queued(std::move(task), timeout_ms);
}

OwnedStateExecution AgentRuntime::execute_owned_state_snapshot(
    std::uint32_t max_ships,
    std::uint32_t max_equipments,
    std::uint32_t max_items,
    std::uint32_t timeout_ms) {
    return execute_queued(
        OwnedStateLuaTask{
            .max_ships = max_ships,
            .max_equipments = max_equipments,
            .max_items = max_items,
            .outcome = {},
        },
        timeout_ms);
}

ShipDetailsExecution AgentRuntime::execute_ship_details_snapshot(
    std::uint32_t max_ships,
    std::uint32_t timeout_ms) {
    return execute_queued(
        ShipDetailsLuaTask{
            .max_ships = max_ships,
            .module_sha256 = std::string(config_.module_sha256),
            .outcome = {},
        },
        timeout_ms);
}

ShipCatalogPageExecution AgentRuntime::execute_ship_catalog_page(
    std::string table_key, std::uint32_t start_index, std::uint32_t page_size, std::uint32_t timeout_ms) {
    const auto table = std::find(kSupportedShipCatalogTables.begin(), kSupportedShipCatalogTables.end(), table_key);
    // 科技扩展与账号图鉴沿用实时读取，只有基础静态白名单跨连接保留。
    if (table == kSupportedShipCatalogTables.end() || std::distance(kSupportedShipCatalogTables.begin(), table) >= 17) {
        return read_ship_catalog_page(std::move(table_key), start_index, page_size, timeout_ms);
    }
    const auto key = "ship:" + table_key + ":" + std::to_string(start_index) + ":" + std::to_string(page_size);
    return read_static_cached(key, [&] { return read_ship_catalog_page(std::move(table_key), start_index, page_size, timeout_ms); },
        [](const ShipCatalogPageExecution& result) -> std::size_t {
            return result.success && result.page.complete ? encode_ship_catalog_page("0000000000000001", result.page).size() : 0;
        });
}

ShipCatalogPageExecution AgentRuntime::read_ship_catalog_page(
    std::string table_key,
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::uint32_t timeout_ms) {
    ShipCatalogLuaTask task;
    task.progress.table_key = std::move(table_key);
    task.progress.start_index = start_index;
    task.progress.page_size = page_size;
    task.progress.module_sha256 = std::string(config_.module_sha256);
    task.progress.cursor = start_index;
    const bool continue_page = task.progress.table_key == "collection_ship_group" &&
                               start_index != 0 && parked_collection_index_ &&
                               parked_collection_next_ == start_index &&
                               parked_collection_module_ == task.progress.module_sha256;
    if (!continue_page) {
        release_parked_collection_index();
    }
    if (task.progress.table_key == "collection_ship_group") {
        if (continue_page) {
            task.progress.collection_index = std::move(parked_collection_index_);
        } else {
            task.progress.collection_index = std::make_shared<CollectionReadIndex>();
        }
        parked_collection_module_ = task.progress.module_sha256;
    }
    auto queued = std::make_shared<LuaTask>(std::move(task));
    ShipCatalogPageExecution failure{};
    if (!wait_for_task(queued, timeout_ms, &failure.error)) {
        return failure;
    }
    auto& body = std::get<ShipCatalogLuaTask>(queued->body);
    if (body.outcome.success && body.outcome.page.next_index.has_value() && body.progress.collection_index &&
        body.progress.collection_index->ready) {
        parked_collection_next_ = *body.outcome.page.next_index;
        parked_collection_index_ = body.progress.collection_index;
    } else {
        parked_collection_index_.reset();
    }
    return std::move(body.outcome);
}

void AgentRuntime::release_parked_collection_index() noexcept {
    parked_collection_index_.reset();
    parked_collection_next_ = 0;
    parked_collection_module_.clear();
}

EquipmentConfigPageExecution AgentRuntime::execute_equipment_config_page(
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::uint32_t timeout_ms,
    const std::vector<std::uint64_t>& ids) {
    EquipmentConfigLuaTask task;
    task.progress.ids = ids;
    task.progress.start_index = start_index;
    task.progress.page_size = page_size;
    task.progress.module_sha256 = std::string(config_.module_sha256);
    task.progress.cursor = start_index;
    std::string key = "equipment:" + std::to_string(start_index) + ":" + std::to_string(page_size);
    for (auto id : ids) key += ":" + std::to_string(id);
    return read_static_cached(key,
        [&] { return execute_queued(std::move(task), timeout_ms); },
        [](const EquipmentConfigPageExecution& result) -> std::size_t {
            return result.success && result.page.complete ? encode_equipment_config_page("0000000000000001", result.page).size() : 0;
        });
}

ComposeRecipePageExecution AgentRuntime::execute_compose_recipe_page(
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::uint32_t timeout_ms) {
    ComposeRecipeLuaTask task;
    task.progress.start_index = start_index;
    task.progress.page_size = page_size;
    task.progress.module_sha256 = std::string(config_.module_sha256);
    task.progress.cursor = start_index;
    return read_static_cached("recipe:" + std::to_string(start_index) + ":" + std::to_string(page_size),
        [&] { return execute_queued(std::move(task), timeout_ms); },
        [](const ComposeRecipePageExecution& result) -> std::size_t {
            return result.success && result.page.complete ? encode_compose_recipe_page("0000000000000001", result.page).size() : 0;
        });
}

EquipmentWeaponBatchExecution AgentRuntime::execute_equipment_weapon_batch(
    std::vector<std::uint64_t> weapon_ids,
    std::uint32_t timeout_ms) {
    std::string key = "weapons";
    for (auto id : weapon_ids) key += ":" + std::to_string(id);
    return read_static_cached(key, [&] { return execute_queued(
        EquipmentWeaponLuaTask{
            .weapon_ids = std::move(weapon_ids),
            .module_sha256 = std::string(config_.module_sha256),
            .outcome = {},
        },
        timeout_ms); }, [](const EquipmentWeaponBatchExecution& result) -> std::size_t {
            return result.success && result.batch.complete ? encode_equipment_weapon_batch("0000000000000001", result.batch).size() : 0;
        });
}

SkillEffectBatchExecution AgentRuntime::execute_skill_effect_batch(
    std::vector<SkillEffectQuery> skills,
    std::uint32_t timeout_ms) {
    std::string key = "skills";
    for (const auto& skill : skills) key += ":" + std::to_string(skill.skill_id) + "/" + std::to_string(skill.level);
    return read_static_cached(key, [&] { return execute_queued(
        SkillEffectLuaTask{
            .skills = std::move(skills),
            .module_sha256 = std::string(config_.module_sha256),
            .outcome = {},
        },
        timeout_ms); }, [](const SkillEffectBatchExecution& result) -> std::size_t {
            return result.success && result.batch.complete ? encode_skill_effect_batch("0000000000000001", result.batch).size() : 0;
        });
}

EquipmentReferenceNameBatchExecution AgentRuntime::execute_equipment_reference_name_batch(
    std::vector<std::uint64_t> equipment_type_ids,
    std::vector<std::uint64_t> nation_ids,
    std::vector<std::uint64_t> ship_type_ids,
    std::vector<std::string> attribute_keys,
    std::uint32_t timeout_ms) {
    std::string key = "names";
    for (const auto* ids : {&equipment_type_ids, &nation_ids, &ship_type_ids}) {
        key += ";";
        for (auto id : *ids) key += ":" + std::to_string(id);
    }
    key += ";";
    for (const auto& attribute : attribute_keys) key += std::to_string(attribute.size()) + ":" + attribute;
    return read_static_cached(key, [&] { return execute_queued(
        EquipmentReferenceNameLuaTask{
            .equipment_type_ids = std::move(equipment_type_ids),
            .nation_ids = std::move(nation_ids),
            .ship_type_ids = std::move(ship_type_ids),
            .attribute_keys = std::move(attribute_keys),
            .module_sha256 = std::string(config_.module_sha256),
            .outcome = {},
        },
        timeout_ms); }, [](const EquipmentReferenceNameBatchExecution& result) -> std::size_t {
            return result.success && result.batch.complete ? encode_equipment_reference_name_batch("0000000000000001", result.batch).size() : 0;
        });
}

EquipmentCommandExecution AgentRuntime::execute_equipment_command(
    EquipmentCommand command_value,
    std::uint32_t timeout_ms) {
    if (!equipment_commands_ready()) {
        return EquipmentCommandExecution{
            .success = false,
            .receipt = {},
            .error = queue_error(
                "equipment_commands_not_ready",
                "装备命令要求主线程可用且已完成一份完整运行态快照"),
        };
    }

    const auto deadline = EquipmentCommandLedger::Clock::now() +
                          std::chrono::milliseconds(timeout_ms);
    EquipmentCommandBeginResult begin =
        equipment_commands_.begin(std::move(command_value), deadline);
    if (begin.kind != EquipmentCommandBeginKind::DispatchRequired) {
        return std::move(begin.execution);
    }

    auto task = std::make_shared<LuaTask>(EquipmentCommandLuaTask{
        .command = begin.handle,
        .outcome = {},
    });
    AgentError wait_error;
    bool completed = wait_for_task_until(task, deadline, &wait_error);
    bool call_may_have_started = false;
    auto& command_task = std::get<EquipmentCommandLuaTask>(task->body);
    if (!completed) {
        std::lock_guard task_lock(task->mutex);
        if (task->state == TaskState::Running) {
            call_may_have_started =
                cancel_equipment_command_dispatch(command_task.dispatch_gate.get()) ==
                EquipmentCommandDispatchCancelResult::CallMayHaveStarted;
        }
        completed = task->state == TaskState::Completed;
    }
    if (!completed && call_may_have_started) {
        equipment_commands_.mark_dispatched(begin.handle, AgentError{});
        return equipment_commands_.query(
            EquipmentCommandLedger::command(begin.handle).command_id,
            EquipmentCommandLedger::Clock::now());
    }
    if (!completed) {
        equipment_commands_.remove_undispatched(begin.handle);
        return EquipmentCommandExecution{
            .success = false,
            .receipt = {},
            .error = std::move(wait_error),
        };
    }
    if (!command_task.outcome.write_dispatched) {
        return EquipmentCommandExecution{
            .success = false,
            .receipt = {},
            .error = std::move(command_task.outcome.error),
        };
    }
    return equipment_commands_.query(
        EquipmentCommandLedger::command(begin.handle).command_id,
        EquipmentCommandLedger::Clock::now());
}

EquipmentCommandExecution AgentRuntime::query_equipment_command(
    std::string_view command_id) {
    return equipment_commands_.query(
        command_id,
        EquipmentCommandLedger::Clock::now());
}

EquipmentCommandExecution AgentRuntime::cancel_equipment_command(
    std::string_view command_id) {
    return equipment_commands_.cancel(
        command_id,
        EquipmentCommandLedger::Clock::now());
}

}  // namespace azlw::agent

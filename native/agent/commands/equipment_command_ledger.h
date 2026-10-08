// 声明单会话装备命令的幂等账本、未决写门禁和状态收敛规则。

#pragma once

#include <chrono>
#include <cstddef>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>

#include "equipment_command.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// execute、query 和 cancel 共用的成功收据或顶层 Agent 错误。
struct EquipmentCommandExecution final {
    bool success = false;
    EquipmentCommandReceipt receipt;
    AgentError error;
};

/// 账本记录只通过不透明共享句柄跨越主线程任务边界。
struct EquipmentCommandRecord;
using EquipmentCommandHandle = std::shared_ptr<EquipmentCommandRecord>;

/// begin 的三种互斥结果，避免把已有命令误当成需要再次派发。
enum class EquipmentCommandBeginKind { DispatchRequired, Existing, Rejected };

/// 新建或复用命令的结果；只有 DispatchRequired 携带待执行句柄。
struct EquipmentCommandBeginResult final {
    EquipmentCommandBeginKind kind = EquipmentCommandBeginKind::Rejected;
    EquipmentCommandHandle handle;
    EquipmentCommandExecution execution;
};

/// 保存一次认证会话内全部 command_id，旧记录不驱逐以维持严格幂等。
class EquipmentCommandLedger final {
public:
    using Clock = EquipmentCommandClock;
    using TimePoint = EquipmentCommandDeadline;

    /// 账本包含互斥量保护的可变收据，不可复制。
    EquipmentCommandLedger() = default;
    EquipmentCommandLedger(const EquipmentCommandLedger&) = delete;
    EquipmentCommandLedger& operator=(const EquipmentCommandLedger&) = delete;

    /// 登记新命令，或返回同 ID 已有收据；不同载荷复用 ID 会稳定冲突。
    EquipmentCommandBeginResult begin(
        EquipmentCommand command,
        TimePoint deadline);
    /// 返回句柄绑定的不可变原命令，供游戏主线程执行和观察。
    static const EquipmentCommand& command(const EquipmentCommandHandle& handle) noexcept;
    /// 返回登记时冻结的唯一绝对截止时间，供等待、派发和观察共同使用。
    static TimePoint deadline(const EquipmentCommandHandle& handle) noexcept;
    /// 标记官方通知调用已经开始，并保存可能的派发诊断。
    void mark_dispatched(
        const EquipmentCommandHandle& handle,
        AgentError diagnostic);
    /// 移除确定未派发的记录，使相同命令可以在前态修复后安全重试。
    void remove_undispatched(const EquipmentCommandHandle& handle);
    /// 返回当前收据；查询本身不会重新派发或延长原始时限。
    EquipmentCommandExecution query(
        std::string_view command_id,
        TimePoint now);
    /// 停止继续观察，但不声称已经撤回服务器请求。
    EquipmentCommandExecution cancel(
        std::string_view command_id,
        TimePoint now);
    /// 返回当前仍允许逐帧观察的唯一已派发命令。
    std::optional<EquipmentCommandHandle> active_observation() const;
    /// 合入一次主线程状态观察，并按完整后态、前态或异常变化收敛。
    void observe(
        const EquipmentCommandHandle& handle,
        EquipmentCommandStateMatch match,
        AgentError diagnostic,
        TimePoint now);
    /// 关闭会话前终止所有观察等待，保留未知写入的事实。
    void stop();
    /// 返回保留的命令记录数，供边界测试核对不驱逐策略。
    std::size_t size() const;

private:
    /// 在持锁状态把仍在观察的命令收敛为 unknown/uncertain。
    void mark_uncertain_locked(
        const EquipmentCommandHandle& handle,
        std::string code,
        std::string message);
    /// 到达原始时限时停止观察，不让后续查询隐式延长等待。
    void expire_locked(
        const EquipmentCommandHandle& handle,
        TimePoint now);

    mutable std::mutex mutex_;
    std::unordered_map<std::string, EquipmentCommandHandle> records_;
    std::optional<std::string> unresolved_command_id_;
};

}  // namespace azlw::agent

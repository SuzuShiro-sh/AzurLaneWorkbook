// 实现装备命令的有界幂等账本、单写门禁和保守状态收敛。

#include "equipment_command_ledger.h"

#include <cstdint>
#include <limits>
#include <utility>

namespace azlw::agent {

/// 保存不可变原命令、观察时限和所有操作共享的可变收据。
struct EquipmentCommandRecord final {
    EquipmentCommandRecord(
        EquipmentCommand requested_command,
        EquipmentCommandLedger::TimePoint requested_deadline)
        : command(std::move(requested_command)), deadline(requested_deadline) {
        receipt.command_id = command.command_id;
    }

    EquipmentCommand command;
    EquipmentCommandReceipt receipt;
    EquipmentCommandLedger::TimePoint deadline;
};

namespace {

constexpr std::size_t kMaximumEquipmentCommandRecords = 10'000;

/// 构造不会自动重试且保持当前认证会话的账本错误。
AgentError ledger_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.equipment_command_ledger",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

/// 从已验证记录复制线上收据。
EquipmentCommandExecution receipt_execution(const EquipmentCommandHandle& handle) {
    return EquipmentCommandExecution{
        .success = true,
        .receipt = handle->receipt,
        .error = {},
    };
}

/// 核对 map 中仍保存完全相同的共享记录。
bool record_is_current(
    const std::unordered_map<std::string, EquipmentCommandHandle>& records,
    const EquipmentCommandHandle& handle) {
    if (!handle) {
        return false;
    }
    const auto found = records.find(handle->command.command_id);
    return found != records.end() && found->second == handle;
}

}  // namespace

EquipmentCommandBeginResult EquipmentCommandLedger::begin(
    EquipmentCommand command_value,
    TimePoint deadline) {
    std::lock_guard lock(mutex_);
    const auto found = records_.find(command_value.command_id);
    if (found != records_.end()) {
        if (found->second->command != command_value) {
            return EquipmentCommandBeginResult{
                .kind = EquipmentCommandBeginKind::Rejected,
                .handle = {},
                .execution = EquipmentCommandExecution{
                    .success = false,
                    .receipt = {},
                    .error = ledger_error(
                        "equipment_command_id_conflict",
                        "同一 command_id 已绑定不同装备命令载荷"),
                },
            };
        }
        if (!found->second->receipt.write_dispatched) {
            return EquipmentCommandBeginResult{
                .kind = EquipmentCommandBeginKind::Rejected,
                .handle = {},
                .execution = EquipmentCommandExecution{
                    .success = false,
                    .receipt = {},
                    .error = ledger_error(
                        "equipment_command_dispatch_pending",
                        "相同装备命令仍在等待游戏主线程派发"),
                },
            };
        }
        expire_locked(found->second, Clock::now());
        return EquipmentCommandBeginResult{
            .kind = EquipmentCommandBeginKind::Existing,
            .handle = found->second,
            .execution = receipt_execution(found->second),
        };
    }

    if (unresolved_command_id_.has_value()) {
        return EquipmentCommandBeginResult{
            .kind = EquipmentCommandBeginKind::Rejected,
            .handle = {},
            .execution = EquipmentCommandExecution{
                .success = false,
                .receipt = {},
                .error = ledger_error(
                    "equipment_command_in_progress",
                    "当前会话仍有一条未能确定结果的装备命令"),
            },
        };
    }
    if (records_.size() >= kMaximumEquipmentCommandRecords) {
        return EquipmentCommandBeginResult{
            .kind = EquipmentCommandBeginKind::Rejected,
            .handle = {},
            .execution = EquipmentCommandExecution{
                .success = false,
                .receipt = {},
                .error = ledger_error(
                    "equipment_command_ledger_full",
                    "当前会话装备命令记录已达到安全上限"),
            },
        };
    }

    auto handle = std::make_shared<EquipmentCommandRecord>(
        std::move(command_value),
        deadline);
    unresolved_command_id_ = handle->command.command_id;
    records_.emplace(handle->command.command_id, handle);
    return EquipmentCommandBeginResult{
        .kind = EquipmentCommandBeginKind::DispatchRequired,
        .handle = std::move(handle),
        .execution = {},
    };
}

const EquipmentCommand& EquipmentCommandLedger::command(
    const EquipmentCommandHandle& handle) noexcept {
    return handle->command;
}

EquipmentCommandLedger::TimePoint EquipmentCommandLedger::deadline(
    const EquipmentCommandHandle& handle) noexcept {
    return handle->deadline;
}

void EquipmentCommandLedger::mark_dispatched(
    const EquipmentCommandHandle& handle,
    AgentError diagnostic) {
    std::lock_guard lock(mutex_);
    if (!record_is_current(records_, handle)) {
        return;
    }
    handle->receipt.write_dispatched = true;
    if (!diagnostic.code.empty() &&
        handle->receipt.phase != EquipmentCommandPhase::Succeeded) {
        handle->receipt.error_code = std::move(diagnostic.code);
        handle->receipt.message = std::move(diagnostic.message);
    }
}

void EquipmentCommandLedger::remove_undispatched(
    const EquipmentCommandHandle& handle) {
    std::lock_guard lock(mutex_);
    if (!record_is_current(records_, handle) || handle->receipt.write_dispatched) {
        return;
    }
    if (unresolved_command_id_ == handle->command.command_id) {
        unresolved_command_id_.reset();
    }
    records_.erase(handle->command.command_id);
}

EquipmentCommandExecution EquipmentCommandLedger::query(
    std::string_view command_id,
    TimePoint now) {
    std::lock_guard lock(mutex_);
    const auto found = records_.find(std::string(command_id));
    if (found == records_.end() || !found->second->receipt.write_dispatched) {
        return EquipmentCommandExecution{
            .success = false,
            .receipt = {},
            .error = ledger_error(
                "equipment_command_not_found",
                "当前会话没有这条已派发装备命令"),
        };
    }
    expire_locked(found->second, now);
    return receipt_execution(found->second);
}

EquipmentCommandExecution EquipmentCommandLedger::cancel(
    std::string_view command_id,
    TimePoint now) {
    std::lock_guard lock(mutex_);
    const auto found = records_.find(std::string(command_id));
    if (found == records_.end() || !found->second->receipt.write_dispatched) {
        return EquipmentCommandExecution{
            .success = false,
            .receipt = {},
            .error = ledger_error(
                "equipment_command_not_found",
                "当前会话没有这条可取消观察的已派发装备命令"),
        };
    }
    expire_locked(found->second, now);
    found->second->receipt.cancel_requested = true;
    if (found->second->receipt.phase == EquipmentCommandPhase::Observing) {
        mark_uncertain_locked(
            found->second,
            "equipment_command_observation_canceled",
            "已停止等待装备命令，但服务器请求可能仍会完成");
    }
    return receipt_execution(found->second);
}

std::optional<EquipmentCommandHandle> EquipmentCommandLedger::active_observation() const {
    std::lock_guard lock(mutex_);
    if (!unresolved_command_id_.has_value()) {
        return std::nullopt;
    }
    const auto found = records_.find(*unresolved_command_id_);
    if (found == records_.end() || !found->second->receipt.write_dispatched ||
        found->second->receipt.phase != EquipmentCommandPhase::Observing) {
        return std::nullopt;
    }
    return found->second;
}

void EquipmentCommandLedger::observe(
    const EquipmentCommandHandle& handle,
    EquipmentCommandStateMatch match,
    AgentError diagnostic,
    TimePoint now) {
    std::lock_guard lock(mutex_);
    if (!record_is_current(records_, handle) || !handle->receipt.write_dispatched ||
        handle->receipt.phase != EquipmentCommandPhase::Observing) {
        return;
    }
    expire_locked(handle, now);
    if (handle->receipt.phase != EquipmentCommandPhase::Observing) {
        return;
    }
    if (handle->receipt.observation_count < std::numeric_limits<std::uint32_t>::max()) {
        ++handle->receipt.observation_count;
    }

    if (!diagnostic.code.empty()) {
        handle->receipt.error_code = std::move(diagnostic.code);
        handle->receipt.message = std::move(diagnostic.message);
    }
    if (match == EquipmentCommandStateMatch::After) {
        handle->receipt.status = EquipmentCommandStatus::Success;
        handle->receipt.phase = EquipmentCommandPhase::Succeeded;
        handle->receipt.error_code.reset();
        handle->receipt.message.reset();
        if (unresolved_command_id_ == handle->command.command_id) {
            unresolved_command_id_.reset();
        }
        return;
    }
    if (match == EquipmentCommandStateMatch::Mismatch) {
        mark_uncertain_locked(
            handle,
            "equipment_command_state_mismatch",
            "派发后的局部状态既不是完整前态，也不是完整预期后态");
        return;
    }
}

void EquipmentCommandLedger::stop() {
    std::lock_guard lock(mutex_);
    for (auto record = records_.begin(); record != records_.end();) {
        const EquipmentCommandHandle handle = record->second;
        if (!handle->receipt.write_dispatched) {
            if (unresolved_command_id_ == handle->command.command_id) {
                unresolved_command_id_.reset();
            }
            record = records_.erase(record);
            continue;
        }
        if (handle->receipt.write_dispatched &&
            handle->receipt.phase == EquipmentCommandPhase::Observing) {
            mark_uncertain_locked(
                handle,
                "equipment_command_session_stopping",
                "Agent 关闭已停止观察，服务器请求可能仍会完成");
        }
        ++record;
    }
}

std::size_t EquipmentCommandLedger::size() const {
    std::lock_guard lock(mutex_);
    return records_.size();
}

void EquipmentCommandLedger::mark_uncertain_locked(
    const EquipmentCommandHandle& handle,
    std::string code,
    std::string message) {
    handle->receipt.status = EquipmentCommandStatus::Unknown;
    handle->receipt.phase = EquipmentCommandPhase::Uncertain;
    handle->receipt.error_code = std::move(code);
    handle->receipt.message = std::move(message);
}

void EquipmentCommandLedger::expire_locked(
    const EquipmentCommandHandle& handle,
    TimePoint now) {
    if (handle->receipt.phase == EquipmentCommandPhase::Observing && now >= handle->deadline) {
        mark_uncertain_locked(
            handle,
            "equipment_command_observation_timeout",
            "等待装备命令完整后态超时，服务器请求可能仍会完成");
    }
}

}  // namespace azlw::agent

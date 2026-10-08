// 验证装备命令账本的幂等、未派发、未知结果、取消和关闭收敛，不依赖游戏进程。

#include <chrono>
#include <cstdlib>
#include <iostream>
#include <string>

#include "commands/equipment_command_ledger.h"
#include "protocol/protocol_types.h"

namespace {

using azlw::agent::AgentError;
using azlw::agent::EquipmentCommand;
using azlw::agent::EquipmentCommandBeginKind;
using azlw::agent::EquipmentCommandLedger;
using azlw::agent::EquipmentCommandPhase;
using azlw::agent::EquipmentCommandStateMatch;
using azlw::agent::EquipmentCommandStatus;
using azlw::agent::EquipEquipmentCommandAction;
using Clock = EquipmentCommandLedger::Clock;

int failures = 0;

void expect(bool condition, const char* message) {
    if (!condition) {
        std::cerr << message << '\n';
        ++failures;
    }
}

EquipmentCommand command_with_id(std::string command_id) {
    EquipmentCommand command;
    command.schema_version = 1;
    command.command_id = std::move(command_id);
    command.target_fingerprint_sha256 = "target";
    command.plan_hash = "plan";
    command.sequence = 1;
    command.pre_state_content_sha256 = "before";
    return command;
}

EquipmentCommandLedger::TimePoint later() {
    return Clock::now() + std::chrono::hours(1);
}

void test_duplicate_id_with_a_different_payload_conflicts() {
    EquipmentCommandLedger ledger;
    const auto first = command_with_id("same");
    auto begun = ledger.begin(first, later());
    expect(begun.kind == EquipmentCommandBeginKind::DispatchRequired, "新命令应要求派发");
    EquipmentCommand conflict = first;
    conflict.action = EquipEquipmentCommandAction{};
    const auto rejected = ledger.begin(conflict, later());
    expect(rejected.kind == EquipmentCommandBeginKind::Rejected, "不同载荷复用 ID 应拒绝");
    expect(
        !rejected.execution.success &&
            rejected.execution.error.code == "equipment_command_id_conflict",
        "载荷冲突应返回稳定错误码");
    expect(ledger.size() == 1, "冲突不得新增记录");
}

void test_undispatched_command_is_not_queryable_and_can_be_retried() {
    EquipmentCommandLedger ledger;
    const auto command = command_with_id("pending");
    auto begun = ledger.begin(command, later());
    const auto query = ledger.query(command.command_id, Clock::now());
    expect(
        !query.success && query.error.code == "equipment_command_not_found",
        "未派发命令不能被查询");
    const auto again = ledger.begin(command, later());
    expect(
        again.kind == EquipmentCommandBeginKind::Rejected &&
            again.execution.error.code == "equipment_command_dispatch_pending",
        "尚未派发的相同命令不能再次派发");
    const auto blocked = ledger.begin(command_with_id("other"), later());
    expect(
        blocked.kind == EquipmentCommandBeginKind::Rejected &&
            blocked.execution.error.code == "equipment_command_in_progress",
        "未决命令存在时不能开始另一条命令");
    ledger.remove_undispatched(begun.handle);
    expect(ledger.size() == 0, "确定未派发的记录应移除");
    const auto retried = ledger.begin(command, later());
    expect(retried.kind == EquipmentCommandBeginKind::DispatchRequired, "移除后相同命令可以重试");
    ledger.mark_dispatched(retried.handle, {});
    ledger.remove_undispatched(retried.handle);
    expect(ledger.size() == 1, "已派发记录不能因未派发移除而消失");
}

void test_same_dispatched_command_returns_the_existing_receipt() {
    EquipmentCommandLedger ledger;
    const auto command = command_with_id("done");
    auto begun = ledger.begin(command, later());
    ledger.mark_dispatched(begun.handle, {});
    const auto existing = ledger.begin(command, later());
    expect(existing.kind == EquipmentCommandBeginKind::Existing, "已派发的相同命令应复用收据");
    expect(existing.execution.success && existing.execution.receipt.write_dispatched, "复用收据应保留已派发事实");
    expect(ledger.size() == 1, "复用不得增加记录");
}

void test_mismatch_cancel_timeout_and_stop_keep_unknown_writes() {
    EquipmentCommandLedger mismatch_ledger;
    const auto mismatch_command = command_with_id("mismatch");
    auto mismatch = mismatch_ledger.begin(mismatch_command, later());
    mismatch_ledger.mark_dispatched(mismatch.handle, {});
    mismatch_ledger.observe(
        mismatch.handle, EquipmentCommandStateMatch::Mismatch, {}, Clock::now());
    const auto mismatch_query = mismatch_ledger.query(mismatch_command.command_id, Clock::now());
    expect(
        mismatch_query.success &&
            mismatch_query.receipt.status == EquipmentCommandStatus::Unknown &&
            mismatch_query.receipt.phase == EquipmentCommandPhase::Uncertain &&
            mismatch_query.receipt.error_code == "equipment_command_state_mismatch",
        "后态不匹配应收敛为未知结果");
    expect(
        mismatch_ledger.begin(command_with_id("after-mismatch"), later()).kind ==
            EquipmentCommandBeginKind::Rejected,
        "未知写入仍占用会话，不能开始下一条命令");

    EquipmentCommandLedger cancel_ledger;
    const auto cancel_command = command_with_id("cancel");
    auto cancel = cancel_ledger.begin(cancel_command, later());
    cancel_ledger.mark_dispatched(cancel.handle, {});
    const auto canceled = cancel_ledger.cancel(cancel_command.command_id, Clock::now());
    expect(
        canceled.success && canceled.receipt.cancel_requested &&
            canceled.receipt.phase == EquipmentCommandPhase::Uncertain &&
            canceled.receipt.error_code == "equipment_command_observation_canceled",
        "取消只停止观察，不把写入标成未发生");

    EquipmentCommandLedger timeout_ledger;
    const auto timeout_command = command_with_id("timeout");
    const auto deadline = Clock::now() - std::chrono::seconds(1);
    auto timeout = timeout_ledger.begin(timeout_command, deadline);
    timeout_ledger.mark_dispatched(timeout.handle, {});
    const auto timed_out = timeout_ledger.query(timeout_command.command_id, Clock::now());
    expect(
        timed_out.success && timed_out.receipt.phase == EquipmentCommandPhase::Uncertain &&
            timed_out.receipt.error_code == "equipment_command_observation_timeout",
        "超过原始时限应停止观察并保留未知写入");

    EquipmentCommandLedger stop_ledger;
    const auto stop_command = command_with_id("stop");
    auto stopping = stop_ledger.begin(stop_command, later());
    stop_ledger.mark_dispatched(stopping.handle, {});
    stop_ledger.stop();
    const auto stopped = stop_ledger.query(stop_command.command_id, Clock::now());
    expect(
        stopped.success && stopped.receipt.phase == EquipmentCommandPhase::Uncertain &&
            stopped.receipt.error_code == "equipment_command_session_stopping" &&
            stop_ledger.size() == 1,
        "关闭应保留已派发但结果未知的记录");
    expect(
        stop_ledger.begin(command_with_id("after-stop"), later()).kind ==
            EquipmentCommandBeginKind::Rejected,
        "关闭后未知写入仍阻止新命令");

    EquipmentCommandLedger undispatched_stop;
    undispatched_stop.begin(command_with_id("never-sent"), later());
    undispatched_stop.stop();
    expect(undispatched_stop.size() == 0, "关闭应清掉确定未派发的记录");
}

void test_after_state_releases_the_session_without_dropping_history() {
    EquipmentCommandLedger ledger;
    const auto command = command_with_id("after");
    auto begun = ledger.begin(command, later());
    ledger.mark_dispatched(begun.handle, {});
    ledger.observe(begun.handle, EquipmentCommandStateMatch::After, {}, Clock::now());
    const auto query = ledger.query(command.command_id, Clock::now());
    expect(
        query.success && query.receipt.status == EquipmentCommandStatus::Success &&
            query.receipt.phase == EquipmentCommandPhase::Succeeded &&
            !query.receipt.error_code.has_value(),
        "完整后态应收敛为成功");
    const auto next = ledger.begin(command_with_id("next"), later());
    expect(next.kind == EquipmentCommandBeginKind::DispatchRequired, "成功后可以开始下一条命令");
    expect(ledger.size() == 2, "成功记录仍保留，不因新命令被驱逐");
}

}  // namespace

int main() {
    test_duplicate_id_with_a_different_payload_conflicts();
    test_undispatched_command_is_not_queryable_and_can_be_retried();
    test_same_dispatched_command_returns_the_existing_receipt();
    test_mismatch_cancel_timeout_and_stop_keep_unknown_writes();
    test_after_state_releases_the_session_without_dropping_history();
    if (failures != 0) {
        std::cerr << failures << " 项账本状态断言失败\n";
        return EXIT_FAILURE;
    }
    return EXIT_SUCCESS;
}

//! 顺序执行计划、收敛命令回执并按独立回读证据判定终态。

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::domain::GameState;

use super::preflight::{
    ExpectedExecution, build_execution_preflight, equipment_state_snapshot, expected_execution,
    resource_state_snapshot,
};
use super::readback::{
    build_readback_evidence, execution_state_match_summary, first_equipment_mismatch,
    first_resource_mismatch, readback_evidence_summary, state_mismatch_summary,
};
use super::{
    AppError, AppErrorCode, COMMAND_RECEIPT_POLL_INTERVAL, COMMAND_RECEIPT_POLL_TIMEOUT,
    EXECUTION_SCHEMA_VERSION, ExecutionAction, ExecutionCancellation, ExecutionCommand,
    ExecutionCommandReceipt, ExecutionFinalVerificationStatus, ExecutionPort,
    ExecutionReadbackEvidence, ExecutionReport, ExecutionReportDigestInput, ExecutionReportStatus,
    ExecutionSendResult, ExecutionStatus, ExecutionStepResult, ExecutionStopReason,
    ExecutionTargetIdentity, ExecutionWriteEffect, error_diagnostics, execution_digest,
    request_summary, state_changed_error,
};
use crate::application::{CompiledPlan, OperationProgress, PlanStep};

/// 已确认前缀、未知命令、已确认但未回读，或已观察到不匹配写入。
enum ExecutionProgress {
    ConfirmedPrefix {
        next_index: usize,
    },
    CommandUnknown {
        step_index: usize,
        action: ExecutionAction,
    },
    AcknowledgedUnread {
        step_index: usize,
        action: ExecutionAction,
    },
    ObservedMismatch,
}

/// 使用同一认证目标顺序执行计划，并在每个确认成功的写步骤后立即回读。
#[cfg(test)]
pub(crate) fn execute_plan(
    port: &mut dyn ExecutionPort,
    plan: &CompiledPlan,
    cancellation: &dyn ExecutionCancellation,
) -> Result<ExecutionReport, AppError> {
    let initial_state = port.read_full_state()?;
    let target_identity = port.target_identity(&initial_state)?;
    execute_plan_from_state_with_wait(
        port,
        plan,
        initial_state,
        &target_identity,
        cancellation,
        &|_| {},
        &mut |_| {},
    )
    .map(|outcome| outcome.report)
}

/// 从同一执行会话已经读取的状态开始执行，并再次核对用户确认的目标身份。
pub(crate) fn execute_plan_from_state_with_outcome(
    port: &mut dyn ExecutionPort,
    plan: &CompiledPlan,
    initial_state: crate::application::GameObservation,
    expected_target_identity: &ExecutionTargetIdentity,
    cancellation: &dyn ExecutionCancellation,
    progress: &mut dyn FnMut(OperationProgress),
) -> Result<super::ExecutionOutcome, AppError> {
    execute_plan_from_state_with_wait(
        port,
        plan,
        initial_state,
        expected_target_identity,
        cancellation,
        &std::thread::sleep,
        progress,
    )
}

/// 执行核心通过等待函数隔离真实休眠，使状态机测试能够验证轮询而不延长测试时间。
fn execute_plan_from_state_with_wait(
    port: &mut dyn ExecutionPort,
    plan: &CompiledPlan,
    initial_state: crate::application::GameObservation,
    expected_target_identity: &ExecutionTargetIdentity,
    cancellation: &dyn ExecutionCancellation,
    wait: &dyn Fn(Duration),
    progress: &mut dyn FnMut(OperationProgress),
) -> Result<super::ExecutionOutcome, AppError> {
    progress(OperationProgress::stage("正在预检执行目标、装备状态与资源"));
    let actual_target_identity = port.target_identity(&initial_state)?;
    if actual_target_identity != *expected_target_identity {
        return Err(target_changed_error(
            expected_target_identity,
            &actual_target_identity,
        ));
    }
    let initial_hash = initial_state.source().content_sha256().to_owned();
    if initial_hash != plan.game_state_content_sha256() {
        return Err(state_changed_error(
            plan.game_state_content_sha256(),
            &initial_hash,
        ));
    }

    let expected = expected_execution(&initial_state, plan).map_err(preflight_simulation_error)?;
    let preflight =
        build_execution_preflight(expected_target_identity, plan, &initial_hash, &expected)?;
    port.preflight_plan(&preflight)?;

    let read_scope = initial_state.source().read_scope();
    let mut current_state = initial_state;
    let mut results = Vec::with_capacity(plan.steps().len());
    let mut outcome = (
        ExecutionReportStatus::Success,
        ExecutionStopReason::Completed,
    );
    let mut execution_progress = ExecutionProgress::ConfirmedPrefix { next_index: 0 };

    for (index, step) in plan.steps().iter().cloned().enumerate() {
        let pre_hash = current_state.source().content_sha256().to_owned();
        if cancellation.is_cancelled() {
            append_not_executed(
                &mut results,
                &plan.steps()[index..],
                Some(&pre_hash),
                Some(&pre_hash),
                "执行已在步骤边界取消",
            );
            outcome = (
                ExecutionReportStatus::Cancelled,
                ExecutionStopReason::Cancelled,
            );
            break;
        }

        let total = plan.steps().len();
        let name = match &step {
            PlanStep::Keep { .. } => "保持",
            PlanStep::Unequip { .. } => "卸下",
            PlanStep::Equip { .. } => "装备",
            PlanStep::Compose { .. } => "合成",
            PlanStep::Enhance { .. } => "强化",
            PlanStep::Dismantle { .. } => "拆解",
        };
        progress(OperationProgress::counted(
            format!("正在执行第{}/{total}步：{name}", index + 1),
            index,
            total,
        ));
        let Some(action) = ExecutionAction::from_step(&step) else {
            results.push(keep_result(step, &pre_hash));
            execution_progress = ExecutionProgress::ConfirmedPrefix {
                next_index: index + 1,
            };
            progress(OperationProgress::counted(
                "当前槽位保持不变",
                index + 1,
                total,
            ));
            continue;
        };
        let command = ExecutionCommand::new(
            expected_target_identity,
            plan.content_sha256(),
            step.sequence(),
            &pre_hash,
            action.clone(),
        )?;
        let request_summary = request_summary(&action);
        let receipt = match port.send_command(&command) {
            ExecutionSendResult::Receipt(receipt) => receipt,
            ExecutionSendResult::NotSent(error) => {
                results.push(send_failure_result(
                    step,
                    &command,
                    request_summary,
                    &pre_hash,
                    error,
                ));
                append_not_executed(
                    &mut results,
                    &plan.steps()[index + 1..],
                    Some(&pre_hash),
                    Some(&pre_hash),
                    "前序命令发送失败",
                );
                outcome = (
                    ExecutionReportStatus::Failed,
                    ExecutionStopReason::CommandFailed,
                );
                break;
            }
        };
        let mut receipt = normalize_receipt(&command, receipt);

        let mut cancellation_observed = false;
        if receipt.status == ExecutionStatus::Unknown {
            progress(OperationProgress::counted(
                format!("正在等待第{}步命令回执：{name}", index + 1),
                index,
                total,
            ));
            (receipt, cancellation_observed) = resolve_unknown_receipt(
                port,
                cancellation,
                &command,
                receipt,
                COMMAND_RECEIPT_POLL_TIMEOUT,
                wait,
            );
        }

        if receipt.status == ExecutionStatus::Success {
            progress(OperationProgress::counted(
                format!("正在回读并核验第{}步：{name}", index + 1),
                index,
                total,
            ));
        }
        match receipt.status {
            ExecutionStatus::Success => match port.read_state_with_scope(read_scope, &mut |event| {
                progress(OperationProgress::counted(
                    format!("核验第{}步：{}", index + 1, event.message),
                    index,
                    total,
                ));
            }) {
                Ok(post_state) => {
                    let post_hash = post_state.source().content_sha256().to_owned();
                    let step_expected = &expected.steps()[index];
                    let evidence = build_readback_evidence(
                        &action,
                        &step_expected.equipment_before,
                        &step_expected.equipment_after,
                        &step_expected.resources_before,
                        &step_expected.resources_after,
                        step_expected.verify_resources,
                        &post_state,
                    );
                    let summary = readback_evidence_summary(&action, &evidence);
                    if evidence.matches_expected() {
                        results.push(receipt_result(
                            step,
                            &command,
                            request_summary,
                            &pre_hash,
                            Some(post_hash.clone()),
                            receipt,
                            ExecutionStatus::Success,
                            Some(summary),
                            Some(evidence),
                            true,
                            ExecutionWriteEffect::Verified,
                            "命令执行并通过全量回读核验",
                        ));
                        current_state = post_state;
                        execution_progress = ExecutionProgress::ConfirmedPrefix {
                            next_index: index + 1,
                        };
                        progress(OperationProgress::counted(
                            format!("第{}步已核验：{name}", index + 1),
                            index + 1,
                            total,
                        ));
                        if cancellation_observed {
                            append_not_executed(
                                &mut results,
                                &plan.steps()[index + 1..],
                                Some(&post_hash),
                                Some(&post_hash),
                                "取消请求已锁存，当前命令完成后停止",
                            );
                            if index + 1 < plan.steps().len() {
                                outcome = (
                                    ExecutionReportStatus::Cancelled,
                                    ExecutionStopReason::Cancelled,
                                );
                            }
                            break;
                        }
                    } else {
                        let actual_snapshot = equipment_state_snapshot(&post_state);
                        let actual_resources = resource_state_snapshot(&post_state);
                        let write_effect = if actual_snapshot != *step_expected.equipment_before
                            || (step_expected.verify_resources
                                && actual_resources != *step_expected.resources_before)
                        {
                            ExecutionWriteEffect::StateChangedMismatch
                        } else {
                            ExecutionWriteEffect::Possible
                        };
                        receipt.message = Some("回读状态不符合完整步骤预期".to_owned());
                        results.push(receipt_result(
                            step,
                            &command,
                            request_summary,
                            &pre_hash,
                            Some(post_hash.clone()),
                            receipt,
                            ExecutionStatus::Failed,
                            Some(summary),
                            Some(evidence),
                            true,
                            write_effect,
                            "回读状态不符合完整步骤预期",
                        ));
                        execution_progress = ExecutionProgress::ObservedMismatch;
                        append_not_executed(
                            &mut results,
                            &plan.steps()[index + 1..],
                            Some(&post_hash),
                            Some(&post_hash),
                            "前序步骤全量回读不匹配",
                        );
                        outcome = (
                            ExecutionReportStatus::Failed,
                            ExecutionStopReason::ReadbackMismatch,
                        );
                        break;
                    }
                }
                Err(error) => {
                    results.push(readback_failure_result(
                        step,
                        &command,
                        request_summary,
                        &pre_hash,
                        receipt,
                        error,
                    ));
                    execution_progress = ExecutionProgress::AcknowledgedUnread {
                        step_index: index,
                        action,
                    };
                    append_not_executed(
                        &mut results,
                        &plan.steps()[index + 1..],
                        None,
                        None,
                        "前序写入无法完成回读确认",
                    );
                    outcome = (
                        ExecutionReportStatus::Unknown,
                        ExecutionStopReason::ReadbackFailed,
                    );
                    break;
                }
            },
            ExecutionStatus::Failed => {
                results.push(receipt_result(
                    step,
                    &command,
                    request_summary,
                    &pre_hash,
                    Some(pre_hash.clone()),
                    receipt,
                    ExecutionStatus::Failed,
                    None,
                    None,
                    false,
                    ExecutionWriteEffect::None,
                    "命令返回明确失败状态",
                ));
                append_not_executed(
                    &mut results,
                    &plan.steps()[index + 1..],
                    Some(&pre_hash),
                    Some(&pre_hash),
                    "前序命令失败",
                );
                outcome = (
                    ExecutionReportStatus::Failed,
                    ExecutionStopReason::CommandFailed,
                );
                break;
            }
            ExecutionStatus::Unknown => {
                results.push(receipt_result(
                    step,
                    &command,
                    request_summary,
                    &pre_hash,
                    None,
                    receipt,
                    ExecutionStatus::Unknown,
                    None,
                    None,
                    false,
                    ExecutionWriteEffect::Possible,
                    "命令最终状态未知，已停止后续写入",
                ));
                execution_progress = ExecutionProgress::CommandUnknown {
                    step_index: index,
                    action,
                };
                append_not_executed(
                    &mut results,
                    &plan.steps()[index + 1..],
                    None,
                    None,
                    "前序命令状态未知",
                );
                outcome = (
                    ExecutionReportStatus::Unknown,
                    ExecutionStopReason::CommandUnknown,
                );
                break;
            }
            ExecutionStatus::NotExecuted => unreachable!("回执已在进入状态机前校验"),
        }
    }

    progress(OperationProgress::stage("正在读取并核验独立最终状态"));
    let final_readback = port.read_state_with_scope(read_scope, &mut |event| {
        progress(OperationProgress::stage(format!(
            "最终核验：{}",
            event.message
        )));
    });
    let (final_state_hash, final_verification_status, final_verification_summary) =
        match &final_readback {
            Ok(final_state) => {
                let final_state_hash = Some(final_state.source().content_sha256().to_owned());
                let actual = equipment_state_snapshot(final_state);
                let actual_resources = resource_state_snapshot(final_state);
                if outcome.1 == ExecutionStopReason::Completed {
                    let final_index = expected.checkpoint_count() - 1;
                    let expected_equipment = expected.equipment_at(final_index);
                    let expected_resources = expected.resources_at(final_index);
                    let verify_resources = expected.verify_resources_at(final_index);
                    let mismatch =
                        first_equipment_mismatch(expected_equipment, &actual).or_else(|| {
                            verify_resources
                                .then(|| {
                                    first_resource_mismatch(expected_resources, &actual_resources)
                                })
                                .flatten()
                        });
                    match mismatch {
                        None => (
                            final_state_hash,
                            ExecutionFinalVerificationStatus::Verified,
                            Some(execution_state_match_summary(
                                expected_equipment,
                                verify_resources,
                            )),
                        ),
                        Some(mismatch) => {
                            outcome = (
                                ExecutionReportStatus::Failed,
                                ExecutionStopReason::FinalStateMismatch,
                            );
                            (
                                final_state_hash,
                                ExecutionFinalVerificationStatus::Mismatch,
                                Some(state_mismatch_summary(&mismatch)),
                            )
                        }
                    }
                } else {
                    match &execution_progress {
                        ExecutionProgress::CommandUnknown { step_index, action } => {
                            let (verification_status, summary) = observe_unconfirmed_step(
                                &mut results,
                                *step_index,
                                action.clone(),
                                &expected,
                                final_state,
                            );
                            (final_state_hash, verification_status, Some(summary))
                        }
                        ExecutionProgress::AcknowledgedUnread { step_index, action } => {
                            let (relation, summary) = observe_acknowledged_unread_step(
                                &mut results,
                                *step_index,
                                action.clone(),
                                &expected,
                                final_state,
                            );
                            match relation {
                                ObservedStepRelation::ExpectedAfter
                                    if *step_index + 1 == plan.steps().len() =>
                                {
                                    outcome = (
                                        ExecutionReportStatus::Success,
                                        ExecutionStopReason::Completed,
                                    );
                                    (
                                        final_state_hash,
                                        ExecutionFinalVerificationStatus::Verified,
                                        Some(summary),
                                    )
                                }
                                ObservedStepRelation::ExpectedAfter => {
                                    outcome = (
                                        ExecutionReportStatus::Failed,
                                        ExecutionStopReason::ReadbackFailed,
                                    );
                                    (
                                        final_state_hash,
                                        ExecutionFinalVerificationStatus::Incomplete,
                                        Some(summary),
                                    )
                                }
                                ObservedStepRelation::ExpectedBefore
                                | ObservedStepRelation::Other => {
                                    outcome = (
                                        ExecutionReportStatus::Failed,
                                        ExecutionStopReason::ReadbackMismatch,
                                    );
                                    (
                                        final_state_hash,
                                        ExecutionFinalVerificationStatus::Mismatch,
                                        Some(summary),
                                    )
                                }
                            }
                        }
                        ExecutionProgress::ConfirmedPrefix { next_index } => {
                            let expected_equipment = expected.equipment_at(*next_index);
                            let expected_resources = expected.resources_at(*next_index);
                            let verify_resources = expected.verify_resources_at(*next_index);
                            let mismatch = first_equipment_mismatch(expected_equipment, &actual)
                                .or_else(|| {
                                    verify_resources
                                        .then(|| {
                                            first_resource_mismatch(
                                                expected_resources,
                                                &actual_resources,
                                            )
                                        })
                                        .flatten()
                                });
                            match mismatch {
                                None => (
                                    final_state_hash,
                                    ExecutionFinalVerificationStatus::Incomplete,
                                    Some("执行提前停止；独立终态与已确认进度一致".to_owned()),
                                ),
                                Some(mismatch) => (
                                    final_state_hash,
                                    ExecutionFinalVerificationStatus::Mismatch,
                                    Some(format!(
                                        "执行提前停止后的独立终态存在差异：{}",
                                        state_mismatch_summary(&mismatch)
                                    )),
                                ),
                            }
                        }
                        ExecutionProgress::ObservedMismatch => (
                            final_state_hash,
                            ExecutionFinalVerificationStatus::Incomplete,
                            Some(
                                "执行提前停止；已取得独立终态，但未知命令或不匹配写入无法按计划收敛"
                                    .to_owned(),
                            ),
                        ),
                    }
                }
            }
            Err(error) => {
                if outcome.1 == ExecutionStopReason::Completed {
                    outcome = (
                        ExecutionReportStatus::Unknown,
                        ExecutionStopReason::FinalReadbackFailed,
                    );
                }
                (
                    None,
                    ExecutionFinalVerificationStatus::Unavailable,
                    Some(format!(
                        "独立终态读取失败：{} [{}]",
                        error.message(),
                        error.code().as_str()
                    )),
                )
            }
        };

    let report = build_report(
        plan,
        expected_target_identity.clone(),
        initial_hash,
        ExecutionCompletion {
            final_state_content_sha256: final_state_hash,
            status: outcome.0,
            stop_reason: outcome.1,
            final_verification_status,
            final_verification_summary,
        },
        results,
    )?;
    Ok(super::ExecutionOutcome {
        report,
        final_state: final_readback.ok(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservedStepRelation {
    ExpectedBefore,
    ExpectedAfter,
    Other,
}

fn observe_acknowledged_unread_step(
    results: &mut [ExecutionStepResult],
    step_index: usize,
    action: ExecutionAction,
    expected: &ExpectedExecution,
    final_state: &GameState,
) -> (ObservedStepRelation, String) {
    let step = &expected.steps()[step_index];
    let before = &step.equipment_before;
    let expected_after = &step.equipment_after;
    let actual = equipment_state_snapshot(final_state);
    let actual_resources = resource_state_snapshot(final_state);
    let evidence = build_readback_evidence(
        &action,
        before,
        expected_after,
        &step.resources_before,
        &step.resources_after,
        step.verify_resources,
        final_state,
    );
    let matches_expected_after = evidence.matches_expected();
    let matches_before = first_equipment_mismatch(before, &actual).is_none()
        && (!expected.verify_resources_at(step_index)
            || first_resource_mismatch(&step.resources_before, &actual_resources).is_none());
    let state_summary = readback_evidence_summary(&action, &evidence);
    let result = &mut results[step_index];
    result.post_state_content_sha256 = Some(final_state.source().content_sha256().to_owned());
    result.readback_evidence = Some(evidence);

    if matches_expected_after {
        result.status = ExecutionStatus::Success;
        result.write_effect = ExecutionWriteEffect::Verified;
        result.error_code = None;
        result.readback_summary = Some(format!("首次回读失败；独立终态确认：{state_summary}"));
        result.message = "命令已确认成功，并由独立终态完成全量回读核验".to_owned();
        return (
            ObservedStepRelation::ExpectedAfter,
            "命令已确认成功；独立终态与该步骤预期后态一致".to_owned(),
        );
    }
    if matches_before {
        result.status = ExecutionStatus::Failed;
        result.write_effect = ExecutionWriteEffect::Possible;
        result.error_code = Some(AppErrorCode::EquipmentStateChanged.as_str().to_owned());
        result.readback_summary = Some("命令已确认成功，但独立终态仍与步骤前态一致".to_owned());
        result.message = "命令成功回执与独立终态不一致".to_owned();
        return (
            ObservedStepRelation::ExpectedBefore,
            "命令已确认成功，但独立终态未观察到该步骤结果".to_owned(),
        );
    }

    result.status = ExecutionStatus::Failed;
    result.write_effect = ExecutionWriteEffect::StateChangedMismatch;
    result.error_code = Some(AppErrorCode::EquipmentStateChanged.as_str().to_owned());
    result.readback_summary = Some(format!(
        "命令已确认成功，但独立终态既不符合步骤前态，也不符合预期后态：{state_summary}"
    ));
    result.message = "命令成功回执后的独立终态发生非预期变化".to_owned();
    (
        ObservedStepRelation::Other,
        format!("命令成功回执后的独立终态发生偏离：{state_summary}"),
    )
}

fn observe_unconfirmed_step(
    results: &mut [ExecutionStepResult],
    step_index: usize,
    action: ExecutionAction,
    expected: &ExpectedExecution,
    final_state: &GameState,
) -> (ExecutionFinalVerificationStatus, String) {
    let step = &expected.steps()[step_index];
    let before = &step.equipment_before;
    let expected_after = &step.equipment_after;
    let actual = equipment_state_snapshot(final_state);
    let actual_resources = resource_state_snapshot(final_state);
    let evidence = build_readback_evidence(
        &action,
        before,
        expected_after,
        &step.resources_before,
        &step.resources_after,
        step.verify_resources,
        final_state,
    );
    let matches_expected_after = evidence.matches_expected();
    let matches_before = first_equipment_mismatch(before, &actual).is_none()
        && (!expected.verify_resources_at(step_index)
            || first_resource_mismatch(&step.resources_before, &actual_resources).is_none());
    let state_summary = readback_evidence_summary(&action, &evidence);
    let result = &mut results[step_index];
    result.post_state_content_sha256 = Some(final_state.source().content_sha256().to_owned());
    result.readback_evidence = Some(evidence);

    if matches_expected_after {
        result.write_effect = ExecutionWriteEffect::ExpectedPostStateObserved;
        result.readback_summary = Some(format!("命令回执仍未知；{state_summary}"));
        return (
            ExecutionFinalVerificationStatus::UnconfirmedStepReached,
            "命令回执仍未知；独立终态与该步骤预期后态一致，后续写入未继续".to_owned(),
        );
    }
    if matches_before {
        result.write_effect = ExecutionWriteEffect::Possible;
        result.readback_summary =
            Some("命令回执仍未知；独立终态仍与步骤前态一致，但不能排除命令稍后生效".to_owned());
        return (
            ExecutionFinalVerificationStatus::UnconfirmedStepNotObserved,
            "命令回执仍未知；独立终态尚未观察到该步骤，且不能排除命令稍后生效".to_owned(),
        );
    }

    result.write_effect = ExecutionWriteEffect::StateChangedMismatch;
    result.readback_summary = Some(format!(
        "命令回执仍未知；独立终态既不符合步骤前态，也不符合预期后态：{state_summary}"
    ));
    (
        ExecutionFinalVerificationStatus::Mismatch,
        format!("未知命令后的独立终态发生偏离：{state_summary}"),
    )
}

fn target_changed_error(
    expected: &ExecutionTargetIdentity,
    actual: &ExecutionTargetIdentity,
) -> AppError {
    AppError::from_source(
        "plan.execute.target",
        AppErrorCode::EquipmentStateChanged,
        "当前执行目标与用户确认的设备或持有资产不一致",
        std::io::Error::other("execution target identity changed after confirmation"),
    )
    .with_context(
        "expected_target_fingerprint_sha256",
        expected.fingerprint_sha256(),
    )
    .with_context(
        "actual_target_fingerprint_sha256",
        actual.fingerprint_sha256(),
    )
}

fn preflight_simulation_error(message: String) -> AppError {
    AppError::from_source(
        "plan.execute.preflight",
        AppErrorCode::FullCheckFailed,
        "完整计划无法形成确定的装备状态模拟",
        std::io::Error::other(message.clone()),
    )
    .with_context("simulation_error", message)
}

pub(super) fn resolve_unknown_receipt(
    port: &mut dyn ExecutionPort,
    cancellation: &dyn ExecutionCancellation,
    command: &ExecutionCommand,
    initial: ExecutionCommandReceipt,
    poll_timeout: Duration,
    wait: &dyn Fn(Duration),
) -> (ExecutionCommandReceipt, bool) {
    let started_at = Instant::now();
    let mut latest = initial.clone();
    let mut query_count = 0_u32;
    let mut observing_query_count = 0_u32;

    loop {
        let remaining = poll_timeout.saturating_sub(started_at.elapsed());
        let cancellation_observed = cancellation.is_cancelled();
        if remaining.is_zero() {
            return (
                timeout_receipt(
                    latest,
                    poll_timeout,
                    started_at,
                    query_count,
                    observing_query_count,
                ),
                cancellation_observed,
            );
        }
        let result = if cancellation_observed {
            port.cancel_command(command.command_id(), remaining)
        } else {
            query_count = query_count.saturating_add(1);
            port.query_command(command.command_id(), remaining)
        };
        let follow_up_failed = result.is_err();
        let mut receipt = match result {
            Ok(receipt) => normalize_receipt(command, receipt),
            Err(error) => follow_up_error_receipt(command, &latest, error),
        };
        attach_initial_receipt_diagnostics(&mut receipt, &initial);

        if !cancellation_observed && !follow_up_failed && receipt_is_observing(&receipt) {
            observing_query_count = observing_query_count.saturating_add(1);
        }
        receipt
            .diagnostics
            .insert("resolution.query_count".to_owned(), query_count.to_string());
        receipt.diagnostics.insert(
            "resolution.observing_query_count".to_owned(),
            observing_query_count.to_string(),
        );
        receipt.diagnostics.insert(
            "resolution.elapsed_ms".to_owned(),
            started_at.elapsed().as_millis().to_string(),
        );

        if cancellation_observed || follow_up_failed || !receipt_is_observing(&receipt) {
            return (receipt, cancellation_observed);
        }

        let elapsed = started_at.elapsed();
        if elapsed >= poll_timeout {
            receipt
                .diagnostics
                .insert("resolution.timeout_reached".to_owned(), "true".to_owned());
            receipt.diagnostics.insert(
                "resolution.timeout_ms".to_owned(),
                poll_timeout.as_millis().to_string(),
            );
            receipt
                .diagnostics
                .insert("resolution.budget_exhausted".to_owned(), "true".to_owned());
            return (receipt, cancellation.is_cancelled());
        }

        latest = receipt;
        wait(COMMAND_RECEIPT_POLL_INTERVAL.min(poll_timeout - elapsed));
    }
}

fn timeout_receipt(
    mut receipt: ExecutionCommandReceipt,
    poll_timeout: Duration,
    started_at: Instant,
    query_count: u32,
    observing_query_count: u32,
) -> ExecutionCommandReceipt {
    receipt
        .diagnostics
        .insert("resolution.query_count".to_owned(), query_count.to_string());
    receipt.diagnostics.insert(
        "resolution.observing_query_count".to_owned(),
        observing_query_count.to_string(),
    );
    receipt.diagnostics.insert(
        "resolution.elapsed_ms".to_owned(),
        started_at.elapsed().as_millis().to_string(),
    );
    receipt
        .diagnostics
        .insert("resolution.timeout_reached".to_owned(), "true".to_owned());
    receipt.diagnostics.insert(
        "resolution.timeout_ms".to_owned(),
        poll_timeout.as_millis().to_string(),
    );
    receipt
        .diagnostics
        .insert("resolution.budget_exhausted".to_owned(), "true".to_owned());
    receipt
}

/// 只有设备端明确声明仍在观察的未知回执才允许继续轮询。
fn receipt_is_observing(receipt: &ExecutionCommandReceipt) -> bool {
    receipt.status == ExecutionStatus::Unknown
        && !receipt.diagnostics.contains_key("actual_command_id")
        && !receipt.diagnostics.contains_key("invalid_receipt_status")
        && receipt
            .diagnostics
            .get("phase")
            .is_some_and(|phase| phase == "observing")
}

/// 将首次发送回执保留为最终收敛结果的审计证据。
fn attach_initial_receipt_diagnostics(
    receipt: &mut ExecutionCommandReceipt,
    initial: &ExecutionCommandReceipt,
) {
    for (key, value) in &initial.diagnostics {
        receipt
            .diagnostics
            .insert(format!("initial.{key}"), value.clone());
    }
    if let Some(summary) = &initial.response_summary {
        receipt
            .diagnostics
            .insert("initial.response_summary".to_owned(), summary.clone());
    }
    if let Some(error_code) = &initial.error_code {
        receipt
            .diagnostics
            .insert("initial.error_code".to_owned(), error_code.clone());
    }
    if let Some(message) = &initial.message {
        receipt
            .diagnostics
            .insert("initial.message".to_owned(), message.clone());
    }
}

/// 查询失败时保留最近一份原命令回执，并追加这次查询的错误链。
fn follow_up_error_receipt(
    command: &ExecutionCommand,
    latest: &ExecutionCommandReceipt,
    error: AppError,
) -> ExecutionCommandReceipt {
    let mut diagnostics = latest.diagnostics.clone();
    diagnostics.insert("follow_up_stage".to_owned(), error.stage().to_owned());
    for (key, value) in error.context() {
        diagnostics.insert(format!("follow_up_context.{key}"), value.clone());
    }
    ExecutionCommandReceipt::new(
        command.command_id(),
        ExecutionStatus::Unknown,
        latest.response_summary.clone(),
        Some(error.code().as_str().to_owned()),
        Some(error.message().to_owned()),
        diagnostics,
    )
}

fn normalize_receipt(
    command: &ExecutionCommand,
    receipt: ExecutionCommandReceipt,
) -> ExecutionCommandReceipt {
    if receipt.command_id != command.command_id() {
        let mut diagnostics = receipt.diagnostics;
        diagnostics.insert(
            "expected_command_id".to_owned(),
            command.command_id().to_owned(),
        );
        diagnostics.insert("actual_command_id".to_owned(), receipt.command_id);
        return ExecutionCommandReceipt::new(
            command.command_id(),
            ExecutionStatus::Unknown,
            receipt.response_summary,
            Some(AppErrorCode::RuntimeIncompatible.as_str().to_owned()),
            Some("运行态回执的命令 ID 与请求不一致".to_owned()),
            diagnostics,
        );
    }
    if receipt.status == ExecutionStatus::NotExecuted {
        let mut diagnostics = receipt.diagnostics;
        diagnostics.insert(
            "invalid_receipt_status".to_owned(),
            ExecutionStatus::NotExecuted.as_str().to_owned(),
        );
        return ExecutionCommandReceipt::new(
            command.command_id(),
            ExecutionStatus::Unknown,
            receipt.response_summary,
            Some(AppErrorCode::RuntimeIncompatible.as_str().to_owned()),
            Some("运行态回执返回了不允许的未执行状态".to_owned()),
            diagnostics,
        );
    }
    receipt
}

fn keep_result(step: PlanStep, state_hash: &str) -> ExecutionStepResult {
    ExecutionStepResult {
        sequence: step.sequence(),
        step_kind: step.stable_key(),
        step,
        status: ExecutionStatus::Success,
        command_id: None,
        pre_state_content_sha256: Some(state_hash.to_owned()),
        post_state_content_sha256: Some(state_hash.to_owned()),
        request_summary: None,
        response_summary: None,
        readback_summary: Some("当前槽位按计划保持，不需要写入".to_owned()),
        readback_evidence: None,
        write_acknowledged: false,
        write_effect: ExecutionWriteEffect::None,
        error_code: None,
        message: "保持步骤无需发送命令".to_owned(),
        diagnostics: BTreeMap::new(),
    }
}

fn send_failure_result(
    step: PlanStep,
    command: &ExecutionCommand,
    request_summary: String,
    state_hash: &str,
    error: AppError,
) -> ExecutionStepResult {
    let diagnostics = error_diagnostics(&error);
    ExecutionStepResult {
        sequence: step.sequence(),
        step_kind: step.stable_key(),
        step,
        status: ExecutionStatus::Failed,
        command_id: Some(command.command_id().to_owned()),
        pre_state_content_sha256: Some(state_hash.to_owned()),
        post_state_content_sha256: Some(state_hash.to_owned()),
        request_summary: Some(request_summary),
        response_summary: None,
        readback_summary: Some("端口确认命令未发送".to_owned()),
        readback_evidence: None,
        write_acknowledged: false,
        write_effect: ExecutionWriteEffect::None,
        error_code: Some(error.code().as_str().to_owned()),
        message: error.message().to_owned(),
        diagnostics,
    }
}

#[allow(clippy::too_many_arguments)]
fn receipt_result(
    step: PlanStep,
    command: &ExecutionCommand,
    request_summary: String,
    pre_state_hash: &str,
    post_state_hash: Option<String>,
    receipt: ExecutionCommandReceipt,
    status: ExecutionStatus,
    readback_summary: Option<String>,
    readback_evidence: Option<ExecutionReadbackEvidence>,
    write_acknowledged: bool,
    write_effect: ExecutionWriteEffect,
    default_message: &str,
) -> ExecutionStepResult {
    ExecutionStepResult {
        sequence: step.sequence(),
        step_kind: step.stable_key(),
        step,
        status,
        command_id: Some(command.command_id().to_owned()),
        pre_state_content_sha256: Some(pre_state_hash.to_owned()),
        post_state_content_sha256: post_state_hash,
        request_summary: Some(request_summary),
        response_summary: receipt.response_summary,
        readback_summary,
        readback_evidence,
        write_acknowledged,
        write_effect,
        error_code: receipt.error_code,
        message: receipt
            .message
            .unwrap_or_else(|| default_message.to_owned()),
        diagnostics: receipt.diagnostics,
    }
}

fn readback_failure_result(
    step: PlanStep,
    command: &ExecutionCommand,
    request_summary: String,
    pre_state_hash: &str,
    receipt: ExecutionCommandReceipt,
    error: AppError,
) -> ExecutionStepResult {
    let mut diagnostics = receipt.diagnostics;
    diagnostics.extend(
        error_diagnostics(&error)
            .into_iter()
            .map(|(key, value)| (format!("readback.{key}"), value)),
    );
    ExecutionStepResult {
        sequence: step.sequence(),
        step_kind: step.stable_key(),
        step,
        status: ExecutionStatus::Unknown,
        command_id: Some(command.command_id().to_owned()),
        pre_state_content_sha256: Some(pre_state_hash.to_owned()),
        post_state_content_sha256: None,
        request_summary: Some(request_summary),
        response_summary: receipt.response_summary,
        readback_summary: Some("命令已确认成功，但完整状态回读失败".to_owned()),
        readback_evidence: None,
        write_acknowledged: true,
        write_effect: ExecutionWriteEffect::Possible,
        error_code: Some(error.code().as_str().to_owned()),
        message: error.message().to_owned(),
        diagnostics,
    }
}

fn append_not_executed(
    results: &mut Vec<ExecutionStepResult>,
    steps: &[PlanStep],
    pre_state_content_sha256: Option<&str>,
    post_state_content_sha256: Option<&str>,
    reason: &str,
) {
    results.extend(steps.iter().cloned().map(|step| ExecutionStepResult {
        sequence: step.sequence(),
        step_kind: step.stable_key(),
        step,
        status: ExecutionStatus::NotExecuted,
        command_id: None,
        pre_state_content_sha256: pre_state_content_sha256.map(str::to_owned),
        post_state_content_sha256: post_state_content_sha256.map(str::to_owned),
        request_summary: None,
        response_summary: None,
        readback_summary: None,
        readback_evidence: None,
        write_acknowledged: false,
        write_effect: ExecutionWriteEffect::None,
        error_code: None,
        message: reason.to_owned(),
        diagnostics: BTreeMap::new(),
    }));
}
struct ExecutionCompletion {
    final_state_content_sha256: Option<String>,
    status: ExecutionReportStatus,
    stop_reason: ExecutionStopReason,
    final_verification_status: ExecutionFinalVerificationStatus,
    final_verification_summary: Option<String>,
}

fn build_report(
    plan: &CompiledPlan,
    target_identity: ExecutionTargetIdentity,
    initial_state_content_sha256: String,
    completion: ExecutionCompletion,
    steps: Vec<ExecutionStepResult>,
) -> Result<ExecutionReport, AppError> {
    let ExecutionCompletion {
        final_state_content_sha256,
        status,
        stop_reason,
        final_verification_status,
        final_verification_summary,
    } = completion;
    let acknowledged_write_count = steps.iter().filter(|step| step.write_acknowledged).count();
    let observed_state_change_count = steps
        .iter()
        .filter(|step| step.write_effect.observed_state_change())
        .count();
    let verified_write_count = steps
        .iter()
        .filter(|step| step.write_effect == ExecutionWriteEffect::Verified)
        .count();
    let may_have_writes = steps.iter().any(|step| step.write_effect.may_have_write());
    let digest_input = ExecutionReportDigestInput {
        schema_version: EXECUTION_SCHEMA_VERSION,
        plan_schema_version: plan.schema_version(),
        target_identity: &target_identity,
        plan_hash: plan.content_sha256(),
        initial_state_content_sha256: &initial_state_content_sha256,
        final_state_content_sha256: &final_state_content_sha256,
        status,
        stop_reason,
        acknowledged_write_count,
        observed_state_change_count,
        verified_write_count,
        may_have_writes,
        final_verification_status,
        final_verification_summary: &final_verification_summary,
        steps: &steps,
    };
    let content_sha256 = execution_digest("execution.report", &digest_input)?;
    Ok(ExecutionReport {
        schema_version: EXECUTION_SCHEMA_VERSION,
        plan_schema_version: plan.schema_version(),
        target_identity,
        plan_hash: plan.content_sha256().to_owned(),
        initial_state_content_sha256,
        final_state_content_sha256,
        status,
        stop_reason,
        acknowledged_write_count,
        observed_state_change_count,
        verified_write_count,
        may_have_writes,
        final_verification_status,
        final_verification_summary,
        steps,
        content_sha256,
    })
}

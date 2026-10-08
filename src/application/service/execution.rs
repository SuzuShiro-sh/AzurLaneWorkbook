//! 编排工作簿执行的确认、备份、单次执行、结果持久化与错误证据。

use super::super::execution::{ExecutionOutcome, execute_plan_from_state_with_outcome};
use super::super::{
    AppError, AppErrorCode, CheckReport, ExecutionCancellation, ExecutionHistoryReport,
    ExecutionReport, ExecutionResultsWriteReport, ExecutionTargetIdentity, NoExecutionCancellation,
    WorkbookBackupReport, WorkbookExecutionReport, WorkbookRef, compile_plan_with_inventory,
    map_plan_check_error, project_execution_report_rows, workbook_plan_has_modifications,
};
use super::{GameOperationAccess, WorkbookExecuteService};
use crate::application::{ExecutionPort, ExecutionResultsPort, WorkbookPlanInputs, WorkbookPort};
use crate::application::{SessionCleanup, StageResult};

/// 工作簿执行在读取修改列后的稳定结果。
/// 已结束的执行报告留在变体里，调用方一次匹配就能同时看到业务结果和收尾成败。
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum WorkbookExecuteOutcome {
    /// 修改列存在要求，已完成备份、执行、结果写回和会话清理。
    Completed(Box<WorkbookExecutionReport>),
    /// 修改列没有配装、强化或拆解要求，未连接游戏也未写回执行结果。
    NoModifications,
    /// 游戏执行已经结束，历史、写回或会话清理没有全部完成。
    Finished {
        execution: ExecutionReport,
        backup: WorkbookBackupReport,
        history: StageResult<ExecutionHistoryReport>,
        /// 历史未保存时写回保持未尝试。
        writeback: StageResult<ExecutionResultsWriteReport>,
        cleanup: SessionCleanup,
    },
}

impl WorkbookExecuteOutcome {
    /// 返回已完成的执行报告；没有修改列要求或收尾未完成时为空。
    pub const fn completed(&self) -> Option<&WorkbookExecutionReport> {
        match self {
            Self::Completed(report) => Some(&**report),
            Self::NoModifications | Self::Finished { .. } => None,
        }
    }

    /// 日志终态同时看执行报告和收尾是否完成，不让后一步覆盖前一步。
    pub fn log_terminal(&self) -> crate::application::OperationTerminal {
        match self {
            Self::NoModifications => crate::application::OperationTerminal::Succeeded,
            Self::Completed(report) => {
                crate::application::OperationTerminal::from_execution(report.execution().status())
            }
            Self::Finished { execution, .. } => {
                crate::application::OperationTerminal::from_execution(execution.status())
                    .with_incomplete_tail()
            }
        }
    }
}

struct ExecutionPersistence {
    execution: ExecutionReport,
    backup: WorkbookBackupReport,
    history: StageResult<ExecutionHistoryReport>,
    writeback: StageResult<ExecutionResultsWriteReport>,
}

fn projection_write_error(
    message: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    AppError::from_source(
        "execution.workbook.project",
        AppErrorCode::ExecutionResultsWriteFailed,
        message,
        source,
    )
}

fn execute_outcome(
    persisted: ExecutionPersistence,
    cleanup: SessionCleanup,
) -> WorkbookExecuteOutcome {
    let ExecutionPersistence {
        execution,
        backup,
        history,
        writeback,
    } = persisted;
    match (history, writeback, cleanup) {
        (
            StageResult::Completed(history),
            StageResult::Completed(writeback),
            SessionCleanup::Completed,
        ) => WorkbookExecuteOutcome::Completed(Box::new(WorkbookExecutionReport::new(
            backup, execution, history, writeback,
        ))),
        (history, writeback, cleanup) => WorkbookExecuteOutcome::Finished {
            execution,
            backup,
            history,
            writeback,
            cleanup,
        },
    }
}

/// 完整执行流程在同一会话内复核的计划与稳定目标身份。
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutionConfirmation {
    target_identity: ExecutionTargetIdentity,
    check_report: CheckReport,
    source_package_sha256: String,
}

impl ExecutionConfirmation {
    /// 返回设备、包、运行模块与舰船归属联合身份的匿名化稳定指纹。
    const fn target_identity(&self) -> &ExecutionTargetIdentity {
        &self.target_identity
    }

    /// 返回执行时必须重新编译并精确匹配的计划摘要。
    fn plan_hash(&self) -> &str {
        self.check_report.plan().content_sha256()
    }
}

fn missing_execution_port_error() -> AppError {
    AppError::from_source(
        "plan.execute",
        AppErrorCode::CapabilityMissing,
        "当前运行态连接未提供配装执行能力",
        std::io::Error::other("execution port is not configured"),
    )
    .with_context("missing_port", "execution")
}

fn plan_confirmation_changed_error(expected: &str, actual: &str) -> AppError {
    AppError::from_source(
        "plan.execute.confirmation",
        AppErrorCode::EquipmentStateChanged,
        "当前工作簿或游戏状态已经改变，请重新检查并确认计划",
        std::io::Error::other("recompiled plan does not match confirmed plan hash"),
    )
    .with_context("expected_plan_hash", expected)
    .with_context("actual_plan_hash", actual)
}

fn target_confirmation_changed_error(
    expected: &ExecutionTargetIdentity,
    actual: &ExecutionTargetIdentity,
) -> AppError {
    AppError::from_source(
        "plan.execute.confirmation",
        AppErrorCode::EquipmentStateChanged,
        "当前执行目标与用户确认的设备或持有资产不一致",
        std::io::Error::other("execution target does not match confirmed target identity"),
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

/// 用这次操作已经取得的执行连接生成确认。
fn prepare_workbook_execution(
    results: &dyn ExecutionResultsPort,
    execution: &mut dyn ExecutionPort,
    workbook: &WorkbookRef,
    inputs: &WorkbookPlanInputs,
    expected_plan_hash: Option<&str>,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
) -> Result<ExecutionConfirmation, AppError> {
    progress(crate::application::OperationProgress::stage(
        "正在读取并复核工作簿计划",
    ));
    let desired = &inputs.desired;
    let inventory_plan = &inputs.inventory;
    progress(crate::application::OperationProgress::stage(
        "正在连接游戏并读取执行状态",
    ));
    let state = execution.read_state_with_scope(inputs.layout.read_scope(), progress)?;
    progress(crate::application::OperationProgress::stage(
        "正在校验写回结构、执行目标与计划",
    ));
    validate_writeback(
        results,
        workbook,
        &inputs.source_package_sha256,
        &inputs.layout,
        &state,
    )?;
    let target_identity = execution.target_identity(&state)?;
    let check_report = compile_plan_with_inventory(&state, desired, inventory_plan)
        .map_err(map_plan_check_error)?;
    if let Some(expected) = expected_plan_hash
        && expected != check_report.plan().content_sha256()
    {
        return Err(plan_confirmation_changed_error(
            expected,
            check_report.plan().content_sha256(),
        ));
    }
    execution.bind_current_session()?;
    Ok(ExecutionConfirmation {
        target_identity,
        check_report,
        source_package_sha256: inputs.source_package_sha256.clone(),
    })
}

/// 用同一次执行连接复核并执行，写回完成前不关闭会话。
fn execute_confirmed_workbook_plan(
    workbook_port: &dyn WorkbookPort,
    results: &dyn ExecutionResultsPort,
    execution: &mut dyn ExecutionPort,
    workbook: &WorkbookRef,
    confirmation: &ExecutionConfirmation,
    cancellation: &dyn ExecutionCancellation,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
) -> Result<ExecutionOutcome, AppError> {
    progress(crate::application::OperationProgress::stage(
        "正在读取并复核工作簿计划",
    ));
    let inputs = workbook_port.load_plan_inputs(workbook)?;
    if inputs.source_package_sha256 != confirmation.source_package_sha256 {
        return Err(source_identity_changed_error(
            &confirmation.source_package_sha256,
            &inputs.source_package_sha256,
        ));
    }
    let desired = &inputs.desired;
    let inventory_plan = &inputs.inventory;
    progress(crate::application::OperationProgress::stage(
        "正在连接游戏并读取执行状态",
    ));
    let state = execution.read_state_with_scope(inputs.layout.read_scope(), progress)?;
    progress(crate::application::OperationProgress::stage(
        "正在校验写回结构、执行目标与计划",
    ));
    validate_writeback(
        results,
        workbook,
        &inputs.source_package_sha256,
        &inputs.layout,
        &state,
    )?;
    let target_identity = execution.target_identity(&state)?;
    if target_identity != *confirmation.target_identity() {
        return Err(target_confirmation_changed_error(
            confirmation.target_identity(),
            &target_identity,
        ));
    }
    let report = compile_plan_with_inventory(&state, desired, inventory_plan)
        .map_err(map_plan_check_error)?;
    let plan = report.plan();
    if plan.content_sha256() != confirmation.plan_hash() {
        return Err(plan_confirmation_changed_error(
            confirmation.plan_hash(),
            plan.content_sha256(),
        ));
    }
    execute_plan_from_state_with_outcome(
        execution,
        plan,
        state,
        confirmation.target_identity(),
        cancellation,
        progress,
    )
}

impl WorkbookExecuteService {
    /// 读取工作簿修改列；没有配装、强化或拆解要求时不连接游戏、不备份也不写回执行结果。
    pub fn execute_workbook(
        &mut self,
        workbook: &WorkbookRef,
    ) -> Result<WorkbookExecuteOutcome, AppError> {
        self.execute_workbook_with_cancellation(workbook, &NoExecutionCancellation)
    }

    /// 读取修改列后执行工作簿，并在写步骤边界观察调用方的协作式取消信号。
    pub fn execute_workbook_with_cancellation(
        &mut self,
        workbook: &WorkbookRef,
        cancellation: &dyn ExecutionCancellation,
    ) -> Result<WorkbookExecuteOutcome, AppError> {
        self.execute_workbook_with_progress(workbook, cancellation, &mut |_| {})
    }

    /// 观察执行与持久化阶段，进度通知不参与命令发送和结果判定。
    pub(crate) fn execute_workbook_with_progress(
        &mut self,
        workbook: &WorkbookRef,
        cancellation: &dyn ExecutionCancellation,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<WorkbookExecuteOutcome, AppError> {
        self.execute_workbook_checked(workbook, None, cancellation, progress)
    }

    /// 从外部检查结果执行时，必须在备份和发送操作前核对计划摘要。
    pub(crate) fn execute_workbook_checked(
        &mut self,
        workbook: &WorkbookRef,
        expected_plan_hash: Option<&str>,
        cancellation: &dyn ExecutionCancellation,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<WorkbookExecuteOutcome, AppError> {
        progress(crate::application::OperationProgress::stage(
            "正在读取工作簿计划",
        ));
        let inputs = self.workbook.load_plan_inputs(workbook)?;
        if !workbook_plan_has_modifications(&inputs.desired, &inputs.inventory) {
            if let Some(expected) = expected_plan_hash {
                let check = crate::application::compile_workbook_without_modifications(
                    &inputs.desired,
                    &inputs.inventory,
                )
                .map_err(map_plan_check_error)?;
                if expected != check.plan().content_sha256() {
                    return Err(plan_confirmation_changed_error(
                        expected,
                        check.plan().content_sha256(),
                    ));
                }
            }
            progress(crate::application::OperationProgress::stage(
                "修改列没有需要执行的操作",
            ));
            return Ok(WorkbookExecuteOutcome::NoModifications);
        }
        let operation = (|| {
            let execution = self
                .session
                .execution_mut()
                .ok_or_else(missing_execution_port_error)?;
            progress(crate::application::OperationProgress::stage(
                "正在读取执行布局与检查执行能力",
            ));
            let confirmation = prepare_workbook_execution(
                self.execution_results.as_ref(),
                execution,
                workbook,
                &inputs,
                expected_plan_hash,
                progress,
            )?;
            progress(crate::application::OperationProgress::stage(
                "正在备份工作簿",
            ));
            let backup = self.workbook_backup.backup_workbook(workbook)?;
            if backup.source_package_sha256() != inputs.source_package_sha256 {
                return Err(source_identity_changed_error(
                    &inputs.source_package_sha256,
                    backup.source_package_sha256(),
                ));
            }
            let outcome = execute_confirmed_workbook_plan(
                self.workbook.as_ref(),
                self.execution_results.as_ref(),
                execution,
                workbook,
                &confirmation,
                cancellation,
                progress,
            )?;
            let execution = outcome.report;
            progress(crate::application::OperationProgress::stage(
                "正在保存执行记录",
            ));
            let history = self
                .execution_history
                .save_execution_history(workbook, &execution);
            let saved_at = history
                .as_ref()
                .ok()
                .map(ExecutionHistoryReport::recorded_at_unix_millis);
            let writeback = if let Some(saved_at) = saved_at {
                progress(crate::application::OperationProgress::stage(
                    "正在整理执行结果与最终装备状态",
                ));
                let rows = match project_execution_report_rows(&execution, saved_at) {
                    Ok(rows) => rows,
                    Err(source) => {
                        return Ok(ExecutionPersistence {
                            execution,
                            backup,
                            history: StageResult::from_result(history),
                            writeback: StageResult::Failed(projection_write_error(
                                "执行报告未能建立有效的工作簿结果投影",
                                source,
                            )),
                        });
                    }
                };
                let projection = match outcome.final_state.as_ref().map(|state| {
                    crate::application::workbook::projection_mapper::project_game_state_for_layout(
                        state,
                        &inputs.layout,
                    )
                }).transpose() {
                    Ok(projection) => projection,
                    Err(source) => {
                        return Ok(ExecutionPersistence {
                            execution,
                            backup,
                            history: StageResult::from_result(history),
                            writeback: StageResult::Failed(projection_write_error(
                                "独立终态未能建立有效的工作簿投影",
                                source,
                            )),
                        });
                    }
                };
                progress(crate::application::OperationProgress::stage(
                    "正在写回装备总表、配装计划与下拉选项",
                ));
                StageResult::from_result(self.execution_results.write_execution_results(
                    workbook,
                    backup.source_package_sha256(),
                    &inputs.layout,
                    &rows,
                    projection,
                    saved_at,
                ))
            } else {
                StageResult::NotAttempted
            };
            Ok(ExecutionPersistence {
                execution,
                backup,
                history: StageResult::from_result(history),
                writeback,
            })
        })();
        progress(crate::application::OperationProgress::stage(
            "正在结束本次连接",
        ));
        match self
            .session
            .finish_operation(GameOperationAccess::Write, operation)
        {
            super::GameOperationFinish::Ready(persisted) => Ok(execute_outcome(
                persisted,
                SessionCleanup::from_shutdown(Ok(())),
            )),
            super::GameOperationFinish::ValueWithCleanup { value, cleanup } => Ok(execute_outcome(
                value,
                SessionCleanup::from_shutdown(Err(cleanup)),
            )),
            super::GameOperationFinish::Failed(error) => Err(error),
        }
    }
}

/// 复用写回适配器的结构绑定，在确认和实际执行前拒绝不可写回的工作簿。
fn source_identity_changed_error(expected: &str, actual: &str) -> AppError {
    AppError::from_source(
        "execution.workbook.preflight",
        AppErrorCode::WorkbookInvalid,
        "工作簿在确认后发生变化，尚未发送游戏写命令",
        std::io::Error::other("workbook source digest changed before execution"),
    )
    .with_context("source_changed", "true")
    .with_context("expected", expected)
    .with_context("actual", actual)
}

pub(super) fn validate_writeback(
    results: &dyn crate::application::ExecutionResultsPort,
    workbook: &WorkbookRef,
    expected_source_package_sha256: &str,
    layout: &crate::application::WorkbookLayout,
    state: &crate::domain::GameState,
) -> Result<(), AppError> {
    let projection =
        crate::application::workbook::projection_mapper::project_game_state_for_layout(
            state, layout,
        )
        .map_err(|source| {
            AppError::from_source(
                "execution.workbook.preflight",
                AppErrorCode::WorkbookInvalid,
                "当前状态未能建立写回预检投影",
                source,
            )
        })?;
    results.validate_writeback(workbook, expected_source_package_sha256, layout, projection)
}

//! 承载配装计划检查、检查结果写回和历史保存用例。

use super::{GameOperationAccess, WorkbookCheckService, execution};
use crate::application::{
    AppError, CheckHistoryReport, CheckReport, OperationProgress, WorkbookRef,
    compile_plan_with_inventory, compile_workbook_without_modifications, map_plan_check_error,
    workbook_plan_has_modifications,
};

impl WorkbookCheckService {
    /// 读取工作簿修改列；没有配装、强化或拆解要求时不连接游戏。
    pub fn check_workbook_plan(&mut self, workbook: &WorkbookRef) -> Result<CheckReport, AppError> {
        self.check_workbook_plan_with_progress(workbook, &mut |_| {})
    }

    pub(crate) fn check_workbook_plan_with_progress(
        &mut self,
        workbook: &WorkbookRef,
        progress: &mut dyn FnMut(OperationProgress),
    ) -> Result<CheckReport, AppError> {
        progress(OperationProgress::stage("正在读取工作簿计划"));
        let inputs = self.workbook.load_plan_inputs(workbook)?;
        match self.check_loaded_workbook_plan(workbook, &inputs, progress)? {
            CheckAttempt::Checked(report) => Ok(report),
            CheckAttempt::ReadWithoutCheck(cleanup) => Err(read_without_check_error(cleanup)),
        }
    }

    fn check_loaded_workbook_plan(
        &mut self,
        workbook: &WorkbookRef,
        inputs: &crate::application::WorkbookPlanInputs,
        progress: &mut dyn FnMut(OperationProgress),
    ) -> Result<CheckAttempt, AppError> {
        let desired = &inputs.desired;
        let inventory_plan = &inputs.inventory;
        if !workbook_plan_has_modifications(desired, inventory_plan) {
            progress(OperationProgress::stage("修改列没有需要检查的操作"));
            return compile_workbook_without_modifications(desired, inventory_plan)
                .map(CheckAttempt::Checked)
                .map_err(map_plan_check_error);
        }
        let state = match self
            .read_game_state_for_plan_with_progress(inputs.layout.read_scope(), progress)?
        {
            PlanRead::Ready(state) => state,
            PlanRead::ReadWithoutCheck(cleanup) => {
                return Ok(CheckAttempt::ReadWithoutCheck(cleanup));
            }
        };
        progress(OperationProgress::stage("正在校验工作簿写回结构"));
        execution::validate_writeback(
            self.execution_results.as_ref(),
            workbook,
            &inputs.source_package_sha256,
            &inputs.layout,
            &state,
        )?;
        progress(OperationProgress::stage("正在检查装备来源、数量和合成资源"));
        compile_plan_with_inventory(&state, desired, inventory_plan)
            .map(CheckAttempt::Checked)
            .map_err(map_plan_check_error)
    }

    /// 写回最近一次检查结论，并在检查成功后发布不可覆盖的完整历史记录。
    pub fn check_and_save_workbook_plan(
        &mut self,
        workbook: &WorkbookRef,
    ) -> Result<CheckAndSaveOutcome, AppError> {
        self.check_and_save_workbook_plan_with_progress(workbook, &mut |_| {})
    }

    /// 上报检查阶段和游戏读取的实际完成量，历史只在检查通过后保存。
    pub(crate) fn check_and_save_workbook_plan_with_progress(
        &mut self,
        workbook: &WorkbookRef,
        progress: &mut dyn FnMut(OperationProgress),
    ) -> Result<CheckAndSaveOutcome, AppError> {
        progress(OperationProgress::stage("正在读取工作簿计划"));
        let inputs = self.workbook.load_plan_inputs(workbook)?;
        let source_hash = inputs.source_package_sha256.clone();
        match self.check_loaded_workbook_plan(workbook, &inputs, progress) {
            Ok(CheckAttempt::ReadWithoutCheck(cleanup)) => {
                Ok(CheckAndSaveOutcome::ReadWithoutCheck { cleanup })
            }
            Err(error) => {
                progress(OperationProgress::stage("正在写入检查结果工作表"));
                let writeback = self.execution_results.write_check_results(
                    workbook,
                    &source_hash,
                    &inputs.layout,
                    Err(&error),
                    None,
                );
                Ok(CheckAndSaveOutcome::CheckFailed { error, writeback })
            }
            Ok(CheckAttempt::Checked(report)) => {
                progress(OperationProgress::stage("正在保存计划检查记录"));
                let history = self.check_history.save_check_history(workbook, &report);
                progress(OperationProgress::stage("正在写入检查结果工作表"));
                let checked_at = history
                    .as_ref()
                    .ok()
                    .map(CheckHistoryReport::checked_at_unix_millis);
                let writeback = self.execution_results.write_check_results(
                    workbook,
                    &source_hash,
                    &inputs.layout,
                    Ok(&report),
                    checked_at,
                );
                Ok(match (history, writeback) {
                    (Ok(history), Ok(())) => CheckAndSaveOutcome::Saved(history),
                    (history, writeback) => CheckAndSaveOutcome::SaveIncomplete {
                        report,
                        history,
                        writeback,
                    },
                })
            }
        }
    }

    /// 用调用方已经持有的布局读取范围建立一次性游戏快照。
    fn read_game_state_for_plan_with_progress(
        &mut self,
        read_scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(OperationProgress),
    ) -> Result<PlanRead, AppError> {
        let operation: Result<crate::application::GameObservation, AppError> =
            match self.session.game_mut() {
                Some(game) => {
                    progress(OperationProgress::stage("正在连接游戏并按模板读取状态"));
                    game.read_state_with_scope(read_scope, progress)
                }
                None => Err(super::missing_game_port(
                    "plan.check",
                    "配装计划检查需要已认证的游戏运行态连接",
                )),
            };
        progress(OperationProgress::stage("正在结束本次连接"));
        match self
            .session
            .finish_operation(GameOperationAccess::ReadOnly, operation)
        {
            super::GameOperationFinish::Ready(state) => Ok(PlanRead::Ready(state)),
            super::GameOperationFinish::ValueWithCleanup { cleanup, .. } => Ok(
                PlanRead::ReadWithoutCheck(crate::application::SessionCleanup::from_error(cleanup)),
            ),
            super::GameOperationFinish::Failed(error) => Err(error),
        }
    }
}

/// 游戏状态已经读到，但清理未完成，因此检查没有开始。不保留游戏状态本身。
enum PlanRead {
    Ready(crate::application::GameObservation),
    ReadWithoutCheck(crate::application::SessionCleanup),
}

enum CheckAttempt {
    Checked(CheckReport),
    ReadWithoutCheck(crate::application::SessionCleanup),
}

/// 检查、历史保存和结果写回的一次完整事实。
/// 已通过的检查报告留在变体里，历史或写回失败时调用方仍能拿到这份报告。
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CheckAndSaveOutcome {
    /// 检查通过，历史和结果写回都已完成。
    Saved(CheckHistoryReport),
    /// 检查未通过。历史未尝试，写回单独记录成败。
    CheckFailed {
        error: AppError,
        writeback: Result<(), AppError>,
    },
    /// 检查已通过，但历史或结果写回没有全部完成。
    SaveIncomplete {
        report: CheckReport,
        history: Result<CheckHistoryReport, AppError>,
        writeback: Result<(), AppError>,
    },
    /// 游戏状态已读取，清理未完成，检查没有开始。
    ReadWithoutCheck {
        cleanup: crate::application::SessionCleanup,
    },
}

impl CheckAndSaveOutcome {
    /// 日志终态按检查业务结果决定，不把检查失败写成未完成。
    pub fn log_terminal(&self) -> crate::application::OperationTerminal {
        match self {
            Self::Saved(_) => crate::application::OperationTerminal::Succeeded,
            Self::CheckFailed { .. } => crate::application::OperationTerminal::Failed,
            Self::SaveIncomplete { .. } | Self::ReadWithoutCheck { .. } => {
                crate::application::OperationTerminal::Incomplete
            }
        }
    }
}

fn read_without_check_error(cleanup: crate::application::SessionCleanup) -> AppError {
    match cleanup {
        crate::application::SessionCleanup::Recovered(error)
        | crate::application::SessionCleanup::Failed(error) => {
            error.with_context("check_stage", "read_without_check")
        }
        crate::application::SessionCleanup::Completed => AppError::from_source(
            "plan.check",
            crate::application::AppErrorCode::RuntimeBootstrapFailed,
            "已读取游戏状态，但会话清理未完成，本次没有开始检查",
            std::io::Error::other("cleanup completed without a read-without-check error"),
        ),
    }
}

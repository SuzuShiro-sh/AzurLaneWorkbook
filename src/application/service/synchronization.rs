//! 承载游戏状态读取与数据工作簿生成用例。

use super::{GameOperationAccess, WorkbookSyncService};
use crate::application::workbook::generation::generate_workbook_with_progress;
use crate::application::{AppError, OperationProgress, SessionCleanup, WorkbookGenerationOutcome};

impl WorkbookSyncService {
    /// 在同一次服务调用中读取布局和完整游戏状态，再生成一份数据工作簿。
    pub fn generate_workbook(
        &mut self,
        requested_name: Option<&str>,
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkbookGenerationOutcome, AppError> {
        self.generate_workbook_with_progress(requested_name, &mut |_| {}, is_cancelled)
    }

    /// 各阶段完成量只用于观察，关闭会话仍由原有收尾边界保证执行。
    pub(crate) fn generate_workbook_with_progress(
        &mut self,
        requested_name: Option<&str>,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkbookGenerationOutcome, AppError> {
        let operation = match self.session.game_mut() {
            Some(game) => {
                game.prepare_synchronization();
                generate_workbook_with_progress(
                    self.workbook.as_ref(),
                    game,
                    self.generation.as_ref(),
                    requested_name,
                    progress,
                    is_cancelled,
                )
            }
            None => match self.workbook.load_layout() {
                Err(error) => Err(error),
                Ok(_) => Err(super::missing_game_port(
                    "workbook.generate",
                    "工作簿生成需要已认证的游戏运行态连接",
                )),
            },
        };
        progress(OperationProgress::stage("正在结束本次连接"));
        match self
            .session
            .finish_operation(GameOperationAccess::ReadOnly, operation)
        {
            super::GameOperationFinish::Ready(report) => {
                Ok(WorkbookGenerationOutcome::Completed(report))
            }
            super::GameOperationFinish::ValueWithCleanup { value, cleanup } => {
                Ok(WorkbookGenerationOutcome::CleanupIncomplete {
                    cleanup: SessionCleanup::from_error(
                        cleanup
                            .with_context("output_path", value.output_path())
                            .with_context("output_package_sha256", value.output_package_sha256()),
                    ),
                    report: value,
                })
            }
            super::GameOperationFinish::Failed(error) => Err(error),
        }
    }
}

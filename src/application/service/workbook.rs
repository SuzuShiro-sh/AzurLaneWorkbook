//! 承载执行前的工作簿备份。

use super::WorkbookExecuteService;
use crate::application::{AppError, WorkbookBackupReport, WorkbookRef};

impl WorkbookExecuteService {
    /// 在发送任何游戏写命令前保存并重读核验当前工作簿原始字节。
    pub fn backup_workbook(
        &self,
        workbook: &WorkbookRef,
    ) -> Result<WorkbookBackupReport, AppError> {
        self.workbook_backup.backup_workbook(workbook)
    }
}

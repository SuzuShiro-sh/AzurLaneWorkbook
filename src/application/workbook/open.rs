//! 定义工作簿交给系统默认程序后的稳定应用报告。

use serde::Serialize;

/// 打开工作簿成功后供 CLI 和 GUI 展示的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookOpenReport {
    message: &'static str,
    workbook_path: String,
    launcher: &'static str,
}

impl WorkbookOpenReport {
    /// 从已经通过受控路径校验的工作簿和实际打开器建立报告。
    pub(crate) fn new(relative_path: impl Into<String>, launcher: &'static str) -> Self {
        Self {
            message: "工作簿已交给系统默认程序打开",
            workbook_path: relative_path.into(),
            launcher,
        }
    }

    /// 返回面向用户的成功说明。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回工具目录内的规范相对路径。
    pub fn workbook_path(&self) -> &str {
        &self.workbook_path
    }

    /// 返回实际使用的系统打开器名称。
    pub const fn launcher(&self) -> &'static str {
        self.launcher
    }
}

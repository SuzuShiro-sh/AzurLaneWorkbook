//! 布局检查、升级和预览的窄服务。

use serde::Serialize;

use crate::application::{
    AppError, LayoutPreviewPort, LayoutPreviewReport, LayoutUpgradePort, LayoutUpgradeReport,
    WorkbookLayout, WorkbookPort,
};

/// 一次严格布局检查成功后可供界面和命令行展示的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayoutCheckReport {
    message: &'static str,
    schema_version: u32,
    template_name: String,
    purpose: String,
    sheet_count: usize,
    field_count: usize,
    enum_option_count: usize,
    style_count: usize,
    content_sha256: String,
}

impl LayoutCheckReport {
    /// 从已经通过完整契约校验的不可变布局建立摘要。
    fn from_layout(layout: &WorkbookLayout) -> Self {
        Self {
            message: "工作簿布局检查通过",
            schema_version: layout.schema_version(),
            template_name: layout.template_name().to_owned(),
            purpose: layout.purpose().to_owned(),
            sheet_count: layout.sheets().len(),
            field_count: layout.fields().len(),
            enum_option_count: layout.enum_options().len(),
            style_count: layout.styles().len(),
            content_sha256: layout.content_sha256().to_owned(),
        }
    }

    /// 返回面向用户的检查结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回布局 schema 版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回布局模板名称。
    pub fn template_name(&self) -> &str {
        &self.template_name
    }

    /// 返回布局用途说明。
    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    /// 返回通过检查的工作表数量。
    pub const fn sheet_count(&self) -> usize {
        self.sheet_count
    }

    /// 返回通过检查的字段数量。
    pub const fn field_count(&self) -> usize {
        self.field_count
    }

    /// 返回通过检查的枚举选项数量。
    pub const fn enum_option_count(&self) -> usize {
        self.enum_option_count
    }

    /// 返回通过检查的样式数量。
    pub const fn style_count(&self) -> usize {
        self.style_count
    }

    /// 返回规范布局内容的 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 只负责根布局的检查、升级和预览。
pub struct LayoutService {
    workbook: Box<dyn WorkbookPort>,
    layout_upgrade: Box<dyn LayoutUpgradePort>,
    layout_preview: Box<dyn LayoutPreviewPort>,
}

impl LayoutService {
    /// 一次接收布局读取、升级和预览三项端口。
    pub(crate) fn with_ports(
        workbook: Box<dyn WorkbookPort>,
        layout_upgrade: Box<dyn LayoutUpgradePort>,
        layout_preview: Box<dyn LayoutPreviewPort>,
    ) -> Self {
        Self {
            workbook,
            layout_upgrade,
            layout_preview,
        }
    }

    /// 严格检查当前根布局，不读取游戏状态也不写入任何文件。
    pub fn check_layout(&self) -> Result<LayoutCheckReport, AppError> {
        let layout = self.workbook.load_layout()?;
        Ok(LayoutCheckReport::from_layout(&layout))
    }

    /// 将根布局升级到独立新文件，不读取游戏状态也不覆盖任一既有文件。
    pub fn upgrade_layout(&self) -> Result<LayoutUpgradeReport, AppError> {
        self.layout_upgrade.upgrade_layout()
    }

    /// 严格读取根布局后刷新固定预览路径，不读取游戏或设备状态。
    pub fn preview_layout(&self) -> Result<LayoutPreviewReport, AppError> {
        let layout = self.workbook.load_layout()?;
        self.layout_preview.preview_layout(&layout)
    }
}

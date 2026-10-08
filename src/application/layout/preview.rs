//! 定义布局预览工作簿刷新成功后可供界面、命令行和日志消费的稳定摘要。

use serde::Serialize;

/// 一次布局预览经过写出、重载和原子替换后形成的摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayoutPreviewReport {
    message: &'static str,
    output_path: String,
    layout_schema_version: u32,
    workbook_schema_version: u32,
    generated_sheets: usize,
    hidden_sheets: usize,
    omitted_sheets: usize,
    generated_fields: usize,
    hidden_fields: usize,
    omitted_fields: usize,
    example_rows: usize,
    layout_content_sha256: String,
    output_package_sha256: String,
}

impl LayoutPreviewReport {
    /// 汇总经过严格验证的固定输出路径、布局范围和包摘要。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        output_path: String,
        layout_schema_version: u32,
        workbook_schema_version: u32,
        generated_sheets: usize,
        hidden_sheets: usize,
        omitted_sheets: usize,
        generated_fields: usize,
        hidden_fields: usize,
        omitted_fields: usize,
        example_rows: usize,
        layout_content_sha256: String,
        output_package_sha256: String,
    ) -> Self {
        Self {
            message: "工作簿布局预览已刷新",
            output_path,
            layout_schema_version,
            workbook_schema_version,
            generated_sheets,
            hidden_sheets,
            omitted_sheets,
            generated_fields,
            hidden_fields,
            omitted_fields,
            example_rows,
            layout_content_sha256,
            output_package_sha256,
        }
    }

    /// 返回面向用户的刷新结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回相对工具根目录的预览路径。
    pub fn output_path(&self) -> &str {
        &self.output_path
    }

    /// 返回生成预览时使用的布局 schema 版本。
    pub const fn layout_schema_version(&self) -> u32 {
        self.layout_schema_version
    }

    /// 返回预览工作簿投影 schema 版本。
    pub const fn workbook_schema_version(&self) -> u32 {
        self.workbook_schema_version
    }

    /// 返回实际写入的工作表数量。
    pub const fn generated_sheets(&self) -> usize {
        self.generated_sheets
    }

    /// 返回实际写入且隐藏的工作表数量。
    pub const fn hidden_sheets(&self) -> usize {
        self.hidden_sheets
    }

    /// 返回按布局明确省略的工作表数量。
    pub const fn omitted_sheets(&self) -> usize {
        self.omitted_sheets
    }

    /// 返回实际写入的字段数量。
    pub const fn generated_fields(&self) -> usize {
        self.generated_fields
    }

    /// 返回实际写入且隐藏的字段数量。
    pub const fn hidden_fields(&self) -> usize {
        self.hidden_fields
    }

    /// 返回按布局明确省略的字段数量。
    pub const fn omitted_fields(&self) -> usize {
        self.omitted_fields
    }

    /// 返回全部工作表写入的示例数据行数量。
    pub const fn example_rows(&self) -> usize {
        self.example_rows
    }

    /// 返回生成时采用的规范布局内容摘要。
    pub fn layout_content_sha256(&self) -> &str {
        &self.layout_content_sha256
    }

    /// 返回最终预览 XLSX 包字节摘要。
    pub fn output_package_sha256(&self) -> &str {
        &self.output_package_sha256
    }
}

//! 定义布局升级结果中可供 CLI、GUI 和日志稳定消费的摘要。

use serde::Serialize;

/// 按稳定项类型统计布局升级保留或新增的记录数量。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct LayoutUpgradeItemCounts {
    sheets: usize,
    fields: usize,
    enum_options: usize,
    styles: usize,
}

impl LayoutUpgradeItemCounts {
    /// 建立四类布局记录的稳定计数。
    pub(crate) const fn new(
        sheets: usize,
        fields: usize,
        enum_options: usize,
        styles: usize,
    ) -> Self {
        Self {
            sheets,
            fields,
            enum_options,
            styles,
        }
    }

    /// 返回保留或新增的工作表设置数量。
    pub const fn sheets(&self) -> usize {
        self.sheets
    }

    /// 返回保留或新增的字段设置数量。
    pub const fn fields(&self) -> usize {
        self.fields
    }

    /// 返回保留或新增的枚举设置数量。
    pub const fn enum_options(&self) -> usize {
        self.enum_options
    }

    /// 返回保留或新增的样式设置数量。
    pub const fn styles(&self) -> usize {
        self.styles
    }
}

/// 一次非覆盖布局升级成功后形成的完整、可序列化报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayoutUpgradeReport {
    message: &'static str,
    source_path: String,
    output_path: String,
    source_schema_version: u32,
    target_schema_version: u32,
    preserved: LayoutUpgradeItemCounts,
    added: LayoutUpgradeItemCounts,
    added_items: Vec<String>,
    source_package_sha256: String,
    output_package_sha256: String,
    output_content_sha256: String,
}

impl LayoutUpgradeReport {
    /// 汇总经过严格重载确认的升级路径、变化和内容摘要。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source_path: String,
        output_path: String,
        source_schema_version: u32,
        target_schema_version: u32,
        preserved: LayoutUpgradeItemCounts,
        added: LayoutUpgradeItemCounts,
        added_items: Vec<String>,
        source_package_sha256: String,
        output_package_sha256: String,
        output_content_sha256: String,
    ) -> Self {
        Self {
            message: "工作簿布局升级完成",
            source_path,
            output_path,
            source_schema_version,
            target_schema_version,
            preserved,
            added,
            added_items,
            source_package_sha256,
            output_package_sha256,
            output_content_sha256,
        }
    }

    /// 返回面向用户的升级结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回相对工具根目录的源布局路径。
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// 返回相对工具根目录的升级产物路径。
    pub fn output_path(&self) -> &str {
        &self.output_path
    }

    /// 返回源布局声明的 schema 版本。
    pub const fn source_schema_version(&self) -> u32 {
        self.source_schema_version
    }

    /// 返回升级产物使用的 schema 版本。
    pub const fn target_schema_version(&self) -> u32 {
        self.target_schema_version
    }

    /// 返回按稳定键完整保留的四类记录数量。
    pub const fn preserved(&self) -> LayoutUpgradeItemCounts {
        self.preserved
    }

    /// 返回由当前注册表补入的四类记录数量。
    pub const fn added(&self) -> LayoutUpgradeItemCounts {
        self.added
    }

    /// 返回带类型前缀的新增稳定项。
    pub fn added_items(&self) -> &[String] {
        &self.added_items
    }

    /// 返回源 XLSX 包字节的 SHA-256。
    pub fn source_package_sha256(&self) -> &str {
        &self.source_package_sha256
    }

    /// 返回升级 XLSX 包字节的 SHA-256。
    pub fn output_package_sha256(&self) -> &str {
        &self.output_package_sha256
    }

    /// 返回严格重载后规范布局模型的 SHA-256。
    pub fn output_content_sha256(&self) -> &str {
        &self.output_content_sha256
    }
}

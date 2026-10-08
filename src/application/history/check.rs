//! 定义配装检查历史发布后的稳定应用报告。

use serde::Serialize;

/// 检查历史 JSON 外壳的稳定版本。
pub const CHECK_HISTORY_SCHEMA_VERSION: u32 = 1;

/// 一次检查历史文件发布后的稳定证据摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckHistoryReport {
    message: &'static str,
    schema_version: u32,
    plan_schema_version: u32,
    workbook_name: String,
    checked_at_unix_millis: i64,
    relative_path: String,
    size_bytes: u64,
    file_sha256: String,
    plan_content_sha256: String,
}

impl CheckHistoryReport {
    /// 从已经重读核验的历史文件建立报告。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        plan_schema_version: u32,
        workbook_name: String,
        checked_at_unix_millis: i64,
        relative_path: String,
        size_bytes: u64,
        file_sha256: String,
        plan_content_sha256: String,
    ) -> Self {
        Self {
            message: "配装计划检查记录已保存",
            schema_version: CHECK_HISTORY_SCHEMA_VERSION,
            plan_schema_version,
            workbook_name,
            checked_at_unix_millis,
            relative_path,
            size_bytes,
            file_sha256,
            plan_content_sha256,
        }
    }

    /// 返回保存结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回检查历史 JSON 外壳版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回嵌套只读计划的契约版本。
    pub const fn plan_schema_version(&self) -> u32 {
        self.plan_schema_version
    }

    /// 返回被检查的工作簿文件名。
    pub fn workbook_name(&self) -> &str {
        &self.workbook_name
    }

    /// 返回检查开始发布时记录的 Unix 毫秒时间戳。
    pub const fn checked_at_unix_millis(&self) -> i64 {
        self.checked_at_unix_millis
    }

    /// 返回工具根目录内的历史文件相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回已发布 JSON 的 UTF-8 字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回已发布 JSON 文件字节的 SHA-256。
    pub fn file_sha256(&self) -> &str {
        &self.file_sha256
    }

    /// 返回检查计划内容摘要。
    pub fn plan_content_sha256(&self) -> &str {
        &self.plan_content_sha256
    }
}

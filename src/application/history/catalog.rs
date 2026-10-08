//! 定义历史目录读取后供 CLI 和 GUI 共用的稳定摘要。

use serde::Serialize;

use super::super::ExecutionReportStatus;

/// 历史目录报告的稳定 JSON 外壳版本。
pub const HISTORY_CATALOG_SCHEMA_VERSION: u32 = 2;

/// 工具根目录内不可覆盖历史记录的固定位置。
pub const HISTORY_DIRECTORY: &str = "data/history";

/// 历史记录的业务类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRecordKind {
    /// 只读配装检查记录。
    Check,
    /// 已完成或停止的配装执行记录。
    Execution,
}

/// 执行历史外壳中的稳定状态，不依赖完整执行报告反序列化。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryExecutionStatus {
    /// 全部步骤和最终状态均已确认。
    Success,
    /// 至少一个步骤或最终状态明确失败。
    Failed,
    /// 可能已经写入，但最终状态不能确认。
    Unknown,
    /// 执行在完成前被取消。
    Cancelled,
}

impl From<ExecutionReportStatus> for HistoryExecutionStatus {
    fn from(status: ExecutionReportStatus) -> Self {
        match status {
            ExecutionReportStatus::Success => Self::Success,
            ExecutionReportStatus::Failed => Self::Failed,
            ExecutionReportStatus::Unknown => Self::Unknown,
            ExecutionReportStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// 一条已经重新读取、校验并计算文件摘要的历史目录项。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HistoryCatalogEntry {
    kind: HistoryRecordKind,
    schema_version: u32,
    workbook_name: String,
    timestamp_unix_millis: i64,
    relative_path: String,
    size_bytes: u64,
    file_sha256: String,
    plan_content_sha256: String,
    execution_status: Option<HistoryExecutionStatus>,
    may_have_writes: Option<bool>,
    target_fingerprint_sha256: Option<String>,
}

impl HistoryCatalogEntry {
    /// 从检查历史外壳建立目录项。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_check(
        schema_version: u32,
        workbook_name: String,
        timestamp_unix_millis: i64,
        relative_path: String,
        size_bytes: u64,
        file_sha256: String,
        plan_content_sha256: String,
    ) -> Self {
        Self {
            kind: HistoryRecordKind::Check,
            schema_version,
            workbook_name,
            timestamp_unix_millis,
            relative_path,
            size_bytes,
            file_sha256,
            plan_content_sha256,
            execution_status: None,
            may_have_writes: None,
            target_fingerprint_sha256: None,
        }
    }

    /// 从执行历史外壳建立目录项。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_execution(
        schema_version: u32,
        workbook_name: String,
        timestamp_unix_millis: i64,
        relative_path: String,
        size_bytes: u64,
        file_sha256: String,
        plan_content_sha256: String,
        execution_status: HistoryExecutionStatus,
        may_have_writes: bool,
        target_fingerprint_sha256: String,
    ) -> Self {
        Self {
            kind: HistoryRecordKind::Execution,
            schema_version,
            workbook_name,
            timestamp_unix_millis,
            relative_path,
            size_bytes,
            file_sha256,
            plan_content_sha256,
            execution_status: Some(execution_status),
            may_have_writes: Some(may_have_writes),
            target_fingerprint_sha256: Some(target_fingerprint_sha256),
        }
    }

    /// 返回历史记录类型。
    pub const fn kind(&self) -> HistoryRecordKind {
        self.kind
    }

    /// 返回历史 JSON 外壳版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回对应工作簿文件名。
    pub fn workbook_name(&self) -> &str {
        &self.workbook_name
    }

    /// 返回检查或执行记录的 Unix 毫秒时间戳。
    pub const fn timestamp_unix_millis(&self) -> i64 {
        self.timestamp_unix_millis
    }

    /// 返回工具根目录内的历史文件相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回重新读取时确认的文件字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回历史文件本身的 SHA-256。
    pub fn file_sha256(&self) -> &str {
        &self.file_sha256
    }

    /// 返回历史中记录的计划内容摘要。
    pub fn plan_content_sha256(&self) -> &str {
        &self.plan_content_sha256
    }

    /// 返回执行记录状态；检查记录没有该字段。
    pub const fn execution_status(&self) -> Option<HistoryExecutionStatus> {
        self.execution_status
    }

    /// 返回执行记录是否可能已经向游戏写入；检查记录没有该字段。
    pub const fn may_have_writes(&self) -> Option<bool> {
        self.may_have_writes
    }

    /// 返回执行目标的脱敏指纹；检查记录没有该字段。
    pub fn target_fingerprint_sha256(&self) -> Option<&str> {
        self.target_fingerprint_sha256.as_deref()
    }
}

/// 历史目录读取完成后供 CLI 和 GUI 消费的稳定报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HistoryCatalogReport {
    message: &'static str,
    schema_version: u32,
    directory: &'static str,
    entries: Vec<HistoryCatalogEntry>,
    failures: Vec<super::super::diagnostics::log_catalog::DirectoryEntryFailure>,
}

impl HistoryCatalogReport {
    /// 从已按时间降序排列的目录项建立报告。
    pub(crate) fn new(
        entries: Vec<HistoryCatalogEntry>,
        failures: Vec<super::super::diagnostics::log_catalog::DirectoryEntryFailure>,
    ) -> Self {
        Self {
            message: if failures.is_empty() {
                "历史目录读取完成"
            } else {
                "历史目录部分文件读取失败"
            },
            schema_version: HISTORY_CATALOG_SCHEMA_VERSION,
            directory: HISTORY_DIRECTORY,
            entries,
            failures,
        }
    }

    /// 返回面向用户的目录读取结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回历史目录报告版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回被读取的工具目录相对路径。
    pub const fn directory(&self) -> &'static str {
        self.directory
    }

    /// 返回按时间从新到旧排列的历史目录项。
    pub fn entries(&self) -> &[HistoryCatalogEntry] {
        &self.entries
    }

    /// 返回未能读取的历史文件；目录本身无法访问时不会出现在这里。
    pub fn failures(&self) -> &[super::super::diagnostics::log_catalog::DirectoryEntryFailure] {
        &self.failures
    }

    /// 返回目录中的历史记录数量。
    pub const fn count(&self) -> usize {
        self.entries.len()
    }
}

//! 定义日志目录读取后供 CLI 和 GUI 共用的脱敏摘要。

use serde::Serialize;

/// 日志目录报告的稳定 JSON 外壳版本。
pub const LOG_CATALOG_SCHEMA_VERSION: u32 = 2;

/// 工具根目录内运行日志的固定位置。
pub const LOG_DIRECTORY: &str = "data/logs";

/// 已登记的日志文件类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogRecordKind {
    /// 运行态探针产生的逐阶段 JSONL 日志。
    RuntimeProbe,
    /// 随包 ADB 服务的标准输出和错误输出日志。
    AdbServer,
    /// 界面操作的阶段、结果与完整错误链日志。
    Operation,
}

/// 一条日志目录项。规范编号只留在内存中，不写入目录 JSON。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LogCatalogEntry {
    kind: LogRecordKind,
    relative_path: String,
    size_bytes: u64,
    file_sha256: String,
    #[serde(skip)]
    timestamp_unix_millis: i64,
    /// 文件名中的规范编号；没有规范编号时为空。
    #[serde(skip)]
    sequence: Option<u64>,
}

impl LogCatalogEntry {
    /// 从已通过文件名和读取边界校验的日志建立目录项。
    /// `sequence` 来自既有编号解析；无编号样本为 `None`。
    pub(crate) fn new(
        kind: LogRecordKind,
        relative_path: String,
        size_bytes: u64,
        file_sha256: String,
        timestamp_unix_millis: i64,
        sequence: Option<u64>,
    ) -> Self {
        Self {
            kind,
            relative_path,
            size_bytes,
            file_sha256,
            timestamp_unix_millis,
            sequence,
        }
    }

    /// 返回首条事件时间；旧日志无事件时间时使用文件时间，供界面与历史混排。
    pub const fn timestamp_unix_millis(&self) -> i64 {
        self.timestamp_unix_millis
    }

    /// 返回文件名中的规范编号。
    pub(crate) const fn sequence(&self) -> Option<u64> {
        self.sequence
    }

    /// 返回日志类型。
    pub const fn kind(&self) -> LogRecordKind {
        self.kind
    }

    /// 返回工具根目录内的稳定相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回读取时确认的文件字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回日志文件内容的 SHA-256，不包含日志正文。
    pub fn file_sha256(&self) -> &str {
        &self.file_sha256
    }
}

/// 目录中单个文件无法读取时保留的路径和原因。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DirectoryEntryFailure {
    relative_path: String,
    error_code: String,
    message: String,
}

impl DirectoryEntryFailure {
    /// 记录一个不导致整组目录失败的文件错误。
    pub(crate) fn new(
        relative_path: impl Into<String>,
        error_code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            relative_path: relative_path.into(),
            error_code: error_code.into(),
            message: message.into(),
        }
    }

    /// 返回工具根目录内的相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回稳定错误码。
    pub fn error_code(&self) -> &str {
        &self.error_code
    }

    /// 返回可展示的失败原因。
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 日志目录读取完成后供 CLI 和 GUI 消费的稳定报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LogCatalogReport {
    message: &'static str,
    schema_version: u32,
    directory: &'static str,
    entries: Vec<LogCatalogEntry>,
    failures: Vec<DirectoryEntryFailure>,
}

impl LogCatalogReport {
    /// 从已排序的健康目录项和逐文件失败建立报告。
    pub(crate) fn new(entries: Vec<LogCatalogEntry>, failures: Vec<DirectoryEntryFailure>) -> Self {
        Self {
            message: if failures.is_empty() {
                "日志目录读取完成"
            } else {
                "日志目录部分文件读取失败"
            },
            schema_version: LOG_CATALOG_SCHEMA_VERSION,
            directory: LOG_DIRECTORY,
            entries,
            failures,
        }
    }

    /// 返回面向用户的目录读取结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回日志目录报告的稳定版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回被读取的工具目录相对路径。
    pub const fn directory(&self) -> &'static str {
        self.directory
    }

    /// 返回按相对路径稳定排序的日志目录项。
    pub fn entries(&self) -> &[LogCatalogEntry] {
        &self.entries
    }

    /// 返回目录中的日志文件数量。
    pub const fn count(&self) -> usize {
        self.entries.len()
    }

    /// 返回未能读取的日志文件；目录本身无法访问时不会出现在这里。
    pub fn failures(&self) -> &[DirectoryEntryFailure] {
        &self.failures
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{LOG_CATALOG_SCHEMA_VERSION, LogCatalogEntry, LogCatalogReport, LogRecordKind};

    #[test]
    fn serializes_only_log_metadata() {
        let entry = LogCatalogEntry::new(
            LogRecordKind::RuntimeProbe,
            "data/logs/001-runtime.jsonl".to_owned(),
            42,
            "a".repeat(64),
            1_700_000_000_123,
            Some(1),
        );
        assert_eq!(entry.sequence(), Some(1));
        let report = LogCatalogReport::new(vec![entry], Vec::new());

        assert_eq!(
            serde_json::to_value(report).unwrap(),
            json!({
                "message": "日志目录读取完成",
                "schema_version": LOG_CATALOG_SCHEMA_VERSION,
                "directory": "data/logs",
                "entries": [{
                    "kind": "runtime_probe",
                    "relative_path": "data/logs/001-runtime.jsonl",
                    "size_bytes": 42,
                    "file_sha256": "a".repeat(64),
                }],
                "failures": [],
            })
        );
    }
}

//! 读取并严格校验工具根目录内的配装历史 JSON 外壳。

use std::io;
use std::path::Path;

use serde::Deserialize;
use serde::de::IgnoredAny;
use thiserror::Error;

use super::super::file_snapshot::{FileSnapshotError, read_bounded_file_snapshot};
use super::super::tool_root::{ToolRoot, ToolRootError};
use super::execution::EXECUTION_HISTORY_SCHEMA_VERSION;
use super::{HISTORY_DIRECTORY, is_canonical_sha256, relative_path_string, validate_workbook_name};
use crate::adapters::numbered_files::numbered_name;
use crate::application::{
    AppError, AppErrorCode, CHECK_HISTORY_SCHEMA_VERSION, DirectoryEntryFailure,
    HistoryCatalogEntry, HistoryCatalogPort, HistoryCatalogReport, HistoryExecutionStatus,
    PLAN_SCHEMA_VERSION,
};

const MAXIMUM_HISTORY_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn catalog_cancelled() -> AppError {
    AppError::from_source(
        "catalog.cancelled",
        AppErrorCode::HistoryReadFailed,
        "目录读取已取消",
        io::Error::new(io::ErrorKind::Interrupted, "目录读取已取消"),
    )
}

fn directory_failure_message(error: &AppError) -> String {
    match std::error::Error::source(error) {
        Some(source) => format!("{}：{source}", error.message()),
        None => error.message().to_owned(),
    }
}

/// 使用固定工具根目录读取历史目录，并重新验证每条文件边界。
pub(crate) struct JsonHistoryCatalogPort {
    tool_root: ToolRoot,
}

impl JsonHistoryCatalogPort {
    /// 固定受控工具根目录，读取时不接受外部历史路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl HistoryCatalogPort for JsonHistoryCatalogPort {
    fn list_history(
        &self,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<HistoryCatalogReport, AppError> {
        let files = self
            .tool_root
            .list_history_files()
            .map_err(map_directory_error)?;
        let mut entries = Vec::new();
        let mut failures = Vec::new();
        for (relative, _) in &files {
            // 运行诊断和装备证据由运行核验入口读取，不属于配装历史列表。
            if relative
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(numbered_name)
                .is_some_and(|(_, kind)| {
                    matches!(
                        kind,
                        "probe.json" | "equipment.json" | "portable.json" | "ships.json"
                    )
                })
            {
                continue;
            }
            if is_cancelled() {
                return Err(catalog_cancelled());
            }
            match self.read_entry(relative) {
                Ok(entry) => entries.push(entry),
                Err(error) => failures.push(DirectoryEntryFailure::new(
                    relative_path_string(relative),
                    error.code().as_str(),
                    directory_failure_message(&error),
                )),
            }
        }
        entries.sort_by(|left, right| {
            right
                .timestamp_unix_millis()
                .cmp(&left.timestamp_unix_millis())
                .then_with(|| history_kind_rank(left).cmp(&history_kind_rank(right)))
                .then_with(|| left.relative_path().cmp(right.relative_path()))
        });
        failures.sort_by(|left, right| left.relative_path().cmp(right.relative_path()));
        Ok(HistoryCatalogReport::new(entries, failures))
    }
}

impl JsonHistoryCatalogPort {
    pub(crate) fn read_entry(&self, relative: &Path) -> Result<HistoryCatalogEntry, AppError> {
        let (bytes, file_sha256) = self.read_file(relative)?;
        self.entry_from_bytes(relative, &bytes, file_sha256)
    }

    /// 读取同一份受控快照，校验外壳后返回选中的顶层字段。
    pub(crate) fn read_details(
        &self,
        relative: &Path,
        fields: Option<&[String]>,
    ) -> Result<serde_json::Value, AppError> {
        let (bytes, sha256) = self.read_file(relative)?;
        self.entry_from_bytes(relative, &bytes, sha256)?;
        let value: serde_json::Value = parse_header(relative, &bytes)?;
        let Some(fields) = fields else {
            return Ok(value);
        };
        let mut selected = serde_json::Map::new();
        for field in fields {
            let item = value.get(field).ok_or_else(|| {
                AppError::from_source(
                    "history.fields",
                    AppErrorCode::InputInvalid,
                    "历史字段选择不支持：仅支持存在的顶层字段，不支持嵌套字段路径",
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("历史不存在顶层字段 {field}"),
                    ),
                )
                .with_context("path", relative_path_string(relative))
                .with_context("field", field)
            })?;
            selected.insert(field.clone(), item.clone());
        }
        Ok(serde_json::Value::Object(selected))
    }

    fn entry_from_bytes(
        &self,
        relative: &Path,
        bytes: &[u8],
        file_sha256: String,
    ) -> Result<HistoryCatalogEntry, AppError> {
        let file_name = relative
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid_entry(relative, "历史文件名必须是有效 Unicode 文本"))?;
        if numbered_name(file_name).is_some_and(|(_, kind)| kind == "check.json") {
            let header: CheckHistoryHeader = parse_header(relative, bytes)?;
            validate_check_header(relative, &header)?;
            Ok(HistoryCatalogEntry::new_check(
                header.schema_version,
                header.workbook_name,
                header.checked_at_unix_millis,
                relative_path_string(relative),
                bytes.len() as u64,
                file_sha256,
                header.plan_content_sha256,
            ))
        } else if numbered_name(file_name).is_some_and(|(_, kind)| kind == "exec.json") {
            let header: ExecutionHistoryHeader = parse_header(relative, bytes)?;
            validate_execution_header(relative, &header)?;
            Ok(HistoryCatalogEntry::new_execution(
                header.schema_version,
                header.workbook_name,
                header.recorded_at_unix_millis,
                relative_path_string(relative),
                bytes.len() as u64,
                file_sha256,
                header.plan_content_sha256,
                header.report_status.into(),
                header.may_have_writes,
                header.target_fingerprint_sha256,
            ))
        } else {
            Err(invalid_entry(
                relative,
                "历史文件名必须使用编号加 check.json 或 exec.json",
            ))
        }
    }

    fn read_file(&self, relative: &Path) -> Result<(Vec<u8>, String), AppError> {
        read_bounded_file_snapshot(&self.tool_root, relative, MAXIMUM_HISTORY_FILE_BYTES)
            .map(|snapshot| snapshot.into_parts())
            .map_err(|source| map_snapshot_error(relative, source))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckHistoryHeader {
    schema_version: u32,
    plan_schema_version: u32,
    workbook_name: String,
    checked_at_unix_millis: i64,
    plan_content_sha256: String,
    #[serde(rename = "report")]
    _report: IgnoredAny,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionHistoryHeader {
    schema_version: u32,
    execution_schema_version: u32,
    plan_schema_version: u32,
    workbook_name: String,
    recorded_at_unix_millis: i64,
    target_fingerprint_sha256: String,
    plan_content_sha256: String,
    report_content_sha256: String,
    report_status: WireExecutionStatus,
    may_have_writes: bool,
    #[serde(rename = "report")]
    _report: IgnoredAny,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireExecutionStatus {
    Success,
    Failed,
    Unknown,
    Cancelled,
}

impl From<WireExecutionStatus> for HistoryExecutionStatus {
    fn from(status: WireExecutionStatus) -> Self {
        match status {
            WireExecutionStatus::Success => Self::Success,
            WireExecutionStatus::Failed => Self::Failed,
            WireExecutionStatus::Unknown => Self::Unknown,
            WireExecutionStatus::Cancelled => Self::Cancelled,
        }
    }
}

fn parse_header<T: for<'de> Deserialize<'de>>(
    relative: &Path,
    bytes: &[u8],
) -> Result<T, AppError> {
    serde_json::from_slice(bytes).map_err(|source| {
        AppError::from_source(
            "history.catalog",
            AppErrorCode::HistoryReadFailed,
            "历史 JSON 外壳无法通过严格校验",
            HistoryReadError::Json {
                path: relative_path_string(relative),
                source,
            },
        )
        .with_context("path", relative_path_string(relative))
    })
}

fn validate_check_header(relative: &Path, header: &CheckHistoryHeader) -> Result<(), AppError> {
    if header.schema_version != CHECK_HISTORY_SCHEMA_VERSION {
        return Err(unsupported_schema(
            relative,
            header.schema_version,
            CHECK_HISTORY_SCHEMA_VERSION,
        ));
    }
    validate_common_header(
        relative,
        &header.workbook_name,
        header.checked_at_unix_millis,
        header.plan_content_sha256.as_str(),
    )?;
    if header.plan_schema_version != PLAN_SCHEMA_VERSION {
        return Err(invalid_entry(
            relative,
            "检查历史的计划 schema 与当前应用不一致",
        ));
    }
    Ok(())
}

fn validate_execution_header(
    relative: &Path,
    header: &ExecutionHistoryHeader,
) -> Result<(), AppError> {
    if header.schema_version != EXECUTION_HISTORY_SCHEMA_VERSION {
        return Err(unsupported_schema(
            relative,
            header.schema_version,
            EXECUTION_HISTORY_SCHEMA_VERSION,
        ));
    }
    validate_common_header(
        relative,
        &header.workbook_name,
        header.recorded_at_unix_millis,
        header.report_content_sha256.as_str(),
    )?;
    if header.execution_schema_version == 0
        || header.plan_schema_version != PLAN_SCHEMA_VERSION
        || !is_canonical_sha256(&header.plan_content_sha256)
        || !is_canonical_sha256(&header.target_fingerprint_sha256)
    {
        return Err(invalid_entry(relative, "执行历史的版本或摘要字段无效"));
    }
    Ok(())
}

fn validate_common_header(
    relative: &Path,
    workbook_name: &str,
    timestamp_unix_millis: i64,
    plan_content_sha256: &str,
) -> Result<(), AppError> {
    validate_workbook_name(workbook_name)
        .map_err(|source| invalid_entry(relative, &source.to_string()))?;
    if timestamp_unix_millis < 0 || !is_canonical_sha256(plan_content_sha256) {
        return Err(invalid_entry(relative, "历史时间戳或计划摘要字段无效"));
    }
    Ok(())
}

fn history_kind_rank(entry: &HistoryCatalogEntry) -> u8 {
    match entry.kind() {
        crate::application::HistoryRecordKind::Execution => 0,
        crate::application::HistoryRecordKind::Check => 1,
    }
}

fn invalid_entry(relative: &Path, message: &str) -> AppError {
    AppError::from_source(
        "history.catalog",
        AppErrorCode::HistoryReadFailed,
        "历史目录中存在无法核验的记录",
        HistoryReadError::Invalid {
            path: relative_path_string(relative),
            message: message.to_owned(),
        },
    )
    .with_context("path", relative_path_string(relative))
}

fn unsupported_schema(relative: &Path, actual: u32, expected: u32) -> AppError {
    AppError::from_source(
        "history.catalog",
        AppErrorCode::HistoryReadFailed,
        "历史记录 schema 版本不受支持",
        HistoryReadError::Schema {
            path: relative_path_string(relative),
            actual,
            expected,
        },
    )
    .with_context("path", relative_path_string(relative))
    .with_context("actual", actual.to_string())
    .with_context("expected", expected.to_string())
}

fn file_io_error(relative: &Path, source: io::Error) -> AppError {
    AppError::from_source(
        "history.catalog",
        AppErrorCode::HistoryReadFailed,
        "历史文件读取失败",
        HistoryReadError::Io {
            path: relative_path_string(relative),
            source,
        },
    )
    .with_context("path", relative_path_string(relative))
}

fn map_snapshot_error(relative: &Path, source: FileSnapshotError) -> AppError {
    match source {
        FileSnapshotError::Path(source) => map_file_error(relative, source),
        FileSnapshotError::Io { operation, source } => {
            file_io_error(relative, source).with_context("operation", operation)
        }
        FileSnapshotError::TooLarge { actual, maximum } => AppError::from_source(
            "history.catalog",
            AppErrorCode::HistoryReadFailed,
            "历史文件超过允许的读取大小",
            HistoryReadError::TooLarge {
                path: relative_path_string(relative),
                actual,
                maximum,
            },
        )
        .with_context("path", relative_path_string(relative)),
        FileSnapshotError::Changed => AppError::from_source(
            "history.catalog",
            AppErrorCode::HistoryReadFailed,
            "历史文件在读取期间发生变化",
            HistoryReadError::Changed {
                path: relative_path_string(relative),
            },
        )
        .with_context("path", relative_path_string(relative)),
    }
}

fn map_file_error(relative: &Path, source: ToolRootError) -> AppError {
    match source {
        ToolRootError::Io {
            operation, source, ..
        } => AppError::from_source(
            "history.catalog",
            AppErrorCode::HistoryReadFailed,
            "历史文件路径校验失败",
            HistoryReadError::Io {
                path: relative_path_string(relative),
                source,
            },
        )
        .with_context("path", relative_path_string(relative))
        .with_context("operation", operation),
        ToolRootError::InvalidRelativePath { message, .. }
        | ToolRootError::UnsafePath { message, .. }
        | ToolRootError::PathConflict { message, .. } => invalid_entry(relative, &message),
    }
}

fn map_directory_error(source: ToolRootError) -> AppError {
    let message = match source {
        ToolRootError::Io { operation, .. } => format!("{operation}失败"),
        ToolRootError::InvalidRelativePath { message, .. }
        | ToolRootError::UnsafePath { message, .. }
        | ToolRootError::PathConflict { message, .. } => message,
    };
    AppError::from_source(
        "history.catalog",
        AppErrorCode::HistoryReadFailed,
        "历史目录未能安全读取",
        HistoryReadError::Directory { message },
    )
    .with_context("path", HISTORY_DIRECTORY)
}

#[derive(Debug, Error)]
enum HistoryReadError {
    #[error("历史目录结构无效: {message}")]
    Directory { message: String },
    #[error("读取历史文件 {path} 失败: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("历史文件 {path} 超过 {maximum} 字节上限，实际为 {actual}")]
    TooLarge {
        path: String,
        actual: u64,
        maximum: u64,
    },
    #[error("历史文件 {path} 在读取期间发生变化")]
    Changed { path: String },
    #[error("历史文件 {path} JSON 无效: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("历史文件 {path} 无效: {message}")]
    Invalid { path: String, message: String },
    #[error("历史文件 {path} schema 为 {actual}，只支持 {expected}")]
    Schema {
        path: String,
        actual: u32,
        expected: u32,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::JsonHistoryCatalogPort;
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::HistoryCatalogPort;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn details_select_top_level_fields_without_scanning_other_files() {
        let fixture = TestDirectory::new("details");
        let directory = fixture.root.join("data/history");
        fs::create_dir_all(&directory).unwrap();
        write_check(&directory, 10, 'a');
        fs::write(directory.join("011-check.json"), b"broken").unwrap();
        let path = directory.join("010-check.json");
        let before = fs::read(&path).unwrap();
        let fields = vec!["report".to_owned(), "schema_version".to_owned()];
        let result =
            crate::bootstrap::read_history_details(&fixture.root, "010-check.json", Some(&fields))
                .unwrap();
        assert_eq!(result.as_object().unwrap().len(), 2);
        assert!(result.get("report").is_some());
        assert_eq!(fs::read(path).unwrap(), before);
        for field in ["missing", "report.message"] {
            let error = crate::bootstrap::read_history_details(
                &fixture.root,
                "010-check.json",
                Some(&[field.to_owned()]),
            )
            .unwrap_err();
            assert_eq!(error.stage(), "history.fields");
            assert_eq!(error.code(), crate::application::AppErrorCode::InputInvalid);
            assert!(error.message().contains("仅支持存在的顶层字段"));
            assert_eq!(
                error.context().get("field").map(String::as_str),
                Some(field)
            );
        }
        let corrupted = crate::bootstrap::read_history_details(
            &fixture.root,
            "011-check.json",
            Some(&["report.message".to_owned()]),
        )
        .unwrap_err();
        assert_eq!(corrupted.stage(), "history.catalog");
        assert_eq!(
            corrupted.code(),
            crate::application::AppErrorCode::HistoryReadFailed
        );
        assert!(corrupted.message().contains("JSON"));
        for name in [
            "../010-check.json",
            "data/history/010-check.json",
            "C:\\010-check.json",
            "..",
            "",
            "011-check.json",
        ] {
            assert!(crate::bootstrap::read_history_details(&fixture.root, name, None).is_err());
        }
    }

    #[test]
    fn lists_valid_history_in_newest_first_order_and_hashes_files() {
        let fixture = TestDirectory::new("valid");
        let directory = fixture.root.join("data/history");
        fs::create_dir_all(&directory).unwrap();
        write_check(&directory, 10, 'a');
        write_execution(&directory, 20, 'b');
        fs::write(directory.join("021-probe.json"), b"{}").unwrap();
        fs::write(directory.join("022-equipment.json"), b"{}").unwrap();
        fs::write(directory.join("023-portable.json"), b"{}").unwrap();
        fs::write(directory.join("024-ships.json"), b"{}").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let report = JsonHistoryCatalogPort::new(root)
            .list_history(&|| false)
            .unwrap();

        assert_eq!(report.count(), 2);
        assert_eq!(kind_label(report.entries()[0].kind()), "execution");
        assert_eq!(report.entries()[0].timestamp_unix_millis(), 20);
        assert_eq!(kind_label(report.entries()[1].kind()), "check");
        assert_eq!(report.entries()[1].timestamp_unix_millis(), 10);
        assert_eq!(
            report.entries()[0].file_sha256(),
            suzushiro_content_digest::sha256_bytes(
                &fs::read(directory.join("020-exec.json")).unwrap()
            )
        );
    }

    #[test]
    fn maps_an_oversized_history_file_to_the_existing_application_contract() {
        let fixture = TestDirectory::new("too-large");
        let directory = fixture.root.join("data/history");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("010-check.json");
        fs::File::create(path)
            .unwrap()
            .set_len(super::MAXIMUM_HISTORY_FILE_BYTES + 1)
            .unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let report = JsonHistoryCatalogPort::new(root)
            .list_history(&|| false)
            .unwrap();

        assert!(report.entries().is_empty());
        assert_eq!(report.failures().len(), 1);
        assert_eq!(
            report.failures()[0].relative_path(),
            "data/history/010-check.json"
        );
        assert_eq!(report.failures()[0].error_code(), "HISTORY_READ_FAILED");
        assert!(!report.failures()[0].message().is_empty());
    }

    #[test]
    fn cancellation_stops_before_the_next_history_file_and_is_not_a_partial_catalog() {
        use std::sync::atomic::AtomicUsize;

        let fixture = TestDirectory::new("cancel");
        let directory = fixture.root.join("data/history");
        fs::create_dir_all(&directory).unwrap();
        write_check(&directory, 10, 'a');
        write_check(&directory, 11, 'b');
        let checks = AtomicUsize::new(0);
        let error = JsonHistoryCatalogPort::new(ToolRoot::open(&fixture.root).unwrap())
            .list_history(&|| checks.fetch_add(1, Ordering::SeqCst) > 0)
            .unwrap_err();

        assert_eq!(error.stage(), "catalog.cancelled");
        assert_eq!(checks.load(Ordering::SeqCst), 2);
    }

    fn write_check(directory: &std::path::Path, timestamp: i64, token: char) {
        let digest = token.to_string().repeat(64);
        let document = json!({
            "schema_version": 1,
            "plan_schema_version": crate::application::PLAN_SCHEMA_VERSION,
            "workbook_name": "plan.xlsx",
            "checked_at_unix_millis": timestamp,
            "plan_content_sha256": digest,
            "report": {},
        });
        fs::write(
            directory.join(format!("{timestamp:03}-check.json")),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    }

    fn write_execution(directory: &std::path::Path, timestamp: i64, token: char) {
        let digest = token.to_string().repeat(64);
        let document = json!({
            "schema_version": 1,
            "execution_schema_version": 1,
            "plan_schema_version": crate::application::PLAN_SCHEMA_VERSION,
            "workbook_name": "plan.xlsx",
            "recorded_at_unix_millis": timestamp,
            "target_fingerprint_sha256": "c".repeat(64),
            "plan_content_sha256": digest,
            "report_content_sha256": "d".repeat(64),
            "report_status": "success",
            "may_have_writes": false,
            "report": {},
        });
        fs::write(
            directory.join(format!("{timestamp:03}-exec.json")),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    }

    fn kind_label(kind: crate::application::HistoryRecordKind) -> &'static str {
        match kind {
            crate::application::HistoryRecordKind::Check => "check",
            crate::application::HistoryRecordKind::Execution => "execution",
        }
    }

    struct TestDirectory {
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let root = home
                .join("suzushiro/scratch/azlw-history-catalog-tests")
                .join(format!(
                    "{label}-{}",
                    NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

//! 将工具根目录内的运行日志转换为不包含正文的稳定目录摘要。

use std::io;
use std::path::Path;

use thiserror::Error;

use super::super::file_snapshot::{FileSnapshotError, read_bounded_file_snapshot};
use super::super::history::relative_path_string;
use super::super::tool_root::{ToolRoot, ToolRootError};
use crate::application::{
    AppError, AppErrorCode, DirectoryEntryFailure, LOG_DIRECTORY, LogCatalogEntry, LogCatalogPort,
    LogCatalogReport, LogRecordKind,
};

pub(crate) const MAXIMUM_LOG_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn catalog_cancelled(code: AppErrorCode) -> AppError {
    AppError::from_source(
        "catalog.cancelled",
        code,
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

/// 使用固定工具根目录读取运行日志元数据。
pub(crate) struct JsonLogCatalogPort {
    tool_root: ToolRoot,
}

impl JsonLogCatalogPort {
    /// 固定受控工具根目录，读取时不接受外部日志路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl LogCatalogPort for JsonLogCatalogPort {
    fn list_logs(&self, is_cancelled: &dyn Fn() -> bool) -> Result<LogCatalogReport, AppError> {
        let files = self
            .tool_root
            .list_log_files()
            .map_err(map_directory_error)?;
        let mut entries = Vec::new();
        let mut failures = Vec::new();
        for (relative, _) in &files {
            if is_cancelled() {
                return Err(catalog_cancelled(AppErrorCode::LogReadFailed));
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
        entries.sort_by(|left, right| left.relative_path().cmp(right.relative_path()));
        failures.sort_by(|left, right| left.relative_path().cmp(right.relative_path()));
        Ok(LogCatalogReport::new(entries, failures))
    }
}

impl JsonLogCatalogPort {
    /// 状态过滤先于尾部截取；ADB 文本保持原行，不推断事件状态。
    pub(crate) fn read_details(
        &self,
        relative: &Path,
        tail: Option<usize>,
        status: Option<&str>,
    ) -> Result<serde_json::Value, AppError> {
        let kind = relative
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(classify_file_name)
            .map(|(_, kind)| kind)
            .ok_or_else(|| invalid_entry(relative, "日志文件名不是当前登记的日志格式"))?;
        let snapshot =
            read_bounded_file_snapshot(&self.tool_root, relative, MAXIMUM_LOG_FILE_BYTES)
                .map_err(|source| map_snapshot_error(relative, source))?;
        let (bytes, _) = snapshot.into_parts();
        let text = std::str::from_utf8(&bytes).map_err(|source| {
            AppError::from_source(
                "logs.show",
                AppErrorCode::LogReadFailed,
                "日志不是有效 UTF-8",
                source,
            )
            .with_context("path", relative_path_string(relative))
        })?;
        let mut events = Vec::new();
        let mut plain_text = false;
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(event) if event.is_object() => events.push(event),
                result => {
                    if kind == LogRecordKind::AdbServer {
                        plain_text = true;
                        break;
                    }
                    return Err(match result {
                        Err(source) => AppError::from_source(
                            "logs.show",
                            AppErrorCode::LogReadFailed,
                            "日志事件 JSON 无效",
                            source,
                        )
                        .with_context("path", relative_path_string(relative))
                        .with_context("line", (index + 1).to_string()),
                        Ok(_) => invalid_entry(
                            relative,
                            &format!("第 {} 行日志事件必须是 JSON 对象", index + 1),
                        ),
                    });
                }
            }
        }
        let format = if plain_text {
            if status.is_some() {
                return Err(invalid_entry(relative, "纯文本日志不支持 --status"));
            }
            events = text
                .lines()
                .map(|line| serde_json::Value::String(line.to_owned()))
                .collect();
            "text"
        } else {
            if let Some(status) = status {
                events.retain(|event| {
                    event.get("status").and_then(serde_json::Value::as_str) == Some(status)
                });
            }
            "jsonl"
        };
        if let Some(tail) = tail {
            let start = events.len().saturating_sub(tail);
            events.drain(..start);
        }
        Ok(serde_json::json!({ "format": format, "records": events }))
    }

    pub(crate) fn read_entry(&self, relative: &Path) -> Result<LogCatalogEntry, AppError> {
        let file_name = relative
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid_entry(relative, "日志文件名必须是有效 Unicode 文本"))?;
        let (sequence, kind) = classify_file_name(file_name)
            .ok_or_else(|| invalid_entry(relative, "日志文件名不是当前登记的日志格式"))?;
        let (size_bytes, file_sha256, timestamp_unix_millis) = self.read_file(relative)?;
        Ok(LogCatalogEntry::new(
            kind,
            relative_path_string(relative),
            size_bytes,
            file_sha256,
            timestamp_unix_millis,
            Some(sequence),
        ))
    }

    fn read_file(&self, relative: &Path) -> Result<(u64, String, i64), AppError> {
        let snapshot =
            read_bounded_file_snapshot(&self.tool_root, relative, MAXIMUM_LOG_FILE_BYTES)
                .map_err(|source| map_snapshot_error(relative, source))?;
        let modified = snapshot
            .modified()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid_entry(relative, "日志修改时间早于 Unix 纪元"))?;
        let timestamp_unix_millis = i64::try_from(modified.as_millis())
            .map_err(|_| invalid_entry(relative, "日志修改时间超出可表示范围"))?;
        let (bytes, sha256) = snapshot.into_parts();
        // 首条事件代表开始时间；纯文本或空日志没有此信息时使用文件时间。
        let started = bytes
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
            .and_then(|event| {
                event
                    .get("timestamp_ms")
                    .and_then(serde_json::Value::as_i64)
            })
            .filter(|timestamp| *timestamp > 0);
        Ok((
            bytes.len() as u64,
            sha256,
            started.unwrap_or(timestamp_unix_millis),
        ))
    }
}

/// 解析规范编号，并按三种已登记的日志名返回类型。
pub(crate) fn classify_file_name(file_name: &str) -> Option<(u64, LogRecordKind)> {
    let (sequence, name) = crate::adapters::numbered_files::numbered_name(file_name)?;
    let kind = match name {
        "app.log" => LogRecordKind::Operation,
        "runtime.jsonl" => LogRecordKind::RuntimeProbe,
        "adb.log" => LogRecordKind::AdbServer,
        _ => return None,
    };
    Some((sequence, kind))
}

fn invalid_entry(relative: &Path, message: &str) -> AppError {
    AppError::from_source(
        "logs.catalog",
        AppErrorCode::LogReadFailed,
        "日志目录中存在无法核验的文件",
        LogReadError::Invalid {
            path: relative_path_string(relative),
            message: message.to_owned(),
        },
    )
    .with_context("path", relative_path_string(relative))
}

fn file_io_error(relative: &Path, source: io::Error) -> AppError {
    AppError::from_source(
        "logs.catalog",
        AppErrorCode::LogReadFailed,
        "日志文件读取失败",
        LogReadError::Io {
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
            "logs.catalog",
            AppErrorCode::LogReadFailed,
            "日志文件超过允许的读取大小",
            LogReadError::TooLarge {
                path: relative_path_string(relative),
                actual,
                maximum,
            },
        )
        .with_context("path", relative_path_string(relative)),
        FileSnapshotError::Changed => AppError::from_source(
            "logs.catalog",
            AppErrorCode::LogReadFailed,
            "日志文件在读取期间发生变化",
            LogReadError::Changed {
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
            "logs.catalog",
            AppErrorCode::LogReadFailed,
            "日志文件路径校验失败",
            LogReadError::Io {
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
        "logs.catalog",
        AppErrorCode::LogReadFailed,
        "日志目录未能安全读取",
        LogReadError::Directory { message },
    )
    .with_context("path", LOG_DIRECTORY)
}

#[derive(Debug, Error)]
enum LogReadError {
    #[error("日志目录结构无效: {message}")]
    Directory { message: String },
    #[error("读取日志文件 {path} 失败: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("日志文件 {path} 超过 {maximum} 字节上限，实际为 {actual}")]
    TooLarge {
        path: String,
        actual: u64,
        maximum: u64,
    },
    #[error("日志文件 {path} 在读取期间发生变化")]
    Changed { path: String },
    #[error("日志文件 {path} 无效: {message}")]
    Invalid { path: String, message: String },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::JsonLogCatalogPort;
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::{LogCatalogPort, LogRecordKind};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn details_filter_before_tail_and_preserve_text_logs() {
        let fixture = TestDirectory::new("details");
        let directory = fixture.root.join("data/logs");
        fs::create_dir_all(&directory).unwrap();
        let bytes = b"{\"status\":\"failed\",\"id\":1}\n{\"status\":\"ok\",\"id\":2}\n{\"status\":\"failed\",\"id\":3}\n{\"status\":\"ok\",\"id\":4}\n";
        fs::write(directory.join("001-app.log"), bytes).unwrap();
        fs::write(directory.join("002-runtime.jsonl"), b"broken").unwrap();
        fs::write(directory.join("003-adb.log"), b"first\n\nlast\n").unwrap();
        let read = |name, tail, status| {
            crate::bootstrap::read_log_details(&fixture.root, name, tail, status)
        };
        let selected = read("001-app.log", Some(1), Some("failed")).unwrap();
        assert_eq!(
            selected,
            serde_json::json!({"format":"jsonl", "records":[{"status":"failed", "id":3}]})
        );
        assert_eq!(
            read("001-app.log", Some(0), None).unwrap()["records"],
            serde_json::json!([])
        );
        assert_eq!(
            read("001-app.log", None, Some("missing")).unwrap()["records"],
            serde_json::json!([])
        );
        assert_eq!(
            read("003-adb.log", Some(2), None).unwrap(),
            serde_json::json!({"format":"text", "records":["", "last"]})
        );
        assert!(read("003-adb.log", None, Some("failed")).is_err());
        let error = read("002-runtime.jsonl", None, None).unwrap_err();
        assert_eq!(error.context().get("line").map(String::as_str), Some("1"));
        assert!(std::error::Error::source(&error).is_some());
        assert_eq!(fs::read(directory.join("001-app.log")).unwrap(), bytes);
        for name in [
            "../001-app.log",
            "data/logs/001-app.log",
            "C:\\001-app.log",
            "004-other.log",
        ] {
            assert!(read(name, None, None).is_err());
        }
        fs::File::create(directory.join("005-app.log"))
            .unwrap()
            .set_len(super::MAXIMUM_LOG_FILE_BYTES + 1)
            .unwrap();
        assert!(read("005-app.log", Some(1), None).is_err());
    }

    #[test]
    fn lists_known_logs_with_hashes_and_without_content() {
        let fixture = TestDirectory::new("valid");
        let directory = fixture.root.join("data/logs");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("002-runtime.jsonl"),
            br#"{"stage":"host.start","details":{"serial":"127.0.0.1:16384"}}"#,
        )
        .unwrap();
        fs::write(directory.join("001-adb.log"), b"server output").unwrap();
        fs::write(
            directory.join("1000-app.log"),
            br#"{"timestamp_ms":1,"stage":"operation.start"}"#,
        )
        .unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let report = JsonLogCatalogPort::new(root).list_logs(&|| false).unwrap();

        assert_eq!(report.count(), 3);
        for entry in report.entries() {
            if entry.sequence() == Some(1000) {
                assert_eq!(entry.timestamp_unix_millis(), 1);
                continue;
            }
            let modified = fs::metadata(fixture.root.join(entry.relative_path()))
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis();
            assert_eq!(
                entry.timestamp_unix_millis(),
                i64::try_from(modified).unwrap()
            );
        }

        assert_eq!(report.entries()[0].kind(), LogRecordKind::AdbServer);
        assert_eq!(report.entries()[0].sequence(), Some(1));
        assert_eq!(report.entries()[1].kind(), LogRecordKind::RuntimeProbe);
        assert_eq!(report.entries()[1].sequence(), Some(2));
        assert_eq!(report.entries()[2].kind(), LogRecordKind::Operation);
        assert_eq!(report.entries()[2].sequence(), Some(1000));
        assert!(
            report.entries()[2].timestamp_unix_millis()
                < report.entries()[0].timestamp_unix_millis()
        );
        assert!(report.entries()[1].file_sha256().len() == 64);
        let serialized = serde_json::to_value(&report).unwrap();
        assert!(serialized["entries"][2].get("sequence").is_none());
        assert!(!serialized.to_string().contains("127.0.0.1:16384"));
    }

    #[test]
    fn appending_completion_does_not_change_the_log_start_time() {
        use std::io::Write;
        let fixture = TestDirectory::new("start-time");
        let directory = fixture.root.join("data/logs");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("001-app.log");
        fs::write(
            &path,
            b"{\"timestamp_ms\":123,\"stage\":\"operation.start\"}\n",
        )
        .unwrap();
        let adapter = JsonLogCatalogPort::new(ToolRoot::open(&fixture.root).unwrap());
        let before = adapter.list_logs(&|| false).unwrap();
        assert_eq!(before.entries()[0].timestamp_unix_millis(), 123);
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"{\"timestamp_ms\":999,\"stage\":\"operation.complete\"}\n")
            .unwrap();
        drop(file);
        let after = adapter.list_logs(&|| false).unwrap();
        assert_eq!(after.entries()[0].timestamp_unix_millis(), 123);
        assert_ne!(
            before.entries()[0].file_sha256(),
            after.entries()[0].file_sha256()
        );
    }

    #[test]
    fn numbered_names_preserve_kind() {
        for (name, sequence, kind) in [
            ("001-app.log", 1, LogRecordKind::Operation),
            ("002-adb.log", 2, LogRecordKind::AdbServer),
            ("010-runtime.jsonl", 10, LogRecordKind::RuntimeProbe),
            ("1000-app.log", 1000, LogRecordKind::Operation),
        ] {
            assert_eq!(super::classify_file_name(name), Some((sequence, kind)));
        }
        for name in [
            "",
            "app.log",
            "1-app.log",
            "000-app.log",
            "0001-app.log",
            "001-app.log.bak",
            "001-operation-00112233445566778899aabbccddeeff.log",
            "18446744073709551616-app.log",
            "００１-app.log",
        ] {
            assert_eq!(super::classify_file_name(name), None);
        }
    }

    #[test]
    fn rejects_unknown_log_names_instead_of_skipping_them() {
        let fixture = TestDirectory::new("unknown");
        let directory = fixture.root.join("data/logs");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("notes.txt"), b"unexpected").unwrap();
        fs::write(directory.join("003-app.log"), b"healthy").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let report = JsonLogCatalogPort::new(root).list_logs(&|| false).unwrap();

        assert_eq!(report.count(), 1);
        assert_eq!(report.entries()[0].relative_path(), "data/logs/003-app.log");
        assert_eq!(report.failures().len(), 1);
        assert_eq!(report.failures()[0].relative_path(), "data/logs/notes.txt");
        assert_eq!(report.failures()[0].error_code(), "LOG_READ_FAILED");
        assert!(!report.failures()[0].message().is_empty());
    }

    #[test]
    fn treats_a_missing_log_directory_as_empty() {
        let fixture = TestDirectory::new("empty");
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert_eq!(
            JsonLogCatalogPort::new(root)
                .list_logs(&|| false)
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn maps_an_oversized_log_to_the_existing_application_contract() {
        let fixture = TestDirectory::new("too-large");
        let directory = fixture.root.join("data/logs");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("002-runtime.jsonl");
        fs::File::create(path)
            .unwrap()
            .set_len(super::MAXIMUM_LOG_FILE_BYTES + 1)
            .unwrap();
        fs::write(directory.join("003-app.log"), b"healthy").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let report = JsonLogCatalogPort::new(root).list_logs(&|| false).unwrap();

        assert_eq!(report.count(), 1);
        assert_eq!(report.failures().len(), 1);
        assert_eq!(
            report.failures()[0].relative_path(),
            "data/logs/002-runtime.jsonl"
        );
        assert_eq!(report.failures()[0].error_code(), "LOG_READ_FAILED");
    }

    #[test]
    fn cancellation_stops_before_the_next_log_and_is_not_a_partial_catalog() {
        use std::sync::atomic::AtomicUsize;

        let fixture = TestDirectory::new("cancel");
        let directory = fixture.root.join("data/logs");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("001-app.log"), b"first").unwrap();
        fs::write(directory.join("002-app.log"), b"second").unwrap();
        let checks = AtomicUsize::new(0);
        let error = JsonLogCatalogPort::new(ToolRoot::open(&fixture.root).unwrap())
            .list_logs(&|| checks.fetch_add(1, Ordering::SeqCst) > 0)
            .unwrap_err();

        assert_eq!(error.stage(), "catalog.cancelled");
        assert_eq!(checks.load(Ordering::SeqCst), 2);
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
                .join("suzushiro/scratch/azlw-log-catalog-tests")
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

//! 为运行态与界面操作提供一致的追加式事件写入。

use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 每条事件完整编码、追加并刷盘；调用方负责排他创建文件和处理写入错误。
pub(crate) fn write_event(
    file: &mut File,
    stage: &str,
    status: &str,
    details: Value,
    related: Option<&RelatedLogSink>,
) -> io::Result<()> {
    let result = (|| {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis();
        let mut bytes = serde_json::to_vec(&json!({
            "timestamp_ms": timestamp_ms, "stage": stage, "status": status, "details": details
        }))
        .map_err(io::Error::other)?;
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_data()
    })();
    if let Err(error) = &result
        && let Some(related) = related
    {
        related.note_error(format!("日志阶段 {stage} 写入失败: {error}"));
    }
    result
}

#[derive(Default)]
struct RelatedLogState {
    paths: Vec<String>,
    errors: Vec<String>,
}

/// 一次操作收集到的底层日志路径和写入故障。
#[derive(Clone)]
pub struct RelatedLogSink {
    state: Arc<Mutex<RelatedLogState>>,
}

impl std::fmt::Debug for RelatedLogSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RelatedLogSink")
    }
}

impl RelatedLogSink {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RelatedLogState::default())),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RelatedLogState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn register(&self, path: &Path) {
        self.lock().paths.push(path.display().to_string());
    }

    pub(crate) fn note_error(&self, message: impl Into<String>) {
        self.lock().errors.push(message.into());
    }

    pub(crate) fn errors(&self) -> Vec<String> {
        self.lock().errors.clone()
    }

    pub(crate) fn paths(&self) -> Vec<String> {
        self.lock().paths.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn related_logs_stay_with_the_operation_that_records_them() {
        let operation = RelatedLogSink::new();
        let other = RelatedLogSink::new();
        operation.register(Path::new("outer.log"));
        other.register(Path::new("other.log"));
        let mut file = File::open("Cargo.toml").unwrap();
        assert!(write_event(&mut file, "test.write", "ok", json!({}), Some(&operation)).is_err());
        assert_eq!(operation.paths(), ["outer.log"]);
        assert_eq!(operation.errors().len(), 1);
        assert_eq!(other.paths(), ["other.log"]);
        assert!(other.errors().is_empty());
    }
}

/// 在同一工具目录内串行分配日志编号；锁句柄关闭时由操作系统释放。
pub(crate) fn create_numbered_log(
    root: &super::super::tool_root::ToolRoot,
    name: &str,
) -> io::Result<(std::path::PathBuf, std::path::PathBuf, File)> {
    use crate::adapters::numbered_files::NumberedDirectory;
    use std::fs::OpenOptions;
    let (kind, extension) = match name {
        "app.log" => ("app", "log"),
        "runtime.jsonl" => ("runtime", "jsonl"),
        "adb.log" => ("adb", "log"),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "日志基本文件名无效",
            ));
        }
    };
    let directory = NumberedDirectory::open(root, Path::new("data/logs"))?;
    let relative = directory.next_path(kind, extension, true)?;
    let path = root.prepare_new_file(&relative).map_err(io::Error::other)?;
    let file = OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(&path)?;
    Ok((relative, path, file))
}

#[cfg(test)]
mod numbering_tests {
    use super::*;
    use crate::adapters::tool_root::ToolRoot;

    #[test]
    fn numbering_is_shared_resumes_and_keeps_existing_files() {
        let id = suzushiro_session_core::SessionId::generate().unwrap();
        let path = std::path::PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-log-number-tests")
        .join(id.to_string());
        std::fs::create_dir_all(path.join("data/logs")).unwrap();
        let old = path.join("data/logs/001-adb.log");
        std::fs::write(&old, b"old log").unwrap();
        let root = ToolRoot::open(&path).unwrap();
        let (first, _, file) = create_numbered_log(&root, "app.log").unwrap();
        drop(file);
        assert_eq!(first.file_name().unwrap(), "002-app.log");
        drop(root);
        let root = ToolRoot::open(&path).unwrap();
        let (second, _, file) = create_numbered_log(&root, "adb.log").unwrap();
        drop(file);
        assert_eq!(second.file_name().unwrap(), "003-adb.log");
        let workers: Vec<_> = (4..=15)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let (relative, _, file) = create_numbered_log(&root, "runtime.jsonl").unwrap();
                    drop(file);
                    crate::adapters::numbered_files::numbered_name(
                        relative.file_name().unwrap().to_str().unwrap(),
                    )
                    .unwrap()
                    .0
                })
            })
            .collect();
        let mut numbers: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        numbers.sort();
        assert_eq!(numbers, (4..=15).collect::<Vec<_>>());
        assert_eq!(std::fs::read(old).unwrap(), b"old log");
        assert!(create_numbered_log(&root, "invalid.log").is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
}

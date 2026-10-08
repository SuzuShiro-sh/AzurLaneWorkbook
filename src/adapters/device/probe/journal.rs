//! 持久化追加式探针日志，并保留可诊断错误正文。

use std::fs::File;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use crate::adapters::diagnostics::journal::write_event;
use serde_json::Value;

use super::super::reading::equipment::EquipmentReadError;
#[cfg(test)]
use super::super::session::SessionId;
use super::RuntimeProbeError;
use crate::adapters::tool_root::ToolRoot;

/// 装备读取保持脱敏分类，其余错误保留完整正文与上下文。
pub(super) fn journal_error_summary(error: &RuntimeProbeError) -> String {
    match error {
        RuntimeProbeError::EquipmentRead(source) => match source {
            EquipmentReadError::Runtime(_) => "完整装备目录运行态请求失败".to_owned(),
            EquipmentReadError::Mapping(mapping) => mapping.diagnostic_summary(),
            EquipmentReadError::Protocol(_) => "完整装备目录引用键无效".to_owned(),
            EquipmentReadError::EncodeRawRecords(_) => "完整装备原始记录摘要编码失败".to_owned(),
            EquipmentReadError::IncompleteDetails(details) => format!(
                "{}详情批次不完整，共 {} 条失败记录",
                details.kind(),
                details.failures().len()
            ),
        },
        RuntimeProbeError::GameStateRead(source) => {
            let cause = std::error::Error::source(source)
                .map(ToString::to_string)
                .unwrap_or_else(|| "无底层原因".to_owned());
            format!("{}; context={:?}; source={cause}", source, source.context())
        }
        _ => error.to_string(),
    }
}

/// 持有单次探针的追加式 JSONL 日志文件和稳定路径。
pub(super) struct ProbeJournal {
    pub(super) path: PathBuf,
    file: File,
    related: Option<crate::adapters::RelatedLogSink>,
}

impl ProbeJournal {
    /// 以排他新建方式打开会话日志，防止覆盖既有证据。
    pub(super) fn create(
        tool_root: &ToolRoot,
        related: Option<&crate::adapters::RelatedLogSink>,
    ) -> Result<Self, RuntimeProbeError> {
        let (relative_path, path, file) =
            crate::adapters::create_numbered_log(tool_root, "runtime.jsonl").map_err(|source| {
                RuntimeProbeError::Io {
                    stage: "host.create_journal",
                    path: tool_root.as_path().join("data/logs"),
                    source,
                }
            })?;
        if let Some(related) = related {
            related.register(&relative_path);
        }
        Ok(Self {
            path,
            file,
            related: related.cloned(),
        })
    }

    /// 写入单条带时间、阶段、状态和结构化详情的日志并立即刷新。
    pub(super) fn record(
        &mut self,
        stage: &'static str,
        status: &'static str,
        details: Value,
    ) -> Result<(), RuntimeProbeError> {
        write_event(
            &mut self.file,
            stage,
            status,
            details,
            self.related.as_ref(),
        )
        .map_err(|source| RuntimeProbeError::Io {
            stage: "host.flush_journal",
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_preserves_full_events_and_allocates_distinct_files() {
        let id = SessionId::generate().unwrap();
        let path = PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-probe-log-tests")
        .join(id.to_string());
        std::fs::create_dir_all(&path).unwrap();
        let root = ToolRoot::open(&path).unwrap();
        root.ensure_directory(Path::new("data/logs")).unwrap();
        let scope = crate::adapters::RelatedLogSink::new();
        let mut journal = ProbeJournal::create(&root, Some(&scope)).unwrap();
        let detail = format!("{}\n末尾", "错误".repeat(1000));
        journal
            .record("test.start", "ok", serde_json::json!({}))
            .unwrap();
        journal
            .record(
                "test.failure",
                "error",
                serde_json::json!({"message": detail}),
            )
            .unwrap();
        let bytes = std::fs::read(&journal.path).unwrap();
        assert!(!bytes.contains(&b'\r'));
        let events: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["details"]["message"], detail);
        let second = ProbeJournal::create(&root, Some(&scope)).unwrap();
        assert_ne!(second.path, journal.path);
        drop(second);
        assert_eq!(std::fs::read(&journal.path).unwrap(), bytes);
        assert!(
            scope
                .paths()
                .iter()
                .all(|path| Path::new(path).is_relative())
        );
        assert!(scope.errors().is_empty());
        drop(journal);
        std::fs::remove_dir_all(path).unwrap();
    }
}

//! 将操作阶段通知与持久操作日志绑定，日志失败不打断正在进行的业务收尾。

use crate::adapters::settings::{Settings, SettingsSnapshot};
use crate::adapters::{RelatedLogSink, tool_root::ToolRoot, write_event};
use crate::application::{OperationTerminal, UserPreferences};
use serde_json::json;
use std::cell::RefCell;
use std::fs::File;
use std::path::Path;
use suzushiro_task_runtime::TaskSignalError;

/// 一次操作写日志时使用的标识，不依赖界面枚举。
pub struct OperationIdentity<'a> {
    pub name: &'a str,
    pub workbook: Option<&'a str>,
}

/// 本次操作已经读取的诊断开关。日志上下文不再自己打开设置文件。
pub struct OperationSettings {
    pub detailed_diagnostics: bool,
    pub read_error: Option<String>,
}

/// 一次操作校验后的设置。日志、设备和生成只使用这里的片段。
pub struct OperationConfiguration {
    settings: Option<Settings>,
    source_sha256: Option<String>,
    detail: Option<String>,
    summary: Option<String>,
}

impl OperationConfiguration {
    /// 读取并校验一次设置，保留这份原文的内容身份。
    pub fn load(root: &Path) -> Self {
        match SettingsSnapshot::load(root) {
            Ok(snapshot) => Self {
                source_sha256: Some(snapshot.source_sha256().to_owned()),
                settings: Some(snapshot.into_settings()),
                detail: None,
                summary: None,
            },
            Err(error) => Self {
                settings: None,
                source_sha256: None,
                detail: Some(error.to_string()),
                summary: Some(error.summary()),
            },
        }
    }

    /// 返回这次校验所依据的原文摘要。
    pub fn source_sha256(&self) -> Option<&str> {
        self.source_sha256.as_deref()
    }

    /// 返回已校验设置。失败时保留不含绝对路径的原始摘要。
    pub fn settings(&self) -> Result<&Settings, &str> {
        self.settings
            .as_ref()
            .ok_or_else(|| self.summary.as_deref().unwrap_or("设置未读取"))
    }

    /// 返回交给工作簿生成的用户偏好。
    pub fn preferences(&self) -> Result<UserPreferences, String> {
        self.settings()
            .map(Settings::preferences)
            .map_err(str::to_owned)
    }

    /// 从这份已加载设置派生日志诊断开关，不再打开设置文件。
    pub fn operation_settings(&self) -> OperationSettings {
        OperationSettings {
            detailed_diagnostics: self
                .settings
                .as_ref()
                .map(|settings| settings.preferences().detailed_diagnostics)
                .unwrap_or(true),
            read_error: self.detail.clone(),
        }
    }
}

/// 业务结束后写入操作日志的结构化结果。
pub struct OperationRecord {
    pub terminal: OperationTerminal,
    pub message: String,
    pub detail: String,
}

/// 操作期间的进度与取消信号。界面和命令行各自提供实现。
pub trait OperationSignals {
    fn notify_activity(
        &self,
        units: Option<(usize, usize)>,
        message: String,
    ) -> Result<(), TaskSignalError>;
    fn notify_progress(&self, percent: u8, message: String) -> Result<(), TaskSignalError>;
    fn cancelled(&self) -> bool;

    fn share_cancellation(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(self.cancelled()))
    }
}

pub struct OperationContext<'a> {
    signals: &'a dyn OperationSignals,
    file: RefCell<Option<File>>,
    write_error: RefCell<Option<String>>,
    related: Option<RelatedLogSink>,
    detailed: bool,
}

impl<'a> OperationContext<'a> {
    pub(super) fn unlogged(signals: &'a dyn OperationSignals) -> Self {
        Self {
            signals,
            file: RefCell::new(None),
            write_error: RefCell::new(None),
            related: None,
            detailed: true,
        }
    }

    pub fn new(
        root: &Path,
        operation: OperationIdentity<'_>,
        instance: Option<&str>,
        settings: &OperationSettings,
        signals: &'a dyn OperationSignals,
    ) -> Self {
        let opened = (|| -> Result<File, Box<dyn std::error::Error>> {
            let root = ToolRoot::open(root)?;
            root.ensure_directory(Path::new("data/logs"))?;
            let (_, _, file) = crate::adapters::create_numbered_log(&root, "app.log")?;
            Ok(file)
        })();
        let (file, error) = match opened {
            Ok(file) => (Some(file), None),
            Err(error) => (None, Some(format!("创建操作日志失败: {error}"))),
        };
        let logged = Self {
            signals,
            file: RefCell::new(file),
            write_error: RefCell::new(error),
            related: Some(RelatedLogSink::new()),
            detailed: settings.detailed_diagnostics,
        };
        logged.record(
            "operation.start",
            "ok",
            json!({"operation": operation.name, "instance": instance, "workbook": operation.workbook}),
        );
        if let Some(error) = &settings.read_error {
            logged.record("settings.read", "failed", json!({"detail": error}));
        }
        logged
    }

    fn record(&self, stage: &str, status: &str, details: serde_json::Value) {
        if let Some(file) = self.file.borrow_mut().as_mut()
            && let Err(error) = write_event(file, stage, status, details, self.related.as_ref())
        {
            self.write_error
                .borrow_mut()
                .get_or_insert_with(|| format!("操作日志写入失败: {error}"));
        }
    }

    pub(crate) fn report_activity(
        &self,
        units: Option<(usize, usize)>,
        message: impl Into<String>,
    ) -> Result<(), TaskSignalError> {
        let message = message.into();
        if self.detailed || units.is_none() {
            self.record(
                "operation.progress",
                "ok",
                json!({"message": message, "units": units}),
            );
        }
        self.signals.notify_activity(units, message)
    }

    pub(super) fn report_progress(
        &self,
        percent: u8,
        message: impl Into<String>,
    ) -> Result<(), TaskSignalError> {
        let message = message.into();
        self.record(
            "operation.progress",
            "ok",
            json!({"message": message, "percent": percent}),
        );
        self.signals.notify_progress(percent, message)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.signals.cancelled()
    }

    pub(super) fn cancellation_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.signals.share_cancellation()
    }

    pub fn finish(&self, record: &OperationRecord) -> Vec<String> {
        self.record(
            "operation.complete",
            record.terminal.status(),
            json!({"message": record.message, "detail": record.detail, "uncertain": record.terminal.uncertain(), "related_logs": self.related.as_ref().map(RelatedLogSink::paths)}),
        );
        self.file.borrow_mut().take();
        let mut errors = self
            .related
            .as_ref()
            .map(RelatedLogSink::errors)
            .unwrap_or_default();
        if let Some(error) = self.write_error.borrow().as_ref() {
            errors.push(error.clone());
        }
        errors
    }

    /// 返回这一次操作的相关日志收集器，供同一次设备会话登记路径。
    pub fn related_logs(&self) -> Option<RelatedLogSink> {
        self.related.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use suzushiro_task_runtime::{BackgroundTask, TaskEvent};

    struct TestSignals;

    impl OperationSignals for TestSignals {
        fn notify_activity(
            &self,
            _units: Option<(usize, usize)>,
            _message: String,
        ) -> Result<(), TaskSignalError> {
            Ok(())
        }

        fn notify_progress(&self, _percent: u8, _message: String) -> Result<(), TaskSignalError> {
            Ok(())
        }

        fn cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn preserves_complete_events_long_errors_and_prior_operations() {
        let root = std::path::PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-operation-log-tests")
        .join(
            suzushiro_session_core::SessionId::generate()
                .unwrap()
                .to_string(),
        );
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            include_str!("../../settings.json"),
        )
        .unwrap();
        for (state, detailed) in [
            ("ok", true),
            ("failed", true),
            ("cancelled", true),
            ("failed", false),
        ] {
            let task_root = root.clone();
            let task = BackgroundTask::new(move |_context| {
                let signals = TestSignals;
                let logged = OperationContext::new(
                    &task_root,
                    OperationIdentity {
                        name: "刷新信息",
                        workbook: None,
                    },
                    None,
                    &OperationSettings {
                        detailed_diagnostics: detailed,
                        read_error: None,
                    },
                    &signals,
                );
                logged.report_activity(Some((1, 2)), "逐项明细").unwrap();
                logged.report_activity(None, "测试阶段").unwrap();
                logged
                    .related
                    .as_ref()
                    .unwrap()
                    .register(Path::new("data/logs/linked.jsonl"));
                let record = if state == "failed" {
                    OperationRecord {
                        terminal: OperationTerminal::Failed,
                        message: "失败".to_owned(),
                        detail: format!("{}完整末尾", "长错误".repeat(1000)),
                    }
                } else if state == "cancelled" {
                    OperationRecord {
                        terminal: OperationTerminal::Cancelled,
                        message: "已取消".to_owned(),
                        detail: String::new(),
                    }
                } else {
                    OperationRecord {
                        terminal: OperationTerminal::Succeeded,
                        message: "成功".to_owned(),
                        detail: String::new(),
                    }
                };
                assert!(logged.finish(&record).is_empty());
                Ok(())
            });
            let running = task.spawn(|| {});
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match running.try_recv() {
                    Ok(TaskEvent::Succeeded { .. }) => break,
                    Ok(TaskEvent::Failed { failure }) => panic!("{}", failure.detail()),
                    _ => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            running.finish().unwrap();
        }
        let logs: Vec<_> = std::fs::read_dir(root.join("data/logs"))
            .unwrap()
            .map(|entry| {
                let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
                let events: Vec<serde_json::Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                assert!(events.len() == 3 || events.len() == 4);
                assert_eq!(events[0]["stage"], "operation.start");
                assert_eq!(events[events.len() - 2]["details"]["message"], "测试阶段");
                assert_eq!(events.last().unwrap()["stage"], "operation.complete");
                text
            })
            .collect();
        assert_eq!(logs.len(), 4);
        let concise = logs
            .iter()
            .filter(|text| !text.contains("逐项明细"))
            .collect::<Vec<_>>();
        assert_eq!(concise.len(), 1);
        assert!(concise[0].contains("完整末尾"));
        assert!(logs.iter().any(|text| text.contains("cancelled")));
        assert!(
            logs.iter()
                .all(|text| text.contains("data/logs/linked.jsonl"))
        );
        assert!(logs.iter().any(|text| text.contains("完整末尾")));
        std::fs::remove_dir_all(root).unwrap();
    }
}

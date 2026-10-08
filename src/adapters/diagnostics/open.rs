//! 核对选中的历史或日志后，用系统程序打开原文件。

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::super::history::catalog::JsonHistoryCatalogPort;
use super::log_catalog::JsonLogCatalogPort;
use crate::application::{
    AppError, AppErrorCode, DiagnosticArtifactKind, DiagnosticArtifactRef, DiagnosticOpenPort,
    HISTORY_DIRECTORY, LOG_DIRECTORY,
};

/// 对固定工具根目录内的已登记诊断文件执行打开。
pub(crate) struct ValidatedDiagnosticOpenPort {
    tool_root: crate::adapters::tool_root::ToolRoot,
}

impl ValidatedDiagnosticOpenPort {
    /// 固定来源工具根目录。
    pub(crate) fn new(tool_root: crate::adapters::tool_root::ToolRoot) -> Self {
        Self { tool_root }
    }

    fn open_with_launcher(
        &self,
        source: &DiagnosticArtifactRef,
        launch: impl FnOnce(&Path) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let relative = selected_relative(source)?;
        let (size_bytes, file_sha256) = match source.kind() {
            DiagnosticArtifactKind::History => {
                let entry =
                    JsonHistoryCatalogPort::new(self.tool_root.clone()).read_entry(&relative)?;
                (entry.size_bytes(), entry.file_sha256().to_owned())
            }
            DiagnosticArtifactKind::Log => {
                let entry =
                    JsonLogCatalogPort::new(self.tool_root.clone()).read_entry(&relative)?;
                (entry.size_bytes(), entry.file_sha256().to_owned())
            }
        };
        if size_bytes != source.size_bytes() || file_sha256 != source.file_sha256() {
            return Err(open_error(
                source,
                DiagnosticOpenError::SourceChanged {
                    relative_path: source.relative_path().to_owned(),
                },
            ));
        }
        let path = self.tool_root.existing_file(&relative).map_err(|error| {
            AppError::from_source(
                "diagnostics.open",
                code_for(source.kind()),
                "诊断文件路径校验失败",
                error,
            )
            .with_context("path", source.relative_path())
        })?;
        launch(&path)
    }
}

impl DiagnosticOpenPort for ValidatedDiagnosticOpenPort {
    fn open_diagnostic(&self, source: &DiagnosticArtifactRef) -> Result<(), AppError> {
        self.open_with_launcher(source, |path| {
            crate::adapters::workbook::launch_default_application(path)
                .map(|_| ())
                .map_err(|error| {
                    AppError::from_source(
                        "diagnostics.open",
                        code_for(source.kind()),
                        "系统默认程序未能打开所选诊断文件",
                        error,
                    )
                    .with_context("path", source.relative_path())
                })
        })
        .map_err(|error| {
            AppError::from_source(
                "diagnostics.open",
                code_for(source.kind()),
                "所选诊断文件未能打开，请刷新信息后核对文件",
                error,
            )
            .with_context("path", source.relative_path())
        })
    }
}

/// 只接受对应目录下的直接文件名，避免打开时扫描整个目录。
fn selected_relative(source: &DiagnosticArtifactRef) -> Result<PathBuf, AppError> {
    let directory = match source.kind() {
        DiagnosticArtifactKind::History => HISTORY_DIRECTORY,
        DiagnosticArtifactKind::Log => LOG_DIRECTORY,
    };
    let relative = Path::new(source.relative_path());
    let parent = relative
        .parent()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    let file_name = relative.file_name().and_then(|name| name.to_str());
    if parent.as_deref() != Some(directory)
        || file_name.is_none()
        || source.relative_path().contains("..")
    {
        return Err(open_error(
            source,
            DiagnosticOpenError::SourceChanged {
                relative_path: source.relative_path().to_owned(),
            },
        ));
    }
    Ok(relative.to_path_buf())
}

fn code_for(kind: DiagnosticArtifactKind) -> AppErrorCode {
    match kind {
        DiagnosticArtifactKind::History => AppErrorCode::HistoryReadFailed,
        DiagnosticArtifactKind::Log => AppErrorCode::LogReadFailed,
    }
}

fn open_error(source: &DiagnosticArtifactRef, error: DiagnosticOpenError) -> AppError {
    AppError::from_source(
        "diagnostics.open",
        code_for(source.kind()),
        "所选诊断文件已变化，请刷新后再打开",
        error,
    )
    .with_context("path", source.relative_path())
}

#[derive(Debug, Error)]
enum DiagnosticOpenError {
    #[error("所选诊断文件已变化: {relative_path}")]
    SourceChanged { relative_path: String },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::ValidatedDiagnosticOpenPort;
    use crate::adapters::diagnostics::log_catalog::JsonLogCatalogPort;
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::{DiagnosticArtifactRef, LogCatalogPort};

    #[test]
    fn opens_selected_source_without_creating_a_copy_and_rejects_changed_source() {
        let base = test_root("open");
        let release = base.join("release");
        fs::create_dir_all(release.join("data/logs")).unwrap();
        let path = release.join("data/logs/001-adb.log");
        fs::write(&path, b"source").unwrap();
        let root = ToolRoot::open(&release).unwrap();
        let catalog = JsonLogCatalogPort::new(root.clone())
            .list_logs(&|| false)
            .unwrap();
        let source = DiagnosticArtifactRef::from_log(&catalog.entries()[0]);
        let adapter = ValidatedDiagnosticOpenPort::new(root);
        let mut launched = false;
        adapter
            .open_with_launcher(&source, |opened| {
                assert_eq!(
                    fs::canonicalize(opened).unwrap(),
                    fs::canonicalize(&path).unwrap()
                );
                launched = true;
                Ok(())
            })
            .unwrap();
        assert!(launched);
        assert_eq!(fs::read(&path).unwrap(), b"source");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::write(&path, b"edited").unwrap();
        let error = adapter
            .open_with_launcher(&source, |_| panic!("内容变化后不应打开"))
            .unwrap_err();
        assert!(error.to_string().contains("诊断"));
        fs::remove_file(&path).unwrap();
        assert!(
            adapter
                .open_with_launcher(&source, |_| panic!("缺失文件不应打开"))
                .is_err()
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn opens_numbered_log_through_existing_catalog_validation() {
        let base = test_root("numbered-open");
        fs::create_dir_all(base.join("data/logs")).unwrap();
        let root = ToolRoot::open(&base).unwrap();
        let (_, path, mut file) = crate::adapters::create_numbered_log(&root, "adb.log").unwrap();
        crate::adapters::write_event(&mut file, "test", "ok", serde_json::json!({}), None).unwrap();
        drop(file);
        let report = JsonLogCatalogPort::new(root.clone())
            .list_logs(&|| false)
            .unwrap();
        let source = DiagnosticArtifactRef::from_log(&report.entries()[0]);
        assert_eq!(source.relative_path(), "data/logs/001-adb.log");
        ValidatedDiagnosticOpenPort::new(root)
            .open_with_launcher(&source, |opened| {
                assert_eq!(opened, path);
                Ok(())
            })
            .unwrap();
        fs::remove_dir_all(base).unwrap();
    }

    fn test_root(label: &str) -> PathBuf {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).unwrap();
        home.join("suzushiro/scratch/azlw-diagnostic-open-tests")
            .join(format!(
                "{label}-{}-{:016x}",
                std::process::id(),
                u64::from_le_bytes(random)
            ))
    }
}

//! 将项目资源目录和统一日志接入独立 ADB 库。
use crate::adapters::tool_root::ToolRoot;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use suzushiro_adb::{AdbBundle, AdbConfig, AdbLogSink, IsolatedAdbError};

pub(crate) fn load_adb_bundle(
    root: &Path,
    related: Option<&crate::adapters::RelatedLogSink>,
) -> Result<AdbBundle, IsolatedAdbError> {
    load_adb_bundle_from_executable(root, Path::new("runtime/adb/adb.exe"), related)
}
pub(crate) fn load_adb_bundle_from_executable(
    root: &Path,
    executable: &Path,
    related: Option<&crate::adapters::RelatedLogSink>,
) -> Result<AdbBundle, IsolatedAdbError> {
    let tool_root = ToolRoot::open(root)?;
    AdbBundle::load(
        AdbConfig {
            root: root.into(),
            executable: executable.into(),
            state_directory: "data/adb".into(),
            log_directory: "data/logs".into(),
        },
        Arc::new(ProjectAdbLog {
            root: tool_root,
            related: related.cloned(),
        }),
    )
}

#[derive(Debug)]
struct ProjectAdbLog {
    root: ToolRoot,
    related: Option<crate::adapters::RelatedLogSink>,
}
impl AdbLogSink for ProjectAdbLog {
    fn create(&self, directory: &Path, _name: &str) -> std::io::Result<(PathBuf, File)> {
        let expected = self
            .root
            .ensure_directory(Path::new("data/logs"))
            .map_err(std::io::Error::other)?;
        if directory != expected {
            return Err(std::io::Error::other("ADB 日志目录与项目配置不一致"));
        }
        let (relative, path, file) = crate::adapters::create_numbered_log(&self.root, "adb.log")?;
        if let Some(related) = &self.related {
            related.register(&relative);
        }
        Ok((path, file))
    }
    fn write_event(
        &self,
        file: &mut File,
        stage: &str,
        status: &str,
        details: serde_json::Value,
    ) -> std::io::Result<()> {
        crate::adapters::write_event(file, stage, status, details, self.related.as_ref())
    }
}

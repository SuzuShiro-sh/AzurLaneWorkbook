//! 定义可打开的历史和日志原文件身份。

use serde::Serialize;

use super::super::{HistoryCatalogEntry, LogCatalogEntry};

/// 可以显式打开的受控诊断文件类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticArtifactKind {
    /// data/history 下已经通过目录外壳校验的历史文件。
    History,
    /// data/logs 下已经通过文件名和内容边界校验的运行日志。
    Log,
}

/// 由目录项建立的来源身份，打开前必须重新核对路径、大小和摘要。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticArtifactRef {
    kind: DiagnosticArtifactKind,
    relative_path: String,
    size_bytes: u64,
    file_sha256: String,
}

impl DiagnosticArtifactRef {
    /// 从已校验的历史目录项建立来源身份。
    pub(crate) fn from_history(entry: &HistoryCatalogEntry) -> Self {
        Self {
            kind: DiagnosticArtifactKind::History,
            relative_path: entry.relative_path().to_owned(),
            size_bytes: entry.size_bytes(),
            file_sha256: entry.file_sha256().to_owned(),
        }
    }

    /// 从已校验的日志目录项建立来源身份。
    pub(crate) fn from_log(entry: &LogCatalogEntry) -> Self {
        Self {
            kind: DiagnosticArtifactKind::Log,
            relative_path: entry.relative_path().to_owned(),
            size_bytes: entry.size_bytes(),
            file_sha256: entry.file_sha256().to_owned(),
        }
    }

    /// 返回来源业务类型。
    pub const fn kind(&self) -> DiagnosticArtifactKind {
        self.kind
    }

    /// 返回工具根目录内的受控相对路径。
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// 返回目录读取时确认的文件大小。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回目录读取时确认的文件摘要。
    pub fn file_sha256(&self) -> &str {
        &self.file_sha256
    }
}

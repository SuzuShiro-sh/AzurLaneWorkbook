//! 一次数据工作簿读取持有的受限字节、源摘要和包索引。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use suzushiro_content_digest::sha256_bytes;

use super::WorkbookProbeError;
use super::editor::{reject_external_content, reject_macro_content};
use super::package::{MAX_RAW_PACKAGE_BYTES, PackageSnapshot, read_bounded_workbook_bytes};

/// 同一份不可变字节上的包索引和源摘要。
/// 备份、设备执行和最终发布必须重新读取磁盘摘要；摘要一致时才能复用这里的索引。
pub(in crate::adapters::workbook) struct WorkbookDocument {
    bytes: Vec<u8>,
    source_package_sha256: String,
    package: PackageSnapshot,
}

/// 一次操作里最近读取的数据工作簿。不是按路径长期缓存。
#[derive(Clone, Default)]
pub(crate) struct WorkbookDocuments {
    opened: Arc<RefCell<Option<OpenedWorkbook>>>,
}

struct OpenedWorkbook {
    path: PathBuf,
    document: WorkbookDocument,
}

impl WorkbookDocument {
    /// 一次读入原始字节，建立包索引，并拒绝外链和宏。
    pub(in crate::adapters::workbook) fn read(
        path: &Path,
        description: &'static str,
    ) -> Result<Self, WorkbookProbeError> {
        let bytes = read_bounded_workbook_bytes(path, MAX_RAW_PACKAGE_BYTES, description)?;
        let source_package_sha256 = sha256_bytes(&bytes);
        let package = PackageSnapshot::from_bytes(&bytes, path)?;
        reject_external_content(&package)?;
        reject_macro_content(&package)?;
        Ok(Self {
            bytes,
            source_package_sha256,
            package,
        })
    }

    pub(in crate::adapters::workbook) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(in crate::adapters::workbook) fn source_package_sha256(&self) -> &str {
        &self.source_package_sha256
    }

    pub(in crate::adapters::workbook) fn package(&self) -> &PackageSnapshot {
        &self.package
    }
}

/// 只读取磁盘上的源摘要，不建立包索引。
pub(in crate::adapters::workbook) fn source_digest(
    path: &Path,
    description: &'static str,
) -> Result<String, WorkbookProbeError> {
    let bytes = read_bounded_workbook_bytes(path, MAX_RAW_PACKAGE_BYTES, description)?;
    Ok(sha256_bytes(&bytes))
}

impl WorkbookDocuments {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn holds_document(&self) -> bool {
        self.opened.borrow().is_some()
    }

    /// 预检已经核对源身份后放开这份索引，避免写回再读入时同时持有两份源包。
    pub(in crate::adapters::workbook) fn release(&self) {
        *self.opened.borrow_mut() = None;
    }

    pub(in crate::adapters::workbook) fn remember(
        &self,
        path: PathBuf,
        document: WorkbookDocument,
    ) {
        *self.opened.borrow_mut() = Some(OpenedWorkbook { path, document });
    }

    /// 磁盘摘要与本次持有的文档一致时复用包索引，否则重新读取。
    pub(in crate::adapters::workbook) fn with_current_index<T>(
        &self,
        path: &Path,
        digest: &str,
        use_document: impl FnOnce(&WorkbookDocument) -> Result<T, WorkbookProbeError>,
    ) -> Result<T, WorkbookProbeError> {
        {
            let opened = self.opened.borrow();
            if let Some(opened) = opened.as_ref()
                && opened.path == path
                && opened.document.source_package_sha256 == digest
            {
                return use_document(&opened.document);
            }
        }
        let document = WorkbookDocument::read(path, "写回预检工作簿")?;
        if document.source_package_sha256 != digest {
            return Err(WorkbookProbeError::SourceChanged {
                path: path.to_path_buf(),
                expected: digest.to_owned(),
                actual: document.source_package_sha256,
            });
        }
        let value = use_document(&document)?;
        self.remember(path.to_path_buf(), document);
        Ok(value)
    }
}

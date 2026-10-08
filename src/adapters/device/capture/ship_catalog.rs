//! 将完整舰船静态目录原子写入发布目录外，并只公开可核验的脱敏身份。

use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use super::super::reading::ship_catalog::{ShipCatalogDocument, ShipCatalogReadResult};
use super::super::session::SessionId;
use super::full_state::{FullStateCaptureError, open_external_capture_root};
use crate::adapters::json_artifact::{JsonArtifactError, PublishedJson, write_new_pretty_json};
use crate::adapters::numbered_files::NumberedDirectory;
use crate::adapters::tool_root::ToolRoot;

const SHIP_CATALOG_CAPTURE_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_SHIP_CATALOG_CAPTURE_BYTES: u64 = 1024 * 1024 * 1024;
const CAPTURE_DIRECTORY: &str = "ship-catalog-captures";
const DIGEST_ALGORITHM: &str = "sha256";
const DIGEST_ENCODING: &str = "sorted_keys_compact_json";

/// 一次固定白名单目录捕获的外部受控根和运行态会话身份。
pub(crate) struct ShipCatalogCaptureRequest {
    root: ToolRoot,
    session_id: SessionId,
}

impl ShipCatalogCaptureRequest {
    /// 打开外部受控根，并拒绝把原始静态配置写入工具发布目录。
    pub(crate) fn new(
        tool_root: &Path,
        capture_root: &Path,
        session_id: SessionId,
    ) -> Result<Self, ShipCatalogCaptureError> {
        Ok(Self {
            root: open_external_capture_root(tool_root, capture_root)?,
            session_id,
        })
    }
}

/// 外部原始目录文件的路径、字节摘要和内容身份，不复制任何配置正文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShipCatalogCaptureEvidence {
    pub schema_version: u32,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub file_sha256: String,
    pub session_id: SessionId,
    pub module_sha256: String,
    pub content_sha256: String,
    pub table_count: u32,
    pub record_count: u32,
}

/// 静态目录捕获的边界、计数或原子发布未形成完整证据。
#[derive(Debug, Error)]
pub(crate) enum ShipCatalogCaptureError {
    #[error(transparent)]
    ExternalRoot(#[from] FullStateCaptureError),
    #[error("舰船静态目录捕获字段 {field} 的数量 {actual} 超出 u32 范围")]
    CountOverflow { field: &'static str, actual: usize },
    #[error("分配证据文件编号失败: {0}")]
    Sequence(#[from] std::io::Error),
    #[error(transparent)]
    Artifact(#[from] JsonArtifactError),
}

#[derive(Serialize)]
struct ShipCatalogCaptureDocument<'a> {
    capture_schema_version: u32,
    session_id: SessionId,
    module_sha256: &'a str,
    digest_algorithm: &'static str,
    digest_encoding: &'static str,
    content_sha256: &'a str,
    table_count: u32,
    record_count: u32,
    catalog: ShipCatalogDocument<'a>,
}

/// 流式编码完整目录，并在同步成功后排他发布外部证据文件。
pub(crate) fn write_ship_catalog_capture(
    request: &ShipCatalogCaptureRequest,
    result: &ShipCatalogReadResult,
) -> Result<ShipCatalogCaptureEvidence, ShipCatalogCaptureError> {
    let table_count = capture_count("table_count", result.tables().len())?;
    let record_count = capture_count("record_count", result.record_count())?;
    let document = ShipCatalogCaptureDocument {
        capture_schema_version: SHIP_CATALOG_CAPTURE_SCHEMA_VERSION,
        session_id: request.session_id,
        module_sha256: result.module_sha256(),
        digest_algorithm: DIGEST_ALGORITHM,
        digest_encoding: DIGEST_ENCODING,
        content_sha256: result.content_sha256(),
        table_count,
        record_count,
        catalog: result.document(),
    };
    let directory = NumberedDirectory::open(&request.root, Path::new(CAPTURE_DIRECTORY))?;
    let target_relative = directory.next_path("ships", "json", true)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let published: PublishedJson = write_new_pretty_json(
        &request.root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_SHIP_CATALOG_CAPTURE_BYTES,
        &document,
    )?;
    Ok(ShipCatalogCaptureEvidence {
        schema_version: SHIP_CATALOG_CAPTURE_SCHEMA_VERSION,
        path: published.path().to_path_buf(),
        size_bytes: published.size_bytes(),
        file_sha256: published.sha256().to_owned(),
        session_id: request.session_id,
        module_sha256: result.module_sha256().to_owned(),
        content_sha256: result.content_sha256().to_owned(),
        table_count,
        record_count,
    })
}

fn capture_count(field: &'static str, actual: usize) -> Result<u32, ShipCatalogCaptureError> {
    u32::try_from(actual).map_err(|_| ShipCatalogCaptureError::CountOverflow { field, actual })
}

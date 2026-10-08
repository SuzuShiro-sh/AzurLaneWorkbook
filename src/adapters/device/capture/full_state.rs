//! 将一次完整读取的原始 DTO 以有界、原子方式写入发布目录外的黄金证据文件。

use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use super::super::reading::equipment::{EquipmentRawDocument, EquipmentReadResult};
use super::super::reading::ship_catalog::{ShipCatalogDocument, ShipCatalogReadResult};
use super::super::runtime::{
    CapabilitiesResult, RuntimeSkillEffectDetail, SnapshotOwnedStateResult,
    SnapshotShipDetailsResult,
};
use super::super::session::SessionId;
use crate::adapters::json_artifact::{JsonArtifactError, PublishedJson, write_new_pretty_json};
use crate::adapters::numbered_files::NumberedDirectory;
use crate::adapters::tool_root::{ToolRoot, ToolRootError};

pub(crate) const FULL_STATE_CAPTURE_SCHEMA_VERSION: u32 = 2;
/// 单份完整状态捕获写入和后续证据重读共用的最大字节数。
pub(crate) const MAXIMUM_FULL_STATE_CAPTURE_BYTES: u64 = 512 * 1024 * 1024;
const CAPTURE_DIRECTORY: &str = "full-state-captures";

/// 一次完整读取对应的外部证据目录、会话和读取序号。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FullStateCaptureRequest {
    root: ToolRoot,
    session_id: SessionId,
    read_index: u64,
}

impl FullStateCaptureRequest {
    /// 重新验证证据目录，并拒绝把私人原始正文写入发布目录。
    pub(crate) fn new(
        tool_root: &Path,
        capture_root: &Path,
        session_id: SessionId,
        read_index: u64,
    ) -> Result<Self, FullStateCaptureError> {
        if read_index == 0 {
            return Err(FullStateCaptureError::InvalidReadIndex);
        }
        let root: ToolRoot = open_external_capture_root(tool_root, capture_root)?;
        Ok(Self {
            root,
            session_id,
            read_index,
        })
    }
}

/// 原始 DTO 已安全发布后的脱敏定位信息。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FullStateCaptureEvidence {
    schema_version: u32,
    path: PathBuf,
    size_bytes: u64,
    sha256: String,
    session_id: SessionId,
    read_index: u64,
    game_state_content_sha256: String,
}

impl FullStateCaptureEvidence {
    /// 返回发布目录外的原始证据文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 返回完整证据文件的字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回完整证据文件的 SHA-256。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// 返回原始证据结构版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回产生该证据的运行态会话标识。
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// 返回会话内从一开始计数的读取序号。
    pub const fn read_index(&self) -> u64 {
        self.read_index
    }

    /// 返回该次原始读取映射出的完整游戏状态摘要。
    pub fn game_state_content_sha256(&self) -> &str {
        &self.game_state_content_sha256
    }
}

/// 完整原始读取证据的所有借用输入。
pub(crate) struct FullStateCaptureInput<'a> {
    pub(crate) module_sha256: &'a str,
    pub(crate) game_state_content_sha256: &'a str,
    pub(crate) capabilities_before: &'a CapabilitiesResult,
    pub(crate) owned_state_before: &'a SnapshotOwnedStateResult,
    pub(crate) ship_details: &'a SnapshotShipDetailsResult,
    pub(crate) owned_state_after: &'a SnapshotOwnedStateResult,
    pub(crate) equipment: &'a EquipmentReadResult,
    pub(crate) ship_catalog: &'a ShipCatalogReadResult,
    pub(crate) ship_skill_effects: &'a [RuntimeSkillEffectDetail],
    pub(crate) capabilities_after: &'a CapabilitiesResult,
}

/// 原始证据路径、序列化或原子发布未完整完成。
#[derive(Debug, Error)]
pub(crate) enum FullStateCaptureError {
    #[error("完整状态证据目录必须位于发布目录之外: capture={capture}, tool={tool}")]
    InsideToolRoot { capture: PathBuf, tool: PathBuf },
    #[error("完整状态证据读取序号必须大于零")]
    InvalidReadIndex,
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
    #[error("分配证据文件编号失败: {0}")]
    Sequence(#[from] std::io::Error),
    #[error(transparent)]
    Artifact(#[from] JsonArtifactError),
}

/// 只校验证据目录边界并返回规范路径，供配置层在启动设备资源前拒绝错误配置。
pub(crate) fn validate_external_capture_root(
    tool_root: &Path,
    capture_root: &Path,
) -> Result<PathBuf, FullStateCaptureError> {
    open_external_capture_root(tool_root, capture_root)
        .map(|root: ToolRoot| root.as_path().to_path_buf())
}

/// 流式编码完整原始 DTO；只有映射成功后的读取才会调用本函数。
pub(crate) fn write_full_state_capture(
    request: &FullStateCaptureRequest,
    input: FullStateCaptureInput<'_>,
) -> Result<FullStateCaptureEvidence, FullStateCaptureError> {
    let document = FullStateCaptureDocument {
        capture_schema_version: FULL_STATE_CAPTURE_SCHEMA_VERSION,
        session_id: request.session_id,
        read_index: request.read_index,
        module_sha256: input.module_sha256,
        game_state_content_sha256: input.game_state_content_sha256,
        equipment_catalog_content_sha256: input.equipment.catalog().source().content_sha256(),
        equipment_raw_content_sha256: input.equipment.raw_content_sha256(),
        ship_catalog_content_sha256: input.ship_catalog.content_sha256(),
        capabilities_before: input.capabilities_before,
        owned_state_before: input.owned_state_before,
        ship_details: input.ship_details,
        owned_state_after: input.owned_state_after,
        equipment_raw: input.equipment.raw_records().document(),
        ship_catalog: input.ship_catalog.document(),
        ship_skill_effects: input.ship_skill_effects,
        capabilities_after: input.capabilities_after,
    };
    let directory = NumberedDirectory::open(&request.root, Path::new(CAPTURE_DIRECTORY))?;
    let target_relative = directory.next_path("state", "json", true)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let published: PublishedJson = write_new_pretty_json(
        &request.root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_FULL_STATE_CAPTURE_BYTES,
        &document,
    )?;
    let size_bytes: u64 = published.size_bytes();
    let sha256: String = published.sha256().to_owned();
    let path: PathBuf = published.into_path();
    Ok(FullStateCaptureEvidence {
        schema_version: FULL_STATE_CAPTURE_SCHEMA_VERSION,
        path,
        size_bytes,
        sha256,
        session_id: request.session_id,
        read_index: request.read_index,
        game_state_content_sha256: input.game_state_content_sha256.to_owned(),
    })
}

pub(super) fn open_external_capture_root(
    tool_root: &Path,
    capture_root: &Path,
) -> Result<ToolRoot, FullStateCaptureError> {
    let tool: ToolRoot = ToolRoot::open(tool_root)?;
    let capture: ToolRoot = ToolRoot::open(capture_root)?;
    if capture.as_path().starts_with(tool.as_path()) {
        return Err(FullStateCaptureError::InsideToolRoot {
            capture: capture.as_path().to_path_buf(),
            tool: tool.as_path().to_path_buf(),
        });
    }
    Ok(capture)
}

/// 捕获文件保留原始协议结构和两侧能力报告，便于独立工具重新构造期望值。
#[derive(Serialize)]
struct FullStateCaptureDocument<'a> {
    capture_schema_version: u32,
    session_id: SessionId,
    read_index: u64,
    module_sha256: &'a str,
    game_state_content_sha256: &'a str,
    equipment_catalog_content_sha256: &'a str,
    equipment_raw_content_sha256: &'a str,
    ship_catalog_content_sha256: &'a str,
    capabilities_before: &'a CapabilitiesResult,
    owned_state_before: &'a SnapshotOwnedStateResult,
    ship_details: &'a SnapshotShipDetailsResult,
    owned_state_after: &'a SnapshotOwnedStateResult,
    equipment_raw: EquipmentRawDocument<'a>,
    ship_catalog: ShipCatalogDocument<'a>,
    ship_skill_effects: &'a [RuntimeSkillEffectDetail],
    capabilities_after: &'a CapabilitiesResult,
}

//! 保存包含装备原始读取结果与规范化目录的受控维护样本。

use std::path::Path;

use serde::Serialize;
use thiserror::Error;

use super::super::mapping::equipment::EquipmentCatalogDocument;
use super::super::reading::equipment::{EquipmentRawDocument, EquipmentReadResult};
use super::super::session::SessionId;
use crate::adapters::json_artifact::{JsonArtifactError, PublishedJson, write_new_pretty_json};
use crate::adapters::numbered_files::NumberedDirectory;
use crate::adapters::tool_root::ToolRoot;

const EQUIPMENT_SAMPLE_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_EQUIPMENT_SAMPLE_BYTES: u64 = 256 * 1024 * 1024;
const DIGEST_ALGORITHM: &str = "sha256";
const DIGEST_ENCODING: &str = "sorted_keys_compact_json";

/// 运行态收据公开的装备样本身份，不携带任何原始配置正文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EquipmentSampleEvidence {
    /// 维护样本顶层结构版本。
    pub schema_version: u32,
    /// 使用正斜杠表示的工具根目录相对路径。
    pub relative_path: String,
    /// 完整样本文件的 UTF-8 JSON 字节数。
    pub size_bytes: u64,
    /// 完整样本文件字节的 SHA-256。
    pub file_sha256: String,
    /// 读取静态配置时已经验证的目标模块 SHA-256。
    pub module_sha256: String,
    /// 规范化装备目录的结构版本。
    pub catalog_schema_version: u32,
    /// `normalized` 正文按声明编码计算的 SHA-256。
    pub catalog_content_sha256: String,
    /// 原始装备记录正文的结构版本。
    pub raw_schema_version: u32,
    /// `raw` 正文按声明编码计算的 SHA-256。
    pub raw_content_sha256: String,
    /// 规范化装备族数量。
    pub family_count: u32,
    /// 原始装备配置数量。
    pub config_count: u32,
    /// 原始合成配方数量。
    pub recipe_count: u32,
    /// 四类引用名称的合计数量。
    pub reference_count: u32,
    /// 原始武器详情数量。
    pub weapon_count: u32,
    /// 原始技能详情数量。
    pub skill_count: u32,
}

#[derive(Clone, Copy, Serialize)]
struct EquipmentSampleCounts {
    family_count: u32,
    config_count: u32,
    recipe_count: u32,
    reference_count: u32,
    weapon_count: u32,
    skill_count: u32,
}

/// 装备样本的计数转换或原子发布没有满足固定契约。
#[derive(Debug, Error)]
pub(crate) enum EquipmentSampleError {
    #[error("装备样本字段 {field} 的数量 {actual} 超出 u32 范围")]
    CountOverflow { field: &'static str, actual: usize },
    #[error("分配证据文件编号失败: {0}")]
    Sequence(#[from] std::io::Error),
    #[error(transparent)]
    Artifact(#[from] JsonArtifactError),
}

/// 样本正文显式分离原始适配数据与规范化领域数据，二者摘要均可独立复算。
#[derive(Serialize)]
struct EquipmentSampleDocument<'a> {
    schema_version: u32,
    session_id: SessionId,
    module_sha256: &'a str,
    digest_algorithm: &'static str,
    digest_encoding: &'static str,
    raw_content_sha256: &'a str,
    catalog_content_sha256: &'a str,
    counts: EquipmentSampleCounts,
    raw: EquipmentRawDocument<'a>,
    normalized: EquipmentCatalogDocument<'a>,
}

/// 在工具历史目录排他发布一次完整装备维护样本。
pub(crate) fn write_equipment_sample(
    tool_root: &ToolRoot,
    session_id: SessionId,
    result: &EquipmentReadResult,
) -> Result<EquipmentSampleEvidence, EquipmentSampleError> {
    let catalog = result.catalog();
    let raw_records = result.raw_records();
    let counts = EquipmentSampleCounts {
        family_count: sample_count("family_count", catalog.families().len())?,
        config_count: sample_count("config_count", catalog.config_count())?,
        recipe_count: sample_count("recipe_count", raw_records.recipe_count())?,
        reference_count: sample_count("reference_count", raw_records.reference_count())?,
        weapon_count: sample_count("weapon_count", raw_records.weapon_count())?,
        skill_count: sample_count("skill_count", raw_records.skill_count())?,
    };
    let document = EquipmentSampleDocument {
        schema_version: EQUIPMENT_SAMPLE_SCHEMA_VERSION,
        session_id,
        module_sha256: catalog.source().module_sha256(),
        digest_algorithm: DIGEST_ALGORITHM,
        digest_encoding: DIGEST_ENCODING,
        raw_content_sha256: result.raw_content_sha256(),
        catalog_content_sha256: catalog.source().content_sha256(),
        counts,
        raw: raw_records.document(),
        normalized: EquipmentCatalogDocument::from_catalog(catalog),
    };
    let directory = NumberedDirectory::open(tool_root, Path::new("data/history"))?;
    let target_relative = directory.next_path("equipment", "json", true)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let published: PublishedJson = write_new_pretty_json(
        tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_EQUIPMENT_SAMPLE_BYTES,
        &document,
    )?;

    Ok(EquipmentSampleEvidence {
        schema_version: EQUIPMENT_SAMPLE_SCHEMA_VERSION,
        relative_path: target_relative.to_string_lossy().replace('\\', "/"),
        size_bytes: published.size_bytes(),
        file_sha256: published.sha256().to_owned(),
        module_sha256: catalog.source().module_sha256().to_owned(),
        catalog_schema_version: catalog.schema_version(),
        catalog_content_sha256: catalog.source().content_sha256().to_owned(),
        raw_schema_version: raw_records.schema_version(),
        raw_content_sha256: result.raw_content_sha256().to_owned(),
        family_count: counts.family_count,
        config_count: counts.config_count,
        recipe_count: counts.recipe_count,
        reference_count: counts.reference_count,
        weapon_count: counts.weapon_count,
        skill_count: counts.skill_count,
    })
}

fn sample_count(field: &'static str, actual: usize) -> Result<u32, EquipmentSampleError> {
    u32::try_from(actual).map_err(|_| EquipmentSampleError::CountOverflow { field, actual })
}

//! 编排完整装备配置、配方、名称和详情的只读运行态请求。

use std::collections::BTreeSet;
use std::fmt::{Debug, Display, Formatter};
use std::num::NonZeroUsize;

use serde::Serialize;
use thiserror::Error;

use super::super::mapping::equipment::{
    EquipmentMappingError, build_reference_request_from_pages, map_equipment_catalog,
};
use super::super::runtime::{
    AgentClient, ComposeRecipePageResult, EquipmentComposeRecipe as RuntimeEquipmentComposeRecipe,
    EquipmentConfigPageResult, EquipmentReferenceNameBatchPayload,
    EquipmentReferenceNameBatchResult, EquipmentWeaponBatchResult, MAX_EQUIPMENT_PAGE_SIZE,
    MAX_EQUIPMENT_WEAPON_BATCH_SIZE, MAX_SKILL_EFFECT_BATCH_SIZE, RuntimeClientError,
    RuntimeEquipmentConfig, RuntimeEquipmentWeaponDetail, RuntimeProtocolError,
    RuntimeSkillEffectDetail, SkillEffectBatchResult, SkillEffectQuery,
};
use super::collections::{PageState, read_complete_batches, read_restartable_pages};
use crate::domain::EquipmentCatalog;
use suzushiro_content_digest::sha256_sorted_json;

const EQUIPMENT_RAW_RECORDS_SCHEMA_VERSION: u32 = 2;
// 游戏在静态目录懒加载或切换页面的短窗口内可能返回已校验但不完整的页。
// 发现这类响应时重启整套目录读取，避免把不同快照的页面拼在一起。
const MAX_INCOMPLETE_PAGE_RETRIES: u8 = 2;

/// 完整装备目录及同一读取时刻的原始内容身份。
#[derive(Clone, PartialEq)]
pub struct EquipmentReadResult {
    catalog: EquipmentCatalog,
    raw_records: EquipmentRawRecords,
    weapons_read: bool,
    skills_read: bool,
}

impl Debug for EquipmentReadResult {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EquipmentReadResult")
            .field("module_sha256", &self.catalog.source().module_sha256())
            .field("catalog_schema_version", &self.catalog.schema_version())
            .field(
                "catalog_content_sha256",
                &self.catalog.source().content_sha256(),
            )
            .field("raw_content_sha256", &self.raw_content_sha256())
            .field("family_count", &self.catalog.families().len())
            .field("config_count", &self.catalog.config_count())
            .field("recipe_count", &self.raw_records.recipe_count())
            .field("reference_count", &self.raw_records.reference_count())
            .field("weapon_count", &self.raw_records.weapon_count())
            .field("skill_count", &self.raw_records.skill_count())
            .finish()
    }
}

impl EquipmentReadResult {
    #[cfg(test)]
    pub(crate) fn new(catalog: EquipmentCatalog, raw_records: EquipmentRawRecords) -> Self {
        Self {
            catalog,
            raw_records,
            weapons_read: true,
            skills_read: true,
        }
    }

    pub(crate) const fn weapons_read(&self) -> bool {
        self.weapons_read
    }
    pub(crate) const fn skills_read(&self) -> bool {
        self.skills_read
    }

    /// 返回已经通过分页、引用和强化链校验的核心装备目录。
    pub const fn catalog(&self) -> &EquipmentCatalog {
        &self.catalog
    }

    /// 返回供跟版比较使用的完整原始配置、引用和详情内容摘要。
    pub fn raw_content_sha256(&self) -> &str {
        self.raw_records().content_sha256()
    }

    /// 原始 DTO 只供 crate 内后续详情映射使用，不构成库的公共契约。
    pub(crate) const fn raw_records(&self) -> &EquipmentRawRecords {
        &self.raw_records
    }
}

/// 保留领域层未接收的运行态记录，动态字段只在适配层内流转。
#[derive(Clone, PartialEq)]
pub(crate) struct EquipmentRawRecords {
    content_sha256: String,
    configs: Vec<RuntimeEquipmentConfig>,
    recipes: Vec<RuntimeEquipmentComposeRecipe>,
    reference_names: EquipmentReferenceNameBatchResult,
    weapons: Vec<RuntimeEquipmentWeaponDetail>,
    skills: Vec<RuntimeSkillEffectDetail>,
}

impl EquipmentRawRecords {
    pub(crate) fn new(
        configs: Vec<RuntimeEquipmentConfig>,
        recipes: Vec<RuntimeEquipmentComposeRecipe>,
        reference_names: EquipmentReferenceNameBatchResult,
        weapons: Vec<RuntimeEquipmentWeaponDetail>,
        skills: Vec<RuntimeSkillEffectDetail>,
    ) -> Result<Self, serde_json::Error> {
        let digest_input = EquipmentRawDocument {
            schema_version: EQUIPMENT_RAW_RECORDS_SCHEMA_VERSION,
            configs: &configs,
            recipes: &recipes,
            reference_names: &reference_names,
            weapons: &weapons,
            skills: &skills,
        };
        Ok(Self {
            content_sha256: sha256_sorted_json(&digest_input)?,
            configs,
            recipes,
            reference_names,
            weapons,
            skills,
        })
    }

    /// 返回原始记录规范序列化后的稳定内容摘要。
    const fn content_sha256(&self) -> &str {
        self.content_sha256.as_str()
    }

    /// 返回与原始内容摘要使用完全相同的稳定序列化投影。
    pub(crate) fn document(&self) -> EquipmentRawDocument<'_> {
        EquipmentRawDocument {
            schema_version: EQUIPMENT_RAW_RECORDS_SCHEMA_VERSION,
            configs: &self.configs,
            recipes: &self.recipes,
            reference_names: &self.reference_names,
            weapons: &self.weapons,
            skills: &self.skills,
        }
    }

    pub(crate) const fn schema_version(&self) -> u32 {
        EQUIPMENT_RAW_RECORDS_SCHEMA_VERSION
    }

    pub(crate) fn configs(&self) -> &[RuntimeEquipmentConfig] {
        &self.configs
    }

    pub(crate) fn recipes(&self) -> &[RuntimeEquipmentComposeRecipe] {
        &self.recipes
    }

    pub(crate) const fn reference_names(&self) -> &EquipmentReferenceNameBatchResult {
        &self.reference_names
    }

    pub(crate) fn weapons(&self) -> &[RuntimeEquipmentWeaponDetail] {
        &self.weapons
    }

    pub(crate) fn skills(&self) -> &[RuntimeSkillEffectDetail] {
        &self.skills
    }

    pub(crate) fn recipe_count(&self) -> usize {
        self.recipes.len()
    }

    pub(crate) fn reference_count(&self) -> usize {
        self.reference_names.count as usize
    }

    pub(crate) fn weapon_count(&self) -> usize {
        self.weapons.len()
    }

    pub(crate) fn skill_count(&self) -> usize {
        self.skills.len()
    }
}

/// 原始内容排除分页和分片，只覆盖已验证且确定排序的记录正文。
#[derive(Serialize)]
pub(crate) struct EquipmentRawDocument<'a> {
    schema_version: u32,
    configs: &'a [RuntimeEquipmentConfig],
    recipes: &'a [RuntimeEquipmentComposeRecipe],
    reference_names: &'a EquipmentReferenceNameBatchResult,
    weapons: &'a [RuntimeEquipmentWeaponDetail],
    skills: &'a [RuntimeSkillEffectDetail],
}

#[derive(Clone, Copy)]
struct EquipmentReadLimits {
    page_size: u32,
    weapon_batch_size: usize,
    skill_batch_size: usize,
}

impl EquipmentReadLimits {
    const PRODUCTION: Self = Self {
        page_size: MAX_EQUIPMENT_PAGE_SIZE,
        weapon_batch_size: MAX_EQUIPMENT_WEAPON_BATCH_SIZE,
        skill_batch_size: MAX_SKILL_EFFECT_BATCH_SIZE,
    };
}

/// 复用已验证的核心目录，仅补齐本次请求而缓存尚未覆盖的详情批次。
pub(crate) fn read_equipment_catalog_with_scope(
    client: &mut AgentClient,
    timeout_ms: u32,
    expected_module_sha256: &str,
    scope: crate::domain::GameReadScope,
    cached: Option<EquipmentReadResult>,
) -> Result<EquipmentReadResult, EquipmentReadError> {
    read_equipment_catalog_scoped(
        client,
        timeout_ms,
        expected_module_sha256,
        EquipmentReadLimits::PRODUCTION,
        scope,
        cached,
    )
}

fn read_equipment_catalog_scoped<R: EquipmentRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    expected_module_sha256: &str,
    limits: EquipmentReadLimits,
    scope: crate::domain::GameReadScope,
    cached: Option<EquipmentReadResult>,
) -> Result<EquipmentReadResult, EquipmentReadError> {
    debug_assert!(limits.page_size > 0);
    debug_assert!(limits.weapon_batch_size > 0);
    debug_assert!(limits.skill_batch_size > 0);
    let mut result = if let Some(cached) = cached {
        if cached.catalog().source().module_sha256() != expected_module_sha256 {
            return Err(RuntimeProtocolError::new(
                "equipment_cache_module_mismatch",
                "缓存装备目录与当前模块身份不一致",
            )
            .into());
        }
        cached
    } else {
        let config_pages = read_config_pages(
            runtime,
            timeout_ms,
            limits.page_size,
            expected_module_sha256,
        )?;
        let recipe_pages = read_recipe_pages(
            runtime,
            timeout_ms,
            limits.page_size,
            expected_module_sha256,
        )?;
        let reference_request = build_reference_request_from_pages(
            &config_pages,
            limits.page_size,
            expected_module_sha256,
        )?;
        let reference_names = runtime.snapshot_equipment_reference_names(
            timeout_ms,
            &reference_request,
            expected_module_sha256,
        )?;
        let catalog = map_equipment_catalog(
            &config_pages,
            limits.page_size,
            &recipe_pages,
            limits.page_size,
            &reference_names,
            expected_module_sha256,
        )?;

        let configs = config_pages
            .into_iter()
            .flat_map(|page| page.configs)
            .collect();
        let recipes = recipe_pages
            .into_iter()
            .flat_map(|page| page.recipes)
            .collect();
        let raw_records =
            EquipmentRawRecords::new(configs, recipes, reference_names, Vec::new(), Vec::new())
                .map_err(EquipmentReadError::EncodeRawRecords)?;
        EquipmentReadResult {
            catalog,
            raw_records,
            weapons_read: false,
            skills_read: false,
        }
    };
    let needs_weapons = scope.equipment_weapons() && !result.weapons_read;
    let needs_skills = scope.equipment_skill_effects() && !result.skills_read;
    if !needs_weapons && !needs_skills {
        return Ok(result);
    }
    let (weapon_ids, skill_queries) = collect_detail_keys(result.catalog())?;
    if needs_weapons {
        result.raw_records.weapons = read_weapon_details(
            runtime,
            timeout_ms,
            &weapon_ids,
            limits.weapon_batch_size,
            expected_module_sha256,
        )?;
        result.weapons_read = true;
    }
    if needs_skills {
        result.raw_records.skills = read_skill_details(
            runtime,
            timeout_ms,
            &skill_queries,
            limits.skill_batch_size,
            expected_module_sha256,
        )?;
        result.skills_read = true;
    }
    let raw = result.raw_records;
    result.raw_records = EquipmentRawRecords::new(
        raw.configs,
        raw.recipes,
        raw.reference_names,
        raw.weapons,
        raw.skills,
    )
    .map_err(EquipmentReadError::EncodeRawRecords)?;
    Ok(result)
}

fn read_config_pages<R: EquipmentRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    page_size: u32,
    expected_module_sha256: &str,
) -> Result<Vec<EquipmentConfigPageResult>, EquipmentReadError> {
    read_restartable_pages(
        0,
        MAX_INCOMPLETE_PAGE_RETRIES,
        |start_index| {
            let page = runtime.snapshot_equipment_configs(
                timeout_ms,
                start_index,
                page_size,
                expected_module_sha256,
            )?;
            page.validate(start_index, page_size, expected_module_sha256)?;
            Ok(page)
        },
        |page| {
            PageState::new(
                page.next_index,
                !page.complete || !page.read_errors.is_empty(),
            )
        },
    )
}

fn read_recipe_pages<R: EquipmentRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    page_size: u32,
    expected_module_sha256: &str,
) -> Result<Vec<ComposeRecipePageResult>, EquipmentReadError> {
    read_restartable_pages(
        0,
        MAX_INCOMPLETE_PAGE_RETRIES,
        |start_index| {
            let page = runtime.snapshot_compose_recipes(
                timeout_ms,
                start_index,
                page_size,
                expected_module_sha256,
            )?;
            page.validate(start_index, page_size, expected_module_sha256)?;
            Ok(page)
        },
        |page| {
            PageState::new(
                page.next_index,
                !page.complete || !page.read_errors.is_empty(),
            )
        },
    )
}

fn collect_detail_keys(
    catalog: &EquipmentCatalog,
) -> Result<(Vec<u64>, Vec<SkillEffectQuery>), RuntimeProtocolError> {
    let mut weapon_ids = BTreeSet::new();
    let mut skill_keys = BTreeSet::new();
    for config in catalog
        .families()
        .iter()
        .flat_map(|family| family.configs())
    {
        weapon_ids.extend(config.weapon_ids().iter().copied());
        skill_keys.extend(
            config
                .skill_references()
                .iter()
                .map(|reference| (reference.skill_id(), reference.level())),
        );
    }
    let skills = skill_keys
        .into_iter()
        .map(|(skill_id, level)| SkillEffectQuery::new(skill_id, level))
        .collect::<Result<_, _>>()?;
    Ok((weapon_ids.into_iter().collect(), skills))
}

fn read_weapon_details<R: EquipmentRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    weapon_ids: &[u64],
    batch_size: usize,
    expected_module_sha256: &str,
) -> Result<Vec<RuntimeEquipmentWeaponDetail>, EquipmentReadError> {
    read_complete_batches(
        weapon_ids,
        nonzero_batch_size(batch_size),
        |batch_ids| {
            runtime
                .snapshot_equipment_weapons(timeout_ms, batch_ids, expected_module_sha256)
                .map_err(EquipmentReadError::from)
        },
        |batch, batch_ids| {
            batch
                .validate(batch_ids, expected_module_sha256)
                .map_err(EquipmentReadError::from)
        },
        |batch| batch.complete,
        |batch| IncompleteEquipmentDetails::from_weapons(batch).into(),
        |batch| batch.weapons,
    )
}

fn read_skill_details<R: EquipmentRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    skill_queries: &[SkillEffectQuery],
    batch_size: usize,
    expected_module_sha256: &str,
) -> Result<Vec<RuntimeSkillEffectDetail>, EquipmentReadError> {
    read_complete_batches(
        skill_queries,
        nonzero_batch_size(batch_size),
        |batch_queries| {
            runtime
                .snapshot_skill_effects(timeout_ms, batch_queries, expected_module_sha256)
                .map_err(EquipmentReadError::from)
        },
        |batch, batch_queries| {
            batch
                .validate(batch_queries, expected_module_sha256)
                .map_err(EquipmentReadError::from)
        },
        |batch| batch.complete,
        |batch| IncompleteEquipmentDetails::from_skills(batch).into(),
        |batch| batch.skills,
    )
}

fn nonzero_batch_size(batch_size: usize) -> NonZeroUsize {
    NonZeroUsize::new(batch_size).expect("装备详情批大小必须大于零")
}

trait EquipmentRuntime {
    fn snapshot_equipment_configs(
        &mut self,
        timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<EquipmentConfigPageResult, RuntimeClientError>;

    fn snapshot_compose_recipes(
        &mut self,
        timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<ComposeRecipePageResult, RuntimeClientError>;

    fn snapshot_equipment_reference_names(
        &mut self,
        timeout_ms: u32,
        request: &EquipmentReferenceNameBatchPayload,
        expected_module_sha256: &str,
    ) -> Result<EquipmentReferenceNameBatchResult, RuntimeClientError>;

    fn snapshot_equipment_weapons(
        &mut self,
        timeout_ms: u32,
        weapon_ids: &[u64],
        expected_module_sha256: &str,
    ) -> Result<EquipmentWeaponBatchResult, RuntimeClientError>;

    fn snapshot_skill_effects(
        &mut self,
        timeout_ms: u32,
        skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError>;
}

impl EquipmentRuntime for AgentClient {
    fn snapshot_equipment_configs(
        &mut self,
        timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<EquipmentConfigPageResult, RuntimeClientError> {
        AgentClient::snapshot_equipment_configs(
            self,
            timeout_ms,
            start_index,
            page_size,
            expected_module_sha256,
        )
    }

    fn snapshot_compose_recipes(
        &mut self,
        timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<ComposeRecipePageResult, RuntimeClientError> {
        AgentClient::snapshot_compose_recipes(
            self,
            timeout_ms,
            start_index,
            page_size,
            expected_module_sha256,
        )
    }

    fn snapshot_equipment_reference_names(
        &mut self,
        timeout_ms: u32,
        request: &EquipmentReferenceNameBatchPayload,
        expected_module_sha256: &str,
    ) -> Result<EquipmentReferenceNameBatchResult, RuntimeClientError> {
        AgentClient::snapshot_equipment_reference_names(
            self,
            timeout_ms,
            &request.equipment_type_ids,
            &request.nation_ids,
            &request.ship_type_ids,
            &request.attribute_keys,
            expected_module_sha256,
        )
    }

    fn snapshot_equipment_weapons(
        &mut self,
        timeout_ms: u32,
        weapon_ids: &[u64],
        expected_module_sha256: &str,
    ) -> Result<EquipmentWeaponBatchResult, RuntimeClientError> {
        AgentClient::snapshot_equipment_weapons(
            self,
            timeout_ms,
            weapon_ids,
            expected_module_sha256,
        )
    }

    fn snapshot_skill_effects(
        &mut self,
        timeout_ms: u32,
        skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError> {
        AgentClient::snapshot_skill_effects(self, timeout_ms, skills, expected_module_sha256)
    }
}

/// 一条未完整读取的详情键及其全部来源诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentDetailFailure {
    key: String,
    diagnostics: Vec<String>,
}

impl EquipmentDetailFailure {
    /// 返回武器 ID 或 `技能 ID:等级` 键。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 返回带来源名称的逐条诊断。
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

/// 同一 RPC 批次中所有未完整读取的装备详情。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncompleteEquipmentDetails {
    kind: &'static str,
    failures: Vec<EquipmentDetailFailure>,
}

impl IncompleteEquipmentDetails {
    fn from_weapons(batch: &EquipmentWeaponBatchResult) -> Self {
        let failures = batch
            .weapons
            .iter()
            .filter(|detail| !detail.complete)
            .map(|detail| EquipmentDetailFailure {
                key: detail.weapon_id.to_string(),
                diagnostics: detail.read_errors.clone(),
            })
            .collect();
        Self::new("装备武器", failures)
    }

    fn from_skills(batch: &SkillEffectBatchResult) -> Self {
        let failures = batch
            .skills
            .iter()
            .filter(|detail| !detail.complete)
            .map(|detail| EquipmentDetailFailure {
                key: format!("{}:{}", detail.skill_id, detail.level),
                diagnostics: skill_diagnostics(detail),
            })
            .collect();
        Self::new("装备技能", failures)
    }

    fn new(kind: &'static str, failures: Vec<EquipmentDetailFailure>) -> Self {
        debug_assert!(!failures.is_empty());
        debug_assert!(
            failures
                .iter()
                .all(|failure| !failure.diagnostics.is_empty())
        );
        Self { kind, failures }
    }

    /// 返回详情类别。
    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    /// 返回按请求顺序排列的所有失败详情。
    pub fn failures(&self) -> &[EquipmentDetailFailure] {
        &self.failures
    }
}

impl Display for IncompleteEquipmentDetails {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}详情批次不完整，共 {} 条失败记录",
            self.kind,
            self.failures.len()
        )
    }
}

impl std::error::Error for IncompleteEquipmentDetails {}

fn skill_diagnostics(detail: &RuntimeSkillEffectDetail) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if !detail.display.complete {
        append_skill_source_diagnostics(&mut diagnostics, "展示配置", &detail.display);
    }
    let has_battle_source = detail.battle_skill.available || detail.battle_buff.available;
    for (source_name, source) in [
        ("战斗技能", &detail.battle_skill),
        ("战斗 Buff", &detail.battle_buff),
    ] {
        if (!has_battle_source && !source.available) || (source.available && !source.complete) {
            append_skill_source_diagnostics(&mut diagnostics, source_name, source);
        }
    }
    diagnostics
}

fn append_skill_source_diagnostics(
    diagnostics: &mut Vec<String>,
    source_name: &str,
    source: &super::super::runtime::RuntimeSkillEffectSource,
) {
    if let Some(error) = &source.error {
        diagnostics.push(format!("{source_name}：{error}"));
    }
    diagnostics.extend(
        source
            .read_errors
            .iter()
            .map(|error| format!("{source_name}：{error}")),
    );
}

/// 装备只读编排在运行态、映射和详情完整性边界上的失败分类。
#[derive(Debug, Error)]
pub enum EquipmentReadError {
    /// 运行态连接、RPC 或线上协议失败。
    #[error(transparent)]
    Runtime(#[from] RuntimeClientError),
    /// 原始配置无法映射为严格核心目录。
    #[error(transparent)]
    Mapping(#[from] EquipmentMappingError),
    /// 领域引用不能转换为运行态技能请求键。
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
    /// 原始记录无法规范序列化时不发布没有内容身份的结果。
    #[error("序列化装备原始记录摘要失败：{0}")]
    EncodeRawRecords(#[source] serde_json::Error),
    /// 任一详情批次携带读取错误时不发布部分目录。
    #[error(transparent)]
    IncompleteDetails(#[from] IncompleteEquipmentDetails),
}

#[cfg(test)]
mod tests;

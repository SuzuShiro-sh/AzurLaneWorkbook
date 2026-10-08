//! 保存当前客户端完整舰船静态目录的最小可导航领域视图。

use super::SkillEffectEvidenceCatalog;

/// 舰船静态目录映射契约版本。
pub const SHIP_CATALOG_SCHEMA_VERSION: u32 = 1;

/// 舰船静态目录的模块身份和规范内容摘要。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipCatalogSource {
    schema_version: u32,
    module_sha256: String,
    content_sha256: String,
}

impl ShipCatalogSource {
    pub(crate) fn new(module_sha256: String, content_sha256: String) -> Self {
        Self {
            schema_version: SHIP_CATALOG_SCHEMA_VERSION,
            module_sha256,
            content_sha256,
        }
    }

    /// 返回舰船静态目录领域契约版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回读取目录时验证的目标模块 SHA-256。
    pub fn module_sha256(&self) -> &str {
        &self.module_sha256
    }

    /// 返回核心静态表及本次请求的科技配置、图鉴历史的规范内容摘要。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 一项与舰船组关联的技能定义及其完整等级证据索引。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipStaticSkill {
    skill_id: u64,
    name: String,
    description: String,
    declared_max_level: u32,
    effect_levels: Vec<u32>,
    definition_raw_ref: Option<String>,
    display_raw_ref: Option<String>,
    evidence_gap: Option<String>,
}

impl ShipStaticSkill {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        skill_id: u64,
        name: String,
        description: String,
        declared_max_level: u32,
        effect_levels: Vec<u32>,
        definition_raw_ref: Option<String>,
        display_raw_ref: Option<String>,
        evidence_gap: Option<String>,
    ) -> Self {
        Self {
            skill_id,
            name,
            description,
            declared_max_level,
            effect_levels,
            definition_raw_ref,
            display_raw_ref,
            evidence_gap,
        }
    }

    /// 返回静态技能 ID。
    pub const fn skill_id(&self) -> u64 {
        self.skill_id
    }

    /// 返回技能定义名称；定义缺失时可能为空。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回技能定义中的说明模板。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 返回静态定义声明的最大等级，零表示使用默认一级效果。
    pub const fn declared_max_level(&self) -> u32 {
        self.declared_max_level
    }

    /// 返回实际读取效果证据的等级集合。
    pub fn effect_levels(&self) -> &[u32] {
        &self.effect_levels
    }

    /// 返回 `skill_data_template` 原始记录引用。
    pub fn definition_raw_ref(&self) -> Option<&str> {
        self.definition_raw_ref.as_deref()
    }

    /// 返回可选的 `skill_data_display` 原始记录引用。
    pub fn display_raw_ref(&self) -> Option<&str> {
        self.display_raw_ref.as_deref()
    }

    /// 返回定义或等级证据缺口；完整时为空。
    pub fn evidence_gap(&self) -> Option<&str> {
        self.evidence_gap.as_deref()
    }
}

/// 同一 `group_id` 下全部变体的静态身份和明确关系入口。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipCatalogGroup {
    group_id: u64,
    representative_config_id: u64,
    name: String,
    english_name: String,
    ship_type_id: u64,
    nation_id: u64,
    armor_type_id: u64,
    rarity: u8,
    maximum_level: u32,
    maximum_stars: u32,
    slot_allowed_equipment_type_ids: [Vec<u64>; 5],
    variant_config_ids: Vec<u64>,
    relationship_raw_refs: Vec<String>,
    skill_ids: Vec<u64>,
}

impl ShipCatalogGroup {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        group_id: u64,
        representative_config_id: u64,
        name: String,
        english_name: String,
        ship_type_id: u64,
        nation_id: u64,
        armor_type_id: u64,
        rarity: u8,
        maximum_level: u32,
        maximum_stars: u32,
        slot_allowed_equipment_type_ids: [Vec<u64>; 5],
        variant_config_ids: Vec<u64>,
        relationship_raw_refs: Vec<String>,
        skill_ids: Vec<u64>,
    ) -> Self {
        Self {
            group_id,
            representative_config_id,
            name,
            english_name,
            ship_type_id,
            nation_id,
            armor_type_id,
            rarity,
            maximum_level,
            maximum_stars,
            slot_allowed_equipment_type_ids,
            variant_config_ids,
            relationship_raw_refs,
            skill_ids,
        }
    }

    /// 返回客户端舰船组 ID。
    pub const fn group_id(&self) -> u64 {
        self.group_id
    }

    /// 返回经严格验证存在的 `group_id * 10 + 1` 基础配置 ID。
    pub const fn representative_config_id(&self) -> u64 {
        self.representative_config_id
    }

    /// 返回基础配置的中文名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回基础配置的英文名称。
    pub fn english_name(&self) -> &str {
        &self.english_name
    }

    /// 返回基础配置的舰种 ID。
    pub const fn ship_type_id(&self) -> u64 {
        self.ship_type_id
    }

    /// 返回基础配置的阵营 ID。
    pub const fn nation_id(&self) -> u64 {
        self.nation_id
    }

    /// 返回基础配置的装甲类型 ID。
    pub const fn armor_type_id(&self) -> u64 {
        self.armor_type_id
    }

    /// 返回基础配置的稀有度。
    pub const fn rarity(&self) -> u8 {
        self.rarity
    }

    /// 返回基础配置声明的最大等级。
    pub const fn maximum_level(&self) -> u32 {
        self.maximum_level
    }

    /// 返回基础配置声明的最大星级。
    pub const fn maximum_stars(&self) -> u32 {
        self.maximum_stars
    }

    /// 返回基础配置五个槽位允许的装备类型 ID。
    pub fn slot_allowed_equipment_type_ids(&self) -> &[Vec<u64>; 5] {
        &self.slot_allowed_equipment_type_ids
    }

    /// 返回同组全部模板配置 ID。
    pub fn variant_config_ids(&self) -> &[u64] {
        &self.variant_config_ids
    }

    /// 返回可在 `raw_data` 中直接定位的变体、突破、改造、科研和 META 记录引用。
    pub fn relationship_raw_refs(&self) -> &[String] {
        &self.relationship_raw_refs
    }

    /// 返回该组经明确字段关联到的全部静态技能 ID。
    pub fn skill_ids(&self) -> &[u64] {
        &self.skill_ids
    }
}

/// 当前客户端 17 张静态表映射出的完整舰船目录。
#[derive(Clone, Debug, PartialEq)]
pub struct ShipCatalog {
    source: ShipCatalogSource,
    groups: Vec<ShipCatalogGroup>,
    skills: Vec<ShipStaticSkill>,
    skill_effects: SkillEffectEvidenceCatalog,
    technology_history_available: bool,
    technology_read_error: Option<String>,
}

impl ShipCatalog {
    pub(crate) fn new(
        source: ShipCatalogSource,
        groups: Vec<ShipCatalogGroup>,
        skills: Vec<ShipStaticSkill>,
        skill_effects: SkillEffectEvidenceCatalog,
    ) -> Self {
        Self {
            source,
            groups,
            skills,
            skill_effects,
            technology_history_available: false,
            technology_read_error: None,
        }
    }

    pub(crate) fn with_technology_history(mut self, available: bool) -> Self {
        self.technology_history_available = available;
        self
    }
    pub(crate) fn with_technology_error(mut self, error: Option<String>) -> Self {
        self.technology_read_error = error;
        self
    }
    pub fn technology_read_error(&self) -> Option<&str> {
        self.technology_read_error.as_deref()
    }
    pub const fn technology_history_available(&self) -> bool {
        self.technology_history_available
    }

    /// 返回静态目录来源身份。
    pub const fn source(&self) -> &ShipCatalogSource {
        &self.source
    }

    /// 返回按 `group_id` 严格升序保存的全部舰船组。
    pub fn groups(&self) -> &[ShipCatalogGroup] {
        &self.groups
    }

    /// 按舰船组 ID 查找静态组。
    pub fn group(&self, group_id: u64) -> Option<&ShipCatalogGroup> {
        self.groups
            .binary_search_by_key(&group_id, ShipCatalogGroup::group_id)
            .ok()
            .map(|index| &self.groups[index])
    }

    /// 返回按技能 ID 严格升序保存的静态技能定义。
    pub fn skills(&self) -> &[ShipStaticSkill] {
        &self.skills
    }

    /// 按技能 ID 查找静态定义。
    pub fn skill(&self, skill_id: u64) -> Option<&ShipStaticSkill> {
        self.skills
            .binary_search_by_key(&skill_id, ShipStaticSkill::skill_id)
            .ok()
            .map(|index| &self.skills[index])
    }

    /// 返回全部静态技能等级效果证据。
    pub const fn skill_effects(&self) -> &SkillEffectEvidenceCatalog {
        &self.skill_effects
    }
}

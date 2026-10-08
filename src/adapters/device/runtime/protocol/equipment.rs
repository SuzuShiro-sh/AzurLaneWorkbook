//! 定义装备目录、配方、武器、技能效果和引用名称协议模型。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    MAX_EQUIPMENT_CATALOG_ITEMS, MAX_EQUIPMENT_PAGE_SIZE, MAX_EQUIPMENT_REFERENCE_BATCH_SIZE,
    MAX_EQUIPMENT_WEAPON_BATCH_SIZE, MAX_SKILL_EFFECT_BATCH_SIZE, RuntimeProtocolError,
    deserialize_required_nullable, validate_lua_json_value, validate_next_catalog_index,
    validate_non_empty, validate_nonnegative_lua_integer, validate_positive_lua_integer,
    validate_positive_u32, validate_positive_u64, validate_sha256, validate_stable_token,
};

/// 装备静态目录请求使用的零基起点和有界页容量。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EquipmentCatalogPagePayload {
    /// `all` 数组中的零基起点。
    pub start_index: u32,
    /// 本次最多消费的目录索引数量。
    pub page_size: u32,
}

impl EquipmentCatalogPagePayload {
    /// 校验目录边界后创建分页载荷。
    pub fn new(start_index: u32, page_size: u32) -> Result<Self, RuntimeProtocolError> {
        if start_index > MAX_EQUIPMENT_CATALOG_ITEMS
            || !(1..=MAX_EQUIPMENT_PAGE_SIZE).contains(&page_size)
        {
            return Err(RuntimeProtocolError::new(
                "catalog_page_out_of_range",
                format!(
                    "start_index 只允许 0 至 {MAX_EQUIPMENT_CATALOG_ITEMS}，page_size 只允许 1 至 {MAX_EQUIPMENT_PAGE_SIZE}"
                ),
            ));
        }
        Ok(Self {
            start_index,
            page_size,
        })
    }
}

/// 显式配置 ID 使用单帧容量，避免读取整个目录。
#[derive(Clone, Debug, Serialize)]
pub(crate) struct EquipmentConfigBatchPayload {
    pub ids: Vec<u64>,
}
impl EquipmentConfigBatchPayload {
    pub fn new(ids: &[u64]) -> Result<Self, RuntimeProtocolError> {
        if ids.is_empty() || ids.len() > super::MAX_EQUIPMENT_FRAME_SIZE as usize {
            return Err(RuntimeProtocolError::new(
                "equipment_config_batch_invalid",
                "ids 必须包含 1 至 250 个标识",
            ));
        }
        validate_reference_identifiers("ids", ids, false)?;
        Ok(Self { ids: ids.to_vec() })
    }
}

/// 显式装备配置查询，缺失与读取失败分别报告。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentConfigBatchResult {
    pub schema_version: u32,
    pub complete: bool,
    pub count: u32,
    pub source: EquipmentConfigSource,
    pub configs: Vec<RuntimeEquipmentConfig>,
    pub missing_ids: Vec<u64>,
    pub read_errors: Vec<EquipmentConfigPageReadError>,
}
impl EquipmentConfigBatchResult {
    pub(crate) fn validate(&self, ids: &[u64], expected: &str) -> Result<(), RuntimeProtocolError> {
        self.source.validate(expected)?;
        if self.schema_version != 1
            || self.count as usize != self.configs.len()
            || !self.read_errors.is_empty()
        {
            return Err(RuntimeProtocolError::new(
                "equipment_config_batch_invalid",
                "装备配置批次版本、数量或错误字段无效",
            ));
        }
        let found: Vec<_> = self.configs.iter().map(|c| c.config_id).collect();
        validate_reference_identifiers("configs", &found, false)?;
        validate_reference_identifiers("missing_ids", &self.missing_ids, false)?;
        let mut accounted = found;
        accounted.extend_from_slice(&self.missing_ids);
        accounted.sort_unstable();
        if accounted != ids {
            return Err(RuntimeProtocolError::new(
                "equipment_config_batch_invalid",
                "configs 与 missing_ids 必须恰好覆盖请求",
            ));
        }
        for config in &self.configs {
            config.validate()?;
        }
        if self.complete != self.configs.iter().all(|c| c.complete) {
            return Err(RuntimeProtocolError::new(
                "equipment_config_batch_invalid",
                "装备配置完整性声明不一致",
            ));
        }
        Ok(())
    }
}

/// 显式武器 ID 批次；严格顺序使请求与响应可以逐项核对。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EquipmentWeaponBatchPayload {
    pub weapon_ids: Vec<u64>,
}

impl EquipmentWeaponBatchPayload {
    /// 要求非空、受限、严格升序且不重复的 Lua 精确整数标识。
    pub fn new(weapon_ids: &[u64]) -> Result<Self, RuntimeProtocolError> {
        if weapon_ids.is_empty() || weapon_ids.len() > MAX_EQUIPMENT_WEAPON_BATCH_SIZE {
            return Err(RuntimeProtocolError::new(
                "equipment_weapon_batch_invalid",
                format!("weapon_ids 只允许 1 至 {MAX_EQUIPMENT_WEAPON_BATCH_SIZE} 个标识"),
            ));
        }
        let mut previous = None;
        for &weapon_id in weapon_ids {
            validate_positive_lua_integer("equipment_weapon.weapon_id", weapon_id)?;
            if previous.is_some_and(|value| value >= weapon_id) {
                return Err(RuntimeProtocolError::new(
                    "equipment_weapon_batch_invalid",
                    "weapon_ids 必须严格升序且不重复",
                ));
            }
            previous = Some(weapon_id);
        }
        Ok(Self {
            weapon_ids: weapon_ids.to_vec(),
        })
    }
}

/// 技能效果证据按技能标识和客户端实际生效等级唯一定位。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEffectQuery {
    pub skill_id: u64,
    pub level: u32,
}

impl SkillEffectQuery {
    /// 创建可由 Lua number 精确承载的正整数技能键。
    pub fn new(skill_id: u64, level: u32) -> Result<Self, RuntimeProtocolError> {
        let query = Self { skill_id, level };
        query.validate()?;
        Ok(query)
    }

    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("skill_effect.skill_id", self.skill_id)?;
        validate_positive_u32("skill_effect.level", self.level)
    }
}

/// 显式技能等级批次；顺序与唯一性共同冻结响应关联方式。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkillEffectBatchPayload {
    pub skills: Vec<SkillEffectQuery>,
}

impl SkillEffectBatchPayload {
    pub fn new(skills: &[SkillEffectQuery]) -> Result<Self, RuntimeProtocolError> {
        if skills.is_empty() || skills.len() > MAX_SKILL_EFFECT_BATCH_SIZE {
            return Err(RuntimeProtocolError::new(
                "skill_effect_batch_invalid",
                format!("skills 只允许 1 至 {MAX_SKILL_EFFECT_BATCH_SIZE} 个查询"),
            ));
        }
        let mut previous: Option<(u64, u32)> = None;
        for skill in skills {
            skill.validate()?;
            let key = (skill.skill_id, skill.level);
            if previous.is_some_and(|value| value >= key) {
                return Err(RuntimeProtocolError::new(
                    "skill_effect_batch_invalid",
                    "skills 必须按 skill_id、level 严格升序且不重复",
                ));
            }
            previous = Some(key);
        }
        Ok(Self {
            skills: skills.to_vec(),
        })
    }
}

/// 装备目录去重后的四类名称引用；各数组分别保持确定性顺序。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EquipmentReferenceNameBatchPayload {
    pub equipment_type_ids: Vec<u64>,
    pub nation_ids: Vec<u64>,
    pub ship_type_ids: Vec<u64>,
    pub attribute_keys: Vec<String>,
}

impl EquipmentReferenceNameBatchPayload {
    /// 校验各命名空间的顺序、唯一性、字符边界和合计容量。
    pub fn new(
        equipment_type_ids: &[u64],
        nation_ids: &[u64],
        ship_type_ids: &[u64],
        attribute_keys: &[String],
    ) -> Result<Self, RuntimeProtocolError> {
        let total_count = equipment_type_ids.len()
            + nation_ids.len()
            + ship_type_ids.len()
            + attribute_keys.len();
        if total_count == 0 || total_count > MAX_EQUIPMENT_REFERENCE_BATCH_SIZE {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_batch_invalid",
                format!(
                    "装备引用名称请求合计只允许 1 至 {MAX_EQUIPMENT_REFERENCE_BATCH_SIZE} 个键"
                ),
            ));
        }
        validate_reference_identifiers("equipment_type_ids", equipment_type_ids, false)?;
        validate_reference_identifiers("nation_ids", nation_ids, true)?;
        validate_reference_identifiers("ship_type_ids", ship_type_ids, false)?;

        let mut previous_key: Option<&str> = None;
        for key in attribute_keys {
            if key.is_empty()
                || key.len() > 128
                || !key.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
            {
                return Err(RuntimeProtocolError::new(
                    "equipment_reference_batch_invalid",
                    "attribute_keys 只能包含 1 至 128 字节的小写稳定标识",
                ));
            }
            if previous_key.is_some_and(|previous| previous >= key.as_str()) {
                return Err(RuntimeProtocolError::new(
                    "equipment_reference_batch_invalid",
                    "attribute_keys 必须严格字典序且不重复",
                ));
            }
            previous_key = Some(key);
        }

        Ok(Self {
            equipment_type_ids: equipment_type_ids.to_vec(),
            nation_ids: nation_ids.to_vec(),
            ship_type_ids: ship_type_ids.to_vec(),
            attribute_keys: attribute_keys.to_vec(),
        })
    }
}

fn validate_reference_identifiers(
    field: &'static str,
    identifiers: &[u64],
    allow_zero: bool,
) -> Result<(), RuntimeProtocolError> {
    let mut previous = None;
    for &identifier in identifiers {
        if allow_zero {
            validate_nonnegative_lua_integer(field, identifier)?;
        } else {
            validate_positive_lua_integer(field, identifier)?;
        }
        if previous.is_some_and(|value| value >= identifier) {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_batch_invalid",
                format!("{field} 必须严格升序且不重复"),
            ));
        }
        previous = Some(identifier);
    }
    Ok(())
}

/// 装备静态配置与配方分页共同使用的目标模块身份。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentConfigSource {
    /// loader 已验证并写入 bootstrap 的 64 位小写 SHA-256。
    pub module_sha256: String,
}

impl EquipmentConfigSource {
    /// 要求摘要格式规范且与当前会话预期模块完全一致。
    pub(super) fn validate(&self, expected: &str) -> Result<(), RuntimeProtocolError> {
        validate_sha256("equipment_config.source.module_sha256", &self.module_sha256)?;
        validate_sha256("expected_module_sha256", expected)?;
        if self.module_sha256 != expected {
            return Err(RuntimeProtocolError::new(
                "equipment_config_source_mismatch",
                format!(
                    "装备配置模块摘要应为 {expected}，实际为 {}",
                    self.module_sha256
                ),
            ));
        }
        Ok(())
    }
}

/// `snapshot_equipment_configs` 返回的一页客户端装备静态配置。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentConfigPageResult {
    /// 装备配置分页 schema 固定为 1。
    pub schema_version: u32,
    /// 页内每个目录位置和配置记录都完整时为 true。
    pub complete: bool,
    /// 成功返回的配置记录数量。
    pub count: u32,
    /// 当前已验证的目标模块身份。
    pub source: EquipmentConfigSource,
    /// 本页在 `equip_data_template.all` 中的零基起点。
    pub start_index: u32,
    /// 当前目录总条目数，所有页面必须保持一致。
    pub total_count: u32,
    /// 下一页起点；已经抵达目录末尾时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub next_index: Option<u32>,
    /// 按目录顺序成功读取的装备配置。
    pub configs: Vec<RuntimeEquipmentConfig>,
    /// 目录索引本身无法读取时的诊断。
    pub read_errors: Vec<EquipmentConfigPageReadError>,
}

impl EquipmentConfigPageResult {
    /// 复核来源、页游标、条目顺序、动态值边界及完整性声明。
    pub(crate) fn validate(
        &self,
        requested_start_index: u32,
        requested_page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "equipment_config_schema_unsupported",
                format!("只支持装备配置 schema 1，实际为 {}", self.schema_version),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        if self.start_index != requested_start_index
            || self.total_count == 0
            || self.total_count > MAX_EQUIPMENT_CATALOG_ITEMS
            || self.start_index > self.total_count
        {
            return Err(RuntimeProtocolError::new(
                "equipment_config_cursor_invalid",
                "装备配置页起点或总数不符合请求边界",
            ));
        }
        if self.configs.len() != self.count as usize || self.count > requested_page_size {
            return Err(RuntimeProtocolError::new(
                "equipment_config_count_mismatch",
                "装备配置 count 必须等于 configs 长度且不超过请求页容量",
            ));
        }
        validate_next_catalog_index(
            "equipment_config",
            self.start_index,
            self.total_count,
            requested_page_size,
            self.next_index,
        )?;

        let mut previous_config_id: Option<u64> = None;
        for config in &self.configs {
            config.validate()?;
            if previous_config_id.is_some_and(|previous| previous >= config.config_id) {
                return Err(RuntimeProtocolError::new(
                    "equipment_config_order_invalid",
                    "装备配置必须按目录中的 config_id 严格升序返回",
                ));
            }
            previous_config_id = Some(config.config_id);
        }
        for error in &self.read_errors {
            error.validate(
                self.start_index,
                self.next_index.unwrap_or(self.total_count),
            )?;
        }
        let records_complete = self.configs.iter().all(|config| config.complete);
        if self.complete != (self.read_errors.is_empty() && records_complete) {
            return Err(RuntimeProtocolError::new(
                "equipment_config_completeness_mismatch",
                "装备配置页 complete 必须与记录和页级错误保持一致",
            ));
        }
        Ok(())
    }
}

/// 单个 Equipment 对象的静态配置、派生结果和引用标识。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentConfig {
    /// 当前强化页的装备配置 ID。
    pub config_id: u64,
    /// 强化链的根配置 ID；读取根对象失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub root_config_id: Option<u64>,
    /// 三个已证明来源按统计、模板、运行态优先级合并后的原始配置。
    pub raw_config: Value,
    /// `Equipment:GetAttributes()` 的完整结果。
    pub attributes: Value,
    /// `Equipment:GetPropertiesInfo()` 的完整结果。
    pub properties: Value,
    /// `Equipment:GetSkill()` 的原始结果。
    pub skill: Value,
    /// `Equipment:GetPropertyRate()` 的原始结果。
    pub property_rate: Value,
    /// 后续武器详情批量读取所需的引用 ID。
    pub weapon_ids: Vec<u64>,
    /// 客户端计算的装备分数；方法失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub gear_score: Option<u64>,
    /// 海域加成；当前装备没有加成时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub anti_siren_power: Option<f64>,
    /// 客户端是否把装备分类为设备；方法失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub is_device: Option<bool>,
    /// 客户端是否把装备分类为舰载机；方法失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub is_aircraft: Option<bool>,
    /// 所有来源和派生方法都完整时为 true。
    pub complete: bool,
    /// 单条配置的长期可定位读取错误。
    pub read_errors: Vec<String>,
}

impl RuntimeEquipmentConfig {
    /// 校验身份、原始结构、引用顺序和逐项完整性。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("equipment_config.config_id", self.config_id)?;
        if let Some(root_config_id) = self.root_config_id {
            validate_positive_u64("equipment_config.root_config_id", root_config_id)?;
        }
        if !self.raw_config.is_object() {
            return Err(RuntimeProtocolError::new(
                "equipment_raw_config_invalid",
                format!("config_id={} 的 raw_config 不是对象", self.config_id),
            ));
        }
        validate_lua_json_value("raw_config", &self.raw_config, 0)?;
        for (field, value) in [
            ("attributes", &self.attributes),
            ("properties", &self.properties),
            ("skill", &self.skill),
            ("property_rate", &self.property_rate),
        ] {
            validate_lua_json_value(field, value, 0)?;
        }
        let mut previous_weapon_id: Option<u64> = None;
        for &weapon_id in &self.weapon_ids {
            validate_positive_u64("equipment_config.weapon_id", weapon_id)?;
            if previous_weapon_id.is_some_and(|previous| previous >= weapon_id) {
                return Err(RuntimeProtocolError::new(
                    "equipment_weapon_order_invalid",
                    "weapon_ids 必须严格升序且不重复",
                ));
            }
            previous_weapon_id = Some(weapon_id);
        }
        if self
            .anti_siren_power
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err(RuntimeProtocolError::new(
                "equipment_anti_siren_invalid",
                "anti_siren_power 必须为 null 或非负有限数值",
            ));
        }
        for error in &self.read_errors {
            validate_non_empty("equipment_config.read_error", error, 4096)?;
        }
        if self.complete != self.read_errors.is_empty() {
            return Err(RuntimeProtocolError::new(
                "equipment_record_completeness_mismatch",
                "装备配置记录 complete 必须与 read_errors 保持一致",
            ));
        }
        Ok(())
    }
}

/// 装备目录页级错误保留零基目录位置和可空配置 ID。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentConfigPageReadError {
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub catalog_index: Option<u32>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub config_id: Option<u64>,
    pub code: String,
    pub message: String,
}

impl EquipmentConfigPageReadError {
    fn validate(&self, start_index: u32, end_index: u32) -> Result<(), RuntimeProtocolError> {
        if self
            .catalog_index
            .is_some_and(|index| index < start_index || index >= end_index)
        {
            return Err(RuntimeProtocolError::new(
                "equipment_error_index_invalid",
                "装备目录错误位置不在当前页范围内",
            ));
        }
        if let Some(config_id) = self.config_id {
            validate_positive_u64("equipment_config_error.config_id", config_id)?;
        }
        validate_stable_token("equipment_config_error.code", &self.code)?;
        validate_non_empty("equipment_config_error.message", &self.message, 4096)
    }
}

/// `snapshot_compose_recipes` 返回的一页静态合成配方。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeRecipePageResult {
    pub schema_version: u32,
    pub complete: bool,
    pub count: u32,
    pub source: EquipmentConfigSource,
    pub start_index: u32,
    pub total_count: u32,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub next_index: Option<u32>,
    pub recipes: Vec<EquipmentComposeRecipe>,
    pub read_errors: Vec<ComposeRecipePageReadError>,
}

impl ComposeRecipePageResult {
    /// 校验配方分页连续性、来源一致性、条目顺序和完整性。
    pub(crate) fn validate(
        &self,
        requested_start_index: u32,
        requested_page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "compose_recipe_schema_unsupported",
                format!("只支持合成配方 schema 1，实际为 {}", self.schema_version),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        if self.start_index != requested_start_index
            || self.total_count == 0
            || self.total_count > MAX_EQUIPMENT_CATALOG_ITEMS
            || self.start_index > self.total_count
            || self.recipes.len() != self.count as usize
            || self.count > requested_page_size
        {
            return Err(RuntimeProtocolError::new(
                "compose_recipe_page_invalid",
                "合成配方页的起点、总数或 count 不符合请求边界",
            ));
        }
        validate_next_catalog_index(
            "compose_recipe",
            self.start_index,
            self.total_count,
            requested_page_size,
            self.next_index,
        )?;
        let mut previous_recipe_id: Option<u64> = None;
        for recipe in &self.recipes {
            recipe.validate()?;
            if previous_recipe_id.is_some_and(|previous| previous >= recipe.recipe_id) {
                return Err(RuntimeProtocolError::new(
                    "compose_recipe_order_invalid",
                    "合成配方必须按 recipe_id 严格升序返回",
                ));
            }
            previous_recipe_id = Some(recipe.recipe_id);
        }
        for error in &self.read_errors {
            error.validate(
                self.start_index,
                self.next_index.unwrap_or(self.total_count),
            )?;
        }
        if self.complete != self.read_errors.is_empty() {
            return Err(RuntimeProtocolError::new(
                "compose_recipe_completeness_mismatch",
                "合成配方页 complete 必须与 read_errors 保持一致",
            ));
        }
        Ok(())
    }
}

/// 与玩家资源无关的装备合成配方。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentComposeRecipe {
    pub recipe_id: u64,
    pub material_id: u64,
    pub material_count: u64,
    pub gold: u64,
    pub equipment_id: u64,
}

impl EquipmentComposeRecipe {
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("compose_recipe.recipe_id", self.recipe_id)?;
        validate_positive_u64("compose_recipe.material_id", self.material_id)?;
        validate_positive_u64("compose_recipe.material_count", self.material_count)?;
        validate_positive_u64("compose_recipe.equipment_id", self.equipment_id)
    }
}

/// 配方目录页级错误保留零基位置和可空配方 ID。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeRecipePageReadError {
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub catalog_index: Option<u32>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub recipe_id: Option<u64>,
    pub code: String,
    pub message: String,
}

impl ComposeRecipePageReadError {
    fn validate(&self, start_index: u32, end_index: u32) -> Result<(), RuntimeProtocolError> {
        if self
            .catalog_index
            .is_some_and(|index| index < start_index || index >= end_index)
        {
            return Err(RuntimeProtocolError::new(
                "compose_recipe_error_index_invalid",
                "合成配方错误位置不在当前页范围内",
            ));
        }
        if let Some(recipe_id) = self.recipe_id {
            validate_positive_u64("compose_recipe_error.recipe_id", recipe_id)?;
        }
        validate_stable_token("compose_recipe_error.code", &self.code)?;
        validate_non_empty("compose_recipe_error.message", &self.message, 4096)
    }
}

/// `snapshot_equipment_weapons` 返回的显式武器参数批次。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentWeaponBatchResult {
    pub schema_version: u32,
    pub complete: bool,
    pub count: u32,
    pub source: EquipmentConfigSource,
    pub weapons: Vec<RuntimeEquipmentWeaponDetail>,
}

impl EquipmentWeaponBatchResult {
    /// 核对来源、请求顺序、Lua 动态值边界和批次完整性。
    pub(crate) fn validate(
        &self,
        requested_weapon_ids: &[u64],
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "equipment_weapon_schema_unsupported",
                format!("只支持装备武器 schema 1，实际为 {}", self.schema_version),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        if self.weapons.len() != self.count as usize
            || self.weapons.len() != requested_weapon_ids.len()
            || self.weapons.is_empty()
            || self.weapons.len() > MAX_EQUIPMENT_WEAPON_BATCH_SIZE
        {
            return Err(RuntimeProtocolError::new(
                "equipment_weapon_count_mismatch",
                "装备武器 count 必须与请求和 weapons 长度完全一致",
            ));
        }
        for (detail, &requested_id) in self.weapons.iter().zip(requested_weapon_ids) {
            detail.validate(requested_id)?;
        }
        if self.complete != self.weapons.iter().all(|detail| detail.complete) {
            return Err(RuntimeProtocolError::new(
                "equipment_weapon_completeness_mismatch",
                "装备武器批次 complete 必须与所有记录保持一致",
            ));
        }
        Ok(())
    }
}

/// 单个武器配置的完整原始 Lua 字段。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentWeaponDetail {
    pub weapon_id: u64,
    pub raw: Value,
    pub complete: bool,
    pub read_errors: Vec<String>,
}

impl RuntimeEquipmentWeaponDetail {
    fn validate(&self, requested_id: u64) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("equipment_weapon.weapon_id", self.weapon_id)?;
        if self.weapon_id != requested_id {
            return Err(RuntimeProtocolError::new(
                "equipment_weapon_order_mismatch",
                format!(
                    "装备武器响应应返回 weapon_id={requested_id}，实际为 {}",
                    self.weapon_id
                ),
            ));
        }
        validate_lua_json_value("equipment_weapon.raw", &self.raw, 0)?;
        for error in &self.read_errors {
            validate_non_empty("equipment_weapon.read_error", error, 4096)?;
        }
        if self.complete != self.read_errors.is_empty() {
            return Err(RuntimeProtocolError::new(
                "equipment_weapon_record_completeness_mismatch",
                "装备武器记录 complete 必须与 read_errors 保持一致",
            ));
        }
        Ok(())
    }
}

/// `snapshot_skill_effects` 返回的显式技能效果证据批次。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEffectBatchResult {
    pub schema_version: u32,
    pub complete: bool,
    pub count: u32,
    pub source: EquipmentConfigSource,
    pub skills: Vec<RuntimeSkillEffectDetail>,
}

impl SkillEffectBatchResult {
    /// 核对每个技能键、三路来源状态和批次完整性。
    pub(crate) fn validate(
        &self,
        requested_skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "skill_effect_schema_unsupported",
                format!(
                    "只支持技能效果证据 schema 1，实际为 {}",
                    self.schema_version
                ),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        if self.skills.len() != self.count as usize
            || self.skills.len() != requested_skills.len()
            || self.skills.is_empty()
            || self.skills.len() > MAX_SKILL_EFFECT_BATCH_SIZE
        {
            return Err(RuntimeProtocolError::new(
                "skill_effect_count_mismatch",
                "技能效果 count 必须与请求和 skills 长度完全一致",
            ));
        }
        for (detail, requested) in self.skills.iter().zip(requested_skills) {
            detail.validate(requested)?;
        }
        if self.complete != self.skills.iter().all(|detail| detail.complete) {
            return Err(RuntimeProtocolError::new(
                "skill_effect_completeness_mismatch",
                "技能效果证据批次 complete 必须与所有记录保持一致",
            ));
        }
        Ok(())
    }
}

/// 一路技能来源区分调用失败和 Lua 值局部截断。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSkillEffectSource {
    pub available: bool,
    pub complete: bool,
    pub value: Value,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub error: Option<String>,
    pub read_errors: Vec<String>,
}

impl RuntimeSkillEffectSource {
    fn validate(&self, field: &'static str) -> Result<(), RuntimeProtocolError> {
        validate_lua_json_value(field, &self.value, 0)?;
        for error in &self.read_errors {
            validate_non_empty("skill_effect_source.read_error", error, 4096)?;
        }
        if let Some(error) = &self.error {
            validate_non_empty("skill_effect_source.error", error, 4096)?;
        }
        if self.available {
            if self.error.is_some() || self.complete != self.read_errors.is_empty() {
                return Err(RuntimeProtocolError::new(
                    "skill_effect_source_state_invalid",
                    "可用技能源不得含调用错误，complete 必须与 read_errors 保持一致",
                ));
            }
        } else if self.complete
            || self.error.is_none()
            || !self.read_errors.is_empty()
            || !self.value.is_null()
        {
            return Err(RuntimeProtocolError::new(
                "skill_effect_source_state_invalid",
                "不可用技能源必须包含调用错误，且不得声明完整、携带读取错误或返回值",
            ));
        }
        Ok(())
    }
}

/// 同一技能等级的展示配置、战斗技能和战斗 Buff 原始证据。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSkillEffectDetail {
    pub skill_id: u64,
    pub level: u32,
    pub display: RuntimeSkillEffectSource,
    pub battle_skill: RuntimeSkillEffectSource,
    pub battle_buff: RuntimeSkillEffectSource,
    pub complete: bool,
}

impl RuntimeSkillEffectDetail {
    fn validate(&self, requested: &SkillEffectQuery) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("skill_effect.skill_id", self.skill_id)?;
        validate_positive_u32("skill_effect.level", self.level)?;
        if self.skill_id != requested.skill_id || self.level != requested.level {
            return Err(RuntimeProtocolError::new(
                "skill_effect_order_mismatch",
                format!(
                    "技能效果证据响应应返回 ({}, {})，实际为 ({}, {})",
                    requested.skill_id, requested.level, self.skill_id, self.level
                ),
            ));
        }
        self.display.validate("skill_effect.display")?;
        self.battle_skill.validate("skill_effect.battle_skill")?;
        self.battle_buff.validate("skill_effect.battle_buff")?;
        let has_battle_source = self.battle_skill.available || self.battle_buff.available;
        let available_sources_complete = (!self.battle_skill.available
            || self.battle_skill.complete)
            && (!self.battle_buff.available || self.battle_buff.complete);
        let expected_complete =
            self.display.complete && has_battle_source && available_sources_complete;
        if self.complete != expected_complete {
            return Err(RuntimeProtocolError::new(
                "skill_effect_record_completeness_mismatch",
                "技能效果证据记录 complete 与三路来源状态不一致",
            ));
        }
        Ok(())
    }
}

/// `snapshot_equipment_reference_names` 返回的四类当前客户端显示名称。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentReferenceNameBatchResult {
    pub schema_version: u32,
    pub complete: bool,
    pub count: u32,
    pub source: EquipmentConfigSource,
    pub equipment_types: Vec<RuntimeEquipmentTypeName>,
    pub nations: Vec<RuntimeEquipmentNationName>,
    pub ship_types: Vec<RuntimeEquipmentShipTypeName>,
    pub attributes: Vec<RuntimeEquipmentAttributeName>,
}

impl EquipmentReferenceNameBatchResult {
    /// 核对来源、四类请求顺序、名称状态、合计数量和批次完整性。
    pub(crate) fn validate(
        &self,
        requested: &EquipmentReferenceNameBatchPayload,
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_schema_unsupported",
                format!(
                    "只支持装备引用名称 schema 1，实际为 {}",
                    self.schema_version
                ),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        let actual_count = self.equipment_types.len()
            + self.nations.len()
            + self.ship_types.len()
            + self.attributes.len();
        if actual_count != self.count as usize
            || actual_count == 0
            || actual_count > MAX_EQUIPMENT_REFERENCE_BATCH_SIZE
            || self.equipment_types.len() != requested.equipment_type_ids.len()
            || self.nations.len() != requested.nation_ids.len()
            || self.ship_types.len() != requested.ship_type_ids.len()
            || self.attributes.len() != requested.attribute_keys.len()
        {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_count_mismatch",
                "装备引用名称 count 与请求及四类响应长度不一致",
            ));
        }

        for (record, &identifier) in self
            .equipment_types
            .iter()
            .zip(&requested.equipment_type_ids)
        {
            record.validate(identifier)?;
        }
        for (record, &identifier) in self.nations.iter().zip(&requested.nation_ids) {
            record.validate(identifier)?;
        }
        for (record, &identifier) in self.ship_types.iter().zip(&requested.ship_type_ids) {
            record.validate(identifier)?;
        }
        for (record, key) in self.attributes.iter().zip(&requested.attribute_keys) {
            record.validate(key)?;
        }

        let expected_complete = self
            .equipment_types
            .iter()
            .all(RuntimeEquipmentTypeName::is_complete)
            && self
                .nations
                .iter()
                .all(RuntimeEquipmentNationName::is_complete)
            && self
                .ship_types
                .iter()
                .all(RuntimeEquipmentShipTypeName::is_complete)
            && self
                .attributes
                .iter()
                .all(RuntimeEquipmentAttributeName::is_complete);
        if self.complete != expected_complete {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_completeness_mismatch",
                "装备引用名称 complete 与逐项名称状态不一致",
            ));
        }
        Ok(())
    }
}

fn validate_reference_name_state(
    field: &'static str,
    name: &Option<String>,
    error: &Option<String>,
) -> Result<(), RuntimeProtocolError> {
    match (name, error) {
        (Some(name), None) => validate_non_empty(field, name, 512),
        (None, Some(error)) => validate_non_empty("equipment_reference.error", error, 4096),
        _ => Err(RuntimeProtocolError::new(
            "equipment_reference_state_invalid",
            format!("{field} 的 name 与 error 必须恰好存在一个"),
        )),
    }
}

/// 装备类型 ID 对应的当前客户端显示名称。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentTypeName {
    pub equipment_type_id: u64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub name: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub error: Option<String>,
}

impl RuntimeEquipmentTypeName {
    fn validate(&self, requested_id: u64) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("equipment_type_id", self.equipment_type_id)?;
        if self.equipment_type_id != requested_id {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_order_mismatch",
                format!(
                    "装备类型响应应返回 equipment_type_id={requested_id}，实际为 {}",
                    self.equipment_type_id
                ),
            ));
        }
        validate_reference_name_state("equipment_type.name", &self.name, &self.error)
    }

    const fn is_complete(&self) -> bool {
        self.name.is_some() && self.error.is_none()
    }
}

/// 阵营 ID 对应的当前客户端显示名称。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentNationName {
    pub nation_id: u64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub name: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub error: Option<String>,
}

impl RuntimeEquipmentNationName {
    fn validate(&self, requested_id: u64) -> Result<(), RuntimeProtocolError> {
        validate_nonnegative_lua_integer("nation_id", self.nation_id)?;
        if self.nation_id != requested_id {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_order_mismatch",
                format!(
                    "阵营响应应返回 nation_id={requested_id}，实际为 {}",
                    self.nation_id
                ),
            ));
        }
        validate_reference_name_state("nation.name", &self.name, &self.error)
    }

    const fn is_complete(&self) -> bool {
        self.name.is_some() && self.error.is_none()
    }
}

/// 舰种 ID 对应的当前客户端显示名称。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentShipTypeName {
    pub ship_type_id: u64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub name: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub error: Option<String>,
}

impl RuntimeEquipmentShipTypeName {
    fn validate(&self, requested_id: u64) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("ship_type_id", self.ship_type_id)?;
        if self.ship_type_id != requested_id {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_order_mismatch",
                format!(
                    "舰种响应应返回 ship_type_id={requested_id}，实际为 {}",
                    self.ship_type_id
                ),
            ));
        }
        validate_reference_name_state("ship_type.name", &self.name, &self.error)
    }

    const fn is_complete(&self) -> bool {
        self.name.is_some() && self.error.is_none()
    }
}

/// 属性稳定键对应的当前客户端显示名称。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentAttributeName {
    pub attribute_key: String,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub name: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub error: Option<String>,
}

impl RuntimeEquipmentAttributeName {
    fn validate(&self, requested_key: &str) -> Result<(), RuntimeProtocolError> {
        validate_stable_token("attribute_key", &self.attribute_key)?;
        if self.attribute_key != requested_key {
            return Err(RuntimeProtocolError::new(
                "equipment_reference_order_mismatch",
                format!(
                    "属性响应应返回 attribute_key={requested_key}，实际为 {}",
                    self.attribute_key
                ),
            ));
        }
        validate_reference_name_state("attribute.name", &self.name, &self.error)
    }

    const fn is_complete(&self) -> bool {
        self.name.is_some() && self.error.is_none()
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn selected_config_ids_are_bounded_and_exactly_accounted_for() {
        for ids in [vec![], vec![0], vec![2, 1], vec![1, 1], (1..=251).collect()] {
            assert!(EquipmentConfigBatchPayload::new(&ids).is_err());
        }
        let source = "a".repeat(64);
        let mut result = EquipmentConfigBatchResult {
            schema_version: 1,
            complete: true,
            count: 0,
            source: EquipmentConfigSource {
                module_sha256: source.clone(),
            },
            configs: vec![],
            missing_ids: vec![100, 200],
            read_errors: vec![],
        };
        assert!(result.validate(&[100, 200], &source).is_ok());
        assert!(result.validate(&[100, 300], &source).is_err());
        result.missing_ids = vec![100, 100];
        assert!(result.validate(&[100, 200], &source).is_err());
    }
}

//! 规范化的装备目录、强化配置、资源成本和合成配方模型。

use super::{EnhanceLevel, EquipmentConfigId, EquipmentFamilyId};

/// 当前核心装备目录领域结构及其键排序紧凑 JSON 摘要编码的冻结版本。
pub const EQUIPMENT_CATALOG_SCHEMA_VERSION: u32 = 2;

const EQUIPMENT_IMPORTANCE_MARKER: u32 = 2;
const DISMANTLE_CONFIRMATION_RARITY: u32 = 4;
// 配置 ID 末尾区段是工具附加的保守保护边界，并与设备端前检保持一致。
const PROTECTED_VARIANT_REMAINDER: u64 = 10;
const PROTECTED_VARIANT_MODULUS: u64 = 20;

/// 一次完整装备目录映射使用的模块身份和内容身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentCatalogSource {
    module_sha256: String,
    content_sha256: String,
}

impl EquipmentCatalogSource {
    pub(crate) fn new(module_sha256: String, content_sha256: String) -> Self {
        Self {
            module_sha256,
            content_sha256,
        }
    }

    /// 返回 loader 已验证的目标模块 SHA-256。
    pub fn module_sha256(&self) -> &str {
        &self.module_sha256
    }

    /// 返回按稳定顺序编码的核心装备目录语义内容 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 按装备族和配方标识稳定排序的核心静态目录。
///
/// 本目录保存身份、强化链、直接属性、兼容性、详情引用和合成配方；武器参数与技能效果
/// 由独立详情目录按引用关联。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentCatalog {
    source: EquipmentCatalogSource,
    families: Vec<EquipmentFamily>,
    recipes: Vec<EquipmentComposeRecipe>,
    config_count: usize,
}

impl EquipmentCatalog {
    pub(crate) fn new(
        source: EquipmentCatalogSource,
        families: Vec<EquipmentFamily>,
        recipes: Vec<EquipmentComposeRecipe>,
        config_count: usize,
    ) -> Self {
        Self {
            source,
            families,
            recipes,
            config_count,
        }
    }

    /// 返回当前装备目录领域结构的版本。
    pub const fn schema_version(&self) -> u32 {
        EQUIPMENT_CATALOG_SCHEMA_VERSION
    }

    /// 返回生成目录时使用的模块和内容身份。
    pub const fn source(&self) -> &EquipmentCatalogSource {
        &self.source
    }

    /// 返回按装备族 ID 严格升序排列的全部装备族。
    pub fn families(&self) -> &[EquipmentFamily] {
        &self.families
    }

    /// 返回按配方 ID 严格升序排列的全部合成配方。
    pub fn recipes(&self) -> &[EquipmentComposeRecipe] {
        &self.recipes
    }

    /// 返回所有装备族包含的强化配置总数。
    pub const fn config_count(&self) -> usize {
        self.config_count
    }

    /// 按装备族 ID 查找完整强化链。
    pub fn family(&self, family_id: EquipmentFamilyId) -> Option<&EquipmentFamily> {
        self.families
            .binary_search_by_key(&family_id, EquipmentFamily::family_id)
            .ok()
            .map(|index| &self.families[index])
    }
}

/// 同一根配置下按展示强化等级排列的完整装备族。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentFamily {
    family_id: EquipmentFamilyId,
    configs: Vec<EquipmentDefinition>,
}

impl EquipmentFamily {
    pub(crate) fn new(family_id: EquipmentFamilyId, configs: Vec<EquipmentDefinition>) -> Self {
        Self { family_id, configs }
    }

    /// 返回跨强化配置使用的装备族 ID。
    pub const fn family_id(&self) -> EquipmentFamilyId {
        self.family_id
    }

    /// 返回根配置解析出的装备名称。
    pub fn name(&self) -> &str {
        self.configs[0].identity.name()
    }

    /// 返回从 `+0` 开始连续排列的全部强化配置。
    pub fn configs(&self) -> &[EquipmentDefinition] {
        &self.configs
    }

    /// 按展示强化等级查找具体配置。
    pub fn config(&self, level: EnhanceLevel) -> Option<&EquipmentDefinition> {
        self.configs
            .binary_search_by_key(&level, |config| config.enhancement.level())
            .ok()
            .map(|index| &self.configs[index])
    }
}

/// 一个具体强化配置的规范化身份和静态参数。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentDefinition {
    identity: EquipmentIdentity,
    classification: EquipmentClassification,
    enhancement: EquipmentEnhancement,
    attributes: Vec<EquipmentAttribute>,
    compatibility: EquipmentCompatibility,
    weapon_ids: Vec<u64>,
    skill_references: Vec<EquipmentSkillReference>,
    labels: Vec<String>,
    description: String,
    gear_score: u64,
    anti_siren_power: Option<f64>,
    importance: u32,
    equipment_limit: u64,
}

impl EquipmentDefinition {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: EquipmentIdentity,
        classification: EquipmentClassification,
        enhancement: EquipmentEnhancement,
        attributes: Vec<EquipmentAttribute>,
        compatibility: EquipmentCompatibility,
        weapon_ids: Vec<u64>,
        skill_references: Vec<EquipmentSkillReference>,
        labels: Vec<String>,
        description: String,
        gear_score: u64,
        anti_siren_power: Option<f64>,
        importance: u32,
        equipment_limit: u64,
    ) -> Self {
        Self {
            identity,
            classification,
            enhancement,
            attributes,
            compatibility,
            weapon_ids,
            skill_references,
            labels,
            description,
            gear_score,
            anti_siren_power,
            importance,
            equipment_limit,
        }
    }

    /// 返回具体配置及其所属装备族的身份。
    pub const fn identity(&self) -> &EquipmentIdentity {
        &self.identity
    }

    /// 返回装备类型、阵营、品质和客户端分类结果。
    pub const fn classification(&self) -> &EquipmentClassification {
        &self.classification
    }

    /// 返回强化链位置、升级成本与拆解资源。
    pub const fn enhancement(&self) -> &EquipmentEnhancement {
        &self.enhancement
    }

    /// 返回按属性稳定键严格排序的直接属性。
    pub fn attributes(&self) -> &[EquipmentAttribute] {
        &self.attributes
    }

    /// 返回主槽、副槽和明确禁用舰种。
    pub const fn compatibility(&self) -> &EquipmentCompatibility {
        &self.compatibility
    }

    /// 依据槽位允许类型和明确禁用舰种判断适配；主副槽舰种表不推断槽位角色。
    pub(crate) fn can_be_equipped_by(&self, ship_type_id: u64, allowed_type_ids: &[u64]) -> bool {
        allowed_type_ids.contains(&self.classification().equipment_type().equipment_type_id())
            && !self
                .compatibility()
                .forbidden_ship_types()
                .iter()
                .any(|kind| kind.ship_type_id() == ship_type_id)
    }

    /// 返回按武器 ID 严格升序排列的武器详情引用。
    pub fn weapon_ids(&self) -> &[u64] {
        &self.weapon_ids
    }

    /// 返回按技能 ID、等级和可见性稳定排序的装备技能引用。
    pub fn skill_references(&self) -> &[EquipmentSkillReference] {
        &self.skill_references
    }

    /// 返回客户端用于装备分类的稳定标签。
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// 返回客户端装备说明；允许客户端以空串表示未填写。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 返回客户端计算的装备分数。
    pub const fn gear_score(&self) -> u64 {
        self.gear_score
    }

    /// 返回可选海域加成。
    pub const fn anti_siren_power(&self) -> Option<f64> {
        self.anti_siren_power
    }

    /// 返回客户端配置的重要程度值。
    pub const fn importance(&self) -> u32 {
        self.importance
    }

    /// 返回客户端装备互斥限制键；零表示没有限制。
    pub const fn equipment_limit(&self) -> u64 {
        self.equipment_limit
    }

    /// 根据客户端显式标志和工具附加保护边界返回自动拆解安全判定。
    pub const fn dismantle_safety(&self) -> EquipmentDismantleSafety {
        EquipmentDismantleSafety::new(
            self.identity.config_id,
            self.importance,
            self.classification.rarity,
            self.enhancement.level,
        )
    }
}

/// 从静态配置推导出的拆解保护条件。
///
/// 客户端的重要标志、品质和强化状态与工具的配置变体保护共同构成安全边界。
/// 自动执行只接受四类保护条件均未命中的装备，不把配置编号区段推断成客户端标志。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EquipmentDismantleSafety {
    important: bool,
    protected_variant: bool,
    rarity_confirmation_required: bool,
    enhanced: bool,
}

impl EquipmentDismantleSafety {
    pub(crate) const fn new(
        config_id: EquipmentConfigId,
        importance: u32,
        rarity: u32,
        enhance_level: EnhanceLevel,
    ) -> Self {
        Self {
            important: importance == EQUIPMENT_IMPORTANCE_MARKER,
            protected_variant: config_id.get() % PROTECTED_VARIANT_MODULUS
                >= PROTECTED_VARIANT_REMAINDER,
            rarity_confirmation_required: rarity >= DISMANTLE_CONFIRMATION_RARITY,
            enhanced: enhance_level.get() > 0,
        }
    }

    /// 返回客户端是否把装备标记为重要装备。
    pub const fn is_important(self) -> bool {
        self.important
    }

    /// 返回配置 ID 是否命中工具附加的保守变体保护区段。
    pub const fn is_protected_variant(self) -> bool {
        self.protected_variant
    }

    /// 返回装备品质是否达到客户端拆解界面的额外确认阈值。
    pub const fn requires_rarity_confirmation(self) -> bool {
        self.rarity_confirmation_required
    }

    /// 返回装备是否已经强化。
    pub const fn is_enhanced(self) -> bool {
        self.enhanced
    }

    /// 返回装备是否需要由用户额外确认后才能拆解。
    pub const fn requires_confirmation(self) -> bool {
        self.protected_variant || self.rarity_confirmation_required || self.enhanced
    }

    /// 返回装备是否满足无人值守自动拆解的保守安全边界。
    pub const fn allows_automatic_dismantle(self) -> bool {
        !self.important && !self.requires_confirmation()
    }
}

/// 具体强化配置与装备族的强类型身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentIdentity {
    config_id: EquipmentConfigId,
    family_id: EquipmentFamilyId,
    name: String,
    icon_key: String,
}

impl EquipmentIdentity {
    pub(crate) fn new(
        config_id: EquipmentConfigId,
        family_id: EquipmentFamilyId,
        name: String,
        icon_key: String,
    ) -> Self {
        Self {
            config_id,
            family_id,
            name,
            icon_key,
        }
    }

    /// 返回当前强化配置 ID。
    pub const fn config_id(&self) -> EquipmentConfigId {
        self.config_id
    }

    /// 返回当前配置所属的装备族 ID。
    pub const fn family_id(&self) -> EquipmentFamilyId {
        self.family_id
    }

    /// 返回当前客户端装备名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回当前客户端图标资源键。
    pub fn icon_key(&self) -> &str {
        &self.icon_key
    }
}

/// 装备的类型、阵营、品质和客户端分类结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentClassification {
    equipment_type: NamedEquipmentType,
    nation: NamedEquipmentNation,
    rarity: u32,
    tech_level: u32,
    speciality: String,
    ammo_type: u32,
    torpedo_ammo: u32,
    is_device: bool,
    is_aircraft: bool,
}

impl EquipmentClassification {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        equipment_type: NamedEquipmentType,
        nation: NamedEquipmentNation,
        rarity: u32,
        tech_level: u32,
        speciality: String,
        ammo_type: u32,
        torpedo_ammo: u32,
        is_device: bool,
        is_aircraft: bool,
    ) -> Self {
        Self {
            equipment_type,
            nation,
            rarity,
            tech_level,
            speciality,
            ammo_type,
            torpedo_ammo,
            is_device,
            is_aircraft,
        }
    }

    /// 返回客户端装备类型及其显示名称。
    pub const fn equipment_type(&self) -> &NamedEquipmentType {
        &self.equipment_type
    }

    /// 返回客户端阵营及其显示名称；阵营零值仍保留为有效分类。
    pub const fn nation(&self) -> &NamedEquipmentNation {
        &self.nation
    }

    /// 返回客户端品质数值。
    pub const fn rarity(&self) -> u32 {
        self.rarity
    }

    /// 返回客户端科技等级。
    pub const fn tech_level(&self) -> u32 {
        self.tech_level
    }

    /// 返回客户端特殊分类文本。
    pub fn speciality(&self) -> &str {
        &self.speciality
    }

    /// 返回客户端弹药类型标识。
    pub const fn ammo_type(&self) -> u32 {
        self.ammo_type
    }

    /// 返回客户端鱼雷弹药标识。
    pub const fn torpedo_ammo(&self) -> u32 {
        self.torpedo_ammo
    }

    /// 返回客户端是否将配置分类为设备。
    pub const fn is_device(&self) -> bool {
        self.is_device
    }

    /// 返回客户端是否将配置分类为舰载机。
    pub const fn is_aircraft(&self) -> bool {
        self.is_aircraft
    }
}

/// 装备类型 ID 及当前客户端显示名称。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedEquipmentType {
    equipment_type_id: u64,
    name: String,
}

impl NamedEquipmentType {
    pub(crate) fn new(equipment_type_id: u64, name: String) -> Self {
        Self {
            equipment_type_id,
            name,
        }
    }

    /// 返回装备类型的客户端数值 ID。
    pub const fn equipment_type_id(&self) -> u64 {
        self.equipment_type_id
    }

    /// 返回装备类型的客户端显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 阵营 ID 及当前客户端显示名称。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedEquipmentNation {
    nation_id: u64,
    name: String,
}

impl NamedEquipmentNation {
    pub(crate) fn new(nation_id: u64, name: String) -> Self {
        Self { nation_id, name }
    }

    /// 返回阵营的客户端数值 ID。
    pub const fn nation_id(&self) -> u64 {
        self.nation_id
    }

    /// 返回阵营的客户端显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 一项客户端直接装备属性。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentAttribute {
    key: String,
    name: String,
    value: f64,
    auxiliary_boost: bool,
}

impl EquipmentAttribute {
    pub(crate) fn new(key: String, name: String, value: f64, auxiliary_boost: bool) -> Self {
        Self {
            key,
            name,
            value,
            auxiliary_boost,
        }
    }

    /// 返回稳定属性键。
    pub fn key(&self) -> &str {
        &self.key
    }

    /// 返回属性的客户端显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回客户端直接属性值。
    pub const fn value(&self) -> f64 {
        self.value
    }

    /// 返回客户端是否标记了辅助增益。
    pub const fn auxiliary_boost(&self) -> bool {
        self.auxiliary_boost
    }
}

/// 装备在主槽、副槽和禁用列表中的舰种约束。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentCompatibility {
    main_ship_types: Vec<NamedEquipmentShipType>,
    sub_ship_types: Vec<NamedEquipmentShipType>,
    forbidden_ship_types: Vec<NamedEquipmentShipType>,
}

impl EquipmentCompatibility {
    pub(crate) fn new(
        main_ship_types: Vec<NamedEquipmentShipType>,
        sub_ship_types: Vec<NamedEquipmentShipType>,
        forbidden_ship_types: Vec<NamedEquipmentShipType>,
    ) -> Self {
        Self {
            main_ship_types,
            sub_ship_types,
            forbidden_ship_types,
        }
    }

    /// 返回允许放入主槽的舰种列表。
    pub fn main_ship_types(&self) -> &[NamedEquipmentShipType] {
        &self.main_ship_types
    }

    /// 返回允许放入副槽的舰种列表。
    pub fn sub_ship_types(&self) -> &[NamedEquipmentShipType] {
        &self.sub_ship_types
    }

    /// 返回客户端明确禁用的舰种列表。
    pub fn forbidden_ship_types(&self) -> &[NamedEquipmentShipType] {
        &self.forbidden_ship_types
    }
}

/// 舰种 ID 及当前客户端显示名称。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedEquipmentShipType {
    ship_type_id: u64,
    name: String,
}

impl NamedEquipmentShipType {
    pub(crate) fn new(ship_type_id: u64, name: String) -> Self {
        Self { ship_type_id, name }
    }

    /// 返回舰种的客户端数值 ID。
    pub const fn ship_type_id(&self) -> u64 {
        self.ship_type_id
    }

    /// 返回舰种的客户端显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 技能引用在装备配置中的可见性。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EquipmentSkillVisibility {
    /// 装备面板中直接展示的技能。
    Visible,
    /// 客户端用于内部效果计算但不直接展示的技能。
    Hidden,
}

/// 装备效果目录关联所需的技能 ID、等级和可见性。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EquipmentSkillReference {
    skill_id: u64,
    level: u32,
    visibility: EquipmentSkillVisibility,
}

impl EquipmentSkillReference {
    pub(crate) const fn new(
        skill_id: u64,
        level: u32,
        visibility: EquipmentSkillVisibility,
    ) -> Self {
        Self {
            skill_id,
            level,
            visibility,
        }
    }

    /// 返回技能配置 ID。
    pub const fn skill_id(self) -> u64 {
        self.skill_id
    }

    /// 返回装备引用所需的技能等级。
    pub const fn level(self) -> u32 {
        self.level
    }

    /// 返回该技能引用的展示可见性。
    pub const fn visibility(self) -> EquipmentSkillVisibility {
        self.visibility
    }
}

/// 当前配置在强化链中的位置、资源成本和可回收资源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentEnhancement {
    level: EnhanceLevel,
    base_config_id: Option<EquipmentConfigId>,
    previous_config_id: Option<EquipmentConfigId>,
    next_config_id: Option<EquipmentConfigId>,
    upgrade_formula_ids: Vec<u64>,
    next_cost: EquipmentResources,
    restore_yield: EquipmentResources,
    destroy_yield: EquipmentResources,
}

impl EquipmentEnhancement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        level: EnhanceLevel,
        base_config_id: Option<EquipmentConfigId>,
        previous_config_id: Option<EquipmentConfigId>,
        next_config_id: Option<EquipmentConfigId>,
        upgrade_formula_ids: Vec<u64>,
        next_cost: EquipmentResources,
        restore_yield: EquipmentResources,
        destroy_yield: EquipmentResources,
    ) -> Self {
        Self {
            level,
            base_config_id,
            previous_config_id,
            next_config_id,
            upgrade_formula_ids,
            next_cost,
            restore_yield,
            destroy_yield,
        }
    }

    /// 返回当前配置在强化链中的展示等级。
    pub const fn level(&self) -> EnhanceLevel {
        self.level
    }

    /// 返回客户端声明的根配置 ID。
    pub const fn base_config_id(&self) -> Option<EquipmentConfigId> {
        self.base_config_id
    }

    /// 返回前一强化配置 ID。
    pub const fn previous_config_id(&self) -> Option<EquipmentConfigId> {
        self.previous_config_id
    }

    /// 返回后一强化配置 ID。
    pub const fn next_config_id(&self) -> Option<EquipmentConfigId> {
        self.next_config_id
    }

    /// 返回客户端声明的升级公式 ID 列表。
    pub fn upgrade_formula_ids(&self) -> &[u64] {
        &self.upgrade_formula_ids
    }

    /// 返回升到下一等级所需的资源。
    pub const fn next_cost(&self) -> &EquipmentResources {
        &self.next_cost
    }

    /// 返回还原配置可获得的资源。
    pub const fn restore_yield(&self) -> &EquipmentResources {
        &self.restore_yield
    }

    /// 返回拆解配置可获得的资源。
    pub const fn destroy_yield(&self) -> &EquipmentResources {
        &self.destroy_yield
    }
}

/// 一组物资和物品数量，用于强化成本及拆解或还原产物。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentResources {
    gold: u64,
    items: Vec<EquipmentItemQuantity>,
}

impl EquipmentResources {
    pub(crate) const fn new(gold: u64, items: Vec<EquipmentItemQuantity>) -> Self {
        Self { gold, items }
    }

    /// 返回资源中的物资数量。
    pub const fn gold(&self) -> u64 {
        self.gold
    }

    /// 返回按物品 ID 严格升序排列的非零数量。
    pub fn items(&self) -> &[EquipmentItemQuantity] {
        &self.items
    }
}

/// 一种强化、拆解或合成资源及其数量。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EquipmentItemQuantity {
    item_id: u64,
    quantity: u64,
}

impl EquipmentItemQuantity {
    pub(crate) const fn new(item_id: u64, quantity: u64) -> Self {
        Self { item_id, quantity }
    }

    /// 返回物品 ID。
    pub const fn item_id(self) -> u64 {
        self.item_id
    }

    /// 返回物品数量。
    pub const fn quantity(self) -> u64 {
        self.quantity
    }
}

/// 与玩家当前持有量无关的一条静态装备合成配方。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentComposeRecipe {
    recipe_id: u64,
    material: EquipmentItemQuantity,
    gold: u64,
    equipment_config_id: EquipmentConfigId,
}

impl EquipmentComposeRecipe {
    pub(crate) const fn new(
        recipe_id: u64,
        material: EquipmentItemQuantity,
        gold: u64,
        equipment_config_id: EquipmentConfigId,
    ) -> Self {
        Self {
            recipe_id,
            material,
            gold,
            equipment_config_id,
        }
    }

    /// 返回合成配方 ID。
    pub const fn recipe_id(&self) -> u64 {
        self.recipe_id
    }

    /// 返回配方要求的材料及数量。
    pub const fn material(&self) -> EquipmentItemQuantity {
        self.material
    }

    /// 返回配方要求的物资数量。
    pub const fn gold(&self) -> u64 {
        self.gold
    }

    /// 返回配方产出的装备配置 ID。
    pub const fn equipment_config_id(&self) -> EquipmentConfigId {
        self.equipment_config_id
    }
}

#[cfg(test)]
mod tests {
    use super::EquipmentDismantleSafety;
    use crate::domain::{EnhanceLevel, EquipmentConfigId};

    #[test]
    fn automatic_dismantle_accepts_only_low_rarity_unenhanced_non_important_equipment() {
        let gray = EquipmentDismantleSafety::new(
            EquipmentConfigId::new(1000).unwrap(),
            0,
            2,
            EnhanceLevel::new(0),
        );
        let blue = EquipmentDismantleSafety::new(
            EquipmentConfigId::new(1009).unwrap(),
            1,
            3,
            EnhanceLevel::new(0),
        );

        assert!(gray.allows_automatic_dismantle());
        assert!(blue.allows_automatic_dismantle());
        assert!(!gray.is_important());
        assert!(!blue.is_protected_variant());
        assert!(!gray.requires_confirmation());
    }

    #[test]
    fn automatic_dismantle_rejects_every_protection_condition() {
        let safe_config = EquipmentConfigId::new(1000).unwrap();
        let important = EquipmentDismantleSafety::new(safe_config, 2, 2, EnhanceLevel::new(0));
        let protected_variant = EquipmentDismantleSafety::new(
            EquipmentConfigId::new(1010).unwrap(),
            0,
            2,
            EnhanceLevel::new(0),
        );
        let protected_variant_end = EquipmentDismantleSafety::new(
            EquipmentConfigId::new(1019).unwrap(),
            0,
            2,
            EnhanceLevel::new(0),
        );
        let next_safe_variant = EquipmentDismantleSafety::new(
            EquipmentConfigId::new(1020).unwrap(),
            0,
            2,
            EnhanceLevel::new(0),
        );
        let purple = EquipmentDismantleSafety::new(safe_config, 0, 4, EnhanceLevel::new(0));
        let enhanced = EquipmentDismantleSafety::new(safe_config, 0, 2, EnhanceLevel::new(1));

        assert!(important.is_important());
        assert!(!important.allows_automatic_dismantle());
        assert!(protected_variant.is_protected_variant());
        assert!(protected_variant.requires_confirmation());
        assert!(!protected_variant.allows_automatic_dismantle());
        assert!(protected_variant_end.is_protected_variant());
        assert!(!next_safe_variant.is_protected_variant());
        assert!(next_safe_variant.allows_automatic_dismantle());
        assert!(purple.requires_rarity_confirmation());
        assert!(purple.requires_confirmation());
        assert!(!purple.allows_automatic_dismantle());
        assert!(enhanced.is_enhanced());
        assert!(enhanced.requires_confirmation());
        assert!(!enhanced.allows_automatic_dismantle());
    }
}

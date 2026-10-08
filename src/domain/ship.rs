//! 规范化的舰船身份、养成、分类、属性和自身技能模型。

use super::{
    EnhanceLevel, EquipmentConfigId, ShipInstanceId, SkillEffectEvidence,
    SkillEffectEvidenceCatalog, SlotIndex,
};

/// 一次完整舰船详情映射使用的模块身份和内容身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipRosterSource {
    module_sha256: String,
    content_sha256: String,
}

impl ShipRosterSource {
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

    /// 返回规范序列化后的船坞、详情与技能证据映射输入 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 按舰船实例 ID 稳定排序的完整舰船名册。
#[derive(Clone, Debug, PartialEq)]
pub struct ShipRoster {
    source: ShipRosterSource,
    ships: Vec<ShipProfile>,
    skill_effects: SkillEffectEvidenceCatalog,
}

impl ShipRoster {
    #[cfg(test)]
    pub(crate) fn new(source: ShipRosterSource, ships: Vec<ShipProfile>) -> Self {
        Self {
            source,
            ships,
            skill_effects: SkillEffectEvidenceCatalog::default(),
        }
    }

    pub(crate) fn new_with_skill_effects(
        source: ShipRosterSource,
        ships: Vec<ShipProfile>,
        skill_effects: SkillEffectEvidenceCatalog,
    ) -> Self {
        Self {
            source,
            ships,
            skill_effects,
        }
    }

    /// 返回生成名册时使用的模块和内容身份。
    pub const fn source(&self) -> &ShipRosterSource {
        &self.source
    }

    /// 返回按舰船实例 ID 严格升序排列的全部舰船。
    pub fn ships(&self) -> &[ShipProfile] {
        &self.ships
    }

    /// 返回舰船数量。
    pub fn len(&self) -> usize {
        self.ships.len()
    }

    /// 返回名册是否为空。
    pub fn is_empty(&self) -> bool {
        self.ships.is_empty()
    }

    /// 返回按实际生效技能 ID 和等级去重的共享效果证据。
    pub const fn skill_effects(&self) -> &SkillEffectEvidenceCatalog {
        &self.skill_effects
    }

    /// 查找一项舰船技能当前等级对应的效果证据。
    pub fn skill_effect(&self, skill_id: u64, level: u32) -> Option<&SkillEffectEvidence> {
        self.skill_effects.evidence(skill_id, level)
    }
}

/// 账号内一艘舰船的运行态身份和当前客户端名称。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipIdentity {
    instance_id: ShipInstanceId,
    config_id: u64,
    name: String,
    create_time: u64,
}

impl ShipIdentity {
    pub(crate) fn new(
        instance_id: ShipInstanceId,
        config_id: u64,
        name: String,
        create_time: u64,
    ) -> Self {
        Self {
            instance_id,
            config_id,
            name,
            create_time,
        }
    }

    /// 返回账号内稳定的舰船实例 ID。
    pub const fn instance_id(&self) -> ShipInstanceId {
        self.instance_id
    }

    /// 返回关联当前舰船静态配置的 ID。
    pub const fn config_id(&self) -> u64 {
        self.config_id
    }

    /// 返回当前客户端解析出的舰船名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回舰船对象保存的获得时间原始值。
    pub const fn create_time(&self) -> u64 {
        self.create_time
    }
}

/// 舰船等级、经验、心情和熟练度等养成状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShipGrowth {
    level: u32,
    max_level: u32,
    experience_in_level: u64,
    total_experience: u64,
    next_level_experience: u64,
    energy: u64,
    proficiency: u64,
}

impl ShipGrowth {
    pub(crate) const fn new(
        level: u32,
        max_level: u32,
        experience_in_level: u64,
        total_experience: u64,
        next_level_experience: u64,
        energy: u64,
        proficiency: u64,
    ) -> Self {
        Self {
            level,
            max_level,
            experience_in_level,
            total_experience,
            next_level_experience,
            energy,
            proficiency,
        }
    }

    /// 返回当前等级。
    pub const fn level(self) -> u32 {
        self.level
    }

    /// 返回当前舰船允许达到的等级上限。
    pub const fn max_level(self) -> u32 {
        self.max_level
    }

    /// 返回当前等级内已经积累的经验。
    pub const fn experience_in_level(self) -> u64 {
        self.experience_in_level
    }

    /// 返回客户端计算的累计经验。
    pub const fn total_experience(self) -> u64 {
        self.total_experience
    }

    /// 返回升到下一级所需经验；满级时为零。
    pub const fn next_level_experience(self) -> u64 {
        self.next_level_experience
    }

    /// 返回舰船实例保存的当前心情值。
    pub const fn energy(self) -> u64 {
        self.energy
    }

    /// 返回舰船实例保存的熟练度原始值。
    pub const fn proficiency(self) -> u64 {
        self.proficiency
    }
}

/// 舰船当前好感、客户端阶段说明、上限和誓约状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipIntimacy {
    raw_hundredths: u64,
    maximum: u64,
    stage_id: u64,
    stage_description: String,
    proposed: bool,
    propose_time: u64,
}

impl ShipIntimacy {
    pub(crate) fn new(
        raw_hundredths: u64,
        maximum: u64,
        stage_id: u64,
        stage_description: String,
        proposed: bool,
        propose_time: u64,
    ) -> Self {
        Self {
            raw_hundredths,
            maximum,
            stage_id,
            stage_description,
            proposed,
            propose_time,
        }
    }

    /// 返回客户端以百分之一为单位保存的原始好感值。
    pub const fn raw_hundredths(&self) -> u64 {
        self.raw_hundredths
    }

    /// 返回客户端当前规则计算的好感显示上限，不使用百分之一原始单位。
    pub const fn maximum(&self) -> u64 {
        self.maximum
    }

    /// 返回客户端 `getIntimacyLevel` 使用的阶段配置 ID。
    pub const fn stage_id(&self) -> u64 {
        self.stage_id
    }

    /// 返回客户端阶段配置提供的本地化说明。
    pub fn stage_description(&self) -> &str {
        &self.stage_description
    }

    /// 返回舰船是否已经完成誓约。
    pub const fn proposed(&self) -> bool {
        self.proposed
    }

    /// 返回舰船对象保存的誓约时间原始值。
    pub const fn propose_time(&self) -> u64 {
        self.propose_time
    }
}

/// 客户端持久编队的业务种类。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ShipFleetKind {
    /// 普通水面编队。
    Regular,
    /// 潜艇编队。
    Submarine,
    /// 演习防守编队。
    Exercise,
}

impl ShipFleetKind {
    /// 返回协议和摘要使用的稳定英文键。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Submarine => "submarine",
            Self::Exercise => "exercise",
        }
    }

    /// 返回用于工作簿和日志的稳定中文标签。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Regular => "常规",
            Self::Submarine => "潜艇",
            Self::Exercise => "演习",
        }
    }
}

/// 客户端编队内的队伍种类。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ShipFleetTeam {
    /// 主力队伍。
    Main,
    /// 先锋队伍。
    Vanguard,
    /// 潜艇队伍。
    Submarine,
}

impl ShipFleetTeam {
    /// 返回协议和摘要使用的稳定英文键。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Vanguard => "vanguard",
            Self::Submarine => "submarine",
        }
    }

    /// 返回用于工作簿的稳定中文标签。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Main => "主力",
            Self::Vanguard => "先锋",
            Self::Submarine => "潜艇",
        }
    }
}

/// 一艘舰船在一个客户端持久编队中的队伍和位置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipFleetMembership {
    fleet_id: u32,
    display_name: Option<String>,
    kind: ShipFleetKind,
    team: ShipFleetTeam,
    position: u32,
}

impl ShipFleetMembership {
    pub(crate) fn new(
        fleet_id: u32,
        display_name: Option<String>,
        kind: ShipFleetKind,
        team: ShipFleetTeam,
        position: u32,
    ) -> Self {
        Self {
            fleet_id,
            display_name,
            kind,
            team,
            position,
        }
    }

    /// 返回 FleetProxy 使用的编队 ID。
    pub const fn fleet_id(&self) -> u32 {
        self.fleet_id
    }

    /// 返回客户端自定义或默认本地化名称。
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// 返回编队业务种类。
    pub const fn kind(&self) -> ShipFleetKind {
        self.kind
    }

    /// 返回舰船所属队伍。
    pub const fn team(&self) -> ShipFleetTeam {
        self.team
    }

    /// 返回队伍内一至三号位置。
    pub const fn position(&self) -> u32 {
        self.position
    }
}

/// 一个带当前客户端显示名称的正整数分类。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedShipClass {
    id: u64,
    name: String,
}

impl NamedShipClass {
    pub(crate) fn new(id: u64, name: String) -> Self {
        Self { id, name }
    }

    /// 返回客户端分类 ID。
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// 返回当前客户端显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 舰船当前星级和能够达到的最大星级。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShipStars {
    current: u32,
    maximum: u32,
}

impl ShipStars {
    pub(crate) const fn new(current: u32, maximum: u32) -> Self {
        Self { current, maximum }
    }

    /// 返回当前星级。
    pub const fn current(self) -> u32 {
        self.current
    }

    /// 返回最大星级。
    pub const fn maximum(self) -> u32 {
        self.maximum
    }
}

/// 舰种、装甲、阵营、稀有度、星级和皮肤分类。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipClassification {
    group_id: u64,
    ship_type: NamedShipClass,
    armor_type: NamedShipClass,
    nation: NamedShipClass,
    rarity: u32,
    stars: ShipStars,
    skin_id: u64,
}

impl ShipClassification {
    pub(crate) fn new(
        group_id: u64,
        ship_type: NamedShipClass,
        armor_type: NamedShipClass,
        nation: NamedShipClass,
        rarity: u32,
        stars: ShipStars,
        skin_id: u64,
    ) -> Self {
        Self {
            group_id,
            ship_type,
            armor_type,
            nation,
            rarity,
            stars,
            skin_id,
        }
    }

    /// 返回跨改造配置归组时使用的舰船组 ID。
    pub const fn group_id(&self) -> u64 {
        self.group_id
    }

    /// 返回舰种分类。
    pub const fn ship_type(&self) -> &NamedShipClass {
        &self.ship_type
    }

    /// 返回装甲分类。
    pub const fn armor_type(&self) -> &NamedShipClass {
        &self.armor_type
    }

    /// 返回阵营分类。
    pub const fn nation(&self) -> &NamedShipClass {
        &self.nation
    }

    /// 返回客户端稀有度数值。
    pub const fn rarity(&self) -> u32 {
        self.rarity
    }

    /// 返回当前与最大星级。
    pub const fn stars(&self) -> ShipStars {
        self.stars
    }

    /// 返回当前生效皮肤配置 ID。
    pub const fn skin_id(&self) -> u64 {
        self.skin_id
    }
}

/// 客户端当前冻结的十一项舰船面板属性。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipAttributeValues {
    durability: f64,
    cannon: f64,
    torpedo: f64,
    anti_aircraft: f64,
    air: f64,
    reload: f64,
    hit: f64,
    dodge: f64,
    anti_sub: f64,
    luck: f64,
    speed: f64,
}

impl ShipAttributeValues {
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new(
        durability: f64,
        cannon: f64,
        torpedo: f64,
        anti_aircraft: f64,
        air: f64,
        reload: f64,
        hit: f64,
        dodge: f64,
        anti_sub: f64,
        luck: f64,
        speed: f64,
    ) -> Self {
        Self {
            durability,
            cannon,
            torpedo,
            anti_aircraft,
            air,
            reload,
            hit,
            dodge,
            anti_sub,
            luck,
            speed,
        }
    }

    pub(crate) const fn subtract(self, earlier: Self) -> Self {
        Self {
            durability: self.durability - earlier.durability,
            cannon: self.cannon - earlier.cannon,
            torpedo: self.torpedo - earlier.torpedo,
            anti_aircraft: self.anti_aircraft - earlier.anti_aircraft,
            air: self.air - earlier.air,
            reload: self.reload - earlier.reload,
            hit: self.hit - earlier.hit,
            dodge: self.dodge - earlier.dodge,
            anti_sub: self.anti_sub - earlier.anti_sub,
            luck: self.luck - earlier.luck,
            speed: self.speed - earlier.speed,
        }
    }

    /// 返回耐久值。
    pub const fn durability(self) -> f64 {
        self.durability
    }

    /// 返回炮击值。
    pub const fn cannon(self) -> f64 {
        self.cannon
    }

    /// 返回雷击值。
    pub const fn torpedo(self) -> f64 {
        self.torpedo
    }

    /// 返回防空值。
    pub const fn anti_aircraft(self) -> f64 {
        self.anti_aircraft
    }

    /// 返回航空值。
    pub const fn air(self) -> f64 {
        self.air
    }

    /// 返回装填值。
    pub const fn reload(self) -> f64 {
        self.reload
    }

    /// 返回命中值。
    pub const fn hit(self) -> f64 {
        self.hit
    }

    /// 返回机动值。
    pub const fn dodge(self) -> f64 {
        self.dodge
    }

    /// 返回反潜值。
    pub const fn anti_sub(self) -> f64 {
        self.anti_sub
    }

    /// 返回幸运值。
    pub const fn luck(self) -> f64 {
        self.luck
    }

    /// 返回航速值；当前客户端允许合法小数。
    pub const fn speed(self) -> f64 {
        self.speed
    }
}

/// 三个可验证属性阶段及其相邻差额。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipAttributeBreakdown {
    base: ShipAttributeValues,
    equipment_applied: ShipAttributeValues,
    effective: ShipAttributeValues,
    equipment_delta: ShipAttributeValues,
    global_delta: ShipAttributeValues,
}

impl ShipAttributeBreakdown {
    pub(crate) const fn new(
        base: ShipAttributeValues,
        equipment_applied: ShipAttributeValues,
        effective: ShipAttributeValues,
    ) -> Self {
        Self {
            base,
            equipment_applied,
            effective,
            equipment_delta: equipment_applied.subtract(base),
            global_delta: effective.subtract(equipment_applied),
        }
    }

    /// 返回不含装备和全局加成的基础属性。
    pub const fn base(self) -> ShipAttributeValues {
        self.base
    }

    /// 返回装备结算完成、全局加成结算前的绝对属性。
    pub const fn equipment_applied(self) -> ShipAttributeValues {
        self.equipment_applied
    }

    /// 返回当前客户端最终生效的绝对属性。
    pub const fn effective(self) -> ShipAttributeValues {
        self.effective
    }

    /// 返回装备阶段相对于基础阶段的差额；装备减益时可以为负数。
    pub const fn equipment_delta(self) -> ShipAttributeValues {
        self.equipment_delta
    }

    /// 返回最终阶段相对于装备阶段的差额。
    ///
    /// 当前客户端证据还不能把这部分稳定拆成舰队科技和其他来源，因此这里不作更细归因。
    pub const fn global_delta(self) -> ShipAttributeValues {
        self.global_delta
    }
}

/// 客户端开始战斗、结束战斗和总油耗的计算结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShipOilCost {
    start: u64,
    end: u64,
    total: u64,
}

impl ShipOilCost {
    pub(crate) const fn new(start: u64, end: u64, total: u64) -> Self {
        Self { start, end, total }
    }

    /// 返回开始战斗时的油耗。
    pub const fn start(self) -> u64 {
        self.start
    }

    /// 返回结束战斗时的油耗。
    pub const fn end(self) -> u64 {
        self.end
    }

    /// 返回客户端计算的总油耗。
    pub const fn total(self) -> u64 {
        self.total
    }
}

/// 舰船综合性能、锁定、油耗和属性状态。
#[derive(Clone, Debug, PartialEq)]
pub struct ShipPerformance {
    combat_power: u64,
    locked: bool,
    oil_cost: ShipOilCost,
    attributes: ShipAttributeBreakdown,
}

impl ShipPerformance {
    pub(crate) const fn new(
        combat_power: u64,
        locked: bool,
        oil_cost: ShipOilCost,
        attributes: ShipAttributeBreakdown,
    ) -> Self {
        Self {
            combat_power,
            locked,
            oil_cost,
            attributes,
        }
    }

    /// 返回客户端计算的综合性能。
    pub const fn combat_power(&self) -> u64 {
        self.combat_power
    }

    /// 返回舰船是否锁定。
    pub const fn locked(&self) -> bool {
        self.locked
    }

    /// 返回三种油耗计算结果。
    pub const fn oil_cost(&self) -> ShipOilCost {
        self.oil_cost
    }

    /// 返回三个属性阶段及其差额。
    pub const fn attributes(&self) -> ShipAttributeBreakdown {
        self.attributes
    }
}

/// 自身技能的原始身份和当前生效展示身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipSkillIdentity {
    skill_id: u64,
    effective_skill_id: u64,
    name: String,
}

impl ShipSkillIdentity {
    pub(crate) fn new(skill_id: u64, effective_skill_id: u64, name: String) -> Self {
        Self {
            skill_id,
            effective_skill_id,
            name,
        }
    }

    /// 返回 `Ship.skills` 使用的原始技能 ID。
    pub const fn skill_id(&self) -> u64 {
        self.skill_id
    }

    /// 返回当前等级实际展示的生效技能 ID。
    pub const fn effective_skill_id(&self) -> u64 {
        self.effective_skill_id
    }

    /// 返回技能显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// 自身技能当前等级、等级上限和经验状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShipSkillProgress {
    level: u32,
    max_level: u32,
    experience: u64,
    next_level_experience: u64,
}

impl ShipSkillProgress {
    pub(crate) const fn new(
        level: u32,
        max_level: u32,
        experience: u64,
        next_level_experience: u64,
    ) -> Self {
        Self {
            level,
            max_level,
            experience,
            next_level_experience,
        }
    }

    /// 返回当前技能等级。
    pub const fn level(self) -> u32 {
        self.level
    }

    /// 返回技能等级上限。
    pub const fn max_level(self) -> u32 {
        self.max_level
    }

    /// 返回当前等级内技能经验。
    pub const fn experience(self) -> u64 {
        self.experience
    }

    /// 返回升级所需经验；满级时为零。
    pub const fn next_level_experience(self) -> u64 {
        self.next_level_experience
    }
}

/// 一条只来自 `Ship.skills` 的已学习自身技能。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipSkill {
    identity: ShipSkillIdentity,
    progress: ShipSkillProgress,
    description_template: String,
    current_effect: String,
}

impl ShipSkill {
    pub(crate) fn new(
        identity: ShipSkillIdentity,
        progress: ShipSkillProgress,
        description_template: String,
        current_effect: String,
    ) -> Self {
        Self {
            identity,
            progress,
            description_template,
            current_effect,
        }
    }

    /// 返回技能原始身份和当前展示身份。
    pub const fn identity(&self) -> &ShipSkillIdentity {
        &self.identity
    }

    /// 返回技能等级和经验状态。
    pub const fn progress(&self) -> ShipSkillProgress {
        self.progress
    }

    /// 返回静态配置中的技能描述模板。
    pub fn description_template(&self) -> &str {
        &self.description_template
    }

    /// 返回当前等级参数已经代入后的效果文本。
    pub fn current_effect(&self) -> &str {
        &self.current_effect
    }
}

/// 舰船槽位中当前装备的运行态身份和配置状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShipEquipment {
    runtime_id: u64,
    config_id: EquipmentConfigId,
    enhance_level: EnhanceLevel,
}

impl ShipEquipment {
    pub(crate) const fn new(
        runtime_id: u64,
        config_id: EquipmentConfigId,
        enhance_level: EnhanceLevel,
    ) -> Self {
        Self {
            runtime_id,
            config_id,
            enhance_level,
        }
    }

    /// 返回当次运行态中的装备 ID；该值不作为跨位置持久身份。
    pub const fn runtime_id(self) -> u64 {
        self.runtime_id
    }

    /// 返回当前具体强化配置 ID。
    pub const fn config_id(self) -> EquipmentConfigId {
        self.config_id
    }

    /// 返回界面显示的实际强化等级。
    pub const fn enhance_level(self) -> EnhanceLevel {
        self.enhance_level
    }
}

/// 舰船的一个固定装备槽位。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShipEquipmentSlot {
    index: SlotIndex,
    equipment: Option<ShipEquipment>,
    allowed_equipment_type_ids: Vec<u64>,
}

impl ShipEquipmentSlot {
    pub(crate) fn new(
        index: SlotIndex,
        equipment: Option<ShipEquipment>,
        allowed_equipment_type_ids: Vec<u64>,
    ) -> Self {
        Self {
            index,
            equipment,
            allowed_equipment_type_ids,
        }
    }

    /// 返回 1 至 5 的槽位编号。
    pub const fn index(&self) -> SlotIndex {
        self.index
    }

    /// 返回当前槽位装备；空槽保持为空。
    pub const fn equipment(&self) -> Option<ShipEquipment> {
        self.equipment
    }

    /// 返回当前突破阶段允许的装备类型 ID，集合严格升序且不重复。
    pub fn allowed_equipment_type_ids(&self) -> &[u64] {
        &self.allowed_equipment_type_ids
    }

    /// 替换槽位中的当前装备，同时保留客户端给出的槽位规则。
    #[cfg(test)]
    pub(crate) fn with_equipment(&self, equipment: Option<ShipEquipment>) -> Self {
        Self {
            index: self.index,
            equipment,
            allowed_equipment_type_ids: self.allowed_equipment_type_ids.clone(),
        }
    }
}

/// 一艘真实舰船的规范化只读详情。
#[derive(Clone, Debug, PartialEq)]
pub struct ShipProfile {
    identity: ShipIdentity,
    growth: ShipGrowth,
    intimacy: ShipIntimacy,
    fleet_memberships: Vec<ShipFleetMembership>,
    classification: ShipClassification,
    performance: ShipPerformance,
    skills: Vec<ShipSkill>,
    slots: [ShipEquipmentSlot; 5],
}

impl ShipProfile {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: ShipIdentity,
        growth: ShipGrowth,
        intimacy: ShipIntimacy,
        fleet_memberships: Vec<ShipFleetMembership>,
        classification: ShipClassification,
        performance: ShipPerformance,
        skills: Vec<ShipSkill>,
        slots: [ShipEquipmentSlot; 5],
    ) -> Self {
        Self {
            identity,
            growth,
            intimacy,
            fleet_memberships,
            classification,
            performance,
            skills,
            slots,
        }
    }

    /// 返回舰船运行态身份和名称。
    pub const fn identity(&self) -> &ShipIdentity {
        &self.identity
    }

    /// 返回等级、经验、心情和熟练度状态。
    pub const fn growth(&self) -> ShipGrowth {
        self.growth
    }

    /// 返回好感和誓约状态。
    pub const fn intimacy(&self) -> &ShipIntimacy {
        &self.intimacy
    }

    /// 返回按编队 ID、队伍和位置排序的全部持久编队关系。
    pub fn fleet_memberships(&self) -> &[ShipFleetMembership] {
        &self.fleet_memberships
    }

    /// 返回舰种、装甲、阵营、稀有度和星级分类。
    pub const fn classification(&self) -> &ShipClassification {
        &self.classification
    }

    /// 返回综合性能、锁定、油耗和属性状态。
    pub const fn performance(&self) -> &ShipPerformance {
        &self.performance
    }

    /// 返回按原始技能 ID 严格升序排列的自身技能。
    pub fn skills(&self) -> &[ShipSkill] {
        &self.skills
    }

    /// 返回按 1 至 5 固定排列的装备槽位。
    pub const fn slots(&self) -> &[ShipEquipmentSlot; 5] {
        &self.slots
    }
}

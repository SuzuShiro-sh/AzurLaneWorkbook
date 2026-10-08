//! 规范化装备武器参数、技能效果及其递归参数值。

/// 按武器 ID 和技能键严格排序的装备详情目录。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentDetailCatalog {
    weapons: Vec<EquipmentWeapon>,
    skills: Vec<EquipmentSkillDetail>,
}

impl EquipmentDetailCatalog {
    pub(crate) fn new(weapons: Vec<EquipmentWeapon>, skills: Vec<EquipmentSkillDetail>) -> Self {
        Self { weapons, skills }
    }

    /// 返回按武器 ID 严格升序排列的全部武器参数。
    pub fn weapons(&self) -> &[EquipmentWeapon] {
        &self.weapons
    }

    /// 返回按技能 ID 和等级严格升序排列的全部装备技能。
    pub fn skills(&self) -> &[EquipmentSkillDetail] {
        &self.skills
    }

    /// 按武器 ID 查找参数。
    pub fn weapon(&self, weapon_id: u64) -> Option<&EquipmentWeapon> {
        self.weapons
            .binary_search_by_key(&weapon_id, EquipmentWeapon::weapon_id)
            .ok()
            .map(|index| &self.weapons[index])
    }

    /// 按技能 ID 和等级查找效果。
    pub fn skill(&self, skill_id: u64, level: u32) -> Option<&EquipmentSkillDetail> {
        self.skills
            .binary_search_by_key(&(skill_id, level), EquipmentSkillDetail::key)
            .ok()
            .map(|index| &self.skills[index])
    }
}

/// 武器锁定阶段参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WeaponChargeParameter {
    /// 客户端以空字符串表示未配置锁定阶段。
    Empty,
    /// 锁定等待时间和最大锁定目标数。
    Lock { lock_time: f64, max_lock: u64 },
}

/// 武器预施法参数。
#[derive(Clone, Debug, PartialEq)]
pub enum WeaponPrecastParameter {
    /// 当前客户端返回的数值序列；现有样本为空序列。
    Values(Vec<f64>),
    /// 旧配置以单个空白字符串表示未配置。
    LegacyWhitespace,
}

/// 一条完整物化并校验过的武器参数。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentWeapon {
    weapon_id: u64,
    base_weapon_id: Option<u64>,
    action_index: String,
    aim_type: u64,
    angle: u64,
    attack_attribute: u64,
    attack_attribute_ratio: u64,
    auto_aftercast: f64,
    axis_angle: u64,
    barrage_ids: Vec<u64>,
    bullet_ids: Vec<u64>,
    charge_parameter: WeaponChargeParameter,
    corrected: u64,
    damage: u64,
    effect_move: u64,
    expose: u64,
    fire_fx: String,
    fire_fx_loop_type: u64,
    fire_sfx: String,
    initial_over_heat: u64,
    min_range: u64,
    oxygen_types: Vec<u64>,
    precast_parameter: WeaponPrecastParameter,
    queue: u64,
    range: u64,
    recover_time: f64,
    reload_max: u64,
    search_conditions: Vec<u64>,
    search_type: u64,
    shakescreen: u64,
    spawn_bound: String,
    suppress: u64,
    torpedo_ammo: u64,
    weapon_type: u64,
}

impl EquipmentWeapon {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        weapon_id: u64,
        base_weapon_id: Option<u64>,
        action_index: String,
        aim_type: u64,
        angle: u64,
        attack_attribute: u64,
        attack_attribute_ratio: u64,
        auto_aftercast: f64,
        axis_angle: u64,
        barrage_ids: Vec<u64>,
        bullet_ids: Vec<u64>,
        charge_parameter: WeaponChargeParameter,
        corrected: u64,
        damage: u64,
        effect_move: u64,
        expose: u64,
        fire_fx: String,
        fire_fx_loop_type: u64,
        fire_sfx: String,
        initial_over_heat: u64,
        min_range: u64,
        oxygen_types: Vec<u64>,
        precast_parameter: WeaponPrecastParameter,
        queue: u64,
        range: u64,
        recover_time: f64,
        reload_max: u64,
        search_conditions: Vec<u64>,
        search_type: u64,
        shakescreen: u64,
        spawn_bound: String,
        suppress: u64,
        torpedo_ammo: u64,
        weapon_type: u64,
    ) -> Self {
        Self {
            weapon_id,
            base_weapon_id,
            action_index,
            aim_type,
            angle,
            attack_attribute,
            attack_attribute_ratio,
            auto_aftercast,
            axis_angle,
            barrage_ids,
            bullet_ids,
            charge_parameter,
            corrected,
            damage,
            effect_move,
            expose,
            fire_fx,
            fire_fx_loop_type,
            fire_sfx,
            initial_over_heat,
            min_range,
            oxygen_types,
            precast_parameter,
            queue,
            range,
            recover_time,
            reload_max,
            search_conditions,
            search_type,
            shakescreen,
            spawn_bound,
            suppress,
            torpedo_ammo,
            weapon_type,
        }
    }

    /// 返回武器配置 ID。
    pub const fn weapon_id(&self) -> u64 {
        self.weapon_id
    }

    /// 返回继承来源武器 ID；根武器保持为空。
    pub const fn base_weapon_id(&self) -> Option<u64> {
        self.base_weapon_id
    }

    /// 返回客户端攻击动作键。
    pub fn action_index(&self) -> &str {
        &self.action_index
    }

    /// 返回瞄准类型。
    pub const fn aim_type(&self) -> u64 {
        self.aim_type
    }

    /// 返回武器射界角度。
    pub const fn angle(&self) -> u64 {
        self.angle
    }

    /// 返回伤害属性类型。
    pub const fn attack_attribute(&self) -> u64 {
        self.attack_attribute
    }

    /// 返回伤害属性比例。
    pub const fn attack_attribute_ratio(&self) -> u64 {
        self.attack_attribute_ratio
    }

    /// 返回攻击后的自动收尾等待时间。
    pub const fn auto_aftercast(&self) -> f64 {
        self.auto_aftercast
    }

    /// 返回轴向角度。
    pub const fn axis_angle(&self) -> u64 {
        self.axis_angle
    }

    /// 返回齐射模板 ID 列表。
    pub fn barrage_ids(&self) -> &[u64] {
        &self.barrage_ids
    }

    /// 返回子弹模板 ID 列表；不发射子弹的武器允许为空。
    pub fn bullet_ids(&self) -> &[u64] {
        &self.bullet_ids
    }

    /// 返回锁定阶段参数。
    pub const fn charge_parameter(&self) -> WeaponChargeParameter {
        self.charge_parameter
    }

    /// 返回客户端伤害修正值。
    pub const fn corrected(&self) -> u64 {
        self.corrected
    }

    /// 返回单段基础伤害值。
    pub const fn damage(&self) -> u64 {
        self.damage
    }

    /// 返回武器移动效果标记。
    pub const fn effect_move(&self) -> u64 {
        self.effect_move
    }

    /// 返回暴露标记。
    pub const fn expose(&self) -> u64 {
        self.expose
    }

    /// 返回开火视觉效果键。
    pub fn fire_fx(&self) -> &str {
        &self.fire_fx
    }

    /// 返回开火视觉效果循环类型。
    pub const fn fire_fx_loop_type(&self) -> u64 {
        self.fire_fx_loop_type
    }

    /// 返回开火音效键。
    pub fn fire_sfx(&self) -> &str {
        &self.fire_sfx
    }

    /// 返回初始过热标记。
    pub const fn initial_over_heat(&self) -> u64 {
        self.initial_over_heat
    }

    /// 返回最小射程。
    pub const fn min_range(&self) -> u64 {
        self.min_range
    }

    /// 返回氧气环境类型列表。
    pub fn oxygen_types(&self) -> &[u64] {
        &self.oxygen_types
    }

    /// 返回预施法参数。
    pub const fn precast_parameter(&self) -> &WeaponPrecastParameter {
        &self.precast_parameter
    }

    /// 返回攻击队列优先级。
    pub const fn queue(&self) -> u64 {
        self.queue
    }

    /// 返回最大射程。
    pub const fn range(&self) -> u64 {
        self.range
    }

    /// 返回攻击恢复时间。
    pub const fn recover_time(&self) -> f64 {
        self.recover_time
    }

    /// 返回客户端武器装填基值。
    pub const fn reload_max(&self) -> u64 {
        self.reload_max
    }

    /// 返回目标搜索条件列表。
    pub fn search_conditions(&self) -> &[u64] {
        &self.search_conditions
    }

    /// 返回目标搜索类型。
    pub const fn search_type(&self) -> u64 {
        self.search_type
    }

    /// 返回屏幕震动效果 ID。
    pub const fn shakescreen(&self) -> u64 {
        self.shakescreen
    }

    /// 返回发射挂点键。
    pub fn spawn_bound(&self) -> &str {
        &self.spawn_bound
    }

    /// 返回压制标记。
    pub const fn suppress(&self) -> u64 {
        self.suppress
    }

    /// 返回鱼雷弹药数量字段。
    pub const fn torpedo_ammo(&self) -> u64 {
        self.torpedo_ammo
    }

    /// 返回武器类型 ID。
    pub const fn weapon_type(&self) -> u64 {
        self.weapon_type
    }
}

/// 装备技能展示配置。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentSkillDisplay {
    name: String,
    description: String,
    acquire_description: String,
    system_transform: SkillValue,
}

impl EquipmentSkillDisplay {
    pub(crate) fn new(
        name: String,
        description: String,
        acquire_description: String,
        system_transform: SkillValue,
    ) -> Self {
        Self {
            name,
            description,
            acquire_description,
            system_transform,
        }
    }

    /// 返回技能显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回技能描述。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 返回获得技能时使用的说明。
    pub fn acquire_description(&self) -> &str {
        &self.acquire_description
    }

    /// 返回客户端系统变换参数。
    pub const fn system_transform(&self) -> &SkillValue {
        &self.system_transform
    }
}

/// 战斗技能或战斗 Buff 的规范化效果来源。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentSkillSource {
    config_id: u64,
    name: String,
    description: String,
    cooldown: Option<f64>,
    duration: Option<f64>,
    stack: Option<u64>,
    effects: Vec<EquipmentSkillEffect>,
}

impl EquipmentSkillSource {
    pub(crate) fn new(
        config_id: u64,
        name: String,
        description: String,
        cooldown: Option<f64>,
        duration: Option<f64>,
        stack: Option<u64>,
        effects: Vec<EquipmentSkillEffect>,
    ) -> Self {
        Self {
            config_id,
            name,
            description,
            cooldown,
            duration,
            stack,
            effects,
        }
    }

    /// 返回战斗技能或战斗 Buff 正文使用的实际配置 ID。
    pub const fn config_id(&self) -> u64 {
        self.config_id
    }

    /// 返回当前来源的显示名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回当前来源的描述。
    pub fn description(&self) -> &str {
        &self.description
    }

    /// 返回战斗技能冷却时间。
    pub const fn cooldown(&self) -> Option<f64> {
        self.cooldown
    }

    /// 返回战斗 Buff 持续时间。
    pub const fn duration(&self) -> Option<f64> {
        self.duration
    }

    /// 返回战斗 Buff 叠加数量。
    pub const fn stack(&self) -> Option<u64> {
        self.stack
    }

    /// 返回按客户端声明顺序保存的完整效果列表。
    pub fn effects(&self) -> &[EquipmentSkillEffect] {
        &self.effects
    }
}

/// 指定技能 ID 和等级的展示信息与战斗效果。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentSkillDetail {
    skill_id: u64,
    level: u32,
    display: EquipmentSkillDisplay,
    battle_skill: Option<EquipmentSkillSource>,
    battle_buff: Option<EquipmentSkillSource>,
}

impl EquipmentSkillDetail {
    pub(crate) fn new(
        skill_id: u64,
        level: u32,
        display: EquipmentSkillDisplay,
        battle_skill: Option<EquipmentSkillSource>,
        battle_buff: Option<EquipmentSkillSource>,
    ) -> Self {
        Self {
            skill_id,
            level,
            display,
            battle_skill,
            battle_buff,
        }
    }

    pub(crate) const fn key(&self) -> (u64, u32) {
        (self.skill_id, self.level)
    }

    /// 返回技能配置 ID。
    pub const fn skill_id(&self) -> u64 {
        self.skill_id
    }

    /// 返回装备配置明确引用的技能等级。
    pub const fn level(&self) -> u32 {
        self.level
    }

    /// 返回展示配置。
    pub const fn display(&self) -> &EquipmentSkillDisplay {
        &self.display
    }

    /// 返回战斗技能效果；客户端没有对应配置时为空。
    pub const fn battle_skill(&self) -> Option<&EquipmentSkillSource> {
        self.battle_skill.as_ref()
    }

    /// 返回战斗 Buff 效果。
    pub const fn battle_buff(&self) -> Option<&EquipmentSkillSource> {
        self.battle_buff.as_ref()
    }
}

/// 一条装备技能效果。
#[derive(Clone, Debug, PartialEq)]
pub struct EquipmentSkillEffect {
    sequence: u32,
    effect_type: String,
    target_choices: Vec<String>,
    triggers: Vec<String>,
    arguments: Vec<SkillEffectArgument>,
    metadata: Vec<SkillValueField>,
}

impl EquipmentSkillEffect {
    pub(crate) fn new(
        sequence: u32,
        effect_type: String,
        target_choices: Vec<String>,
        triggers: Vec<String>,
        arguments: Vec<SkillEffectArgument>,
        metadata: Vec<SkillValueField>,
    ) -> Self {
        Self {
            sequence,
            effect_type,
            target_choices,
            triggers,
            arguments,
            metadata,
        }
    }

    /// 返回效果在来源数组中的一基序号。
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// 返回客户端效果类型键。
    pub fn effect_type(&self) -> &str {
        &self.effect_type
    }

    /// 返回规范化为列表的目标选择器。
    pub fn target_choices(&self) -> &[String] {
        &self.target_choices
    }

    /// 返回触发条件列表。
    pub fn triggers(&self) -> &[String] {
        &self.triggers
    }

    /// 返回按参数名严格升序排列的动态效果参数。
    pub fn arguments(&self) -> &[SkillEffectArgument] {
        &self.arguments
    }

    /// 返回动画等不属于触发、目标或参数的有名效果字段。
    pub fn metadata(&self) -> &[SkillValueField] {
        &self.metadata
    }
}

/// 一项带稳定名称的动态技能参数。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillEffectArgument {
    name: String,
    value: SkillValue,
}

impl SkillEffectArgument {
    pub(crate) fn new(name: String, value: SkillValue) -> Self {
        Self { name, value }
    }

    /// 返回参数名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回参数值。
    pub const fn value(&self) -> &SkillValue {
        &self.value
    }
}

/// 一个普通对象中的稳定字段。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillValueField {
    name: String,
    value: SkillValue,
}

impl SkillValueField {
    pub(crate) fn new(name: String, value: SkillValue) -> Self {
        Self { name, value }
    }

    /// 返回字段名。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回字段值。
    pub const fn value(&self) -> &SkillValue {
        &self.value
    }
}

/// Lua 混合表条目使用的原始键类型。
#[derive(Clone, Debug, PartialEq)]
pub enum SkillTableKey {
    /// Lua 数值键。
    Number(f64),
    /// Lua 字符串键。
    String(String),
}

/// Lua 混合表中的一条有序记录。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillTableEntry {
    key: SkillTableKey,
    value: SkillValue,
}

impl SkillTableEntry {
    pub(crate) fn new(key: SkillTableKey, value: SkillValue) -> Self {
        Self { key, value }
    }

    /// 返回保留原始类型的 Lua 表键。
    pub const fn key(&self) -> &SkillTableKey {
        &self.key
    }

    /// 返回条目值。
    pub const fn value(&self) -> &SkillValue {
        &self.value
    }
}

/// 完整读取的 Lua 混合表。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillMixedTable {
    entries: Vec<SkillTableEntry>,
    reason: Option<String>,
}

impl SkillMixedTable {
    pub(crate) fn new(entries: Vec<SkillTableEntry>, reason: Option<String>) -> Self {
        Self { entries, reason }
    }

    /// 返回保持原始键类型和顺序的表条目。
    pub fn entries(&self) -> &[SkillTableEntry] {
        &self.entries
    }

    /// 返回原生读取器提供的补充原因；完整样本通常为空。
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// 技能参数允许出现的递归值，不依赖 JSON 或设备 DTO。
#[derive(Clone, Debug, PartialEq)]
pub enum SkillValue {
    /// Lua nil 或 JSON null。
    Null,
    /// 布尔值。
    Bool(bool),
    /// 有限数值。
    Number(f64),
    /// 字符串。
    String(String),
    /// 保持元素顺序的列表。
    List(Vec<SkillValue>),
    /// 按字段名严格升序排列的普通对象。
    Object(Vec<SkillValueField>),
    /// 同时含数值键和字符串键的 Lua 表。
    MixedTable(SkillMixedTable),
}

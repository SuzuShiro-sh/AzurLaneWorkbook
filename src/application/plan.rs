//! 把已读取的游戏状态和已校验的配装目标编译为只读检查计划。

use serde::Serialize;
use thiserror::Error;

use crate::domain::{
    DesiredState, EnhanceLevel, EquipmentConfigId, EquipmentFamilyId, EquipmentInventoryPlan,
    EquipmentSourceRef, GameState, ShipInstanceId, ShipSlotRef, SlotTarget,
};

mod compiler;
mod digest;
pub(crate) mod workbook;

use compiler::compile_plan_only;

/// 只读配装计划的稳定契约版本。
pub const PLAN_SCHEMA_VERSION: u32 = 5;

/// 计划中可引用的仓库、物资和背包资源键。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[non_exhaustive]
pub enum ResourceKey {
    /// 仓库中按具体强化配置聚合的装备数量。
    WarehouseEquipment { config_id: u64 },
    /// 当前账号持有的物资。
    Gold,
    /// 背包中按物品 ID 聚合的材料数量。
    Item { item_id: u64 },
}

/// 一条资源约束及其核算结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ResourceConstraint {
    key: ResourceKey,
    available: u64,
    required: u64,
    remaining: u64,
}

impl ResourceConstraint {
    /// 返回资源键。
    pub const fn key(self) -> ResourceKey {
        self.key
    }

    /// 返回完整状态中可用的数量。
    pub const fn available(self) -> u64 {
        self.available
    }

    /// 返回当前计划需要的数量。
    pub const fn required(self) -> u64 {
        self.required
    }

    /// 返回执行这些移动后该资源桶的剩余数量。
    pub const fn remaining(self) -> u64 {
        self.remaining
    }
}

/// 计划对已核算资源桶产生的变化。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ResourceChange {
    key: ResourceKey,
    delta: i64,
}

impl ResourceChange {
    /// 返回发生变化的资源键。
    pub const fn key(self) -> ResourceKey {
        self.key
    }

    /// 返回计划对资源桶的有符号变化量。
    pub const fn delta(self) -> i64 {
        self.delta
    }
}

/// 计划级资源变化，统一核算拆解产物、合成成本和强化成本。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResourceDelta {
    changes: Vec<ResourceChange>,
}

impl ResourceDelta {
    /// 返回按资源键稳定排序的变化列表。
    pub fn changes(&self) -> &[ResourceChange] {
        &self.changes
    }

    /// 返回没有资源变化的空摘要。
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub(crate) fn empty() -> Self {
        Self {
            changes: Vec::new(),
        }
    }
}

/// 计划引用的舰船槽位，使用协议中的原始整数以便稳定序列化。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PlanSlot {
    ship_instance_id: u64,
    slot_index: u8,
}

impl PlanSlot {
    fn from_domain(slot: ShipSlotRef) -> Self {
        Self {
            ship_instance_id: slot.ship_instance_id().get(),
            slot_index: slot.slot_index().get(),
        }
    }

    /// 从已经通过领域校验的运行态槽位标识建立执行和回读引用。
    pub(crate) const fn from_raw(ship_instance_id: u64, slot_index: u8) -> Self {
        Self {
            ship_instance_id,
            slot_index,
        }
    }

    /// 返回舰船实例 ID。
    pub const fn ship_instance_id(self) -> u64 {
        self.ship_instance_id
    }

    /// 返回 1 至 5 的槽位编号。
    pub const fn slot_index(self) -> u8 {
        self.slot_index
    }
}

/// 计划步骤使用的稳定来源引用。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[non_exhaustive]
pub enum PlanSource {
    /// 从仓库消费一件具体强化配置。
    Warehouse { config_id: u64 },
    /// 从另一条舰船槽位移动当前装备。
    ShipSlot {
        ship_instance_id: u64,
        slot_index: u8,
    },
    /// 由一条已经校验且预留资源的静态配方产生装备。
    Compose { recipe_id: u64 },
}

impl PlanSource {
    fn from_domain(source: EquipmentSourceRef) -> Self {
        match source {
            EquipmentSourceRef::Warehouse(config_id) => Self::Warehouse {
                config_id: config_id.get(),
            },
            EquipmentSourceRef::ShipSlot(slot) => Self::ShipSlot {
                ship_instance_id: slot.ship_instance_id().get(),
                slot_index: slot.slot_index().get(),
            },
        }
    }
}

/// 计划中确定的装备配置、装备族和实际强化等级。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PlanEquipment {
    family_id: u64,
    config_id: u64,
    enhance_level: u8,
}

impl PlanEquipment {
    fn new(
        family_id: EquipmentFamilyId,
        config_id: EquipmentConfigId,
        level: EnhanceLevel,
    ) -> Self {
        Self {
            family_id: family_id.get(),
            config_id: config_id.get(),
            enhance_level: level.get(),
        }
    }

    /// 从已知合法的原始值建立执行契约测试装备。
    #[cfg(test)]
    pub(crate) const fn from_raw(family_id: u64, config_id: u64, enhance_level: u8) -> Self {
        Self {
            family_id,
            config_id,
            enhance_level,
        }
    }

    /// 返回装备族 ID。
    pub const fn family_id(self) -> u64 {
        self.family_id
    }

    /// 返回具体配置 ID。
    pub const fn config_id(self) -> u64 {
        self.config_id
    }

    /// 返回实际强化等级。
    pub const fn enhance_level(self) -> u8 {
        self.enhance_level
    }
}

/// 单级强化消耗的一种背包材料。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PlanEnhanceMaterialCost {
    item_id: u64,
    quantity: u64,
}

impl PlanEnhanceMaterialCost {
    /// 从已知合法的原始值建立执行契约测试材料成本。
    #[cfg(test)]
    pub(crate) const fn from_raw(item_id: u64, quantity: u64) -> Self {
        Self { item_id, quantity }
    }

    /// 返回背包物品 ID。
    pub const fn item_id(self) -> u64 {
        self.item_id
    }

    /// 返回本级强化消耗的数量。
    pub const fn quantity(self) -> u64 {
        self.quantity
    }
}

/// 一件装备升至相邻下一配置所需的完整资源成本。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanEnhanceCost {
    gold: u64,
    materials: Vec<PlanEnhanceMaterialCost>,
}

impl PlanEnhanceCost {
    /// 从已知合法且已排序的原始值建立执行契约测试强化成本。
    #[cfg(test)]
    pub(crate) fn from_raw(gold: u64, materials: Vec<PlanEnhanceMaterialCost>) -> Self {
        Self { gold, materials }
    }

    /// 返回本级强化消耗的物资。
    pub const fn gold(&self) -> u64 {
        self.gold
    }

    /// 返回按物品 ID 严格升序排列的材料成本。
    pub fn materials(&self) -> &[PlanEnhanceMaterialCost] {
        &self.materials
    }
}

/// 一条不含设备写入能力的有序计划步骤。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum PlanStep {
    /// 当前槽位已经满足保持要求，不发送写命令。
    Keep { sequence: u32, slot: PlanSlot },
    /// 将当前槽位清空。
    Unequip { sequence: u32, slot: PlanSlot },
    /// 从已经确认的来源扣减并销毁指定数量；舰船来源会先生成卸装步骤。
    Dismantle {
        sequence: u32,
        source: PlanSource,
        equipment: PlanEquipment,
        quantity: u64,
    },
    /// 按静态配方合成装备；相邻同配方目标会在仓库容量允许时聚合。
    Compose {
        sequence: u32,
        recipe_id: u64,
        equipment: PlanEquipment,
        quantity: u64,
        material_id: u64,
        material_quantity_per_unit: u64,
        gold_per_unit: u64,
    },
    /// 把实际位置中的一件装备强化至相邻的下一配置。
    Enhance {
        sequence: u32,
        source: PlanSource,
        source_equipment: PlanEquipment,
        target_equipment: PlanEquipment,
        cost: PlanEnhanceCost,
    },
    /// 将确定来源的装备放入目标槽位。
    Equip {
        sequence: u32,
        slot: PlanSlot,
        source: PlanSource,
        equipment: PlanEquipment,
    },
}

impl PlanStep {
    /// 返回日志、执行报告和外部投影共用的稳定步骤类型。
    pub const fn stable_key(&self) -> &'static str {
        match self {
            Self::Keep { .. } => "keep",
            Self::Unequip { .. } => "unequip",
            Self::Dismantle { .. } => "dismantle",
            Self::Compose { .. } => "compose",
            Self::Enhance { .. } => "enhance",
            Self::Equip { .. } => "equip",
        }
    }

    /// 返回计划内从 1 开始的稳定步骤序号。
    pub const fn sequence(&self) -> u32 {
        match self {
            Self::Keep { sequence, .. }
            | Self::Unequip { sequence, .. }
            | Self::Dismantle { sequence, .. }
            | Self::Compose { sequence, .. }
            | Self::Enhance { sequence, .. }
            | Self::Equip { sequence, .. } => *sequence,
        }
    }

    /// 返回步骤直接作用的舰船槽位；纯库存步骤没有目标槽位。
    pub const fn slot(&self) -> Option<PlanSlot> {
        match self {
            Self::Keep { slot, .. } | Self::Unequip { slot, .. } | Self::Equip { slot, .. } => {
                Some(*slot)
            }
            Self::Enhance {
                source:
                    PlanSource::ShipSlot {
                        ship_instance_id,
                        slot_index,
                    },
                ..
            } => Some(PlanSlot::from_raw(*ship_instance_id, *slot_index)),
            Self::Dismantle { .. } | Self::Compose { .. } | Self::Enhance { .. } => None,
        }
    }

    /// 返回步骤使用的装备来源；不消费来源的步骤为空。
    pub const fn source(&self) -> Option<PlanSource> {
        match self {
            Self::Dismantle { source, .. }
            | Self::Enhance { source, .. }
            | Self::Equip { source, .. } => Some(*source),
            Self::Compose { recipe_id, .. } => Some(PlanSource::Compose {
                recipe_id: *recipe_id,
            }),
            Self::Keep { .. } | Self::Unequip { .. } => None,
        }
    }

    fn with_sequence(self, sequence: u32) -> Self {
        match self {
            Self::Keep { slot, .. } => Self::Keep { sequence, slot },
            Self::Unequip { slot, .. } => Self::Unequip { sequence, slot },
            Self::Dismantle {
                source,
                equipment,
                quantity,
                ..
            } => Self::Dismantle {
                sequence,
                source,
                equipment,
                quantity,
            },
            Self::Compose {
                recipe_id,
                equipment,
                quantity,
                material_id,
                material_quantity_per_unit,
                gold_per_unit,
                ..
            } => Self::Compose {
                sequence,
                recipe_id,
                equipment,
                quantity,
                material_id,
                material_quantity_per_unit,
                gold_per_unit,
            },
            Self::Enhance {
                source,
                source_equipment,
                target_equipment,
                cost,
                ..
            } => Self::Enhance {
                sequence,
                source,
                source_equipment,
                target_equipment,
                cost,
            },
            Self::Equip {
                slot,
                source,
                equipment,
                ..
            } => Self::Equip {
                sequence,
                slot,
                source,
                equipment,
            },
        }
    }
}

/// 资源、来源和步骤均已确定的不可变配装计划。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompiledPlan {
    schema_version: u32,
    game_state_content_sha256: String,
    desired_state_content_sha256: String,
    inventory_plan_content_sha256: String,
    steps: Vec<PlanStep>,
    resource_constraints: Vec<ResourceConstraint>,
    resource_delta: ResourceDelta,
    content_sha256: String,
}

impl CompiledPlan {
    /// 返回计划契约版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回编译所依据的完整游戏状态摘要。
    pub fn game_state_content_sha256(&self) -> &str {
        &self.game_state_content_sha256
    }

    /// 返回编译所依据的目标状态摘要。
    pub fn desired_state_content_sha256(&self) -> &str {
        &self.desired_state_content_sha256
    }

    /// 返回编译所依据的装备库存处理计划摘要。
    pub fn inventory_plan_content_sha256(&self) -> &str {
        &self.inventory_plan_content_sha256
    }

    /// 返回按目标优先级和槽位稳定排列的步骤。
    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    /// 返回全部资源约束。
    pub fn resource_constraints(&self) -> &[ResourceConstraint] {
        &self.resource_constraints
    }

    /// 返回计划计算出的资源变化。
    pub const fn resource_delta(&self) -> &ResourceDelta {
        &self.resource_delta
    }

    /// 返回不包含自身字段的计划内容 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 一次计划检查成功后的稳定报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    message: &'static str,
    plan: CompiledPlan,
    checked_slots: usize,
    checked_inventory_actions: usize,
    warnings: Vec<String>,
}

impl CheckReport {
    fn new(plan: CompiledPlan, checked_slots: usize, checked_inventory_actions: usize) -> Self {
        Self {
            message: "配装计划检查通过",
            plan,
            checked_slots,
            checked_inventory_actions,
            warnings: Vec::new(),
        }
    }

    fn without_modifications(plan: CompiledPlan) -> Self {
        Self {
            message: "修改列没有需要检查的操作",
            plan,
            checked_slots: 0,
            checked_inventory_actions: 0,
            warnings: Vec::new(),
        }
    }

    /// 返回面向 CLI、GUI 和日志的检查结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回不可变编译计划。
    pub const fn plan(&self) -> &CompiledPlan {
        &self.plan
    }

    /// 返回已经核对的目标槽位数量。
    pub const fn checked_slots(&self) -> usize {
        self.checked_slots
    }

    /// 返回已经核对的装备库存处理动作数量。
    pub const fn checked_inventory_actions(&self) -> usize {
        self.checked_inventory_actions
    }

    /// 返回检查产生的警告；未实现能力会直接返回错误，因此成功报告为空。
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// 计划检查阶段发现的稳定业务错误。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanCheckError {
    /// 目标槽位所属舰船不存在。
    #[error("目标舰船 {ship_instance_id} 不存在")]
    ShipNotFound { ship_instance_id: ShipInstanceId },
    /// 目标装备族不存在。
    #[error("装备族 {family_id} 不存在")]
    EquipmentFamilyNotFound { family_id: EquipmentFamilyId },
    /// 当前装备配置无法在完整目录中定位。
    #[error("装备配置 {config_id} 不存在")]
    EquipmentConfigNotFound { config_id: EquipmentConfigId },
    /// 目标装备类型或明确禁用舰种与舰船槽位不兼容。
    #[error(
        "目标槽位 {slot} 不允许装备配置 {config_id}（装备类型 {equipment_type_id}，舰种 {ship_type_id}）"
    )]
    EquipmentIncompatible {
        slot: ShipSlotRef,
        config_id: EquipmentConfigId,
        equipment_type_id: u64,
        ship_type_id: u64,
        allowed_equipment_type_ids: Vec<u64>,
        forbidden_ship_type_ids: Vec<u64>,
    },
    /// 指定来源缺失；正常构造的 DesiredEquipment 不会触发该分支。
    #[error("来源规则为指定来源时缺少具体来源")]
    ExactSourceMissing,
    /// 指定或自动选择的来源不存在。
    #[error("装备来源 {source_ref} 不存在或当前为空")]
    SourceNotFound { source_ref: EquipmentSourceRef },
    /// 来源装备与目标装备族不一致。
    #[error(
        "装备来源 {source_ref} 属于装备族 {actual_family_id}，目标要求装备族 {expected_family_id}"
    )]
    SourceFamilyMismatch {
        source_ref: EquipmentSourceRef,
        actual_family_id: EquipmentFamilyId,
        expected_family_id: EquipmentFamilyId,
    },
    /// 仓库配置数量不足。
    #[error("装备来源 {source_ref} 数量不足，可用 {available}，需要 {required}")]
    SourceUnavailable {
        source_ref: EquipmentSourceRef,
        available: u64,
        required: u64,
    },
    /// 当前装备族没有可用于自动选择的仓库来源。
    #[error("装备族 {family_id} 没有可用的仓库来源")]
    NoWarehouseSource { family_id: EquipmentFamilyId },
    /// 当前装备族没有静态装备合成配方。
    #[error("装备族 {family_id} 没有可用的装备合成配方")]
    ComposeRecipeNotFound { family_id: EquipmentFamilyId },
    /// 静态配方没有出现在同一完整状态的背包合成快照中。
    #[error("装备合成配方 {recipe_id} 当前不可用")]
    ComposeRecipeUnavailable { recipe_id: u64 },
    /// 静态配方与背包合成快照的产物或成本不一致。
    #[error("装备合成配方 {recipe_id} 的静态定义与当前背包状态不一致")]
    ComposeRecipeMismatch { recipe_id: u64 },
    /// 初始背包材料或物资不能覆盖全部已预留的合成次数。
    #[error("装备合成配方 {recipe_id} 的资源 {key:?} 不足，可用 {available}，需要 {required}")]
    ComposeResourceUnavailable {
        recipe_id: u64,
        key: ResourceKey,
        available: u64,
        required: u64,
    },
    /// 内部合成来源预留数量与需要装配的目标数量不一致。
    #[error("装备合成配方 {recipe_id} 预留 {reserved} 件，但装配目标需要 {targets} 件")]
    ComposeTargetReservationMismatch {
        recipe_id: u64,
        reserved: u64,
        targets: u64,
    },
    /// 来源槽位要求保持不变，或已经被另一个目标分配。
    #[error("装备来源槽位 {source_ref} 不能再次分配")]
    SourceTargetConflict { source_ref: ShipSlotRef },
    /// 目标槽位不能从自身取装备。
    #[error("目标槽位 {slot} 不能作为自己的装备来源")]
    SourceEqualsTarget { slot: ShipSlotRef },
    /// 目标强化等级不在装备族强化链中。
    #[error("装备族 {family_id} 没有强化等级 +{target_level}")]
    TargetEnhanceLevelUnavailable {
        family_id: EquipmentFamilyId,
        target_level: u8,
    },
    /// 当前版本只允许保持或提升，不允许降级。
    #[error("装备来源当前为 +{source_level}，目标为 +{target_level}，不允许降级")]
    EnhanceDowngrade { source_level: u8, target_level: u8 },
    /// 装备目录中的相邻强化配置不能组成双向连续链。
    #[error("装备强化链从配置 {source_config_id} 到 {target_config_id} 不连续")]
    EnhanceChainMismatch {
        source_config_id: u64,
        target_config_id: u64,
    },
    /// 初始背包材料或物资不能覆盖全部已预留的强化步骤。
    #[error(
        "装备配置 {source_config_id} 的强化资源 {key:?} 不足，可用 {available}，需要 {required}"
    )]
    EnhanceResourceUnavailable {
        source_config_id: u64,
        key: ResourceKey,
        available: u64,
        required: u64,
    },
    /// 装备库存动作指定的来源不存在。
    #[error("库存动作来源 {source_ref} 不存在或当前为空")]
    InventorySourceNotFound { source_ref: EquipmentSourceRef },
    /// 装备库存动作指定的来源数量不足。
    #[error("库存动作来源 {source_ref} 数量不足，可用 {available}，需要 {required}")]
    InventorySourceUnavailable {
        source_ref: EquipmentSourceRef,
        available: u64,
        required: u64,
    },
    /// 装备库存动作与配装目标或另一个来源处理发生冲突。
    #[error("库存动作来源 {source_ref} 与配装计划冲突")]
    InventorySourceConflict { source_ref: EquipmentSourceRef },
    /// 装备命中客户端保护条件，不能进入无人值守自动拆解计划。
    #[error(
        "库存动作来源 {source_ref} 命中自动拆解保护: important={important}, protected_variant={protected_variant}, rarity_confirmation_required={rarity_confirmation_required}, enhanced={enhanced}"
    )]
    InventoryDismantleProtected {
        source_ref: EquipmentSourceRef,
        important: bool,
        protected_variant: bool,
        rarity_confirmation_required: bool,
        enhanced: bool,
    },
    /// 当前步骤顺序无法容纳所有先行卸下的装备。
    #[error("装备仓库临时空位不足，可用 {available}，卸下阶段需要 {required}")]
    TemporaryEquipmentCapacityUnavailable { available: u64, required: u64 },
    /// 完成先行卸装、拆解和已有来源装配后，仓库仍无法容纳下一批合成产物。
    #[error("装备仓库合成空位不足，可用 {available}，下一批合成需要 {required}")]
    ComposeEquipmentCapacityUnavailable { available: u64, required: u64 },
    /// 仓库容量快照小于计划中已经确认存在的装备来源数量。
    #[error("装备仓库容量快照不一致，当前容量 {available}，已有来源需要 {required}")]
    EquipmentCapacitySnapshotMismatch { available: u64, required: u64 },
    /// 将数量编码为资源变化时超出稳定整数范围。
    #[error("资源数量 {quantity} 超出计划变化的整数范围")]
    QuantityOverflow { quantity: u64 },
    /// 聚合后的资源变化超出稳定有符号整数范围。
    #[error("资源 {key:?} 的计划变化超出稳定整数范围")]
    ResourceChangeOverflow { key: ResourceKey },
    /// 计划步骤数量超出稳定序号范围。
    #[error("计划步骤数量超出稳定序号范围")]
    StepSequenceOverflow,
    /// 计划摘要不能编码为稳定 JSON。
    #[error("编码配装计划摘要失败: {message}")]
    DigestEncoding { message: String },
}

/// 兼容入口：仅依据完整游戏状态检查配装目标，使用空装备库存计划。
///
/// 需要把工作簿中的装备库存处理要求纳入同一份检查时，调用
/// [`compile_plan_with_inventory`]，避免把旧的配装专用入口误当成全量检查。
pub fn compile_plan(
    state: &GameState,
    desired: &DesiredState,
) -> Result<CheckReport, PlanCheckError> {
    let inventory_plan =
        EquipmentInventoryPlan::new(Vec::new()).expect("空装备库存计划应始终满足模型不变量");
    compile_plan_with_inventory(state, desired, &inventory_plan)
}

/// 工作簿修改列是否包含需要检查或执行的配装、强化或拆解要求。
pub fn workbook_plan_has_modifications(
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
) -> bool {
    desired
        .slots()
        .iter()
        .any(|slot| !matches!(slot.target(), SlotTarget::Keep))
        || inventory_plan
            .actions()
            .iter()
            .any(|action| !action.is_noop())
}

/// 仅根据工作簿修改列确认没有可执行要求，不读取游戏状态。
pub fn compile_workbook_without_modifications(
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
) -> Result<CheckReport, PlanCheckError> {
    debug_assert!(!workbook_plan_has_modifications(desired, inventory_plan));
    Ok(CheckReport::without_modifications(
        compiler::compile_absent_modifications_plan(desired, inventory_plan)?,
    ))
}

/// 依据完整游戏状态、配装目标和装备库存处理要求生成只读检查报告。
pub fn compile_plan_with_inventory(
    state: &GameState,
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
) -> Result<CheckReport, PlanCheckError> {
    let plan: CompiledPlan = compile_plan_only(state, desired, inventory_plan)?;
    let checked_inventory_actions: usize = inventory_plan
        .actions()
        .iter()
        .filter(|action| !action.is_noop())
        .count();
    Ok(CheckReport::new(
        plan,
        desired.len(),
        checked_inventory_actions,
    ))
}

pub(crate) fn compile_direct_plan(
    state: &GameState,
    desired: &DesiredState,
    inventory: &EquipmentInventoryPlan,
    compositions: &[(u64, u64)],
) -> Result<CheckReport, PlanCheckError> {
    let plan = compiler::compile_plan_with_compositions(state, desired, inventory, compositions)?;
    Ok(CheckReport::new(
        plan,
        desired.len(),
        inventory
            .actions()
            .iter()
            .filter(|action| !action.is_noop())
            .count()
            + compositions.len(),
    ))
}

#[cfg(test)]
mod tests;

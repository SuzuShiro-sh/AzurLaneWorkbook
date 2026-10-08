//! 配装目标、来源规则及其创建时必须成立的不变量。

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::num::NonZeroU64;

use thiserror::Error;

macro_rules! positive_identifier {
    ($(#[$metadata:meta])* $name:ident, $field:literal) => {
        $(#[$metadata])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// 从游戏返回的正整数建立强类型标识。
            pub fn new(value: u64) -> Result<Self, LoadoutModelError> {
                NonZeroU64::new(value).map(Self).ok_or(
                    LoadoutModelError::IdentifierMustBePositive { field: $field },
                )
            }

            /// 返回游戏协议使用的原始整数值。
            pub const fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

positive_identifier!(
    /// 账号内一条真实舰船的实例标识。
    ShipInstanceId,
    "舰船实例 ID"
);
positive_identifier!(
    /// 一种具体强化配置的装备标识；仓库以该标识聚合数量。
    EquipmentConfigId,
    "装备配置 ID"
);
positive_identifier!(
    /// 跨强化配置归并后的一种装备标识。
    EquipmentFamilyId,
    "装备族 ID"
);

/// 舰船装备槽编号；船坞快照与装备命令固定使用 1 至 5。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SlotIndex(u8);

impl SlotIndex {
    /// 最小有效槽位编号。
    pub const MIN: u8 = 1;
    /// 最大有效槽位编号。
    pub const MAX: u8 = 5;

    /// 校验并建立舰船装备槽编号。
    pub fn new(value: u8) -> Result<Self, LoadoutModelError> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(LoadoutModelError::SlotIndexOutOfRange { value })
        }
    }

    /// 返回游戏运行态使用的原始槽位编号。
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Display for SlotIndex {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// 一条真实舰船上的唯一装备槽。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ShipSlotRef {
    ship_instance_id: ShipInstanceId,
    slot_index: SlotIndex,
}

impl ShipSlotRef {
    /// 组合已经分别校验的舰船实例和槽位标识。
    pub const fn new(ship_instance_id: ShipInstanceId, slot_index: SlotIndex) -> Self {
        Self {
            ship_instance_id,
            slot_index,
        }
    }

    /// 返回槽位所属舰船的实例标识。
    pub const fn ship_instance_id(self) -> ShipInstanceId {
        self.ship_instance_id
    }

    /// 返回 1 至 5 的槽位编号。
    pub const fn slot_index(self) -> SlotIndex {
        self.slot_index
    }
}

impl Display for ShipSlotRef {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.ship_instance_id, self.slot_index)
    }
}

/// 用户能够精确指定的装备来源。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EquipmentSourceRef {
    /// 仓库中某一具体强化配置；同配置的多件装备彼此等价。
    Warehouse(EquipmentConfigId),
    /// 舰船上的具体槽位；该位置在一次配装中只能被消费一次。
    ShipSlot(ShipSlotRef),
}

impl Display for EquipmentSourceRef {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Warehouse(config_id) => write!(formatter, "仓库配置 {config_id}"),
            Self::ShipSlot(slot) => write!(formatter, "舰船槽位 {slot}"),
        }
    }
}

/// 自动选择装备来源时使用的稳定优先顺序。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePolicy {
    /// 先保留目标槽当前装备，再依次考虑仓库、合成和其他舰船。
    CurrentThenWarehouseThenComposeThenShip,
    /// 保留已满足目标的当前装备，否则依次考虑仓库与合成，不从其他舰船取用。
    WarehouseThenCompose,
    /// 依次考虑仓库、合成和其他舰船。
    WarehouseThenComposeThenShip,
    /// 依次考虑仓库、其他舰船和合成。
    WarehouseThenShipThenCompose,
    /// 依次考虑合成、仓库和其他舰船。
    ComposeThenWarehouseThenShip,
    /// 只使用仓库中的装备数量。
    WarehouseOnly,
    /// 只使用合成后增加的装备数量。
    ComposeOnly,
    /// 只从其他舰船槽位取得装备。
    ShipOnly,
    /// 只使用用户明确选择的仓库配置或舰船槽位。
    ExactSource,
}

impl SourcePolicy {
    /// 返回该来源规则是否必须同时携带指定来源。
    pub const fn requires_exact_source(self) -> bool {
        matches!(self, Self::ExactSource)
    }
}

impl Display for SourcePolicy {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let text: &str = match self {
            Self::CurrentThenWarehouseThenComposeThenShip => "当前 -> 仓库 -> 合成 -> 舰上",
            Self::WarehouseThenCompose => "仓库 -> 合成",
            Self::WarehouseThenComposeThenShip => "仓库 -> 合成 -> 舰上",
            Self::WarehouseThenShipThenCompose => "仓库 -> 舰上 -> 合成",
            Self::ComposeThenWarehouseThenShip => "合成 -> 仓库 -> 舰上",
            Self::WarehouseOnly => "只用仓库",
            Self::ComposeOnly => "只用合成",
            Self::ShipOnly => "只从舰上拔取",
            Self::ExactSource => "指定来源",
        };
        formatter.write_str(text)
    }
}

/// 游戏界面展示的装备强化等级；具体装备的上限由完整配置链校验。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EnhanceLevel(u8);

impl EnhanceLevel {
    /// 建立非负的展示强化等级。
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// 返回游戏界面使用的 `+N` 数值。
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// 一个装备目标及其允许的来源和可选强化要求。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesiredEquipment {
    family_id: EquipmentFamilyId,
    source_policy: SourcePolicy,
    exact_source: Option<EquipmentSourceRef>,
    target_enhance_level: Option<EnhanceLevel>,
}

impl DesiredEquipment {
    /// 建立装备目标，并保证来源规则与指定来源成对出现。
    pub fn new(
        family_id: EquipmentFamilyId,
        source_policy: SourcePolicy,
        exact_source: Option<EquipmentSourceRef>,
        target_enhance_level: Option<EnhanceLevel>,
    ) -> Result<Self, LoadoutModelError> {
        match (source_policy.requires_exact_source(), exact_source) {
            (true, None) => return Err(LoadoutModelError::ExactSourceRequired),
            (false, Some(_)) => {
                return Err(LoadoutModelError::ExactSourceNotAllowed { source_policy });
            }
            _ => {}
        }
        Ok(Self {
            family_id,
            source_policy,
            exact_source,
            target_enhance_level,
        })
    }

    /// 返回需要取得的装备族。
    pub const fn family_id(self) -> EquipmentFamilyId {
        self.family_id
    }

    /// 返回允许使用的装备来源顺序。
    pub const fn source_policy(self) -> SourcePolicy {
        self.source_policy
    }

    /// 返回用户明确指定的仓库配置或舰船槽位。
    pub const fn exact_source(self) -> Option<EquipmentSourceRef> {
        self.exact_source
    }

    /// 返回目标强化等级；空值表示保持来源装备的实际等级。
    pub const fn target_enhance_level(self) -> Option<EnhanceLevel> {
        self.target_enhance_level
    }
}

/// 用户要求一个槽位最终达到的状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotTarget {
    /// 保持执行前最新读取到的实际槽位，不产生配装操作。
    Keep,
    /// 最终槽位必须为空。
    Empty,
    /// 最终槽位必须装上满足要求的装备。
    Equipment(DesiredEquipment),
}

/// 一个启用槽位的最终状态要求。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesiredSlotState {
    slot: ShipSlotRef,
    target: SlotTarget,
    allocation_priority: i32,
}

impl DesiredSlotState {
    /// 建立槽位要求；数值更小的分配优先级会更早取得稀缺装备。
    pub const fn new(slot: ShipSlotRef, target: SlotTarget, allocation_priority: i32) -> Self {
        Self {
            slot,
            target,
            allocation_priority,
        }
    }

    /// 返回目标舰船槽位。
    pub const fn slot(self) -> ShipSlotRef {
        self.slot
    }

    /// 返回槽位最终状态。
    pub const fn target(self) -> SlotTarget {
        self.target
    }

    /// 返回装备分配优先级。
    pub const fn allocation_priority(self) -> i32 {
        self.allocation_priority
    }

    fn exact_ship_source(self) -> Option<ShipSlotRef> {
        match self.target {
            SlotTarget::Equipment(equipment) => match equipment.exact_source() {
                Some(EquipmentSourceRef::ShipSlot(slot)) => Some(slot),
                Some(EquipmentSourceRef::Warehouse(_)) | None => None,
            },
            SlotTarget::Keep | SlotTarget::Empty => None,
        }
    }
}

/// 已通过跨行唯一性校验并按稳定顺序排列的配装要求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredState {
    slots: Vec<DesiredSlotState>,
}

impl DesiredState {
    /// 校验目标槽位和指定舰船来源唯一性，并按优先级、舰船和槽位稳定排序。
    pub fn new(mut slots: Vec<DesiredSlotState>) -> Result<Self, LoadoutModelError> {
        let mut seen_slots: BTreeSet<ShipSlotRef> = BTreeSet::new();
        let mut seen_ship_sources: BTreeSet<ShipSlotRef> = BTreeSet::new();
        for desired in &slots {
            if !seen_slots.insert(desired.slot()) {
                return Err(LoadoutModelError::DuplicateSlot {
                    slot: desired.slot(),
                });
            }
            if let Some(source) = desired.exact_ship_source()
                && !seen_ship_sources.insert(source)
            {
                return Err(LoadoutModelError::DuplicateShipSource {
                    source_slot: source,
                });
            }
        }
        slots.sort_by_key(|desired| {
            (
                desired.allocation_priority(),
                desired.slot().ship_instance_id(),
                desired.slot().slot_index(),
            )
        });
        Ok(Self { slots })
    }

    /// 返回按稳定分配顺序排列的全部启用槽位。
    pub fn slots(&self) -> &[DesiredSlotState] {
        &self.slots
    }

    /// 返回启用槽位数量。
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// 返回工作簿是否没有启用任何配装要求。
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// 配装输入在进入计划器前违反了结构性业务约束。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LoadoutModelError {
    /// 游戏对象标识使用了协议中的空值 0。
    #[error("{field} 必须大于 0")]
    IdentifierMustBePositive { field: &'static str },
    /// 槽位超出船坞快照与装备命令使用的 1 至 5 范围。
    #[error("槽位编号必须在 1 至 5 之间，实际为 {value}")]
    SlotIndexOutOfRange { value: u8 },
    /// 指定来源规则没有携带仓库配置或舰船槽位。
    #[error("来源规则为指定来源时必须填写仓库装备配置或舰船槽位")]
    ExactSourceRequired,
    /// 自动来源规则错误地混入了指定来源。
    #[error("来源规则 {source_policy} 不允许填写指定来源")]
    ExactSourceNotAllowed { source_policy: SourcePolicy },
    /// 同一真实槽位在配装输入中出现多次。
    #[error("舰船槽位 {slot} 出现重复目标")]
    DuplicateSlot { slot: ShipSlotRef },
    /// 同一舰船来源槽位被多个最终槽位指定。
    #[error("来源舰船槽位 {source_slot} 被重复分配")]
    DuplicateShipSource { source_slot: ShipSlotRef },
}

#[cfg(test)]
mod tests {
    use super::{
        DesiredEquipment, DesiredSlotState, DesiredState, EnhanceLevel, EquipmentConfigId,
        EquipmentFamilyId, EquipmentSourceRef, LoadoutModelError, ShipInstanceId, ShipSlotRef,
        SlotIndex, SlotTarget, SourcePolicy,
    };

    #[test]
    fn rejects_zero_identifiers_and_out_of_range_slots() {
        assert_eq!(
            ShipInstanceId::new(0),
            Err(LoadoutModelError::IdentifierMustBePositive {
                field: "舰船实例 ID"
            })
        );
        assert_eq!(
            EquipmentConfigId::new(0),
            Err(LoadoutModelError::IdentifierMustBePositive {
                field: "装备配置 ID"
            })
        );
        assert_eq!(
            EquipmentFamilyId::new(0),
            Err(LoadoutModelError::IdentifierMustBePositive {
                field: "装备族 ID"
            })
        );
        assert_eq!(
            SlotIndex::new(0),
            Err(LoadoutModelError::SlotIndexOutOfRange { value: 0 })
        );
        assert_eq!(
            SlotIndex::new(6),
            Err(LoadoutModelError::SlotIndexOutOfRange { value: 6 })
        );
        assert_eq!(SlotIndex::new(1).unwrap().get(), 1);
        assert_eq!(SlotIndex::new(5).unwrap().get(), 5);
    }

    #[test]
    fn exact_source_and_source_policy_must_agree() {
        let family_id: EquipmentFamilyId = EquipmentFamilyId::new(10_040).unwrap();
        let source: EquipmentSourceRef =
            EquipmentSourceRef::Warehouse(EquipmentConfigId::new(90_001).unwrap());

        assert_eq!(
            DesiredEquipment::new(family_id, SourcePolicy::ExactSource, None, None),
            Err(LoadoutModelError::ExactSourceRequired)
        );
        assert_eq!(
            DesiredEquipment::new(family_id, SourcePolicy::WarehouseOnly, Some(source), None),
            Err(LoadoutModelError::ExactSourceNotAllowed {
                source_policy: SourcePolicy::WarehouseOnly,
            })
        );

        let desired: DesiredEquipment = DesiredEquipment::new(
            family_id,
            SourcePolicy::ExactSource,
            Some(source),
            Some(EnhanceLevel::new(10)),
        )
        .unwrap();
        assert_eq!(desired.family_id(), family_id);
        assert_eq!(desired.exact_source(), Some(source));
        assert_eq!(desired.target_enhance_level().unwrap().get(), 10);
    }

    #[test]
    fn desired_state_rejects_duplicate_target_and_source_slots() {
        let first_slot: ShipSlotRef = ship_slot(1, 1);
        let second_slot: ShipSlotRef = ship_slot(2, 1);
        let source_slot: ShipSlotRef = ship_slot(3, 2);
        let equipment: DesiredEquipment = DesiredEquipment::new(
            EquipmentFamilyId::new(7).unwrap(),
            SourcePolicy::ExactSource,
            Some(EquipmentSourceRef::ShipSlot(source_slot)),
            None,
        )
        .unwrap();

        assert_eq!(
            DesiredState::new(vec![
                DesiredSlotState::new(first_slot, SlotTarget::Keep, 0),
                DesiredSlotState::new(first_slot, SlotTarget::Empty, 1),
            ]),
            Err(LoadoutModelError::DuplicateSlot { slot: first_slot })
        );
        assert_eq!(
            DesiredState::new(vec![
                DesiredSlotState::new(first_slot, SlotTarget::Equipment(equipment), 0),
                DesiredSlotState::new(second_slot, SlotTarget::Equipment(equipment), 1),
            ]),
            Err(LoadoutModelError::DuplicateShipSource { source_slot })
        );
    }

    #[test]
    fn desired_state_allows_reusing_a_warehouse_configuration_with_quantity() {
        let source: EquipmentSourceRef =
            EquipmentSourceRef::Warehouse(EquipmentConfigId::new(70).unwrap());
        let equipment: DesiredEquipment = DesiredEquipment::new(
            EquipmentFamilyId::new(7).unwrap(),
            SourcePolicy::ExactSource,
            Some(source),
            None,
        )
        .unwrap();

        let desired: DesiredState = DesiredState::new(vec![
            DesiredSlotState::new(ship_slot(1, 1), SlotTarget::Equipment(equipment), 0),
            DesiredSlotState::new(ship_slot(2, 1), SlotTarget::Equipment(equipment), 1),
        ])
        .unwrap();

        assert_eq!(desired.len(), 2);
    }

    #[test]
    fn desired_state_uses_priority_then_slot_as_stable_order() {
        let desired: DesiredState = DesiredState::new(vec![
            DesiredSlotState::new(ship_slot(2, 3), SlotTarget::Keep, 20),
            DesiredSlotState::new(ship_slot(2, 2), SlotTarget::Empty, 10),
            DesiredSlotState::new(ship_slot(1, 5), SlotTarget::Keep, 10),
        ])
        .unwrap();

        let ordered: Vec<(u64, u8)> = desired
            .slots()
            .iter()
            .map(|item| {
                (
                    item.slot().ship_instance_id().get(),
                    item.slot().slot_index().get(),
                )
            })
            .collect();
        assert_eq!(ordered, vec![(1, 5), (2, 2), (2, 3)]);
    }

    fn ship_slot(ship_id: u64, slot_index: u8) -> ShipSlotRef {
        ShipSlotRef::new(
            ShipInstanceId::new(ship_id).unwrap(),
            SlotIndex::new(slot_index).unwrap(),
        )
    }
}

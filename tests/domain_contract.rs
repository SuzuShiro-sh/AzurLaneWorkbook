//! 从库使用方视角验证配装领域类型的公开构造与读取边界。

use azur_lane_workbook::domain::{
    DesiredEquipment, DesiredSlotState, DesiredState, EquipmentConfigId, EquipmentFamilyId,
    EquipmentSourceRef, ShipInstanceId, ShipSlotRef, SlotIndex, SlotTarget, SourcePolicy,
};

#[test]
fn public_loadout_contract_builds_a_typed_desired_state() {
    let slot: ShipSlotRef = ShipSlotRef::new(
        ShipInstanceId::new(50_865).expect("舰船实例 ID 应有效"),
        SlotIndex::new(4).expect("槽位应有效"),
    );
    let equipment: DesiredEquipment = DesiredEquipment::new(
        EquipmentFamilyId::new(10_040).expect("装备族 ID 应有效"),
        SourcePolicy::WarehouseThenComposeThenShip,
        None,
        None,
    )
    .expect("自动来源不应携带指定来源");
    let desired: DesiredState = DesiredState::new(vec![DesiredSlotState::new(
        slot,
        SlotTarget::Equipment(equipment),
        10,
    )])
    .expect("单个目标应满足唯一性约束");

    assert_eq!(desired.len(), 1);
    assert_eq!(desired.slots()[0].slot(), slot);
    let SlotTarget::Equipment(actual) = desired.slots()[0].target() else {
        panic!("目标应保持为装备状态");
    };
    assert_eq!(
        actual.source_policy(),
        SourcePolicy::WarehouseThenComposeThenShip
    );
}

#[test]
fn public_loadout_contract_distinguishes_warehouse_and_ship_sources() {
    let warehouse: EquipmentSourceRef =
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(2_620).expect("装备配置 ID 应有效"));
    let ship_slot: ShipSlotRef = ShipSlotRef::new(
        ShipInstanceId::new(50_865).expect("舰船实例 ID 应有效"),
        SlotIndex::new(1).expect("槽位应有效"),
    );

    assert_eq!(warehouse.to_string(), "仓库配置 2620");
    assert_eq!(
        EquipmentSourceRef::ShipSlot(ship_slot).to_string(),
        "舰船槽位 50865:1"
    );
}

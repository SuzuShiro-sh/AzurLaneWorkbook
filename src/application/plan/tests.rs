//! 覆盖计划编译、来源选择和资源约束的单元测试。

use super::compiler::{
    ComposeReservations, PlannedCompose, ValidatedCompose, schedule_compose_steps,
};
use super::{
    CheckReport, PlanCheckError, PlanEquipment, PlanSlot, PlanSource, PlanStep, ResourceKey,
    compile_plan, compile_plan_with_inventory,
};
use crate::application::test_support::{
    plan_game_state, plan_game_state_with_capacity, plan_game_state_with_compose,
    plan_game_state_with_compose_and_enhance, plan_game_state_with_current_equipment_only,
    plan_game_state_with_enhance, plan_game_state_with_equipment_compatibility,
    plan_game_state_with_incompatible_compose_mismatch,
};
use crate::domain::{
    DesiredEquipment, DesiredSlotState, DesiredState, EnhanceLevel, EquipmentConfigId,
    EquipmentFamilyId, EquipmentInventoryAction, EquipmentInventoryActionKind,
    EquipmentInventoryPlan, EquipmentSourceRef, ShipInstanceId, ShipSlotRef, SlotIndex, SlotTarget,
    SourcePolicy,
};

fn slot(ship: u64, index: u8) -> ShipSlotRef {
    ShipSlotRef::new(
        ShipInstanceId::new(ship).unwrap(),
        SlotIndex::new(index).unwrap(),
    )
}

fn inventory_plan(action: EquipmentInventoryAction) -> EquipmentInventoryPlan {
    EquipmentInventoryPlan::new(vec![action]).unwrap()
}

#[test]
fn workbook_without_modifications_skips_game_state() {
    let desired = DesiredState::new(Vec::new()).unwrap();
    let inventory = EquipmentInventoryPlan::new(Vec::new()).unwrap();
    assert!(!super::workbook_plan_has_modifications(
        &desired, &inventory
    ));
    let report = super::compile_workbook_without_modifications(&desired, &inventory).unwrap();
    assert_eq!(report.message(), "修改列没有需要检查的操作");
    assert_eq!(report.checked_slots(), 0);
    assert_eq!(report.checked_inventory_actions(), 0);
    assert!(report.plan().steps().is_empty());
}

#[test]
fn keep_only_workbook_inputs_are_not_modifications() {
    let desired =
        DesiredState::new(vec![DesiredSlotState::new(slot(1, 1), SlotTarget::Keep, 0)]).unwrap();
    let inventory = inventory_plan(
        EquipmentInventoryAction::new(
            EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
            EquipmentInventoryActionKind::Keep,
            None,
            None,
            None,
        )
        .unwrap(),
    );
    assert!(!super::workbook_plan_has_modifications(
        &desired, &inventory
    ));
}

#[test]
fn empty_slot_and_inventory_enhance_are_workbook_modifications() {
    let empty_slot = DesiredState::new(vec![DesiredSlotState::new(
        slot(1, 1),
        SlotTarget::Empty,
        0,
    )])
    .unwrap();
    let empty_inventory = EquipmentInventoryPlan::new(Vec::new()).unwrap();
    assert!(super::workbook_plan_has_modifications(
        &empty_slot,
        &empty_inventory
    ));

    let desired = DesiredState::new(Vec::new()).unwrap();
    let enhance = inventory_plan(
        EquipmentInventoryAction::new(
            EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
            EquipmentInventoryActionKind::Keep,
            None,
            Some(EnhanceLevel::new(1)),
            Some(1),
        )
        .unwrap(),
    );
    assert!(super::workbook_plan_has_modifications(&desired, &enhance));
}

#[test]
fn empty_plan_has_stable_report_and_no_resource_changes() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();

    let report: CheckReport = compile_plan(&state, &desired).unwrap();

    assert_eq!(report.message(), "配装计划检查通过");
    assert_eq!(report.checked_slots(), 0);
    assert_eq!(report.checked_inventory_actions(), 0);
    assert!(report.warnings().is_empty());
    assert!(report.plan().steps().is_empty());
    assert!(report.plan().resource_constraints().is_empty());
    assert!(report.plan().resource_delta().is_empty());
    assert_eq!(report.plan().content_sha256().len(), 64);
    assert_eq!(report.plan().inventory_plan_content_sha256().len(), 64);
}

#[test]
fn inventory_keep_is_checked_and_part_of_the_plan_digest() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let empty_report = compile_plan(&state, &desired).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(1),
    )
    .unwrap();

    let report = compile_plan_with_inventory(&state, &desired, &inventory_plan(action))
        .expect("相同强化等级的保留动作应通过只读检查");

    assert_eq!(report.checked_inventory_actions(), 1);
    assert_ne!(
        report.plan().inventory_plan_content_sha256(),
        empty_report.plan().inventory_plan_content_sha256()
    );
    assert_ne!(
        report.plan().content_sha256(),
        empty_report.plan().content_sha256()
    );
    assert!(report.plan().steps().is_empty());
}

#[test]
fn inventory_enhance_quantity_is_part_of_the_plan_digest() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let action_with_one = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(1),
    )
    .unwrap();
    let action_with_two = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(2),
    )
    .unwrap();

    let first =
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action_with_one)).unwrap();
    let second =
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action_with_two)).unwrap();

    assert_ne!(
        first.plan().inventory_plan_content_sha256(),
        second.plan().inventory_plan_content_sha256()
    );
    assert_ne!(
        first.plan().content_sha256(),
        second.plan().content_sha256()
    );
}

#[test]
fn inventory_enhance_quantity_is_revalidated_against_each_source() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let warehouse_source = EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap());
    let warehouse = EquipmentInventoryAction::new(
        warehouse_source,
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(3),
    )
    .unwrap();
    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(warehouse)),
        Err(PlanCheckError::InventorySourceUnavailable {
            source_ref: warehouse_source,
            available: 2,
            required: 3,
        })
    );

    let ship_source = EquipmentSourceRef::ShipSlot(slot(9001, 1));
    let ship = EquipmentInventoryAction::new(
        ship_source,
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(0)),
        Some(2),
    )
    .unwrap();
    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(ship)),
        Err(PlanCheckError::InventorySourceUnavailable {
            source_ref: ship_source,
            available: 1,
            required: 2,
        })
    );
}

#[test]
fn inventory_noop_does_not_change_the_checked_plan() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let empty_report = compile_plan(&state, &desired).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        None,
        None,
    )
    .unwrap();

    let report = compile_plan_with_inventory(&state, &desired, &inventory_plan(action))
        .expect("无操作库存行不应阻断计划检查");

    assert_eq!(report.checked_inventory_actions(), 0);
    assert_eq!(
        report.plan().inventory_plan_content_sha256(),
        empty_report.plan().inventory_plan_content_sha256()
    );
    assert_eq!(
        report.plan().content_sha256(),
        empty_report.plan().content_sha256()
    );
}

#[test]
fn safe_warehouse_dismantle_compiles_steps_constraints_and_yields() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let source = EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap());
    let action = EquipmentInventoryAction::new(
        source,
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    let report = compile_plan_with_inventory(&state, &desired, &inventory_plan(action))
        .expect("低品质、未强化且非重要的仓库装备应生成拆解计划");

    assert_eq!(report.checked_inventory_actions(), 1);
    assert_eq!(report.plan().schema_version(), 5);
    assert_eq!(
        report.plan().steps(),
        &[PlanStep::Dismantle {
            sequence: 1,
            source: PlanSource::Warehouse { config_id: 1000 },
            equipment: super::PlanEquipment::new(
                EquipmentFamilyId::new(1000).unwrap(),
                EquipmentConfigId::new(1000).unwrap(),
                EnhanceLevel::new(0),
            ),
            quantity: 1,
        }]
    );
    let constraints = report.plan().resource_constraints();
    assert_eq!(constraints.len(), 1);
    assert_eq!(
        constraints[0].key(),
        ResourceKey::WarehouseEquipment { config_id: 1000 }
    );
    assert_eq!(constraints[0].available(), 1);
    assert_eq!(constraints[0].required(), 1);
    assert_eq!(constraints[0].remaining(), 0);
    let changes = report.plan().resource_delta().changes();
    assert_eq!(changes.len(), 3);
    assert_eq!(
        (changes[0].key(), changes[0].delta()),
        (ResourceKey::WarehouseEquipment { config_id: 1000 }, -1)
    );
    assert_eq!(
        (changes[1].key(), changes[1].delta()),
        (ResourceKey::Gold, 10)
    );
    assert_eq!(
        (changes[2].key(), changes[2].delta()),
        (ResourceKey::Item { item_id: 2001 }, 2)
    );
}

#[test]
fn enhanced_equipment_is_rejected_before_a_dismantle_plan_is_created() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let source = EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap());
    let action = EquipmentInventoryAction::new(
        source,
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::InventoryDismantleProtected {
            source_ref: source,
            important: false,
            protected_variant: false,
            rarity_confirmation_required: false,
            enhanced: true,
        })
    );
}

#[test]
fn ship_dismantle_requires_capacity_for_the_preceding_unequip_step() {
    let state = plan_game_state_with_capacity(3, 3);
    let desired = DesiredState::new(Vec::new()).unwrap();
    let source = EquipmentSourceRef::ShipSlot(slot(9001, 1));
    let action = EquipmentInventoryAction::new(
        source,
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::TemporaryEquipmentCapacityUnavailable {
            available: 0,
            required: 1,
        })
    );
}

#[test]
fn inventory_dismantle_checks_available_quantity_before_safety() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
        EquipmentInventoryActionKind::Dismantle,
        Some(3),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::InventorySourceUnavailable {
            source_ref: EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
            available: 2,
            required: 3,
        })
    );
}

#[test]
fn inventory_source_must_exist_in_the_current_snapshot() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let source = EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1999).unwrap());
    let action = EquipmentInventoryAction::new(
        source,
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(0)),
        Some(1),
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::InventorySourceNotFound { source_ref: source })
    );
}

#[test]
fn ship_dismantle_quantity_is_limited_to_one_slot_item() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let source = EquipmentSourceRef::ShipSlot(slot(9001, 4));
    let action = EquipmentInventoryAction::new(
        source,
        EquipmentInventoryActionKind::Dismantle,
        Some(2),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::InventorySourceUnavailable {
            source_ref: source,
            available: 1,
            required: 2,
        })
    );
}

#[test]
fn safe_ship_dismantle_unloads_once_before_destroying_the_warehouse_copy() {
    let state = plan_game_state();
    let source_slot = slot(9001, 1);
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        source_slot,
        SlotTarget::Empty,
        0,
    )])
    .unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::ShipSlot(source_slot),
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    let report = compile_plan_with_inventory(&state, &desired, &inventory_plan(action))
        .expect("安全舰装应先卸下再拆解");

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                sequence: 1,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
            },
            PlanStep::Dismantle {
                sequence: 2,
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                quantity: 1,
                ..
            }
        ]
    ));
    assert!(report.plan().resource_constraints().is_empty());
    assert_eq!(report.plan().resource_delta().changes().len(), 2);
    assert_eq!(
        (
            report.plan().resource_delta().changes()[0].key(),
            report.plan().resource_delta().changes()[0].delta(),
        ),
        (ResourceKey::Gold, 10)
    );
    assert_eq!(
        (
            report.plan().resource_delta().changes()[1].key(),
            report.plan().resource_delta().changes()[1].delta(),
        ),
        (ResourceKey::Item { item_id: 2001 }, 2)
    );
}

#[test]
fn inventory_dismantle_conflicts_with_a_kept_ship_slot() {
    let state = plan_game_state();
    let source_slot = slot(9001, 1);
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        source_slot,
        SlotTarget::Keep,
        0,
    )])
    .unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::ShipSlot(source_slot),
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::InventorySourceConflict {
            source_ref: EquipmentSourceRef::ShipSlot(source_slot),
        })
    );
}

#[test]
fn inventory_dismantle_reserves_a_warehouse_source_from_loadout_use() {
    let state = plan_game_state();
    let source = EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap());
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(source),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    let action = EquipmentInventoryAction::new(
        source,
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::InventorySourceConflict { source_ref: source })
    );
}

#[test]
fn inventory_dismantle_excludes_the_whole_warehouse_source_from_auto_selection() {
    let state = plan_game_state();
    let desired_equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::WarehouseOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(desired_equipment),
        0,
    )])
    .unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::EnhanceDowngrade {
            source_level: 1,
            target_level: 0,
        })
    );
}

#[test]
fn inventory_keep_generates_requested_enhancement_step() {
    let state = plan_game_state();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(1),
    )
    .unwrap();

    let report = compile_plan_with_inventory(&state, &desired, &inventory_plan(action))
        .expect("有效的相邻强化请求应生成单级强化步骤");

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Enhance {
            sequence: 1,
            source: PlanSource::Warehouse { config_id: 1000 },
            source_equipment: PlanEquipment {
                config_id: 1000,
                enhance_level: 0,
                ..
            },
            target_equipment: PlanEquipment {
                config_id: 1001,
                enhance_level: 1,
                ..
            },
            ..
        }]
    ));
}

#[test]
fn inventory_enhance_tracks_each_level_cost_and_final_warehouse_output() {
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let desired = DesiredState::new(Vec::new()).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(2)),
        Some(1),
    )
    .unwrap();

    let report = compile_plan_with_inventory(&state, &desired, &inventory_plan(action))
        .expect("两段强化链和聚合成本均满足时应生成连续步骤");
    let [
        PlanStep::Enhance {
            sequence: 1,
            source: PlanSource::Warehouse { config_id: 1000 },
            source_equipment: first_source,
            target_equipment: first_target,
            cost: first_cost,
        },
        PlanStep::Enhance {
            sequence: 2,
            source: PlanSource::Warehouse { config_id: 1001 },
            source_equipment: second_source,
            target_equipment: second_target,
            cost: second_cost,
        },
    ] = report.plan().steps()
    else {
        panic!("库存连续强化必须逐级生成仓库步骤");
    };
    assert_eq!(
        (first_source.config_id(), first_target.config_id()),
        (1000, 1001)
    );
    assert_eq!(
        (second_source.config_id(), second_target.config_id()),
        (1001, 1002)
    );
    assert_eq!(first_cost.gold(), 10);
    assert_eq!(
        first_cost
            .materials()
            .iter()
            .map(|material| (material.item_id(), material.quantity()))
            .collect::<Vec<_>>(),
        vec![(3001, 2)]
    );
    assert_eq!(second_cost.gold(), 20);
    assert_eq!(
        second_cost
            .materials()
            .iter()
            .map(|material| (material.item_id(), material.quantity()))
            .collect::<Vec<_>>(),
        vec![(3001, 3)]
    );
    assert_eq!(
        report
            .plan()
            .resource_constraints()
            .iter()
            .map(|constraint| (
                constraint.key(),
                constraint.available(),
                constraint.required(),
                constraint.remaining(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (ResourceKey::WarehouseEquipment { config_id: 1000 }, 1, 1, 0,),
            (ResourceKey::Gold, 100, 30, 70),
            (ResourceKey::Item { item_id: 3001 }, 10, 5, 5),
        ]
    );
    assert_eq!(
        report
            .plan()
            .resource_delta()
            .changes()
            .iter()
            .map(|change| (change.key(), change.delta()))
            .collect::<Vec<_>>(),
        vec![
            (ResourceKey::WarehouseEquipment { config_id: 1000 }, -1),
            (ResourceKey::WarehouseEquipment { config_id: 1002 }, 1),
            (ResourceKey::Gold, -30),
            (ResourceKey::Item { item_id: 3001 }, -5),
        ]
    );
}

#[test]
fn inventory_enhance_rejects_aggregate_gold_shortage() {
    let state = plan_game_state_with_enhance(29, 10, 3, 300);
    let desired = DesiredState::new(Vec::new()).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(2)),
        Some(1),
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(&state, &desired, &inventory_plan(action)),
        Err(PlanCheckError::EnhanceResourceUnavailable {
            source_config_id: 1000,
            key: ResourceKey::Gold,
            available: 29,
            required: 30,
        })
    );
}

#[test]
fn matching_current_equipment_is_enhanced_in_place_across_each_level() {
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(2)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 1),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Enhance {
                sequence: 1,
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                source_equipment: PlanEquipment {
                    config_id: 1000,
                    enhance_level: 0,
                    ..
                },
                target_equipment: PlanEquipment {
                    config_id: 1001,
                    enhance_level: 1,
                    ..
                },
                ..
            },
            PlanStep::Enhance {
                sequence: 2,
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                source_equipment: PlanEquipment {
                    config_id: 1001,
                    enhance_level: 1,
                    ..
                },
                target_equipment: PlanEquipment {
                    config_id: 1002,
                    enhance_level: 2,
                    ..
                },
                ..
            }
        ]
    ));
    assert_eq!(
        report
            .plan()
            .resource_delta()
            .changes()
            .iter()
            .map(|change| (change.key(), change.delta()))
            .collect::<Vec<_>>(),
        vec![
            (ResourceKey::Gold, -30),
            (ResourceKey::Item { item_id: 3001 }, -5),
        ]
    );
}

#[test]
fn external_ship_source_is_unloaded_enhanced_in_warehouse_and_equipped() {
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(slot(9001, 1))),
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                sequence: 1,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
            },
            PlanStep::Enhance {
                sequence: 2,
                source: PlanSource::Warehouse { config_id: 1000 },
                source_equipment: PlanEquipment {
                    config_id: 1000,
                    enhance_level: 0,
                    ..
                },
                target_equipment: PlanEquipment {
                    config_id: 1001,
                    enhance_level: 1,
                    ..
                },
                ..
            },
            PlanStep::Equip {
                sequence: 3,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 2,
                },
                source: PlanSource::Warehouse { config_id: 1001 },
                equipment: PlanEquipment {
                    config_id: 1001,
                    enhance_level: 1,
                    ..
                },
            }
        ]
    ));
}

#[test]
fn composed_equipment_is_enhanced_before_it_is_equipped() {
    let state = plan_game_state_with_compose_and_enhance(1_000, 20, 10, 3, 300, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Compose {
                sequence: 1,
                recipe_id: 5001,
                quantity: 1,
                ..
            },
            PlanStep::Enhance {
                sequence: 2,
                source: PlanSource::Warehouse { config_id: 1000 },
                source_equipment: PlanEquipment {
                    config_id: 1000,
                    enhance_level: 0,
                    ..
                },
                target_equipment: PlanEquipment {
                    config_id: 1001,
                    enhance_level: 1,
                    ..
                },
                ..
            },
            PlanStep::Equip {
                sequence: 3,
                source: PlanSource::Warehouse { config_id: 1001 },
                equipment: PlanEquipment {
                    config_id: 1001,
                    enhance_level: 1,
                    ..
                },
                ..
            }
        ]
    ));
    assert_eq!(
        report
            .plan()
            .resource_constraints()
            .iter()
            .map(|constraint| (constraint.key(), constraint.required()))
            .collect::<Vec<_>>(),
        vec![
            (ResourceKey::Gold, 110),
            (ResourceKey::Item { item_id: 2001 }, 5),
            (ResourceKey::Item { item_id: 3001 }, 2),
        ]
    );
    assert_eq!(
        report
            .plan()
            .resource_delta()
            .changes()
            .iter()
            .map(|change| (change.key(), change.delta()))
            .collect::<Vec<_>>(),
        vec![
            (ResourceKey::Gold, -110),
            (ResourceKey::Item { item_id: 2001 }, -5),
            (ResourceKey::Item { item_id: 3001 }, -2),
        ]
    );
}

#[test]
fn exact_warehouse_source_generates_constraint_and_equip_step() {
    let state = plan_game_state();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::Warehouse(
            crate::domain::EquipmentConfigId::new(1001).unwrap(),
        )),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert_eq!(report.plan().resource_constraints().len(), 1);
    let constraint = report.plan().resource_constraints()[0];
    assert_eq!(
        constraint.key(),
        ResourceKey::WarehouseEquipment { config_id: 1001 }
    );
    assert_eq!(constraint.available(), 2);
    assert_eq!(constraint.required(), 1);
    assert_eq!(constraint.remaining(), 1);
    assert_eq!(report.plan().resource_delta().changes()[0].delta(), -1);
    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Equip {
            source: PlanSource::Warehouse { config_id: 1001 },
            ..
        }]
    ));
    assert_eq!(report.plan().steps()[0].sequence(), 1);
}

#[test]
fn exact_ship_source_clears_source_before_equipping_target() {
    let state = plan_game_state();
    let source_slot = slot(9001, 1);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(source_slot)),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip { sequence: 1, .. },
            PlanStep::Equip {
                sequence: 2,
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                ..
            }
        ]
    ));
    assert!(report.plan().resource_delta().is_empty());
}

#[test]
fn incompatible_equipment_type_is_reported_for_every_source_path() {
    let state = plan_game_state_with_equipment_compatibility(vec![2], Vec::new(), [1, 1, 1], true);
    let family_id = EquipmentFamilyId::new(1000).unwrap();
    let cases = [
        (
            "当前装备",
            slot(9001, 1),
            DesiredEquipment::new(
                family_id,
                SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
                None,
                None,
            )
            .unwrap(),
        ),
        (
            "指定仓库",
            slot(9001, 2),
            DesiredEquipment::new(
                family_id,
                SourcePolicy::ExactSource,
                Some(EquipmentSourceRef::Warehouse(
                    EquipmentConfigId::new(1001).unwrap(),
                )),
                None,
            )
            .unwrap(),
        ),
        (
            "指定舰船",
            slot(9001, 2),
            DesiredEquipment::new(
                family_id,
                SourcePolicy::ExactSource,
                Some(EquipmentSourceRef::ShipSlot(slot(9001, 4))),
                None,
            )
            .unwrap(),
        ),
        (
            "自动仓库",
            slot(9001, 2),
            DesiredEquipment::new(family_id, SourcePolicy::WarehouseOnly, None, None).unwrap(),
        ),
        (
            "自动舰船",
            slot(9001, 2),
            DesiredEquipment::new(family_id, SourcePolicy::ShipOnly, None, None).unwrap(),
        ),
        (
            "装备合成",
            slot(9001, 2),
            DesiredEquipment::new(family_id, SourcePolicy::ComposeOnly, None, None).unwrap(),
        ),
    ];

    for (source_path, target_slot, equipment) in cases {
        let desired = DesiredState::new(vec![DesiredSlotState::new(
            target_slot,
            SlotTarget::Equipment(equipment),
            0,
        )])
        .unwrap();

        assert_eq!(
            compile_plan(&state, &desired),
            Err(PlanCheckError::EquipmentIncompatible {
                slot: target_slot,
                config_id: EquipmentConfigId::new(1000).unwrap(),
                equipment_type_id: 1,
                ship_type_id: 1,
                allowed_equipment_type_ids: vec![2],
                forbidden_ship_type_ids: Vec::new(),
            }),
            "{source_path}路径必须返回兼容性错误"
        );
    }
}

#[test]
fn automatic_warehouse_skips_an_incompatible_config_in_the_same_family() {
    let state = plan_game_state_with_equipment_compatibility(vec![1], Vec::new(), [2, 1, 1], false);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::WarehouseOnly,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Equip {
            source: PlanSource::Warehouse { config_id: 1001 },
            equipment: PlanEquipment {
                config_id: 1001,
                ..
            },
            ..
        }]
    ));
}

#[test]
fn automatic_ship_source_skips_an_incompatible_config() {
    let state = plan_game_state_with_equipment_compatibility(vec![1], Vec::new(), [2, 1, 1], false);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ShipOnly,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                ..
            },
            PlanStep::Equip {
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                equipment: PlanEquipment {
                    config_id: 1001,
                    ..
                },
                ..
            }
        ]
    ));
}

#[test]
fn current_policy_falls_back_when_the_existing_config_is_incompatible() {
    let state = plan_game_state_with_equipment_compatibility(vec![1], Vec::new(), [2, 1, 1], false);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 1),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                ..
            },
            PlanStep::Equip {
                source: PlanSource::Warehouse { config_id: 1001 },
                equipment: PlanEquipment {
                    config_id: 1001,
                    ..
                },
                ..
            }
        ]
    ));
}

#[test]
fn current_policy_preserves_incompatibility_when_no_fallback_source_exists() {
    let state = plan_game_state_with_current_equipment_only(vec![1], [2, 1, 1]);
    let target_slot = slot(9001, 1);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        target_slot,
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::EquipmentIncompatible {
            slot: target_slot,
            config_id: EquipmentConfigId::new(1000).unwrap(),
            equipment_type_id: 2,
            ship_type_id: 1,
            allowed_equipment_type_ids: vec![1],
            forbidden_ship_type_ids: Vec::new(),
        })
    );
}

#[test]
fn automatic_policy_continues_after_an_incompatible_compose_candidate() {
    let state = plan_game_state_with_equipment_compatibility(vec![1], Vec::new(), [2, 1, 1], true);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeThenWarehouseThenShip,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Equip {
            source: PlanSource::Warehouse { config_id: 1001 },
            ..
        }]
    ));
}

#[test]
fn exact_and_compose_sources_report_the_selected_incompatible_config() {
    let state = plan_game_state_with_equipment_compatibility(vec![1], Vec::new(), [2, 1, 1], true);
    let target_slot = slot(9001, 2);
    let family_id = EquipmentFamilyId::new(1000).unwrap();
    let cases = [
        DesiredEquipment::new(
            family_id,
            SourcePolicy::ExactSource,
            Some(EquipmentSourceRef::Warehouse(
                EquipmentConfigId::new(1000).unwrap(),
            )),
            None,
        )
        .unwrap(),
        DesiredEquipment::new(family_id, SourcePolicy::ComposeOnly, None, None).unwrap(),
    ];

    for equipment in cases {
        let desired = DesiredState::new(vec![DesiredSlotState::new(
            target_slot,
            SlotTarget::Equipment(equipment),
            0,
        )])
        .unwrap();

        assert_eq!(
            compile_plan(&state, &desired),
            Err(PlanCheckError::EquipmentIncompatible {
                slot: target_slot,
                config_id: EquipmentConfigId::new(1000).unwrap(),
                equipment_type_id: 2,
                ship_type_id: 1,
                allowed_equipment_type_ids: vec![1],
                forbidden_ship_type_ids: Vec::new(),
            })
        );
    }
}

#[test]
fn compose_snapshot_mismatch_precedes_candidate_incompatibility() {
    let state = plan_game_state_with_incompatible_compose_mismatch();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::ComposeRecipeMismatch { recipe_id: 5001 })
    );
}

#[test]
fn forbidden_ship_type_rejects_the_explicit_target_config() {
    let state = plan_game_state_with_equipment_compatibility(vec![1], vec![1], [1, 1, 1], false);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::Warehouse(
            EquipmentConfigId::new(1001).unwrap(),
        )),
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    let target_slot = slot(9001, 2);
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        target_slot,
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::EquipmentIncompatible {
            slot: target_slot,
            config_id: EquipmentConfigId::new(1001).unwrap(),
            equipment_type_id: 1,
            ship_type_id: 1,
            allowed_equipment_type_ids: vec![1],
            forbidden_ship_type_ids: vec![1],
        })
    );
}

#[test]
fn exchange_ship_sources_unloads_both_slots_before_equipping() {
    let state = plan_game_state();
    let first_slot = slot(9001, 1);
    let second_slot = slot(9001, 4);
    let first_equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(second_slot)),
        None,
    )
    .unwrap();
    let second_equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(first_slot)),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(first_slot, SlotTarget::Equipment(first_equipment), 0),
        DesiredSlotState::new(second_slot, SlotTarget::Equipment(second_equipment), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                sequence: 1,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
            },
            PlanStep::Unequip {
                sequence: 2,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
            },
            PlanStep::Equip {
                sequence: 3,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                ..
            },
            PlanStep::Equip {
                sequence: 4,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
                ..
            },
        ]
    ));
}

#[test]
fn keep_target_cannot_be_used_as_a_ship_source() {
    let state = plan_game_state();
    let kept_slot = slot(9001, 1);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(kept_slot)),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(kept_slot, SlotTarget::Keep, 0),
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(equipment), 1),
    ])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::SourceTargetConflict {
            source_ref: kept_slot,
        })
    );
}

#[test]
fn automatic_ship_source_can_use_a_movable_target_slot() {
    let state = plan_game_state();
    let source_slot = slot(9001, 4);
    let target_slot = slot(9001, 2);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ShipOnly,
        None,
        Some(crate::domain::EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(target_slot, SlotTarget::Equipment(equipment), 0),
        DesiredSlotState::new(source_slot, SlotTarget::Empty, 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                sequence: 1,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
            },
            PlanStep::Equip {
                sequence: 2,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 2,
                },
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                ..
            },
        ]
    ));
}

#[test]
fn all_unloads_precede_equipment_steps() {
    let state = plan_game_state();
    let source_slot = slot(9001, 1);
    let replaced_slot = slot(9001, 4);
    let ship_equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(source_slot)),
        None,
    )
    .unwrap();
    let warehouse_equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::Warehouse(
            crate::domain::EquipmentConfigId::new(1001).unwrap(),
        )),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(ship_equipment), 0),
        DesiredSlotState::new(replaced_slot, SlotTarget::Equipment(warehouse_equipment), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip { .. },
            PlanStep::Unequip { .. },
            PlanStep::Equip { .. },
            PlanStep::Equip { .. },
        ]
    ));
    assert_eq!(report.plan().steps()[0].sequence(), 1);
    assert_eq!(report.plan().steps()[1].sequence(), 2);
    assert_eq!(report.plan().steps()[2].sequence(), 3);
    assert_eq!(report.plan().steps()[3].sequence(), 4);
    assert_eq!(report.plan().resource_constraints().len(), 1);
}

#[test]
fn report_serialization_contains_version_steps_constraints_and_hashes() {
    let state = plan_game_state();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::Warehouse(
            crate::domain::EquipmentConfigId::new(1001).unwrap(),
        )),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();
    let value = serde_json::to_value(&report).unwrap();

    assert_eq!(value["plan"]["schema_version"], 5);
    assert_eq!(value["plan"]["steps"][0]["Equip"]["sequence"], 1);
    assert_eq!(value["plan"]["resource_constraints"][0]["required"], 1);
    assert_eq!(value["plan"]["content_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        value["plan"]["inventory_plan_content_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(value["checked_slots"], 1);
    assert_eq!(value["checked_inventory_actions"], 0);
}

#[test]
fn warehouse_quantity_is_checked_across_targets() {
    let state = plan_game_state();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::Warehouse(
            crate::domain::EquipmentConfigId::new(1001).unwrap(),
        )),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(equipment), 0),
        DesiredSlotState::new(slot(9001, 3), SlotTarget::Equipment(equipment), 1),
        DesiredSlotState::new(slot(9001, 4), SlotTarget::Equipment(equipment), 2),
    ])
    .unwrap();

    let error = compile_plan(&state, &desired).unwrap_err();

    assert_eq!(
        error,
        PlanCheckError::SourceUnavailable {
            source_ref: EquipmentSourceRef::Warehouse(
                crate::domain::EquipmentConfigId::new(1001).unwrap(),
            ),
            available: 2,
            required: 3,
        }
    );
}

#[test]
fn warehouse_auto_source_skips_lower_level_until_exact_match() {
    let state = plan_game_state();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::WarehouseOnly,
        None,
        Some(crate::domain::EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 3),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Equip {
            source: PlanSource::Warehouse { config_id: 1001 },
            ..
        }]
    ));
}

#[test]
fn ship_auto_source_skips_lower_level_until_exact_match() {
    let state = plan_game_state();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ShipOnly,
        None,
        Some(crate::domain::EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 3),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Unequip {
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                ..
            },
            PlanStep::Equip {
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                ..
            }
        ]
    ));
}

#[test]
fn compose_only_reports_a_missing_recipe_instead_of_being_treated_as_success() {
    let state = plan_game_state();
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::ComposeRecipeNotFound {
            family_id: EquipmentFamilyId::new(1000).unwrap(),
        })
    );
}

#[test]
fn compose_only_aggregates_recipe_steps_and_reserves_initial_resources() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(equipment), 0),
        DesiredSlotState::new(slot(9001, 3), SlotTarget::Equipment(equipment), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert_eq!(report.plan().schema_version(), 5);
    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Compose {
                sequence: 1,
                recipe_id: 5001,
                quantity: 2,
                material_id: 2001,
                material_quantity_per_unit: 5,
                gold_per_unit: 100,
                ..
            },
            PlanStep::Equip {
                sequence: 2,
                source: PlanSource::Compose { recipe_id: 5001 },
                ..
            },
            PlanStep::Equip {
                sequence: 3,
                source: PlanSource::Compose { recipe_id: 5001 },
                ..
            }
        ]
    ));
    let constraints = report.plan().resource_constraints();
    assert_eq!(constraints.len(), 2);
    assert_eq!(constraints[0].key(), ResourceKey::Gold);
    assert_eq!(constraints[0].available(), 1_000);
    assert_eq!(constraints[0].required(), 200);
    assert_eq!(constraints[0].remaining(), 800);
    assert_eq!(constraints[1].key(), ResourceKey::Item { item_id: 2001 });
    assert_eq!(constraints[1].available(), 20);
    assert_eq!(constraints[1].required(), 10);
    assert_eq!(constraints[1].remaining(), 10);
    assert_eq!(
        report
            .plan()
            .resource_delta()
            .changes()
            .iter()
            .map(|change| (change.key(), change.delta()))
            .collect::<Vec<_>>(),
        vec![
            (ResourceKey::Gold, -200),
            (ResourceKey::Item { item_id: 2001 }, -10)
        ]
    );
}

#[test]
fn compose_scheduler_preserves_target_order_across_recipes() {
    let mut reservations = ComposeReservations::default();
    reservations.by_recipe.insert(
        5001,
        ValidatedCompose {
            recipe_id: 5001,
            equipment: PlanEquipment::from_raw(1000, 1000, 0),
            quantity: 2,
            material_id: 2001,
            material_quantity_per_unit: 5,
            gold_per_unit: 100,
        },
    );
    reservations.by_recipe.insert(
        5002,
        ValidatedCompose {
            recipe_id: 5002,
            equipment: PlanEquipment::from_raw(2000, 2000, 0),
            quantity: 1,
            material_id: 2002,
            material_quantity_per_unit: 3,
            gold_per_unit: 50,
        },
    );
    let equip_plans = vec![
        PlannedCompose {
            recipe_id: 5001,
            enhancement_steps: Vec::new(),
            equip_step: PlanStep::Equip {
                sequence: 0,
                slot: PlanSlot::from_raw(9001, 1),
                source: PlanSource::Compose { recipe_id: 5001 },
                equipment: PlanEquipment::from_raw(1000, 1000, 0),
            },
        },
        PlannedCompose {
            recipe_id: 5002,
            enhancement_steps: Vec::new(),
            equip_step: PlanStep::Equip {
                sequence: 0,
                slot: PlanSlot::from_raw(9001, 2),
                source: PlanSource::Compose { recipe_id: 5002 },
                equipment: PlanEquipment::from_raw(2000, 2000, 0),
            },
        },
        PlannedCompose {
            recipe_id: 5001,
            enhancement_steps: Vec::new(),
            equip_step: PlanStep::Equip {
                sequence: 0,
                slot: PlanSlot::from_raw(9001, 3),
                source: PlanSource::Compose { recipe_id: 5001 },
                equipment: PlanEquipment::from_raw(1000, 1000, 0),
            },
        },
    ];

    let steps = schedule_compose_steps(&reservations, &equip_plans, 1).unwrap();

    assert!(matches!(
        steps.as_slice(),
        [
            PlanStep::Compose {
                recipe_id: 5001,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                slot: PlanSlot { slot_index: 1, .. },
                ..
            },
            PlanStep::Compose {
                recipe_id: 5002,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                slot: PlanSlot { slot_index: 2, .. },
                ..
            },
            PlanStep::Compose {
                recipe_id: 5001,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                slot: PlanSlot { slot_index: 3, .. },
                ..
            }
        ]
    ));
}

#[test]
fn compose_only_rejects_aggregate_gold_shortage() {
    let state = plan_game_state_with_compose(199, 20, 3, 300, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(equipment), 0),
        DesiredSlotState::new(slot(9001, 3), SlotTarget::Equipment(equipment), 1),
    ])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::ComposeResourceUnavailable {
            recipe_id: 5001,
            key: ResourceKey::Gold,
            available: 199,
            required: 200,
        })
    );
}

#[test]
fn compose_capacity_is_checked_after_planned_dismantle() {
    let state = plan_game_state_with_compose(100, 5, 3, 3, Some(1));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    assert_eq!(
        compile_plan(&state, &desired),
        Err(PlanCheckError::ComposeEquipmentCapacityUnavailable {
            available: 0,
            required: 1,
        })
    );

    let dismantle = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();
    let report = compile_plan_with_inventory(
        &state,
        &desired,
        &EquipmentInventoryPlan::new(vec![dismantle]).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Dismantle { sequence: 1, .. },
            PlanStep::Compose { sequence: 2, .. },
            PlanStep::Equip { sequence: 3, .. }
        ]
    ));
    assert_eq!(
        report
            .plan()
            .resource_constraints()
            .iter()
            .find(|constraint| constraint.key() == ResourceKey::Item { item_id: 2001 })
            .map(|constraint| constraint.required()),
        Some(5)
    );
}

#[test]
fn current_first_keeps_a_matching_slot_without_consuming_external_resources() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 1),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Keep { sequence: 1, .. }]
    ));
    assert!(report.plan().resource_constraints().is_empty());
    assert!(report.plan().resource_delta().is_empty());
}

#[test]
fn current_first_matching_slot_cannot_be_borrowed_by_an_earlier_target() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let keep_current = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let move_from_ship = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ShipOnly,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(move_from_ship), 0),
        DesiredSlotState::new(slot(9001, 1), SlotTarget::Equipment(keep_current), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Keep {
                sequence: 1,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 1,
                },
            },
            PlanStep::Unequip {
                sequence: 2,
                slot: PlanSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
            },
            PlanStep::Equip {
                sequence: 3,
                source: PlanSource::ShipSlot {
                    ship_instance_id: 9001,
                    slot_index: 4,
                },
                ..
            }
        ]
    ));
}

#[test]
fn current_first_matching_slot_conflicts_with_inventory_dismantle() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let source_slot = slot(9001, 1);
    let keep_current = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        source_slot,
        SlotTarget::Equipment(keep_current),
        0,
    )])
    .unwrap();
    let dismantle = EquipmentInventoryAction::new(
        EquipmentSourceRef::ShipSlot(source_slot),
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        compile_plan_with_inventory(
            &state,
            &desired,
            &EquipmentInventoryPlan::new(vec![dismantle]).unwrap(),
        ),
        Err(PlanCheckError::InventorySourceConflict {
            source_ref: EquipmentSourceRef::ShipSlot(source_slot),
        })
    );
}

#[test]
fn compose_preference_falls_back_to_warehouse_when_no_batch_can_start() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 3, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeThenWarehouseThenShip,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Equip {
            sequence: 1,
            source: PlanSource::Warehouse { config_id: 1000 },
            ..
        }]
    ));
}

#[test]
fn existing_source_equip_frees_capacity_before_compose() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 3, Some(4));
    let warehouse = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::WarehouseOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let compose = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(warehouse), 0),
        DesiredSlotState::new(slot(9001, 3), SlotTarget::Equipment(compose), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Equip {
                sequence: 1,
                source: PlanSource::Warehouse { config_id: 1000 },
                ..
            },
            PlanStep::Compose {
                sequence: 2,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                sequence: 3,
                source: PlanSource::Compose { recipe_id: 5001 },
                ..
            }
        ]
    ));
}

#[test]
fn compose_is_split_into_batches_that_fit_the_available_capacity() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 4, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(equipment), 0),
        DesiredSlotState::new(slot(9001, 3), SlotTarget::Equipment(equipment), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Compose {
                sequence: 1,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                sequence: 2,
                source: PlanSource::Compose { recipe_id: 5001 },
                ..
            },
            PlanStep::Compose {
                sequence: 3,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                sequence: 4,
                source: PlanSource::Compose { recipe_id: 5001 },
                ..
            }
        ]
    ));
}

#[test]
fn warehouse_then_compose_uses_compose_before_a_ship_fallback() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::WarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Equipment(equipment), 0),
        DesiredSlotState::new(slot(9001, 3), SlotTarget::Equipment(equipment), 1),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [
            PlanStep::Equip {
                sequence: 1,
                source: PlanSource::Warehouse { config_id: 1000 },
                ..
            },
            PlanStep::Compose {
                sequence: 2,
                quantity: 1,
                ..
            },
            PlanStep::Equip {
                sequence: 3,
                source: PlanSource::Compose { recipe_id: 5001 },
                ..
            }
        ]
    ));
}

#[test]
fn keep_and_empty_targets_do_not_revalidate_existing_equipment() {
    let state = plan_game_state_with_equipment_compatibility(vec![2], vec![1], [1, 1, 1], false);
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(slot(9001, 1), SlotTarget::Keep, 0),
        DesiredSlotState::new(slot(9001, 2), SlotTarget::Empty, 0),
    ])
    .unwrap();

    let report = compile_plan(&state, &desired).unwrap();

    assert!(matches!(
        report.plan().steps(),
        [PlanStep::Keep { .. }, PlanStep::Keep { .. }]
    ));
}

#[test]
fn warehouse_then_compose_never_uses_another_ship_as_fallback() {
    let state = plan_game_state_with_current_equipment_only(vec![1], [1, 1, 1]);
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(
            DesiredEquipment::new(
                EquipmentFamilyId::new(1000).unwrap(),
                SourcePolicy::WarehouseThenCompose,
                None,
                None,
            )
            .unwrap(),
        ),
        0,
    )])
    .unwrap();
    assert!(compile_plan(&state, &desired).is_err());
}

#[test]
fn warehouse_then_compose_keeps_a_current_equipment_that_already_satisfies_the_target() {
    let state = plan_game_state_with_current_equipment_only(vec![1], [1, 1, 1]);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::WarehouseThenCompose,
        None,
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 1),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    let report = compile_plan(&state, &desired).unwrap();
    assert!(matches!(report.plan().steps(), [PlanStep::Keep { .. }]));
}

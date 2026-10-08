//! 构造逐步回读证据并比较装备、仓库、物资和材料状态。

use std::collections::BTreeSet;

use crate::domain::GameState;

use super::preflight::{
    EquipmentStateSnapshot, ResourceStateSnapshot, equipment_state_snapshot,
    resource_state_snapshot,
};
use super::{
    ExecutionAction, ExecutionGoldReadback, ExecutionMaterialReadback, ExecutionReadbackEvidence,
    ExecutionSlotReadback, ExecutionSlotState, ExecutionStateMismatch, ExecutionWarehouseReadback,
    source_summary,
};
use crate::application::{PlanSlot, PlanSource};

pub(super) fn build_readback_evidence(
    action: &ExecutionAction,
    before: &EquipmentStateSnapshot,
    expected_after: &EquipmentStateSnapshot,
    before_resources: &ResourceStateSnapshot,
    expected_after_resources: &ResourceStateSnapshot,
    verify_resources: bool,
    actual_after: &GameState,
) -> ExecutionReadbackEvidence {
    let actual_equipment = equipment_state_snapshot(actual_after);
    let actual_resources = resource_state_snapshot(actual_after);
    let dismantled_source_quantity = match action {
        ExecutionAction::Dismantle { quantity, .. } => Some(*quantity),
        ExecutionAction::Unequip { .. }
        | ExecutionAction::Equip { .. }
        | ExecutionAction::Compose { .. }
        | ExecutionAction::Enhance { .. } => None,
    };
    let composed_output_quantity = match action {
        ExecutionAction::Compose { quantity, .. } => Some(*quantity),
        ExecutionAction::Unequip { .. }
        | ExecutionAction::Equip { .. }
        | ExecutionAction::Dismantle { .. }
        | ExecutionAction::Enhance { .. } => None,
    };
    let enhanced_target_quantity = matches!(action, ExecutionAction::Enhance { .. }).then_some(1);
    let (target, source, source_slot, warehouse_config_ids) = match action {
        ExecutionAction::Unequip { slot } => (
            Some(*slot),
            None,
            None,
            before
                .slots
                .get(&(slot.ship_instance_id(), slot.slot_index()))
                .and_then(|equipment| *equipment)
                .map(|equipment| equipment.config_id)
                .into_iter()
                .collect(),
        ),
        ExecutionAction::Equip {
            slot,
            source,
            equipment,
        } => {
            let source_slot = match source {
                PlanSource::Warehouse { .. } | PlanSource::Compose { .. } => None,
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => Some(PlanSlot::from_raw(*ship_instance_id, *slot_index)),
            };
            (
                Some(*slot),
                Some(*source),
                source_slot,
                BTreeSet::from([equipment.config_id()]),
            )
        }
        ExecutionAction::Dismantle {
            source, equipment, ..
        } => {
            let source_slot = match source {
                PlanSource::Warehouse { .. } | PlanSource::Compose { .. } => None,
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => Some(PlanSlot::from_raw(*ship_instance_id, *slot_index)),
            };
            (
                None,
                Some(*source),
                source_slot,
                BTreeSet::from([equipment.config_id()]),
            )
        }
        ExecutionAction::Compose {
            recipe_id,
            equipment,
            ..
        } => (
            None,
            Some(PlanSource::Compose {
                recipe_id: *recipe_id,
            }),
            None,
            BTreeSet::from([equipment.config_id()]),
        ),
        ExecutionAction::Enhance {
            source,
            source_equipment,
            target_equipment,
            ..
        } => {
            let source_slot = match source {
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => Some(PlanSlot::from_raw(*ship_instance_id, *slot_index)),
                PlanSource::Warehouse { .. } | PlanSource::Compose { .. } => None,
            };
            let warehouse_config_ids = match source {
                PlanSource::Warehouse { .. } => {
                    BTreeSet::from([source_equipment.config_id(), target_equipment.config_id()])
                }
                PlanSource::ShipSlot { .. } | PlanSource::Compose { .. } => BTreeSet::new(),
            };
            (
                source_slot,
                Some(*source),
                source_slot,
                warehouse_config_ids,
            )
        }
    };
    let first_mismatch =
        first_equipment_mismatch(expected_after, &actual_equipment).or_else(|| {
            verify_resources
                .then(|| first_resource_mismatch(expected_after_resources, &actual_resources))
                .flatten()
        });
    let resource_action = dismantled_source_quantity.is_some()
        || composed_output_quantity.is_some()
        || enhanced_target_quantity.is_some();
    let gold = resource_action.then_some(ExecutionGoldReadback {
        before: before_resources.gold,
        expected_after: expected_after_resources.gold,
        actual_after: actual_resources.gold,
    });
    let materials = if resource_action {
        before_resources
            .materials
            .keys()
            .chain(expected_after_resources.materials.keys())
            .chain(actual_resources.materials.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|item_id| {
                let before_quantity = material_quantity(before_resources, item_id);
                let expected_after_quantity = material_quantity(expected_after_resources, item_id);
                let actual_after_quantity = material_quantity(&actual_resources, item_id);
                (before_quantity != expected_after_quantity
                    || actual_after_quantity != expected_after_quantity)
                    .then_some(ExecutionMaterialReadback {
                        item_id,
                        before_quantity,
                        expected_after_quantity,
                        actual_after_quantity,
                    })
            })
            .collect()
    } else {
        Vec::new()
    };
    ExecutionReadbackEvidence {
        matches_expected: first_mismatch.is_none(),
        target_slot: target
            .map(|slot| slot_readback(slot, before, expected_after, &actual_equipment)),
        source,
        source_slot: source_slot
            .map(|slot| slot_readback(slot, before, expected_after, &actual_equipment)),
        warehouse: warehouse_config_ids
            .into_iter()
            .map(|config_id| ExecutionWarehouseReadback {
                config_id,
                before_quantity: warehouse_snapshot_quantity(before, config_id),
                expected_after_quantity: warehouse_snapshot_quantity(expected_after, config_id),
                actual_after_quantity: warehouse_snapshot_quantity(&actual_equipment, config_id),
            })
            .collect(),
        gold,
        materials,
        dismantled_source_quantity,
        composed_output_quantity,
        enhanced_target_quantity,
        first_mismatch,
    }
}

fn slot_readback(
    slot: PlanSlot,
    before: &EquipmentStateSnapshot,
    expected_after: &EquipmentStateSnapshot,
    actual_after: &EquipmentStateSnapshot,
) -> ExecutionSlotReadback {
    ExecutionSlotReadback {
        slot,
        before: snapshot_slot_state(before, slot),
        expected_after: snapshot_slot_state(expected_after, slot),
        actual_after: snapshot_slot_state(actual_after, slot),
    }
}

fn snapshot_slot_state(state: &EquipmentStateSnapshot, slot: PlanSlot) -> ExecutionSlotState {
    match state
        .slots
        .get(&(slot.ship_instance_id(), slot.slot_index()))
    {
        None => ExecutionSlotState::Missing,
        Some(None) => ExecutionSlotState::Empty,
        Some(Some(equipment)) => ExecutionSlotState::Equipped(*equipment),
    }
}

fn warehouse_snapshot_quantity(state: &EquipmentStateSnapshot, config_id: u64) -> u64 {
    state.warehouse.get(&config_id).copied().unwrap_or(0)
}

fn material_quantity(state: &ResourceStateSnapshot, item_id: u64) -> u64 {
    state.materials.get(&item_id).copied().unwrap_or(0)
}

pub(super) fn first_equipment_mismatch(
    expected: &EquipmentStateSnapshot,
    actual: &EquipmentStateSnapshot,
) -> Option<ExecutionStateMismatch> {
    let slot_keys: BTreeSet<(u64, u8)> = expected
        .slots
        .keys()
        .chain(actual.slots.keys())
        .copied()
        .collect();
    for (ship_instance_id, slot_index) in slot_keys {
        let slot = PlanSlot::from_raw(ship_instance_id, slot_index);
        let expected_state = snapshot_slot_state(expected, slot);
        let actual_state = snapshot_slot_state(actual, slot);
        if expected_state != actual_state {
            return Some(ExecutionStateMismatch::Slot {
                slot,
                expected: expected_state,
                actual: actual_state,
            });
        }
    }
    let warehouse_config_ids: BTreeSet<u64> = expected
        .warehouse
        .keys()
        .chain(actual.warehouse.keys())
        .copied()
        .collect();
    for config_id in warehouse_config_ids {
        let expected_quantity = expected.warehouse.get(&config_id).copied().unwrap_or(0);
        let actual_quantity = actual.warehouse.get(&config_id).copied().unwrap_or(0);
        if expected_quantity != actual_quantity {
            return Some(ExecutionStateMismatch::Warehouse {
                config_id,
                expected_quantity,
                actual_quantity,
            });
        }
    }
    None
}

pub(super) fn first_resource_mismatch(
    expected: &ResourceStateSnapshot,
    actual: &ResourceStateSnapshot,
) -> Option<ExecutionStateMismatch> {
    if expected.gold != actual.gold {
        return Some(ExecutionStateMismatch::Gold {
            expected_quantity: expected.gold,
            actual_quantity: actual.gold,
        });
    }
    let material_ids: BTreeSet<u64> = expected
        .materials
        .keys()
        .chain(actual.materials.keys())
        .copied()
        .collect();
    for item_id in material_ids {
        let expected_quantity = material_quantity(expected, item_id);
        let actual_quantity = material_quantity(actual, item_id);
        if expected_quantity != actual_quantity {
            return Some(ExecutionStateMismatch::Material {
                item_id,
                expected_quantity,
                actual_quantity,
            });
        }
    }
    None
}

pub(super) fn readback_evidence_summary(
    action: &ExecutionAction,
    evidence: &ExecutionReadbackEvidence,
) -> String {
    if let Some(mismatch) = evidence.first_mismatch() {
        return state_mismatch_summary(mismatch);
    }
    match action {
        ExecutionAction::Unequip { slot } => format!(
            "舰船 {} 的槽位 {} 已为空；全装备状态与完整模拟一致",
            slot.ship_instance_id(),
            slot.slot_index()
        ),
        ExecutionAction::Equip {
            slot, equipment, ..
        } => format!(
            "舰船 {} 的槽位 {} 已装备配置 {}、强化等级 {}；全装备状态与完整模拟一致",
            slot.ship_instance_id(),
            slot.slot_index(),
            equipment.config_id(),
            equipment.enhance_level()
        ),
        ExecutionAction::Dismantle {
            equipment,
            quantity,
            ..
        } => {
            let gold_delta = evidence
                .gold()
                .map(|gold| {
                    gold.expected_after()
                        .checked_sub(gold.before())
                        .expect("拆解预期物资不得减少")
                })
                .unwrap_or(0);
            let materials = evidence
                .materials()
                .iter()
                .map(|material| {
                    format!(
                        "{}:+{}",
                        material.item_id(),
                        material
                            .expected_after_quantity()
                            .checked_sub(material.before_quantity())
                            .expect("拆解预期材料不得减少")
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "仓库配置 {} 已减少 {} 件；物资增加 {}，材料 [{}]；装备与资源状态均符合完整模拟",
                equipment.config_id(),
                quantity,
                gold_delta,
                materials
            )
        }
        ExecutionAction::Compose {
            recipe_id,
            equipment,
            quantity,
            ..
        } => {
            let gold_cost = evidence
                .gold()
                .map(|gold| {
                    gold.before()
                        .checked_sub(gold.expected_after())
                        .expect("合成预期物资不得增加")
                })
                .unwrap_or(0);
            let materials = evidence
                .materials()
                .iter()
                .map(|material| {
                    format!(
                        "{}:-{}",
                        material.item_id(),
                        material
                            .before_quantity()
                            .checked_sub(material.expected_after_quantity())
                            .expect("合成预期材料不得增加")
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "配方 {recipe_id} 已合成 {quantity} 件配置 {}；物资消耗 {gold_cost}，材料 [{materials}]；装备与资源状态均符合完整模拟",
                equipment.config_id()
            )
        }
        ExecutionAction::Enhance {
            source,
            source_equipment,
            target_equipment,
            ..
        } => {
            let gold_cost = evidence
                .gold()
                .map(|gold| {
                    gold.before()
                        .checked_sub(gold.expected_after())
                        .expect("强化预期物资不得增加")
                })
                .unwrap_or(0);
            let materials = evidence
                .materials()
                .iter()
                .map(|material| {
                    format!(
                        "{}:-{}",
                        material.item_id(),
                        material
                            .before_quantity()
                            .checked_sub(material.expected_after_quantity())
                            .expect("强化预期材料不得增加")
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{}中的配置 {}、强化等级 {} 已变为配置 {}、强化等级 {}；物资消耗 {gold_cost}，材料 [{materials}]；装备与资源状态均符合完整模拟",
                source_summary(*source),
                source_equipment.config_id(),
                source_equipment.enhance_level(),
                target_equipment.config_id(),
                target_equipment.enhance_level()
            )
        }
    }
}

pub(super) fn execution_state_match_summary(
    expected: &EquipmentStateSnapshot,
    resources_verified: bool,
) -> String {
    if resources_verified {
        format!(
            "独立终态已核对 {} 个舰船槽位、{} 类仓库装备以及物资和材料",
            expected.slots.len(),
            expected.warehouse.len()
        )
    } else {
        format!(
            "独立终态已核对 {} 个舰船槽位和 {} 类仓库装备",
            expected.slots.len(),
            expected.warehouse.len()
        )
    }
}

pub(super) fn state_mismatch_summary(mismatch: &ExecutionStateMismatch) -> String {
    match mismatch {
        ExecutionStateMismatch::Slot {
            slot,
            expected,
            actual,
        } => format!(
            "舰船 {} 槽位 {} 为 {}，期望 {}",
            slot.ship_instance_id(),
            slot.slot_index(),
            slot_state_summary(*actual),
            slot_state_summary(*expected)
        ),
        ExecutionStateMismatch::Warehouse {
            config_id,
            expected_quantity,
            actual_quantity,
        } => format!("仓库配置 {config_id} 数量为 {actual_quantity}，期望 {expected_quantity}"),
        ExecutionStateMismatch::Gold {
            expected_quantity,
            actual_quantity,
        } => format!("物资数量为 {actual_quantity}，期望 {expected_quantity}"),
        ExecutionStateMismatch::Material {
            item_id,
            expected_quantity,
            actual_quantity,
        } => format!("背包材料 {item_id} 数量为 {actual_quantity}，期望 {expected_quantity}"),
    }
}

fn slot_state_summary(state: ExecutionSlotState) -> String {
    match state {
        ExecutionSlotState::Missing => "缺少槽位".to_owned(),
        ExecutionSlotState::Empty => "空槽".to_owned(),
        ExecutionSlotState::Equipped(equipment) => format!(
            "配置 {}、强化等级 {}",
            equipment.config_id, equipment.enhance_level
        ),
    }
}

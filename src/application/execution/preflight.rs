//! 模拟计划执行前后的装备与资源状态并建立整批预演契约。

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::domain::{EnhanceLevel, EquipmentFamilyId, GameState};
use serde::Serialize;

use super::{
    AppError, AppErrorCode, ExecutionAction, ExecutionEquipmentState, ExecutionPreflight,
    ExecutionPreflightStep, ExecutionTargetIdentity,
};
use crate::application::{
    CompiledPlan, PlanEnhanceCost, PlanEnhanceMaterialCost, PlanEquipment, PlanSource, PlanStep,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct EquipmentStateSnapshot {
    pub(super) slots: BTreeMap<(u64, u8), Option<ExecutionEquipmentState>>,
    pub(super) warehouse: BTreeMap<u64, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct ResourceStateSnapshot {
    pub(super) gold: u64,
    pub(super) materials: BTreeMap<u64, u64>,
}

pub(super) fn equipment_state_snapshot(state: &GameState) -> EquipmentStateSnapshot {
    let slots = state
        .ships()
        .ships()
        .iter()
        .flat_map(|ship| {
            let ship_instance_id = ship.identity().instance_id().get();
            ship.slots().iter().map(move |slot| {
                (
                    (ship_instance_id, slot.index().get()),
                    slot.equipment().map(|equipment| ExecutionEquipmentState {
                        config_id: equipment.config_id().get(),
                        enhance_level: equipment.enhance_level().get(),
                    }),
                )
            })
        })
        .collect();
    let warehouse = state
        .equipment_inventory()
        .warehouse()
        .iter()
        .copied()
        .filter(|stack| stack.quantity() > 0)
        .map(|stack| (stack.config_id().get(), stack.quantity()))
        .collect();
    EquipmentStateSnapshot { slots, warehouse }
}

pub(super) fn resource_state_snapshot(state: &GameState) -> ResourceStateSnapshot {
    ResourceStateSnapshot {
        gold: state.resources().gold(),
        materials: state
            .bag()
            .items()
            .iter()
            .map(|item| (item.item_id(), item.quantity()))
            .collect(),
    }
}

fn validate_enhance_step(
    initial: &GameState,
    source_equipment: PlanEquipment,
    target_equipment: PlanEquipment,
    cost: &PlanEnhanceCost,
) -> Result<(), String> {
    if source_equipment.family_id() != target_equipment.family_id() {
        return Err("强化步骤的来源和目标装备族不一致".to_owned());
    }
    let family_id = EquipmentFamilyId::new(source_equipment.family_id())
        .map_err(|_| format!("强化计划包含无效装备族 {}", source_equipment.family_id()))?;
    let family = initial
        .equipment_catalog()
        .family(family_id)
        .ok_or_else(|| format!("装备目录缺少强化装备族 {}", family_id.get()))?;
    let source_level = EnhanceLevel::new(source_equipment.enhance_level());
    let target_level = EnhanceLevel::new(target_equipment.enhance_level());
    let expected_target_level = source_level
        .get()
        .checked_add(1)
        .ok_or_else(|| format!("配置 {} 的强化等级溢出", source_equipment.config_id()))?;
    if target_level.get() != expected_target_level {
        return Err(format!(
            "强化步骤不是相邻等级：来源 +{}，目标 +{}",
            source_level.get(),
            target_level.get()
        ));
    }
    let source_definition = family
        .config(source_level)
        .filter(|definition| {
            definition.identity().config_id().get() == source_equipment.config_id()
        })
        .ok_or_else(|| {
            format!(
                "装备目录缺少强化来源配置 {} 的等级 {}",
                source_equipment.config_id(),
                source_equipment.enhance_level()
            )
        })?;
    let target_definition = family
        .config(target_level)
        .filter(|definition| {
            definition.identity().config_id().get() == target_equipment.config_id()
        })
        .ok_or_else(|| {
            format!(
                "装备目录缺少强化目标配置 {} 的等级 {}",
                target_equipment.config_id(),
                target_equipment.enhance_level()
            )
        })?;
    if source_definition.enhancement().next_config_id()
        != Some(target_definition.identity().config_id())
        || target_definition.enhancement().previous_config_id()
            != Some(source_definition.identity().config_id())
    {
        return Err(format!(
            "装备目录中的强化链 {} -> {} 不连续",
            source_equipment.config_id(),
            target_equipment.config_id()
        ));
    }
    let expected_cost = source_definition.enhancement().next_cost();
    let materials_match = expected_cost.items().len() == cost.materials().len()
        && expected_cost
            .items()
            .iter()
            .zip(cost.materials())
            .all(|(expected, planned)| {
                expected.item_id() == planned.item_id() && expected.quantity() == planned.quantity()
            });
    if expected_cost.gold() != cost.gold() || !materials_match {
        return Err(format!(
            "配置 {} 的强化成本与装备目录不一致",
            source_equipment.config_id()
        ));
    }
    Ok(())
}

/// 一次强化步骤已通过目录链和成本核对后的纯规则。资源与装备模拟共用这一结果。
struct EnhanceChainRule<'a> {
    gold: u64,
    materials: &'a [PlanEnhanceMaterialCost],
}

fn enhance_chain_rules<'a>(
    initial: &GameState,
    plan: &'a CompiledPlan,
) -> Result<Vec<Option<EnhanceChainRule<'a>>>, String> {
    plan.steps()
        .iter()
        .map(|step| match step {
            PlanStep::Enhance {
                source_equipment,
                target_equipment,
                cost,
                ..
            } => {
                validate_enhance_step(initial, *source_equipment, *target_equipment, cost)?;
                Ok(Some(EnhanceChainRule {
                    gold: cost.gold(),
                    materials: cost.materials(),
                }))
            }
            _ => Ok(None),
        })
        .collect()
}

fn expected_resource_states<'a>(
    initial: &GameState,
    plan: &CompiledPlan,
    rules: &[Option<EnhanceChainRule<'a>>],
) -> Result<(Vec<Arc<ResourceStateSnapshot>>, Vec<bool>), String> {
    let mut current = Arc::new(resource_state_snapshot(initial));
    let mut states = vec![Arc::clone(&current)];
    let mut verify_resources = vec![false];
    let mut resource_write_seen = false;
    for (step, rule) in plan.steps().iter().cloned().zip(rules.iter()) {
        match step {
            PlanStep::Dismantle {
                equipment,
                quantity,
                ..
            } => {
                resource_write_seen = true;
                let family_id = EquipmentFamilyId::new(equipment.family_id())
                    .map_err(|_| format!("拆解计划包含无效装备族 {}", equipment.family_id()))?;
                let definition = initial
                    .equipment_catalog()
                    .family(family_id)
                    .and_then(|family| family.config(EnhanceLevel::new(equipment.enhance_level())))
                    .filter(|definition| {
                        definition.identity().config_id().get() == equipment.config_id()
                    })
                    .ok_or_else(|| {
                        format!(
                            "装备目录缺少拆解配置 {} 的强化等级 {}",
                            equipment.config_id(),
                            equipment.enhance_level()
                        )
                    })?;
                let destroy_yield = definition.enhancement().destroy_yield();
                let gold_delta = destroy_yield
                    .gold()
                    .checked_mul(quantity)
                    .ok_or_else(|| format!("配置 {} 的预期拆解物资溢出", equipment.config_id()))?;
                let expected = Arc::make_mut(&mut current);
                expected.gold = expected.gold.checked_add(gold_delta).ok_or_else(|| {
                    format!("配置 {} 的预期拆解后物资溢出", equipment.config_id())
                })?;
                for item in destroy_yield.items() {
                    let item_delta = item
                        .quantity()
                        .checked_mul(quantity)
                        .ok_or_else(|| format!("拆解产物 {} 的预期数量溢出", item.item_id()))?;
                    let current = expected
                        .materials
                        .get(&item.item_id())
                        .copied()
                        .unwrap_or(0);
                    let after = current
                        .checked_add(item_delta)
                        .ok_or_else(|| format!("拆解产物 {} 的预期背包数量溢出", item.item_id()))?;
                    expected.materials.insert(item.item_id(), after);
                }
            }
            PlanStep::Compose {
                recipe_id,
                equipment,
                quantity,
                material_id,
                material_quantity_per_unit,
                gold_per_unit,
                ..
            } => {
                resource_write_seen = true;
                let recipe = initial
                    .equipment_catalog()
                    .recipes()
                    .iter()
                    .find(|recipe| recipe.recipe_id() == recipe_id)
                    .ok_or_else(|| format!("装备目录缺少合成配方 {recipe_id}"))?;
                let material = recipe.material();
                if recipe.equipment_config_id().get() != equipment.config_id()
                    || equipment.enhance_level() != 0
                    || material.item_id() != material_id
                    || material.quantity() != material_quantity_per_unit
                    || recipe.gold() != gold_per_unit
                {
                    return Err(format!("合成配方 {recipe_id} 与计划步骤不一致"));
                }
                let gold_cost = gold_per_unit
                    .checked_mul(quantity)
                    .ok_or_else(|| format!("合成配方 {recipe_id} 的预期物资成本溢出"))?;
                let expected = Arc::make_mut(&mut current);
                expected.gold = expected
                    .gold
                    .checked_sub(gold_cost)
                    .ok_or_else(|| format!("合成配方 {recipe_id} 的预期物资不足"))?;
                let material_cost = material_quantity_per_unit
                    .checked_mul(quantity)
                    .ok_or_else(|| format!("合成配方 {recipe_id} 的预期材料成本溢出"))?;
                let material_before = expected.materials.get(&material_id).copied().unwrap_or(0);
                let material_after = material_before
                    .checked_sub(material_cost)
                    .ok_or_else(|| format!("合成配方 {recipe_id} 的预期材料不足"))?;
                expected.materials.insert(material_id, material_after);
            }
            PlanStep::Enhance {
                source_equipment, ..
            } => {
                resource_write_seen = true;
                let Some(rule) = rule.as_ref() else {
                    return Err("强化步骤缺少已校验的强化链规则".to_owned());
                };
                let expected = Arc::make_mut(&mut current);
                expected.gold = expected.gold.checked_sub(rule.gold).ok_or_else(|| {
                    format!("配置 {} 的强化预期物资不足", source_equipment.config_id())
                })?;
                for material in rule.materials {
                    let before = expected
                        .materials
                        .get(&material.item_id())
                        .copied()
                        .unwrap_or(0);
                    let after = before.checked_sub(material.quantity()).ok_or_else(|| {
                        format!(
                            "配置 {} 的强化材料 {} 预期数量不足",
                            source_equipment.config_id(),
                            material.item_id()
                        )
                    })?;
                    expected.materials.insert(material.item_id(), after);
                }
            }
            PlanStep::Keep { .. } | PlanStep::Unequip { .. } | PlanStep::Equip { .. } => {}
        }
        states.push(Arc::clone(&current));
        verify_resources.push(resource_write_seen);
    }
    Ok((states, verify_resources))
}

fn expected_equipment_states<'a>(
    initial: &GameState,
    plan: &CompiledPlan,
    rules: &[Option<EnhanceChainRule<'a>>],
) -> Result<Vec<Arc<EquipmentStateSnapshot>>, String> {
    let mut current = Arc::new(equipment_state_snapshot(initial));
    let mut states = vec![Arc::clone(&current)];
    for (step, rule) in plan.steps().iter().cloned().zip(rules.iter()) {
        match step {
            PlanStep::Keep { slot, .. } => {
                let slot_key = (slot.ship_instance_id(), slot.slot_index());
                if !current.slots.contains_key(&slot_key) {
                    return Err(format!(
                        "计划保持的舰船 {} 槽位 {} 不在初始状态中",
                        slot.ship_instance_id(),
                        slot.slot_index()
                    ));
                }
            }
            PlanStep::Unequip { slot, .. } => {
                let slot_key = (slot.ship_instance_id(), slot.slot_index());
                let expected = Arc::make_mut(&mut current);
                let equipment = expected
                    .slots
                    .get_mut(&slot_key)
                    .ok_or_else(|| {
                        format!(
                            "计划卸装的舰船 {} 槽位 {} 不在初始状态中",
                            slot.ship_instance_id(),
                            slot.slot_index()
                        )
                    })?
                    .take()
                    .ok_or_else(|| {
                        format!(
                            "计划卸装的舰船 {} 槽位 {} 在预期状态中已经为空",
                            slot.ship_instance_id(),
                            slot.slot_index()
                        )
                    })?;
                let quantity = expected.warehouse.entry(equipment.config_id).or_default();
                *quantity = quantity
                    .checked_add(1)
                    .ok_or_else(|| format!("仓库配置 {} 的预期数量溢出", equipment.config_id))?;
            }
            PlanStep::Dismantle {
                source,
                equipment,
                quantity,
                ..
            } => {
                let expected = Arc::make_mut(&mut current);
                let source_config_id = match source {
                    PlanSource::Warehouse { config_id } => config_id,
                    PlanSource::ShipSlot {
                        ship_instance_id,
                        slot_index,
                    } => {
                        match expected.slots.get(&(ship_instance_id, slot_index)) {
                            Some(None) => {}
                            Some(Some(_)) => {
                                return Err(format!(
                                    "待拆解来源舰船 {ship_instance_id} 的槽位 {slot_index} 在预期状态中尚未清空"
                                ));
                            }
                            None => {
                                return Err(format!(
                                    "待拆解来源舰船 {ship_instance_id} 的槽位 {slot_index} 不在初始状态中"
                                ));
                            }
                        }
                        equipment.config_id()
                    }
                    PlanSource::Compose { recipe_id } => {
                        return Err(format!("拆解步骤不能直接使用合成配方 {recipe_id} 作为来源"));
                    }
                };
                if source_config_id != equipment.config_id() {
                    return Err(format!(
                        "计划拆解来源配置 {source_config_id} 与装备配置 {} 不一致",
                        equipment.config_id()
                    ));
                }
                let remaining = expected
                    .warehouse
                    .get(&source_config_id)
                    .copied()
                    .and_then(|available| available.checked_sub(quantity))
                    .ok_or_else(|| {
                        format!("仓库配置 {source_config_id} 的预期数量不足以拆解 {quantity} 件")
                    })?;
                if remaining == 0 {
                    expected.warehouse.remove(&source_config_id);
                } else {
                    expected.warehouse.insert(source_config_id, remaining);
                }
            }
            PlanStep::Compose {
                recipe_id,
                equipment,
                quantity,
                ..
            } => {
                let recipe = initial
                    .equipment_catalog()
                    .recipes()
                    .iter()
                    .find(|recipe| recipe.recipe_id() == recipe_id)
                    .ok_or_else(|| format!("装备目录缺少合成配方 {recipe_id}"))?;
                if recipe.equipment_config_id().get() != equipment.config_id()
                    || equipment.enhance_level() != 0
                {
                    return Err(format!("合成配方 {recipe_id} 的装备产物与计划不一致"));
                }
                let expected = Arc::make_mut(&mut current);
                let warehouse_quantity =
                    expected.warehouse.entry(equipment.config_id()).or_default();
                *warehouse_quantity = warehouse_quantity
                    .checked_add(quantity)
                    .ok_or_else(|| format!("合成配方 {recipe_id} 的预期仓库数量溢出"))?;
            }
            PlanStep::Enhance {
                source,
                source_equipment,
                target_equipment,
                ..
            } => {
                if rule.is_none() {
                    return Err("强化步骤缺少已校验的强化链规则".to_owned());
                }
                let expected = Arc::make_mut(&mut current);
                match source {
                    PlanSource::Warehouse { config_id } => {
                        if config_id != source_equipment.config_id() {
                            return Err(format!(
                                "强化来源配置 {config_id} 与计划装备配置 {} 不一致",
                                source_equipment.config_id()
                            ));
                        }
                        let remaining = expected
                            .warehouse
                            .get(&config_id)
                            .copied()
                            .and_then(|quantity| quantity.checked_sub(1))
                            .ok_or_else(|| {
                                format!("仓库配置 {config_id} 的预期数量不足以完成强化")
                            })?;
                        if remaining == 0 {
                            expected.warehouse.remove(&config_id);
                        } else {
                            expected.warehouse.insert(config_id, remaining);
                        }
                        let target_quantity = expected
                            .warehouse
                            .entry(target_equipment.config_id())
                            .or_default();
                        *target_quantity = target_quantity.checked_add(1).ok_or_else(|| {
                            format!(
                                "强化目标配置 {} 的预期仓库数量溢出",
                                target_equipment.config_id()
                            )
                        })?;
                    }
                    PlanSource::ShipSlot {
                        ship_instance_id,
                        slot_index,
                    } => {
                        let slot = expected
                            .slots
                            .get_mut(&(ship_instance_id, slot_index))
                            .ok_or_else(|| {
                                format!(
                                    "计划强化的舰船 {ship_instance_id} 槽位 {slot_index} 不在初始状态中"
                                )
                            })?;
                        let source_state = slot.ok_or_else(|| {
                            format!(
                                "计划强化的舰船 {ship_instance_id} 槽位 {slot_index} 在预期状态中为空"
                            )
                        })?;
                        if source_state.config_id != source_equipment.config_id()
                            || source_state.enhance_level != source_equipment.enhance_level()
                        {
                            return Err(format!(
                                "舰船 {ship_instance_id} 槽位 {slot_index} 的强化来源与计划不一致"
                            ));
                        }
                        *slot = Some(ExecutionEquipmentState {
                            config_id: target_equipment.config_id(),
                            enhance_level: target_equipment.enhance_level(),
                        });
                    }
                    PlanSource::Compose { recipe_id } => {
                        return Err(format!(
                            "强化步骤不能直接使用合成配方 {recipe_id} 作为实际位置"
                        ));
                    }
                }
            }
            PlanStep::Equip {
                slot,
                source,
                equipment,
                ..
            } => {
                let slot_key = (slot.ship_instance_id(), slot.slot_index());
                let expected = Arc::make_mut(&mut current);
                let source_config_id = match source {
                    PlanSource::Warehouse { config_id } => config_id,
                    PlanSource::ShipSlot {
                        ship_instance_id,
                        slot_index,
                    } => {
                        match expected.slots.get(&(ship_instance_id, slot_index)) {
                            Some(None) => {}
                            Some(Some(_)) => {
                                return Err(format!(
                                    "来源舰船 {ship_instance_id} 的槽位 {slot_index} 在预期状态中尚未清空"
                                ));
                            }
                            None => {
                                return Err(format!(
                                    "来源舰船 {ship_instance_id} 的槽位 {slot_index} 不在初始状态中"
                                ));
                            }
                        }
                        equipment.config_id()
                    }
                    PlanSource::Compose { recipe_id } => {
                        let recipe = initial
                            .equipment_catalog()
                            .recipes()
                            .iter()
                            .find(|recipe| recipe.recipe_id() == recipe_id)
                            .ok_or_else(|| format!("装备目录缺少合成配方 {recipe_id}"))?;
                        recipe.equipment_config_id().get()
                    }
                };
                if source_config_id != equipment.config_id() {
                    return Err(format!(
                        "计划来源配置 {source_config_id} 与目标配置 {} 不一致",
                        equipment.config_id()
                    ));
                }
                let target = expected.slots.get_mut(&slot_key).ok_or_else(|| {
                    format!(
                        "计划装备的舰船 {} 槽位 {} 不在初始状态中",
                        slot.ship_instance_id(),
                        slot.slot_index()
                    )
                })?;
                if target.is_some() {
                    return Err(format!(
                        "计划装备的舰船 {} 槽位 {} 在预期状态中不是空槽",
                        slot.ship_instance_id(),
                        slot.slot_index()
                    ));
                }
                let remaining = expected
                    .warehouse
                    .get(&source_config_id)
                    .copied()
                    .and_then(|quantity| quantity.checked_sub(1))
                    .ok_or_else(|| {
                        format!("仓库配置 {source_config_id} 的预期数量不足以完成装备")
                    })?;
                if remaining == 0 {
                    expected.warehouse.remove(&source_config_id);
                } else {
                    expected.warehouse.insert(source_config_id, remaining);
                }
                *target = Some(ExecutionEquipmentState {
                    config_id: equipment.config_id(),
                    enhance_level: equipment.enhance_level(),
                });
            }
        }
        states.push(Arc::clone(&current));
    }
    Ok(states)
}

/// 单个计划步骤的预期前后态。相同检查点共享同一份不可变内容。
/// `verify_resources` 表示该步完成后是否核对资源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedStep {
    pub(super) equipment_before: Arc<EquipmentStateSnapshot>,
    pub(super) equipment_after: Arc<EquipmentStateSnapshot>,
    pub(super) resources_before: Arc<ResourceStateSnapshot>,
    pub(super) resources_after: Arc<ResourceStateSnapshot>,
    pub(super) verify_resources: bool,
}

/// 一次预演的初始检查点，以及每个步骤自己的前后态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedExecution {
    initial_equipment: Arc<EquipmentStateSnapshot>,
    initial_resources: Arc<ResourceStateSnapshot>,
    steps: Vec<ExpectedStep>,
}

impl ExpectedExecution {
    pub(super) fn steps(&self) -> &[ExpectedStep] {
        &self.steps
    }

    pub(super) fn checkpoint_count(&self) -> usize {
        self.steps.len() + 1
    }

    pub(super) fn equipment_at(&self, index: usize) -> &EquipmentStateSnapshot {
        if index == 0 {
            self.initial_equipment.as_ref()
        } else {
            self.steps[index - 1].equipment_after.as_ref()
        }
    }

    pub(super) fn resources_at(&self, index: usize) -> &ResourceStateSnapshot {
        if index == 0 {
            self.initial_resources.as_ref()
        } else {
            self.steps[index - 1].resources_after.as_ref()
        }
    }

    pub(super) fn verify_resources_at(&self, index: usize) -> bool {
        if index == 0 {
            false
        } else {
            self.steps[index - 1].verify_resources
        }
    }
}

pub(super) fn expected_execution(
    initial: &GameState,
    plan: &CompiledPlan,
) -> Result<ExpectedExecution, String> {
    let rules = enhance_chain_rules(initial, plan)?;
    let equipment_states = expected_equipment_states(initial, plan, &rules)?;
    let (resource_states, verify_resources) = expected_resource_states(initial, plan, &rules)?;
    let checkpoint_count = plan.steps().len() + 1;
    if equipment_states.len() != checkpoint_count
        || resource_states.len() != checkpoint_count
        || verify_resources.len() != checkpoint_count
    {
        return Err("预演装备状态与资源状态数量不一致".to_owned());
    }
    let steps = (0..plan.steps().len())
        .map(|index| ExpectedStep {
            equipment_before: Arc::clone(&equipment_states[index]),
            equipment_after: Arc::clone(&equipment_states[index + 1]),
            resources_before: Arc::clone(&resource_states[index]),
            resources_after: Arc::clone(&resource_states[index + 1]),
            verify_resources: verify_resources[index + 1],
        })
        .collect();
    Ok(ExpectedExecution {
        initial_equipment: Arc::clone(&equipment_states[0]),
        initial_resources: Arc::clone(&resource_states[0]),
        steps,
    })
}

pub(super) fn build_execution_preflight(
    target_identity: &ExecutionTargetIdentity,
    plan: &CompiledPlan,
    initial_state_content_sha256: &str,
    expected: &ExpectedExecution,
) -> Result<ExecutionPreflight, AppError> {
    let mut steps = Vec::new();
    for (index, step) in plan.steps().iter().cloned().enumerate() {
        let Some(action) = ExecutionAction::from_step(&step) else {
            continue;
        };
        if expected.steps()[index].equipment_before == expected.steps()[index].equipment_after {
            return Err(AppError::from_source(
                "plan.execute.preflight",
                AppErrorCode::RuntimeIncompatible,
                "写步骤的预期前后装备状态不能相同",
                std::io::Error::other("write step did not change the simulated equipment state"),
            )
            .with_context("step_sequence", step.sequence().to_string()));
        }
        steps.push(ExecutionPreflightStep { action });
    }
    Ok(ExecutionPreflight {
        target_identity: target_identity.clone(),
        initial_state_content_sha256: initial_state_content_sha256.to_owned(),
        steps,
    })
}

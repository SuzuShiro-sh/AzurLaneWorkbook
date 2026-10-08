//! 验证库存动作并构建强化步骤，维护库存与强化成本预留。

use super::super::{
    PlanCheckError, PlanEnhanceCost, PlanEnhanceMaterialCost, PlanEquipment, PlanSource, PlanStep,
};
use super::Reservations;
use super::equipment_rules::{config_definition, current_equipment, ensure_family};
use crate::domain::{
    EnhanceLevel, EquipmentCatalog, EquipmentConfigId, EquipmentDefinition, EquipmentFamilyId,
    EquipmentInventoryAction, EquipmentInventoryActionKind, EquipmentInventoryPlan,
    EquipmentResources, EquipmentSourceRef, GameState, ShipSlotRef,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct EnhanceReservations {
    pub(super) material_usage: BTreeMap<u64, u64>,
    pub(super) gold_usage: u64,
    pub(super) first_source_config_id: Option<u64>,
}

#[derive(Clone, Copy)]
pub(super) struct ValidatedEnhance {
    pub(super) source_ref: EquipmentSourceRef,
    pub(super) family_id: EquipmentFamilyId,
    pub(super) source_config_id: EquipmentConfigId,
    pub(super) source_level: EnhanceLevel,
    pub(super) target_level: EnhanceLevel,
    pub(super) quantity: u64,
}

/// 预留库存动作占用的来源，供配装来源选择阶段扣除和冲突检查。
#[derive(Default)]
pub(super) struct InventoryReservations {
    /// 已有拆解预留的仓库配置会从整个来源候选中排除。
    pub(super) warehouse_dismantle: BTreeMap<EquipmentConfigId, u64>,
    pub(super) ship_dismantle: BTreeSet<ShipSlotRef>,
    pub(super) dismantles: Vec<ValidatedDismantle>,
    pub(super) warehouse_enhance: BTreeMap<EquipmentConfigId, u64>,
    pub(super) ship_enhance: BTreeSet<ShipSlotRef>,
    pub(super) enhancements: Vec<ValidatedEnhance>,
}

pub(super) struct ValidatedDismantle {
    pub(super) source_ref: EquipmentSourceRef,
    pub(super) source: PlanSource,
    pub(super) equipment: PlanEquipment,
    pub(super) quantity: u64,
    pub(super) gold_yield: u64,
    pub(super) item_yields: Vec<(u64, u64)>,
}

pub(super) fn validate_inventory_plan(
    state: &GameState,
    inventory_plan: &EquipmentInventoryPlan,
) -> Result<InventoryReservations, PlanCheckError> {
    let mut reservations = InventoryReservations::default();
    for action in inventory_plan.actions() {
        if action.is_noop() {
            continue;
        }
        match action.source() {
            EquipmentSourceRef::Warehouse(config_id) => {
                let source = action.source();
                let stack = state
                    .equipment_inventory()
                    .warehouse_stack(config_id)
                    .ok_or(PlanCheckError::InventorySourceNotFound { source_ref: source })?;
                let definition = config_definition(state.equipment_catalog(), config_id)?;
                let actual_family_id = definition.identity().family_id();
                ensure_family(source, actual_family_id, stack.family_id())?;
                validate_inventory_target_level(
                    state,
                    stack.family_id(),
                    stack.enhance_level(),
                    action.target_enhance_level(),
                )?;
                validate_inventory_enhance_quantity(source, stack.quantity(), action)?;
                match action.kind() {
                    EquipmentInventoryActionKind::Dismantle => {
                        let quantity = action
                            .dismantle_quantity()
                            .expect("模型已保证拆解动作带有数量");
                        if quantity > stack.quantity() {
                            return Err(PlanCheckError::InventorySourceUnavailable {
                                source_ref: source,
                                available: stack.quantity(),
                                required: quantity,
                            });
                        }
                        let reserved = reservations
                            .warehouse_dismantle
                            .get(&config_id)
                            .copied()
                            .unwrap_or(0)
                            .checked_add(quantity)
                            .ok_or(PlanCheckError::QuantityOverflow { quantity })?;
                        if reserved > stack.quantity() {
                            return Err(PlanCheckError::InventorySourceUnavailable {
                                source_ref: source,
                                available: stack.quantity(),
                                required: reserved,
                            });
                        }
                        reservations.warehouse_dismantle.insert(config_id, reserved);
                        reservations.dismantles.push(validate_dismantle(
                            source,
                            definition,
                            stack.enhance_level(),
                            quantity,
                        )?);
                    }
                    EquipmentInventoryActionKind::Keep => {
                        if let (Some(target_level), Some(quantity)) =
                            (action.target_enhance_level(), action.enhance_quantity())
                            && target_level > stack.enhance_level()
                        {
                            reservations.warehouse_enhance.insert(config_id, quantity);
                            reservations.enhancements.push(ValidatedEnhance {
                                source_ref: source,
                                family_id: stack.family_id(),
                                source_config_id: stack.config_id(),
                                source_level: stack.enhance_level(),
                                target_level,
                                quantity,
                            });
                        }
                    }
                }
            }
            EquipmentSourceRef::ShipSlot(source_slot) => {
                let source = action.source();
                let source_ship = state
                    .ships()
                    .ships()
                    .iter()
                    .find(|ship| ship.identity().instance_id() == source_slot.ship_instance_id())
                    .ok_or(PlanCheckError::InventorySourceNotFound { source_ref: source })?;
                let source_equipment = current_equipment(source_ship, source_slot)
                    .ok_or(PlanCheckError::InventorySourceNotFound { source_ref: source })?;
                let definition =
                    config_definition(state.equipment_catalog(), source_equipment.config_id())?;
                let family_id = definition.identity().family_id();
                validate_inventory_target_level(
                    state,
                    family_id,
                    source_equipment.enhance_level(),
                    action.target_enhance_level(),
                )?;
                validate_inventory_enhance_quantity(source, 1, action)?;
                match action.kind() {
                    EquipmentInventoryActionKind::Dismantle => {
                        let quantity = action
                            .dismantle_quantity()
                            .expect("模型已保证拆解动作带有数量");
                        if quantity > 1 {
                            return Err(PlanCheckError::InventorySourceUnavailable {
                                source_ref: source,
                                available: 1,
                                required: quantity,
                            });
                        }
                        reservations.ship_dismantle.insert(source_slot);
                        reservations.dismantles.push(validate_dismantle(
                            source,
                            definition,
                            source_equipment.enhance_level(),
                            quantity,
                        )?);
                    }
                    EquipmentInventoryActionKind::Keep => {
                        if let (Some(target_level), Some(quantity)) =
                            (action.target_enhance_level(), action.enhance_quantity())
                            && target_level > source_equipment.enhance_level()
                        {
                            reservations.ship_enhance.insert(source_slot);
                            reservations.enhancements.push(ValidatedEnhance {
                                source_ref: source,
                                family_id,
                                source_config_id: source_equipment.config_id(),
                                source_level: source_equipment.enhance_level(),
                                target_level,
                                quantity,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(reservations)
}

fn validate_inventory_enhance_quantity(
    source_ref: EquipmentSourceRef,
    available: u64,
    action: &EquipmentInventoryAction,
) -> Result<(), PlanCheckError> {
    if let Some(required) = action.enhance_quantity()
        && required > available
    {
        return Err(PlanCheckError::InventorySourceUnavailable {
            source_ref,
            available,
            required,
        });
    }
    Ok(())
}

fn validate_dismantle(
    source_ref: EquipmentSourceRef,
    definition: &EquipmentDefinition,
    source_level: EnhanceLevel,
    quantity: u64,
) -> Result<ValidatedDismantle, PlanCheckError> {
    if definition.enhancement().level() != source_level {
        return Err(PlanCheckError::InventorySourceConflict { source_ref });
    }
    let safety = definition.dismantle_safety();
    if !safety.allows_automatic_dismantle() {
        return Err(PlanCheckError::InventoryDismantleProtected {
            source_ref,
            important: safety.is_important(),
            protected_variant: safety.is_protected_variant(),
            rarity_confirmation_required: safety.requires_rarity_confirmation(),
            enhanced: safety.is_enhanced(),
        });
    }
    let destroy_yield = definition.enhancement().destroy_yield();
    Ok(ValidatedDismantle {
        source_ref,
        source: PlanSource::from_domain(source_ref),
        equipment: PlanEquipment::new(
            definition.identity().family_id(),
            definition.identity().config_id(),
            definition.enhancement().level(),
        ),
        quantity,
        gold_yield: destroy_yield.gold(),
        item_yields: destroy_yield
            .items()
            .iter()
            .map(|item| (item.item_id(), item.quantity()))
            .collect(),
    })
}

fn validate_inventory_target_level(
    state: &GameState,
    family_id: EquipmentFamilyId,
    source_level: EnhanceLevel,
    target_level: Option<EnhanceLevel>,
) -> Result<(), PlanCheckError> {
    let Some(target_level) = target_level else {
        return Ok(());
    };
    let family = state
        .equipment_catalog()
        .family(family_id)
        .ok_or(PlanCheckError::EquipmentFamilyNotFound { family_id })?;
    if family.config(target_level).is_none() {
        return Err(PlanCheckError::TargetEnhanceLevelUnavailable {
            family_id,
            target_level: target_level.get(),
        });
    }
    if target_level < source_level {
        return Err(PlanCheckError::EnhanceDowngrade {
            source_level: source_level.get(),
            target_level: target_level.get(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_enhance_steps(
    catalog: &EquipmentCatalog,
    family_id: EquipmentFamilyId,
    source: PlanSource,
    source_config_id: EquipmentConfigId,
    source_level: EnhanceLevel,
    target_level: EnhanceLevel,
    quantity: u64,
    reservations: &mut Reservations,
) -> Result<(Vec<PlanStep>, PlanEquipment), PlanCheckError> {
    let family = catalog
        .family(family_id)
        .ok_or(PlanCheckError::EquipmentFamilyNotFound { family_id })?;
    let source_definition = family
        .config(source_level)
        .filter(|definition| definition.identity().config_id() == source_config_id)
        .ok_or(PlanCheckError::EquipmentConfigNotFound {
            config_id: source_config_id,
        })?;
    let target_definition =
        family
            .config(target_level)
            .ok_or(PlanCheckError::TargetEnhanceLevelUnavailable {
                family_id,
                target_level: target_level.get(),
            })?;
    if target_level < source_level {
        return Err(PlanCheckError::EnhanceDowngrade {
            source_level: source_level.get(),
            target_level: target_level.get(),
        });
    }
    let target_equipment = PlanEquipment::new(
        family_id,
        target_definition.identity().config_id(),
        target_level,
    );
    if target_level == source_level {
        return Ok((Vec::new(), target_equipment));
    }

    let mut steps = Vec::new();
    for _ in 0..quantity {
        let mut current_definition = source_definition;
        let mut current_level = source_level;
        while current_level < target_level {
            let next_level_value = current_level.get().checked_add(1).ok_or(
                PlanCheckError::TargetEnhanceLevelUnavailable {
                    family_id,
                    target_level: target_level.get(),
                },
            )?;
            let next_level = EnhanceLevel::new(next_level_value);
            let next_definition =
                family
                    .config(next_level)
                    .ok_or(PlanCheckError::TargetEnhanceLevelUnavailable {
                        family_id,
                        target_level: next_level_value,
                    })?;
            let current_config_id = current_definition.identity().config_id();
            let next_config_id = next_definition.identity().config_id();
            if current_definition.enhancement().next_config_id() != Some(next_config_id)
                || next_definition.enhancement().previous_config_id() != Some(current_config_id)
            {
                return Err(PlanCheckError::EnhanceChainMismatch {
                    source_config_id: current_config_id.get(),
                    target_config_id: next_config_id.get(),
                });
            }
            let cost = plan_enhance_cost(current_definition.enhancement().next_cost());
            reservations.reserve_enhance_cost(current_config_id, &cost)?;
            let step_source = match source {
                PlanSource::ShipSlot { .. } => source,
                PlanSource::Warehouse { .. } => PlanSource::Warehouse {
                    config_id: current_config_id.get(),
                },
                PlanSource::Compose { .. } => {
                    unreachable!("强化步骤必须使用实际仓库或舰船位置")
                }
            };
            steps.push(PlanStep::Enhance {
                sequence: 0,
                source: step_source,
                source_equipment: PlanEquipment::new(family_id, current_config_id, current_level),
                target_equipment: PlanEquipment::new(family_id, next_config_id, next_level),
                cost,
            });
            current_definition = next_definition;
            current_level = next_level;
        }
    }
    Ok((steps, target_equipment))
}

fn plan_enhance_cost(resources: &EquipmentResources) -> PlanEnhanceCost {
    PlanEnhanceCost {
        gold: resources.gold(),
        materials: resources
            .items()
            .iter()
            .map(|item| PlanEnhanceMaterialCost {
                item_id: item.item_id(),
                quantity: item.quantity(),
            })
            .collect(),
    }
}

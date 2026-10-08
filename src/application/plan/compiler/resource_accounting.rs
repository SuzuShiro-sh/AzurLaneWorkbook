//! 汇总本次计划的资源预留，生成资源约束与净变化。

use super::super::{
    PlanCheckError, ResourceChange, ResourceConstraint, ResourceDelta, ResourceKey,
};
use super::Reservations;
use super::inventory_planner::InventoryReservations;
use crate::domain::{EquipmentConfigId, EquipmentSourceRef, GameState};
use std::collections::BTreeMap;

pub(super) fn account_resources(
    state: &GameState,
    reservations: &Reservations,
    inventory_reservations: &InventoryReservations,
    warehouse_enhance_outputs: &BTreeMap<EquipmentConfigId, u64>,
) -> Result<(Vec<ResourceConstraint>, ResourceDelta), PlanCheckError> {
    let resources = state.resources();
    let compose_reservations = reservations.compose();
    let enhance_reservations = reservations.enhance();
    let mut warehouse_requirements = reservations.warehouse_usage().clone();
    for (config_id, quantity) in &inventory_reservations.warehouse_dismantle {
        let required = warehouse_requirements
            .get(config_id)
            .copied()
            .unwrap_or(0)
            .checked_add(*quantity)
            .ok_or(PlanCheckError::QuantityOverflow {
                quantity: *quantity,
            })?;
        warehouse_requirements.insert(*config_id, required);
    }
    for (config_id, quantity) in &inventory_reservations.warehouse_enhance {
        let required = warehouse_requirements
            .get(config_id)
            .copied()
            .unwrap_or(0)
            .checked_add(*quantity)
            .ok_or(PlanCheckError::QuantityOverflow {
                quantity: *quantity,
            })?;
        warehouse_requirements.insert(*config_id, required);
    }

    let total_gold_usage = compose_reservations
        .gold_usage
        .checked_add(enhance_reservations.gold_usage)
        .ok_or(PlanCheckError::QuantityOverflow {
            quantity: enhance_reservations.gold_usage,
        })?;
    let mut total_material_usage = compose_reservations.material_usage.clone();
    for (&item_id, &quantity) in &enhance_reservations.material_usage {
        let required = total_material_usage
            .get(&item_id)
            .copied()
            .unwrap_or(0)
            .checked_add(quantity)
            .ok_or(PlanCheckError::QuantityOverflow { quantity })?;
        total_material_usage.insert(item_id, required);
    }

    let mut resource_constraints: Vec<ResourceConstraint> =
        Vec::with_capacity(warehouse_requirements.len() + total_material_usage.len() + 1);
    for (config_id, required) in &warehouse_requirements {
        let Some(stack) = state.equipment_inventory().warehouse_stack(*config_id) else {
            return Err(PlanCheckError::SourceNotFound {
                source_ref: EquipmentSourceRef::Warehouse(*config_id),
            });
        };
        let remaining: u64 =
            stack
                .quantity()
                .checked_sub(*required)
                .ok_or(PlanCheckError::SourceUnavailable {
                    source_ref: EquipmentSourceRef::Warehouse(*config_id),
                    available: stack.quantity(),
                    required: *required,
                })?;
        resource_constraints.push(ResourceConstraint {
            key: ResourceKey::WarehouseEquipment {
                config_id: config_id.get(),
            },
            available: stack.quantity(),
            required: *required,
            remaining,
        });
    }
    if total_gold_usage > 0 {
        let remaining = resources
            .gold()
            .checked_sub(total_gold_usage)
            .ok_or_else(|| {
                if enhance_reservations.gold_usage > 0
                    && let Some(source_config_id) = enhance_reservations.first_source_config_id
                {
                    PlanCheckError::EnhanceResourceUnavailable {
                        source_config_id,
                        key: ResourceKey::Gold,
                        available: resources.gold(),
                        required: total_gold_usage,
                    }
                } else {
                    let recipe_id = compose_reservations
                        .by_recipe
                        .keys()
                        .next()
                        .copied()
                        .expect("非零合成物资预留必须来自至少一条配方");
                    PlanCheckError::ComposeResourceUnavailable {
                        recipe_id,
                        key: ResourceKey::Gold,
                        available: resources.gold(),
                        required: total_gold_usage,
                    }
                }
            })?;
        resource_constraints.push(ResourceConstraint {
            key: ResourceKey::Gold,
            available: resources.gold(),
            required: total_gold_usage,
            remaining,
        });
    }
    for (&item_id, &required) in &total_material_usage {
        if required == 0 {
            continue;
        }
        let available = state.bag().item(item_id).map_or(0, |item| item.quantity());
        let remaining = available.checked_sub(required).ok_or_else(|| {
            if enhance_reservations.material_usage.contains_key(&item_id)
                && let Some(source_config_id) = enhance_reservations.first_source_config_id
            {
                PlanCheckError::EnhanceResourceUnavailable {
                    source_config_id,
                    key: ResourceKey::Item { item_id },
                    available,
                    required,
                }
            } else {
                let recipe_id = compose_reservations
                    .by_recipe
                    .values()
                    .find(|compose| compose.material_id == item_id)
                    .map(|compose| compose.recipe_id)
                    .expect("非零合成材料预留必须来自至少一条配方");
                PlanCheckError::ComposeResourceUnavailable {
                    recipe_id,
                    key: ResourceKey::Item { item_id },
                    available,
                    required,
                }
            }
        })?;
        resource_constraints.push(ResourceConstraint {
            key: ResourceKey::Item { item_id },
            available,
            required,
            remaining,
        });
    }
    let mut resource_changes: BTreeMap<ResourceKey, i128> = BTreeMap::new();
    for (config_id, quantity) in &warehouse_requirements {
        add_resource_change(
            &mut resource_changes,
            ResourceKey::WarehouseEquipment {
                config_id: config_id.get(),
            },
            *quantity,
            1,
            false,
        )?;
    }
    for compose in compose_reservations.by_recipe.values() {
        add_resource_change(
            &mut resource_changes,
            ResourceKey::WarehouseEquipment {
                config_id: compose.equipment.config_id(),
            },
            compose.quantity,
            1,
            true,
        )?;
    }
    for (config_id, quantity) in &compose_reservations.output_usage {
        add_resource_change(
            &mut resource_changes,
            ResourceKey::WarehouseEquipment {
                config_id: config_id.get(),
            },
            *quantity,
            1,
            false,
        )?;
    }
    for (config_id, quantity) in warehouse_enhance_outputs {
        add_resource_change(
            &mut resource_changes,
            ResourceKey::WarehouseEquipment {
                config_id: config_id.get(),
            },
            *quantity,
            1,
            true,
        )?;
    }
    add_resource_change(
        &mut resource_changes,
        ResourceKey::Gold,
        total_gold_usage,
        1,
        false,
    )?;
    for (&item_id, &quantity) in &total_material_usage {
        add_resource_change(
            &mut resource_changes,
            ResourceKey::Item { item_id },
            quantity,
            1,
            false,
        )?;
    }
    for dismantle in &inventory_reservations.dismantles {
        add_resource_change(
            &mut resource_changes,
            ResourceKey::Gold,
            dismantle.gold_yield,
            dismantle.quantity,
            true,
        )?;
        for &(item_id, quantity) in &dismantle.item_yields {
            add_resource_change(
                &mut resource_changes,
                ResourceKey::Item { item_id },
                quantity,
                dismantle.quantity,
                true,
            )?;
        }
    }
    let resource_delta = ResourceDelta {
        changes: resource_changes
            .iter()
            .filter(|(_, delta)| **delta != 0)
            .map(|(key, delta)| {
                let delta = i64::try_from(*delta)
                    .map_err(|_| PlanCheckError::ResourceChangeOverflow { key: *key })?;
                Ok(ResourceChange { key: *key, delta })
            })
            .collect::<Result<Vec<_>, PlanCheckError>>()?,
    };
    Ok((resource_constraints, resource_delta))
}

fn add_resource_change(
    changes: &mut BTreeMap<ResourceKey, i128>,
    key: ResourceKey,
    quantity_per_unit: u64,
    units: u64,
    positive: bool,
) -> Result<(), PlanCheckError> {
    if quantity_per_unit == 0 || units == 0 {
        return Ok(());
    }
    let magnitude = quantity_per_unit
        .checked_mul(units)
        .ok_or(PlanCheckError::ResourceChangeOverflow { key })?;
    let delta = if positive {
        i128::from(magnitude)
    } else {
        -i128::from(magnitude)
    };
    let total = changes
        .get(&key)
        .copied()
        .unwrap_or(0)
        .checked_add(delta)
        .ok_or(PlanCheckError::ResourceChangeOverflow { key })?;
    changes.insert(key, total);
    Ok(())
}

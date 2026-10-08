//! 将配装目标、库存动作、来源选择和资源约束编译为确定性执行计划。

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    DesiredState, EnhanceLevel, EquipmentConfigId, EquipmentInventoryPlan, EquipmentSourceRef,
    GameState, ShipEquipment, ShipProfile, ShipSlotRef, SlotTarget,
};

use super::ResourceDelta;
use super::digest::{
    absent_modifications_game_state_digest, desired_state_digest, inventory_plan_digest,
    plan_digest,
};
use super::{
    CompiledPlan, PLAN_SCHEMA_VERSION, PlanCheckError, PlanEnhanceCost, PlanEquipment, PlanSlot,
    PlanSource, PlanStep, ResourceKey,
};

mod equipment_rules;
mod inventory_planner;
mod resource_accounting;
mod source_selection;

use equipment_rules::{
    collect_preserved_slots, config_definition, current_equipment, ensure_equipment_compatible,
    ensure_requested_equipment_compatibility, family_definition, find_ship,
    resolve_target_equipment,
};
use inventory_planner::{
    EnhanceReservations, InventoryReservations, build_enhance_steps, validate_inventory_plan,
};
use resource_accounting::account_resources;
use source_selection::{SourceSelection, select_source, source_policy_can_fallback_after_compose};

pub(super) fn compile_absent_modifications_plan(
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
) -> Result<CompiledPlan, PlanCheckError> {
    let desired_state_content_sha256 = desired_state_digest(desired)?;
    let inventory_plan_content_sha256 = inventory_plan_digest(inventory_plan)?;
    let game_state_content_sha256 = absent_modifications_game_state_digest()?;
    let steps = Vec::new();
    let resource_constraints = Vec::new();
    let resource_delta = ResourceDelta::empty();
    let content_sha256 = plan_digest(
        PLAN_SCHEMA_VERSION,
        &game_state_content_sha256,
        &desired_state_content_sha256,
        &inventory_plan_content_sha256,
        &steps,
        &resource_constraints,
        &resource_delta,
    )?;
    Ok(CompiledPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        game_state_content_sha256,
        desired_state_content_sha256,
        inventory_plan_content_sha256,
        steps,
        resource_constraints,
        resource_delta,
        content_sha256,
    })
}

pub(super) fn compile_plan_only(
    state: &GameState,
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
) -> Result<CompiledPlan, PlanCheckError> {
    compile_plan_with_compositions(state, desired, inventory_plan, &[])
}

pub(super) fn compile_plan_with_compositions(
    state: &GameState,
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
    compositions: &[(u64, u64)],
) -> Result<CompiledPlan, PlanCheckError> {
    let first_attempt = compile_plan_attempt(state, desired, inventory_plan, compositions, None);
    let capacity_error = match first_attempt {
        Err(error @ PlanCheckError::ComposeEquipmentCapacityUnavailable { .. }) => error,
        result => return result,
    };

    // 一件已有装备来源就能腾出一个可反复用于“合成后立即装配”的空位。
    // 从低优先级目标开始放弃合成，避免无谓改变更高优先级目标的来源选择。
    for desired_slot in desired.slots().iter().rev() {
        let SlotTarget::Equipment(equipment) = desired_slot.target() else {
            continue;
        };
        if !source_policy_can_fallback_after_compose(equipment.source_policy()) {
            continue;
        }
        if let Ok(plan) = compile_plan_attempt(
            state,
            desired,
            inventory_plan,
            compositions,
            Some(desired_slot.slot()),
        ) {
            return Ok(plan);
        }
    }

    Err(capacity_error)
}

fn compile_plan_attempt(
    state: &GameState,
    desired: &DesiredState,
    inventory_plan: &EquipmentInventoryPlan,
    compositions: &[(u64, u64)],
    compose_fallback_slot: Option<ShipSlotRef>,
) -> Result<CompiledPlan, PlanCheckError> {
    let desired_state_content_sha256: String = desired_state_digest(desired)?;
    let inventory_plan_content_sha256: String = inventory_plan_digest(inventory_plan)?;
    let inventory_reservations: InventoryReservations =
        validate_inventory_plan(state, inventory_plan)?;
    let preserved_slots = collect_preserved_slots(state, desired)?;
    for source_slot in &inventory_reservations.ship_dismantle {
        if preserved_slots.contains(source_slot) {
            return Err(PlanCheckError::InventorySourceConflict {
                source_ref: EquipmentSourceRef::ShipSlot(*source_slot),
            });
        }
    }
    for source_slot in &inventory_reservations.ship_enhance {
        if desired
            .slots()
            .iter()
            .any(|desired_slot| desired_slot.slot() == *source_slot)
        {
            return Err(PlanCheckError::InventorySourceConflict {
                source_ref: EquipmentSourceRef::ShipSlot(*source_slot),
            });
        }
    }
    let mut reservations = Reservations::default();
    let mut keep_steps: Vec<PlanStep> = Vec::new();
    let mut unequip_slots: BTreeSet<ShipSlotRef> = inventory_reservations.ship_dismantle.clone();
    let mut dismantle_steps: Vec<PlanStep> = Vec::new();
    let mut inventory_enhance_steps: Vec<PlanStep> = Vec::new();
    let mut current_enhance_steps: Vec<PlanStep> = Vec::new();
    let mut equip_plans: Vec<PlannedEquip> = Vec::new();
    let mut warehouse_enhance_outputs: BTreeMap<EquipmentConfigId, u64> = BTreeMap::new();

    for enhance in &inventory_reservations.enhancements {
        let source = match enhance.source_ref {
            EquipmentSourceRef::Warehouse(config_id) => PlanSource::Warehouse {
                config_id: config_id.get(),
            },
            EquipmentSourceRef::ShipSlot(slot) => {
                PlanSource::from_domain(EquipmentSourceRef::ShipSlot(slot))
            }
        };
        let (steps, target_equipment) = build_enhance_steps(
            state.equipment_catalog(),
            enhance.family_id,
            source,
            enhance.source_config_id,
            enhance.source_level,
            enhance.target_level,
            enhance.quantity,
            &mut reservations,
        )?;
        if matches!(enhance.source_ref, EquipmentSourceRef::Warehouse(_)) {
            let output_config_id = EquipmentConfigId::new(target_equipment.config_id())
                .expect("计划装备配置已经通过领域校验");
            let output_quantity = warehouse_enhance_outputs
                .get(&output_config_id)
                .copied()
                .unwrap_or(0)
                .checked_add(enhance.quantity)
                .ok_or(PlanCheckError::QuantityOverflow {
                    quantity: enhance.quantity,
                })?;
            warehouse_enhance_outputs.insert(output_config_id, output_quantity);
        }
        inventory_enhance_steps.extend(steps);
    }

    for desired_slot in desired.slots() {
        let ship: &ShipProfile = find_ship(state, desired_slot.slot().ship_instance_id())?;
        let current: Option<ShipEquipment> = current_equipment(ship, desired_slot.slot());
        match desired_slot.target() {
            SlotTarget::Keep => keep_steps.push(PlanStep::Keep {
                sequence: 0,
                slot: PlanSlot::from_domain(desired_slot.slot()),
            }),
            SlotTarget::Empty => {
                if current.is_some() {
                    unequip_slots.insert(desired_slot.slot());
                } else {
                    keep_steps.push(PlanStep::Keep {
                        sequence: 0,
                        slot: PlanSlot::from_domain(desired_slot.slot()),
                    });
                }
            }
            SlotTarget::Equipment(equipment) => {
                ensure_requested_equipment_compatibility(
                    state.equipment_catalog(),
                    ship,
                    desired_slot.slot(),
                    equipment,
                )?;
                if preserved_slots.contains(&desired_slot.slot()) {
                    let current = current.expect("保留的配装目标必须已有当前装备");
                    let target_level = equipment
                        .target_enhance_level()
                        .unwrap_or(current.enhance_level());
                    let target_definition = family_definition(
                        state.equipment_catalog(),
                        equipment.family_id(),
                        target_level,
                    )?;
                    ensure_equipment_compatible(ship, desired_slot.slot(), target_definition)?;
                    if target_level > current.enhance_level() {
                        let (steps, _) = build_enhance_steps(
                            state.equipment_catalog(),
                            equipment.family_id(),
                            PlanSource::from_domain(EquipmentSourceRef::ShipSlot(
                                desired_slot.slot(),
                            )),
                            current.config_id(),
                            current.enhance_level(),
                            target_level,
                            1,
                            &mut reservations,
                        )?;
                        current_enhance_steps.extend(steps);
                    } else {
                        keep_steps.push(PlanStep::Keep {
                            sequence: 0,
                            slot: PlanSlot::from_domain(desired_slot.slot()),
                        });
                    }
                    continue;
                }
                if current.is_some() {
                    // 外部来源只有在统一卸装阶段完成后才会进入目标槽位。
                    unequip_slots.insert(desired_slot.slot());
                }
                let selection = select_source(
                    state,
                    desired_slot,
                    equipment,
                    &preserved_slots,
                    &inventory_reservations,
                    &mut reservations,
                    compose_fallback_slot == Some(desired_slot.slot()),
                )?;
                if let Some(EquipmentSourceRef::ShipSlot(source_slot)) = selection.source_ref() {
                    unequip_slots.insert(source_slot);
                }
                let target: PlanEquipment = if let Some(source_ref) = selection.source_ref() {
                    resolve_target_equipment(
                        state,
                        equipment.family_id(),
                        selection.source_config_id(),
                        selection.source_level(),
                        source_ref,
                        equipment.target_enhance_level(),
                    )?
                } else {
                    let target_level = equipment
                        .target_enhance_level()
                        .unwrap_or(selection.source_level());
                    let family = state
                        .equipment_catalog()
                        .family(equipment.family_id())
                        .ok_or(PlanCheckError::EquipmentFamilyNotFound {
                            family_id: equipment.family_id(),
                        })?;
                    let definition = family.config(target_level).ok_or(
                        PlanCheckError::TargetEnhanceLevelUnavailable {
                            family_id: equipment.family_id(),
                            target_level: target_level.get(),
                        },
                    )?;
                    PlanEquipment::new(
                        equipment.family_id(),
                        definition.identity().config_id(),
                        target_level,
                    )
                };
                let target_config_id = EquipmentConfigId::new(target.config_id())
                    .expect("计划装备配置已经通过领域校验");
                ensure_equipment_compatible(
                    ship,
                    desired_slot.slot(),
                    config_definition(state.equipment_catalog(), target_config_id)?,
                )?;
                let (enhancement_steps, _) = build_enhance_steps(
                    state.equipment_catalog(),
                    equipment.family_id(),
                    PlanSource::Warehouse {
                        config_id: selection.source_config_id().get(),
                    },
                    selection.source_config_id(),
                    selection.source_level(),
                    EnhanceLevel::new(target.enhance_level()),
                    1,
                    &mut reservations,
                )?;
                let equip_source = if enhancement_steps.is_empty() {
                    selection.source()
                } else {
                    PlanSource::Warehouse {
                        config_id: target.config_id(),
                    }
                };
                let equip_step = PlanStep::Equip {
                    sequence: 0,
                    slot: PlanSlot::from_domain(desired_slot.slot()),
                    source: equip_source,
                    equipment: target,
                };
                equip_plans.push(match selection {
                    SourceSelection::Compose { recipe_id, .. } => {
                        PlannedEquip::Compose(PlannedCompose {
                            recipe_id,
                            enhancement_steps,
                            equip_step,
                        })
                    }
                    SourceSelection::Owned { .. } => PlannedEquip::Owned(PlannedOwned {
                        enhancement_steps,
                        equip_step,
                    }),
                });
            }
        }
    }

    for dismantle in &inventory_reservations.dismantles {
        if let EquipmentSourceRef::ShipSlot(source_slot) = dismantle.source_ref {
            unequip_slots.insert(source_slot);
        }
        dismantle_steps.push(PlanStep::Dismantle {
            sequence: 0,
            source: dismantle.source,
            equipment: dismantle.equipment,
            quantity: dismantle.quantity,
        });
    }

    let required_temporary_capacity: u64 =
        u64::try_from(unequip_slots.len()).map_err(|_| PlanCheckError::StepSequenceOverflow)?;
    let resources = state.resources();
    // 状态异常到已用容量超过上限时按零空位处理，避免计划继续扩大不一致。
    let available_temporary_capacity: u64 = resources
        .equipment_limit()
        .saturating_sub(resources.equipment_capacity());
    if required_temporary_capacity > available_temporary_capacity {
        return Err(PlanCheckError::TemporaryEquipmentCapacityUnavailable {
            available: available_temporary_capacity,
            required: required_temporary_capacity,
        });
    }

    let capacity_after_unloads = resources
        .equipment_capacity()
        .checked_add(required_temporary_capacity)
        .ok_or(PlanCheckError::QuantityOverflow {
            quantity: required_temporary_capacity,
        })?;
    let dismantle_quantity =
        inventory_reservations
            .dismantles
            .iter()
            .try_fold(0_u64, |total, dismantle| {
                total
                    .checked_add(dismantle.quantity)
                    .ok_or(PlanCheckError::QuantityOverflow {
                        quantity: dismantle.quantity,
                    })
            })?;
    let capacity_after_dismantle = capacity_after_unloads
        .checked_sub(dismantle_quantity)
        .ok_or(PlanCheckError::TemporaryEquipmentCapacityUnavailable {
            available: capacity_after_unloads,
            required: dismantle_quantity,
        })?;

    let mut owned_equip_plans: Vec<PlannedOwned> = Vec::new();
    let mut compose_equip_plans: Vec<PlannedCompose> = Vec::new();
    for plan in equip_plans {
        match plan {
            PlannedEquip::Owned(plan) => owned_equip_plans.push(plan),
            PlannedEquip::Compose(plan) => compose_equip_plans.push(plan),
        }
    }
    let owned_equip_quantity =
        u64::try_from(owned_equip_plans.len()).map_err(|_| PlanCheckError::StepSequenceOverflow)?;
    let capacity_before_compose = capacity_after_dismantle
        .checked_sub(owned_equip_quantity)
        .ok_or(PlanCheckError::EquipmentCapacitySnapshotMismatch {
            available: capacity_after_dismantle,
            required: owned_equip_quantity,
        })?;
    let compose_batch_capacity = resources
        .equipment_limit()
        .saturating_sub(capacity_before_compose);
    if !compose_equip_plans.is_empty() && compose_batch_capacity == 0 {
        return Err(PlanCheckError::ComposeEquipmentCapacityUnavailable {
            available: 0,
            required: 1,
        });
    }

    let compose_execution_steps = schedule_compose_steps(
        reservations.compose(),
        &compose_equip_plans,
        compose_batch_capacity,
    )?;

    // 独立合成在装配完成后入库，并共享配方次数和资源账本。
    let mut standalone_steps = Vec::new();
    let mut standalone_quantity = 0_u64;
    for &(recipe_id, count) in compositions {
        standalone_quantity = standalone_quantity
            .checked_add(count)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: count })?;
        if count == 0 || standalone_quantity > compose_batch_capacity {
            return Err(PlanCheckError::ComposeEquipmentCapacityUnavailable {
                available: compose_batch_capacity,
                required: standalone_quantity.max(1),
            });
        }
        let request = source_selection::compose_request(state, recipe_id)?;
        standalone_steps.push(PlanStep::Compose {
            sequence: 0,
            recipe_id,
            equipment: request.equipment,
            quantity: count,
            material_id: request.material_id,
            material_quantity_per_unit: request.material_per_unit,
            gold_per_unit: request.gold_per_unit,
        });
        match reservations.reserve_compose_quantity(request, count, false)? {
            ComposeReserve::Reserved => {}
            ComposeReserve::Unavailable(error) => return Err(error),
        }
    }
    // 先统一腾空并完成库存动作，再处理已有来源；合成按容量分批并立即强化、装配。
    let steps: Vec<PlanStep> = keep_steps
        .into_iter()
        .chain(unequip_slots.into_iter().map(|slot| PlanStep::Unequip {
            sequence: 0,
            slot: PlanSlot::from_domain(slot),
        }))
        .chain(dismantle_steps)
        .chain(inventory_enhance_steps)
        .chain(current_enhance_steps)
        .chain(
            owned_equip_plans
                .into_iter()
                .flat_map(PlannedOwned::into_steps),
        )
        .chain(compose_execution_steps)
        .chain(standalone_steps)
        .collect();
    let steps: Vec<PlanStep> = steps
        .into_iter()
        .enumerate()
        .map(|(index, step)| {
            let sequence =
                u32::try_from(index + 1).map_err(|_| PlanCheckError::StepSequenceOverflow)?;
            Ok(step.with_sequence(sequence))
        })
        .collect::<Result<Vec<_>, PlanCheckError>>()?;

    let (resource_constraints, resource_delta) = account_resources(
        state,
        &reservations,
        &inventory_reservations,
        &warehouse_enhance_outputs,
    )?;
    let game_state_content_sha256: String = state.source().content_sha256().to_owned();
    let content_sha256 = plan_digest(
        PLAN_SCHEMA_VERSION,
        &game_state_content_sha256,
        &desired_state_content_sha256,
        &inventory_plan_content_sha256,
        &steps,
        &resource_constraints,
        &resource_delta,
    )?;
    Ok(CompiledPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        game_state_content_sha256,
        desired_state_content_sha256,
        inventory_plan_content_sha256,
        steps,
        resource_constraints,
        resource_delta,
        content_sha256,
    })
}

#[derive(Clone)]
struct PlannedOwned {
    enhancement_steps: Vec<PlanStep>,
    equip_step: PlanStep,
}

impl PlannedOwned {
    fn into_steps(self) -> impl Iterator<Item = PlanStep> {
        self.enhancement_steps
            .into_iter()
            .chain(std::iter::once(self.equip_step))
    }
}

/// 待装配的合成结果。配方身份在进入排程前已经确定。
#[derive(Clone)]
pub(super) struct PlannedCompose {
    pub(super) recipe_id: u64,
    pub(super) enhancement_steps: Vec<PlanStep>,
    pub(super) equip_step: PlanStep,
}

enum PlannedEquip {
    Owned(PlannedOwned),
    Compose(PlannedCompose),
}

#[derive(Clone, Copy)]
pub(super) struct ValidatedCompose {
    pub(super) recipe_id: u64,
    pub(super) equipment: PlanEquipment,
    pub(super) quantity: u64,
    pub(super) material_id: u64,
    pub(super) material_quantity_per_unit: u64,
    pub(super) gold_per_unit: u64,
}

#[derive(Default)]
pub(super) struct ComposeReservations {
    pub(super) by_recipe: BTreeMap<u64, ValidatedCompose>,
    material_usage: BTreeMap<u64, u64>,
    output_usage: BTreeMap<EquipmentConfigId, u64>,
    gold_usage: u64,
    total_quantity: u64,
}

pub(super) struct WarehouseUnit {
    config_id: EquipmentConfigId,
    usage: u64,
}

pub(super) enum WarehouseUnitPreview {
    Available(WarehouseUnit),
    Short { available: u64, required: u64 },
}

pub(super) enum ComposeReserve {
    Reserved,
    Unavailable(PlanCheckError),
}

pub(super) struct ComposeRequest {
    pub(super) recipe_id: u64,
    pub(super) equipment: PlanEquipment,
    pub(super) material_id: u64,
    pub(super) material_per_unit: u64,
    pub(super) gold_per_unit: u64,
    pub(super) max_count: Option<u64>,
    pub(super) material_available: u64,
    pub(super) gold_available: u64,
    pub(super) output_config_id: EquipmentConfigId,
}

/// 同一次计划编译的仓库占用、舰船来源、合成预留和强化预留。
#[derive(Default)]
pub(super) struct Reservations {
    warehouse_usage: BTreeMap<EquipmentConfigId, u64>,
    ship_sources: BTreeSet<ShipSlotRef>,
    compose: ComposeReservations,
    enhance: EnhanceReservations,
}

impl Reservations {
    pub(super) fn warehouse_used(&self, config_id: EquipmentConfigId) -> u64 {
        self.warehouse_usage.get(&config_id).copied().unwrap_or(0)
    }

    /// 按当前占用和库存预留判断能否再取一件。不够或溢出时不改账本。
    pub(super) fn preview_warehouse_unit(
        &self,
        config_id: EquipmentConfigId,
        stack_quantity: u64,
        inventory_reserved: u64,
    ) -> Result<WarehouseUnitPreview, PlanCheckError> {
        let used = self.warehouse_used(config_id);
        let next_used = used
            .checked_add(1)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: u64::MAX })?;
        let required = next_used
            .checked_add(inventory_reserved)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: u64::MAX })?;
        if required > stack_quantity {
            Ok(WarehouseUnitPreview::Short {
                available: stack_quantity,
                required,
            })
        } else {
            Ok(WarehouseUnitPreview::Available(WarehouseUnit {
                config_id,
                usage: next_used,
            }))
        }
    }

    pub(super) fn commit_warehouse_unit(&mut self, unit: WarehouseUnit) {
        self.warehouse_usage.insert(unit.config_id, unit.usage);
    }

    pub(super) fn warehouse_usage(&self) -> &BTreeMap<EquipmentConfigId, u64> {
        &self.warehouse_usage
    }

    pub(super) fn ship_occupied(&self, slot: ShipSlotRef) -> bool {
        self.ship_sources.contains(&slot)
    }

    pub(super) fn occupy_ship_source(&mut self, slot: ShipSlotRef) {
        self.ship_sources.insert(slot);
    }

    pub(super) fn compose(&self) -> &ComposeReservations {
        &self.compose
    }

    /// 按配方和当前库存核算一件合成。资源不足时不改账本，溢出时也不改。
    pub(super) fn try_reserve_compose(
        &mut self,
        request: ComposeRequest,
    ) -> Result<ComposeReserve, PlanCheckError> {
        self.reserve_compose_quantity(request, 1, true)
    }

    fn reserve_compose_quantity(
        &mut self,
        request: ComposeRequest,
        count: u64,
        consumed: bool,
    ) -> Result<ComposeReserve, PlanCheckError> {
        let ComposeRequest {
            recipe_id,
            equipment,
            material_id,
            material_per_unit,
            gold_per_unit,
            max_count,
            material_available,
            gold_available,
            output_config_id,
        } = request;
        let material_total = material_per_unit
            .checked_mul(count)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: count })?;
        let gold_total = gold_per_unit
            .checked_mul(count)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: count })?;
        let quantity = self
            .compose
            .by_recipe
            .get(&recipe_id)
            .map_or(0, |compose| compose.quantity)
            .checked_add(count)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: u64::MAX })?;
        if max_count.is_some_and(|max_count| quantity > max_count) {
            return Ok(ComposeReserve::Unavailable(
                PlanCheckError::ComposeRecipeUnavailable { recipe_id },
            ));
        }
        let material_usage = self
            .compose
            .material_usage
            .get(&material_id)
            .copied()
            .unwrap_or(0)
            .checked_add(material_total)
            .ok_or(PlanCheckError::QuantityOverflow {
                quantity: material_per_unit,
            })?;
        if material_usage > material_available {
            return Ok(ComposeReserve::Unavailable(
                PlanCheckError::ComposeResourceUnavailable {
                    recipe_id,
                    key: ResourceKey::Item {
                        item_id: material_id,
                    },
                    available: material_available,
                    required: material_usage,
                },
            ));
        }
        let gold_usage = self.compose.gold_usage.checked_add(gold_total).ok_or(
            PlanCheckError::QuantityOverflow {
                quantity: gold_per_unit,
            },
        )?;
        if gold_usage > gold_available {
            return Ok(ComposeReserve::Unavailable(
                PlanCheckError::ComposeResourceUnavailable {
                    recipe_id,
                    key: ResourceKey::Gold,
                    available: gold_available,
                    required: gold_usage,
                },
            ));
        }
        let total_quantity = self
            .compose
            .total_quantity
            .checked_add(count)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: u64::MAX })?;
        let output_usage = self
            .compose
            .output_usage
            .get(&output_config_id)
            .copied()
            .unwrap_or(0)
            .checked_add(count)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: u64::MAX })?;
        self.compose
            .material_usage
            .insert(material_id, material_usage);
        self.compose.gold_usage = gold_usage;
        self.compose.total_quantity = total_quantity;
        if consumed {
            self.compose
                .output_usage
                .insert(output_config_id, output_usage);
        }
        self.compose.by_recipe.insert(
            recipe_id,
            ValidatedCompose {
                recipe_id,
                equipment,
                quantity,
                material_id,
                material_quantity_per_unit: material_per_unit,
                gold_per_unit,
            },
        );
        Ok(ComposeReserve::Reserved)
    }

    /// 把一级强化成本计入账本。任一数量溢出时，已有强化预留保持不变。
    pub(super) fn reserve_enhance_cost(
        &mut self,
        source_config_id: EquipmentConfigId,
        cost: &PlanEnhanceCost,
    ) -> Result<(), PlanCheckError> {
        let gold_usage = self.enhance.gold_usage.checked_add(cost.gold()).ok_or(
            PlanCheckError::QuantityOverflow {
                quantity: cost.gold(),
            },
        )?;
        let mut material_usage = self.enhance.material_usage.clone();
        for material in cost.materials() {
            let required = material_usage
                .get(&material.item_id())
                .copied()
                .unwrap_or(0)
                .checked_add(material.quantity())
                .ok_or(PlanCheckError::QuantityOverflow {
                    quantity: material.quantity(),
                })?;
            material_usage.insert(material.item_id(), required);
        }
        if self.enhance.first_source_config_id.is_none() {
            self.enhance.first_source_config_id = Some(source_config_id.get());
        }
        self.enhance.gold_usage = gold_usage;
        self.enhance.material_usage = material_usage;
        Ok(())
    }

    pub(in crate::application::plan::compiler) fn enhance(&self) -> &EnhanceReservations {
        &self.enhance
    }
}

pub(super) fn schedule_compose_steps(
    reservations: &ComposeReservations,
    equip_plans: &[PlannedCompose],
    batch_capacity: u64,
) -> Result<Vec<PlanStep>, PlanCheckError> {
    if equip_plans.is_empty() {
        return Ok(Vec::new());
    }

    let mut target_counts: BTreeMap<u64, u64> = BTreeMap::new();
    for plan in equip_plans {
        let count = target_counts.entry(plan.recipe_id).or_default();
        *count = count
            .checked_add(1)
            .ok_or(PlanCheckError::QuantityOverflow { quantity: *count })?;
    }
    for (&recipe_id, reservation) in &reservations.by_recipe {
        let targets = target_counts.get(&recipe_id).copied().unwrap_or(0);
        if reservation.quantity != targets {
            return Err(PlanCheckError::ComposeTargetReservationMismatch {
                recipe_id,
                reserved: reservation.quantity,
                targets,
            });
        }
    }

    let batch_capacity = usize::try_from(batch_capacity).unwrap_or(usize::MAX);
    let mut scheduled: Vec<PlanStep> = Vec::new();
    let mut offset = 0;
    while offset < equip_plans.len() {
        let recipe_id = equip_plans[offset].recipe_id;
        let adjacent_count = equip_plans[offset..]
            .iter()
            .take_while(|plan| plan.recipe_id == recipe_id)
            .count();
        let batch_count = adjacent_count.min(batch_capacity);
        if batch_count == 0 {
            return Err(PlanCheckError::ComposeEquipmentCapacityUnavailable {
                available: 0,
                required: 1,
            });
        }
        let quantity =
            u64::try_from(batch_count).map_err(|_| PlanCheckError::StepSequenceOverflow)?;
        let reservation = reservations.by_recipe.get(&recipe_id).ok_or(
            PlanCheckError::ComposeTargetReservationMismatch {
                recipe_id,
                reserved: 0,
                targets: quantity,
            },
        )?;
        scheduled.push(PlanStep::Compose {
            sequence: 0,
            recipe_id,
            equipment: reservation.equipment,
            quantity,
            material_id: reservation.material_id,
            material_quantity_per_unit: reservation.material_quantity_per_unit,
            gold_per_unit: reservation.gold_per_unit,
        });
        for plan in &equip_plans[offset..offset + batch_count] {
            scheduled.extend(plan.enhancement_steps.iter().cloned());
            scheduled.push(plan.equip_step.clone());
        }
        offset += batch_count;
    }
    Ok(scheduled)
}

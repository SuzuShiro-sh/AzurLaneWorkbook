//! 根据来源策略选择现有装备或合成来源，并记录本次计划的来源预留。

use super::super::{PlanCheckError, PlanEquipment, PlanSource};
use super::equipment_rules::{
    config_definition, config_family, current_equipment, current_source_incompatibility,
    ensure_equipment_compatible, ensure_family, find_ship, selected_target_definition,
};
use super::inventory_planner::InventoryReservations;
use super::{ComposeRequest, ComposeReserve, Reservations, WarehouseUnitPreview};
use crate::domain::{
    DesiredEquipment, DesiredSlotState, EnhanceLevel, EquipmentConfigId, EquipmentFamily,
    EquipmentFamilyId, EquipmentSourceRef, GameState, ShipEquipment, ShipProfile, ShipSlotRef,
    SourcePolicy,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(super) enum SourceSelection {
    Owned {
        source: PlanSource,
        source_ref: EquipmentSourceRef,
        source_config_id: EquipmentConfigId,
        source_level: EnhanceLevel,
    },
    Compose {
        recipe_id: u64,
        source_config_id: EquipmentConfigId,
        source_level: EnhanceLevel,
    },
}

impl SourceSelection {
    pub(super) const fn source(self) -> PlanSource {
        match self {
            Self::Owned { source, .. } => source,
            Self::Compose { recipe_id, .. } => PlanSource::Compose { recipe_id },
        }
    }

    pub(super) const fn source_ref(self) -> Option<EquipmentSourceRef> {
        match self {
            Self::Owned { source_ref, .. } => Some(source_ref),
            Self::Compose { .. } => None,
        }
    }

    pub(super) const fn source_config_id(self) -> EquipmentConfigId {
        match self {
            Self::Owned {
                source_config_id, ..
            }
            | Self::Compose {
                source_config_id, ..
            } => source_config_id,
        }
    }

    pub(super) const fn source_level(self) -> EnhanceLevel {
        match self {
            Self::Owned { source_level, .. } | Self::Compose { source_level, .. } => source_level,
        }
    }
}

pub(super) fn select_source(
    state: &GameState,
    desired_slot: &DesiredSlotState,
    equipment: DesiredEquipment,
    preserved_slots: &BTreeSet<ShipSlotRef>,
    inventory_reservations: &InventoryReservations,
    reservations: &mut Reservations,
    skip_compose: bool,
) -> Result<SourceSelection, PlanCheckError> {
    let family_id: EquipmentFamilyId = equipment.family_id();
    let family: &EquipmentFamily = state
        .equipment_catalog()
        .family(family_id)
        .ok_or(PlanCheckError::EquipmentFamilyNotFound { family_id })?;
    if let Some(target_level) = equipment.target_enhance_level()
        && family.config(target_level).is_none()
    {
        return Err(PlanCheckError::TargetEnhanceLevelUnavailable {
            family_id,
            target_level: target_level.get(),
        });
    }
    if equipment.source_policy() == SourcePolicy::ExactSource {
        let source: EquipmentSourceRef = equipment
            .exact_source()
            .ok_or(PlanCheckError::ExactSourceMissing)?;
        return select_exact_source(
            state,
            desired_slot.slot(),
            family_id,
            source,
            preserved_slots,
            inventory_reservations,
            reservations,
        );
    }

    let candidates = automatic_source_order(equipment.source_policy());
    let mut first_error: Option<PlanCheckError> = None;
    let mut compose_error: Option<PlanCheckError> = None;
    let target_ship = find_ship(state, desired_slot.slot().ship_instance_id())?;
    let mut incompatibility_error =
        current_source_incompatibility(state, target_ship, desired_slot.slot(), equipment)?;
    for candidate in candidates.iter().copied() {
        if skip_compose && matches!(candidate, AutomaticSource::Compose) {
            continue;
        }
        let result = match candidate {
            AutomaticSource::Warehouse => select_warehouse_source(
                state,
                desired_slot.slot(),
                family_id,
                equipment.target_enhance_level(),
                inventory_reservations,
                reservations,
            ),
            AutomaticSource::Compose => select_compose_source(
                state,
                desired_slot.slot(),
                family_id,
                equipment.target_enhance_level(),
                reservations,
            ),
            AutomaticSource::Ship => select_ship_source(
                state,
                desired_slot.slot(),
                family_id,
                equipment.target_enhance_level(),
                preserved_slots,
                inventory_reservations,
                reservations,
            ),
        };
        match result {
            Ok(selection) => return Ok(selection),
            Err(error @ PlanCheckError::EquipmentIncompatible { .. }) => {
                incompatibility_error.get_or_insert(error);
            }
            Err(error) if automatic_source_error_is_recoverable(&error) => {
                if matches!(candidate, AutomaticSource::Compose) {
                    compose_error = Some(error.clone());
                }
                first_error.get_or_insert(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(incompatibility_error
        .or(compose_error)
        .or(first_error)
        .unwrap_or(PlanCheckError::ComposeEquipmentCapacityUnavailable {
            available: 0,
            required: 1,
        }))
}

#[derive(Clone, Copy)]
enum AutomaticSource {
    Warehouse,
    Compose,
    Ship,
}

fn automatic_source_order(policy: SourcePolicy) -> &'static [AutomaticSource] {
    use AutomaticSource::{Compose, Ship, Warehouse};
    match policy {
        SourcePolicy::WarehouseThenCompose => &[Warehouse, Compose],
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip
        | SourcePolicy::WarehouseThenComposeThenShip => &[Warehouse, Compose, Ship],
        SourcePolicy::WarehouseThenShipThenCompose => &[Warehouse, Ship, Compose],
        SourcePolicy::ComposeThenWarehouseThenShip => &[Compose, Warehouse, Ship],
        SourcePolicy::WarehouseOnly => &[Warehouse],
        SourcePolicy::ComposeOnly => &[Compose],
        SourcePolicy::ShipOnly => &[Ship],
        SourcePolicy::ExactSource => &[],
    }
}

pub(super) fn source_policy_can_fallback_after_compose(policy: SourcePolicy) -> bool {
    matches!(
        policy,
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip
            | SourcePolicy::WarehouseThenComposeThenShip
            | SourcePolicy::ComposeThenWarehouseThenShip
    )
}

fn automatic_source_error_is_recoverable(error: &PlanCheckError) -> bool {
    matches!(
        error,
        PlanCheckError::NoWarehouseSource { .. }
            | PlanCheckError::SourceNotFound { .. }
            | PlanCheckError::EnhanceDowngrade { .. }
            | PlanCheckError::ComposeRecipeNotFound { .. }
            | PlanCheckError::ComposeRecipeUnavailable { .. }
            | PlanCheckError::ComposeResourceUnavailable { .. }
    )
}

fn select_exact_source(
    state: &GameState,
    target: ShipSlotRef,
    family_id: EquipmentFamilyId,
    source: EquipmentSourceRef,
    preserved_slots: &BTreeSet<ShipSlotRef>,
    inventory_reservations: &InventoryReservations,
    reservations: &mut Reservations,
) -> Result<SourceSelection, PlanCheckError> {
    match source {
        EquipmentSourceRef::Warehouse(config_id) => {
            if inventory_reservations
                .warehouse_dismantle
                .contains_key(&config_id)
            {
                return Err(PlanCheckError::InventorySourceConflict { source_ref: source });
            }
            let stack = state
                .equipment_inventory()
                .warehouse_stack(config_id)
                .ok_or(PlanCheckError::SourceNotFound { source_ref: source })?;
            ensure_family(source, stack.family_id(), family_id)?;
            let inventory_reserved = inventory_reservations
                .warehouse_enhance
                .get(&config_id)
                .copied()
                .unwrap_or(0);
            let unit = match reservations.preview_warehouse_unit(
                config_id,
                stack.quantity(),
                inventory_reserved,
            )? {
                WarehouseUnitPreview::Available(unit) => unit,
                WarehouseUnitPreview::Short {
                    available,
                    required,
                } => {
                    return Err(PlanCheckError::SourceUnavailable {
                        source_ref: source,
                        available,
                        required,
                    });
                }
            };
            reservations.commit_warehouse_unit(unit);
            Ok(SourceSelection::Owned {
                source: PlanSource::from_domain(source),
                source_ref: source,
                source_config_id: stack.config_id(),
                source_level: stack.enhance_level(),
            })
        }
        EquipmentSourceRef::ShipSlot(source_slot) => {
            if inventory_reservations.ship_dismantle.contains(&source_slot)
                || inventory_reservations.ship_enhance.contains(&source_slot)
            {
                return Err(PlanCheckError::InventorySourceConflict { source_ref: source });
            }
            ensure_source_slot_is_available(target, source_slot, preserved_slots, reservations)?;
            let source_ship: &ShipProfile = state
                .ships()
                .ships()
                .iter()
                .find(|ship| ship.identity().instance_id() == source_slot.ship_instance_id())
                .ok_or(PlanCheckError::SourceNotFound { source_ref: source })?;
            let source_equipment: ShipEquipment = current_equipment(source_ship, source_slot)
                .ok_or(PlanCheckError::SourceNotFound { source_ref: source })?;
            let actual_family_id: EquipmentFamilyId =
                config_family(state.equipment_catalog(), source_equipment.config_id())?;
            ensure_family(source, actual_family_id, family_id)?;
            reservations.occupy_ship_source(source_slot);
            Ok(SourceSelection::Owned {
                source: PlanSource::from_domain(source),
                source_ref: source,
                source_config_id: source_equipment.config_id(),
                source_level: source_equipment.enhance_level(),
            })
        }
    }
}

fn select_warehouse_source(
    state: &GameState,
    target: ShipSlotRef,
    family_id: EquipmentFamilyId,
    target_level: Option<EnhanceLevel>,
    inventory_reservations: &InventoryReservations,
    reservations: &mut Reservations,
) -> Result<SourceSelection, PlanCheckError> {
    let target_ship = find_ship(state, target.ship_instance_id())?;
    let mut lower_level: Option<(EnhanceLevel, SourceSelection, super::WarehouseUnit)> = None;
    let mut higher_level: Option<EnhanceLevel> = None;
    let mut incompatibility_error: Option<PlanCheckError> = None;
    for stack in state.equipment_inventory().warehouse() {
        if stack.family_id() != family_id {
            continue;
        }
        if inventory_reservations
            .warehouse_dismantle
            .contains_key(&stack.config_id())
        {
            continue;
        }
        let inventory_reserved = inventory_reservations
            .warehouse_enhance
            .get(&stack.config_id())
            .copied()
            .unwrap_or(0);
        let Some(unit) = (match reservations.preview_warehouse_unit(
            stack.config_id(),
            stack.quantity(),
            inventory_reserved,
        )? {
            WarehouseUnitPreview::Available(unit) => Some(unit),
            WarehouseUnitPreview::Short { .. } => None,
        }) else {
            continue;
        };
        let selection = SourceSelection::Owned {
            source: PlanSource::from_domain(stack.source()),
            source_ref: stack.source(),
            source_config_id: stack.config_id(),
            source_level: stack.enhance_level(),
        };
        let target_definition = selected_target_definition(
            state.equipment_catalog(),
            family_id,
            selection.source_config_id(),
            target_level,
        )?;
        if let Err(error) = ensure_equipment_compatible(target_ship, target, target_definition) {
            incompatibility_error.get_or_insert(error);
            continue;
        }
        match target_level {
            None => {
                reservations.commit_warehouse_unit(unit);
                return Ok(selection);
            }
            Some(target) if selection.source_level() == target => {
                reservations.commit_warehouse_unit(unit);
                return Ok(selection);
            }
            Some(target) if selection.source_level() < target => {
                let source_level = selection.source_level();
                if lower_level
                    .as_ref()
                    .is_none_or(|(best_level, _, _)| source_level > *best_level)
                {
                    lower_level = Some((source_level, selection, unit));
                }
            }
            Some(_) => {
                let source_level = selection.source_level();
                if higher_level.is_none_or(|best_level| source_level < best_level) {
                    higher_level = Some(source_level);
                }
            }
        }
    }
    if let Some(target) = target_level {
        if let Some((_, selection, unit)) = lower_level {
            reservations.commit_warehouse_unit(unit);
            return Ok(selection);
        }
        if let Some(source_level) = higher_level {
            return Err(PlanCheckError::EnhanceDowngrade {
                source_level: source_level.get(),
                target_level: target.get(),
            });
        }
    }
    if let Some(error) = incompatibility_error {
        return Err(error);
    }
    Err(PlanCheckError::NoWarehouseSource { family_id })
}

/// 静态配方与实时快照的校验同时供装配和独立合成使用。
pub(super) fn compose_request(
    state: &GameState,
    recipe_id: u64,
) -> Result<ComposeRequest, PlanCheckError> {
    let recipe = state
        .equipment_catalog()
        .recipes()
        .iter()
        .find(|recipe| recipe.recipe_id() == recipe_id)
        .ok_or(PlanCheckError::ComposeRecipeUnavailable { recipe_id })?;
    let definition = config_definition(state.equipment_catalog(), recipe.equipment_config_id())?;
    let live = state
        .bag()
        .item(recipe_id)
        .and_then(|item| item.compose())
        .ok_or(PlanCheckError::ComposeRecipeUnavailable { recipe_id })?;
    let material = recipe.material();
    if definition.enhancement().level().get() != 0
        || live.recipe_id() != recipe_id
        || live.material_id() != material.item_id()
        || live.material_count() != material.quantity()
        || live.gold() != recipe.gold()
        || live.equipment_config_id() != Some(recipe.equipment_config_id())
    {
        return Err(PlanCheckError::ComposeRecipeMismatch { recipe_id });
    }
    Ok(ComposeRequest {
        recipe_id,
        equipment: PlanEquipment::new(
            definition.identity().family_id(),
            recipe.equipment_config_id(),
            definition.enhancement().level(),
        ),
        material_id: material.item_id(),
        material_per_unit: material.quantity(),
        gold_per_unit: recipe.gold(),
        max_count: live.max_count(),
        material_available: state
            .bag()
            .item(material.item_id())
            .map_or(0, |item| item.quantity()),
        gold_available: state.resources().gold(),
        output_config_id: recipe.equipment_config_id(),
    })
}

fn select_compose_source(
    state: &GameState,
    target: ShipSlotRef,
    family_id: EquipmentFamilyId,
    target_level: Option<EnhanceLevel>,
    reservations: &mut Reservations,
) -> Result<SourceSelection, PlanCheckError> {
    let target_ship = find_ship(state, target.ship_instance_id())?;
    let mut family_recipe_seen = false;
    let mut availability_error: Option<PlanCheckError> = None;
    let mut incompatibility_error: Option<PlanCheckError> = None;
    for recipe in state.equipment_catalog().recipes() {
        let definition =
            config_definition(state.equipment_catalog(), recipe.equipment_config_id())?;
        if definition.identity().family_id() != family_id {
            continue;
        }
        family_recipe_seen = true;
        let source_level = definition.enhancement().level();
        if source_level.get() != 0 {
            return Err(PlanCheckError::ComposeRecipeMismatch {
                recipe_id: recipe.recipe_id(),
            });
        }
        if let Some(target) = target_level
            && source_level > target
        {
            return Err(PlanCheckError::EnhanceDowngrade {
                source_level: source_level.get(),
                target_level: target.get(),
            });
        }
        let request = match compose_request(state, recipe.recipe_id()) {
            Ok(request) => request,
            Err(error @ PlanCheckError::ComposeRecipeUnavailable { .. }) => {
                availability_error.get_or_insert(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let target_definition = selected_target_definition(
            state.equipment_catalog(),
            family_id,
            recipe.equipment_config_id(),
            target_level,
        )?;
        if let Err(error) = ensure_equipment_compatible(target_ship, target, target_definition) {
            incompatibility_error.get_or_insert(error);
            continue;
        }

        match reservations.try_reserve_compose(request)? {
            ComposeReserve::Reserved => {}
            ComposeReserve::Unavailable(error) => {
                availability_error.get_or_insert(error);
                continue;
            }
        }
        return Ok(SourceSelection::Compose {
            recipe_id: recipe.recipe_id(),
            source_config_id: recipe.equipment_config_id(),
            source_level,
        });
    }

    if !family_recipe_seen {
        return Err(PlanCheckError::ComposeRecipeNotFound { family_id });
    }
    if let Some(error) = availability_error {
        return Err(error);
    }
    if let Some(error) = incompatibility_error {
        return Err(error);
    }
    Err(PlanCheckError::ComposeRecipeNotFound { family_id })
}

fn select_ship_source(
    state: &GameState,
    target: ShipSlotRef,
    family_id: EquipmentFamilyId,
    target_level: Option<EnhanceLevel>,
    preserved_slots: &BTreeSet<ShipSlotRef>,
    inventory_reservations: &InventoryReservations,
    reservations: &mut Reservations,
) -> Result<SourceSelection, PlanCheckError> {
    let target_ship = find_ship(state, target.ship_instance_id())?;
    let mut lower_level: Option<(EnhanceLevel, SourceSelection, ShipSlotRef)> = None;
    let mut higher_level: Option<EnhanceLevel> = None;
    let mut incompatibility_error: Option<PlanCheckError> = None;
    for ship in state.ships().ships() {
        for slot in ship.slots() {
            let source_slot = ShipSlotRef::new(ship.identity().instance_id(), slot.index());
            if source_slot == target
                || preserved_slots.contains(&source_slot)
                || inventory_reservations.ship_dismantle.contains(&source_slot)
                || inventory_reservations.ship_enhance.contains(&source_slot)
                || reservations.ship_occupied(source_slot)
            {
                continue;
            }
            let Some(source_equipment) = slot.equipment() else {
                continue;
            };
            let actual_family_id =
                config_family(state.equipment_catalog(), source_equipment.config_id())?;
            if actual_family_id != family_id {
                continue;
            }
            let selection = SourceSelection::Owned {
                source: PlanSource::from_domain(EquipmentSourceRef::ShipSlot(source_slot)),
                source_ref: EquipmentSourceRef::ShipSlot(source_slot),
                source_config_id: source_equipment.config_id(),
                source_level: source_equipment.enhance_level(),
            };
            let target_definition = selected_target_definition(
                state.equipment_catalog(),
                family_id,
                selection.source_config_id(),
                target_level,
            )?;
            if let Err(error) = ensure_equipment_compatible(target_ship, target, target_definition)
            {
                incompatibility_error.get_or_insert(error);
                continue;
            }
            match target_level {
                None => {
                    reservations.occupy_ship_source(source_slot);
                    return Ok(selection);
                }
                Some(target) if selection.source_level() == target => {
                    reservations.occupy_ship_source(source_slot);
                    return Ok(selection);
                }
                Some(target) if selection.source_level() < target => {
                    let source_level = selection.source_level();
                    if lower_level
                        .as_ref()
                        .is_none_or(|(best_level, _, _)| source_level > *best_level)
                    {
                        lower_level = Some((source_level, selection, source_slot));
                    }
                }
                Some(_) => {
                    let source_level = selection.source_level();
                    if higher_level.is_none_or(|best_level| source_level < best_level) {
                        higher_level = Some(source_level);
                    }
                }
            }
        }
    }
    if let Some(target) = target_level {
        if let Some((_, selection, source_slot)) = lower_level {
            reservations.occupy_ship_source(source_slot);
            return Ok(selection);
        }
        if let Some(source_level) = higher_level {
            return Err(PlanCheckError::EnhanceDowngrade {
                source_level: source_level.get(),
                target_level: target.get(),
            });
        }
    }
    if let Some(error) = incompatibility_error {
        return Err(error);
    }
    Err(PlanCheckError::SourceNotFound {
        source_ref: EquipmentSourceRef::ShipSlot(target),
    })
}

fn ensure_source_slot_is_available(
    target: ShipSlotRef,
    source: ShipSlotRef,
    preserved_slots: &BTreeSet<ShipSlotRef>,
    reservations: &Reservations,
) -> Result<(), PlanCheckError> {
    if source == target {
        return Err(PlanCheckError::SourceEqualsTarget { slot: target });
    }
    if preserved_slots.contains(&source) {
        return Err(PlanCheckError::SourceTargetConflict { source_ref: source });
    }
    if reservations.ship_occupied(source) {
        return Err(PlanCheckError::SourceTargetConflict { source_ref: source });
    }
    Ok(())
}

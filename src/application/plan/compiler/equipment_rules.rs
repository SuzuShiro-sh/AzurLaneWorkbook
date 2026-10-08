//! 查询装备定义并判断来源、目标与舰船槽位的适配关系。

use super::super::{PlanCheckError, PlanEquipment};
use crate::domain::{
    DesiredEquipment, DesiredState, EnhanceLevel, EquipmentCatalog, EquipmentConfigId,
    EquipmentDefinition, EquipmentFamily, EquipmentFamilyId, EquipmentSourceRef, GameState,
    ShipEquipment, ShipEquipmentSlot, ShipInstanceId, ShipProfile, ShipSlotRef, SlotTarget,
    SourcePolicy,
};
use std::collections::BTreeSet;

pub(super) fn resolve_target_equipment(
    state: &GameState,
    family_id: EquipmentFamilyId,
    source_config_id: EquipmentConfigId,
    source_level: EnhanceLevel,
    source_ref: EquipmentSourceRef,
    target_level: Option<EnhanceLevel>,
) -> Result<PlanEquipment, PlanCheckError> {
    let family: &EquipmentFamily = state
        .equipment_catalog()
        .family(family_id)
        .ok_or(PlanCheckError::EquipmentFamilyNotFound { family_id })?;
    let source_config_family: EquipmentFamilyId =
        config_family(state.equipment_catalog(), source_config_id)?;
    if source_config_family != family_id {
        return Err(PlanCheckError::SourceFamilyMismatch {
            source_ref,
            actual_family_id: source_config_family,
            expected_family_id: family_id,
        });
    }
    let level: EnhanceLevel = target_level.unwrap_or(source_level);
    let definition = family
        .config(level)
        .ok_or(PlanCheckError::TargetEnhanceLevelUnavailable {
            family_id,
            target_level: level.get(),
        })?;
    if level < source_level {
        return Err(PlanCheckError::EnhanceDowngrade {
            source_level: source_level.get(),
            target_level: level.get(),
        });
    }
    Ok(PlanEquipment::new(
        family_id,
        definition.identity().config_id(),
        definition.enhancement().level(),
    ))
}

pub(super) fn find_ship(
    state: &GameState,
    ship_instance_id: ShipInstanceId,
) -> Result<&ShipProfile, PlanCheckError> {
    state
        .ships()
        .ships()
        .iter()
        .find(|ship| ship.identity().instance_id() == ship_instance_id)
        .ok_or(PlanCheckError::ShipNotFound { ship_instance_id })
}

pub(super) fn current_equipment(ship: &ShipProfile, slot: ShipSlotRef) -> Option<ShipEquipment> {
    ship.slots()
        .iter()
        .find(|item| item.index() == slot.slot_index())
        .and_then(|item| item.equipment())
}

pub(super) fn ensure_requested_equipment_compatibility(
    catalog: &EquipmentCatalog,
    ship: &ShipProfile,
    slot: ShipSlotRef,
    desired: DesiredEquipment,
) -> Result<(), PlanCheckError> {
    let family =
        catalog
            .family(desired.family_id())
            .ok_or(PlanCheckError::EquipmentFamilyNotFound {
                family_id: desired.family_id(),
            })?;
    if let Some(target_level) = desired.target_enhance_level() {
        return ensure_equipment_compatible(
            ship,
            slot,
            family_definition(catalog, desired.family_id(), target_level)?,
        );
    }
    if family
        .configs()
        .iter()
        .any(|definition| equipment_is_compatible(ship, slot, definition))
    {
        return Ok(());
    }
    let root_definition = family
        .configs()
        .first()
        .expect("装备族领域模型必须至少包含根配置");
    ensure_equipment_compatible(ship, slot, root_definition)
}

pub(super) fn ensure_equipment_compatible(
    ship: &ShipProfile,
    slot: ShipSlotRef,
    definition: &EquipmentDefinition,
) -> Result<(), PlanCheckError> {
    if equipment_is_compatible(ship, slot, definition) {
        return Ok(());
    }
    let target_slot = ship_slot(ship, slot);
    let equipment_type_id = definition
        .classification()
        .equipment_type()
        .equipment_type_id();
    let ship_type_id = ship.classification().ship_type().id();
    Err(PlanCheckError::EquipmentIncompatible {
        slot,
        config_id: definition.identity().config_id(),
        equipment_type_id,
        ship_type_id,
        allowed_equipment_type_ids: target_slot.allowed_equipment_type_ids().to_vec(),
        forbidden_ship_type_ids: definition
            .compatibility()
            .forbidden_ship_types()
            .iter()
            .map(|ship_type| ship_type.ship_type_id())
            .collect(),
    })
}

fn equipment_is_compatible(
    ship: &ShipProfile,
    slot: ShipSlotRef,
    definition: &EquipmentDefinition,
) -> bool {
    definition.can_be_equipped_by(
        ship.classification().ship_type().id(),
        ship_slot(ship, slot).allowed_equipment_type_ids(),
    )
}

fn ship_slot(ship: &ShipProfile, slot: ShipSlotRef) -> &ShipEquipmentSlot {
    ship.slots()
        .iter()
        .find(|item| item.index() == slot.slot_index())
        .expect("舰船领域模型必须包含 1 至 5 的固定槽位")
}

pub(super) fn family_definition(
    catalog: &EquipmentCatalog,
    family_id: EquipmentFamilyId,
    target_level: EnhanceLevel,
) -> Result<&EquipmentDefinition, PlanCheckError> {
    catalog
        .family(family_id)
        .ok_or(PlanCheckError::EquipmentFamilyNotFound { family_id })?
        .config(target_level)
        .ok_or(PlanCheckError::TargetEnhanceLevelUnavailable {
            family_id,
            target_level: target_level.get(),
        })
}

pub(super) fn selected_target_definition(
    catalog: &EquipmentCatalog,
    family_id: EquipmentFamilyId,
    source_config_id: EquipmentConfigId,
    target_level: Option<EnhanceLevel>,
) -> Result<&EquipmentDefinition, PlanCheckError> {
    target_level.map_or_else(
        || config_definition(catalog, source_config_id),
        |level| family_definition(catalog, family_id, level),
    )
}

pub(super) fn collect_preserved_slots(
    state: &GameState,
    desired: &DesiredState,
) -> Result<BTreeSet<ShipSlotRef>, PlanCheckError> {
    let mut preserved_slots = BTreeSet::new();
    for desired_slot in desired.slots() {
        match desired_slot.target() {
            SlotTarget::Keep => {
                preserved_slots.insert(desired_slot.slot());
            }
            SlotTarget::Equipment(equipment)
                if matches!(
                    equipment.source_policy(),
                    SourcePolicy::CurrentThenWarehouseThenComposeThenShip
                        | SourcePolicy::WarehouseThenCompose
                ) =>
            {
                let ship = find_ship(state, desired_slot.slot().ship_instance_id())?;
                if let Some(current) = current_equipment(ship, desired_slot.slot())
                    && (equipment.source_policy() != SourcePolicy::WarehouseThenCompose
                        || equipment
                            .target_enhance_level()
                            .is_none_or(|level| level == current.enhance_level()))
                    && current_equipment_satisfies(
                        state,
                        ship,
                        desired_slot.slot(),
                        current,
                        equipment,
                    )?
                {
                    preserved_slots.insert(desired_slot.slot());
                }
            }
            SlotTarget::Empty | SlotTarget::Equipment(_) => {}
        }
    }
    Ok(preserved_slots)
}

fn current_equipment_satisfies(
    state: &GameState,
    ship: &ShipProfile,
    slot: ShipSlotRef,
    current: ShipEquipment,
    desired: DesiredEquipment,
) -> Result<bool, PlanCheckError> {
    let Some(target_definition) =
        current_target_definition(state.equipment_catalog(), current, desired)?
    else {
        return Ok(false);
    };
    Ok(equipment_is_compatible(ship, slot, target_definition))
}

pub(super) fn current_source_incompatibility(
    state: &GameState,
    ship: &ShipProfile,
    slot: ShipSlotRef,
    desired: DesiredEquipment,
) -> Result<Option<PlanCheckError>, PlanCheckError> {
    if desired.source_policy() != SourcePolicy::CurrentThenWarehouseThenComposeThenShip {
        return Ok(None);
    }
    let Some(current) = current_equipment(ship, slot) else {
        return Ok(None);
    };
    let Some(target_definition) =
        current_target_definition(state.equipment_catalog(), current, desired)?
    else {
        return Ok(None);
    };
    Ok(ensure_equipment_compatible(ship, slot, target_definition).err())
}

fn current_target_definition(
    catalog: &EquipmentCatalog,
    current: ShipEquipment,
    desired: DesiredEquipment,
) -> Result<Option<&EquipmentDefinition>, PlanCheckError> {
    let current_family = config_family(catalog, current.config_id())?;
    if current_family != desired.family_id() {
        return Ok(None);
    }
    let target_level = desired
        .target_enhance_level()
        .unwrap_or(current.enhance_level());
    if target_level < current.enhance_level() {
        return Ok(None);
    }
    family_definition(catalog, desired.family_id(), target_level).map(Some)
}

pub(super) fn config_family(
    catalog: &EquipmentCatalog,
    config_id: EquipmentConfigId,
) -> Result<EquipmentFamilyId, PlanCheckError> {
    Ok(config_definition(catalog, config_id)?
        .identity()
        .family_id())
}

pub(super) fn config_definition(
    catalog: &EquipmentCatalog,
    config_id: EquipmentConfigId,
) -> Result<&EquipmentDefinition, PlanCheckError> {
    catalog
        .families()
        .iter()
        .flat_map(EquipmentFamily::configs)
        .find(|definition| definition.identity().config_id() == config_id)
        .ok_or(PlanCheckError::EquipmentConfigNotFound { config_id })
}

pub(super) fn ensure_family(
    source: EquipmentSourceRef,
    actual_family_id: EquipmentFamilyId,
    expected_family_id: EquipmentFamilyId,
) -> Result<(), PlanCheckError> {
    if actual_family_id == expected_family_id {
        Ok(())
    } else {
        Err(PlanCheckError::SourceFamilyMismatch {
            source_ref: source,
            actual_family_id,
            expected_family_id,
        })
    }
}

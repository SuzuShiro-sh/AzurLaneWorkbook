//! 将装备配置、实际来源、参数与效果聚合为单一装备总表。

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    ProjectionIndex, WorkbookProjectionBuilder, WorkbookProjectionError, blank, boolean,
    checked_add_count, compose_availability, decimal, family_compose_recipe, integer, integer_u64,
    json_cell, missing_reference, named_ship_types, optional_integer_u64, optional_text,
    skill_value_json, skill_visibility_value, text,
};
use crate::domain::{
    EquipmentDefinition, EquipmentFamily, EquipmentFamilyId, EquipmentResources,
    EquipmentSkillEffect, EquipmentSkillSource, EquipmentSkillVisibility, EquipmentWeapon,
    GameState, WeaponChargeParameter, WeaponPrecastParameter,
};

pub(super) fn project_equipment_inventory(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
) -> Result<(), WorkbookProjectionError> {
    for stack in state.equipment_inventory().warehouse() {
        let object_ref = format!("warehouse:{}", stack.config_id().get());
        let config = index.config("equipment_inventory", &object_ref, stack.config_id())?;
        let family = index.family(
            "equipment_inventory",
            &object_ref,
            config.identity().family_id(),
        )?;
        push_equipment_row(
            builder,
            state,
            index,
            &object_ref,
            "warehouse",
            Some(stack.runtime_group_id()),
            config,
            family,
            stack.quantity(),
            None,
            None,
            None,
        )?;
    }
    for ship in state.ships().ships() {
        for slot in ship.slots() {
            let Some(equipment) = slot.equipment() else {
                continue;
            };
            let object_ref = format!(
                "ship:{}:{}",
                ship.identity().instance_id().get(),
                slot.index().get()
            );
            let config = index.config("equipment_inventory", &object_ref, equipment.config_id())?;
            let family = index.family(
                "equipment_inventory",
                &object_ref,
                config.identity().family_id(),
            )?;
            push_equipment_row(
                builder,
                state,
                index,
                &object_ref,
                "ship",
                Some(equipment.runtime_id()),
                config,
                family,
                1,
                Some(ship.identity().instance_id().get()),
                Some(ship.identity().name()),
                Some(slot.index().get()),
            )?;
        }
    }
    for family in state.equipment_catalog().families() {
        for config in family.configs() {
            if config.enhancement().level().get() != 0
                || index
                    .owned_config_counts
                    .get(&config.identity().config_id())
                    .copied()
                    .unwrap_or(0)
                    != 0
            {
                continue;
            }
            let object_ref = format!("unowned:{}", config.identity().config_id().get());
            push_equipment_row(
                builder,
                state,
                index,
                &object_ref,
                "unowned",
                None,
                config,
                family,
                0,
                None,
                None,
                None,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_equipment_row(
    builder: &mut WorkbookProjectionBuilder,
    state: &GameState,
    index: &ProjectionIndex<'_>,
    object_ref: &str,
    source_type: &str,
    runtime_id: Option<u64>,
    config: &EquipmentDefinition,
    family: &EquipmentFamily,
    quantity: u64,
    ship_instance_id: Option<u64>,
    ship_name: Option<&str>,
    slot_index: Option<u8>,
) -> Result<(), WorkbookProjectionError> {
    let base = family
        .configs()
        .first()
        .expect("装备族已经确认至少含一个配置");
    let maximum = family
        .configs()
        .last()
        .expect("装备族已经确认至少含一个配置");
    let classification = config.classification();
    let compatibility = config.compatibility();
    let enhancement = config.enhancement();
    let dismantle_safety = config.dismantle_safety();
    let is_unowned = source_type == "unowned";
    let family_warehouse_quantity = index
        .warehouse_counts
        .get(&family.family_id())
        .copied()
        .unwrap_or(0);
    let family_equipped_quantity = index
        .equipped_counts
        .get(&family.family_id())
        .copied()
        .unwrap_or(0);
    let family_owned_quantity = family_warehouse_quantity
        .checked_add(family_equipped_quantity)
        .ok_or_else(|| WorkbookProjectionError::ArithmeticOverflow {
            sheet_key: "equipment_inventory",
            object_ref: object_ref.to_owned(),
            operation: "装备族拥有总数",
        })?;
    let compose = compose_availability(state, object_ref, family_compose_recipe(family, index)?)?;
    let family_potential_quantity = family_owned_quantity
        .checked_add(compose.actual.unwrap_or(0))
        .ok_or_else(|| WorkbookProjectionError::ArithmeticOverflow {
            sheet_key: "equipment_inventory",
            object_ref: object_ref.to_owned(),
            operation: "装备族潜在总数",
        })?;
    let raw_hash = index
        .raw_config_hashes
        .get(&config.identity().config_id())
        .copied();
    let attributes = attributes_json(config);
    let initial_attributes = attributes_json(base);
    let scope = state.source().read_scope();
    let weapons = if scope.equipment_weapons() {
        json_cell(
            "equipment_inventory",
            object_ref,
            "weapons_json",
            &weapons_json(state, config, object_ref)?,
        )?
    } else {
        blank()
    };
    let skill_references = skill_references_json(config);
    let skill_effects = if scope.equipment_skill_effects() {
        json_cell(
            "equipment_inventory",
            object_ref,
            "skill_effects_json",
            &skill_effects_json(state, config, object_ref)?,
        )?
    } else {
        blank()
    };
    let next_cost = equipment_resources_json(enhancement.next_cost());
    let restore_yield = equipment_resources_json(enhancement.restore_yield());
    let dismantle_yield = equipment_resources_json(enhancement.destroy_yield());

    builder.push_row(
        "equipment_inventory",
        object_ref,
        projection_values![
            "source_ref" => text(object_ref),
            "source_type" => text(source_type),
            "family_id" => text(family.family_id()),
            "runtime_id" => optional_text(runtime_id),
            "config_id" => text(config.identity().config_id()),
            "family_base_config_id" => text(base.identity().config_id()),
            "previous_config_id" => optional_text(enhancement.previous_config_id()),
            "next_config_id" => optional_text(enhancement.next_config_id()),
            "name" => text(config.identity().name()),
            "ship_instance_id" => optional_text(ship_instance_id),
            "ship_name" => optional_text(ship_name),
            "equipment_type" => text(classification.equipment_type().name()),
            "nation" => text(classification.nation().name()),
            "speciality" => text(classification.speciality()),
            "ammo_type" => text(classification.ammo_type()),
            "compatible_main_ship_types" => text(named_ship_types(compatibility.main_ship_types())),
            "compatible_sub_ship_types" => text(named_ship_types(compatibility.sub_ship_types())),
            "forbidden_ship_types" => text(named_ship_types(compatibility.forbidden_ship_types())),
            "description" => text(config.description()),
            "labels" => text(config.labels().join("，")),
            "effect_summary" => if scope.equipment_weapons() && scope.equipment_skill_effects() { text(inventory_effect_summary(state, config)) } else { blank() },
            "family_owned_enhance_distribution" => text(owned_enhance_distribution(state, family.family_id(), index)?),
            "blueprint_item_id" => optional_text(compose.material_id),
            "planned_usage" => blank(),
            "raw_config_hash" => optional_text(raw_hash),
            "read_errors" => if raw_hash.is_none() {
                text("缺少装备配置原始记录")
            } else {
                blank()
            },
            "attributes_json" => json_cell("equipment_inventory", object_ref, "attributes_json", &attributes)?,
            "family_initial_attributes_json" => json_cell("equipment_inventory", object_ref, "family_initial_attributes_json", &initial_attributes)?,
            "weapons_json" => weapons,
            "skill_references_json" => json_cell("equipment_inventory", object_ref, "skill_references_json", &skill_references)?,
            "skill_effects_json" => skill_effects,
            "compose_material_costs" => compose.material_costs,
            "next_cost_json" => json_cell("equipment_inventory", object_ref, "next_cost_json", &next_cost)?,
            "restore_yield_json" => json_cell("equipment_inventory", object_ref, "restore_yield_json", &restore_yield)?,
            "dismantle_yield_json" => json_cell("equipment_inventory", object_ref, "dismantle_yield_json", &dismantle_yield)?,
            "current_enhance_level" => integer(i64::from(enhancement.level().get())),
            "family_initial_enhance_level" => integer(i64::from(base.enhancement().level().get())),
            "maximum_enhance_level" => integer(i64::from(maximum.enhancement().level().get())),
            "rarity" => integer(i64::from(classification.rarity())),
            "tech_level" => integer(i64::from(classification.tech_level())),
            "gear_score" => integer_u64("equipment_inventory", object_ref, "gear_score", config.gear_score())?,
            "importance" => integer(i64::from(config.importance())),
            "equipment_limit" => integer_u64("equipment_inventory", object_ref, "equipment_limit", config.equipment_limit())?,
            "quantity" => integer_u64("equipment_inventory", object_ref, "quantity", quantity)?,
            "slot_index" => slot_index.map(|value| integer(i64::from(value))).unwrap_or_else(blank),
            "family_warehouse_quantity" => integer_u64("equipment_inventory", object_ref, "family_warehouse_quantity", family_warehouse_quantity)?,
            "family_equipped_quantity" => integer_u64("equipment_inventory", object_ref, "family_equipped_quantity", family_equipped_quantity)?,
            "family_owned_quantity" => integer_u64("equipment_inventory", object_ref, "family_owned_quantity", family_owned_quantity)?,
            "blueprint_count" => optional_integer_u64("equipment_inventory", object_ref, "blueprint_count", compose.material_available)?,
            "compose_blueprint_cost" => optional_integer_u64("equipment_inventory", object_ref, "compose_blueprint_cost", compose.material_required)?,
            "compose_gold_cost" => optional_integer_u64("equipment_inventory", object_ref, "compose_gold_cost", compose.gold_required)?,
            "craftable_by_gold" => optional_integer_u64("equipment_inventory", object_ref, "craftable_by_gold", compose.by_gold)?,
            "craftable_by_materials" => optional_integer_u64("equipment_inventory", object_ref, "craftable_by_materials", compose.by_material)?,
            "craftable_actual" => optional_integer_u64("equipment_inventory", object_ref, "craftable_actual", compose.actual)?,
            "family_potential_quantity" => integer_u64("equipment_inventory", object_ref, "family_potential_quantity", family_potential_quantity)?,
            "planned_required_count" => blank(),
            "planned_compose_count" => blank(),
            "planned_dismantle_count" => blank(),
            "simulated_remaining_count" => blank(),
            "dismantle_gold_yield" => integer_u64("equipment_inventory", object_ref, "dismantle_gold_yield", enhancement.destroy_yield().gold())?,
            "anti_siren_power" => config.anti_siren_power().map(decimal).unwrap_or_else(blank),
            "locked" => boolean(is_unowned || dismantle_safety.is_important()),
            "protected" => boolean(is_unowned || dismantle_safety.requires_confirmation()),
            "dismantlable" => boolean(!is_unowned && dismantle_safety.allows_automatic_dismantle()),
            "is_device" => boolean(classification.is_device()),
            "is_aircraft" => boolean(classification.is_aircraft()),
            "data_complete" => boolean(true),
            "config_data_complete" => boolean(raw_hash.is_some()),
            "operation" => blank(),
            "processing_quantity" => blank(),
            "target_enhance_level" => blank(),
            "note" => blank(),
        ],
    )
}

fn attributes_json(config: &EquipmentDefinition) -> Value {
    Value::Array(
        config
            .attributes()
            .iter()
            .map(|attribute| {
                json!({
                    "key": attribute.key(),
                    "name": attribute.name(),
                    "value": attribute.value(),
                    "auxiliary_boost": attribute.auxiliary_boost(),
                })
            })
            .collect(),
    )
}

fn weapons_json(
    state: &GameState,
    config: &EquipmentDefinition,
    object_ref: &str,
) -> Result<Value, WorkbookProjectionError> {
    config
        .weapon_ids()
        .iter()
        .map(|weapon_id| {
            state
                .equipment_details()
                .weapon(*weapon_id)
                .map(weapon_json)
                .ok_or_else(|| {
                    missing_reference(
                        "equipment_inventory",
                        object_ref,
                        "武器参数",
                        weapon_id.to_string(),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

fn weapon_json(weapon: &EquipmentWeapon) -> Value {
    json!({
        "weapon_id": weapon.weapon_id(),
        "base_weapon_id": weapon.base_weapon_id(),
        "weapon_type": weapon.weapon_type(),
        "aim_type": weapon.aim_type(),
        "attack_attribute": weapon.attack_attribute(),
        "action_index": weapon.action_index(),
        "fire_fx": weapon.fire_fx(),
        "fire_sfx": weapon.fire_sfx(),
        "spawn_bound": weapon.spawn_bound(),
        "barrage_ids": weapon.barrage_ids(),
        "bullet_ids": weapon.bullet_ids(),
        "search_type": weapon.search_type(),
        "search_conditions": weapon.search_conditions(),
        "oxygen_types": weapon.oxygen_types(),
        "charge_parameter": charge_parameter_json(weapon.charge_parameter()),
        "precast_parameter": precast_parameter_json(weapon.precast_parameter()),
        "angle": weapon.angle(),
        "fire_fx_loop_type": weapon.fire_fx_loop_type(),
        "barrage_count": weapon.barrage_ids().len(),
        "bullet_count": weapon.bullet_ids().len(),
        "damage": weapon.damage(),
        "reload_max": weapon.reload_max(),
        "range": weapon.range(),
        "minimum_range": weapon.min_range(),
        "axis_angle": weapon.axis_angle(),
        "attack_attribute_ratio": weapon.attack_attribute_ratio(),
        "corrected": weapon.corrected(),
        "suppress": weapon.suppress(),
        "initial_over_heat": weapon.initial_over_heat(),
        "queue": weapon.queue(),
        "shakescreen": weapon.shakescreen(),
        "effect_move": weapon.effect_move(),
        "expose": weapon.expose(),
        "torpedo_ammo": weapon.torpedo_ammo(),
        "auto_aftercast": weapon.auto_aftercast(),
        "recover_time": weapon.recover_time(),
    })
}

fn skill_references_json(config: &EquipmentDefinition) -> Value {
    Value::Array(
        config
            .skill_references()
            .iter()
            .copied()
            .enumerate()
            .map(|(index, reference)| {
                json!({
                    "reference_ordinal": index + 1,
                    "skill_id": reference.skill_id(),
                    "skill_level": reference.level(),
                    "skill_visibility": skill_visibility_value(reference.visibility()),
                })
            })
            .collect(),
    )
}

fn skill_effects_json(
    state: &GameState,
    config: &EquipmentDefinition,
    object_ref: &str,
) -> Result<Value, WorkbookProjectionError> {
    config
        .skill_references()
        .iter()
        .copied()
        .enumerate()
        .map(|(index, reference)| {
            let skill = state
                .equipment_details()
                .skill(reference.skill_id(), reference.level())
                .ok_or_else(|| {
                    missing_reference(
                        "equipment_inventory",
                        object_ref,
                        "装备技能",
                        format!("{}:{}", reference.skill_id(), reference.level()),
                    )
                })?;
            let sources = [
                ("battle_skill", skill.battle_skill()),
                ("battle_buff", skill.battle_buff()),
            ]
            .into_iter()
            .filter_map(|(source_kind, source)| {
                source.map(|source| skill_source_json(source_kind, source))
            })
            .collect::<Vec<_>>();
            Ok(json!({
                "reference_ordinal": index + 1,
                "skill_id": reference.skill_id(),
                "skill_level": reference.level(),
                "skill_visibility": skill_visibility_value(reference.visibility()),
                "hidden": reference.visibility() == EquipmentSkillVisibility::Hidden,
                "raw_data_ref": format!("equipment_skill:{}:{}", reference.skill_id(), reference.level()),
                "name": skill.display().name(),
                "description": skill.display().description(),
                "acquire_description": skill.display().acquire_description(),
                "sources": sources,
            }))
        })
        .collect::<Result<Vec<_>, WorkbookProjectionError>>()
        .map(Value::Array)
}

fn skill_source_json(source_kind: &str, source: &EquipmentSkillSource) -> Value {
    json!({
        "source_kind": source_kind,
        "config_id": source.config_id(),
        "name": source.name(),
        "description": source.description(),
        "cooldown": source.cooldown(),
        "duration": source.duration(),
        "stack": source.stack(),
        "effects": source.effects().iter().map(skill_effect_json).collect::<Vec<_>>(),
    })
}

fn skill_effect_json(effect: &EquipmentSkillEffect) -> Value {
    json!({
        "effect_sequence": effect.sequence(),
        "effect_type": effect.effect_type(),
        "target_choices": effect.target_choices(),
        "triggers": effect.triggers(),
        "arguments": effect.arguments().iter().map(|argument| {
            json!({"name": argument.name(), "value": skill_value_json(argument.value())})
        }).collect::<Vec<_>>(),
        "metadata": effect.metadata().iter().map(|field| {
            json!({"name": field.name(), "value": skill_value_json(field.value())})
        }).collect::<Vec<_>>(),
    })
}

fn equipment_resources_json(resources: &EquipmentResources) -> Value {
    json!({
        "gold": resources.gold(),
        "items": resources
            .items()
            .iter()
            .map(|item| json!({"item_id": item.item_id(), "quantity": item.quantity()}))
            .collect::<Vec<_>>(),
    })
}

fn charge_parameter_json(value: WeaponChargeParameter) -> Value {
    match value {
        WeaponChargeParameter::Empty => json!({"kind": "empty"}),
        WeaponChargeParameter::Lock {
            lock_time,
            max_lock,
        } => json!({"kind": "lock", "lock_time": lock_time, "max_lock": max_lock}),
    }
}

fn precast_parameter_json(value: &WeaponPrecastParameter) -> Value {
    match value {
        WeaponPrecastParameter::Values(values) => json!({"kind": "values", "values": values}),
        WeaponPrecastParameter::LegacyWhitespace => json!({"kind": "legacy_whitespace"}),
    }
}

fn owned_enhance_distribution(
    state: &GameState,
    family_id: EquipmentFamilyId,
    index: &ProjectionIndex<'_>,
) -> Result<String, WorkbookProjectionError> {
    let mut counts: BTreeMap<u8, u64> = BTreeMap::new();
    for stack in state
        .equipment_inventory()
        .warehouse()
        .iter()
        .filter(|stack| stack.family_id() == family_id)
    {
        checked_add_count(
            &mut counts,
            stack.enhance_level().get(),
            stack.quantity(),
            "equipment_inventory",
            format!("family:{}", family_id.get()),
            "强化等级仓库数量",
        )?;
    }
    for ship in state.ships().ships() {
        for equipment in ship.slots().iter().filter_map(|slot| slot.equipment()) {
            let config = index.config(
                "equipment_inventory",
                format!("family:{}", family_id.get()),
                equipment.config_id(),
            )?;
            if config.identity().family_id() == family_id {
                checked_add_count(
                    &mut counts,
                    equipment.enhance_level().get(),
                    1,
                    "equipment_inventory",
                    format!("family:{}", family_id.get()),
                    "强化等级舰上数量",
                )?;
            }
        }
    }
    Ok(counts
        .into_iter()
        .map(|(level, count)| format!("+{level}:{count}"))
        .collect::<Vec<_>>()
        .join("，"))
}

fn attribute_summary(config: &EquipmentDefinition) -> String {
    config
        .attributes()
        .iter()
        .map(|attribute| format!("{} {:+}", attribute.name(), attribute.value()))
        .collect::<Vec<_>>()
        .join("，")
}

fn weapon_summary(state: &GameState, config: &EquipmentDefinition) -> String {
    config
        .weapon_ids()
        .iter()
        .map(|weapon_id| {
            state
                .equipment_details()
                .weapon(*weapon_id)
                .map(|weapon| {
                    format!(
                        "{}:伤害{} 装填{} 射程{}",
                        weapon.weapon_id(),
                        weapon.damage(),
                        weapon.reload_max(),
                        weapon.range()
                    )
                })
                .unwrap_or_else(|| weapon_id.to_string())
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn equipment_skill_summary(
    state: &GameState,
    config: &EquipmentDefinition,
    visibility: EquipmentSkillVisibility,
) -> String {
    config
        .skill_references()
        .iter()
        .copied()
        .filter(|reference| reference.visibility() == visibility)
        .map(|reference| {
            state
                .equipment_details()
                .skill(reference.skill_id(), reference.level())
                .map(|skill| {
                    format!(
                        "[{}] {} Lv{}",
                        reference.skill_id(),
                        skill.display().name(),
                        reference.level()
                    )
                })
                .unwrap_or_else(|| format!("[{}] Lv{}", reference.skill_id(), reference.level()))
        })
        .collect::<Vec<_>>()
        .join("；")
}

/// 汇总完整属性、武器及带可见性分组的技能，保留原始标识与等级。
fn inventory_effect_summary(state: &GameState, config: &EquipmentDefinition) -> String {
    format!(
        "属性：{}\n武器：{}\n主技能：{}\n隐藏技能：{}",
        attribute_summary(config),
        weapon_summary(state, config),
        equipment_skill_summary(state, config, EquipmentSkillVisibility::Visible),
        equipment_skill_summary(state, config, EquipmentSkillVisibility::Hidden),
    )
}

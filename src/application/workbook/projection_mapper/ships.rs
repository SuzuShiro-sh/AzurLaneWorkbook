//! 负责持有实例和未持有静态舰船组的同表配装投影。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::{
    ProjectionIndex, ProjectionValue, ProjectionValues, SHIP_SKILL_EVIDENCE_INCOMPLETE,
    SHIP_SKILL_EVIDENCE_MISSING, WorkbookProjectionBuilder, WorkbookProjectionError, blank,
    boolean, decimal, equipment_effect_summary, integer, integer_u64, integer_usize, json_cell,
    skill_value_json, text,
};
use crate::domain::{
    GameState, ShipAttributeValues, ShipCatalogGroup, ShipEquipment, ShipEquipmentSlot,
    ShipProfile, ShipStaticSkill, SkillEffectEvidence,
};

/// 誓约秒值按 UTC 转为 Excel 日期；未誓约和零值不显示纪元时间。
fn propose_datetime(
    proposed: bool,
    seconds: u64,
    object_ref: &str,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    if !proposed {
        return Ok(blank());
    }
    unix_seconds_datetime(seconds, object_ref, "propose_time")
}

/// 客户端 Unix 秒转为 Excel 日期；零值留空。
fn unix_seconds_datetime(
    seconds: u64,
    object_ref: &str,
    field_key: &'static str,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    if seconds == 0 {
        return Ok(blank());
    }
    let millis = seconds
        .checked_mul(1000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| WorkbookProjectionError::IntegerOverflow {
            sheet_key: "loadout_plan".to_owned(),
            object_ref: object_ref.to_owned(),
            field_key: field_key.to_owned(),
            value: seconds,
        })?;
    Ok(ProjectionValue::date_time_unix_millis(millis))
}

pub(super) fn project_loadout_plan(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
) -> Result<(), WorkbookProjectionError> {
    let mut owned_group_ids = BTreeSet::new();
    for ship in state.ships().ships() {
        let object_ref = format!("ship:{}", ship.identity().instance_id().get());
        let identity = ship.identity();
        let growth = ship.growth();
        let intimacy = ship.intimacy();
        let classification = ship.classification();
        let static_group = state
            .ship_catalog()
            .group(classification.group_id())
            .ok_or_else(|| WorkbookProjectionError::MissingReference {
                sheet_key: "loadout_plan".to_owned(),
                object_ref: object_ref.clone(),
                target_type: "舰船静态组",
                target_ref: classification.group_id().to_string(),
            })?;
        owned_group_ids.insert(classification.group_id());
        let performance = ship.performance();
        let oil = performance.oil_cost();
        let stats = performance.attributes();
        let fleet_status = ship_fleet_status(ship);
        let mut values = projection_values![
            "source_type" => text("owned"),
            "source_ref" => text(format!("owned:{}", identity.instance_id().get())),
            "static_summary" => text(static_group_summary(state, static_group)),
            "static_raw_ref" => text(static_group_raw_refs(state, static_group)),
            "instance_id" => text(identity.instance_id()),
            "config_id" => text(identity.config_id()),
            "name" => text(if intimacy.proposed() && identity.name() != static_group.name() {
                format!("{}({})", identity.name(), static_group.name())
            } else { identity.name().to_owned() }),
            "original_name" => text(static_group.name()),
            "acquisition" => text("获取方式未获取"),
            "group_id" => text(classification.group_id()),
            "ship_type" => text(classification.ship_type().name()),
            "nation" => text(classification.nation().name()),
            "armor_type" => text(classification.armor_type().name()),
            "skin_id" => text(classification.skin_id()),
            "fleet_status" => text(fleet_status),
            "intimacy_stage" => text(intimacy.stage_description()),
            "read_errors" => blank(),
            "rarity" => integer(i64::from(classification.rarity())),
            "current_stars" => integer(i64::from(classification.stars().current())),
            "maximum_stars" => integer(i64::from(classification.stars().maximum())),
            "level" => integer(i64::from(growth.level())),
            "maximum_level" => integer(i64::from(growth.max_level())),
            "experience_in_level" => integer_u64("loadout_plan", &object_ref, "experience_in_level", growth.experience_in_level())?,
            "total_experience" => integer_u64("loadout_plan", &object_ref, "total_experience", growth.total_experience())?,
            "next_level_experience" => integer_u64("loadout_plan", &object_ref, "next_level_experience", growth.next_level_experience())?,
            "energy" => integer_u64("loadout_plan", &object_ref, "energy", growth.energy())?,
            "proficiency" => integer_u64("loadout_plan", &object_ref, "proficiency", growth.proficiency())?,
            "intimacy" => decimal(intimacy.raw_hundredths() as f64 / 100.0),
            "intimacy_maximum" => integer_u64("loadout_plan", &object_ref, "intimacy_maximum", intimacy.maximum())?,
            "create_time" => unix_seconds_datetime(identity.create_time(), &object_ref, "create_time")?,
            "propose_time" => propose_datetime(intimacy.proposed(), intimacy.propose_time(), &object_ref)?,
            "combat_power" => integer_u64("loadout_plan", &object_ref, "combat_power", performance.combat_power())?,
            "oil_start" => integer_u64("loadout_plan", &object_ref, "oil_start", oil.start())?,
            "oil_end" => integer_u64("loadout_plan", &object_ref, "oil_end", oil.end())?,
            "oil_total" => integer_u64("loadout_plan", &object_ref, "oil_total", oil.total())?,
            "learned_skill_count" => integer_usize("loadout_plan", &object_ref, "learned_skill_count", ship.skills().len())?,
            "locked" => boolean(performance.locked()),
            "proposed" => boolean(intimacy.proposed()),
            "data_complete" => boolean(true),
        ];
        push_ship_attributes(&mut values, "base", stats.base());
        push_ship_attributes(&mut values, "equipment_delta", stats.equipment_delta());
        push_ship_attributes(&mut values, "global_delta", stats.global_delta());
        push_ship_attributes(&mut values, "final", stats.effective());
        push_attribute_summaries(&mut values);
        push_ship_technology(&mut values, state, classification.group_id());
        push_ship_skills(&mut values, state, ship, &object_ref)?;
        for slot in ship.slots() {
            push_ship_slot_values(&mut values, &object_ref, slot, index)?;
        }
        builder.push_row("loadout_plan", object_ref, values)?;
    }
    for group in state.ship_catalog().groups() {
        if !owned_group_ids.contains(&group.group_id()) {
            project_unowned_group(state, index, group, builder)?;
        }
    }
    Ok(())
}

fn project_unowned_group(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    group: &ShipCatalogGroup,
    builder: &mut WorkbookProjectionBuilder,
) -> Result<(), WorkbookProjectionError> {
    let object_ref = format!("unowned:{}", group.group_id());
    let mut values = projection_values![
        "source_type" => text("unowned"),
        "source_ref" => text(&object_ref),
        "static_summary" => text(static_group_summary(state, group)),
        "static_raw_ref" => text(static_group_raw_refs(state, group)),
        "instance_id" => blank(),
        "config_id" => text(group.representative_config_id()),
        "name" => text(group.name()),
        "original_name" => text(group.name()),
        "acquisition" => text("获取方式未获取"),
        "group_id" => text(group.group_id()),
        "ship_type" => text(static_classification_name(group.ship_type_id(), &index.ship_type_names)),
        "nation" => text(static_classification_name(group.nation_id(), &index.nation_names)),
        "armor_type" => text(static_classification_name(group.armor_type_id(), &index.armor_type_names)),
        "skin_id" => blank(),
        "fleet_status" => blank(),
        "intimacy_stage" => blank(),
        "read_errors" => blank(),
        "rarity" => integer(i64::from(group.rarity())),
        "current_stars" => blank(),
        "maximum_stars" => integer(i64::from(group.maximum_stars())),
        "level" => blank(),
        "maximum_level" => blank(),
        "experience_in_level" => blank(),
        "total_experience" => blank(),
        "next_level_experience" => blank(),
        "energy" => blank(),
        "proficiency" => blank(),
        "intimacy" => blank(),
        "intimacy_maximum" => blank(),
        "create_time" => blank(),
        "propose_time" => blank(),
        "combat_power" => blank(),
        "oil_start" => blank(),
        "oil_end" => blank(),
        "oil_total" => blank(),
        "learned_skill_count" => blank(),
        "locked" => blank(),
        "proposed" => blank(),
        "data_complete" => blank(),
    ];
    for attribute in [
        "durability",
        "cannon",
        "torpedo",
        "air",
        "reload",
        "anti_aircraft",
        "hit",
        "dodge",
        "anti_sub",
        "luck",
        "speed",
    ] {
        for stage in ["base", "equipment_delta", "global_delta", "final"] {
            values.push((format!("stat_{attribute}_{stage}"), blank()));
        }
    }
    push_attribute_summaries(&mut values);
    push_ship_technology(&mut values, state, group.group_id());
    push_unowned_static_skills(&mut values, state, group, &object_ref)?;
    for (slot_index, allowed_type_ids) in group.slot_allowed_equipment_type_ids().iter().enumerate()
    {
        let slot = slot_index + 1;
        let prefix = format!("slot_{slot}");
        let allowed_equipment_types = equipment_type_values(allowed_type_ids, index);
        values.push((
            format!("{prefix}_allowed_equipment_types"),
            text(allowed_equipment_types),
        ));
        for suffix in [
            "equipment_name",
            "runtime_id",
            "config_id",
            "family_id",
            "enhance_level",
            "effect_summary",
        ] {
            values.push((format!("{prefix}_{suffix}"), blank()));
        }
        for suffix in [
            "target_equipment_family",
            "source_policy",
            "exact_source",
            "target_enhance_level",
            "allocation_priority",
            "note",
        ] {
            values.push((format!("{prefix}_{suffix}"), blank()));
        }
    }
    builder.push_row("loadout_plan", object_ref, values)
}

fn static_group_summary(state: &GameState, group: &ShipCatalogGroup) -> String {
    format!(
        "基础配置={}；英文名={}；变体={}；静态技能={}",
        group.representative_config_id(),
        group.english_name(),
        group
            .variant_config_ids()
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(","),
        static_skill_summary(state, group),
    )
}

fn static_group_raw_refs(state: &GameState, group: &ShipCatalogGroup) -> String {
    let mut refs = group
        .relationship_raw_refs()
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for skill_id in group.skill_ids() {
        let Some(skill) = state.ship_catalog().skill(*skill_id) else {
            continue;
        };
        refs.extend(skill.definition_raw_ref().map(str::to_owned));
        refs.extend(skill.display_raw_ref().map(str::to_owned));
        refs.extend(
            skill
                .effect_levels()
                .iter()
                .map(|level| format!("ship_skill:{skill_id}:{level}")),
        );
    }
    refs.into_iter().collect::<Vec<_>>().join("\n")
}

fn static_skill_summary(state: &GameState, group: &ShipCatalogGroup) -> String {
    group
        .skill_ids()
        .iter()
        .filter_map(|skill_id| state.ship_catalog().skill(*skill_id))
        .map(|skill| {
            let level = if skill.declared_max_level() == 0 {
                "声明max=0，默认效果Lv1".to_owned()
            } else {
                format!("Lv1-{}", skill.declared_max_level())
            };
            let name = if skill.name().is_empty() {
                "（定义缺失）"
            } else {
                skill.name()
            };
            format!("[{}] {} {}", skill.skill_id(), name, level)
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn push_unowned_static_skills(
    values: &mut ProjectionValues,
    state: &GameState,
    group: &ShipCatalogGroup,
    object_ref: &str,
) -> Result<(), WorkbookProjectionError> {
    let skills = group
        .skill_ids()
        .iter()
        .filter_map(|skill_id| state.ship_catalog().skill(*skill_id))
        .collect::<Vec<_>>();
    values.push((
        "skills_progress_summary".to_owned(),
        skill_lines(
            skills
                .iter()
                .map(|skill| {
                    format!(
                        "[{}] {}｜等级上限{}｜未持有",
                        skill.skill_id(),
                        skill.name(),
                        skill.declared_max_level()
                    )
                })
                .collect(),
        ),
    ));
    values.push((
        "skills_description_summary".to_owned(),
        skill_lines(
            skills
                .iter()
                .map(|skill| {
                    format!(
                        "[{}] {}\n描述：{}",
                        skill.skill_id(),
                        skill.name(),
                        skill.description()
                    )
                })
                .collect(),
        ),
    ));
    values.extend([
        (
            "skills_effective_skill_id".to_owned(),
            static_skill_lines(&skills, |skill| skill.skill_id().to_string()),
        ),
        (
            "skills_name".to_owned(),
            static_skill_lines(&skills, |skill| skill.name().to_owned()),
        ),
        (
            "skills_description".to_owned(),
            static_skill_lines(&skills, |skill| skill.description().to_owned()),
        ),
        (
            "skills_current_effect".to_owned(),
            static_skill_lines(&skills, |skill| {
                skill
                    .effect_levels()
                    .iter()
                    .map(|level| format!("ship_skill:{}:{level}", skill.skill_id()))
                    .collect::<Vec<_>>()
                    .join(",")
            }),
        ),
        ("skills_level".to_owned(), blank()),
        (
            "skills_maximum_level".to_owned(),
            static_skill_lines(&skills, |skill| skill.declared_max_level().to_string()),
        ),
        ("skills_experience".to_owned(), blank()),
        ("skills_next_level_experience".to_owned(), blank()),
        (
            "skills_data_complete".to_owned(),
            static_skill_lines(&skills, |skill| {
                if skill.evidence_gap().is_none() {
                    "是".to_owned()
                } else {
                    "否".to_owned()
                }
            }),
        ),
        (
            "skills_read_errors".to_owned(),
            static_skill_lines(&skills, |skill| {
                skill.evidence_gap().unwrap_or("（无）").to_owned()
            }),
        ),
    ]);
    if !state.source().read_scope().ship_skill_effects() {
        for (key, value) in values.iter_mut() {
            if matches!(
                key.as_str(),
                "skills_data_complete" | "skills_read_errors" | "skills_current_effect"
            ) {
                *value = blank();
            }
        }
        values.push(("skills_effect_parameters".to_owned(), blank()));
        values.push(("skills_raw_structure".to_owned(), blank()));
        return Ok(());
    }
    let effect_refs = Value::Array(
        skills
            .iter()
            .map(|skill| {
                json!({
                    "skill_id": skill.skill_id().to_string(),
                    "declared_max_level": skill.declared_max_level(),
                    "effect_refs": skill.effect_levels().iter().map(|level| {
                        let evidence = state.ship_catalog().skill_effects().evidence(skill.skill_id(), *level);
                        json!({
                            "level": level,
                            "source_ref": format!("ship_skill:{}:{level}", skill.skill_id()),
                            "complete": evidence.is_some_and(SkillEffectEvidence::complete),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    );
    let raw_refs = Value::Array(
        skills
            .iter()
            .map(|skill| {
                json!({
                    "skill_id": skill.skill_id().to_string(),
                    "definition_raw_ref": skill.definition_raw_ref(),
                    "display_raw_ref": skill.display_raw_ref(),
                    "effect_raw_refs": skill.effect_levels().iter().map(|level| {
                        format!("ship_skill:{}:{level}", skill.skill_id())
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    );
    values.extend([
        (
            "skills_effect_parameters".to_owned(),
            json_cell(
                "loadout_plan",
                object_ref,
                "skills_effect_parameters",
                &effect_refs,
            )?,
        ),
        (
            "skills_raw_structure".to_owned(),
            json_cell(
                "loadout_plan",
                object_ref,
                "skills_raw_structure",
                &raw_refs,
            )?,
        ),
    ]);
    Ok(())
}

fn static_skill_lines(
    skills: &[&ShipStaticSkill],
    value: impl Fn(&ShipStaticSkill) -> String,
) -> ProjectionValue {
    skill_lines(
        skills
            .iter()
            .map(|skill| skill_line(skill.skill_id(), value(skill)))
            .collect(),
    )
}

/// 将客户端已经确认的多编队关系压缩为可扫描的工作簿文本。
fn ship_fleet_status(ship: &ShipProfile) -> String {
    if ship.fleet_memberships().is_empty() {
        return "未编队".to_owned();
    }
    ship.fleet_memberships()
        .iter()
        .map(|membership| {
            let name = membership
                .display_name()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("编队{}", membership.fleet_id()));
            format!(
                "{}（{}）·{}{}",
                name,
                membership.kind().label(),
                membership.team().label(),
                membership.position()
            )
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn push_ship_skills(
    values: &mut ProjectionValues,
    state: &GameState,
    ship: &ShipProfile,
    object_ref: &str,
) -> Result<(), WorkbookProjectionError> {
    let mut effective_skill_ids = Vec::new();
    let mut names = Vec::new();
    let mut descriptions = Vec::new();
    let mut current_effects = Vec::new();
    let mut levels = Vec::new();
    let mut maximum_levels = Vec::new();
    let mut experiences = Vec::new();
    let mut next_level_experiences = Vec::new();
    let mut data_complete = Vec::new();
    let mut read_errors = Vec::new();
    let mut effect_parameters = Vec::new();
    let mut raw_structures = Vec::new();

    let mut progress_summaries = Vec::new();
    let mut description_summaries = Vec::new();
    for skill in ship.skills() {
        let skill_id = skill.identity().skill_id();
        let progress = skill.progress();
        let experience = if progress.level() >= progress.max_level() {
            "已满级".to_owned()
        } else {
            format!(
                "经验{}/{}",
                progress.experience(),
                progress.next_level_experience()
            )
        };
        progress_summaries.push(format!(
            "[{skill_id}] {}｜等级{}/{}｜{experience}",
            skill.identity().name(),
            progress.level(),
            progress.max_level()
        ));
        let description = if skill.description_template() == skill.current_effect() {
            format!("描述/当前：{}", skill.current_effect())
        } else {
            format!(
                "描述：{}\n当前：{}",
                skill.description_template(),
                skill.current_effect()
            )
        };
        description_summaries.push(format!(
            "[{skill_id}] {}\n{description}",
            skill.identity().name()
        ));
        effective_skill_ids.push(skill_line(skill_id, skill.identity().effective_skill_id()));
        names.push(skill_line(skill_id, skill.identity().name()));
        descriptions.push(skill_line(skill_id, skill.description_template()));
        current_effects.push(skill_line(skill_id, skill.current_effect()));
        levels.push(skill_line(skill_id, progress.level()));
        maximum_levels.push(skill_line(skill_id, progress.max_level()));
        experiences.push(skill_line(skill_id, progress.experience()));
        next_level_experiences.push(skill_line(skill_id, progress.next_level_experience()));
        if state.source().read_scope().ship_skill_effects() {
            let skill_object_ref =
                format!("ship-skill:{}:{skill_id}", ship.identity().instance_id());
            let evidence = state
                .ships()
                .skill_effect(skill.identity().effective_skill_id(), progress.level());
            let evidence_values = ship_skill_evidence_values(evidence, &skill_object_ref)?;
            data_complete.push(skill_line(
                skill_id,
                if evidence_values.data_complete {
                    "是"
                } else {
                    "否"
                },
            ));
            read_errors.push(skill_line(
                skill_id,
                evidence_values.read_errors.as_deref().unwrap_or("（无）"),
            ));
            effect_parameters.push(json!({
                "skill_id": skill_id.to_string(),
                "value": evidence_values.effect_parameters,
            }));
            raw_structures.push(json!({
                "skill_id": skill_id.to_string(),
                "value": evidence_values.raw_structure,
            }));
        }
    }

    values.push((
        "skills_progress_summary".to_owned(),
        skill_lines(progress_summaries),
    ));
    values.push((
        "skills_description_summary".to_owned(),
        skill_lines(description_summaries),
    ));
    values.extend([
        (
            "skills_effective_skill_id".to_owned(),
            skill_lines(effective_skill_ids),
        ),
        ("skills_name".to_owned(), skill_lines(names)),
        ("skills_description".to_owned(), skill_lines(descriptions)),
        (
            "skills_current_effect".to_owned(),
            skill_lines(current_effects),
        ),
        ("skills_level".to_owned(), skill_lines(levels)),
        (
            "skills_maximum_level".to_owned(),
            skill_lines(maximum_levels),
        ),
        ("skills_experience".to_owned(), skill_lines(experiences)),
        (
            "skills_next_level_experience".to_owned(),
            skill_lines(next_level_experiences),
        ),
        (
            "skills_data_complete".to_owned(),
            skill_lines(data_complete),
        ),
        ("skills_read_errors".to_owned(), skill_lines(read_errors)),
        (
            "skills_effect_parameters".to_owned(),
            json_cell(
                "loadout_plan",
                object_ref,
                "skills_effect_parameters",
                &Value::Array(effect_parameters),
            )?,
        ),
        (
            "skills_raw_structure".to_owned(),
            json_cell(
                "loadout_plan",
                object_ref,
                "skills_raw_structure",
                &Value::Array(raw_structures),
            )?,
        ),
    ]);
    if !state.source().read_scope().ship_skill_effects() {
        for (key, value) in values.iter_mut() {
            if matches!(
                key.as_str(),
                "skills_data_complete"
                    | "skills_read_errors"
                    | "skills_effect_parameters"
                    | "skills_raw_structure"
            ) {
                *value = blank();
            }
        }
    }
    Ok(())
}

fn skill_line(skill_id: u64, value: impl std::fmt::Display) -> String {
    let value = value.to_string();
    let value = if value.is_empty() {
        "（空）"
    } else {
        &value
    };
    format!("[{skill_id}] {value}")
}

fn skill_lines(lines: Vec<String>) -> ProjectionValue {
    if lines.is_empty() {
        blank()
    } else {
        text(lines.join("\n"))
    }
}

pub(super) struct ShipSkillEvidenceValues {
    pub(super) effect_parameters: Value,
    pub(super) raw_structure: Value,
    pub(super) data_complete: bool,
    pub(super) read_errors: Option<String>,
}

pub(super) fn ship_skill_evidence_values(
    evidence: Option<&SkillEffectEvidence>,
    object_ref: &str,
) -> Result<ShipSkillEvidenceValues, WorkbookProjectionError> {
    let Some(evidence) = evidence else {
        return Ok(ShipSkillEvidenceValues {
            effect_parameters: Value::Null,
            raw_structure: Value::Null,
            data_complete: false,
            read_errors: Some(SHIP_SKILL_EVIDENCE_MISSING.to_owned()),
        });
    };

    let parameters = Value::Array(
        evidence
            .parameter_sources()
            .iter()
            .map(|source| {
                json!({
                    "source_kind": source.kind().as_str(),
                    "effect_list": source.effects().iter().map(|effect| {
                        json!({
                            "sequence": effect.sequence(),
                            "arg_list": effect.arguments().iter().map(|argument| {
                                json!({
                                    "name": argument.name(),
                                    "value": skill_value_json(argument.value()),
                                })
                            }).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect(),
    );
    let read_errors = match evidence.read_errors() {
        [] if evidence.complete() => None,
        [] => Some(SHIP_SKILL_EVIDENCE_INCOMPLETE.to_owned()),
        errors => Some(errors.join(" | ")),
    };
    let raw_structure = serde_json::from_str(evidence.raw_structure_json()).map_err(|source| {
        WorkbookProjectionError::JsonEncode {
            sheet_key: "loadout_plan",
            object_ref: object_ref.to_owned(),
            field_key: "skills_raw_structure",
            source,
        }
    })?;
    Ok(ShipSkillEvidenceValues {
        effect_parameters: parameters,
        raw_structure,
        data_complete: evidence.complete(),
        read_errors,
    })
}

struct CurrentEquipmentValues {
    runtime_id: ProjectionValue,
    config_id: ProjectionValue,
    family_id: ProjectionValue,
    name: ProjectionValue,
    enhance_level: ProjectionValue,
    effect_summary: ProjectionValue,
}

fn current_equipment_values(
    sheet_key: &str,
    object_ref: &str,
    equipment: Option<ShipEquipment>,
    index: &ProjectionIndex<'_>,
) -> Result<CurrentEquipmentValues, WorkbookProjectionError> {
    let Some(equipment) = equipment else {
        return Ok(CurrentEquipmentValues {
            runtime_id: blank(),
            config_id: blank(),
            family_id: blank(),
            name: blank(),
            enhance_level: blank(),
            effect_summary: blank(),
        });
    };
    let config = index.config(sheet_key, object_ref, equipment.config_id())?;
    Ok(CurrentEquipmentValues {
        runtime_id: text(equipment.runtime_id()),
        config_id: text(equipment.config_id()),
        family_id: text(config.identity().family_id()),
        name: text(config.identity().name()),
        enhance_level: integer(i64::from(equipment.enhance_level().get())),
        effect_summary: text(equipment_effect_summary(config)),
    })
}

fn slot_equipment_type_values(slot: &ShipEquipmentSlot, index: &ProjectionIndex<'_>) -> String {
    equipment_type_values(slot.allowed_equipment_type_ids(), index)
}

fn equipment_type_values(equipment_type_ids: &[u64], index: &ProjectionIndex<'_>) -> String {
    let mut identified_names = Vec::with_capacity(equipment_type_ids.len());
    for equipment_type_id in equipment_type_ids {
        if let Some(name) = index.equipment_type_names.get(equipment_type_id) {
            identified_names.push(format!("{equipment_type_id}:{name}"));
        } else {
            let identifier = equipment_type_id.to_string();
            identified_names.push(identifier);
        }
    }
    identified_names.join("，")
}

fn static_classification_name(id: u64, names: &BTreeMap<u64, &str>) -> String {
    names
        .get(&id)
        .map_or_else(|| format!("[{id}] 未解析"), |name| format!("[{id}] {name}"))
}

fn push_ship_attributes(
    values: &mut ProjectionValues,
    stage: &str,
    attributes: ShipAttributeValues,
) {
    for (attribute, value) in [
        ("durability", attributes.durability()),
        ("cannon", attributes.cannon()),
        ("torpedo", attributes.torpedo()),
        ("air", attributes.air()),
        ("reload", attributes.reload()),
        ("anti_aircraft", attributes.anti_aircraft()),
        ("hit", attributes.hit()),
        ("dodge", attributes.dodge()),
        ("anti_sub", attributes.anti_sub()),
        ("luck", attributes.luck()),
        ("speed", attributes.speed()),
    ] {
        values.push((format!("stat_{attribute}_{stage}"), decimal(value)));
    }
}

fn push_ship_slot_values(
    values: &mut ProjectionValues,
    object_ref: &str,
    slot: &ShipEquipmentSlot,
    index: &ProjectionIndex<'_>,
) -> Result<(), WorkbookProjectionError> {
    let prefix = format!("slot_{}", slot.index().get());
    let allowed_equipment_types = slot_equipment_type_values(slot, index);
    values.push((
        format!("{prefix}_allowed_equipment_types"),
        text(allowed_equipment_types),
    ));
    let current = current_equipment_values("loadout_plan", object_ref, slot.equipment(), index)?;
    values.extend([
        (format!("{prefix}_equipment_name"), current.name),
        (format!("{prefix}_runtime_id"), current.runtime_id),
        (format!("{prefix}_config_id"), current.config_id),
        (format!("{prefix}_family_id"), current.family_id),
        (format!("{prefix}_enhance_level"), current.enhance_level),
        (format!("{prefix}_effect_summary"), current.effect_summary),
        (format!("{prefix}_target_equipment_family"), blank()),
        (format!("{prefix}_source_policy"), blank()),
        (format!("{prefix}_exact_source"), blank()),
        (format!("{prefix}_target_enhance_level"), blank()),
        (format!("{prefix}_allocation_priority"), blank()),
        (format!("{prefix}_note"), blank()),
    ]);
    Ok(())
}

/// 合并现有阶段值，不重复读取属性；未持有行保持空白。
fn push_attribute_summaries(values: &mut ProjectionValues) {
    for attribute in [
        "durability",
        "cannon",
        "torpedo",
        "air",
        "reload",
        "anti_aircraft",
        "hit",
        "dodge",
        "anti_sub",
        "luck",
        "speed",
    ] {
        let numbers: Option<Vec<_>> = ["base", "equipment_delta", "global_delta", "final"]
            .iter()
            .map(|stage| {
                values
                    .iter()
                    .find(|(key, _)| key == &format!("stat_{attribute}_{stage}"))
                    .and_then(|(_, value)| {
                        if let ProjectionValue::Decimal(value) = value {
                            Some(if value.fract() == 0.0 {
                                format!("{value:.0}")
                            } else {
                                format!("{value:.2}")
                            })
                        } else {
                            None
                        }
                    })
            })
            .collect();
        values.push((
            format!("stat_{attribute}_summary"),
            numbers
                .map(|parts| text(parts.join("/")))
                .unwrap_or_else(blank),
        ));
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn attribute_summary_preserves_stage_order_and_decimal_precision() {
        let mut values = vec![
            (
                "stat_durability_base".to_owned(),
                ProjectionValue::Decimal(1787.8),
            ),
            (
                "stat_durability_equipment_delta".to_owned(),
                ProjectionValue::Decimal(500.0),
            ),
            (
                "stat_durability_global_delta".to_owned(),
                ProjectionValue::Decimal(300.0),
            ),
            (
                "stat_durability_final".to_owned(),
                ProjectionValue::Decimal(2587.8),
            ),
        ];
        push_attribute_summaries(&mut values);
        assert_eq!(
            values
                .iter()
                .find(|(key, _)| key == "stat_durability_summary")
                .unwrap()
                .1,
            text("1787.80/500/300/2587.80")
        );
        assert_eq!(
            values
                .iter()
                .find(|(key, _)| key == "stat_cannon_summary")
                .unwrap()
                .1,
            blank()
        );
        assert_eq!(values[0].1, ProjectionValue::Decimal(1787.8));
    }
}

#[cfg(test)]
mod propose_time_tests {
    use super::*;

    #[test]
    fn utc_seconds_become_milliseconds_and_absent_dates_stay_blank() {
        assert_eq!(
            propose_datetime(true, 1_700_000_000, "ship:1").unwrap(),
            ProjectionValue::date_time_unix_millis(1_700_000_000_000)
        );
        assert_eq!(
            propose_datetime(false, 1_700_000_000, "ship:1").unwrap(),
            blank()
        );
        assert_eq!(propose_datetime(true, 0, "ship:1").unwrap(), blank());
        assert_eq!(
            unix_seconds_datetime(1_700_000_000, "ship:1", "create_time").unwrap(),
            ProjectionValue::date_time_unix_millis(1_700_000_000_000)
        );
        assert_eq!(
            unix_seconds_datetime(0, "ship:1", "create_time").unwrap(),
            blank()
        );
        assert!(matches!(
            propose_datetime(true, u64::MAX, "ship:1"),
            Err(WorkbookProjectionError::IntegerOverflow { .. })
        ));
    }
}

fn push_ship_technology(values: &mut ProjectionValues, state: &GameState, group_id: u64) {
    let summaries = if !state.source().read_scope().ship_technology() {
        Default::default()
    } else if let Some(error) = state.ship_catalog().technology_read_error() {
        std::array::from_fn(|_| format!("科技数据未获取：{error}"))
    } else {
        crate::application::ship_technology_summaries(
            group_id,
            state.ship_catalog().technology_history_available(),
            |table, record_id| {
                state
                    .raw_records()
                    .record(&crate::domain::RawRecordKey::ShipCatalog {
                        table_key: table.to_owned(),
                        record_id,
                    })
                    .map(|record| {
                        serde_json::from_str(record.canonical_json())
                            .map_err(|e| format!("{table}:{record_id}: {e}"))
                    })
                    .transpose()
            },
        )
        .unwrap_or_else(|error| std::array::from_fn(|_| format!("科技数据未获取：{error}")))
    };
    let bonus = crate::application::technology_bonus_summary(&summaries);
    values.push((
        "technology_bonus".to_owned(),
        if bonus.is_empty() {
            blank()
        } else {
            text(bonus)
        },
    ));
    for (key, summary) in crate::application::TECHNOLOGY_FIELDS
        .into_iter()
        .zip(summaries)
    {
        values.push((
            key.to_owned(),
            if summary.is_empty() {
                blank()
            } else {
                text(summary)
            },
        ));
    }
}

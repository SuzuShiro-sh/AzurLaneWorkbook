//! 覆盖完整游戏状态到工作簿投影的跨领域映射契约。

use std::sync::Arc;

use serde_json::json;

use super::raw::split_raw_json;
use super::ships::ship_skill_evidence_values;
use super::{
    RAW_JSON_CHUNK_UTF16_UNITS, SHIP_SKILL_EVIDENCE_MISSING, WorkbookProjectionBuilder,
    WorkbookProjectionError, WorkbookProjectionSource, blank, boolean, integer, minimum_constraint,
    project_game_state_to_workbook, text,
};
use crate::domain::{
    GameState, ShipProfile, ShipRoster, ShipSkill, ShipSkillIdentity, ShipSkillProgress,
    SkillEffectArgument, SkillEffectEvidence, SkillEffectEvidenceKey, SkillEffectParameterSource,
    SkillEffectParameters, SkillEffectSourceKind, SkillValue,
};

#[test]
fn inventory_projection_exposes_only_low_rarity_unenhanced_equipment_as_dismantlable() {
    let projection =
        project_game_state_to_workbook(&crate::application::test_support::plan_game_state())
            .unwrap();
    let inventory = projection.sheet("equipment_inventory").unwrap();
    let safe = inventory
        .rows()
        .iter()
        .find(|row| row.object_ref() == "warehouse:1000")
        .unwrap();
    let enhanced = inventory
        .rows()
        .iter()
        .find(|row| row.object_ref() == "warehouse:1001")
        .unwrap();

    assert_eq!(safe.value("locked"), Some(&boolean(false)));
    assert_eq!(safe.value("protected"), Some(&boolean(false)));
    assert_eq!(safe.value("dismantlable"), Some(&boolean(true)));
    assert_eq!(safe.value("data_complete"), Some(&boolean(true)));
    assert_eq!(
        safe.value("read_errors"),
        Some(&text("缺少装备配置原始记录"))
    );
    assert_eq!(safe.value("config_data_complete"), Some(&boolean(false)));
    assert_eq!(safe.value("processing_quantity"), Some(&blank()));
    assert_eq!(enhanced.value("protected"), Some(&boolean(true)));
    assert_eq!(enhanced.value("dismantlable"), Some(&boolean(false)));
}

#[test]
fn raw_json_chunking_respects_excel_utf16_boundaries() {
    let value = format!(
        "{}𐐷{}",
        "a".repeat(RAW_JSON_CHUNK_UTF16_UNITS - 2),
        "b".repeat(RAW_JSON_CHUNK_UTF16_UNITS)
    );

    let chunks = split_raw_json(&value);

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].encode_utf16().count(), RAW_JSON_CHUNK_UTF16_UNITS);
    assert_eq!(chunks[1].encode_utf16().count(), RAW_JSON_CHUNK_UTF16_UNITS);
    assert!(chunks[0].ends_with('𐐷'));
    assert_eq!(chunks.concat(), value);
}

#[test]
fn raw_json_chunks_cover_empty_and_exact_boundaries() {
    assert_eq!(split_raw_json(""), vec![String::new()]);
    let exact = "a".repeat(RAW_JSON_CHUNK_UTF16_UNITS);
    let chunks = split_raw_json(&exact);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], exact);
    let overflow = format!("{exact}b");
    let chunks = split_raw_json(&overflow);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0], exact);
    assert_eq!(chunks[1], "b");
    assert_eq!(chunks.concat(), overflow);
}

#[test]
fn raw_json_split_copies_a_large_input_once() {
    for chunk_count in [1_usize, 32, 128] {
        let value = "a".repeat(chunk_count * RAW_JSON_CHUNK_UTF16_UNITS);
        let started = std::time::Instant::now();
        let chunks = split_raw_json(&value);
        let split_us = started.elapsed().as_micros();
        let chunk_bytes: usize = chunks.iter().map(String::len).sum();
        let chunk_capacity: usize = chunks.iter().map(String::capacity).sum();
        assert_eq!(chunks.len(), chunk_count);
        assert_eq!(chunk_bytes, value.len());
        assert_eq!(chunks.concat(), value);
        assert!(
            chunk_capacity <= value.len().saturating_mul(2),
            "输入 {} 字节分成 {chunk_count} 块，容量总和 {chunk_capacity}",
            value.len()
        );
        println!(
            "MEASURE stage=raw_json_split chunks={chunk_count} source_bytes={} chunk_bytes={chunk_bytes} chunk_capacity={chunk_capacity} split_us={split_us} peak_bytes={}",
            value.len(),
            value.len() + chunk_capacity
        );
    }
}

#[test]
fn raw_json_chunk_capacity_stays_linear_for_bounded_ascii() {
    let chunk_count = 8;
    let value = "a".repeat(chunk_count * RAW_JSON_CHUNK_UTF16_UNITS);
    let chunks = split_raw_json(&value);
    let capacity: usize = chunks.iter().map(String::capacity).sum();
    assert_eq!(chunks.len(), chunk_count);
    assert_eq!(chunks.concat(), value);
    assert!(
        capacity <= value.len().saturating_mul(2),
        "输入 {} 字节分成 {} 块，容量总和 {capacity}",
        value.len(),
        chunks.len()
    );
}

#[test]
fn omitted_raw_sheet_skips_rows_and_hidden_keeps_them() {
    use super::project_game_state_for_layout;
    use crate::application::test_support::empty_game_state;
    use crate::application::{LayoutGenerationMode, WorkbookLayout, WorkbookSheetLayout};
    use crate::domain::{RawRecord, RawRecordKey, RawRecordSet};

    let base = empty_game_state();
    let state = GameState::new(
        base.source().clone(),
        base.ships().clone(),
        base.ship_catalog().clone(),
        base.equipment_catalog().clone(),
        base.equipment_details().clone(),
        base.equipment_inventory().clone(),
        base.bag().clone(),
        base.resources(),
        RawRecordSet::new(
            1,
            "1".repeat(64),
            vec![RawRecord::new(
                RawRecordKey::EquipmentReferenceNames,
                "a".repeat(64),
                Arc::<str>::from(r#"{"names":[]}"#),
            )],
        ),
    );
    let layout = |mode| {
        WorkbookLayout::new(
            1,
            "测试".into(),
            "原始表".into(),
            vec![WorkbookSheetLayout::new(
                "raw_data".into(),
                mode,
                "原始数据".into(),
                1,
                None,
                false,
                String::new(),
                false,
            )],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "0".repeat(64),
        )
        .unwrap()
    };
    let omitted =
        project_game_state_for_layout(&state, &layout(LayoutGenerationMode::Omitted)).unwrap();
    assert!(
        omitted
            .sheet("raw_data")
            .expect("注册表仍保留原始表")
            .rows()
            .is_empty()
    );
    let hidden =
        project_game_state_for_layout(&state, &layout(LayoutGenerationMode::Hidden)).unwrap();
    assert_eq!(
        hidden
            .sheet("raw_data")
            .expect("隐藏原始表应保留行")
            .rows()
            .len(),
        1
    );
    let visible =
        project_game_state_for_layout(&state, &layout(LayoutGenerationMode::Visible)).unwrap();
    assert_eq!(
        visible
            .sheet("raw_data")
            .expect("可见原始表应保留行")
            .rows()
            .len(),
        hidden.sheet("raw_data").unwrap().rows().len()
    );
}

#[test]
fn ship_skill_projection_exports_current_level_parameters_and_raw_evidence() {
    let raw_structure = r#"{"battle_buff":{"available":false},"battle_skill":{"available":true},"display":{"available":true}}"#;
    let evidence = SkillEffectEvidence::new(
        SkillEffectEvidenceKey::new(10_411, 1),
        vec![SkillEffectParameterSource::new(
            SkillEffectSourceKind::BattleSkill,
            vec![SkillEffectParameters::new(
                1,
                vec![SkillEffectArgument::new(
                    "ratio".to_owned(),
                    SkillValue::Number(0.25),
                )],
            )],
        )],
        Arc::from(raw_structure),
        true,
        Vec::new(),
    );
    let values = ship_skill_evidence_values(Some(&evidence), "ship-skill:9001:10410").unwrap();
    let expected_parameters = json!([{
        "source_kind": "battle_skill",
        "effect_list": [{
            "sequence": 1,
            "arg_list": [{
                "name": "ratio",
                "value": {"kind": "number", "value": 0.25},
            }],
        }],
    }]);

    assert_eq!(values.effect_parameters, expected_parameters);
    assert_eq!(
        values.raw_structure,
        serde_json::from_str::<serde_json::Value>(raw_structure).unwrap()
    );
    assert_eq!(values.read_errors, None);
    assert!(values.data_complete);
}

#[test]
fn ship_skill_projection_marks_missing_and_partial_evidence() {
    let missing = ship_skill_evidence_values(None, "ship-skill:9001:10410").unwrap();
    assert_eq!(missing.effect_parameters, serde_json::Value::Null);
    assert_eq!(missing.raw_structure, serde_json::Value::Null);
    assert_eq!(
        missing.read_errors.as_deref(),
        Some(SHIP_SKILL_EVIDENCE_MISSING)
    );
    assert!(!missing.data_complete);

    let evidence = SkillEffectEvidence::new(
        SkillEffectEvidenceKey::new(10_411, 1),
        Vec::new(),
        Arc::from(r#"{"battle_skill":null}"#),
        false,
        vec![
            "battle_skill.error: missing".to_owned(),
            "battle_buff.read_error: truncated".to_owned(),
        ],
    );
    let partial = ship_skill_evidence_values(Some(&evidence), "ship-skill:9001:10410").unwrap();

    assert_eq!(partial.effect_parameters, json!([]));
    assert_eq!(partial.raw_structure, json!({"battle_skill": null}));
    assert_eq!(
        partial.read_errors.as_deref(),
        Some("battle_skill.error: missing | battle_buff.read_error: truncated")
    );
    assert!(!partial.data_complete);
}

#[test]
fn ship_skill_columns_preserve_zero_and_variable_length_skill_lists() {
    let empty_projection =
        project_game_state_to_workbook(&crate::application::test_support::plan_game_state())
            .unwrap();
    let empty_ship = &empty_projection.sheet("loadout_plan").unwrap().rows()[0];
    assert_eq!(empty_ship.value("skills_name"), Some(&blank()));
    assert_eq!(
        empty_ship.value("skills_effect_parameters"),
        Some(&super::ProjectionValue::Json("[]".to_owned()))
    );

    let state = plan_game_state_with_skills(7);
    let projection = project_game_state_to_workbook(&state).unwrap();
    let ship = &projection.sheet("loadout_plan").unwrap().rows()[0];
    let names = match ship.value("skills_name").unwrap() {
        super::ProjectionValue::Text(value) => value,
        other => panic!("技能名称应按行聚合，实际为 {other:?}"),
    };
    assert_eq!(names.lines().count(), 7);
    assert!(names.contains("[1006] 技能7"));
    let parameters = match ship.value("skills_effect_parameters").unwrap() {
        super::ProjectionValue::Json(value) => {
            serde_json::from_str::<serde_json::Value>(value).unwrap()
        }
        other => panic!("技能参数应为关联 JSON 数组，实际为 {other:?}"),
    };
    assert_eq!(parameters.as_array().unwrap().len(), 7);
    assert_eq!(parameters[6]["skill_id"], "1006");
    assert!(parameters[6]["value"].is_null());
}

fn plan_game_state_with_skills(count: u64) -> GameState {
    let state = crate::application::test_support::plan_game_state();
    let original = &state.ships().ships()[0];
    let skills = (0..count)
        .map(|index| {
            ShipSkill::new(
                ShipSkillIdentity::new(1_000 + index, 2_000 + index, format!("技能{}", index + 1)),
                ShipSkillProgress::new(1, 10, index, 100 + index),
                format!("描述{}", index + 1),
                format!("效果{}", index + 1),
            )
        })
        .collect();
    let ship = ShipProfile::new(
        original.identity().clone(),
        original.growth(),
        original.intimacy().clone(),
        original.fleet_memberships().to_vec(),
        original.classification().clone(),
        original.performance().clone(),
        skills,
        original.slots().clone(),
    );
    GameState::new(
        state.source().clone(),
        ShipRoster::new(state.ships().source().clone(), vec![ship]),
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    )
}

#[test]
fn client_compose_limit_can_only_tighten_resource_constraints() {
    assert_eq!(minimum_constraint(Some(3), Some(5)), Some(3));
    assert_eq!(minimum_constraint(None, Some(5)), Some(5));
    assert_eq!(minimum_constraint(Some(3), None), Some(3));
}

#[test]
fn compose_projection_reads_the_client_limit_from_the_recipe_item() {
    let state =
        crate::application::test_support::plan_game_state_with_compose(1_000, 100, 3, 300, Some(2));
    let projection = project_game_state_to_workbook(&state).unwrap();
    let equipment_inventory = projection.sheet("equipment_inventory").unwrap();
    let equipment = equipment_inventory
        .rows()
        .iter()
        .find(|row| row.object_ref() == "warehouse:1000")
        .unwrap();
    let resource_recipes = projection.sheet("resource_recipes").unwrap();
    let compose_output = resource_recipes
        .rows()
        .iter()
        .find(|row| row.object_ref() == "compose:5001:output:equipment")
        .unwrap();

    assert_eq!(equipment.value("craftable_actual"), Some(&integer(2)));
    assert_eq!(compose_output.value("maximum_craftable"), Some(&integer(2)));
}

#[test]
fn rejects_rows_that_do_not_cover_the_registered_contract() {
    let mut builder = WorkbookProjectionBuilder::new(fixture_source()).unwrap();

    let error = builder
        .push_row(
            "raw_data",
            "equipment_config:1000:chunk:1",
            [("entity_type".to_owned(), text("equipment_config"))],
        )
        .unwrap_err();

    assert!(matches!(
        error,
        WorkbookProjectionError::MissingFields { ref field_keys, .. }
            if field_keys.contains(&"entity_id".to_owned())
                && field_keys.contains(&"canonical_json_chunk".to_owned())
    ));
}

fn fixture_source() -> WorkbookProjectionSource {
    let digest = "0".repeat(64);
    WorkbookProjectionSource::new(
        1,
        digest.clone(),
        1,
        1,
        1,
        1,
        1,
        digest.clone(),
        digest.clone(),
        digest.clone(),
        digest.clone(),
        digest.clone(),
        digest,
    )
}

#[test]
fn equipment_availability_preserves_owned_levels_and_filters_unowned_levels() {
    let state = crate::application::test_support::plan_game_state();
    let projection = project_game_state_to_workbook(&state).unwrap();
    let refs: Vec<_> = projection
        .sheet("equipment_inventory")
        .unwrap()
        .rows()
        .iter()
        .map(|row| row.object_ref())
        .collect();
    assert_eq!(
        refs,
        [
            "ship:9001:1",
            "ship:9001:4",
            "warehouse:1000",
            "warehouse:1001"
        ]
    );
    let unowned = unowned_equipment_state(&state, None);
    let projection = project_game_state_to_workbook(&unowned).unwrap();
    let rows = projection.sheet("equipment_inventory").unwrap().rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].object_ref(), "unowned:1000");
    assert_eq!(rows[0].value("quantity"), Some(&integer(0)));
    assert_eq!(rows[0].value("source_type"), Some(&text("unowned")));
    assert!(
        projection
            .directly_composable_equipment_configs()
            .is_empty()
    );
}

#[test]
fn equipment_availability_uses_exact_output_and_all_compose_constraints() {
    for (gold, materials, client_max, output, expected) in [
        (1000, 20, Some(4), 1000, true),
        (99, 20, Some(4), 1000, false),
        (1000, 4, Some(4), 1000, false),
        (1000, 20, Some(0), 1000, false),
        (1000, 20, Some(4), 1001, false),
    ] {
        let state = crate::application::test_support::plan_game_state_with_compose(
            gold, materials, 3, 300, client_max,
        );
        let state = unowned_equipment_state(&state, Some(output));
        let projection = project_game_state_to_workbook(&state).unwrap();
        let row = &projection.sheet("equipment_inventory").unwrap().rows()[0];
        assert_eq!(row.object_ref(), "unowned:1000");
        assert_eq!(
            row.is_directly_composable_equipment(
                &projection.directly_composable_equipment_configs()
            ),
            expected
        );
        if output == 1001 {
            assert_eq!(
                row.value("craftable_actual"),
                Some(&integer(4)),
                "族合成量保留原有语义"
            );
        }
    }
}

fn unowned_equipment_state(state: &GameState, output: Option<u64>) -> GameState {
    use crate::domain::{
        EquipmentCatalog, EquipmentComposeRecipe, EquipmentConfigId, EquipmentInventory,
    };
    let catalog = state.equipment_catalog();
    let recipes = catalog
        .recipes()
        .iter()
        .map(|recipe| {
            EquipmentComposeRecipe::new(
                recipe.recipe_id(),
                recipe.material(),
                recipe.gold(),
                EquipmentConfigId::new(output.unwrap()).unwrap(),
            )
        })
        .collect();
    GameState::new(
        state.source().clone(),
        ShipRoster::new(state.ships().source().clone(), Vec::new()),
        state.ship_catalog().clone(),
        EquipmentCatalog::new(
            catalog.source().clone(),
            catalog.families().to_vec(),
            recipes,
            catalog
                .families()
                .iter()
                .map(|family| family.configs().len())
                .sum(),
        ),
        state.equipment_details().clone(),
        EquipmentInventory::new(Vec::new()),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    )
}

#[test]
fn equipment_availability_orders_groups_and_keeps_reference_order_within_groups() {
    let state =
        crate::application::test_support::plan_game_state_with_compose(1000, 20, 3, 300, Some(4));
    let projection = project_game_state_to_workbook(&state).unwrap();
    let template = &projection.sheet("equipment_inventory").unwrap().rows()[0];
    let mut builder = WorkbookProjectionBuilder::new(fixture_source()).unwrap();
    for (reference, source, config, distribution) in [
        ("unowned:z", "unowned", "9999", Some("")),
        ("unowned:b", "unowned", "1000", None),
        ("warehouse:b", "warehouse", "1000", None),
        ("unowned:a", "unowned", "1000", None),
        ("unowned:c", "unowned", "9999", Some("")),
        ("unowned:d", "unowned", "9998", Some("+10:1")),
        ("ship:a", "ship", "1001", None),
    ] {
        let mut values = template.values().clone();
        values.insert("source_ref".to_owned(), text(reference));
        values.insert("source_type".to_owned(), text(source));
        values.insert("config_id".to_owned(), text(config));
        if let Some(distribution) = distribution {
            values.insert(
                "family_owned_enhance_distribution".to_owned(),
                text(distribution),
            );
        }
        builder
            .push_row("equipment_inventory", reference, values)
            .unwrap();
    }
    for row in projection.sheet("resource_recipes").unwrap().rows() {
        builder
            .push_row("resource_recipes", row.object_ref(), row.values().clone())
            .unwrap();
    }
    let projection = builder.finish().unwrap();
    let refs: Vec<_> = projection
        .sheet("equipment_inventory")
        .unwrap()
        .rows()
        .iter()
        .map(|row| row.object_ref())
        .collect();
    assert_eq!(
        refs,
        [
            "ship:a",
            "warehouse:b",
            "unowned:a",
            "unowned:b",
            "unowned:d",
            "unowned:c",
            "unowned:z"
        ]
    );
}

#[test]
fn ship_level_cap_is_displayed_only_for_owned_instances() {
    let state = crate::application::test_support::plan_game_state();
    let owned = project_game_state_to_workbook(&state).unwrap();
    for ship in state.ships().ships() {
        let row = owned
            .sheet("loadout_plan")
            .unwrap()
            .rows()
            .iter()
            .find(|row| row.value("instance_id") == Some(&text(ship.identity().instance_id())))
            .unwrap();
        assert_eq!(
            row.value("maximum_level"),
            Some(&integer(i64::from(ship.growth().max_level())))
        );
    }
    let unowned = GameState::new(
        state.source().clone(),
        ShipRoster::new(state.ships().source().clone(), Vec::new()),
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );
    let projection = project_game_state_to_workbook(&unowned).unwrap();
    let rows = projection.sheet("loadout_plan").unwrap().rows();
    assert!(!rows.is_empty());
    for row in rows {
        assert_eq!(row.value("source_type"), Some(&text("unowned")));
        assert_eq!(row.value("maximum_level"), Some(&blank()));
        assert_ne!(row.value("name"), Some(&blank()));
        assert_ne!(row.value("maximum_stars"), Some(&blank()));
    }
}

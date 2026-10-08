//! 从仓库内独立 JSON 样本回放完整状态，并逐字段核对领域投影。

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{GameStateMappingError, map_game_state_borrowed_with_scope};
use crate::adapters::device::mapping::equipment::map_equipment_catalog;
use crate::adapters::device::reading::equipment::{EquipmentRawRecords, EquipmentReadResult};
use crate::adapters::device::reading::ship_catalog::{ShipCatalogReadResult, ShipCatalogTable};
use crate::adapters::device::runtime::{
    ComposeRecipePageResult, EquipmentConfigPageResult, EquipmentReferenceNameBatchResult,
    RuntimeEquipmentWeaponDetail, RuntimeSkillEffectDetail, RuntimeSkillEffectSource,
    ShipCatalogRecord, ShipCatalogTableKey, SnapshotOwnedStateResult, SnapshotShipDetailsResult,
};
use crate::application::{
    WorkbookProjectionRow, WorkbookProjectionV4, WorkbookProjectionValue as ProjectionValue,
    project_game_state_to_workbook,
};
use crate::domain::{
    BagComposeAvailability, BagItem, EquipmentCatalog, EquipmentComposeRecipe, EquipmentDefinition,
    EquipmentDetailCatalog, EquipmentFamily, EquipmentItemQuantity, EquipmentResources,
    EquipmentSkillDetail, EquipmentSkillEffect, EquipmentSkillSource, EquipmentSkillVisibility,
    EquipmentWeapon, GameState, RawRecord, RawRecordKey, ShipAttributeValues, ShipProfile,
    ShipRoster, SkillEffectArgument, SkillEffectEvidence, SkillEffectEvidenceCatalog,
    SkillEffectEvidenceKey, SkillEffectParameterSource, SkillEffectParameters,
    SkillEffectSourceKind, SkillTableKey, SkillValue, WarehouseEquipmentStack,
    WeaponChargeParameter, WeaponPrecastParameter,
};
use suzushiro_content_digest::sha256_sorted_json;

const INPUT: &str = include_str!("../../../../../tests/fixtures/game_state/full_state_input.json");
const EXPECTED: &str =
    include_str!("../../../../../tests/fixtures/game_state/full_state_expected.json");

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FullStateFixture {
    fixture_schema_version: u32,
    module_sha256: String,
    owned_state_before: SnapshotOwnedStateResult,
    owned_state_after: SnapshotOwnedStateResult,
    ship_details: SnapshotShipDetailsResult,
    equipment: EquipmentFixture,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquipmentFixture {
    config_page_size: u32,
    recipe_page_size: u32,
    config_pages: Vec<EquipmentConfigPageResult>,
    recipe_pages: Vec<ComposeRecipePageResult>,
    reference_names: EquipmentReferenceNameBatchResult,
    weapons: Vec<RuntimeEquipmentWeaponDetail>,
    skills: Vec<RuntimeSkillEffectDetail>,
}

/// 为同 crate 的工作簿和应用用例测试提供一份经过完整映射的固定状态。
pub(crate) fn golden_game_state() -> GameState {
    let fixture = golden_replay_fixture();
    let equipment = fixture_equipment(&fixture);
    let ship_skill_effects = golden_ship_skill_effects();
    let ship_catalog = fixture_ship_catalog(&fixture);
    map_game_state_borrowed_with_scope(
        &fixture.owned_state_before,
        &fixture.ship_details,
        &fixture.owned_state_after,
        &equipment,
        &ship_catalog,
        &ship_skill_effects,
        &fixture.module_sha256,
        crate::domain::GameReadScope::full(),
    )
    .expect("黄金输入应能映射为完整游戏状态")
}

/// 为工作簿零库存行测试保留未持有的 0 级配置与实际持有的强化配置。
pub(crate) fn golden_game_state_with_unowned_config() -> GameState {
    golden_game_state_with_unowned_materials(0)
}

pub(crate) fn golden_game_state_with_unowned_materials(material_quantity: u64) -> GameState {
    let fixture = unowned_replay_fixture(material_quantity);
    let equipment = fixture_equipment(&fixture);
    let ship_skill_effects = golden_ship_skill_effects();
    let ship_catalog = fixture_ship_catalog(&fixture);
    map_game_state_borrowed_with_scope(
        &fixture.owned_state_before,
        &fixture.ship_details,
        &fixture.owned_state_after,
        &equipment,
        &ship_catalog,
        &ship_skill_effects,
        &fixture.module_sha256,
        crate::domain::GameReadScope::full(),
    )
    .expect("未持有配置测试输入应能映射为完整游戏状态")
}

fn unowned_replay_fixture(material_quantity: u64) -> FullStateFixture {
    let mut fixture = golden_replay_fixture();
    fixture.owned_state_before.warehouse.count = 0;
    fixture.owned_state_before.warehouse.items.clear();
    fixture.owned_state_before.player.equipment_capacity = 0;
    fixture.owned_state_after.warehouse.count = 0;
    fixture.owned_state_after.warehouse.items.clear();
    fixture.owned_state_after.player.equipment_capacity = 0;
    for owned in [
        &mut fixture.owned_state_before,
        &mut fixture.owned_state_after,
    ] {
        let equipment = owned.dock.ships[0].slots[0].equipment.as_mut().unwrap();
        equipment.config_id = 1001;
        equipment.enhance_level = 1;
        owned.bag.items[0].quantity = material_quantity;
    }
    fixture
}

fn golden_replay_fixture() -> FullStateFixture {
    let mut fixture = load_fixture();
    fixture.ship_details.ships[0].base_attributes.cannon = 95.97452000000001;
    fixture
}

fn golden_ship_skill_effects() -> Vec<RuntimeSkillEffectDetail> {
    let complete_source = |value| RuntimeSkillEffectSource {
        available: true,
        complete: true,
        value,
        error: None,
        read_errors: Vec::new(),
    };
    vec![RuntimeSkillEffectDetail {
        skill_id: 10_411,
        level: 1,
        display: complete_source(json!({
            "id": 10_411,
            "name": "舰船测试技能",
            "desc": "技能描述模板",
            "desc_get": "",
            "system_transform": [],
        })),
        battle_skill: complete_source(json!({
            "lua_type": "table",
            "entries": [
                {
                    "key": 2,
                    "key_type": "number",
                    "lua_type": null,
                    "value": {"desc": "其他等级"},
                },
                {
                    "key": "effect_list",
                    "key_type": "string",
                    "lua_type": null,
                    "value": [],
                },
            ],
            "truncated": false,
            "reason": null,
        })),
        battle_buff: complete_source(json!({"effect_list": []})),
        complete: true,
    }]
}

/// 提供以公式触发字符开头的外部文本，验证工作簿只按普通字符串写入。
pub(crate) fn formula_like_text_game_state() -> GameState {
    named_ship_game_state("=1+1", false)
}

/// 为舰船名称与图鉴链接测试提供实例名称和誓约状态。
pub(crate) fn named_ship_game_state(name: &str, proposed: bool) -> GameState {
    let mut fixture = load_fixture();
    fixture.ship_details.ships[0].name = name.to_owned();
    fixture.ship_details.ships[0].proposed = proposed;
    map_fixture(&fixture)
}

#[test]
fn replays_full_state_fixture_against_independent_field_projection() {
    let fixture = load_fixture();
    let state = map_fixture(&fixture);
    let expected: Value = serde_json::from_str(EXPECTED).expect("黄金期望应为有效 JSON");
    let actual = json!({
        "fixture_schema_version": fixture.fixture_schema_version,
        "game_state": project_game_state(&state),
    });
    if std::env::var_os("AZLW_UPDATE_GAME_STATE_GOLDEN").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/game_state/full_state_expected.json");
        let mut bytes = serde_json::to_vec_pretty(&actual).expect("黄金期望必须可编码");
        bytes.push(b'\n');
        fs::write(path, bytes).expect("黄金期望必须可写入");
        return;
    }
    assert_eq!(fixture.fixture_schema_version, 2);
    assert_eq!(actual, expected);
}

#[test]
fn mapped_field_change_invalidates_the_frozen_projection_and_digest() {
    let fixture = load_fixture();
    let original = map_fixture(&fixture);
    let original_projection = project_game_state(&original);
    let mut changed_fixture = fixture.clone();
    changed_fixture.owned_state_before.player.gold += 1;
    changed_fixture.owned_state_after.player.gold += 1;
    let changed = map_fixture(&changed_fixture);

    assert_ne!(changed, original);
    assert_ne!(project_game_state(&changed), original_projection);
    assert_ne!(
        changed.source().owned_state_content_sha256(),
        original.source().owned_state_content_sha256()
    );
    assert_ne!(
        changed.source().content_sha256(),
        original.source().content_sha256()
    );
}

#[test]
fn rejects_state_change_between_frozen_snapshots() {
    let mut fixture = load_fixture();
    fixture.owned_state_after.player.gold += 1;

    let error = try_map_fixture(&fixture).expect_err("前后持有状态不一致时应拒绝完整状态");

    assert!(matches!(
        error,
        GameStateMappingError::OwnedStateChanged { .. }
    ));
}

#[test]
fn projects_full_state_fixture_without_omitting_registered_fields() {
    let state = map_fixture(&load_fixture());
    let projection = project_game_state_to_workbook(&state).unwrap();

    assert_eq!(projection.schema_version(), 19);
    assert_eq!(projection.sheets().len(), 10);
    for (sheet_key, expected_rows) in [
        ("check_results", 0),
        ("dictionaries", 12),
        ("equipment_inventory", 2),
        ("execution_results", 0),
        ("loadout_plan", 1),
        ("plan_data", 0),
        ("raw_data", 12),
        ("resource_recipes", 20),
        ("schema", 0),
    ] {
        let sheet = projection
            .sheet(sheet_key)
            .expect("注册表必须生成全部工作表");
        assert_eq!(sheet.rows().len(), expected_rows, "{sheet_key} 行数不符");
        for row in sheet.rows() {
            assert_eq!(
                row.values().keys().collect::<Vec<_>>(),
                sheet.field_keys().iter().collect::<Vec<_>>(),
                "{sheet_key} 的每一行都必须完整覆盖注册字段"
            );
        }
    }

    let source = projection.source();
    assert_eq!(source.game_state_schema_version(), state.schema_version());
    assert_eq!(source.module_sha256(), state.source().module_sha256());
    assert_eq!(
        source.game_state_content_sha256(),
        state.source().content_sha256()
    );
    assert_eq!(
        source.raw_records_content_sha256(),
        state.source().raw_records_content_sha256()
    );
    assert_eq!(projection, project_game_state_to_workbook(&state).unwrap());
    assert_eq!(projection.registry_sha256().len(), 64);
    assert_eq!(projection.content_sha256().len(), 64);
    assert!(
        projection
            .content_sha256()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn projects_ship_skill_evidence_by_effective_skill_and_current_level() {
    let state = map_fixture(&load_fixture());
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
    let roster = ShipRoster::new_with_skill_effects(
        state.ships().source().clone(),
        state.ships().ships().to_vec(),
        SkillEffectEvidenceCatalog::new(vec![evidence]),
    );
    let state = GameState::new(
        state.source().clone(),
        roster,
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );

    let projection = project_game_state_to_workbook(&state).unwrap();
    let ship = projection_row_with_texts(&projection, "loadout_plan", &[("instance_id", "9001")]);
    let expected_parameters = serde_json::to_string(&json!([{
        "skill_id": "10410",
        "value": [{
            "source_kind": "battle_skill",
            "effect_list": [{
                "sequence": 1,
                "arg_list": [{
                    "name": "ratio",
                    "value": {"kind": "number", "value": 0.25},
                }],
            }],
        }],
    }]))
    .unwrap();
    let expected_raw = serde_json::to_string(&json!([{
        "skill_id": "10410",
        "value": serde_json::from_str::<serde_json::Value>(raw_structure).unwrap(),
    }]))
    .unwrap();

    assert_projection_value(
        ship,
        "skills_effect_parameters",
        ProjectionValue::Json(expected_parameters),
    );
    assert_projection_value(
        ship,
        "skills_raw_structure",
        ProjectionValue::Json(expected_raw),
    );
    assert_projection_value(
        ship,
        "skills_read_errors",
        ProjectionValue::text("[10410] （无）"),
    );
    assert_projection_value(
        ship,
        "skills_data_complete",
        ProjectionValue::text("[10410] 是"),
    );
}

#[test]
fn maps_snapshot_values_and_preserves_safe_empty_inputs() {
    let state = map_fixture(&load_fixture());
    let projection = project_game_state_to_workbook(&state).unwrap();

    let ship = projection_row_with_texts(&projection, "loadout_plan", &[("instance_id", "9001")]);
    assert_projection_value(
        ship,
        "skills_name",
        ProjectionValue::text("[10410] 舰船测试技能"),
    );
    assert_projection_value(
        ship,
        "skills_effective_skill_id",
        ProjectionValue::text("[10410] 10411"),
    );
    assert_projection_value(ship, "slot_1_config_id", ProjectionValue::text("1000"));
    assert_projection_value(
        ship,
        "slot_1_allowed_equipment_types",
        ProjectionValue::text("10:主炮"),
    );
    assert_projection_value(
        ship,
        "slot_1_target_equipment_family",
        ProjectionValue::Blank,
    );
    assert_projection_value(ship, "slot_1_note", ProjectionValue::Blank);

    let warehouse = projection_row_with_texts(
        &projection,
        "equipment_inventory",
        &[("source_ref", "warehouse:1001")],
    );
    assert_projection_value(
        warehouse,
        "family_warehouse_quantity",
        ProjectionValue::Integer(2),
    );
    assert_projection_value(
        warehouse,
        "family_equipped_quantity",
        ProjectionValue::Integer(1),
    );
    assert_projection_value(
        warehouse,
        "family_owned_quantity",
        ProjectionValue::Integer(3),
    );
    assert_projection_value(
        warehouse,
        "craftable_by_materials",
        ProjectionValue::Integer(6),
    );
    assert_projection_value(warehouse, "craftable_actual", ProjectionValue::Integer(6));
    assert_projection_value(
        warehouse,
        "family_potential_quantity",
        ProjectionValue::Integer(9),
    );
    assert_projection_value(warehouse, "planned_required_count", ProjectionValue::Blank);
    assert_projection_value(warehouse, "quantity", ProjectionValue::Integer(2));
    assert_projection_value(warehouse, "locked", ProjectionValue::Boolean(false));
    assert_projection_value(warehouse, "protected", ProjectionValue::Boolean(true));
    assert_projection_value(warehouse, "dismantlable", ProjectionValue::Boolean(false));
    assert_projection_value(warehouse, "data_complete", ProjectionValue::Boolean(true));
    assert_projection_value(warehouse, "read_errors", ProjectionValue::Blank);
    assert_projection_value(warehouse, "operation", ProjectionValue::Blank);

    assert_projection_value(ship, "intimacy", ProjectionValue::Decimal(100.0));
    assert_projection_value(
        ship,
        "slot_2_allowed_equipment_types",
        ProjectionValue::text("5，10:主炮"),
    );
    assert_projection_value(
        ship,
        "fleet_status",
        ProjectionValue::text("第一舰队（常规）·先锋1；演习舰队（演习）·先锋2"),
    );
    assert_projection_value(ship, "intimacy_stage", ProjectionValue::text("爱"));
    assert_projection_value(ship, "read_errors", ProjectionValue::Blank);
    assert_projection_value(ship, "slot_1_config_id", ProjectionValue::text("1000"));
    assert_projection_value(ship, "slot_2_config_id", ProjectionValue::Blank);
    assert_projection_value(ship, "data_complete", ProjectionValue::Boolean(true));

    assert_projection_value(
        ship,
        "skills_current_effect",
        ProjectionValue::text("[10410] 当前效果"),
    );
    assert_projection_value(
        ship,
        "skills_effect_parameters",
        ProjectionValue::Json(
            r#"[{"skill_id":"10410","value":[{"effect_list":[],"source_kind":"battle_buff"}]}]"#
                .to_owned(),
        ),
    );
    assert_projection_value(
        ship,
        "skills_raw_structure",
        ProjectionValue::Json(
            r#"[{"skill_id":"10410","value":{"battle_buff":{"available":true,"complete":true,"error":null,"read_errors":[],"value":{"effect_list":[]}},"battle_skill":{"available":true,"complete":true,"error":null,"read_errors":[],"value":{"entries":[{"key":2,"key_type":"number","lua_type":null,"value":{"desc":"其他等级"}},{"key":"effect_list","key_type":"string","lua_type":null,"value":[]}],"lua_type":"table","reason":null,"truncated":false}},"complete":true,"display":{"available":true,"complete":true,"error":null,"read_errors":[],"value":{"desc":"技能描述模板","desc_get":"","id":10411,"name":"舰船测试技能","system_transform":[]}},"level":1,"skill_id":10411}}]"#
                .to_owned(),
        ),
    );
    assert_projection_value(
        ship,
        "skills_read_errors",
        ProjectionValue::text(
            "[10410] battle_skill.normalize: skill_id=10411, level=1 的 battle_skill 无效: 混合表缺少等级 1 的数值键",
        ),
    );
    assert_projection_value(
        ship,
        "skills_data_complete",
        ProjectionValue::text("[10410] 否"),
    );

    let equipped =
        projection_row_with_texts(&projection, "equipment_inventory", &[("config_id", "1000")]);
    assert_projection_value(
        equipped,
        "effect_summary",
        ProjectionValue::text(
            "属性：炮击 +12.5，装填 +4.5\n武器：3680:伤害5 装填405 射程70\n主技能：[10] 装备测试技能 Lv1\n隐藏技能：",
        ),
    );
    let attributes = projection_json(equipped, "attributes_json");
    assert_eq!(attributes[0]["key"], "cannon");
    assert_eq!(attributes[0]["value"], 12.5);
    let weapons = projection_json(equipped, "weapons_json");
    assert_eq!(weapons[0]["weapon_id"], 3680);
    let next_cost = projection_json(equipped, "next_cost_json");
    assert_eq!(next_cost["items"][0]["item_id"], 17001);
    let effects = projection_json(equipped, "skill_effects_json");
    assert_eq!(effects[0]["skill_visibility"], "visible");
    assert_eq!(effects[0]["raw_data_ref"], "equipment_skill:10:1");
    let effect = effects[0]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|source| source["effects"].as_array().unwrap())
        .find(|effect| effect["effect_type"] == "BattleBuffCastSkill")
        .unwrap();
    assert_eq!(effect["effect_sequence"], 1);
    assert_eq!(effect["arguments"].as_array().unwrap().len(), 4);
}

#[test]
fn keeps_visible_and_hidden_references_to_the_same_equipment_skill_distinct() {
    let mut fixture = load_fixture();
    fixture.equipment.config_pages[0].configs[0]
        .raw_config
        .as_object_mut()
        .unwrap()
        .insert("hidden_skill_id".to_owned(), json!([[10, 1]]));
    let state = map_fixture(&fixture);

    let projection = project_game_state_to_workbook(&state).unwrap();
    let effects = equipment_skill_effects(&projection, "1000");
    let visible = effects
        .iter()
        .filter(|effect| effect["skill_id"] == 10 && effect["skill_visibility"] == "visible")
        .count();
    let hidden = effects
        .iter()
        .filter(|effect| effect["skill_id"] == 10 && effect["skill_visibility"] == "hidden")
        .count();
    assert_eq!(visible, 1);
    assert_eq!(hidden, 1);
    let row =
        projection_row_with_texts(&projection, "equipment_inventory", &[("config_id", "1000")]);
    assert_projection_value(
        row,
        "effect_summary",
        ProjectionValue::text(
            "属性：炮击 +12.5，装填 +4.5\n武器：3680:伤害5 装填405 射程70\n主技能：[10] 装备测试技能 Lv1\n隐藏技能：[10] 装备测试技能 Lv1",
        ),
    );
}

#[test]
fn preserves_repeated_equipment_skill_references_by_source_ordinal() {
    let baseline = project_game_state_to_workbook(&map_fixture(&load_fixture())).unwrap();
    let baseline_count = equipment_skill_effects(&baseline, "1000").len();
    let mut fixture = load_fixture();
    fixture.equipment.config_pages[0].configs[0]
        .raw_config
        .as_object_mut()
        .unwrap()
        .insert("skill_id".to_owned(), json!([[10, 1], [10, 1]]));
    let state = map_fixture(&fixture);

    let projection = project_game_state_to_workbook(&state).unwrap();
    let effects = equipment_skill_effects(&projection, "1000");
    let repeated_count = effects.len();
    let ordinals: Vec<u64> = effects
        .iter()
        .map(|effect| effect["reference_ordinal"].as_u64().unwrap())
        .collect();

    assert_eq!(baseline_count, 1);
    assert_eq!(repeated_count, baseline_count * 2);
    assert_eq!(ordinals, [1, 2]);
}

#[test]
fn projects_unowned_zero_level_config_once_with_complete_configuration_details() {
    let projection =
        project_game_state_to_workbook(&golden_game_state_with_unowned_config()).unwrap();
    let row = projection_row_with_texts(
        &projection,
        "equipment_inventory",
        &[("source_ref", "unowned:1000"), ("source_type", "unowned")],
    );

    assert!(
        !projection
            .sheet("equipment_inventory")
            .unwrap()
            .rows()
            .iter()
            .any(|row| row.object_ref() == "unowned:1001")
    );
    assert_projection_value(row, "quantity", ProjectionValue::Integer(0));
    assert_projection_value(row, "runtime_id", ProjectionValue::Blank);
    assert_projection_value(row, "ship_instance_id", ProjectionValue::Blank);
    assert_projection_value(row, "slot_index", ProjectionValue::Blank);
    assert_projection_value(row, "operation", ProjectionValue::Blank);
    assert!(projection_json(row, "attributes_json").as_array().is_some());
    assert!(projection_json(row, "weapons_json").as_array().is_some());
    assert!(
        projection_json(row, "skill_effects_json")
            .as_array()
            .is_some()
    );
    assert_eq!(
        projection
            .sheet("equipment_inventory")
            .unwrap()
            .rows()
            .iter()
            .filter(|candidate| candidate.value("config_id") == Some(&ProjectionValue::text("1000")))
            .count(),
        1
    );
}

#[test]
fn groups_warehouse_quantity_and_keeps_each_equipped_slot_independent() {
    let mut fixture = load_fixture();
    let mut warehouse_base = fixture.owned_state_before.warehouse.items[0].clone();
    warehouse_base.equipment_id = 7000;
    warehouse_base.config_id = 1000;
    warehouse_base.enhance_level = 0;
    warehouse_base.quantity = 10;
    fixture
        .owned_state_before
        .warehouse
        .items
        .push(warehouse_base.clone());
    fixture
        .owned_state_before
        .warehouse
        .items
        .sort_by_key(|item| item.equipment_id);
    fixture.owned_state_before.warehouse.count = 2;
    fixture.owned_state_before.player.equipment_capacity = 12;
    fixture
        .owned_state_after
        .warehouse
        .items
        .push(warehouse_base);
    fixture
        .owned_state_after
        .warehouse
        .items
        .sort_by_key(|item| item.equipment_id);
    fixture.owned_state_after.warehouse.count = 2;
    fixture.owned_state_after.player.equipment_capacity = 12;

    let mut second_slot = fixture.owned_state_before.dock.ships[0].slots[0]
        .equipment
        .clone()
        .unwrap();
    second_slot.equipment_id = 8002;
    fixture.owned_state_before.dock.ships[0].slots[1].equipment = Some(second_slot.clone());
    fixture.owned_state_after.dock.ships[0].slots[1].equipment = Some(second_slot);

    let projection = project_game_state_to_workbook(&map_fixture(&fixture)).unwrap();
    let rows = projection.sheet("equipment_inventory").unwrap().rows();

    assert_eq!(
        rows.iter()
            .filter(|row| row.object_ref() == "warehouse:1000")
            .count(),
        1
    );
    assert_projection_value(
        rows.iter()
            .find(|row| row.object_ref() == "warehouse:1000")
            .unwrap(),
        "quantity",
        ProjectionValue::Integer(10),
    );
    for source_ref in ["ship:9001:1", "ship:9001:2"] {
        let row = rows
            .iter()
            .find(|row| row.object_ref() == source_ref)
            .unwrap();
        assert_projection_value(row, "config_id", ProjectionValue::text("1000"));
        assert_projection_value(row, "quantity", ProjectionValue::Integer(1));
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row.value("config_id") == Some(&ProjectionValue::text("1001")))
            .count(),
        1,
        "不同强化配置必须保持独立行且已有来源时不能追加零库存行"
    );
    assert!(
        rows.iter()
            .all(|row| !row.object_ref().starts_with("unowned:"))
    );
}

#[test]
fn normalizes_resource_inputs_outputs_and_constraints() {
    let state = map_fixture(&load_fixture());
    let projection = project_game_state_to_workbook(&state).unwrap();

    let compose_output = projection_row_with_texts(
        &projection,
        "resource_recipes",
        &[("recipe_id", "17001"), ("resource_type", "equipment")],
    );
    assert_projection_value(compose_output, "resource_id", ProjectionValue::text("1000"));
    assert_projection_value(
        compose_output,
        "result_quantity",
        ProjectionValue::Integer(1),
    );
    assert_projection_value(compose_output, "required_quantity", ProjectionValue::Blank);
    assert_projection_value(
        compose_output,
        "maximum_craftable",
        ProjectionValue::Integer(6),
    );

    let compose_gold = projection_row_with_texts(
        &projection,
        "resource_recipes",
        &[("recipe_id", "17001"), ("resource_type", "gold")],
    );
    assert_projection_value(
        compose_gold,
        "required_quantity",
        ProjectionValue::Integer(100),
    );
    assert_projection_value(
        compose_gold,
        "available_quantity",
        ProjectionValue::Integer(12_345),
    );
    assert_projection_value(
        compose_gold,
        "maximum_craftable",
        ProjectionValue::Integer(123),
    );

    let enhance_item = projection_row_with_texts(
        &projection,
        "resource_recipes",
        &[("recipe_id", "enhance:1000:1001"), ("resource_id", "17002")],
    );
    assert_projection_value(
        enhance_item,
        "required_quantity",
        ProjectionValue::Integer(1),
    );
    assert_projection_value(
        enhance_item,
        "available_quantity",
        ProjectionValue::Integer(4),
    );
    assert_projection_value(
        enhance_item,
        "maximum_craftable",
        ProjectionValue::Integer(4),
    );
    assert_projection_value(enhance_item, "planned_delta", ProjectionValue::Blank);

    let destroy_output = projection_row_with_texts(
        &projection,
        "resource_recipes",
        &[("recipe_id", "destroy:1001"), ("resource_id", "17003")],
    );
    assert_projection_value(
        destroy_output,
        "result_quantity",
        ProjectionValue::Integer(3),
    );
    assert_projection_value(
        destroy_output,
        "maximum_craftable",
        ProjectionValue::Integer(2),
    );
}

#[test]
fn raw_data_chunks_reassemble_every_canonical_record() {
    let state = map_fixture(&load_fixture());
    let projection = project_game_state_to_workbook(&state).unwrap();
    let raw_sheet = projection.sheet("raw_data").unwrap();

    for record in state.raw_records().records() {
        let source_ref = raw_record_source_ref(record.key());
        let rows: Vec<_> = raw_sheet
            .rows()
            .iter()
            .filter(|row| row.value("source_ref") == Some(&ProjectionValue::text(&source_ref)))
            .collect();
        assert!(!rows.is_empty(), "{source_ref} 必须至少存在一个分块");
        let mut reassembled = String::new();
        for (index, row) in rows.iter().enumerate() {
            assert_projection_value(
                row,
                "chunk_index",
                ProjectionValue::Integer(i64::try_from(index + 1).unwrap()),
            );
            assert_projection_value(
                row,
                "chunk_count",
                ProjectionValue::Integer(i64::try_from(rows.len()).unwrap()),
            );
            assert_projection_value(
                row,
                "content_sha256",
                ProjectionValue::text(record.content_sha256()),
            );
            assert_projection_value(
                row,
                "source_content_sha256",
                ProjectionValue::text(state.raw_records().source_content_sha256()),
            );
            let Some(ProjectionValue::Json(chunk)) = row.value("canonical_json_chunk") else {
                panic!("{source_ref} 的原始正文必须保存为 JSON 分块");
            };
            assert!(chunk.encode_utf16().count() <= 30_000);
            reassembled.push_str(chunk);
        }
        assert_eq!(
            reassembled,
            record.canonical_json(),
            "{source_ref} 重组不符"
        );
    }
}

fn projection_row_with_texts<'a>(
    projection: &'a WorkbookProjectionV4,
    sheet_key: &str,
    values: &[(&str, &str)],
) -> &'a WorkbookProjectionRow {
    projection
        .sheet(sheet_key)
        .unwrap()
        .rows()
        .iter()
        .find(|row| {
            values.iter().all(|(field_key, expected)| {
                matches!(
                    row.value(field_key),
                    Some(ProjectionValue::Text(actual)) if actual == expected
                )
            })
        })
        .unwrap_or_else(|| panic!("{sheet_key} 中找不到指定黄金行 {values:?}"))
}

fn equipment_skill_effects(projection: &WorkbookProjectionV4, config_id: &str) -> Vec<Value> {
    let row = projection
        .sheet("equipment_inventory")
        .unwrap()
        .rows()
        .iter()
        .find(|row| row.value("config_id") == Some(&ProjectionValue::text(config_id)))
        .unwrap();
    projection_json(row, "skill_effects_json")
        .as_array()
        .unwrap()
        .clone()
}

fn assert_projection_value(
    row: &WorkbookProjectionRow,
    field_key: &str,
    expected: ProjectionValue,
) {
    assert_eq!(
        row.value(field_key),
        Some(&expected),
        "字段 {field_key} 不符"
    );
}

fn projection_json(row: &WorkbookProjectionRow, field_key: &str) -> Value {
    let Some(ProjectionValue::Json(value)) = row.value(field_key) else {
        panic!("字段 {field_key} 应为 JSON")
    };
    serde_json::from_str(value).unwrap()
}

fn raw_record_source_ref(key: &RawRecordKey) -> String {
    match key {
        RawRecordKey::EquipmentConfig(config_id) => {
            format!("equipment_config:{}", config_id.get())
        }
        RawRecordKey::EquipmentRecipe(recipe_id) => format!("equipment_recipe:{recipe_id}"),
        RawRecordKey::EquipmentReferenceNames => "equipment_reference_names".to_owned(),
        RawRecordKey::EquipmentWeapon(weapon_id) => format!("equipment_weapon:{weapon_id}"),
        RawRecordKey::EquipmentSkill { skill_id, level } => {
            format!("equipment_skill:{skill_id}:{level}")
        }
        RawRecordKey::ShipCatalog {
            table_key,
            record_id,
        } => format!("{table_key}:{record_id}"),
        RawRecordKey::ShipSkill { skill_id, level } => {
            format!("ship_skill:{skill_id}:{level}")
        }
    }
}

fn load_fixture() -> FullStateFixture {
    serde_json::from_str(INPUT).expect("完整状态黄金输入应符合冻结 DTO 结构")
}

fn map_fixture(fixture: &FullStateFixture) -> GameState {
    try_map_fixture(fixture).expect("黄金输入应能映射为完整游戏状态")
}

fn try_map_fixture(fixture: &FullStateFixture) -> Result<GameState, GameStateMappingError> {
    let equipment = fixture_equipment(fixture);
    let ship_catalog = fixture_ship_catalog(fixture);
    let ship_skill_effects = golden_ship_skill_effects();
    map_game_state_borrowed_with_scope(
        &fixture.owned_state_before,
        &fixture.ship_details,
        &fixture.owned_state_after,
        &equipment,
        &ship_catalog,
        &ship_skill_effects,
        &fixture.module_sha256,
        crate::domain::GameReadScope::full(),
    )
}

fn fixture_ship_catalog(fixture: &FullStateFixture) -> ShipCatalogReadResult {
    let records_for = |table_key| match table_key {
        ShipCatalogTableKey::ShipDataGroup => vec![ShipCatalogRecord {
            id: 1,
            raw: json!({
                "code": 1,
                "group_type": 10117,
                "trans_skill": [],
                "trans_type": 0
            }),
        }],
        ShipCatalogTableKey::ShipDataTemplate => vec![ShipCatalogRecord {
            id: 101171,
            raw: json!({
                "id": 101171,
                "group_type": 10117,
                "type": 1,
                "max_level": 125,
                "star_max": 6,
                "equip_1": [10],
                "equip_2": [5, 10],
                "equip_3": [6, 21],
                "equip_4": [10],
                "equip_5": [10],
                "strengthen_id": 0,
                "buff_list": [10411],
                "buff_list_display": [10411],
                "hide_buff_list": []
            }),
        }],
        ShipCatalogTableKey::ShipDataStatistics => vec![ShipCatalogRecord {
            id: 101171,
            raw: json!({
                "id": 101171,
                "name": "拉菲",
                "english_name": "USS Laffey",
                "nationality": 1,
                "armor_type": 1,
                "rarity": 4
            }),
        }],
        ShipCatalogTableKey::SkillDataTemplate => vec![ShipCatalogRecord {
            id: 10411,
            raw: json!({
                "id": 10411,
                "name": "所罗门战神",
                "desc": "测试技能说明",
                "max_level": 1
            }),
        }],
        ShipCatalogTableKey::SkillDataDisplay => vec![ShipCatalogRecord {
            id: 10411,
            raw: json!({"id": 10411, "name": "所罗门战神"}),
        }],
        _ => Vec::new(),
    };
    let catalog = ShipCatalogReadResult::from_capture(
        fixture.module_sha256.clone(),
        String::new(),
        ShipCatalogTableKey::ALL
            .into_iter()
            .map(|table_key| ShipCatalogTable::from_capture(table_key, records_for(table_key)))
            .collect(),
    );
    let content_sha256 =
        sha256_sorted_json(&catalog.document()).expect("黄金舰船静态目录必须可规范编码");
    ShipCatalogReadResult::from_capture(
        fixture.module_sha256.clone(),
        content_sha256,
        catalog.tables().to_vec(),
    )
}

fn fixture_equipment(fixture: &FullStateFixture) -> EquipmentReadResult {
    let equipment = &fixture.equipment;
    let catalog = map_equipment_catalog(
        &equipment.config_pages,
        equipment.config_page_size,
        &equipment.recipe_pages,
        equipment.recipe_page_size,
        &equipment.reference_names,
        &fixture.module_sha256,
    )
    .expect("黄金装备输入应能映射为完整目录");
    let configs = equipment
        .config_pages
        .iter()
        .flat_map(|page| page.configs.iter().cloned())
        .collect();
    let recipes = equipment
        .recipe_pages
        .iter()
        .flat_map(|page| page.recipes.iter().cloned())
        .collect();
    let raw_records = EquipmentRawRecords::new(
        configs,
        recipes,
        equipment.reference_names.clone(),
        equipment.weapons.clone(),
        equipment.skills.clone(),
    )
    .expect("黄金装备原始记录应可计算稳定摘要");
    EquipmentReadResult::new(catalog, raw_records)
}

fn project_game_state(state: &GameState) -> Value {
    let source = state.source();
    json!({
        "schema_version": state.schema_version(),
        "source": {
            "module_sha256": source.module_sha256(),
            "owned_state_schema_version": source.owned_state_schema_version(),
            "ship_details_schema_version": source.ship_details_schema_version(),
            "equipment_catalog_schema_version": source.equipment_catalog_schema_version(),
            "raw_records_schema_version": source.raw_records_schema_version(),
            "owned_state_content_sha256": source.owned_state_content_sha256(),
            "ship_roster_content_sha256": source.ship_roster_content_sha256(),
            "equipment_catalog_content_sha256": source.equipment_catalog_content_sha256(),
            "ship_catalog_schema_version": source.ship_catalog_schema_version(),
            "ship_catalog_content_sha256": source.ship_catalog_content_sha256(),
            "raw_records_content_sha256": source.raw_records_content_sha256(),
            "content_sha256": source.content_sha256(),
            "read_scope": source.read_scope(),
        },
        "ships": {
            "source": {
                "module_sha256": state.ships().source().module_sha256(),
                "content_sha256": state.ships().source().content_sha256(),
            },
            "items": state.ships().ships().iter().map(project_ship).collect::<Vec<_>>(),
        },
        "equipment_catalog": project_equipment_catalog(state.equipment_catalog()),
        "equipment_details": project_equipment_details(state.equipment_details()),
        "equipment_inventory": state
            .equipment_inventory()
            .warehouse()
            .iter()
            .copied()
            .map(project_warehouse_stack)
            .collect::<Vec<_>>(),
        "bag": state.bag().items().iter().map(project_bag_item).collect::<Vec<_>>(),
        "resources": {
            "gold": state.resources().gold(),
            "equipment_capacity": state.resources().equipment_capacity(),
            "equipment_limit": state.resources().equipment_limit(),
        },
        "raw_records": {
            "schema_version": state.raw_records().schema_version(),
            "source_content_sha256": state.raw_records().source_content_sha256(),
            "items": state
                .raw_records()
                .records()
                .iter()
                .map(project_raw_record)
                .collect::<Vec<_>>(),
        },
    })
}

fn project_ship(ship: &ShipProfile) -> Value {
    let identity = ship.identity();
    let growth = ship.growth();
    let intimacy = ship.intimacy();
    let classification = ship.classification();
    let performance = ship.performance();
    let oil_cost = performance.oil_cost();
    let attributes = performance.attributes();
    json!({
        "identity": {
            "instance_id": identity.instance_id().get(),
            "config_id": identity.config_id(),
            "name": identity.name(),
            "create_time": identity.create_time(),
        },
        "growth": {
            "level": growth.level(),
            "max_level": growth.max_level(),
            "experience_in_level": growth.experience_in_level(),
            "total_experience": growth.total_experience(),
            "next_level_experience": growth.next_level_experience(),
            "energy": growth.energy(),
            "proficiency": growth.proficiency(),
        },
        "intimacy": {
            "raw_hundredths": intimacy.raw_hundredths(),
            "maximum": intimacy.maximum(),
            "stage_id": intimacy.stage_id(),
            "stage_description": intimacy.stage_description(),
            "proposed": intimacy.proposed(),
            "propose_time": intimacy.propose_time(),
        },
        "fleet_memberships": ship
            .fleet_memberships()
            .iter()
            .map(|membership| {
                json!({
                    "fleet_id": membership.fleet_id(),
                    "display_name": membership.display_name(),
                    "kind": membership.kind().code(),
                    "team": membership.team().code(),
                    "position": membership.position(),
                })
            })
            .collect::<Vec<_>>(),
        "classification": {
            "group_id": classification.group_id(),
            "ship_type": {
                "id": classification.ship_type().id(),
                "name": classification.ship_type().name(),
            },
            "armor_type": {
                "id": classification.armor_type().id(),
                "name": classification.armor_type().name(),
            },
            "nation": {
                "id": classification.nation().id(),
                "name": classification.nation().name(),
            },
            "rarity": classification.rarity(),
            "stars": {
                "current": classification.stars().current(),
                "maximum": classification.stars().maximum(),
            },
            "skin_id": classification.skin_id(),
        },
        "performance": {
            "combat_power": performance.combat_power(),
            "locked": performance.locked(),
            "oil_cost": {
                "start": oil_cost.start(),
                "end": oil_cost.end(),
                "total": oil_cost.total(),
            },
            "attributes": {
                "base": project_ship_attributes(attributes.base()),
                "equipment_applied": project_ship_attributes(attributes.equipment_applied()),
                "effective": project_ship_attributes(attributes.effective()),
                "equipment_delta": project_ship_attributes(attributes.equipment_delta()),
                "global_delta": project_ship_attributes(attributes.global_delta()),
            },
        },
        "skills": ship
            .skills()
            .iter()
            .map(|skill| {
                let identity = skill.identity();
                let progress = skill.progress();
                json!({
                    "identity": {
                        "skill_id": identity.skill_id(),
                        "effective_skill_id": identity.effective_skill_id(),
                        "name": identity.name(),
                    },
                    "progress": {
                        "level": progress.level(),
                        "max_level": progress.max_level(),
                        "experience": progress.experience(),
                        "next_level_experience": progress.next_level_experience(),
                    },
                    "description_template": skill.description_template(),
                    "current_effect": skill.current_effect(),
                })
            })
            .collect::<Vec<_>>(),
        "slots": ship
            .slots()
            .iter()
            .map(|slot| {
                json!({
                    "index": slot.index().get(),
                    "allowed_equipment_type_ids": slot.allowed_equipment_type_ids(),
                    "equipment": slot.equipment().map(|equipment| {
                        json!({
                            "runtime_id": equipment.runtime_id(),
                            "config_id": equipment.config_id().get(),
                            "enhance_level": equipment.enhance_level().get(),
                        })
                    }),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn project_ship_attributes(attributes: ShipAttributeValues) -> Value {
    json!({
        "durability": attributes.durability(),
        "cannon": attributes.cannon(),
        "torpedo": attributes.torpedo(),
        "anti_aircraft": attributes.anti_aircraft(),
        "air": attributes.air(),
        "reload": attributes.reload(),
        "hit": attributes.hit(),
        "dodge": attributes.dodge(),
        "anti_sub": attributes.anti_sub(),
        "luck": attributes.luck(),
        "speed": attributes.speed(),
    })
}

fn project_equipment_catalog(catalog: &EquipmentCatalog) -> Value {
    json!({
        "schema_version": catalog.schema_version(),
        "source": {
            "module_sha256": catalog.source().module_sha256(),
            "content_sha256": catalog.source().content_sha256(),
        },
        "config_count": catalog.config_count(),
        "families": catalog
            .families()
            .iter()
            .map(project_equipment_family)
            .collect::<Vec<_>>(),
        "recipes": catalog
            .recipes()
            .iter()
            .map(project_compose_recipe)
            .collect::<Vec<_>>(),
    })
}

fn project_equipment_family(family: &EquipmentFamily) -> Value {
    json!({
        "family_id": family.family_id().get(),
        "name": family.name(),
        "configs": family
            .configs()
            .iter()
            .map(project_equipment_definition)
            .collect::<Vec<_>>(),
    })
}

fn project_equipment_definition(config: &EquipmentDefinition) -> Value {
    let identity = config.identity();
    let classification = config.classification();
    let enhancement = config.enhancement();
    json!({
        "identity": {
            "config_id": identity.config_id().get(),
            "family_id": identity.family_id().get(),
            "name": identity.name(),
            "icon_key": identity.icon_key(),
        },
        "classification": {
            "equipment_type": {
                "id": classification.equipment_type().equipment_type_id(),
                "name": classification.equipment_type().name(),
            },
            "nation": {
                "id": classification.nation().nation_id(),
                "name": classification.nation().name(),
            },
            "rarity": classification.rarity(),
            "tech_level": classification.tech_level(),
            "speciality": classification.speciality(),
            "ammo_type": classification.ammo_type(),
            "torpedo_ammo": classification.torpedo_ammo(),
            "is_device": classification.is_device(),
            "is_aircraft": classification.is_aircraft(),
        },
        "enhancement": {
            "level": enhancement.level().get(),
            "base_config_id": enhancement.base_config_id().map(|id| id.get()),
            "previous_config_id": enhancement.previous_config_id().map(|id| id.get()),
            "next_config_id": enhancement.next_config_id().map(|id| id.get()),
            "upgrade_formula_ids": enhancement.upgrade_formula_ids(),
            "next_cost": project_equipment_resources(enhancement.next_cost()),
            "restore_yield": project_equipment_resources(enhancement.restore_yield()),
            "destroy_yield": project_equipment_resources(enhancement.destroy_yield()),
        },
        "attributes": config
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
            .collect::<Vec<_>>(),
        "compatibility": {
            "main_ship_types": project_named_ship_types(config.compatibility().main_ship_types()),
            "sub_ship_types": project_named_ship_types(config.compatibility().sub_ship_types()),
            "forbidden_ship_types": project_named_ship_types(
                config.compatibility().forbidden_ship_types()
            ),
        },
        "weapon_ids": config.weapon_ids(),
        "skill_references": config
            .skill_references()
            .iter()
            .copied()
            .map(|skill| {
                json!({
                    "skill_id": skill.skill_id(),
                    "level": skill.level(),
                    "visibility": match skill.visibility() {
                        EquipmentSkillVisibility::Visible => "visible",
                        EquipmentSkillVisibility::Hidden => "hidden",
                    },
                })
            })
            .collect::<Vec<_>>(),
        "labels": config.labels(),
        "description": config.description(),
        "gear_score": config.gear_score(),
        "anti_siren_power": config.anti_siren_power(),
        "importance": config.importance(),
        "equipment_limit": config.equipment_limit(),
    })
}

fn project_equipment_resources(resources: &EquipmentResources) -> Value {
    json!({
        "gold": resources.gold(),
        "items": resources
            .items()
            .iter()
            .copied()
            .map(project_item_quantity)
            .collect::<Vec<_>>(),
    })
}

fn project_item_quantity(item: EquipmentItemQuantity) -> Value {
    json!({
        "item_id": item.item_id(),
        "quantity": item.quantity(),
    })
}

fn project_named_ship_types(types: &[crate::domain::NamedEquipmentShipType]) -> Vec<Value> {
    types
        .iter()
        .map(|ship_type| {
            json!({
                "id": ship_type.ship_type_id(),
                "name": ship_type.name(),
            })
        })
        .collect()
}

fn project_compose_recipe(recipe: &EquipmentComposeRecipe) -> Value {
    json!({
        "recipe_id": recipe.recipe_id(),
        "material": project_item_quantity(recipe.material()),
        "gold": recipe.gold(),
        "equipment_config_id": recipe.equipment_config_id().get(),
    })
}

fn project_equipment_details(details: &EquipmentDetailCatalog) -> Value {
    json!({
        "weapons": details
            .weapons()
            .iter()
            .map(project_weapon)
            .collect::<Vec<_>>(),
        "skills": details
            .skills()
            .iter()
            .map(project_equipment_skill)
            .collect::<Vec<_>>(),
    })
}

fn project_weapon(weapon: &EquipmentWeapon) -> Value {
    json!({
        "weapon_id": weapon.weapon_id(),
        "base_weapon_id": weapon.base_weapon_id(),
        "action_index": weapon.action_index(),
        "aim_type": weapon.aim_type(),
        "angle": weapon.angle(),
        "attack_attribute": weapon.attack_attribute(),
        "attack_attribute_ratio": weapon.attack_attribute_ratio(),
        "auto_aftercast": weapon.auto_aftercast(),
        "axis_angle": weapon.axis_angle(),
        "barrage_ids": weapon.barrage_ids(),
        "bullet_ids": weapon.bullet_ids(),
        "charge_parameter": match weapon.charge_parameter() {
            WeaponChargeParameter::Empty => json!({"kind": "empty"}),
            WeaponChargeParameter::Lock { lock_time, max_lock } => json!({
                "kind": "lock",
                "lock_time": lock_time,
                "max_lock": max_lock,
            }),
        },
        "corrected": weapon.corrected(),
        "damage": weapon.damage(),
        "effect_move": weapon.effect_move(),
        "expose": weapon.expose(),
        "fire_fx": weapon.fire_fx(),
        "fire_fx_loop_type": weapon.fire_fx_loop_type(),
        "fire_sfx": weapon.fire_sfx(),
        "initial_over_heat": weapon.initial_over_heat(),
        "min_range": weapon.min_range(),
        "oxygen_types": weapon.oxygen_types(),
        "precast_parameter": match weapon.precast_parameter() {
            WeaponPrecastParameter::Values(values) => json!({
                "kind": "values",
                "values": values,
            }),
            WeaponPrecastParameter::LegacyWhitespace => json!({
                "kind": "legacy_whitespace",
            }),
        },
        "queue": weapon.queue(),
        "range": weapon.range(),
        "recover_time": weapon.recover_time(),
        "reload_max": weapon.reload_max(),
        "search_conditions": weapon.search_conditions(),
        "search_type": weapon.search_type(),
        "shakescreen": weapon.shakescreen(),
        "spawn_bound": weapon.spawn_bound(),
        "suppress": weapon.suppress(),
        "torpedo_ammo": weapon.torpedo_ammo(),
        "weapon_type": weapon.weapon_type(),
    })
}

fn project_equipment_skill(skill: &EquipmentSkillDetail) -> Value {
    let display = skill.display();
    json!({
        "skill_id": skill.skill_id(),
        "level": skill.level(),
        "display": {
            "name": display.name(),
            "description": display.description(),
            "acquire_description": display.acquire_description(),
            "system_transform": project_skill_value(display.system_transform()),
        },
        "battle_skill": skill.battle_skill().map(project_skill_source),
        "battle_buff": skill.battle_buff().map(project_skill_source),
    })
}

fn project_skill_source(source: &EquipmentSkillSource) -> Value {
    json!({
        "config_id": source.config_id(),
        "name": source.name(),
        "description": source.description(),
        "cooldown": source.cooldown(),
        "duration": source.duration(),
        "stack": source.stack(),
        "effects": source
            .effects()
            .iter()
            .map(project_skill_effect)
            .collect::<Vec<_>>(),
    })
}

fn project_skill_effect(effect: &EquipmentSkillEffect) -> Value {
    json!({
        "sequence": effect.sequence(),
        "effect_type": effect.effect_type(),
        "target_choices": effect.target_choices(),
        "triggers": effect.triggers(),
        "arguments": effect
            .arguments()
            .iter()
            .map(|argument| {
                json!({
                    "name": argument.name(),
                    "value": project_skill_value(argument.value()),
                })
            })
            .collect::<Vec<_>>(),
        "metadata": effect
            .metadata()
            .iter()
            .map(|field| {
                json!({
                    "name": field.name(),
                    "value": project_skill_value(field.value()),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn project_skill_value(value: &SkillValue) -> Value {
    match value {
        SkillValue::Null => json!({"kind": "null"}),
        SkillValue::Bool(value) => json!({"kind": "bool", "value": value}),
        SkillValue::Number(value) => json!({"kind": "number", "value": value}),
        SkillValue::String(value) => json!({"kind": "string", "value": value}),
        SkillValue::List(values) => json!({
            "kind": "list",
            "items": values.iter().map(project_skill_value).collect::<Vec<_>>(),
        }),
        SkillValue::Object(fields) => json!({
            "kind": "object",
            "fields": fields
                .iter()
                .map(|field| {
                    json!({
                        "name": field.name(),
                        "value": project_skill_value(field.value()),
                    })
                })
                .collect::<Vec<_>>(),
        }),
        SkillValue::MixedTable(table) => json!({
            "kind": "mixed_table",
            "entries": table
                .entries()
                .iter()
                .map(|entry| {
                    let key = match entry.key() {
                        SkillTableKey::Number(value) => {
                            json!({"kind": "number", "value": value})
                        }
                        SkillTableKey::String(value) => {
                            json!({"kind": "string", "value": value})
                        }
                    };
                    json!({
                        "key": key,
                        "value": project_skill_value(entry.value()),
                    })
                })
                .collect::<Vec<_>>(),
            "reason": table.reason(),
        }),
    }
}

fn project_warehouse_stack(stack: WarehouseEquipmentStack) -> Value {
    json!({
        "runtime_group_id": stack.runtime_group_id(),
        "config_id": stack.config_id().get(),
        "family_id": stack.family_id().get(),
        "enhance_level": stack.enhance_level().get(),
        "quantity": stack.quantity(),
    })
}

fn project_bag_item(item: &BagItem) -> Value {
    json!({
        "item_id": item.item_id(),
        "quantity": item.quantity(),
        "name": item.name(),
        "compose": item.compose().map(project_bag_compose),
    })
}

fn project_bag_compose(compose: BagComposeAvailability) -> Value {
    json!({
        "recipe_id": compose.recipe_id(),
        "material_id": compose.material_id(),
        "material_count": compose.material_count(),
        "gold": compose.gold(),
        "equipment_config_id": compose.equipment_config_id().map(|id| id.get()),
        "max_count": compose.max_count(),
    })
}

fn project_raw_record(record: &RawRecord) -> Value {
    let key = match record.key() {
        RawRecordKey::EquipmentConfig(config_id) => {
            json!({"kind": "equipment_config", "config_id": config_id.get()})
        }
        RawRecordKey::EquipmentRecipe(recipe_id) => {
            json!({"kind": "equipment_recipe", "recipe_id": recipe_id})
        }
        RawRecordKey::EquipmentReferenceNames => {
            json!({"kind": "equipment_reference_names"})
        }
        RawRecordKey::EquipmentWeapon(weapon_id) => {
            json!({"kind": "equipment_weapon", "weapon_id": weapon_id})
        }
        RawRecordKey::EquipmentSkill { skill_id, level } => json!({
            "kind": "equipment_skill",
            "skill_id": skill_id,
            "level": level,
        }),
        RawRecordKey::ShipCatalog {
            table_key,
            record_id,
        } => json!({
            "kind": "ship_catalog",
            "table_key": table_key,
            "record_id": record_id,
        }),
        RawRecordKey::ShipSkill { skill_id, level } => json!({
            "kind": "ship_skill",
            "skill_id": skill_id,
            "level": level,
        }),
    };
    let canonical_json: Value =
        serde_json::from_str(record.canonical_json()).expect("领域原始记录应始终保存有效规范 JSON");
    json!({
        "key": key,
        "content_sha256": record.content_sha256(),
        "canonical_json": canonical_json,
    })
}

/// 创建全部卸下且没有仓库装备的候选测试状态，数量仅由合成材料决定。
pub(crate) fn golden_game_state_with_only_composable_equipment(materials: u64) -> GameState {
    let mut fixture = load_fixture();
    for owned in [
        &mut fixture.owned_state_before,
        &mut fixture.owned_state_after,
    ] {
        owned.warehouse.items.clear();
        owned.warehouse.count = 0;
        owned.player.equipment_capacity = 0;
        owned.bag.items[0].quantity = materials;
        for slot in &mut owned.dock.ships[0].slots {
            slot.equipment = None;
        }
    }
    map_fixture(&fixture)
}

#[test]
fn equipment_choices_distinguish_sources_and_share_identical_slot_lists() {
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let dictionary = projection.sheet("dictionaries").unwrap();
    let labels: Vec<_> = dictionary
        .rows()
        .iter()
        .filter_map(|row| match row.value("display_label") {
            Some(ProjectionValue::Text(label)) if label.contains('｜') => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert!(labels.iter().any(|label| label.starts_with("仓库×")));
    assert!(labels.iter().any(|label| label.contains("·槽")));
    assert!(labels.iter().all(|label| label.contains(" T")
        && !label.contains(" +0")
        && !label.contains("warehouse:")
        && !label.contains("ship:")));
    assert_eq!(
        labels
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        labels.len()
    );
    let priorities: Vec<_> = labels
        .iter()
        .map(|label| {
            if label.starts_with("仓库×") {
                0
            } else if label.starts_with("合成×") {
                1
            } else {
                2
            }
        })
        .collect();
    assert!(priorities.windows(2).all(|pair| pair[0] <= pair[1]));
    let binding = |slot: &str| {
        dictionary
            .rows()
            .iter()
            .find(|row| {
                row.value("category_key") == Some(&ProjectionValue::text("equipment_target"))
                    && row.value("stable_value") == Some(&ProjectionValue::text(slot))
            })
            .unwrap()
    };
    assert_eq!(
        binding("1").value("display_label"),
        binding("2").value("display_label")
    );
    assert_ne!(
        binding("1").value("display_label"),
        binding("3").value("display_label")
    );
}

#[test]
fn equipment_choices_exclude_forbidden_ship_types() {
    let mut fixture = load_fixture();
    for page in &mut fixture.equipment.config_pages {
        for config in &mut page.configs {
            config.raw_config["ship_type_forbidden"] = json!([1, 3]);
        }
    }
    let projection = project_game_state_to_workbook(&map_fixture(&fixture)).unwrap();
    assert!(projection.sheet("dictionaries").unwrap().rows().iter().all(|row| !matches!(row.value("display_label"), Some(ProjectionValue::Text(label)) if label.contains('｜'))));
}

#[test]
fn equipment_choices_include_only_currently_affordable_composition_without_owned_items() {
    for materials in [0, 30] {
        let projection = project_game_state_to_workbook(
            &golden_game_state_with_only_composable_equipment(materials),
        )
        .unwrap();
        let count = projection.sheet("dictionaries").unwrap().rows().iter().filter(|row| matches!(row.value("display_label"), Some(ProjectionValue::Text(label)) if label.starts_with("合成×"))).count();
        assert_eq!(count, usize::from(materials == 30));
    }
}

#[test]
fn equipment_scope_preserves_core_mapping_and_only_requires_requested_references() {
    let mut hashes = std::collections::BTreeSet::new();
    for (weapons, skills) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut fixture = golden_replay_fixture();
        if !weapons {
            fixture.equipment.weapons.clear();
        }
        if !skills {
            fixture.equipment.skills.clear();
        }
        let equipment = fixture_equipment(&fixture);
        let ship_catalog = fixture_ship_catalog(&fixture);
        let scope = crate::domain::GameReadScope::full().with_equipment_details(weapons, skills);
        let mapped = super::map_game_state_borrowed_with_scope(
            &fixture.owned_state_before,
            &fixture.ship_details,
            &fixture.owned_state_after,
            &equipment,
            &ship_catalog,
            &golden_ship_skill_effects(),
            &fixture.module_sha256,
            scope,
        )
        .unwrap();
        assert_eq!(mapped.source().read_scope(), scope);
        let full = golden_game_state();
        assert_eq!(mapped.equipment_catalog(), full.equipment_catalog());
        assert_eq!(mapped.equipment_inventory(), full.equipment_inventory());
        assert_eq!(mapped.resources(), full.resources());
        hashes.insert(mapped.source().content_sha256().to_owned());
        let projection = project_game_state_to_workbook(&mapped).unwrap();
        for row in projection.sheet("equipment_inventory").unwrap().rows() {
            assert_eq!(
                row.value("weapons_json") == Some(&ProjectionValue::Blank),
                !weapons
            );
            assert_eq!(
                row.value("skill_effects_json") == Some(&ProjectionValue::Blank),
                !skills
            );
            assert_eq!(
                row.value("effect_summary") == Some(&ProjectionValue::Blank),
                !(weapons && skills)
            );
            assert_eq!(
                row.value("data_complete"),
                Some(&ProjectionValue::Boolean(true))
            );
        }
        if !weapons || !skills {
            let error = super::map_game_state_borrowed_with_scope(
                &fixture.owned_state_before,
                &fixture.ship_details,
                &fixture.owned_state_after,
                &equipment,
                &ship_catalog,
                &golden_ship_skill_effects(),
                &fixture.module_sha256,
                crate::domain::GameReadScope::full(),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                GameStateMappingError::EquipmentDetailMissing { .. }
            ));
        }
    }
    assert_eq!(hashes.len(), 4);
}

/// 为科技列的映射和工作簿验证提供拥有完整图鉴历史的样本。
pub(crate) fn golden_game_state_with_technology() -> GameState {
    let fixture = golden_replay_fixture();
    let equipment = fixture_equipment(&fixture);
    let ship_catalog = fixture_technology_catalog(&fixture);
    let ship_skill_effects = golden_ship_skill_effects();
    map_game_state_borrowed_with_scope(
        &fixture.owned_state_before,
        &fixture.ship_details,
        &fixture.owned_state_after,
        &equipment,
        &ship_catalog,
        &ship_skill_effects,
        &fixture.module_sha256,
        crate::domain::GameReadScope::full(),
    )
    .unwrap()
}

fn fixture_technology_catalog(fixture: &FullStateFixture) -> ShipCatalogReadResult {
    let mut tables = fixture_ship_catalog(fixture).tables().to_vec();
    for (key, rows) in [
        (
            ShipCatalogTableKey::FleetTechShipTemplate,
            vec![(
                10117,
                json!({"max_star":5,"pt_get":8,"pt_upgrage":16,"pt_level":12,"add_get_attr":1,"add_get_value":1,"add_get_shiptype":[1,20,21],"add_level_attr":2,"add_level_value":1,"add_level_shiptype":[1,20,21]}),
            )],
        ),
        (
            ShipCatalogTableKey::ShipDataByType,
            vec![
                (1, json!({"type_name":"驱逐"})),
                (20, json!({"type_name":"导驱"})),
                (21, json!({"type_name":"导驱"})),
            ],
        ),
        (
            ShipCatalogTableKey::AttributeInfoByType,
            vec![
                (1, json!({"condition":"耐久"})),
                (2, json!({"condition":"炮击"})),
            ],
        ),
        (
            ShipCatalogTableKey::CollectionShipGroup,
            vec![(10117, json!({"id":10117,"star":5,"maxLV":120}))],
        ),
    ] {
        tables.push(ShipCatalogTable::from_capture(
            key,
            rows.into_iter()
                .map(|(id, raw)| ShipCatalogRecord { id, raw })
                .collect(),
        ));
    }
    let catalog =
        ShipCatalogReadResult::from_capture(fixture.module_sha256.clone(), String::new(), tables);
    ShipCatalogReadResult::from_capture(
        fixture.module_sha256.clone(),
        sha256_sorted_json(&catalog.document()).unwrap(),
        catalog.tables().to_vec(),
    )
}

#[test]
fn technology_uses_shared_history_for_owned_and_unowned_ship_rows() {
    let state = golden_game_state_with_technology();
    let expected = [
        "已达成\n科技点 +8\n驱逐／导驱：耐久 +1",
        "已达成\n科技点 +16",
        "已达成\n科技点 +12\n驱逐／导驱：炮击 +1",
    ];
    for owned in [true, false] {
        let roster = ShipRoster::new_with_skill_effects(
            state.ships().source().clone(),
            if owned {
                state.ships().ships().to_vec()
            } else {
                Vec::new()
            },
            SkillEffectEvidenceCatalog::new(Vec::new()),
        );
        let variant = GameState::new(
            state.source().clone(),
            roster,
            state.ship_catalog().clone(),
            state.equipment_catalog().clone(),
            state.equipment_details().clone(),
            state.equipment_inventory().clone(),
            state.bag().clone(),
            state.resources(),
            state.raw_records().clone(),
        );
        let projection = project_game_state_to_workbook(&variant).unwrap();
        let rows = projection.sheet("loadout_plan").unwrap().rows();
        assert!(!rows.is_empty());
        for row in rows {
            for (key, text) in crate::application::TECHNOLOGY_FIELDS
                .into_iter()
                .zip(expected)
            {
                assert_projection_value(row, key, ProjectionValue::text(text));
            }
        }
    }
}

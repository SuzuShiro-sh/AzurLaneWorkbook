//! 验证完整运行态与舰船详情的严格关联及规范化领域结果。

#![recursion_limit = "256"]

use azur_lane_workbook::adapters::device::runtime::{
    RuntimeSkillEffectDetail, SnapshotOwnedStateResult, SnapshotShipDetailsResult,
};
use azur_lane_workbook::adapters::device::ship_mapper::{
    ShipMappingError, map_ship_roster_with_skill_effects,
};
use azur_lane_workbook::domain::{
    ShipAttributeBreakdown, ShipProfile, ShipRoster, SkillEffectSourceKind, SkillValue,
};
use serde_json::{Value, json};

const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn complete_snapshots_map_to_stable_ship_roster() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());

    let first: ShipRoster =
        map_ship_roster_with_skill_effects(&owned, &details, &[]).expect("完整对应快照应能映射");
    let second: ShipRoster =
        map_ship_roster_with_skill_effects(&owned, &details, &[]).expect("相同输入应能重复映射");

    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert!(!first.is_empty());
    assert_eq!(first.source().module_sha256(), MODULE_SHA256);
    assert_eq!(first.source().content_sha256().len(), 64);

    let ship: &ShipProfile = &first.ships()[0];
    assert_eq!(ship.identity().instance_id().get(), 9_001);
    assert_eq!(ship.identity().config_id(), 101_174);
    assert_eq!(ship.identity().name(), "测试舰船");
    assert_eq!(ship.growth().level(), 100);
    assert_eq!(ship.growth().max_level(), 125);
    assert_eq!(ship.growth().experience_in_level(), 3_000_000);
    assert_eq!(ship.growth().total_experience(), 4_500_000);
    assert_eq!(ship.growth().next_level_experience(), 10_000);
    assert_eq!(ship.growth().energy(), 150);
    assert_eq!(ship.growth().proficiency(), 7);
    assert_eq!(ship.intimacy().raw_hundredths(), 10_000);
    assert_eq!(ship.intimacy().maximum(), 200);
    assert_eq!(ship.intimacy().stage_id(), 5);
    assert_eq!(ship.intimacy().stage_description(), "爱");
    assert!(!ship.intimacy().proposed());
    assert_eq!(ship.intimacy().propose_time(), 0);
    assert_eq!(ship.fleet_memberships().len(), 1);
    let fleet_membership = &ship.fleet_memberships()[0];
    assert_eq!(fleet_membership.fleet_id(), 1);
    assert_eq!(fleet_membership.display_name(), Some("第一舰队"));
    assert_eq!(fleet_membership.kind().code(), "regular");
    assert_eq!(fleet_membership.team().code(), "vanguard");
    assert_eq!(fleet_membership.position(), 1);
    assert_eq!(ship.classification().group_id(), 10_117);
    assert_eq!(ship.classification().ship_type().name(), "驱逐舰");
    assert_eq!(ship.classification().armor_type().name(), "轻型装甲");
    assert_eq!(ship.classification().nation().name(), "白鹰");
    assert_eq!(ship.classification().rarity(), 4);
    assert_eq!(ship.classification().stars().current(), 5);
    assert_eq!(ship.classification().stars().maximum(), 6);
    assert_eq!(ship.classification().skin_id(), 101_170);
    assert_eq!(ship.performance().combat_power(), 4_321);
    assert!(ship.performance().locked());
    assert_eq!(ship.performance().oil_cost().total(), 10);

    let attributes: ShipAttributeBreakdown = ship.performance().attributes();
    assert_eq!(attributes.base().cannon(), 100.0);
    assert_eq!(attributes.base().speed(), 16.5);
    assert_eq!(attributes.equipment_applied().cannon(), 125.0);
    assert_eq!(attributes.effective().cannon(), 132.0);
    assert_eq!(attributes.equipment_delta().cannon(), 25.0);
    assert_eq!(attributes.equipment_delta().speed(), -1.0);
    assert_eq!(attributes.global_delta().cannon(), 7.0);
    assert_eq!(attributes.global_delta().speed(), 0.5);

    assert_eq!(ship.skills().len(), 1);
    assert_eq!(ship.skills()[0].identity().skill_id(), 10_410);
    assert_eq!(ship.skills()[0].identity().effective_skill_id(), 10_411);
    assert_eq!(ship.skills()[0].identity().name(), "测试技能");
    assert_eq!(ship.skills()[0].progress().level(), 1);
    assert_eq!(ship.skills()[0].progress().max_level(), 10);
    assert_eq!(ship.skills()[0].progress().experience(), 25);
    assert_eq!(ship.skills()[0].progress().next_level_experience(), 100);
    assert_eq!(ship.skills()[0].description_template(), "技能描述模板");
    assert_eq!(ship.skills()[0].current_effect(), "当前效果");
    assert_eq!(ship.slots().len(), 5);
    for (expected, slot) in (1_u8..=5).zip(ship.slots()) {
        assert_eq!(slot.index().get(), expected);
        assert!(slot.equipment().is_none());
    }
    assert_eq!(ship.slots()[0].allowed_equipment_type_ids(), &[1, 2]);
    assert_eq!(ship.slots()[1].allowed_equipment_type_ids(), &[5, 10]);
}

#[test]
fn occupied_slot_maps_runtime_identity_and_enhancement() {
    let mut value = owned_state_value();
    value["dock"]["ships"][0]["slots"][0]["equipment"] = json!({
        "equipment_id": 8001,
        "config_id": 1000,
        "enhance_level": 3
    });
    let owned: SnapshotOwnedStateResult = deserialize(value);
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());

    let roster = map_ship_roster_with_skill_effects(&owned, &details, &[]).unwrap();
    let equipment = roster.ships()[0].slots()[0].equipment().unwrap();
    assert_eq!(equipment.runtime_id(), 8_001);
    assert_eq!(equipment.config_id().get(), 1_000);
    assert_eq!(equipment.enhance_level().get(), 3);
}

#[test]
fn skill_effect_evidence_is_shared_and_materializes_current_level_arguments() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());
    let record: RuntimeSkillEffectDetail = deserialize(skill_effect_value());

    let roster =
        map_ship_roster_with_skill_effects(&owned, &details, &[record]).expect("技能证据应能映射");
    let evidence = roster
        .skill_effect(10_411, 1)
        .expect("应按生效技能键查到证据");

    assert!(evidence.complete());
    assert!(evidence.read_errors().is_empty());
    assert!(evidence.raw_structure_json().contains("\"battle_skill\""));
    assert_eq!(roster.skill_effects().len(), 1);
    assert_eq!(evidence.parameter_sources().len(), 1);
    let source = &evidence.parameter_sources()[0];
    assert_eq!(source.kind(), SkillEffectSourceKind::BattleSkill);
    assert_eq!(source.effects().len(), 1);
    assert_eq!(source.effects()[0].sequence(), 1);
    assert_eq!(source.effects()[0].arguments()[0].name(), "nested");
    assert_eq!(source.effects()[0].arguments()[1].name(), "ratio");
    assert!(matches!(
        source.effects()[0].arguments()[1].value(),
        SkillValue::Number(value) if (*value - 0.25).abs() < f64::EPSILON
    ));

    let without_evidence = map_ship_roster_with_skill_effects(&owned, &details, &[]).unwrap();
    assert!(without_evidence.skill_effects().is_empty());
    assert_ne!(
        roster.source().content_sha256(),
        without_evidence.source().content_sha256()
    );
}

#[test]
fn malformed_parameter_shape_is_retained_as_incomplete_evidence() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());
    let mut value = skill_effect_value();
    value["battle_skill"]["value"]["entries"][3]["value"]["effect_list"][0]["arg_list"] =
        Value::Null;
    let record: RuntimeSkillEffectDetail = deserialize(value);

    let roster = map_ship_roster_with_skill_effects(&owned, &details, &[record]).unwrap();
    let evidence = roster.skill_effect(10_411, 1).unwrap();

    assert!(!evidence.complete());
    assert!(evidence.parameter_sources().is_empty());
    assert_eq!(evidence.read_errors().len(), 1);
    assert!(evidence.read_errors()[0].starts_with("battle_skill.normalize:"));
    assert!(evidence.raw_structure_json().contains("\"arg_list\":null"));
}

#[test]
fn duplicate_skill_effect_keys_are_rejected() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());
    let record: RuntimeSkillEffectDetail = deserialize(skill_effect_value());

    assert!(matches!(
        map_ship_roster_with_skill_effects(&owned, &details, &[record.clone(), record]),
        Err(ShipMappingError::DuplicateSkillEffectEvidence {
            skill_id: 10_411,
            level: 1,
        })
    ));
}

#[test]
fn mapping_rejects_slot_enhance_level_outside_domain_range() {
    let mut value = owned_state_value();
    value["dock"]["ships"][0]["slots"][0]["equipment"] = json!({
        "equipment_id": 8001,
        "config_id": 1000,
        "enhance_level": 256
    });
    let owned: SnapshotOwnedStateResult = deserialize(value);
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());

    assert!(matches!(
        map_ship_roster_with_skill_effects(&owned, &details, &[]),
        Err(ShipMappingError::SlotFieldOutOfRange {
            ship_id: 9_001,
            slot_index: 1,
            field: "enhance_level",
            value: 256,
        })
    ));
}

#[test]
fn roster_content_digest_covers_owned_growth_fields() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let mut changed_value: Value = owned_state_value();
    changed_value["dock"]["ships"][0]["energy"] = json!(149);
    let changed: SnapshotOwnedStateResult = deserialize(changed_value);
    let details: SnapshotShipDetailsResult = deserialize(ship_details_value());

    let original_digest: String = map_ship_roster_with_skill_effects(&owned, &details, &[])
        .unwrap()
        .source()
        .content_sha256()
        .to_owned();
    let changed_digest: String = map_ship_roster_with_skill_effects(&changed, &details, &[])
        .unwrap()
        .source()
        .content_sha256()
        .to_owned();

    assert_ne!(original_digest, changed_digest);
}

#[test]
fn mapping_rejects_ship_state_changed_between_snapshots() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let mut value: Value = ship_details_value();
    value["ships"][0]["level"] = json!(101);
    let details: SnapshotShipDetailsResult = deserialize(value);

    assert!(matches!(
        map_ship_roster_with_skill_effects(&owned, &details, &[]),
        Err(ShipMappingError::ShipFieldMismatch {
            ship_id: 9_001,
            field: "level",
            owned_value: 100,
            detail_value: 101,
        })
    ));
}

#[test]
fn mapping_rejects_skill_progress_changed_between_snapshots() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let mut value: Value = ship_details_value();
    value["ships"][0]["skills"][0]["experience"] = json!(26);
    let details: SnapshotShipDetailsResult = deserialize(value);

    assert!(matches!(
        map_ship_roster_with_skill_effects(&owned, &details, &[]),
        Err(ShipMappingError::SkillProgressMismatch {
            ship_id: 9_001,
            skill_id: 10_410,
            owned_level: 1,
            detail_level: 1,
            owned_experience: 25,
            detail_experience: 26,
        })
    ));
}

#[test]
fn mapping_rejects_diagnostic_detail_subset() {
    let owned: SnapshotOwnedStateResult = deserialize(owned_state_value());
    let mut value: Value = ship_details_value();
    value["complete"] = json!(false);
    value["truncated"] = json!(true);
    let details: SnapshotShipDetailsResult = deserialize(value);

    assert!(matches!(
        map_ship_roster_with_skill_effects(&owned, &details, &[]),
        Err(ShipMappingError::DetailsIncomplete {
            read_errors: 0,
            truncated: true,
        })
    ));
}

fn deserialize<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("fixture 应符合 JSON 结构")
}

fn owned_state_value() -> Value {
    json!({
        "schema_version": 3,
        "complete": true,
        "dock": {
            "complete": true,
            "count": 1,
            "truncated": false,
            "ships": [{
                "ship_id": 9001,
                "config_id": 101174,
                "level": 100,
                "experience_in_level": 3000000,
                "intimacy_raw": 10000,
                "energy": 150,
                "proficiency": 7,
                "fleet_memberships": [{
                    "fleet_id": 1,
                    "display_name": "第一舰队",
                    "kind": "regular",
                    "team": "vanguard",
                    "position": 1
                }],
                "skills": [{"skill_id": 10410, "level": 1, "experience": 25}],
                "slots": [
                    {"slot_index": 1, "equipment": null},
                    {"slot_index": 2, "equipment": null},
                    {"slot_index": 3, "equipment": null},
                    {"slot_index": 4, "equipment": null},
                    {"slot_index": 5, "equipment": null}
                ]
            }],
            "read_errors": []
        },
        "warehouse": {
            "complete": true,
            "count": 0,
            "truncated": false,
            "items": [],
            "read_errors": []
        },
        "bag": {
            "schema_version": 1,
            "complete": true,
            "count": 0,
            "truncated": false,
            "items": [],
            "read_errors": []
        },
        "player": {"gold": 0, "equipment_capacity": 0, "equipment_limit": 300}
    })
}

fn ship_details_value() -> Value {
    json!({
        "schema_version": 4,
        "complete": true,
        "count": 1,
        "truncated": false,
        "source": {"module_sha256": MODULE_SHA256},
        "ships": [{
            "ship_id": 9001,
            "config_id": 101174,
            "name": "测试舰船",
            "level": 100,
            "max_level": 125,
            "experience_in_level": 3000000,
            "total_experience": 4500000,
            "next_level_experience": 10000,
            "intimacy_raw": 10000,
            "intimacy_maximum": 200,
            "intimacy_stage_id": 5,
            "intimacy_stage_description": "爱",
            "proposed": false,
            "propose_time": 0,
            "create_time": 0,
            "combat_power": 4321,
            "locked": true,
            "oil_cost": {"start": 4, "end": 6, "total": 10},
            "classification": {
                "group_id": 10117,
                "ship_type_id": 1,
                "ship_type_name": "驱逐舰",
                "armor_type_id": 1,
                "armor_type_name": "轻型装甲",
                "nation_id": 1,
                "nation_name": "白鹰",
                "rarity": 4,
                "star": 5,
                "max_star": 6,
                "skin_id": 101170
            },
            "base_attributes": ship_attributes(100.0, 16.5),
            "equipment_applied_attributes": ship_attributes(125.0, 15.5),
            "effective_attributes": ship_attributes(132.0, 16.0),
            "slot_rules": [
                {"slot_index": 1, "allowed_equipment_type_ids": [1, 2]},
                {"slot_index": 2, "allowed_equipment_type_ids": [5, 10]},
                {"slot_index": 3, "allowed_equipment_type_ids": [6, 21]},
                {"slot_index": 4, "allowed_equipment_type_ids": [10]},
                {"slot_index": 5, "allowed_equipment_type_ids": [10]}
            ],
            "skills": [{
                "skill_id": 10410,
                "effective_skill_id": 10411,
                "name": "测试技能",
                "level": 1,
                "max_level": 10,
                "experience": 25,
                "next_level_experience": 100,
                "description_template": "技能描述模板",
                "current_effect": "当前效果"
            }]
        }],
        "read_errors": []
    })
}

fn skill_effect_value() -> Value {
    json!({
        "skill_id": 10411,
        "level": 1,
        "display": {
            "available": true,
            "complete": true,
            "value": {"id": 10411, "name": "测试技能", "desc": "测试描述"},
            "error": null,
            "read_errors": []
        },
        "battle_skill": {
            "available": true,
            "complete": true,
            "value": {
                "lua_type": "table",
                "truncated": false,
                "reason": null,
                "entries": [
                    {"key": "id", "key_type": "string", "lua_type": null, "value": 10411},
                    {"key": "name", "key_type": "string", "lua_type": null, "value": "测试技能"},
                    {"key": "effect_list", "key_type": "string", "lua_type": null, "value": [
                        {"type": "BattleSkillFire", "arg_list": {"ratio": 0.1}}
                    ]},
                    {"key": 1, "key_type": "number", "lua_type": null, "value": {
                        "effect_list": [{
                            "type": "BattleSkillFire",
                            "arg_list": {"ratio": 0.25, "nested": {"count": 2}}
                        }]
                    }}
                ]
            },
            "error": null,
            "read_errors": []
        },
        "battle_buff": {
            "available": false,
            "complete": false,
            "value": null,
            "error": "buff template missing",
            "read_errors": []
        },
        "complete": true
    })
}

fn ship_attributes(cannon: f64, speed: f64) -> Value {
    json!({
        "durability": 1000.0,
        "cannon": cannon,
        "torpedo": 80.0,
        "anti_aircraft": 70.0,
        "air": 0.0,
        "reload": 120.0,
        "hit": 90.0,
        "dodge": 60.0,
        "anti_sub": 50.0,
        "luck": 45.0,
        "speed": speed
    })
}

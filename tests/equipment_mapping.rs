//! 验证装备静态分页到领域目录的完整映射和失败边界。

use azur_lane_workbook::adapters::device::equipment_mapper::{
    EquipmentMappingError, map_equipment_catalog,
};
use azur_lane_workbook::adapters::device::runtime::{
    ComposeRecipePageResult, EquipmentConfigPageResult, EquipmentReferenceNameBatchResult,
};
use azur_lane_workbook::domain::{EnhanceLevel, EquipmentFamilyId, EquipmentSkillVisibility};
use serde_json::{Value, json};

const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn complete_pages_map_to_a_stable_equipment_catalog() {
    let (config_pages, recipe_pages, references) = fixtures();
    let first = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("完整装备页应能映射");
    let second = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("相同输入应能重复映射");

    assert_eq!(first, second);
    assert_eq!(first.schema_version(), 2);
    assert_eq!(first.config_count(), 2);
    assert_eq!(first.families().len(), 1);
    assert_eq!(first.source().module_sha256(), MODULE_SHA256);
    assert_eq!(first.source().content_sha256().len(), 64);

    let family = first
        .family(EquipmentFamilyId::new(1000).expect("装备族 ID 应有效"))
        .expect("应找到测试装备族");
    assert_eq!(family.name(), "测试设备");
    assert_eq!(family.configs().len(), 2);
    assert_eq!(
        family
            .config(EnhanceLevel::new(0))
            .unwrap()
            .identity()
            .config_id()
            .get(),
        1000
    );
    assert_eq!(
        family
            .config(EnhanceLevel::new(1))
            .unwrap()
            .identity()
            .config_id()
            .get(),
        1001
    );

    let level_zero = &family.configs()[0];
    assert_eq!(level_zero.classification().nation().nation_id(), 0);
    assert_eq!(level_zero.classification().nation().name(), "其他");
    assert_eq!(level_zero.attributes()[0].name(), "炮击");
    assert!(level_zero.attributes()[0].auxiliary_boost());
    assert_eq!(
        level_zero.compatibility().main_ship_types()[0].name(),
        "驱逐舰"
    );
    assert_eq!(level_zero.weapon_ids(), &[700]);
    assert_eq!(level_zero.skill_references().len(), 2);
    assert_eq!(
        level_zero.skill_references()[0].visibility(),
        EquipmentSkillVisibility::Visible
    );
    assert_eq!(
        level_zero.skill_references()[1].visibility(),
        EquipmentSkillVisibility::Hidden
    );
    assert_eq!(first.recipes()[0].equipment_config_id().get(), 1000);
}

#[test]
fn content_digest_is_independent_from_page_boundaries() {
    let (single_page, recipe_pages, references) = fixtures();
    let mut first_page = single_page[0].clone();
    let mut second_page = single_page[0].clone();
    first_page.count = 1;
    first_page.next_index = Some(1);
    first_page.configs.truncate(1);
    second_page.start_index = 1;
    second_page.count = 1;
    second_page.next_index = None;
    second_page.configs = vec![single_page[0].configs[1].clone()];
    let split_pages = vec![first_page, second_page];

    let original = map_equipment_catalog(
        &single_page,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("单页目录应能映射");
    let repaged = map_equipment_catalog(
        &split_pages,
        1,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("相同目录拆页后仍应能映射");

    assert_eq!(original, repaged);
    assert_eq!(
        original.source().content_sha256(),
        repaged.source().content_sha256()
    );
}

#[test]
fn content_digest_is_independent_from_recipe_page_boundaries() {
    let (config_pages, recipe_pages, references) = fixtures();
    let mut single_page = recipe_pages[0].clone();
    let mut second_recipe = single_page.recipes[0].clone();
    second_recipe.recipe_id = 9001;
    second_recipe.material_id = 17002;
    single_page.count = 2;
    single_page.total_count = 2;
    single_page.recipes.push(second_recipe);

    let mut first_page = single_page.clone();
    let mut second_page = single_page.clone();
    first_page.count = 1;
    first_page.next_index = Some(1);
    first_page.recipes.truncate(1);
    second_page.start_index = 1;
    second_page.count = 1;
    second_page.next_index = None;
    second_page.recipes = vec![single_page.recipes[1].clone()];

    let original = map_equipment_catalog(
        &config_pages,
        2,
        &[single_page],
        2,
        &references,
        MODULE_SHA256,
    )
    .expect("单页配方目录应能映射");
    let repaged = map_equipment_catalog(
        &config_pages,
        2,
        &[first_page, second_page],
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("相同配方目录拆页后仍应能映射");

    assert_eq!(original, repaged);
    assert_eq!(
        original.source().content_sha256(),
        repaged.source().content_sha256()
    );
}

#[test]
fn content_digest_ignores_unmapped_detail_values() {
    let (config_pages, recipe_pages, references) = fixtures();
    let original = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("原始目录应能映射");

    for (field, value) in [
        ("properties", json!({"cannon": 1.25})),
        ("skill", json!({"id": 7000, "level": 1})),
        ("property_rate", json!({"cannon": 0.75})),
    ] {
        let mut changed_pages = config_pages.clone();
        match field {
            "properties" => changed_pages[0].configs[0].properties = value,
            "skill" => changed_pages[0].configs[0].skill = value,
            "property_rate" => changed_pages[0].configs[0].property_rate = value,
            _ => unreachable!("测试字段集合是固定的"),
        }
        let changed = map_equipment_catalog(
            &changed_pages,
            2,
            &recipe_pages,
            1,
            &references,
            MODULE_SHA256,
        )
        .expect("详情原始值变化不应影响核心目录映射");

        assert_eq!(original, changed, "未映射字段 {field} 不应改变核心目录");
        assert_eq!(
            original.source().content_sha256(),
            changed.source().content_sha256(),
            "未映射字段 {field} 不应改变核心目录摘要"
        );
    }
}

#[test]
fn content_digest_changes_when_a_mapped_record_changes() {
    let (config_pages, recipe_pages, references) = fixtures();
    let mut changed_pages = config_pages.clone();
    changed_pages[0].configs[0].raw_config["descrip"] = json!("另一份测试说明");

    let original = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("原始目录应能映射");
    let changed = map_equipment_catalog(
        &changed_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("内容变化后的目录仍应能映射");

    assert_ne!(
        original.source().content_sha256(),
        changed.source().content_sha256()
    );
}

#[test]
fn false_attribute_slots_do_not_change_the_catalog() {
    let (config_pages, recipe_pages, references) = fixtures();
    let mut pages_with_empty_slots = config_pages.clone();
    for config in &mut pages_with_empty_slots[0].configs {
        let configured_attribute = config.attributes[0].clone();
        config.attributes = json!([false, configured_attribute, false]);
    }

    let original = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("仅包含已配置属性的目录应能映射");
    let with_empty_slots = map_equipment_catalog(
        &pages_with_empty_slots,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("Equipment:GetAttributes 返回的 false 空槽应被忽略");

    assert_eq!(original, with_empty_slots);
    assert_eq!(
        original.source().content_sha256(),
        with_empty_slots.source().content_sha256()
    );
}

#[test]
fn mapping_rejects_true_attribute_slots() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[0].attributes = json!([true]);

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );

    assert!(matches!(
        result,
        Err(EquipmentMappingError::InvalidConfigField {
            config_id: 1000,
            field: "attributes",
            ..
        })
    ));
}

#[test]
fn mapping_rejects_cross_page_identifier_reordering() {
    let (config_pages, recipe_pages, references) = fixtures();
    let mut first_page = config_pages[0].clone();
    let mut second_page = config_pages[0].clone();
    first_page.count = 1;
    first_page.next_index = Some(1);
    first_page.configs = vec![config_pages[0].configs[1].clone()];
    second_page.start_index = 1;
    second_page.count = 1;
    second_page.next_index = None;
    second_page.configs = vec![config_pages[0].configs[0].clone()];
    let config_pages = vec![first_page, second_page];

    let result = map_equipment_catalog(
        &config_pages,
        1,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::CatalogOrderInvalid {
            kind: "装备配置",
            previous: 1001,
            actual: 1000,
        })
    ));
}

#[test]
fn mapping_rejects_family_without_its_root_config() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs.remove(0);
    config_pages[0].count = 1;
    config_pages[0].total_count = 1;

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::FamilyChainInvalid {
            family_id: 1000,
            ..
        })
    ));
}

#[test]
fn mapping_keeps_distinct_enhancement_roots_that_share_a_group() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    let mut second_root = config_pages[0].configs[0].clone();
    second_root.config_id = 2000;
    second_root.root_config_id = Some(2000);
    second_root.raw_config["group"] = json!(1000);
    second_root.raw_config["base"] = Value::Null;
    second_root.raw_config["prev"] = Value::Null;
    second_root.raw_config["next"] = json!(2001);

    let mut second_level = config_pages[0].configs[1].clone();
    second_level.config_id = 2001;
    second_level.root_config_id = Some(2000);
    second_level.raw_config["group"] = json!(1000);
    second_level.raw_config["base"] = json!(2000);
    second_level.raw_config["prev"] = json!(2000);
    second_level.raw_config["next"] = Value::Null;

    config_pages[0].count = 4;
    config_pages[0].total_count = 4;
    config_pages[0].configs.extend([second_root, second_level]);

    let catalog = map_equipment_catalog(
        &config_pages,
        4,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("共享客户端 group 的独立强化链应分别映射");

    assert_eq!(catalog.families().len(), 2);
    for family_id in [1000, 2000] {
        let family_id = EquipmentFamilyId::new(family_id).expect("装备族 ID 应有效");
        let family = catalog.family(family_id).expect("应保留独立强化族");
        assert_eq!(family.family_id(), family_id);
        assert_eq!(family.configs().len(), 2);
    }
}

#[test]
fn mapping_rejects_runtime_root_that_conflicts_with_base() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[1].root_config_id = Some(2000);

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::RootConfigMismatch {
            config_id: 1001,
            field: "base",
            expected: 2000,
            actual: Some(1000),
        })
    ));
}

#[test]
fn mapping_rejects_invalid_group() {
    for invalid_group in [json!(0), json!(-1), json!("1000")] {
        let (mut config_pages, recipe_pages, references) = fixtures();
        config_pages[0].configs[0].raw_config["group"] = invalid_group;

        let result = map_equipment_catalog(
            &config_pages,
            2,
            &recipe_pages,
            1,
            &references,
            MODULE_SHA256,
        );
        assert!(matches!(
            result,
            Err(EquipmentMappingError::InvalidConfigField {
                config_id: 1000,
                field: "group",
                ..
            })
        ));
    }
}

#[test]
fn mapping_rejects_base_outside_family() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[1].raw_config["base"] = json!(9999);

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::RootConfigMismatch {
            config_id: 1001,
            field: "base",
            expected: 1000,
            actual: Some(9999),
        })
    ));
}

#[test]
fn mapping_rejects_base_on_root_config() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[0].raw_config["base"] = json!(1000);

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::RootConfigMismatch {
            config_id: 1000,
            field: "base",
            expected: 1000,
            actual: Some(1000),
        })
    ));
}

#[test]
fn mapping_rejects_broken_enhancement_links() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[0].raw_config["next"] = json!(9999);

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::FamilyChainInvalid {
            family_id: 1000,
            ..
        })
    ));
}

#[test]
fn mapping_rejects_recipe_target_outside_catalog() {
    let (config_pages, mut recipe_pages, references) = fixtures();
    recipe_pages[0].recipes[0].equipment_id = 9999;

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::RecipeTargetMissing {
            recipe_id: 9000,
            equipment_id: 9999,
        })
    ));
}

#[test]
fn mapping_rejects_zero_equipment_type_but_allows_zero_nation() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[0].raw_config["type"] = json!(0);

    let result = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    );
    assert!(matches!(
        result,
        Err(EquipmentMappingError::InvalidConfigField {
            config_id: 1000,
            field: "type",
            ..
        })
    ));
}

#[test]
fn mapping_accepts_zero_importance() {
    let (mut config_pages, recipe_pages, references) = fixtures();
    config_pages[0].configs[0].raw_config["important"] = json!(0);

    let catalog = map_equipment_catalog(
        &config_pages,
        2,
        &recipe_pages,
        1,
        &references,
        MODULE_SHA256,
    )
    .expect("客户端允许装备重要程度为零");
    let family = catalog
        .family(EquipmentFamilyId::new(1000).expect("装备族 ID 应有效"))
        .expect("应找到测试装备族");

    assert_eq!(family.configs()[0].importance(), 0);
}

#[test]
fn mapping_rejects_invalid_importance() {
    for invalid_importance in [json!(-1), json!(1.5), json!("1")] {
        let (mut config_pages, recipe_pages, references) = fixtures();
        config_pages[0].configs[0].raw_config["important"] = invalid_importance;

        let result = map_equipment_catalog(
            &config_pages,
            2,
            &recipe_pages,
            1,
            &references,
            MODULE_SHA256,
        );
        assert!(matches!(
            result,
            Err(EquipmentMappingError::InvalidConfigField {
                config_id: 1000,
                field: "important",
                ..
            })
        ));
    }
}

fn fixtures() -> (
    Vec<EquipmentConfigPageResult>,
    Vec<ComposeRecipePageResult>,
    EquipmentReferenceNameBatchResult,
) {
    (
        vec![deserialize(json!({
            "schema_version": 1,
            "complete": true,
            "count": 2,
            "source": {"module_sha256": MODULE_SHA256},
            "start_index": 0,
            "total_count": 2,
            "next_index": null,
            "configs": [
                config(1000, 1, None, Some(1001)),
                config(1001, 2, Some(1000), None)
            ],
            "read_errors": []
        }))],
        vec![deserialize(json!({
            "schema_version": 1,
            "complete": true,
            "count": 1,
            "source": {"module_sha256": MODULE_SHA256},
            "start_index": 0,
            "total_count": 1,
            "next_index": null,
            "recipes": [{
                "recipe_id": 9000,
                "material_id": 17001,
                "material_count": 2,
                "gold": 20,
                "equipment_id": 1000
            }],
            "read_errors": []
        }))],
        deserialize(json!({
            "schema_version": 1,
            "complete": true,
            "count": 4,
            "source": {"module_sha256": MODULE_SHA256},
            "equipment_types": [{"equipment_type_id": 10, "name": "主炮", "error": null}],
            "nations": [{"nation_id": 0, "name": "其他", "error": null}],
            "ship_types": [{"ship_type_id": 1, "name": "驱逐舰", "error": null}],
            "attributes": [{"attribute_key": "cannon", "name": "炮击", "error": null}]
        })),
    )
}

fn config(config_id: u64, level: u64, previous: Option<u64>, next: Option<u64>) -> Value {
    json!({
        "config_id": config_id,
        "root_config_id": 1000,
        "raw_config": {
            "group": 1000,
            "name": "测试设备",
            "icon": "test-icon",
            "type": 10,
            "nationality": 0,
            "rarity": 4,
            "tech": 0,
            "speciality": "无",
            "ammo": 1,
            "torpedo_ammo": 0,
            "level": level,
            "base": (config_id != 1000).then_some(1000),
            "prev": previous,
            "next": next,
            "upgrade_formula_id": [],
            "trans_use_gold": 20,
            "trans_use_item": [[17001, 1]],
            "restore_gold": 10,
            "restore_item": [],
            "destory_gold": 5,
            "destory_item": [[17002, 1]],
            "important": 1,
            "equip_limit": 0,
            "part_main": [1],
            "part_sub": [],
            "ship_type_forbidden": [],
            "skill_id": [[7000, 1]],
            "hidden_skill_id": [8000],
            "label": ["测试"],
            "descrip": "测试说明"
        },
        "attributes": [{"key": "cannon", "value": 12.5, "aux_boost": true}],
        "properties": null,
        "skill": null,
        "property_rate": null,
        "weapon_ids": [700],
        "gear_score": 55,
        "anti_siren_power": 1.5,
        "is_device": true,
        "is_aircraft": false,
        "complete": true,
        "read_errors": []
    })
}

fn deserialize<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("测试夹具应符合冻结协议结构")
}

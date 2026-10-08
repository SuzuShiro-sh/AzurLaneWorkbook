//! 覆盖装备目录分页读取、详情聚合与维护样本的单元测试。

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    ComposeRecipePageResult, EquipmentConfigPageResult, EquipmentReadError, EquipmentReadLimits,
    EquipmentReferenceNameBatchPayload, EquipmentReferenceNameBatchResult, EquipmentRuntime,
    EquipmentWeaponBatchResult, RuntimeClientError, SkillEffectBatchResult, SkillEffectQuery,
    read_equipment_catalog_scoped,
};
use crate::adapters::device::capture::equipment_sample::write_equipment_sample;
use crate::adapters::device::session::SessionId;
use crate::adapters::tool_root::ToolRoot;
use suzushiro_content_digest::sha256_sorted_json;

const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct FakeRuntime {
    config_pages: VecDeque<EquipmentConfigPageResult>,
    recipe_pages: VecDeque<ComposeRecipePageResult>,
    reference_names: EquipmentReferenceNameBatchResult,
    config_requests: Vec<(u32, u32)>,
    recipe_requests: Vec<(u32, u32)>,
    reference_requests: Vec<EquipmentReferenceNameBatchPayload>,
    weapon_requests: Vec<Vec<u64>>,
    skill_requests: Vec<Vec<SkillEffectQuery>>,
    incomplete_weapon_id: Option<u64>,
    incomplete_skill_id: Option<u64>,
    weapon_raw_revision: u64,
    skill_raw_revision: u64,
}

impl FakeRuntime {
    fn complete() -> Self {
        let (config_pages, recipe_pages, reference_names) = fixtures();
        Self {
            config_pages: config_pages.into(),
            recipe_pages: recipe_pages.into(),
            reference_names,
            config_requests: Vec::new(),
            recipe_requests: Vec::new(),
            reference_requests: Vec::new(),
            weapon_requests: Vec::new(),
            skill_requests: Vec::new(),
            incomplete_weapon_id: None,
            incomplete_skill_id: None,
            weapon_raw_revision: 0,
            skill_raw_revision: 0,
        }
    }
}

impl EquipmentRuntime for FakeRuntime {
    fn snapshot_equipment_configs(
        &mut self,
        _timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        _expected_module_sha256: &str,
    ) -> Result<EquipmentConfigPageResult, RuntimeClientError> {
        self.config_requests.push((start_index, page_size));
        Ok(self.config_pages.pop_front().expect("应存在配置页"))
    }

    fn snapshot_compose_recipes(
        &mut self,
        _timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        _expected_module_sha256: &str,
    ) -> Result<ComposeRecipePageResult, RuntimeClientError> {
        self.recipe_requests.push((start_index, page_size));
        Ok(self.recipe_pages.pop_front().expect("应存在配方页"))
    }

    fn snapshot_equipment_reference_names(
        &mut self,
        _timeout_ms: u32,
        request: &EquipmentReferenceNameBatchPayload,
        _expected_module_sha256: &str,
    ) -> Result<EquipmentReferenceNameBatchResult, RuntimeClientError> {
        self.reference_requests.push(request.clone());
        Ok(self.reference_names.clone())
    }

    fn snapshot_equipment_weapons(
        &mut self,
        _timeout_ms: u32,
        weapon_ids: &[u64],
        _expected_module_sha256: &str,
    ) -> Result<EquipmentWeaponBatchResult, RuntimeClientError> {
        self.weapon_requests.push(weapon_ids.to_vec());
        let complete = !weapon_ids.contains(&self.incomplete_weapon_id.unwrap_or_default());
        let raw_revision = self.weapon_raw_revision;
        Ok(deserialize(json!({
            "schema_version": 1,
            "complete": complete,
            "count": weapon_ids.len(),
            "source": {"module_sha256": MODULE_SHA256},
            "weapons": weapon_ids.iter().map(|weapon_id| {
                let detail_complete = Some(*weapon_id) != self.incomplete_weapon_id;
                json!({
                "weapon_id": weapon_id,
                "raw": {"damage": weapon_id, "revision": raw_revision},
                "complete": detail_complete,
                "read_errors": if detail_complete {
                    Vec::<String>::new()
                } else {
                    vec!["读取失败".to_owned()]
                }
            })}).collect::<Vec<_>>()
        })))
    }

    fn snapshot_skill_effects(
        &mut self,
        _timeout_ms: u32,
        skills: &[SkillEffectQuery],
        _expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError> {
        self.skill_requests.push(skills.to_vec());
        let complete = !skills
            .iter()
            .any(|skill| Some(skill.skill_id) == self.incomplete_skill_id);
        let raw_revision = self.skill_raw_revision;
        Ok(deserialize(json!({
            "schema_version": 1,
            "complete": complete,
            "count": skills.len(),
            "source": {"module_sha256": MODULE_SHA256},
            "skills": skills.iter().map(|skill| {
                let detail_complete = Some(skill.skill_id) != self.incomplete_skill_id;
                json!({
                "skill_id": skill.skill_id,
                "level": skill.level,
                "display": {
                    "available": true,
                    "complete": true,
                    "value": {
                        "id": skill.skill_id,
                        "name": "测试技能",
                        "desc": "测试说明",
                        "revision": raw_revision
                    },
                    "error": null,
                    "read_errors": []
                },
                "battle_skill": {
                    "available": true,
                    "complete": detail_complete,
                    "value": {"effect_list": []},
                    "error": null,
                    "read_errors": if detail_complete {
                        Vec::<String>::new()
                    } else {
                        vec!["读取失败".to_owned()]
                    }
                },
                "battle_buff": {
                    "available": false,
                    "complete": false,
                    "value": null,
                    "error": "没有 Buff 模板",
                    "read_errors": []
                },
                "complete": detail_complete
            })}).collect::<Vec<_>>()
        })))
    }
}

#[test]
fn reader_collects_pages_deduplicates_references_and_batches_details() {
    let mut runtime = FakeRuntime::complete();
    let result = read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("完整运行态结果应能编排");

    assert_eq!(runtime.config_requests, vec![(0, 1), (1, 1)]);
    assert_eq!(runtime.recipe_requests, vec![(0, 1)]);
    assert_eq!(runtime.reference_requests.len(), 1);
    let reference_request = &runtime.reference_requests[0];
    assert_eq!(reference_request.equipment_type_ids, vec![10]);
    assert_eq!(reference_request.nation_ids, vec![0]);
    assert_eq!(reference_request.ship_type_ids, vec![1]);
    assert_eq!(reference_request.attribute_keys, vec!["cannon"]);
    assert_eq!(runtime.weapon_requests, vec![vec![101, 102], vec![103]]);
    assert_eq!(
        runtime.skill_requests,
        vec![
            vec![skill_query(201), skill_query(202)],
            vec![skill_query(203)]
        ]
    );
    assert_eq!(result.catalog().config_count(), 2);
    assert_eq!(result.raw_content_sha256().len(), 64);
    assert_eq!(result.raw_records().configs.len(), 2);
    assert_eq!(result.raw_records().recipes.len(), 1);
    assert_eq!(result.raw_records().reference_names.count, 4);
    assert_eq!(result.raw_records().weapons.len(), 3);
    assert_eq!(result.raw_records().skills.len(), 3);
    let debug = format!("{result:?}");
    assert!(debug.contains(result.raw_content_sha256()));
    assert!(!debug.contains("测试设备"));
    assert!(!debug.contains("测试技能"));
    assert!(!debug.contains("damage"));
}

#[test]
fn reader_restarts_catalog_after_an_incomplete_page() {
    let mut runtime = FakeRuntime::complete();
    let first = runtime.config_pages.pop_front().expect("应存在第一页");
    let second = runtime.config_pages.pop_front().expect("应存在第二页");
    let mut incomplete = second.clone();
    incomplete.configs[0].complete = false;
    incomplete.configs[0]
        .read_errors
        .push("暂时读取失败".to_owned());
    incomplete.complete = false;
    runtime
        .config_pages
        .extend([first.clone(), incomplete, first, second]);

    read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("短暂不完整页后应能重启整套目录读取");

    assert_eq!(
        runtime.config_requests,
        vec![(0, 1), (1, 1), (0, 1), (1, 1)]
    );
}

#[test]
fn reader_skips_detail_rpcs_when_catalog_has_no_detail_references() {
    let mut runtime = FakeRuntime::complete();
    for page in &mut runtime.config_pages {
        for config in &mut page.configs {
            config.weapon_ids.clear();
            config.raw_config["skill_id"] = json!([]);
            config.raw_config["hidden_skill_id"] = json!([]);
        }
    }

    let result = read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("没有详情引用的目录仍应完整读取");

    assert!(runtime.weapon_requests.is_empty());
    assert!(runtime.skill_requests.is_empty());
    assert!(result.raw_records().weapons.is_empty());
    assert!(result.raw_records().skills.is_empty());
}

#[test]
fn reader_rejects_incomplete_detail_batch() {
    let mut runtime = FakeRuntime::complete();
    runtime.incomplete_weapon_id = Some(102);

    let error = read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect_err("不完整武器批次不得发布部分目录");

    assert!(matches!(
        error,
        EquipmentReadError::IncompleteDetails(ref source)
            if source.kind() == "装备武器"
                && source.failures().len() == 1
                && source.failures()[0].key() == "102"
                && source.failures()[0].diagnostics() == ["读取失败"]
    ));
    assert_eq!(error.to_string(), "装备武器详情批次不完整，共 1 条失败记录");
    assert!(runtime.skill_requests.is_empty());
}

#[test]
fn reader_rejects_incomplete_skill_batch() {
    let mut runtime = FakeRuntime::complete();
    runtime.incomplete_skill_id = Some(202);

    let error = read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect_err("不完整技能批次不得发布部分目录");

    assert!(matches!(
        error,
        EquipmentReadError::IncompleteDetails(source)
            if source.kind() == "装备技能"
                && source.failures().len() == 1
                && source.failures()[0].key() == "202:1"
                && source.failures()[0].diagnostics() == ["战斗技能：读取失败"]
    ));
    assert_eq!(
        runtime.skill_requests,
        vec![vec![skill_query(201), skill_query(202)]]
    );
}

#[test]
fn reader_rejects_invalid_page_cursor_before_requesting_another_page() {
    let mut runtime = FakeRuntime::complete();
    runtime.config_pages[0].next_index = Some(0);

    let error = read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect_err("坏游标必须在继续翻页前失败");

    assert!(matches!(
        error,
        EquipmentReadError::Protocol(source)
            if source.code == "catalog_next_index_invalid"
    ));
    assert_eq!(runtime.config_requests, vec![(0, 1)]);
    assert!(runtime.recipe_requests.is_empty());
}

#[test]
fn raw_digest_is_independent_from_page_and_batch_boundaries() {
    let mut paged_runtime = FakeRuntime::complete();
    let paged = read_equipment_catalog_scoped(
        &mut paged_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 1,
            skill_batch_size: 1,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("逐条分页和分片应能完整读取");

    let mut combined_runtime = FakeRuntime::complete();
    combine_config_pages(&mut combined_runtime);
    let combined = read_equipment_catalog_scoped(
        &mut combined_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 2,
            weapon_batch_size: 3,
            skill_batch_size: 3,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("合并分页和分片应能完整读取");

    assert_eq!(paged.raw_content_sha256(), combined.raw_content_sha256());
}

#[test]
fn raw_digest_covers_config_weapon_and_skill_runtime_values() {
    let mut original_runtime = FakeRuntime::complete();
    let original = read_equipment_catalog_scoped(
        &mut original_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("原始目录应能完整读取");

    let mut changed_config_runtime = FakeRuntime::complete();
    changed_config_runtime.config_pages[0].configs[0].properties = json!({"range": 99});
    let changed_config = read_equipment_catalog_scoped(
        &mut changed_config_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("合法未映射字段变化后仍应能完整读取");

    let mut changed_weapon_runtime = FakeRuntime::complete();
    changed_weapon_runtime.weapon_raw_revision = 1;
    let changed_weapon = read_equipment_catalog_scoped(
        &mut changed_weapon_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("合法武器原始值变化后仍应能完整读取");

    let mut changed_skill_runtime = FakeRuntime::complete();
    changed_skill_runtime.skill_raw_revision = 1;
    let changed_skill = read_equipment_catalog_scoped(
        &mut changed_skill_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("合法技能原始值变化后仍应能完整读取");

    for changed in [&changed_config, &changed_weapon, &changed_skill] {
        assert_eq!(
            original.catalog().source().content_sha256(),
            changed.catalog().source().content_sha256()
        );
        assert_ne!(original.raw_content_sha256(), changed.raw_content_sha256());
    }
}

#[test]
fn raw_digest_covers_recipes_and_reference_names() {
    let mut original_runtime = FakeRuntime::complete();
    let original = read_equipment_catalog_scoped(
        &mut original_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("原始目录应能完整读取");

    let mut changed_recipe_runtime = FakeRuntime::complete();
    changed_recipe_runtime.recipe_pages[0].recipes[0].gold = 21;
    let changed_recipe = read_equipment_catalog_scoped(
        &mut changed_recipe_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("合法配方变化后仍应能完整读取");

    let mut changed_reference_runtime = FakeRuntime::complete();
    changed_reference_runtime.reference_names.equipment_types[0].name = Some("舰炮".to_owned());
    let changed_reference = read_equipment_catalog_scoped(
        &mut changed_reference_runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("合法引用名称变化后仍应能完整读取");

    for changed in [&changed_recipe, &changed_reference] {
        assert_ne!(
            original.catalog().source().content_sha256(),
            changed.catalog().source().content_sha256()
        );
        assert_ne!(original.raw_content_sha256(), changed.raw_content_sha256());
    }
}

#[test]
fn maintenance_sample_publishes_recomputable_raw_and_normalized_documents() {
    let mut runtime = FakeRuntime::complete();
    let result = read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .expect("完整目录应能生成维护样本");
    let fixture = TestDirectory::new();
    let tool_root = ToolRoot::open(&fixture.root).unwrap();
    let session_id: SessionId = "00000000000000000000000000000001".parse().unwrap();

    let evidence = write_equipment_sample(&tool_root, session_id, &result).unwrap();
    let sample_bytes = fs::read(fixture.root.join(&evidence.relative_path)).unwrap();
    let sample: Value = serde_json::from_slice(&sample_bytes).unwrap();

    assert_eq!(sample_bytes.len() as u64, evidence.size_bytes);
    assert_eq!(
        suzushiro_content_digest::sha256_file(&fixture.root.join(&evidence.relative_path)).unwrap(),
        evidence.file_sha256
    );
    assert_eq!(sample["schema_version"], 1);
    assert_eq!(sample["session_id"], "00000000000000000000000000000001");
    assert_eq!(sample["digest_algorithm"], "sha256");
    assert_eq!(sample["digest_encoding"], "sorted_keys_compact_json");
    assert_eq!(sample["raw"]["schema_version"], 2);
    assert_eq!(sample["normalized"]["schema_version"], 2);
    assert_eq!(sample["counts"]["family_count"], 1);
    assert_eq!(sample["counts"]["config_count"], 2);
    assert_eq!(sample["counts"]["recipe_count"], 1);
    assert_eq!(sample["counts"]["reference_count"], 4);
    assert_eq!(sample["counts"]["weapon_count"], 3);
    assert_eq!(sample["counts"]["skill_count"], 3);
    assert_eq!(sample["raw"]["configs"].as_array().unwrap().len(), 2);
    assert_eq!(sample["raw"]["recipes"].as_array().unwrap().len(), 1);
    assert_eq!(sample["raw"]["reference_names"]["count"], 4);
    assert_eq!(
        sample["normalized"]["families"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        sha256_sorted_json(&sample["raw"]).unwrap(),
        evidence.raw_content_sha256
    );
    assert_eq!(
        sha256_sorted_json(&sample["normalized"]).unwrap(),
        evidence.catalog_content_sha256
    );
    assert_eq!(sample["raw_content_sha256"], evidence.raw_content_sha256);
    assert_eq!(
        sample["catalog_content_sha256"],
        evidence.catalog_content_sha256
    );
}

fn combine_config_pages(runtime: &mut FakeRuntime) {
    let mut first = runtime.config_pages.pop_front().expect("应存在第一页");
    let second = runtime.config_pages.pop_front().expect("应存在第二页");
    first.configs.extend(second.configs);
    first.count = first.configs.len() as u32;
    first.next_index = None;
    runtime.config_pages.push_back(first);
}

fn skill_query(skill_id: u64) -> SkillEffectQuery {
    SkillEffectQuery::new(skill_id, 1).expect("测试技能键应有效")
}

fn fixtures() -> (
    Vec<EquipmentConfigPageResult>,
    Vec<ComposeRecipePageResult>,
    EquipmentReferenceNameBatchResult,
) {
    (
        vec![
            deserialize(json!({
                "schema_version": 1,
                "complete": true,
                "count": 1,
                "source": {"module_sha256": MODULE_SHA256},
                "start_index": 0,
                "total_count": 2,
                "next_index": 1,
                "configs": [config(1000, 1, None, Some(1001))],
                "read_errors": []
            })),
            deserialize(json!({
                "schema_version": 1,
                "complete": true,
                "count": 1,
                "source": {"module_sha256": MODULE_SHA256},
                "start_index": 1,
                "total_count": 2,
                "next_index": null,
                "configs": [config(1001, 2, Some(1000), None)],
                "read_errors": []
            })),
        ],
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
            "skill_id": [[201, 1], [202, 1]],
            "hidden_skill_id": [203],
            "label": ["测试"],
            "descrip": "测试说明"
        },
        "attributes": [{"key": "cannon", "value": 12.5, "aux_boost": true}],
        "properties": null,
        "skill": null,
        "property_rate": null,
        "weapon_ids": [101, 102, 103],
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

struct TestDirectory {
    root: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).expect("测试需要操作系统随机源");
        let root = home
            .join("suzushiro/scratch/azlw-equipment-sample-tests")
            .join(format!(
                "{}-{:016x}",
                std::process::id(),
                u64::from_le_bytes(random)
            ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn template_scope_controls_each_equipment_detail_batch() {
    for (weapons, skills) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut runtime = FakeRuntime::complete();
        if !weapons {
            runtime.incomplete_weapon_id = Some(101);
        }
        if !skills {
            runtime.incomplete_skill_id = Some(201);
        }
        let scope = crate::domain::GameReadScope::full().with_equipment_details(weapons, skills);
        let result = super::read_equipment_catalog_scoped(
            &mut runtime,
            5_000,
            MODULE_SHA256,
            EquipmentReadLimits {
                page_size: 1,
                weapon_batch_size: 2,
                skill_batch_size: 2,
            },
            scope,
            None,
        )
        .unwrap();
        assert_eq!(result.weapons_read(), weapons);
        assert_eq!(result.skills_read(), skills);
        assert_eq!(runtime.weapon_requests.len(), if weapons { 2 } else { 0 });
        assert_eq!(runtime.skill_requests.len(), if skills { 2 } else { 0 });
        assert_eq!(runtime.config_requests, vec![(0, 1), (1, 1)]);
        assert_eq!(runtime.recipe_requests, vec![(0, 1)]);
        assert_eq!(result.catalog().config_count(), 2);
    }
}

#[test]
fn cached_equipment_details_are_filled_once_without_rereading_core_catalog() {
    let mut runtime = FakeRuntime::complete();
    let mut cache = None;
    let mut full_hash = None;
    for (weapons, skills, expected_weapons, expected_skills) in [
        (false, false, false, false),
        (true, false, true, false),
        (true, true, true, true),
        (false, false, true, true),
    ] {
        let scope = crate::domain::GameReadScope::full().with_equipment_details(weapons, skills);
        let result = super::read_equipment_catalog_scoped(
            &mut runtime,
            5_000,
            MODULE_SHA256,
            EquipmentReadLimits {
                page_size: 1,
                weapon_batch_size: 2,
                skill_batch_size: 2,
            },
            scope,
            cache,
        )
        .unwrap();
        assert_eq!(result.weapons_read(), expected_weapons);
        assert_eq!(result.skills_read(), expected_skills);
        assert_eq!(runtime.config_requests, vec![(0, 1), (1, 1)]);
        assert_eq!(runtime.recipe_requests, vec![(0, 1)]);
        assert_eq!(runtime.reference_requests.len(), 1);
        assert_eq!(
            runtime.weapon_requests.len(),
            if expected_weapons { 2 } else { 0 }
        );
        assert_eq!(
            runtime.skill_requests.len(),
            if expected_skills { 2 } else { 0 }
        );
        if expected_weapons && expected_skills {
            if let Some(hash) = full_hash.as_ref() {
                assert_eq!(result.raw_content_sha256(), hash);
            }
            full_hash = Some(result.raw_content_sha256().to_owned());
        }
        cache = Some(result);
    }
    let mut fresh = FakeRuntime::complete();
    let full = read_equipment_catalog_scoped(
        &mut fresh,
        5_000,
        MODULE_SHA256,
        EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        },
        crate::domain::GameReadScope::full(),
        None,
    )
    .unwrap();
    assert_eq!(cache.unwrap(), full);
}

#[test]
fn failed_detail_cache_expansion_is_not_returned_as_complete() {
    for weapons in [false, true] {
        let mut runtime = FakeRuntime::complete();
        let limits = EquipmentReadLimits {
            page_size: 1,
            weapon_batch_size: 2,
            skill_batch_size: 2,
        };
        let compact = crate::domain::GameReadScope::full().with_equipment_details(false, false);
        let cache = super::read_equipment_catalog_scoped(
            &mut runtime,
            5_000,
            MODULE_SHA256,
            limits,
            compact,
            None,
        )
        .unwrap();
        if weapons {
            runtime.incomplete_weapon_id = Some(101);
        } else {
            runtime.incomplete_skill_id = Some(201);
        }
        let error = super::read_equipment_catalog_scoped(
            &mut runtime,
            5_000,
            MODULE_SHA256,
            limits,
            compact.with_equipment_details(weapons, !weapons),
            Some(cache),
        )
        .unwrap_err();
        assert!(matches!(error, EquipmentReadError::IncompleteDetails(_)));
        assert_eq!(runtime.reference_requests.len(), 1);
    }
}

#[test]
fn cached_equipment_from_a_different_module_is_rejected_before_queries() {
    let mut runtime = FakeRuntime::complete();
    let limits = EquipmentReadLimits {
        page_size: 1,
        weapon_batch_size: 2,
        skill_batch_size: 2,
    };
    let compact = crate::domain::GameReadScope::full().with_equipment_details(false, false);
    let cache = super::read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        limits,
        compact,
        None,
    )
    .unwrap();
    let error = super::read_equipment_catalog_scoped(
        &mut runtime,
        5_000,
        &"f".repeat(64),
        limits,
        crate::domain::GameReadScope::full(),
        Some(cache),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("缓存装备目录与当前模块身份不一致")
    );
    assert_eq!(runtime.reference_requests.len(), 1);
    assert!(runtime.weapon_requests.is_empty());
    assert!(runtime.skill_requests.is_empty());
}

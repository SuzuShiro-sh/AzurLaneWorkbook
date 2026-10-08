//! 覆盖固定白名单目录的分页重启、跨页一致性和稳定摘要。

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{
    ShipCatalogPageResult, ShipCatalogReadError, ShipCatalogRuntime, ShipCatalogTableKey,
    read_ship_catalog_with,
};
use crate::adapters::device::runtime::{
    MAX_SHIP_CATALOG_PAGE_SIZE, RuntimeClientError, RuntimeProtocolError,
};
use crate::adapters::device::{
    capture::ship_catalog::{ShipCatalogCaptureRequest, write_ship_catalog_capture},
    session::SessionId,
};
use suzushiro_content_digest::{sha256_file, sha256_sorted_json};

const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct FakeRuntime {
    pages: BTreeMap<ShipCatalogTableKey, VecDeque<ShipCatalogPageResult>>,
    requests: Vec<(ShipCatalogTableKey, u32, u32)>,
}

impl FakeRuntime {
    fn complete() -> Self {
        let pages = ShipCatalogTableKey::ALL
            .into_iter()
            .enumerate()
            .map(|(index, table_key)| {
                let id = 1_000 + index as u64;
                (table_key, VecDeque::from([complete_page(table_key, id)]))
            })
            .collect();
        Self {
            pages,
            requests: Vec::new(),
        }
    }
}

impl ShipCatalogRuntime for FakeRuntime {
    fn snapshot_ship_catalog(
        &mut self,
        _timeout_ms: u32,
        table_key: ShipCatalogTableKey,
        start_index: u32,
        page_size: u32,
        _expected_module_sha256: &str,
    ) -> Result<ShipCatalogPageResult, RuntimeClientError> {
        self.requests.push((table_key, start_index, page_size));
        Ok(self
            .pages
            .get_mut(&table_key)
            .expect("固定表必须有响应队列")
            .pop_front()
            .expect("固定表响应队列不得提前耗尽"))
    }
}

#[test]
fn reads_every_whitelisted_table_in_stable_order() {
    let mut runtime = FakeRuntime::complete();
    let result = read_ship_catalog_with(&mut runtime, 5_000, MODULE_SHA256, 32)
        .expect("完整静态目录应读取成功");

    assert_eq!(result.module_sha256(), MODULE_SHA256);
    assert_eq!(result.tables().len(), ShipCatalogTableKey::ALL.len());
    assert_eq!(result.record_count(), ShipCatalogTableKey::ALL.len());
    assert_eq!(
        result
            .tables()
            .iter()
            .map(|table| table.table_key())
            .collect::<Vec<_>>(),
        ShipCatalogTableKey::ALL
    );
    assert_eq!(runtime.requests.len(), ShipCatalogTableKey::ALL.len());
    assert_eq!(result.content_sha256().len(), 64);
}

#[test]
fn restarts_a_table_after_a_transient_incomplete_page() {
    let mut runtime = FakeRuntime::complete();
    let table_key = ShipCatalogTableKey::ShipDataGroup;
    runtime.pages.insert(
        table_key,
        VecDeque::from([
            deserialize(json!({
                "table_key": table_key,
                "source": {"module_sha256": MODULE_SHA256},
                "start_index": 0,
                "total_count": 1,
                "next_index": null,
                "records": [],
                "read_errors": [{
                    "catalog_index": 0,
                    "id": 1000,
                    "code": "ship_catalog_record_incomplete",
                    "message": "fixture incomplete"
                }],
                "complete": false
            })),
            complete_page(table_key, 1000),
        ]),
    );

    read_ship_catalog_with(&mut runtime, 5_000, MODULE_SHA256, 32)
        .expect("短暂不完整页后应从表首重读");
    assert_eq!(
        runtime
            .requests
            .iter()
            .filter(|(table, _, _)| *table == table_key)
            .copied()
            .collect::<Vec<_>>(),
        [(table_key, 0, 32), (table_key, 0, 32)]
    );
}

#[test]
fn rejects_duplicate_record_ids_across_pages() {
    let mut runtime = FakeRuntime::complete();
    let table_key = ShipCatalogTableKey::ShipDataGroup;
    runtime.pages.insert(
        table_key,
        VecDeque::from([
            page(table_key, 0, 2, Some(1), 1000, 0),
            page(table_key, 1, 2, None, 1000, 1),
        ]),
    );

    let error = read_ship_catalog_with(&mut runtime, 5_000, MODULE_SHA256, 1)
        .expect_err("跨页重复 ID 不得进入完整目录");
    assert!(matches!(
        error,
        ShipCatalogReadError::DuplicateRecord { table_key: actual, id: 1000 }
            if actual == table_key
    ));
}

#[test]
fn rejects_total_count_drift_before_reading_another_page() {
    let mut runtime = FakeRuntime::complete();
    let table_key = ShipCatalogTableKey::ShipDataGroup;
    runtime.pages.insert(
        table_key,
        VecDeque::from([
            page(table_key, 0, 2, Some(1), 1000, 0),
            page(table_key, 1, 3, Some(2), 1001, 1),
        ]),
    );

    let error = read_ship_catalog_with(&mut runtime, 5_000, MODULE_SHA256, 1)
        .expect_err("同表分页总数漂移必须立即失败");
    assert!(matches!(
        error,
        ShipCatalogReadError::TotalCountChanged {
            table_key: actual,
            expected: 2,
            actual: 3,
        } if actual == table_key
    ));
    assert_eq!(runtime.requests.len(), 2);
}

#[test]
fn content_digest_is_stable_and_changes_with_raw_values() {
    let mut first_runtime = FakeRuntime::complete();
    let first = read_ship_catalog_with(&mut first_runtime, 5_000, MODULE_SHA256, 32)
        .expect("第一份目录应成功");
    let mut second_runtime = FakeRuntime::complete();
    let second = read_ship_catalog_with(&mut second_runtime, 5_000, MODULE_SHA256, 32)
        .expect("相同目录应成功");
    assert_eq!(first.content_sha256(), second.content_sha256());

    let mut changed_runtime = FakeRuntime::complete();
    changed_runtime
        .pages
        .get_mut(&ShipCatalogTableKey::SkillDataDisplay)
        .expect("技能展示表应存在")[0]
        .records[0]
        .raw["revision"] = json!(1);
    let changed = read_ship_catalog_with(&mut changed_runtime, 5_000, MODULE_SHA256, 32)
        .expect("合法原始字段变化后目录应成功");
    assert_ne!(first.content_sha256(), changed.content_sha256());
}

#[test]
fn external_capture_is_recomputable_and_stays_outside_the_tool_root() {
    let mut runtime = FakeRuntime::complete();
    let result = read_ship_catalog_with(&mut runtime, 5_000, MODULE_SHA256, 32)
        .expect("完整目录应能形成捕获输入");
    let directory = TestDirectory::new();
    let session_id: SessionId = "00000000000000000000000000000001".parse().unwrap();
    let request =
        ShipCatalogCaptureRequest::new(&directory.tool_root, &directory.capture_root, session_id)
            .expect("外部捕获根应通过边界校验");
    let evidence = write_ship_catalog_capture(&request, &result).expect("外部捕获应原子发布");
    let bytes = fs::read(&evidence.path).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();

    let canonical_capture_root = fs::canonicalize(&directory.capture_root).unwrap();
    let canonical_tool_root = fs::canonicalize(&directory.tool_root).unwrap();
    assert!(evidence.path.starts_with(canonical_capture_root));
    assert!(!evidence.path.starts_with(canonical_tool_root));
    assert_eq!(evidence.size_bytes, bytes.len() as u64);
    assert_eq!(evidence.file_sha256, sha256_file(&evidence.path).unwrap());
    assert_eq!(evidence.table_count, ShipCatalogTableKey::ALL.len() as u32);
    assert_eq!(evidence.record_count, ShipCatalogTableKey::ALL.len() as u32);
    assert_eq!(document["capture_schema_version"], 1);
    assert_eq!(document["catalog"]["schema_version"], 1);
    assert_eq!(
        sha256_sorted_json(&document["catalog"]).unwrap(),
        evidence.content_sha256
    );
    assert_eq!(document["module_sha256"], MODULE_SHA256);
}

#[test]
fn runtime_page_error_includes_table_and_start_index() {
    struct FailingRuntime;

    impl ShipCatalogRuntime for FailingRuntime {
        fn snapshot_ship_catalog(
            &mut self,
            _timeout_ms: u32,
            _table_key: ShipCatalogTableKey,
            _start_index: u32,
            _page_size: u32,
            _expected_module_sha256: &str,
        ) -> Result<ShipCatalogPageResult, RuntimeClientError> {
            Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "fixture_failure",
                "fixture read failed",
            )))
        }
    }

    let error = read_ship_catalog_with(&mut FailingRuntime, 5_000, MODULE_SHA256, 32)
        .expect_err("运行态页失败必须携带目录上下文");
    assert!(matches!(
        &error,
        ShipCatalogReadError::PageRuntime {
            table_key: ShipCatalogTableKey::ShipDataGroup,
            start_index: 0,
            ..
        }
    ));
    assert!(error.to_string().contains("ship_data_group"));
    assert!(error.to_string().contains("start_index=0"));
}

fn complete_page(table_key: ShipCatalogTableKey, id: u64) -> ShipCatalogPageResult {
    page(table_key, 0, 1, None, id, 0)
}

fn page(
    table_key: ShipCatalogTableKey,
    start_index: u32,
    total_count: u32,
    next_index: Option<u32>,
    id: u64,
    revision: u64,
) -> ShipCatalogPageResult {
    deserialize(json!({
        "table_key": table_key,
        "source": {"module_sha256": MODULE_SHA256},
        "start_index": start_index,
        "total_count": total_count,
        "next_index": next_index,
        "records": [{"id": id, "raw": {"id": id, "revision": revision}}],
        "read_errors": [],
        "complete": true
    }))
}

fn deserialize<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("测试夹具应符合冻结协议结构")
}

struct TestDirectory {
    root: PathBuf,
    tool_root: PathBuf,
    capture_root: PathBuf,
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
            .join("suzushiro/scratch/azlw-ship-catalog-tests")
            .join(format!(
                "{}-{:016x}",
                std::process::id(),
                u64::from_le_bytes(random)
            ));
        let tool_root = root.join("tool");
        let capture_root = root.join("capture");
        fs::create_dir_all(&tool_root).unwrap();
        fs::create_dir_all(&capture_root).unwrap();
        Self {
            root,
            tool_root,
            capture_root,
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cached_static_catalog_skips_repeat_pages_and_rejects_a_different_module() {
    let mut runtime = FakeRuntime::complete();
    let cold_started = std::time::Instant::now();
    let cached = read_ship_catalog_with(&mut runtime, 5_000, MODULE_SHA256, 32).unwrap();
    let cold_us = cold_started.elapsed().as_micros();
    let static_requests = runtime.requests.len();
    assert_eq!(static_requests, ShipCatalogTableKey::ALL.len());

    runtime.requests.clear();
    let warm_started = std::time::Instant::now();
    let reused = super::read_ship_catalog_scoped_with(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        false,
        Some(cached.clone()),
    )
    .unwrap();
    let warm_us = warm_started.elapsed().as_micros();
    assert!(runtime.requests.is_empty());
    assert_eq!(reused.content_sha256(), cached.content_sha256());
    assert_eq!(reused.tables().len(), ShipCatalogTableKey::ALL.len());

    for key in ShipCatalogTableKey::TECHNOLOGY {
        let page = if key == ShipCatalogTableKey::CollectionShipGroup {
            deserialize(json!({
                "table_key": key,
                "source": {"module_sha256": MODULE_SHA256},
                "start_index": 0,
                "total_count": 0,
                "next_index": null,
                "records": [],
                "read_errors": [],
                "complete": true
            }))
        } else {
            complete_page(key, 2_000)
        };
        runtime.pages.insert(key, VecDeque::from([page]));
    }
    let technology_started = std::time::Instant::now();
    let with_technology = super::read_ship_catalog_scoped_with(
        &mut runtime,
        5_000,
        MODULE_SHA256,
        true,
        Some(cached.clone()),
    )
    .unwrap();
    assert_eq!(
        runtime.requests,
        ShipCatalogTableKey::TECHNOLOGY
            .into_iter()
            .map(|key| (key, 0, MAX_SHIP_CATALOG_PAGE_SIZE))
            .collect::<Vec<_>>()
    );
    let technology_us = technology_started.elapsed().as_micros();
    assert_eq!(with_technology.tables().len(), 21);
    assert_ne!(with_technology.content_sha256(), cached.content_sha256());
    if std::env::var_os("AZLW_MEASURE_MODE").is_some() {
        println!(
            "\nMEASURE stage=ship_catalog_pages static_tables={} static_requests={static_requests} cache_requests=0 technology_requests={} tables_with_technology={} page_size={} records={} cold_us={cold_us} warm_us={warm_us} technology_us={technology_us} note=synthetic_one_page_per_table json_bytes=not_wire_bytes",
            ShipCatalogTableKey::ALL.len(),
            ShipCatalogTableKey::TECHNOLOGY.len(),
            with_technology.tables().len(),
            MAX_SHIP_CATALOG_PAGE_SIZE,
            cached.record_count()
        );
    }

    runtime.requests.clear();
    let error = super::read_ship_catalog_scoped_with(
        &mut runtime,
        5_000,
        &"f".repeat(64),
        true,
        Some(cached),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("缓存舰船静态目录与当前模块身份不一致")
    );
    assert!(runtime.requests.is_empty());
}

#[test]
fn technology_tables_are_requested_only_with_the_scope_and_empty_history_is_valid() {
    for requested in [false, true] {
        let mut runtime = FakeRuntime::complete();
        if requested {
            for key in ShipCatalogTableKey::TECHNOLOGY {
                let page = if key == ShipCatalogTableKey::CollectionShipGroup {
                    deserialize(
                        json!({"table_key":key,"source":{"module_sha256":MODULE_SHA256},"start_index":0,"total_count":0,"next_index":null,"records":[],"read_errors":[],"complete":true}),
                    )
                } else {
                    complete_page(key, 1000)
                };
                runtime.pages.insert(key, VecDeque::from([page]));
            }
        }
        let result = super::read_ship_catalog_scoped_with(
            &mut runtime,
            5000,
            MODULE_SHA256,
            requested,
            None,
        )
        .unwrap();
        assert_eq!(runtime.requests.len(), 17 + if requested { 4 } else { 0 });
        assert_eq!(result.tables().len(), 17 + if requested { 4 } else { 0 });
        if requested {
            assert!(result.tables().last().unwrap().records().is_empty());
        }
    }
}

#[test]
fn technology_read_errors_remain_distinct_from_empty_history() {
    struct TechnologyFailure(FakeRuntime);
    impl ShipCatalogRuntime for TechnologyFailure {
        fn snapshot_ship_catalog(
            &mut self,
            timeout_ms: u32,
            key: ShipCatalogTableKey,
            start: u32,
            size: u32,
            hash: &str,
        ) -> Result<ShipCatalogPageResult, RuntimeClientError> {
            if ShipCatalogTableKey::TECHNOLOGY.contains(&key) {
                return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                    "fixture_failure",
                    "technology unavailable",
                )));
            }
            self.0
                .snapshot_ship_catalog(timeout_ms, key, start, size, hash)
        }
    }
    let result = super::read_ship_catalog_scoped_with(
        &mut TechnologyFailure(FakeRuntime::complete()),
        5000,
        MODULE_SHA256,
        true,
        None,
    )
    .unwrap();
    assert_eq!(result.tables().len(), 21);
    for table in &result.tables()[17..] {
        assert!(table.records().is_empty());
        assert!(
            table
                .read_error()
                .unwrap()
                .contains("technology unavailable")
        );
    }
}

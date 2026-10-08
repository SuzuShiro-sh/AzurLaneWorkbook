use super::*;
use std::path::PathBuf;

pub(super) struct Fixture(PathBuf);
impl Fixture {
    pub(super) fn new() -> Self {
        let mut token = [0_u8; 8];
        getrandom::fill(&mut token).unwrap();
        let root = PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-ship-acquisition")
        .join(format!("test-{}", u64::from_le_bytes(token)));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    pub(super) fn source(&self) -> ShipAcquisition {
        ShipAcquisition::new(ToolRoot::open(&self.0).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn response() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"parse":{"text":{"*":"<tr><td><b>其他途径</b></td><td>开发船坞</td></tr>"}}})).unwrap()
}

#[test]
fn cancelled_enrichment_stops_pending_online_queries() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let fixture = Fixture::new();
    let layout = crate::adapters::workbook::load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let projection = crate::application::project_game_state_to_workbook(
        &crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state(),
    )
    .unwrap();
    let template = projection.sheet("loadout_plan").unwrap().rows()[0]
        .values()
        .clone();
    let rows = (0..8)
        .map(|index| {
            let mut values = template.clone();
            values.insert(
                "original_name".to_owned(),
                WorkbookProjectionValue::text(format!("取消样本{index}")),
            );
            (format!("ship:{index}"), values)
        })
        .collect();
    let projection = projection
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    for initially_cancelled in [true, false] {
        let cancelled = AtomicBool::new(initially_cancelled);
        let calls = AtomicUsize::new(0);
        let error = fixture
            .source()
            .enrich_with(
                &layout,
                projection.clone(),
                Some(AcquisitionUpdatePolicy::Refresh),
                &mut |_| {},
                &|| cancelled.load(Ordering::Acquire),
                &|_| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    cancelled.store(true, Ordering::Release);
                    Ok(response())
                },
            )
            .unwrap_err();
        assert!(error.is_cancelled());
        let calls = calls.load(Ordering::Relaxed);
        if initially_cancelled {
            assert_eq!(calls, 0);
        } else {
            // 两个已开始的查询可收尾，其余名称不能再发起查询。
            assert_eq!(calls, 1);
        }
    }
}

#[test]
fn enrich_borrows_projection_when_acquisition_does_not_change() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state;
    use crate::application::project_game_state_to_workbook;

    let fixture = Fixture::new();
    let source = fixture.source();
    let layout = crate::adapters::workbook::load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let original_digest = projection.content_sha256().to_owned();
    let omitted = layout.without_ship_acquisition().unwrap();
    let AcquisitionEnrichment {
        projection: disabled,
        values: disabled_applied,
        summary: omitted_summary,
    } = source
        .enrich(&omitted, projection.clone(), None, &mut |_| {}, &|| false)
        .unwrap();
    assert_eq!(disabled.content_sha256(), original_digest);
    assert!(disabled_applied.is_empty());
    assert_eq!(
        omitted_summary.state,
        AcquisitionGenerationState::NotRequested
    );
    assert!(!omitted_summary.has_warnings());

    let cleared = projection
        .clone()
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), Vec::new())]))
        .unwrap();
    let cleared_digest = cleared.content_sha256().to_owned();
    let AcquisitionEnrichment {
        projection: empty,
        values: empty_applied,
        summary: empty_summary,
    } = source
        .enrich(
            &layout,
            cleared,
            Some(AcquisitionUpdatePolicy::Refresh),
            &mut |_| {},
            &|| false,
        )
        .unwrap();
    assert_eq!(empty.content_sha256(), cleared_digest);
    assert!(empty_applied.is_empty());
    assert_eq!(empty_summary.state, AcquisitionGenerationState::Completed);
    assert!(!empty_summary.has_warnings());

    let AcquisitionEnrichment {
        projection: updated,
        values: applied,
        ..
    } = source
        .enrich(&layout, projection, None, &mut |_| {}, &|| false)
        .unwrap();
    assert_ne!(updated.content_sha256(), original_digest);
    assert!(!applied.is_empty());
    assert!(
        applied
            .values()
            .any(|value| value.contains("资料未缓存，待更新"))
    );
    assert_ne!(updated.content_sha256(), original_digest);
}

#[test]
fn cache_has_no_expiry_and_refresh_failure_retains_evidence() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let network_failed = OnceLock::new();
    let first = source.resolve(
        "安克雷奇",
        100,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Ok(response()),
    );
    assert_eq!(first, "其他途径：开发船坞");
    assert_eq!(
        source.read_cache("安克雷奇").unwrap().unwrap().title,
        "安克雷奇"
    );
    assert!(first.stored);
    let cached_only = source.resolve(
        "安克雷奇",
        365 * 24 * 60 * 60,
        Some(AcquisitionUpdatePolicy::UseCache),
        &network_failed,
        &|_| panic!("有效缓存不联网"),
    );
    assert_eq!(cached_only, first);
    assert!(!cached_only.stored);
    let refreshed = source.resolve(
        "安克雷奇",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Err(http::FetchError::http_status(503)),
    );
    assert!(refreshed.contains("开发船坞"));
    assert!(refreshed.contains("HTTP 503"));
    assert_eq!(network_failed.get().unwrap(), "BWiki HTTP 503");
    assert_eq!(
        source.read_cache("安克雷奇").unwrap().unwrap().fetched_at,
        100
    );
    let missing = source.resolve(
        "拉菲",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| panic!("网络故障后不逐舰重复请求"),
    );
    assert!(missing.contains("本次在线查询已暂停：BWiki HTTP 503"));
    assert!(!missing.contains("请同步更新"));
    let next_sync = source.resolve(
        "拉菲",
        102,
        Some(AcquisitionUpdatePolicy::Refresh),
        &OnceLock::new(),
        &|_| Ok(response()),
    );
    assert_eq!(next_sync, "其他途径：开发船坞");
    let cached_only_missing = source.resolve("标枪", 102, None, &OnceLock::new(), &|_| {
        panic!("执行写回不联网")
    });
    assert!(cached_only_missing.contains("资料未缓存，待更新"));
    let use_cache_missing = source.resolve(
        "Z23",
        102,
        Some(AcquisitionUpdatePolicy::UseCache),
        &OnceLock::new(),
        &|_| panic!("缺缓存的使用缓存策略不联网"),
    );
    assert!(use_cache_missing.contains("资料未缓存，待更新"));
    let cached_only = source.resolve(
        "安克雷奇",
        (365 * 24 * 60 * 60) + 102,
        None,
        &OnceLock::new(),
        &|_| panic!("执行写回不联网"),
    );
    assert_eq!(cached_only, first);
}

#[test]
fn exhausted_security_rejection_preserves_cache_and_pauses_the_batch() {
    let fixture = Fixture::new();
    let source = fixture.source();
    source.resolve(
        "U-110",
        100,
        Some(AcquisitionUpdatePolicy::Refresh),
        &OnceLock::new(),
        &|_| Ok(response()),
    );
    let network_failed = OnceLock::new();
    let blocked = source.resolve(
        "DEAD MASTER",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Err(http::FetchError::http_status(567)),
    );
    assert!(blocked.contains("HTTP 567"));
    assert!(network_failed.get().is_some());
    assert!(source.read_cache("DEAD MASTER").unwrap().is_none());
    let retained = source.resolve(
        "U-110",
        102,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| panic!("重试耗尽后暂停批次"),
    );
    assert!(retained.contains("开发船坞"));
    assert!(retained.retained_previous);
    assert_eq!(source.read_cache("U-110").unwrap().unwrap().fetched_at, 100);
}

#[test]
fn page_parse_problems_stay_on_that_page_and_wording_does_not_drive_review() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let network_failed = OnceLock::new();
    let broken = source.resolve(
        "标枪",
        100,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Ok(b"not json".to_vec()),
    );
    assert!(broken.needs_review());
    assert!(broken.contains("BWiki JSON"));
    assert!(network_failed.get().is_none());
    assert_eq!(
        source.resolve(
            "拉菲",
            100,
            Some(AcquisitionUpdatePolicy::Refresh),
            &network_failed,
            &|_| Ok(response()),
        ),
        "其他途径：开发船坞"
    );

    let partial_html =
        r#"<tr><td><b>建造时间</b></td><td>00:27:00</td></tr><tr><td><b>其他途径</b></td><td"#;
    let partial_response =
        serde_json::to_vec(&serde_json::json!({"parse":{"text":{"*": partial_html}}})).unwrap();
    let cached = source.resolve(
        "鞍山",
        100,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Ok(response()),
    );
    assert!(!cached.needs_review());
    let partial = source.resolve(
        "鞍山",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Ok(partial_response.clone()),
    );
    assert_eq!(partial.status, AcquisitionStatus::Incomplete);
    assert!(partial.contains("开发船坞"));
    assert!(partial.contains("获取方式不完整"));
    assert_eq!(source.read_cache("鞍山").unwrap().unwrap().fetched_at, 100);
    assert!(network_failed.get().is_none());
    let uncached = source.resolve(
        "Z1",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Ok(partial_response.clone()),
    );
    assert_eq!(uncached.status, AcquisitionStatus::Incomplete);
    assert!(uncached.contains("建造：00:27:00"));
    assert!(source.read_cache("Z1").unwrap().is_none());

    let wording = source.resolve(
        "长门",
        102,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| {
            Ok(serde_json::to_vec(&serde_json::json!({"parse":{"text":{"*":
                "<tr><td><b>其他途径</b></td><td>未获取奖励</td></tr>"
            }}}))
            .unwrap())
        },
    );
    assert!(wording.contains("未获取"));
    assert!(!wording.needs_review());
    let paused = Acquisition::unavailable("另一套说明");
    assert_eq!(
        [wording, paused]
            .iter()
            .filter(|record| record.needs_review())
            .count(),
        1
    );

    let limited = source.resolve(
        "赤城",
        103,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| Ok(br#"{"error":{"code":"ratelimited","info":"slow down"}}"#.to_vec()),
    );
    assert!(limited.contains("BWiki API"));
    assert!(network_failed.get().unwrap().contains("ratelimited"));
    let paused_next = source.resolve(
        "加贺",
        103,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| panic!("服务级故障后不再请求下一艘"),
    );
    assert_eq!(paused_next.status, AcquisitionStatus::Unavailable);
    assert!(paused_next.contains("本次在线查询已暂停"));
    source
        .update_cache_with(
            &["标枪".to_owned()],
            AcquisitionUpdateMode::Refresh,
            &mut |_| {},
            &|| false,
            &|_| Ok(response()),
        )
        .unwrap();
    let retained = source.resolve(
        "标枪",
        104,
        Some(AcquisitionUpdatePolicy::Refresh),
        &network_failed,
        &|_| panic!("批次已经停止时不再请求"),
    );
    assert!(retained.retained_previous);
    assert!(matches!(
        retained.cache_update("标枪").outcome(),
        crate::application::AcquisitionCacheUpdateOutcome::KeptPrevious { .. }
    ));
}

#[test]
fn cache_update_reports_each_name_and_keeps_the_previous_cache() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let names = vec!["标枪".to_owned()];
    let mut visible = Vec::new();
    let updated = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Refresh,
            &mut |progress| visible.push(progress.units),
            &|| false,
            &|_| Ok(response()),
        )
        .unwrap();
    assert!(matches!(
        updated[0].outcome(),
        crate::application::AcquisitionCacheUpdateOutcome::Updated
    ));
    assert_eq!(updated[0].name(), "标枪");
    assert_eq!(visible, vec![Some((1, 1))]);
    let kept = source.read_cache("标枪").unwrap().unwrap();
    let failed = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Refresh,
            &mut |_| {},
            &|| false,
            &|_| Err(http::FetchError::http_status(503)),
        )
        .unwrap();
    assert!(matches!(
        failed[0].outcome(),
        crate::application::AcquisitionCacheUpdateOutcome::KeptPrevious { detail }
            if detail.contains("HTTP 503")
    ));
    assert_eq!(
        source.read_cache("标枪").unwrap().unwrap().fetched_at,
        kept.fetched_at
    );
    let empty = source
        .update_cache_with(
            &[],
            AcquisitionUpdateMode::Refresh,
            &mut |_| panic!("空名称不更新"),
            &|| false,
            &|_| panic!("空名称不联网"),
        )
        .unwrap();
    assert!(empty.is_empty());
    let fetches = std::sync::atomic::AtomicU32::new(0);
    let cancelled = source
        .update_cache_with(
            &["标枪".to_owned(), "拉菲".to_owned()],
            AcquisitionUpdateMode::Refresh,
            &mut |_| {},
            &|| true,
            &|_| {
                fetches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(response())
            },
        )
        .unwrap();
    assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(cancelled.iter().all(|item| {
        matches!(
            item.outcome(),
            crate::application::AcquisitionCacheUpdateOutcome::NotStarted
        )
    }));
}

#[test]
fn production_dispatch_records_requests_concurrency_stop_and_cancel() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let fixture = Fixture::new();
    let source = fixture.source();
    let names: Vec<String> = ["标枪", "拉菲", "Z23", "绫波"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let requests = AtomicUsize::new(0);
    let inflight = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let mut progress_marks = Vec::new();
    let report = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Refresh,
            &mut |progress| progress_marks.push(progress.units),
            &|| false,
            &|_| {
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                requests.fetch_add(1, Ordering::SeqCst);
                inflight.fetch_sub(1, Ordering::SeqCst);
                Ok(response())
            },
        )
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), names.len());
    assert!(peak.load(Ordering::SeqCst) >= 1);
    assert_eq!(peak.load(Ordering::SeqCst), 1);
    assert_eq!(progress_marks.len(), names.len());
    assert_eq!(
        crate::application::AcquisitionCacheUpdate::operation_terminal(&report),
        crate::application::OperationTerminal::Succeeded
    );
    let stopped = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Refresh,
            &mut |_| {},
            &|| false,
            &|title| {
                if title == "标枪" {
                    Err(http::FetchError::http_status(503))
                } else {
                    Ok(response())
                }
            },
        )
        .unwrap();
    assert!(stopped.iter().any(|item| {
        matches!(
            item.outcome(),
            crate::application::AcquisitionCacheUpdateOutcome::KeptPrevious { detail }
                if detail.contains("HTTP 503")
        )
    }));
    assert_ne!(
        crate::application::AcquisitionCacheUpdate::operation_terminal(&stopped),
        crate::application::OperationTerminal::Succeeded
    );
    let fetches = AtomicUsize::new(0);
    let cancelled = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Refresh,
            &mut |_| {},
            &|| true,
            &|_| {
                fetches.fetch_add(1, Ordering::SeqCst);
                Ok(response())
            },
        )
        .unwrap();
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
    assert_eq!(
        crate::application::AcquisitionCacheUpdate::operation_terminal(&cancelled),
        crate::application::OperationTerminal::Cancelled
    );
}

#[test]
fn malformed_cache_is_reported_without_overwriting_it() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let relative = Path::new(CACHE_DIRECTORY).join(sha256_bytes("标枪".as_bytes()));
    let directory = source.root.ensure_directory(&relative).unwrap();
    let path = directory.join("100-1.json");
    std::fs::write(&path, "invalid").unwrap();
    let result = source.resolve(
        "标枪",
        101,
        Some(AcquisitionUpdatePolicy::UseCache),
        &OnceLock::new(),
        &|_| panic!("损坏缓存需先显式处理"),
    );
    assert!(result.contains("缓存 JSON"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "invalid");
}

#[test]
fn refresh_recovers_malformed_cache_and_preserves_original_evidence() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let relative = Path::new(CACHE_DIRECTORY).join(sha256_bytes("标枪".as_bytes()));
    let directory = source.root.ensure_directory(&relative).unwrap();
    let path = directory.join("100-1.json");
    std::fs::write(&path, "invalid").unwrap();
    let original_error = source.read_cache("标枪").err().unwrap().to_string();
    let result = source.resolve(
        "标枪",
        100,
        Some(AcquisitionUpdatePolicy::Refresh),
        &OnceLock::new(),
        &|_| Ok(response()),
    );
    assert!(result.stored);
    assert!(result.notice.as_ref().unwrap().contains(&original_error));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid");
    assert_eq!(source.read_cache("标枪").unwrap().unwrap().fetched_at, 101);
    let cached = source.resolve(
        "标枪",
        101,
        Some(AcquisitionUpdatePolicy::UseCache),
        &OnceLock::new(),
        &|_| panic!("恢复后的缓存不联网"),
    );
    assert_eq!(cached, "其他途径：开发船坞");
    // 恢复后即使连续同秒刷新或时钟回拨，也必须读取最后一次发布的资料。
    for now in [100, 99] {
        let updated = source.resolve(
            "标枪",
            now,
            Some(AcquisitionUpdatePolicy::Refresh),
            &OnceLock::new(),
            &|_| {
                Ok(String::from_utf8(response())
                    .unwrap()
                    .replace("开发船坞", &format!("刷新{now}"))
                    .into_bytes())
            },
        );
        assert!(updated.stored);
        assert_eq!(
            source.read_cache("标枪").unwrap().unwrap().summary,
            format!("其他途径：刷新{now}")
        );
    }
}

#[test]
fn failed_refresh_preserves_malformed_cache_and_both_errors() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let relative = Path::new(CACHE_DIRECTORY).join(sha256_bytes("标枪".as_bytes()));
    let directory = source.root.ensure_directory(&relative).unwrap();
    let path = directory.join("100-1.json");
    std::fs::write(&path, "invalid").unwrap();
    let original_error = source.read_cache("标枪").err().unwrap().to_string();
    let result = source.resolve(
        "标枪",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &OnceLock::new(),
        &|_| Err(http::FetchError::http_status(503)),
    );
    assert!(!result.stored);
    let notice = result.notice.unwrap();
    assert!(notice.contains(&original_error));
    assert!(notice.contains("HTTP 503"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "invalid");
    assert_eq!(
        std::fs::read_dir(directory)
            .unwrap()
            .filter(|item| item
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "json"))
            .count(),
        1
    );
    assert!(source.last_attempt_failed("标枪").unwrap());
}

#[test]
fn refresh_rejects_unsafe_cache_directory_without_fetching() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let directory = source
        .root
        .ensure_directory(Path::new(CACHE_DIRECTORY))
        .unwrap();
    let path = directory.join(sha256_bytes("标枪".as_bytes()));
    std::fs::write(&path, "not a directory").unwrap();
    let result = source.resolve(
        "标枪",
        101,
        Some(AcquisitionUpdatePolicy::Refresh),
        &OnceLock::new(),
        &|_| panic!("路径错误不能绕过"),
    );
    assert!(!result.stored);
    assert!(result.contains("读取获取方式缓存目录"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "not a directory");
}

#[test]
fn generated_column_and_execution_writeback_share_cached_ship_names() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology;
    use crate::adapters::workbook::XlsxExecutionResultsPort;
    use crate::adapters::workbook::generation::XlsxWorkbookGenerationPort;
    use crate::application::{
        ExecutionResultsPort, WorkbookGenerationPort, project_game_state_to_workbook,
    };
    use calamine::{Reader, open_workbook_auto};
    use std::sync::Arc;

    let fixture = Fixture::new();
    let source = Arc::new(fixture.source());
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("settings.json"),
        fixture.0.join("settings.json"),
    )
    .unwrap();
    crate::adapters::settings::Settings::save_preferences(
        &fixture.0,
        crate::adapters::settings::Settings::load(&fixture.0)
            .unwrap()
            .preferences(),
        crate::application::UserPreferences {
            ship_acquisition_enabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    let state = golden_game_state_with_technology();
    let projection = project_game_state_to_workbook(&state).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for group in state.ship_catalog().groups() {
        let summary = source.resolve(
            group.name(),
            now,
            Some(AcquisitionUpdatePolicy::Refresh),
            &OnceLock::new(),
            &|_| Ok(response()),
        );
        assert!(summary.contains("开发船坞"));
    }
    let layout = crate::adapters::workbook::load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let generator = XlsxWorkbookGenerationPort::new(source.root.clone()).with_acquisition(
        source.clone(),
        crate::adapters::workbook::generation::OperationPreferences::Ready(
            crate::application::UserPreferences {
                ship_acquisition_enabled: true,
                ..Default::default()
            },
        ),
    );
    let report = generator
        .generate_workbook(Some("acquisition.xlsx"), &layout, projection.clone())
        .unwrap();
    assert!(!report.has_warnings());
    assert!(report.acquisition_summary().cached > 0);
    let receipt = serde_json::to_value(&report).unwrap();
    let values = receipt["ship_acquisition"].as_object().unwrap();
    assert_eq!(
        values.len(),
        projection.sheet("loadout_plan").unwrap().rows().len()
    );
    assert!(values.keys().any(|key| key.starts_with("ship:")));
    let unowned = crate::domain::GameState::new(
        state.source().clone(),
        crate::domain::ShipRoster::new(state.ships().source().clone(), Vec::new()),
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );
    let unowned_projection = project_game_state_to_workbook(&unowned).unwrap();
    let AcquisitionEnrichment {
        projection: _,
        values: unowned_values,
        ..
    } = source
        .enrich(&layout, unowned_projection, None, &mut |_| {}, &|| false)
        .unwrap();
    assert!(!unowned_values.is_empty());
    assert!(
        unowned_values
            .iter()
            .all(|(key, value)| key.starts_with("unowned:") && value.contains("开发船坞"))
    );
    assert_eq!(
        report.game_state_content_sha256(),
        state.source().content_sha256()
    );
    assert_ne!(
        report.projection_content_sha256(),
        projection.content_sha256()
    );
    let path = fixture.0.join(report.output_path());
    let verify = || {
        let mut workbook = open_workbook_auto(&path).unwrap();
        let range = workbook.worksheet_range("配装计划").unwrap();
        let header = range.rows().next().unwrap();
        let column = header.iter().position(|cell| *cell == "获取方式").unwrap();
        assert_eq!(header[column - 1].to_string(), "舰船名称");
        for row in range.rows().skip(1) {
            assert!(row[column].to_string().contains("开发船坞"));
        }
    };
    verify();
    let workbook = source.root.existing_workbook("acquisition.xlsx").unwrap();
    let writer = XlsxExecutionResultsPort::new(source.root.clone()).with_acquisition(source);
    let rows = crate::application::project_execution_report_rows(
        &crate::application::test_support::execution_workbook_report_fixture(),
        1_700_000_000_123,
    )
    .unwrap();
    writer
        .write_execution_results(
            &workbook,
            &sha256_bytes(&std::fs::read(&path).unwrap()),
            &layout,
            &rows,
            Some(projection),
            1_700_000_000_123,
        )
        .unwrap();
    verify();
}

#[test]
fn disabled_setting_omits_column_without_querying_and_preserves_workbook_layout_identity() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology;
    use crate::adapters::settings::Settings;
    use crate::adapters::workbook::generation::XlsxWorkbookGenerationPort;
    use crate::application::{
        WorkbookGenerationPort, WorkbookPort, project_game_state_to_workbook,
    };
    use calamine::{Reader, open_workbook_auto};
    use std::sync::Arc;

    let fixture = Fixture::new();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::copy(
        repository.join("settings.json"),
        fixture.0.join("settings.json"),
    )
    .unwrap();
    std::fs::copy(
        repository.join("workbook-layout.xlsx"),
        fixture.0.join("workbook-layout.xlsx"),
    )
    .unwrap();
    let source = Arc::new(fixture.source());
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let layout = crate::adapters::workbook::load_workbook_layout(
        &fixture.0.join("workbook-layout.xlsx"),
        &registry,
    )
    .unwrap();
    let projection = project_game_state_to_workbook(&golden_game_state_with_technology()).unwrap();
    let generator = XlsxWorkbookGenerationPort::new(source.root.clone()).with_acquisition(
        source.clone(),
        crate::adapters::workbook::generation::OperationPreferences::Ready(
            crate::application::UserPreferences::default(),
        ),
    );
    Settings::save_preferences(
        &fixture.0,
        Settings::load(&fixture.0).unwrap().preferences(),
        crate::application::UserPreferences {
            ship_acquisition_enabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut progress = Vec::new();
    let report = generator
        .generate_workbook_with_progress(
            Some("disabled.xlsx"),
            &layout,
            projection.clone(),
            &mut |event| progress.push(format!("{event:?}")),
            &|| false,
        )
        .unwrap();
    assert!(!report.has_warnings());
    assert_eq!(
        report.acquisition_summary().state,
        AcquisitionGenerationState::Disabled
    );
    assert!(!progress.iter().any(|event| event.contains("个舰船名称")));
    assert!(!fixture.0.join(CACHE_DIRECTORY).exists());
    assert!(
        serde_json::to_value(&report)
            .unwrap()
            .get("ship_acquisition")
            .is_none()
    );
    let mut workbook = open_workbook_auto(fixture.0.join(report.output_path())).unwrap();
    assert!(
        !workbook
            .worksheet_range("配装计划")
            .unwrap()
            .rows()
            .next()
            .unwrap()
            .iter()
            .any(|value| *value == "获取方式")
    );
    let port = crate::adapters::workbook::layout::XlsxWorkbookPort::new(
        source.root.clone(),
        fixture.0.join("workbook-layout.xlsx"),
        registry,
    );
    let workbook_ref = source.root.existing_workbook("disabled.xlsx").unwrap();
    Settings::save_preferences(
        &fixture.0,
        Settings::load(&fixture.0).unwrap().preferences(),
        crate::application::UserPreferences {
            ship_acquisition_enabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    let saved = port.load_plan_inputs(&workbook_ref).unwrap();
    assert_eq!(saved.layout, layout.without_ship_acquisition().unwrap());
    // 开启的旧布局同样由文件快照决定，当前关闭开关不改写其列。
    let original = XlsxWorkbookGenerationPort::new(source.root.clone())
        .generate_workbook(Some("original.xlsx"), &layout, projection)
        .unwrap();
    Settings::save_preferences(
        &fixture.0,
        Settings::load(&fixture.0).unwrap().preferences(),
        crate::application::UserPreferences::default(),
    )
    .unwrap();
    let original_ref = source.root.existing_workbook("original.xlsx").unwrap();
    assert_eq!(port.load_plan_inputs(&original_ref).unwrap().layout, layout);
    assert!(fixture.0.join(original.output_path()).is_file());
}

#[test]
#[ignore = "需要访问 BWiki HTTPS，验证真实 WinHTTP 与当前页面格式"]
fn live_wiki_fetch_and_cache() {
    let fixture = Fixture::new();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("settings.json"),
        fixture.0.join("settings.json"),
    )
    .unwrap();
    let source = fixture.source();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (title, expected) in [
        ("标枪", "轻型池"),
        ("安克雷奇", "开发船坞"),
        ("海伦娜·META", "信标档案"),
        ("绊爱", "累计PT奖励"),
    ] {
        let result = source.resolve(
            title,
            now,
            Some(AcquisitionUpdatePolicy::Refresh),
            &OnceLock::new(),
            &|title| http::fetch(&source.root, title, &|| false, &mut |_| {}),
        );
        assert!(result.contains(expected), "{title}: {result}");
        assert!(!result.contains("失败"), "{title}: {result}");
        let cached = source.read_cache(title).unwrap().unwrap();
        assert_eq!(cached.summary, result.text);
        println!("{title}: {}", result.lines().next().unwrap());
    }
}

#[test]
fn omitted_original_name_comes_from_the_name_cell_wiki_link() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology;
    use crate::adapters::workbook::projection_writer::build_projection_workbook_bytes;
    use crate::application::{WorkbookProjectionValue, project_game_state_to_workbook};

    let fixture = Fixture::new();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        fixture.0.join("workbook-layout.xlsx"),
    )
    .unwrap();
    let source = fixture.source();
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let layout = crate::adapters::workbook::load_workbook_layout(
        &fixture.0.join("workbook-layout.xlsx"),
        &registry,
    )
    .unwrap();
    let original = layout
        .fields()
        .iter()
        .find(|field| field.sheet_key() == "loadout_plan" && field.stable_key() == "original_name")
        .unwrap();
    assert_eq!(original.generation(), LayoutGenerationMode::Omitted);
    let projection = project_game_state_to_workbook(&golden_game_state_with_technology()).unwrap();
    let mut expected = BTreeSet::new();
    for row in projection.sheet("loadout_plan").unwrap().rows() {
        let Some(WorkbookProjectionValue::Text(name)) = row.value("original_name") else {
            continue;
        };
        if !name.trim().is_empty() {
            expected.insert(name.clone());
        }
    }
    assert!(!expected.is_empty());
    let path = fixture.0.join("generated.xlsx");
    let build =
        build_projection_workbook_bytes(&path, &layout, &projection, 1_700_000_000_000).unwrap();
    std::fs::write(&path, &build.bytes).unwrap();
    let names = source.ship_titles(&path).unwrap();
    assert_eq!(names, expected.into_iter().collect::<Vec<_>>());

    let mut plain = rust_xlsxwriter::Workbook::new();
    let sheet_name = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap()
        .display_name();
    let sheet = plain.add_worksheet();
    sheet.set_name(sheet_name).unwrap();
    let name_column = layout
        .generated_fields_for_sheet("loadout_plan")
        .iter()
        .position(|field| field.stable_key() == "name")
        .unwrap() as u16;
    if name_column > 0 {
        sheet.write_string(0, 0, "占位").unwrap();
    }
    sheet.write_string(0, name_column, "舰船名称").unwrap();
    sheet.write_string(1, name_column, "没有链接").unwrap();
    let missing = fixture.0.join("missing-identity.xlsx");
    std::fs::write(&missing, plain.save_to_buffer().unwrap()).unwrap();
    let error = source.ship_titles(&missing).unwrap_err();
    let mut detail = error.to_string();
    let mut source_error = std::error::Error::source(&error);
    while let Some(current) = source_error {
        detail.push(' ');
        detail.push_str(&current.to_string());
        source_error = current.source();
    }
    assert!(
        detail.contains("_schema") || detail.contains("布局"),
        "{detail}"
    );
}

#[test]
fn serial_requests_process_each_name_once_and_honor_a_batch_stop() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let fixture = Fixture::new();
    let source = fixture.source();
    let now = 1_700_000_000;
    let names: Vec<String> = ["慢甲", "快乙", "慢丙", "快丁"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let inflight = AtomicUsize::new(0);
    let max_inflight = AtomicUsize::new(0);
    let calls = AtomicUsize::new(0);
    let network_failed = OnceLock::new();
    let records = ShipAcquisition::dispatch_names(
        &names,
        &|name| {
            source.resolve(
                name,
                now,
                Some(AcquisitionUpdatePolicy::Refresh),
                &network_failed,
                &|title| {
                    let current = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_inflight.fetch_max(current, Ordering::SeqCst);
                    if title.starts_with('慢') {
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(response())
                },
            )
        },
        &mut |_, _| {},
    );
    assert_eq!(records.len(), names.len());
    assert_eq!(calls.load(Ordering::SeqCst), names.len());
    assert_eq!(max_inflight.load(Ordering::SeqCst), 1);
    let mut seen: Vec<_> = records.iter().map(|(name, _)| name.clone()).collect();
    seen.sort();
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(seen, expected);

    let cached = source.read_cache("快乙").unwrap().unwrap();
    assert!(!cached.summary.is_empty());
    let cache_calls = AtomicUsize::new(0);
    let _ = ShipAcquisition::dispatch_names(
        &["快乙".to_owned()],
        &|name| {
            source.resolve(
                name,
                now,
                Some(AcquisitionUpdatePolicy::UseCache),
                &OnceLock::new(),
                &|_| {
                    cache_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(response())
                },
            )
        },
        &mut |_, _| {},
    );
    assert_eq!(cache_calls.load(Ordering::SeqCst), 0);

    let stopped = OnceLock::new();
    stopped.set("测试暂停".to_owned()).unwrap();
    let stop_calls = AtomicUsize::new(0);
    let stopped_records = ShipAcquisition::dispatch_names(
        &names,
        &|name| {
            source.resolve(
                name,
                now,
                Some(AcquisitionUpdatePolicy::Refresh),
                &stopped,
                &|_| {
                    stop_calls.fetch_add(1, Ordering::SeqCst);
                    Err(http::FetchError::http_status(503))
                },
            )
        },
        &mut |_, _| {},
    );
    assert_eq!(stopped_records.len(), names.len());
    assert_eq!(stop_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn owner_reports_each_acquisition_before_starting_the_next_request() {
    use std::cell::Cell;
    let names = vec!["甲".to_owned(), "乙".to_owned()];
    let reported = Cell::new(0);
    let records = ShipAcquisition::dispatch_names(
        &names,
        &|name| {
            if name == "乙" {
                assert_eq!(reported.get(), 1);
            }
            name.to_owned()
        },
        &mut |_, _| reported.set(reported.get() + 1),
    );
    assert_eq!(records.len(), 2);
    assert_eq!(reported.get(), 2);
}

#[test]
fn missing_mode_skips_good_cache_and_retries_previous_failures() {
    use crate::application::AcquisitionCacheUpdateOutcome as Outcome;
    let fixture = Fixture::new();
    let source = fixture.source();
    let names = vec!["标枪".to_owned()];
    let first = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Missing,
            &mut |_| {},
            &|| false,
            &|_| Ok(response()),
        )
        .unwrap();
    assert!(matches!(first[0].outcome(), Outcome::Updated));
    let cached = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Missing,
            &mut |_| {},
            &|| false,
            &|_| panic!("已有缓存不联网"),
        )
        .unwrap();
    assert!(matches!(cached[0].outcome(), Outcome::Cached));
    let old = source.read_cache("标枪").unwrap().unwrap().fetched_at;
    let failed = source
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Refresh,
            &mut |_| {},
            &|| false,
            &|_| Err(http::FetchError::http_status(503)),
        )
        .unwrap();
    assert!(matches!(failed[0].outcome(), Outcome::KeptPrevious { .. }));
    assert_eq!(source.read_cache("标枪").unwrap().unwrap().fetched_at, old);
    let retry = fixture
        .source()
        .update_cache_with(
            &names,
            AcquisitionUpdateMode::Missing,
            &mut |_| {},
            &|| false,
            &|_| Ok(response()),
        )
        .unwrap();
    assert!(matches!(retry[0].outcome(), Outcome::Updated));
    let missing_names = vec!["未收录".to_owned()];
    let missing = source
        .update_cache_with(
            &missing_names,
            AcquisitionUpdateMode::Missing,
            &mut |_| {},
            &|| false,
            &|_| Ok(br#"{"error":{"code":"missingtitle"}}"#.to_vec()),
        )
        .unwrap();
    assert!(matches!(missing[0].outcome(), Outcome::Missing));
    let cached_missing = source
        .update_cache_with(
            &missing_names,
            AcquisitionUpdateMode::Missing,
            &mut |_| {},
            &|| false,
            &|_| panic!("确定缺页不重复联网"),
        )
        .unwrap();
    assert!(matches!(cached_missing[0].outcome(), Outcome::Missing));
}

#[test]
fn generation_summary_preserves_typed_partial_failures_and_reused_output_warning() {
    use crate::adapters::workbook::generation::{OperationPreferences, XlsxWorkbookGenerationPort};
    use crate::application::{WorkbookGenerationPort, project_game_state_to_workbook};
    let fixture = Fixture::new();
    let source = std::sync::Arc::new(fixture.source());
    let layout = crate::adapters::workbook::load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let projection = project_game_state_to_workbook(
        &crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state(),
    )
    .unwrap();
    let template = projection.sheet("loadout_plan").unwrap().rows()[0]
        .values()
        .clone();
    let names = ["成功", "缺页", "回退", "失败", ""];
    let rows = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut values = template.clone();
            values.insert(
                "original_name".to_owned(),
                WorkbookProjectionValue::text(*name),
            );
            (format!("ship:{index}"), values)
        })
        .collect();
    let projection = projection
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    source.store_current("回退", 100, "建造".to_owned(), false);
    let result = source
        .enrich_with(
            &layout,
            projection.clone(),
            Some(AcquisitionUpdatePolicy::Refresh),
            &mut |_| {},
            &|| false,
            &|name| match name {
                "成功" => Ok(response()),
                "缺页" => Ok(serde_json::to_vec(
                    &serde_json::json!({"error":{"code":"missingtitle","info":"missing"}}),
                )
                .unwrap()),
                _ => Err(http::FetchError::http_status(404)),
            },
        )
        .unwrap();
    assert_eq!(result.summary.state, AcquisitionGenerationState::Completed);
    assert_eq!(
        (
            result.summary.updated,
            result.summary.missing,
            result.summary.fallback,
            result.summary.failed
        ),
        (1, 1, 1, 2)
    );
    assert_eq!(result.summary.warnings.len(), 4);
    assert!(
        result
            .summary
            .warnings
            .iter()
            .any(|warning| warning.name == "失败" && warning.detail.contains("404"))
    );
    assert!(
        result
            .summary
            .warnings
            .iter()
            .any(|warning| warning.name == "ship:4" && warning.detail.contains("静态名称缺失"))
    );

    // 发布验证使用完整投影，保留装备下拉与舰船行之间的引用。
    let projection = project_game_state_to_workbook(
        &crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state(),
    )
    .unwrap();
    let expected_rows = projection.sheet("loadout_plan").unwrap().rows().len();
    let generator = XlsxWorkbookGenerationPort::new(source.root.clone()).with_acquisition(
        source,
        OperationPreferences::Ready(crate::application::UserPreferences {
            ship_acquisition_enabled: true,
            acquisition_update_policy: AcquisitionUpdatePolicy::UseCache,
            ..Default::default()
        }),
    );
    for expected_reused in [false, true] {
        let report = generator
            .generate_workbook(Some("warnings.xlsx"), &layout, projection.clone())
            .unwrap();
        assert!(report.has_warnings());
        assert_eq!(report.status(), "completed_with_warnings");
        assert!(report.message().contains("警告"));
        assert_eq!(report.reused_existing(), expected_reused);
        assert_eq!(report.acquisition_summary().cached, 0);
        assert!(report.acquisition_summary().failed > 0);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["status"], "completed_with_warnings");
        assert_eq!(
            json["ship_acquisition"].as_object().unwrap().len(),
            expected_rows
        );
        assert!(fixture.0.join(report.output_path()).is_file());
    }
}

#[test]
fn generation_summary_counts_diagnostics_independently_of_cell_wording() {
    let mut summary = AcquisitionGenerationSummary::default();
    Acquisition::current("未获取奖励").record_generation("正常", &mut summary);
    let mut updated = Acquisition::current("建造");
    updated.stored = true;
    updated.notice = Some("原缓存损坏，保留原文件".to_owned());
    updated.record_generation("已恢复", &mut summary);
    for (name, status) in [
        ("不完整", AcquisitionStatus::Incomplete),
        ("存储", AcquisitionStatus::CacheWarning),
        ("未开始", AcquisitionStatus::NotStarted),
    ] {
        Acquisition::with_notice("资料", status, "原始诊断".to_owned())
            .record_generation(name, &mut summary);
    }
    assert_eq!(
        (
            summary.cached,
            summary.updated,
            summary.incomplete,
            summary.cache_warning,
            summary.not_started
        ),
        (1, 1, 1, 1, 1)
    );
    assert_eq!(summary.warnings.len(), 4);
    assert!(
        summary
            .warnings
            .iter()
            .all(|warning| warning.name != "正常")
    );
}

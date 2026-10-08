//! 覆盖游戏状态读取、分页汇总和一致性校验的单元测试。

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::{
    CAPABILITIES_AFTER_STAGE, CAPABILITIES_BEFORE_STAGE, EQUIPMENT_STAGE, GameReadOptions,
    GameReadRuntime, MAP_STATE_STAGE, OWNED_AFTER_STAGE, OWNED_BEFORE_STAGE, SHIP_CATALOG_STAGE,
    SHIP_SKILL_EFFECTS_STAGE, collect_ship_skill_queries,
    read_game_state_with_evidence_and_progress, read_skill_effect_records,
};
use crate::adapters::device::capabilities::ready_capabilities;
use crate::adapters::device::capture::full_state::{
    FullStateCaptureError, FullStateCaptureEvidence, FullStateCaptureRequest,
};
use crate::adapters::device::reading::equipment::{
    EquipmentRawRecords, EquipmentReadError, EquipmentReadResult,
};
use crate::adapters::device::reading::ship_catalog::{
    ShipCatalogReadError, ShipCatalogReadResult, ShipCatalogTable,
};
use crate::adapters::device::runtime::{
    AccountBeforeResult, AgentError, CapabilitiesResult, ClientStage, EquipmentConfigSource,
    EquipmentReferenceNameBatchResult, RequestId, RetryDirective, RuntimeClientError,
    RuntimeProtocolError, RuntimeSkillEffectDetail, SessionEffect, ShipCatalogRecord,
    ShipCatalogTableKey, SkillEffectBatchResult, SkillEffectQuery, SnapshotOwnedStateResult,
    SnapshotShipDetailsResult, validate_capabilities_result,
};
use crate::application::{AppError, AppErrorCode};
use crate::domain::{EquipmentCatalog, EquipmentCatalogSource, GameState};

fn read_game_state_with_evidence<R: GameReadRuntime>(
    runtime: &mut R,
    options: &GameReadOptions,
    cached_equipment: Option<EquipmentReadResult>,
    cached_ship_catalog: Option<ShipCatalogReadResult>,
) -> Result<
    (
        GameState,
        EquipmentReadResult,
        ShipCatalogReadResult,
        CapabilitiesResult,
        Option<FullStateCaptureEvidence>,
    ),
    AppError,
> {
    read_game_state_with_evidence_and_progress(
        runtime,
        options,
        cached_equipment,
        cached_ship_catalog,
        &mut |_| {},
    )
    .map(
        |(state, equipment, ship_catalog, capabilities, capture, _retries)| {
            (state, equipment, ship_catalog, capabilities, capture)
        },
    )
}

fn read_game_state_with<R: GameReadRuntime>(
    runtime: &mut R,
    options: &GameReadOptions,
) -> Result<GameState, AppError> {
    read_game_state_with_evidence(runtime, options, None, None).map(|(state, _, _, _, _)| state)
}

const TIMEOUT_MS: u32 = 12_000;
const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CATALOG_SHA256: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SESSION_ID: &str = "00112233445566778899aabbccddeeff";
static NEXT_CAPTURE_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadCall {
    CapabilitiesBefore,
    Equipment,
    ShipCatalog,
    OwnedBefore,
    AccountBefore,
    OwnedAfter,
    SkillEffects,
    CapabilitiesAfter,
}

struct FakeRuntime {
    calls: Vec<ReadCall>,
    fail_at: Option<ReadCall>,
    equipment: Option<EquipmentReadResult>,
    ship_catalog: Option<ShipCatalogReadResult>,
    owned_before: SnapshotOwnedStateResult,
    ship_details: SnapshotShipDetailsResult,
    owned_after: SnapshotOwnedStateResult,
    capabilities_before: CapabilitiesResult,
    capabilities_after: CapabilitiesResult,
    skill_effects: BTreeMap<(u64, u32), RuntimeSkillEffectDetail>,
    skill_batches: Vec<Vec<(u64, u32)>>,
    equipment_scopes: Vec<crate::domain::GameReadScope>,
    skill_effect_response: Option<SkillEffectBatchResult>,
    owned_before_retryable_failures: u32,
    ship_details_retryable_failures: u32,
}

impl FakeRuntime {
    fn stable() -> Self {
        Self {
            calls: Vec::new(),
            fail_at: None,
            equipment: Some(equipment_result()),
            ship_catalog: Some(ship_catalog_result()),
            owned_before: owned_state(1_000),
            ship_details: ship_details(),
            owned_after: owned_state(1_000),
            capabilities_before: pending_read_capabilities(),
            capabilities_after: ready_capabilities(),
            skill_effects: BTreeMap::new(),
            skill_batches: Vec::new(),
            equipment_scopes: Vec::new(),
            skill_effect_response: None,
            owned_before_retryable_failures: 0,
            ship_details_retryable_failures: 0,
        }
    }

    fn record_owned_call(&mut self) -> ReadCall {
        // 同一次读取里，舰船详情之前的账号快照都是前态；详情之后才是后态复核。
        let call = if self
            .calls
            .iter()
            .any(|call| matches!(call, ReadCall::AccountBefore))
        {
            ReadCall::OwnedAfter
        } else {
            ReadCall::OwnedBefore
        };
        self.calls.push(call);
        call
    }

    fn retryable_agent_error(code: &str) -> RuntimeClientError {
        RuntimeClientError::Agent {
            request_id: RequestId::FIRST,
            error: AgentError {
                code: code.to_owned(),
                stage: "agent.lua".to_owned(),
                message: "测试代理尚未就绪".to_owned(),
                retry: RetryDirective::SameRequest,
                session_effect: SessionEffect::Unchanged,
                details: BTreeMap::new(),
            },
        }
    }

    fn record_capabilities_call(&mut self) -> ReadCall {
        let call = if self
            .calls
            .iter()
            .any(|call| matches!(call, ReadCall::CapabilitiesBefore))
        {
            ReadCall::CapabilitiesAfter
        } else {
            ReadCall::CapabilitiesBefore
        };
        self.calls.push(call);
        call
    }
}

impl GameReadRuntime for FakeRuntime {
    fn read_equipment_catalog(
        &mut self,
        timeout_ms: u32,
        expected_module_sha256: &str,
        scope: crate::domain::GameReadScope,
        cached: Option<EquipmentReadResult>,
    ) -> Result<EquipmentReadResult, EquipmentReadError> {
        self.equipment_scopes.push(scope);
        if let Some(cached) = cached {
            return Ok(cached);
        }
        assert_eq!(timeout_ms, TIMEOUT_MS);
        assert_eq!(expected_module_sha256, MODULE_SHA256);
        self.calls.push(ReadCall::Equipment);
        if self.fail_at == Some(ReadCall::Equipment) {
            return Err(EquipmentReadError::Protocol(RuntimeProtocolError::new(
                "fixture_equipment_failed",
                "测试装备读取失败",
            )));
        }
        Ok(self.equipment.take().expect("装备读取只应调用一次"))
    }

    fn read_ship_catalog(
        &mut self,
        timeout_ms: u32,
        expected_module_sha256: &str,
        _technology: bool,
        cached: Option<ShipCatalogReadResult>,
    ) -> Result<ShipCatalogReadResult, ShipCatalogReadError> {
        if let Some(cached) = cached {
            if cached.module_sha256() != expected_module_sha256 {
                return Err(RuntimeProtocolError::new(
                    "ship_catalog_cache_module_mismatch",
                    "缓存舰船静态目录与当前模块身份不一致",
                )
                .into());
            }
            return Ok(cached);
        }
        assert_eq!(timeout_ms, TIMEOUT_MS);
        assert_eq!(expected_module_sha256, MODULE_SHA256);
        self.calls.push(ReadCall::ShipCatalog);
        if self.fail_at == Some(ReadCall::ShipCatalog) {
            return Err(ShipCatalogReadError::PageProtocol {
                table_key: ShipCatalogTableKey::ShipDataGroup,
                start_index: 0,
                source: RuntimeProtocolError::new(
                    "fixture_ship_catalog_failed",
                    "测试舰船静态目录读取失败",
                ),
            });
        }
        Ok(self.ship_catalog.take().expect("舰船静态目录只应读取一次"))
    }

    fn snapshot_account_before(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
        expected_module_sha256: &str,
    ) -> Result<AccountBeforeResult, RuntimeClientError> {
        assert_eq!(timeout_ms, TIMEOUT_MS);
        assert_eq!((max_ships, max_equipments, max_items), (50, 100, 200));
        assert_eq!(expected_module_sha256, MODULE_SHA256);
        self.calls.push(ReadCall::AccountBefore);
        if self.owned_before_retryable_failures > 0 {
            self.owned_before_retryable_failures -= 1;
            return Err(Self::retryable_agent_error("lua_player_proxy_invalid"));
        }
        if self.ship_details_retryable_failures > 0 {
            self.ship_details_retryable_failures -= 1;
            return Err(Self::retryable_agent_error("lua_bay_proxy_invalid"));
        }
        if matches!(
            self.fail_at,
            Some(ReadCall::AccountBefore | ReadCall::OwnedBefore)
        ) {
            return Err(RuntimeProtocolError::new(
                "fixture_account_before_failed",
                "测试账号前窗口读取失败",
            )
            .into());
        }
        Ok(AccountBeforeResult {
            owned_state: self.owned_before.clone(),
            ship_details: self.ship_details.clone(),
            dock_frames: 1,
        })
    }

    fn snapshot_owned_state(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
    ) -> Result<SnapshotOwnedStateResult, RuntimeClientError> {
        assert_eq!(timeout_ms, TIMEOUT_MS);
        assert_eq!((max_ships, max_equipments, max_items), (50, 100, 200));
        let call = self.record_owned_call();
        if call == ReadCall::OwnedBefore && self.owned_before_retryable_failures > 0 {
            self.owned_before_retryable_failures -= 1;
            return Err(Self::retryable_agent_error("lua_player_proxy_invalid"));
        }
        if self.fail_at == Some(call) {
            return Err(
                RuntimeProtocolError::new("fixture_owned_failed", "测试持有状态读取失败").into(),
            );
        }
        Ok(match call {
            ReadCall::OwnedBefore => self.owned_before.clone(),
            ReadCall::OwnedAfter => self.owned_after.clone(),
            _ => unreachable!(),
        })
    }

    fn snapshot_skill_effects(
        &mut self,
        timeout_ms: u32,
        skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError> {
        assert_eq!(timeout_ms, TIMEOUT_MS);
        assert_eq!(expected_module_sha256, MODULE_SHA256);
        self.calls.push(ReadCall::SkillEffects);
        self.skill_batches.push(
            skills
                .iter()
                .map(|query| (query.skill_id, query.level))
                .collect(),
        );
        if self.fail_at == Some(ReadCall::SkillEffects) {
            return Err(RuntimeProtocolError::new(
                "fixture_skill_effect_failed",
                "测试技能效果读取失败",
            )
            .into());
        }
        if let Some(response) = self.skill_effect_response.take() {
            return Ok(response);
        }
        let records = skills
            .iter()
            .map(|query| {
                self.skill_effects
                    .get(&(query.skill_id, query.level))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeProtocolError::new(
                            "fixture_skill_effect_missing",
                            "测试技能效果不存在",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SkillEffectBatchResult {
            schema_version: 1,
            complete: records.iter().all(|record| record.complete),
            count: records.len() as u32,
            source: EquipmentConfigSource {
                module_sha256: MODULE_SHA256.to_owned(),
            },
            skills: records,
        })
    }

    fn capabilities(&mut self, timeout_ms: u32) -> Result<CapabilitiesResult, RuntimeClientError> {
        assert_eq!(timeout_ms, TIMEOUT_MS);
        let call = self.record_capabilities_call();
        if self.fail_at == Some(call) {
            return Err(RuntimeProtocolError::new(
                "fixture_capabilities_failed",
                "测试能力报告读取失败",
            )
            .into());
        }
        let result = match call {
            ReadCall::CapabilitiesBefore => self.capabilities_before.clone(),
            ReadCall::CapabilitiesAfter => self.capabilities_after.clone(),
            _ => unreachable!(),
        };
        validate_capabilities_result(&result)?;
        Ok(result)
    }
}

#[test]
fn retries_account_collection_after_static_catalogs_and_keeps_cached_equipment() {
    let mut runtime = FakeRuntime::stable();
    runtime.owned_before_retryable_failures = 1;
    runtime.ship_details_retryable_failures = 1;
    let cached = equipment_result();
    let (state, equipment, _, _, capture, retries) = read_game_state_with_evidence_and_progress(
        &mut runtime,
        &options(),
        Some(cached),
        None,
        &mut |_| {},
    )
    .unwrap();

    assert_eq!(
        runtime.calls,
        vec![
            ReadCall::CapabilitiesBefore,
            ReadCall::ShipCatalog,
            ReadCall::AccountBefore,
            ReadCall::AccountBefore,
            ReadCall::AccountBefore,
            ReadCall::OwnedAfter,
            ReadCall::CapabilitiesAfter,
        ]
    );
    assert_eq!(retries.owned_before, 2);
    assert_eq!(retries.ship_details, 0);
    assert_eq!(state.resources().gold(), 1_000);
    assert_eq!(equipment.catalog().source().module_sha256(), MODULE_SHA256);
    assert!(capture.is_none());
    assert!(!runtime.calls.contains(&ReadCall::Equipment));
}

#[test]
fn reads_static_catalog_before_tightly_bracketed_dynamic_state() {
    let mut runtime = FakeRuntime::stable();

    let (state, equipment, _ship_catalog, capabilities, capture) =
        read_game_state_with_evidence(&mut runtime, &options(), None, None).unwrap();

    assert_eq!(
        runtime.calls,
        vec![
            ReadCall::CapabilitiesBefore,
            ReadCall::Equipment,
            ReadCall::ShipCatalog,
            ReadCall::AccountBefore,
            ReadCall::OwnedAfter,
            ReadCall::CapabilitiesAfter,
        ]
    );
    assert_eq!(state.source().module_sha256(), MODULE_SHA256);
    assert_eq!(state.resources().gold(), 1_000);
    assert!(state.ships().is_empty());
    assert!(state.equipment_catalog().families().is_empty());
    assert_eq!(equipment.catalog().source().module_sha256(), MODULE_SHA256);
    assert_eq!(state.equipment_catalog(), equipment.catalog());
    assert_eq!(
        state.source().equipment_catalog_content_sha256(),
        equipment.catalog().source().content_sha256()
    );
    assert_eq!(state.source().ship_catalog_content_sha256(), "3".repeat(64));
    assert_ne!(
        state.source().raw_records_content_sha256(),
        equipment.raw_content_sha256()
    );
    assert_eq!(equipment.raw_records().weapon_count(), 0);
    assert_eq!(equipment.raw_records().skill_count(), 0);
    assert!(capture.is_none());
    assert_eq!(
        capabilities
            .capabilities
            .get("read.owned_state")
            .unwrap()
            .reason_code
            .as_str(),
        "ready"
    );
}

#[test]
fn reuses_validated_catalog_for_later_dynamic_reads_and_captures() {
    let fixture = CaptureTestDirectory::new("cached-equipment");
    let mut runtime = FakeRuntime::stable();
    let (_, equipment, ship_catalog, _, _) =
        read_game_state_with_evidence(&mut runtime, &options(), None, None).unwrap();
    runtime.calls.clear();
    runtime.ship_catalog = Some(ship_catalog_result());
    runtime.owned_before = owned_state(900);
    runtime.owned_after = owned_state(900);
    let request = FullStateCaptureRequest::new(
        &fixture.tool_root,
        &fixture.capture_root,
        SESSION_ID.parse().unwrap(),
        2,
    )
    .unwrap();
    let capture_options = options().with_full_state_capture(request);

    let (state, returned_equipment, _, _, evidence) = read_game_state_with_evidence(
        &mut runtime,
        &capture_options,
        Some(equipment),
        Some(ship_catalog),
    )
    .unwrap();

    assert_eq!(
        runtime.calls,
        vec![
            ReadCall::CapabilitiesBefore,
            ReadCall::AccountBefore,
            ReadCall::OwnedAfter,
            ReadCall::CapabilitiesAfter,
        ]
    );
    assert_eq!(state.resources().gold(), 900);
    assert_eq!(state.equipment_catalog(), returned_equipment.catalog());
    let evidence = evidence.expect("缓存目录也必须进入本次完整捕获");
    let document: Value = serde_json::from_slice(&fs::read(evidence.path()).unwrap()).unwrap();
    assert_eq!(document["read_index"], 2);
    assert_eq!(document["owned_state_before"]["player"]["gold"], 900);
    assert_eq!(document["equipment_raw"]["schema_version"], 2);
    assert_eq!(
        document["equipment_raw_content_sha256"],
        returned_equipment.raw_content_sha256()
    );
}

#[test]
fn explicitly_captures_complete_raw_inputs_outside_the_tool_root() {
    let fixture = CaptureTestDirectory::new("complete");
    let session_id = SESSION_ID.parse().unwrap();
    let request =
        FullStateCaptureRequest::new(&fixture.tool_root, &fixture.capture_root, session_id, 1)
            .unwrap();
    let capture_options = options().with_full_state_capture(request);
    let mut runtime = FakeRuntime::stable();

    let (state, _equipment, _ship_catalog, _capabilities, evidence) =
        read_game_state_with_evidence(&mut runtime, &capture_options, None, None).unwrap();
    let evidence = evidence.expect("显式采集必须返回脱敏证据");
    let bytes = fs::read(evidence.path()).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(evidence.path().file_name().unwrap(), "001-state.json");
    assert_eq!(evidence.schema_version(), 2);
    assert_eq!(evidence.session_id(), session_id);
    assert_eq!(evidence.read_index(), 1);
    assert_eq!(document["capture_schema_version"], 2);
    assert_eq!(document["session_id"], SESSION_ID);
    assert_eq!(document["read_index"], 1);
    assert_eq!(document["module_sha256"], MODULE_SHA256);
    assert_eq!(
        document["game_state_content_sha256"],
        state.source().content_sha256()
    );
    assert_eq!(document["owned_state_before"]["player"]["gold"], 1_000);
    assert_eq!(document["owned_state_after"]["player"]["gold"], 1_000);
    assert_eq!(document["ship_details"]["schema_version"], 4);
    assert_eq!(document["ship_catalog"]["schema_version"], 1);
    assert_eq!(document["ship_catalog_content_sha256"], "3".repeat(64));
    assert_eq!(document["equipment_raw"]["schema_version"], 2);
    assert_eq!(
        document["capabilities_before"]["capabilities"]["read.owned_state"]["reason_code"],
        "main_thread_not_ready"
    );
    assert_eq!(
        document["capabilities_after"]["capabilities"]["read.owned_state"]["reason_code"],
        "ready"
    );
    assert_eq!(bytes.len() as u64, evidence.size_bytes());
    assert_eq!(
        suzushiro_content_digest::sha256_file(evidence.path()).unwrap(),
        evidence.sha256()
    );
    assert!(
        evidence
            .path()
            .starts_with(fs::canonicalize(&fixture.capture_root).unwrap())
    );
    assert!(!fixture.tool_root.join("full-state-captures").exists());
}

#[test]
fn capture_appends_without_overwriting_existing_evidence() {
    let fixture = CaptureTestDirectory::new("existing");
    let capture_directory = fixture.capture_root.join("full-state-captures");
    fs::create_dir_all(&capture_directory).unwrap();
    let target = capture_directory.join("001-state.json");
    fs::write(&target, b"existing").unwrap();
    let request = FullStateCaptureRequest::new(
        &fixture.tool_root,
        &fixture.capture_root,
        SESSION_ID.parse().unwrap(),
        1,
    )
    .unwrap();
    let capture_options = options().with_full_state_capture(request);
    let mut runtime = FakeRuntime::stable();

    let (_, _, _, _, evidence) =
        read_game_state_with_evidence(&mut runtime, &capture_options, None, None).unwrap();
    assert_eq!(
        evidence.unwrap().path().file_name().unwrap(),
        "002-state.json"
    );

    assert_eq!(fs::read(target).unwrap(), b"existing");
    assert!(fs::read_dir(capture_directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
}

#[test]
fn capture_is_not_created_when_state_mapping_fails() {
    let fixture = CaptureTestDirectory::new("mapping-failure");
    let request = FullStateCaptureRequest::new(
        &fixture.tool_root,
        &fixture.capture_root,
        SESSION_ID.parse().unwrap(),
        1,
    )
    .unwrap();
    let capture_options = options().with_full_state_capture(request);
    let mut runtime = FakeRuntime::stable();
    runtime.owned_after = owned_state(1_001);

    let error = read_game_state_with(&mut runtime, &capture_options).unwrap_err();

    assert_eq!(error.stage(), MAP_STATE_STAGE);
    assert!(!fixture.capture_root.join("full-state-captures").exists());
}

#[test]
fn capture_rejects_zero_index_and_a_root_inside_the_release() {
    let fixture = CaptureTestDirectory::new("boundary");
    let inside = fixture.tool_root.join("private");
    fs::create_dir_all(&inside).unwrap();
    let session_id = SESSION_ID.parse().unwrap();

    let zero =
        FullStateCaptureRequest::new(&fixture.tool_root, &fixture.capture_root, session_id, 0)
            .unwrap_err();
    let inside =
        FullStateCaptureRequest::new(&fixture.tool_root, &inside, session_id, 1).unwrap_err();

    assert!(matches!(zero, FullStateCaptureError::InvalidReadIndex));
    assert!(matches!(
        inside,
        FullStateCaptureError::InsideToolRoot { .. }
    ));
}

#[test]
fn collects_unique_effective_ship_skill_keys() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/game_state/full_state_input.json"
    ))
    .unwrap();
    let mut details_value = fixture["ship_details"].clone();
    let duplicate = details_value["ships"][0]["skills"][0].clone();
    details_value["ships"][0]["skills"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let details: SnapshotShipDetailsResult = deserialize(details_value);

    let queries = collect_ship_skill_queries(&details).unwrap();

    assert_eq!(queries, vec![SkillEffectQuery::new(10_411, 1).unwrap()]);
}

#[test]
fn reads_missing_skill_effects_in_bounded_batches() {
    let mut runtime = FakeRuntime::stable();
    let queries: Vec<SkillEffectQuery> = (1_u64..=65)
        .map(|skill_id| SkillEffectQuery::new(skill_id, 1).unwrap())
        .collect();
    runtime.skill_effects = queries
        .iter()
        .map(|query| {
            (
                (query.skill_id, query.level),
                skill_effect_record(query.skill_id, query.level, true),
            )
        })
        .collect();

    let records =
        read_skill_effect_records(&mut runtime, TIMEOUT_MS, &queries, &[], MODULE_SHA256).unwrap();

    assert_eq!(records.len(), 65);
    assert_eq!(runtime.skill_batches.len(), 2);
    assert_eq!(runtime.skill_batches[0].len(), 64);
    assert_eq!(runtime.skill_batches[1], vec![(65, 1)]);
    assert_eq!(records[0].skill_id, 1);
    assert_eq!(records[64].skill_id, 65);
}

#[test]
fn reuses_equipment_skill_evidence_and_keeps_incomplete_records() {
    let mut runtime = FakeRuntime::stable();
    let queries = vec![
        SkillEffectQuery::new(10, 1).unwrap(),
        SkillEffectQuery::new(20, 2).unwrap(),
    ];
    let reusable = skill_effect_record(10, 1, true);
    runtime
        .skill_effects
        .insert((20, 2), skill_effect_record(20, 2, false));

    let records = read_skill_effect_records(
        &mut runtime,
        TIMEOUT_MS,
        &queries,
        &[reusable],
        MODULE_SHA256,
    )
    .unwrap();

    assert_eq!(runtime.skill_batches, vec![vec![(20, 2)]]);
    assert!(records[0].complete);
    assert!(!records[1].complete);
}

#[test]
fn rejects_skill_effect_response_order_mismatch() {
    let mut runtime = FakeRuntime::stable();
    let queries = vec![
        SkillEffectQuery::new(10, 1).unwrap(),
        SkillEffectQuery::new(20, 1).unwrap(),
    ];
    runtime.skill_effect_response = Some(SkillEffectBatchResult {
        schema_version: 1,
        complete: true,
        count: 2,
        source: EquipmentConfigSource {
            module_sha256: MODULE_SHA256.to_owned(),
        },
        skills: vec![
            skill_effect_record(20, 1, true),
            skill_effect_record(10, 1, true),
        ],
    });

    let error = read_skill_effect_records(&mut runtime, TIMEOUT_MS, &queries, &[], MODULE_SHA256)
        .unwrap_err();

    assert_eq!(error.code(), "skill_effect_order_mismatch");
}

#[test]
fn stops_when_ship_skill_effect_read_fails() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/game_state/full_state_input.json"
    ))
    .unwrap();
    let mut runtime = FakeRuntime::stable();
    runtime.ship_details = deserialize(fixture["ship_details"].clone());
    runtime.fail_at = Some(ReadCall::SkillEffects);

    let error = read_game_state_with(&mut runtime, &options()).unwrap_err();

    assert_eq!(
        runtime.calls,
        vec![
            ReadCall::CapabilitiesBefore,
            ReadCall::Equipment,
            ReadCall::ShipCatalog,
            ReadCall::AccountBefore,
            ReadCall::OwnedAfter,
            ReadCall::SkillEffects,
        ]
    );
    assert_eq!(error.stage(), SHIP_SKILL_EFFECTS_STAGE);
    assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
    assert!(error.source().is_some());
}

#[test]
fn stops_immediately_at_each_failed_read_stage() {
    let cases = [
        (
            ReadCall::CapabilitiesBefore,
            vec![ReadCall::CapabilitiesBefore],
            CAPABILITIES_BEFORE_STAGE,
        ),
        (
            ReadCall::Equipment,
            vec![ReadCall::CapabilitiesBefore, ReadCall::Equipment],
            EQUIPMENT_STAGE,
        ),
        (
            ReadCall::ShipCatalog,
            vec![
                ReadCall::CapabilitiesBefore,
                ReadCall::Equipment,
                ReadCall::ShipCatalog,
            ],
            SHIP_CATALOG_STAGE,
        ),
        (
            ReadCall::AccountBefore,
            vec![
                ReadCall::CapabilitiesBefore,
                ReadCall::Equipment,
                ReadCall::ShipCatalog,
                ReadCall::AccountBefore,
            ],
            OWNED_BEFORE_STAGE,
        ),
        (
            ReadCall::OwnedAfter,
            vec![
                ReadCall::CapabilitiesBefore,
                ReadCall::Equipment,
                ReadCall::ShipCatalog,
                ReadCall::AccountBefore,
                ReadCall::OwnedAfter,
            ],
            OWNED_AFTER_STAGE,
        ),
        (
            ReadCall::CapabilitiesAfter,
            vec![
                ReadCall::CapabilitiesBefore,
                ReadCall::Equipment,
                ReadCall::ShipCatalog,
                ReadCall::AccountBefore,
                ReadCall::OwnedAfter,
                ReadCall::CapabilitiesAfter,
            ],
            CAPABILITIES_AFTER_STAGE,
        ),
    ];

    for (fail_at, expected_calls, expected_stage) in cases {
        let mut runtime = FakeRuntime::stable();
        runtime.fail_at = Some(fail_at);

        let error = read_game_state_with(&mut runtime, &options()).unwrap_err();

        assert_eq!(runtime.calls, expected_calls, "失败阶段: {fail_at:?}");
        assert_eq!(error.stage(), expected_stage);
        assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
        assert!(error.source().is_some());
    }
}

#[test]
fn rejects_state_changed_inside_dynamic_snapshot_window() {
    let mut runtime = FakeRuntime::stable();
    runtime.owned_after = owned_state(1_001);

    let error = read_game_state_with(&mut runtime, &options()).unwrap_err();

    assert_eq!(error.stage(), MAP_STATE_STAGE);
    assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
    assert!(error.context().contains_key("owned_before_sha256"));
    assert!(error.context().contains_key("owned_after_sha256"));
    assert!(error.source().is_some());
}

#[test]
fn cached_ship_catalog_still_rejects_a_changed_dynamic_window() {
    let mut runtime = FakeRuntime::stable();
    let (_, equipment, ship_catalog, _, _) =
        read_game_state_with_evidence(&mut runtime, &options(), None, None).unwrap();
    runtime.calls.clear();
    runtime.owned_before = owned_state(900);
    runtime.owned_after = owned_state(901);

    let error = read_game_state_with_evidence(
        &mut runtime,
        &options(),
        Some(equipment),
        Some(ship_catalog),
    )
    .unwrap_err();

    assert!(!runtime.calls.contains(&ReadCall::ShipCatalog));
    assert!(!runtime.calls.contains(&ReadCall::Equipment));
    assert!(runtime.calls.contains(&ReadCall::AccountBefore));
    assert!(runtime.calls.contains(&ReadCall::OwnedAfter));
    assert_eq!(error.stage(), MAP_STATE_STAGE);
    assert!(error.context().contains_key("owned_before_sha256"));
    assert!(error.context().contains_key("owned_after_sha256"));
}

#[test]
fn rejects_capability_report_that_disagrees_with_completed_reads() {
    let mut runtime = FakeRuntime::stable();
    let status = runtime
        .capabilities_after
        .capabilities
        .get_mut("read.owned_state")
        .unwrap();
    status.available = false;
    status.reason_code = "owned_state_not_ready".to_owned();

    let error = read_game_state_with(&mut runtime, &options()).unwrap_err();

    assert_eq!(error.stage(), CAPABILITIES_AFTER_STAGE);
    assert_eq!(error.code(), AppErrorCode::GameNotReady);
    assert_eq!(
        error.context().get("capability").unwrap(),
        "read.owned_state"
    );
    assert_eq!(
        error.context().get("reason_code").unwrap(),
        "owned_state_not_ready"
    );
}

#[test]
fn rejects_an_invalid_capability_contract_before_expensive_reads() {
    let mut missing_key = FakeRuntime::stable();
    missing_key
        .capabilities_before
        .capabilities
        .remove("write.destroy");

    let error = read_game_state_with(&mut missing_key, &options()).unwrap_err();

    assert_eq!(missing_key.calls, vec![ReadCall::CapabilitiesBefore]);
    assert_eq!(error.stage(), CAPABILITIES_BEFORE_STAGE);
    assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
    assert_eq!(
        error.context().get("runtime_code").unwrap(),
        "required_capability_missing"
    );

    let mut invalid_enhance_reason = FakeRuntime::stable();
    let status = invalid_enhance_reason
        .capabilities_before
        .capabilities
        .get_mut("write.enhance")
        .unwrap();
    status.reason_code = "compose_recipes_not_ready".to_owned();

    let error = read_game_state_with(&mut invalid_enhance_reason, &options()).unwrap_err();

    assert_eq!(
        invalid_enhance_reason.calls,
        vec![ReadCall::CapabilitiesBefore]
    );
    assert_eq!(error.stage(), CAPABILITIES_BEFORE_STAGE);
    assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
    assert_eq!(
        error.context().get("runtime_code").unwrap(),
        "equipment_write_capability_invalid"
    );
}

#[test]
fn validates_all_options_before_a_runtime_is_needed() {
    let timeout_error = GameReadOptions::new(0, 50, 100, 200, MODULE_SHA256).unwrap_err();
    assert_eq!(timeout_error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(
        timeout_error.context().get("protocol_code").unwrap(),
        "timeout_out_of_range"
    );

    let limit_error = GameReadOptions::new(TIMEOUT_MS, 0, 100, 200, MODULE_SHA256).unwrap_err();
    assert_eq!(limit_error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(
        limit_error.context().get("protocol_code").unwrap(),
        "snapshot_limit_out_of_range"
    );

    let sha_error = GameReadOptions::new(TIMEOUT_MS, 50, 100, 200, "ABC").unwrap_err();
    assert_eq!(
        sha_error.context().get("protocol_code").unwrap(),
        "sha256_invalid"
    );
    assert_eq!(sha_error.code(), AppErrorCode::RuntimeIncompatible);
}

#[test]
fn classifies_runtime_transport_and_readiness_failures() {
    let transport = RuntimeClientError::Io {
        stage: ClientStage::ReadResponse,
        source: io::Error::new(io::ErrorKind::ConnectionReset, "fixture reset"),
    };
    assert_eq!(
        super::classify_runtime_error(&transport),
        AppErrorCode::FullCheckFailed
    );
    let connect = RuntimeClientError::Io {
        stage: ClientStage::Connect,
        source: io::Error::new(io::ErrorKind::ConnectionRefused, "fixture refused"),
    };
    assert_eq!(
        super::classify_runtime_error(&connect),
        AppErrorCode::RuntimeBootstrapFailed
    );

    let not_ready = RuntimeClientError::Agent {
        request_id: RequestId::FIRST,
        error: AgentError {
            code: "owned_state_not_ready".to_owned(),
            stage: "agent.queue".to_owned(),
            message: "完整状态尚未就绪".to_owned(),
            retry: RetryDirective::SameRequest,
            session_effect: SessionEffect::Unchanged,
            details: BTreeMap::new(),
        },
    };
    assert_eq!(
        super::classify_runtime_error(&not_ready),
        AppErrorCode::GameNotReady
    );
    let app_error =
        super::map_runtime_error(OWNED_BEFORE_STAGE, "读取账号持有状态前快照失败", not_ready);
    assert_eq!(
        app_error.context().get("agent_stage").unwrap(),
        "agent.queue"
    );
    assert_eq!(app_error.context().get("retry").unwrap(), "same_request");
    assert_eq!(
        app_error.context().get("session_effect").unwrap(),
        "unchanged"
    );

    let read_failure = RuntimeClientError::Agent {
        request_id: RequestId::FIRST,
        error: AgentError {
            code: "lua_bay_data_lookup_failed".to_owned(),
            stage: "agent.lua".to_owned(),
            message: "船坞数据读取失败".to_owned(),
            retry: RetryDirective::Never,
            session_effect: SessionEffect::Unchanged,
            details: BTreeMap::new(),
        },
    };
    assert_eq!(
        super::classify_runtime_error(&read_failure),
        AppErrorCode::FullCheckFailed
    );
}

#[test]
fn maps_state_failures_to_specific_stable_codes() {
    let cases = [
        (
            super::map_state_error(super::GameStateMappingError::OwnedStateChanged {
                before_sha256: "a".repeat(64),
                after_sha256: "b".repeat(64),
            }),
            AppErrorCode::FullCheckFailed,
        ),
        (
            super::map_state_error(super::GameStateMappingError::EquipmentConfigMissing {
                context: "warehouse:1000".to_owned(),
                config_id: 1_000,
            }),
            AppErrorCode::EquipmentNotFound,
        ),
        (
            super::map_state_error(super::GameStateMappingError::EnhanceLevelOutOfRange {
                context: "warehouse:1000".to_owned(),
                value: 256,
            }),
            AppErrorCode::EnhanceInvalid,
        ),
        (
            super::map_state_error(super::GameStateMappingError::EnhanceLevelMismatch {
                context: "warehouse:1000".to_owned(),
                config_id: 1_000,
                actual: 1,
                expected: 0,
            }),
            AppErrorCode::EquipmentStateChanged,
        ),
        (
            super::map_state_error(super::GameStateMappingError::DuplicateWarehouseConfig {
                config_id: 1_000,
            }),
            AppErrorCode::EquipmentConflict,
        ),
        (
            super::map_state_error(super::GameStateMappingError::BagRecipeMismatch {
                recipe_id: 17_001,
            }),
            AppErrorCode::FullCheckFailed,
        ),
    ];

    for (error, expected_code) in cases {
        assert_eq!(error.code(), expected_code);
        assert!(error.source().is_some());
    }
}

#[test]
fn preserves_structured_object_context_for_mapping_failures() {
    let ship_error = super::map_state_error(super::GameStateMappingError::Ship(
        super::ShipMappingError::SlotFieldOutOfRange {
            ship_id: 9_001,
            slot_index: 3,
            field: "enhance_level",
            value: 300,
        },
    ));
    assert_eq!(ship_error.context().get("ship_id").unwrap(), "9001");
    assert_eq!(ship_error.context().get("slot_index").unwrap(), "3");
    assert_eq!(ship_error.context().get("field").unwrap(), "enhance_level");

    let detail_error = super::map_state_error(super::GameStateMappingError::EquipmentDetails(
        super::EquipmentDetailMappingError::SkillField {
            skill_id: 60_830,
            level: 14,
            field: "effect_list",
            message: "fixture".to_owned(),
        },
    ));
    assert_eq!(detail_error.context().get("skill_id").unwrap(), "60830");
    assert_eq!(detail_error.context().get("skill_level").unwrap(), "14");
    assert_eq!(detail_error.context().get("field").unwrap(), "effect_list");

    let catalog_error = super::map_equipment_error(EquipmentReadError::Mapping(
        super::EquipmentMappingError::InvalidConfigField {
            config_id: 1_001,
            field: "weapon_id",
            message: "fixture".to_owned(),
        },
    ));
    assert_eq!(catalog_error.context().get("config_id").unwrap(), "1001");
    assert_eq!(catalog_error.context().get("field").unwrap(), "weapon_id");
}

#[test]
fn distinguishes_missing_capabilities_from_temporarily_unready_ones() {
    let missing = super::map_capability_error(super::FullStateCapabilityError::Missing {
        capability: "read.bag",
    });
    assert_eq!(missing.code(), AppErrorCode::CapabilityMissing);

    let not_ready = super::map_capability_error(super::FullStateCapabilityError::NotReady {
        capability: "read.bag",
        available: false,
        reason_code: "bag_proxy_not_ready".to_owned(),
    });
    assert_eq!(not_ready.code(), AppErrorCode::GameNotReady);
}

fn options() -> GameReadOptions {
    GameReadOptions::new(TIMEOUT_MS, 50, 100, 200, MODULE_SHA256).unwrap()
}

fn pending_read_capabilities() -> CapabilitiesResult {
    let mut capabilities = ready_capabilities();
    for (name, status) in &mut capabilities.capabilities {
        if name.starts_with("read.") {
            status.available = false;
            status.reason_code = "main_thread_not_ready".to_owned();
        }
    }
    capabilities
}

fn equipment_result() -> EquipmentReadResult {
    let catalog = EquipmentCatalog::new(
        EquipmentCatalogSource::new(MODULE_SHA256.to_owned(), CATALOG_SHA256.to_owned()),
        Vec::new(),
        Vec::new(),
        0,
    );
    let references: EquipmentReferenceNameBatchResult = deserialize(json!({
        "schema_version": 1,
        "complete": true,
        "count": 1,
        "source": {"module_sha256": MODULE_SHA256},
        "equipment_types": [{"equipment_type_id": 10, "name": "设备", "error": null}],
        "nations": [],
        "ship_types": [],
        "attributes": []
    }));
    let raw = EquipmentRawRecords::new(Vec::new(), Vec::new(), references, Vec::new(), Vec::new())
        .unwrap();
    EquipmentReadResult::new(catalog, raw)
}

fn ship_catalog_result() -> ShipCatalogReadResult {
    let records_for = |table_key| match table_key {
        ShipCatalogTableKey::ShipDataGroup => vec![ShipCatalogRecord {
            id: 1,
            raw: json!({"group_type": 10117, "trans_skill": [], "trans_type": 0}),
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
                "buff_list": [],
                "buff_list_display": [],
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
        _ => Vec::new(),
    };
    ShipCatalogReadResult::from_capture(
        MODULE_SHA256.to_owned(),
        "3".repeat(64),
        ShipCatalogTableKey::ALL
            .into_iter()
            .map(|table_key| ShipCatalogTable::from_capture(table_key, records_for(table_key)))
            .collect(),
    )
}

fn owned_state(gold: u64) -> SnapshotOwnedStateResult {
    deserialize(json!({
        "schema_version": 3,
        "complete": true,
        "dock": {
            "complete": true,
            "count": 0,
            "truncated": false,
            "ships": [],
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
        "player": {
            "gold": gold,
            "equipment_capacity": 0,
            "equipment_limit": 300
        }
    }))
}

fn ship_details() -> SnapshotShipDetailsResult {
    deserialize(json!({
        "schema_version": 4,
        "complete": true,
        "count": 0,
        "truncated": false,
        "source": {"module_sha256": MODULE_SHA256},
        "ships": [],
        "read_errors": []
    }))
}

fn skill_effect_record(skill_id: u64, level: u32, complete: bool) -> RuntimeSkillEffectDetail {
    let display = if complete {
        json!({
            "available": true,
            "complete": true,
            "value": {"id": skill_id},
            "error": null,
            "read_errors": []
        })
    } else {
        json!({
            "available": false,
            "complete": false,
            "value": null,
            "error": "display missing",
            "read_errors": []
        })
    };
    deserialize(json!({
        "skill_id": skill_id,
        "level": level,
        "display": display,
        "battle_skill": {
            "available": true,
            "complete": true,
            "value": {"id": skill_id, "effect_list": []},
            "error": null,
            "read_errors": []
        },
        "battle_buff": {
            "available": false,
            "complete": false,
            "value": null,
            "error": "buff missing",
            "read_errors": []
        },
        "complete": complete
    }))
}

fn deserialize<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("测试数据应满足运行时 DTO 结构")
}

struct CaptureTestDirectory {
    base: PathBuf,
    tool_root: PathBuf,
    capture_root: PathBuf,
}

impl CaptureTestDirectory {
    fn new(label: &str) -> Self {
        let id = NEXT_CAPTURE_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let base = home
            .join("suzushiro/scratch/azlw-full-state-capture-tests")
            .join(format!("{label}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let tool_root = base.join("release");
        let capture_root = base.join("private");
        fs::create_dir_all(&tool_root).unwrap();
        fs::create_dir_all(&capture_root).unwrap();
        Self {
            base,
            tool_root,
            capture_root,
        }
    }
}

impl Drop for CaptureTestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn reports_actual_reading_stages_without_invented_item_counts() {
    let mut runtime = FakeRuntime::stable();
    let mut progress = Vec::new();
    super::read_game_state_with_evidence_and_progress(
        &mut runtime,
        &options(),
        None,
        None,
        &mut |event| progress.push(event),
    )
    .unwrap();
    assert_eq!(
        progress
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>(),
        [
            "正在检查游戏读取能力",
            "正在读取装备目录",
            "正在读取舰船静态目录",
            "正在读取账号前窗口",
            "正在复核账号持有状态",
            "正在读取舰船底层技能效果",
            "正在复核游戏读取能力",
        ]
    );
    assert!(progress.iter().all(|event| event.units.is_none()));
}

fn runtime_with_static_skill_queries() -> FakeRuntime {
    let mut runtime = FakeRuntime::stable();
    let catalog = ship_catalog_result();
    let tables = catalog
        .tables()
        .iter()
        .map(|table| {
            let mut records = table.records().to_vec();
            match table.table_key() {
                ShipCatalogTableKey::ShipDataTemplate => {
                    records[0].raw["buff_list"] = json!([10]);
                }
                ShipCatalogTableKey::SkillDataTemplate => {
                    records.push(ShipCatalogRecord {
                        id: 10,
                        raw: json!({"name":"技能", "desc":"说明", "max_level":2}),
                    });
                }
                _ => {}
            }
            ShipCatalogTable::from_capture(table.table_key(), records)
        })
        .collect();
    runtime.ship_catalog = Some(ShipCatalogReadResult::from_capture(
        MODULE_SHA256.to_owned(),
        "3".repeat(64),
        tables,
    ));
    runtime
        .skill_effects
        .insert((10, 1), skill_effect_record(10, 1, true));
    runtime
        .skill_effects
        .insert((10, 2), skill_effect_record(10, 2, true));
    runtime
}

#[test]
fn template_scope_skips_unrequested_skill_batches_and_records_actual_coverage() {
    let mut complete = runtime_with_static_skill_queries();
    let full = read_game_state_with(&mut complete, &options()).unwrap();
    assert_eq!(complete.skill_batches, vec![vec![(10, 1), (10, 2)]]);
    let mut compact = runtime_with_static_skill_queries();
    compact.fail_at = Some(ReadCall::SkillEffects);
    let scope = crate::domain::GameReadScope::with_ship_skill_effects(false);
    let state = read_game_state_with(&mut compact, &options().with_read_scope(scope)).unwrap();
    assert!(!compact.calls.contains(&ReadCall::SkillEffects));
    assert!(compact.skill_batches.is_empty());
    assert_eq!(state.source().read_scope(), scope);
    assert_ne!(
        state.source().content_sha256(),
        full.source().content_sha256()
    );
    assert_eq!(state.resources(), full.resources());
    let projection = crate::application::project_game_state_to_workbook(&state).unwrap();
    assert_eq!(projection.source().read_scope(), scope);
    for row in projection.sheet("loadout_plan").unwrap().rows() {
        assert_eq!(
            row.value("skills_data_complete"),
            Some(&crate::application::WorkbookProjectionValue::Blank)
        );
        assert_eq!(
            row.value("skills_read_errors"),
            Some(&crate::application::WorkbookProjectionValue::Blank)
        );
    }
}

#[test]
fn full_capture_requests_all_skill_effects_even_with_compact_template() {
    let fixture = CaptureTestDirectory::new("scope-full-capture");
    let request = FullStateCaptureRequest::new(
        &fixture.tool_root,
        &fixture.capture_root,
        SESSION_ID.parse().unwrap(),
        1,
    )
    .unwrap();
    let mut runtime = runtime_with_static_skill_queries();
    let options = options()
        .with_read_scope(
            crate::domain::GameReadScope::with_ship_skill_effects(false)
                .with_equipment_details(false, false),
        )
        .with_full_state_capture(request);
    let (state, _, _, _, capture) =
        read_game_state_with_evidence(&mut runtime, &options, None, None).unwrap();
    assert_eq!(
        runtime.equipment_scopes,
        vec![crate::domain::GameReadScope::full()]
    );
    assert!(state.source().read_scope().ship_skill_effects());
    assert_eq!(runtime.skill_batches, vec![vec![(10, 1), (10, 2)]]);
    assert!(capture.is_some());
}

#[test]
fn cached_equipment_does_not_suppress_newly_requested_ship_skill_effects() {
    let mut runtime = runtime_with_static_skill_queries();
    let scope = crate::domain::GameReadScope::with_ship_skill_effects(false);
    let (_, equipment, _, _, _) =
        read_game_state_with_evidence(&mut runtime, &options().with_read_scope(scope), None, None)
            .unwrap();
    runtime.calls.clear();
    runtime.ship_catalog = runtime_with_static_skill_queries().ship_catalog;
    let (state, _, _, _, _) =
        read_game_state_with_evidence(&mut runtime, &options(), Some(equipment), None).unwrap();
    assert!(!runtime.calls.contains(&ReadCall::Equipment));
    assert_eq!(runtime.skill_batches, vec![vec![(10, 1), (10, 2)]]);
    assert!(state.source().read_scope().ship_skill_effects());
}

#[test]
fn repeated_read_skips_cached_catalog_queries() {
    let mut runtime = FakeRuntime::stable();
    let cold_started = std::time::Instant::now();
    let (state, equipment, catalog, _, _) =
        read_game_state_with_evidence(&mut runtime, &options(), None, None).unwrap();
    let cold_us = cold_started.elapsed().as_micros();
    let cold_calls = runtime.calls.clone();
    let catalog_tables = catalog.tables().len();
    let catalog_records = catalog.record_count();
    let catalog_sha256 = catalog.content_sha256().to_owned();
    let state_sha256 = state.source().content_sha256().to_owned();
    runtime.calls.clear();
    let warm_started = std::time::Instant::now();
    read_game_state_with_evidence(&mut runtime, &options(), Some(equipment), Some(catalog))
        .unwrap();
    let warm_us = warm_started.elapsed().as_micros();
    assert!(cold_calls.contains(&ReadCall::Equipment));
    assert!(cold_calls.contains(&ReadCall::ShipCatalog));
    assert!(!runtime.calls.contains(&ReadCall::Equipment));
    assert!(!runtime.calls.contains(&ReadCall::ShipCatalog));
    assert!(runtime.calls.contains(&ReadCall::AccountBefore));
    assert!(runtime.calls.contains(&ReadCall::OwnedAfter));
    if std::env::var_os("AZLW_MEASURE_MODE").is_some() {
        println!(
            "\nMEASURE stage=synthetic_sync cold_us={cold_us} warm_us={warm_us} cold_calls={cold_calls:?} warm_calls={:?} catalog_tables={} catalog_records={} catalog_sha256={} state_sha256={} note=synthetic_fixture catalog_prep=Equipment,ShipCatalog dynamic_window=OwnedBefore,OwnedAfter technology_still_read_on_production_cache_hit json_bytes=not_wire_bytes",
            runtime.calls, catalog_tables, catalog_records, catalog_sha256, state_sha256
        );
    }
}

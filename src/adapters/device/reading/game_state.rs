//! 使用同一认证运行态连接读取并严格合成完整游戏状态。

use std::collections::{BTreeMap, BTreeSet};

use super::super::capabilities::{FullStateCapabilityError, validate_full_state_read_capabilities};
use super::super::capture::full_state::{
    FullStateCaptureError, FullStateCaptureEvidence, FullStateCaptureInput,
    FullStateCaptureRequest, write_full_state_capture,
};
use super::super::mapping::equipment::EquipmentMappingError;
use super::super::mapping::equipment_detail::EquipmentDetailMappingError;
use super::super::mapping::game_state::{
    GameStateMappingError, map_game_state_borrowed_with_scope,
};
use super::super::mapping::ship::ShipMappingError;
use super::super::mapping::ship_catalog::collect_static_skill_queries;
use super::super::probe::RuntimeProbeError;
use super::super::probe::readiness::{
    is_retryable_owned_state_readiness_error, is_retryable_ship_details_readiness_error,
    poll_retryable_snapshot, validate_complete_owned_state, validate_complete_ship_details,
};
use super::super::runtime::{
    AgentClient, CapabilitiesResult, ClientStage, MAX_SKILL_EFFECT_BATCH_SIZE, RetryDirective,
    RuntimeClientError, RuntimeProtocolError, RuntimeSkillEffectDetail, SessionEffect,
    SkillEffectBatchResult, SkillEffectQuery, SnapshotOwnedStateResult, SnapshotShipDetailsResult,
    validate_full_state_read_options,
};
use super::equipment::{
    EquipmentReadError, EquipmentReadResult,
    read_equipment_catalog_with_scope as read_runtime_equipment,
};
use super::ship_catalog::{
    ShipCatalogReadError, ShipCatalogReadResult,
    read_ship_catalog_scoped as read_runtime_ship_catalog,
};
use crate::application::{AppError, AppErrorCode};
use crate::domain::{GameState, RawRecordKey};

const OPTIONS_STAGE: &str = "game.read.options";
const EQUIPMENT_STAGE: &str = "game.read.equipment_catalog";
const SHIP_CATALOG_STAGE: &str = "game.read.ship_catalog";
const OWNED_BEFORE_STAGE: &str = "game.read.owned_before";
const SHIP_DETAILS_STAGE: &str = "game.read.ship_details";
const OWNED_AFTER_STAGE: &str = "game.read.owned_after";
const SHIP_SKILL_EFFECTS_STAGE: &str = "game.read.ship_skill_effects";
const CAPABILITIES_BEFORE_STAGE: &str = "game.read.capabilities_before";
const CAPABILITIES_AFTER_STAGE: &str = "game.read.capabilities_after";
const MAP_STATE_STAGE: &str = "game.map.full_state";
const CAPTURE_STATE_STAGE: &str = "game.capture.full_state";

/// 一次完整读取使用的超时、容量边界和目标模块身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GameReadOptions {
    read_scope: crate::domain::GameReadScope,
    timeout_ms: u32,
    max_ships: u32,
    max_equipments: u32,
    max_items: u32,
    expected_module_sha256: String,
    full_state_capture: Option<FullStateCaptureRequest>,
}

impl GameReadOptions {
    /// 创建在任何 RPC 发出前已经通过冻结协议校验的读取参数。
    pub(crate) fn new(
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
        expected_module_sha256: impl Into<String>,
    ) -> Result<Self, AppError> {
        let expected_module_sha256 = expected_module_sha256.into();
        validate_full_state_read_options(
            timeout_ms,
            max_ships,
            max_equipments,
            max_items,
            &expected_module_sha256,
        )
        .map_err(|source| {
            let code = match source.code {
                "timeout_out_of_range" | "snapshot_limit_out_of_range" => {
                    AppErrorCode::SettingsInvalid
                }
                _ => AppErrorCode::RuntimeIncompatible,
            };
            let protocol_code = source.code.to_owned();
            AppError::from_source(
                OPTIONS_STAGE,
                code,
                "完整状态读取参数不符合运行态协议",
                source,
            )
            .with_context("protocol_code", protocol_code)
        })?;

        Ok(Self {
            read_scope: crate::domain::GameReadScope::full(),
            timeout_ms,
            max_ships,
            max_equipments,
            max_items,
            expected_module_sha256,
            full_state_capture: None,
        })
    }

    pub(crate) fn with_read_scope(mut self, scope: crate::domain::GameReadScope) -> Self {
        self.read_scope = scope;
        self
    }

    /// 为本次读取附加显式的外部原始证据目标；默认读取不会保存私人正文。
    pub(crate) fn with_full_state_capture(mut self, request: FullStateCaptureRequest) -> Self {
        self.full_state_capture = Some(request);
        self
    }
}

type GameStateReadOutcome = Result<
    (
        GameState,
        EquipmentReadResult,
        ShipCatalogReadResult,
        CapabilitiesResult,
        Option<FullStateCaptureEvidence>,
        AccountCollectionRetries,
    ),
    AppError,
>;

/// 读取完整状态，并交还同一次读取验证过的运行态维护证据。
pub(crate) fn read_game_state_with_runtime_evidence(
    client: &mut AgentClient,
    options: &GameReadOptions,
    cached_equipment: Option<EquipmentReadResult>,
    cached_ship_catalog: Option<ShipCatalogReadResult>,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
) -> GameStateReadOutcome {
    read_game_state_with_evidence_and_progress(
        client,
        options,
        cached_equipment,
        cached_ship_catalog,
        progress,
    )
}

/// 正式账号采集位置发生的白名单重试次数。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AccountCollectionRetries {
    pub(crate) owned_before: u32,
    pub(crate) ship_details: u32,
}

fn read_game_state_with_evidence_and_progress<R: GameReadRuntime>(
    runtime: &mut R,
    options: &GameReadOptions,
    cached_equipment: Option<EquipmentReadResult>,
    cached_ship_catalog: Option<ShipCatalogReadResult>,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
) -> GameStateReadOutcome {
    progress(crate::application::OperationProgress::stage(
        "正在检查游戏读取能力",
    ));
    let capabilities_before = runtime.capabilities(options.timeout_ms).map_err(|source| {
        map_runtime_error(
            CAPABILITIES_BEFORE_STAGE,
            "读取运行态能力预检报告失败",
            source,
        )
    })?;

    // 同一认证会话冻结首轮验证过的静态目录，后续只刷新会变化的账号状态。
    progress(crate::application::OperationProgress::stage(
        "正在读取装备目录",
    ));
    let requested_scope = if options.full_state_capture.is_some() {
        crate::domain::GameReadScope::full()
    } else {
        options.read_scope
    };
    let equipment = runtime
        .read_equipment_catalog(
            options.timeout_ms,
            &options.expected_module_sha256,
            requested_scope,
            cached_equipment,
        )
        .map_err(map_equipment_error)?;
    progress(crate::application::OperationProgress::stage(
        "正在读取舰船静态目录",
    ));
    let ship_catalog = runtime
        .read_ship_catalog(
            options.timeout_ms,
            &options.expected_module_sha256,
            requested_scope.ship_technology(),
            cached_ship_catalog,
        )
        .map_err(map_ship_catalog_read_error)?;
    progress(crate::application::OperationProgress::stage(
        "正在读取账号前窗口",
    ));
    let (account_before, account_before_retries) = poll_retryable_snapshot(
        options.timeout_ms,
        "rpc.wait_account_before",
        "账号前窗口",
        |timeout_ms| {
            runtime.snapshot_account_before(
                timeout_ms,
                options.max_ships,
                options.max_equipments,
                options.max_items,
                &options.expected_module_sha256,
            )
        },
        is_retryable_account_before_error,
    )
    .map_err(|source| map_probe_read_error(OWNED_BEFORE_STAGE, "读取账号前窗口失败", source))?;
    let owned_before = account_before.owned_state;
    let ship_details = account_before.ship_details;
    validate_complete_owned_state(&owned_before)
        .map_err(|source| map_probe_read_error(OWNED_BEFORE_STAGE, "读取账号前窗口失败", source))?;
    validate_complete_ship_details(&ship_details)
        .map_err(|source| map_probe_read_error(SHIP_DETAILS_STAGE, "读取账号前窗口失败", source))?;
    let account_retries = AccountCollectionRetries {
        owned_before: account_before_retries,
        ship_details: 0,
    };
    progress(crate::application::OperationProgress::stage(
        "正在复核账号持有状态",
    ));
    let owned_after = runtime
        .snapshot_owned_state(
            options.timeout_ms,
            options.max_ships,
            options.max_equipments,
            options.max_items,
        )
        .map_err(|source| {
            map_runtime_error(OWNED_AFTER_STAGE, "读取账号持有状态后快照失败", source)
        })?;
    let read_scope =
        requested_scope.with_equipment_details(equipment.weapons_read(), equipment.skills_read());
    let ship_skill_effects = if read_scope.ship_skill_effects() {
        progress(crate::application::OperationProgress::stage(
            "正在读取舰船底层技能效果",
        ));
        read_ship_skill_effects(
            runtime,
            options.timeout_ms,
            &ship_catalog,
            &ship_details,
            equipment.raw_records().skills(),
            &options.expected_module_sha256,
        )
        .map_err(|source| {
            map_runtime_error(
                SHIP_SKILL_EFFECTS_STAGE,
                "读取舰船当前等级技能效果证据失败",
                source,
            )
        })?
    } else {
        progress(crate::application::OperationProgress::stage(
            "模板未请求舰船底层技能效果，跳过独立读取",
        ));
        Vec::new()
    };
    let weapon_count = equipment.raw_records().weapon_count();
    let skill_count = equipment
        .raw_records()
        .skill_count()
        .max(ship_skill_effects.len());
    progress(crate::application::OperationProgress::stage(
        "正在复核游戏读取能力",
    ));
    let capabilities_after = runtime.capabilities(options.timeout_ms).map_err(|source| {
        map_runtime_error(
            CAPABILITIES_AFTER_STAGE,
            "读取运行态能力复核报告失败",
            source,
        )
    })?;
    validate_full_state_read_capabilities(&capabilities_after, weapon_count, skill_count)
        .map_err(map_capability_error)?;

    let state = map_game_state_borrowed_with_scope(
        &owned_before,
        &ship_details,
        &owned_after,
        &equipment,
        &ship_catalog,
        &ship_skill_effects,
        &options.expected_module_sha256,
        read_scope,
    )
    .map_err(map_state_error)?;
    let full_state_capture_evidence = options
        .full_state_capture
        .as_ref()
        .map(|request| {
            write_full_state_capture(
                request,
                FullStateCaptureInput {
                    module_sha256: &options.expected_module_sha256,
                    game_state_content_sha256: state.source().content_sha256(),
                    capabilities_before: &capabilities_before,
                    owned_state_before: &owned_before,
                    ship_details: &ship_details,
                    owned_state_after: &owned_after,
                    equipment: &equipment,
                    ship_catalog: &ship_catalog,
                    ship_skill_effects: &ship_skill_effects,
                    capabilities_after: &capabilities_after,
                },
            )
            .map_err(map_capture_error)
        })
        .transpose()?;
    Ok((
        state,
        equipment,
        ship_catalog,
        capabilities_after,
        full_state_capture_evidence,
        account_retries,
    ))
}

fn map_capture_error(source: FullStateCaptureError) -> AppError {
    AppError::from_source(
        CAPTURE_STATE_STAGE,
        AppErrorCode::FullCheckFailed,
        "完整状态已通过映射，但原始黄金证据未能安全发布",
        source,
    )
    .with_context("component", "full_state_capture")
}

/// 为编排测试保留最窄设备读取接缝；生产实现仍直接复用 `AgentClient`。
trait GameReadRuntime {
    fn read_equipment_catalog(
        &mut self,
        timeout_ms: u32,
        expected_module_sha256: &str,
        scope: crate::domain::GameReadScope,
        cached: Option<EquipmentReadResult>,
    ) -> Result<EquipmentReadResult, EquipmentReadError>;

    fn read_ship_catalog(
        &mut self,
        timeout_ms: u32,
        expected_module_sha256: &str,
        technology: bool,
        cached: Option<ShipCatalogReadResult>,
    ) -> Result<ShipCatalogReadResult, ShipCatalogReadError>;

    fn snapshot_owned_state(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
    ) -> Result<SnapshotOwnedStateResult, RuntimeClientError>;

    fn snapshot_account_before(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
        expected_module_sha256: &str,
    ) -> Result<crate::adapters::device::runtime::AccountBeforeResult, RuntimeClientError>;

    fn snapshot_skill_effects(
        &mut self,
        timeout_ms: u32,
        skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError>;

    fn capabilities(&mut self, timeout_ms: u32) -> Result<CapabilitiesResult, RuntimeClientError>;
}

impl GameReadRuntime for AgentClient {
    fn read_equipment_catalog(
        &mut self,
        timeout_ms: u32,
        expected_module_sha256: &str,
        scope: crate::domain::GameReadScope,
        cached: Option<EquipmentReadResult>,
    ) -> Result<EquipmentReadResult, EquipmentReadError> {
        read_runtime_equipment(self, timeout_ms, expected_module_sha256, scope, cached)
    }

    fn read_ship_catalog(
        &mut self,
        timeout_ms: u32,
        expected_module_sha256: &str,
        technology: bool,
        cached: Option<ShipCatalogReadResult>,
    ) -> Result<ShipCatalogReadResult, ShipCatalogReadError> {
        read_runtime_ship_catalog(self, timeout_ms, expected_module_sha256, technology, cached)
    }

    fn snapshot_account_before(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
        expected_module_sha256: &str,
    ) -> Result<crate::adapters::device::runtime::AccountBeforeResult, RuntimeClientError> {
        AgentClient::snapshot_account_before(
            self,
            timeout_ms,
            max_ships,
            max_equipments,
            max_items,
            expected_module_sha256,
        )
    }

    fn snapshot_owned_state(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
    ) -> Result<SnapshotOwnedStateResult, RuntimeClientError> {
        AgentClient::snapshot_owned_state(self, timeout_ms, max_ships, max_equipments, max_items)
    }

    fn snapshot_skill_effects(
        &mut self,
        timeout_ms: u32,
        skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError> {
        AgentClient::snapshot_skill_effects(self, timeout_ms, skills, expected_module_sha256)
    }

    fn capabilities(&mut self, timeout_ms: u32) -> Result<CapabilitiesResult, RuntimeClientError> {
        AgentClient::capabilities(self, timeout_ms)
    }
}

fn read_ship_skill_effects<R: GameReadRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    ship_catalog: &ShipCatalogReadResult,
    ship_details: &SnapshotShipDetailsResult,
    reusable_records: &[RuntimeSkillEffectDetail],
    expected_module_sha256: &str,
) -> Result<Vec<RuntimeSkillEffectDetail>, RuntimeClientError> {
    let mut keys = collect_static_skill_queries(ship_catalog)
        .map_err(|source| {
            RuntimeProtocolError::new("ship_catalog_skill_query_invalid", source.to_string())
        })?
        .into_iter()
        .map(|query| (query.skill_id, query.level))
        .collect::<BTreeSet<_>>();
    keys.extend(
        collect_ship_skill_queries(ship_details)?
            .into_iter()
            .map(|query| (query.skill_id, query.level)),
    );
    let queries = keys
        .into_iter()
        .map(|(skill_id, level)| SkillEffectQuery::new(skill_id, level))
        .collect::<Result<Vec<_>, _>>()?;
    read_skill_effect_records(
        runtime,
        timeout_ms,
        &queries,
        reusable_records,
        expected_module_sha256,
    )
}

fn collect_ship_skill_queries(
    ship_details: &SnapshotShipDetailsResult,
) -> Result<Vec<SkillEffectQuery>, RuntimeProtocolError> {
    ship_details
        .ships
        .iter()
        .flat_map(|ship| &ship.skills)
        .map(|skill| (skill.effective_skill_id, skill.level))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|(skill_id, level)| SkillEffectQuery::new(skill_id, level))
        .collect()
}

fn read_skill_effect_records<R: GameReadRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    queries: &[SkillEffectQuery],
    reusable_records: &[RuntimeSkillEffectDetail],
    expected_module_sha256: &str,
) -> Result<Vec<RuntimeSkillEffectDetail>, RuntimeClientError> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }

    let requested_keys: BTreeSet<(u64, u32)> = queries
        .iter()
        .map(|query| (query.skill_id, query.level))
        .collect();
    let mut records = BTreeMap::new();
    for record in reusable_records
        .iter()
        .filter(|record| requested_keys.contains(&(record.skill_id, record.level)))
    {
        let key = (record.skill_id, record.level);
        if records.insert(key, record.clone()).is_some() {
            return Err(RuntimeProtocolError::new(
                "skill_effect_duplicate_reusable_record",
                format!(
                    "可复用技能效果证据重复: skill_id={}, level={}",
                    key.0, key.1
                ),
            )
            .into());
        }
    }

    let missing_queries: Vec<SkillEffectQuery> = queries
        .iter()
        .filter(|query| !records.contains_key(&(query.skill_id, query.level)))
        .cloned()
        .collect();
    for batch_queries in missing_queries.chunks(MAX_SKILL_EFFECT_BATCH_SIZE) {
        let batch =
            runtime.snapshot_skill_effects(timeout_ms, batch_queries, expected_module_sha256)?;
        batch.validate(batch_queries, expected_module_sha256)?;
        for record in batch.skills {
            let key = (record.skill_id, record.level);
            if records.insert(key, record).is_some() {
                return Err(RuntimeProtocolError::new(
                    "skill_effect_duplicate_record",
                    format!("技能效果响应重复: skill_id={}, level={}", key.0, key.1),
                )
                .into());
            }
        }
    }

    queries
        .iter()
        .map(|query| {
            records
                .remove(&(query.skill_id, query.level))
                .ok_or_else(|| {
                    RuntimeProtocolError::new(
                        "skill_effect_record_missing",
                        format!(
                            "技能效果响应缺少: skill_id={}, level={}",
                            query.skill_id, query.level
                        ),
                    )
                    .into()
                })
        })
        .collect()
}

fn is_retryable_account_before_error(error: &crate::adapters::device::runtime::AgentError) -> bool {
    is_retryable_owned_state_readiness_error(error)
        || is_retryable_ship_details_readiness_error(error)
}

fn map_probe_read_error(
    stage: &'static str,
    message: &'static str,
    source: RuntimeProbeError,
) -> AppError {
    match source {
        RuntimeProbeError::RuntimeClient(source) => map_runtime_error(stage, message, source),
        source => AppError::from_source(stage, AppErrorCode::RuntimeIncompatible, message, source),
    }
}

fn map_runtime_error(
    stage: &'static str,
    message: &'static str,
    source: RuntimeClientError,
) -> AppError {
    let code = classify_runtime_error(&source);
    let contexts = runtime_error_context(&source);
    let mut error = AppError::from_source(stage, code, message, source);
    for (key, value) in contexts {
        error = error.with_context(key, value);
    }
    error
}

pub(crate) fn classify_runtime_error(source: &RuntimeClientError) -> AppErrorCode {
    match source {
        RuntimeClientError::Io {
            stage:
                ClientStage::Connect
                | ClientStage::ConfigureSocket
                | ClientStage::ReadChallenge
                | ClientStage::WriteProof
                | ClientStage::ReadHandshake,
            ..
        }
        | RuntimeClientError::HandshakeAttemptsExhausted { .. }
        | RuntimeClientError::RandomSource { .. } => AppErrorCode::RuntimeBootstrapFailed,
        RuntimeClientError::Io {
            stage:
                ClientStage::WriteRequest | ClientStage::ReadResponse | ClientStage::ReadShutdownEof,
            ..
        }
        | RuntimeClientError::SessionUnusable => AppErrorCode::FullCheckFailed,
        RuntimeClientError::Agent { error, .. } if error.code.ends_with("_not_ready") => {
            AppErrorCode::GameNotReady
        }
        RuntimeClientError::Agent { .. } => AppErrorCode::FullCheckFailed,
        RuntimeClientError::Json { .. }
        | RuntimeClientError::FrameTooLarge { .. }
        | RuntimeClientError::SecureChannel { .. }
        | RuntimeClientError::Protocol(_) => AppErrorCode::RuntimeIncompatible,
    }
}

fn runtime_error_context(source: &RuntimeClientError) -> Vec<(&'static str, String)> {
    let mut contexts = vec![("runtime_code", source.code().to_owned())];
    match source {
        RuntimeClientError::Io { stage, .. }
        | RuntimeClientError::Json { stage, .. }
        | RuntimeClientError::FrameTooLarge { stage, .. }
        | RuntimeClientError::SecureChannel { stage, .. } => {
            contexts.push(("client_stage", stage.to_string()));
        }
        RuntimeClientError::HandshakeAttemptsExhausted { attempts, .. } => {
            contexts.push(("handshake_attempts", attempts.to_string()));
        }
        RuntimeClientError::Agent { request_id, error } => {
            contexts.push(("request_id", request_id.to_string()));
            contexts.push(("agent_stage", error.stage.clone()));
            contexts.push(("retry", retry_directive_name(error.retry).to_owned()));
            contexts.push((
                "session_effect",
                session_effect_name(error.session_effect).to_owned(),
            ));
        }
        RuntimeClientError::RandomSource { .. }
        | RuntimeClientError::Protocol(_)
        | RuntimeClientError::SessionUnusable => {}
    }
    contexts
}

const fn retry_directive_name(value: RetryDirective) -> &'static str {
    match value {
        RetryDirective::Never => "never",
        RetryDirective::SameRequest => "same_request",
        RetryDirective::AfterReconnect => "after_reconnect",
    }
}

const fn session_effect_name(value: SessionEffect) -> &'static str {
    match value {
        SessionEffect::Unchanged => "unchanged",
        SessionEffect::MustClose => "must_close",
        SessionEffect::StateUnknown => "state_unknown",
    }
}

fn map_equipment_error(source: EquipmentReadError) -> AppError {
    let code = match &source {
        EquipmentReadError::Runtime(source) => classify_runtime_error(source),
        EquipmentReadError::Mapping(_) | EquipmentReadError::Protocol(_) => {
            AppErrorCode::RuntimeIncompatible
        }
        EquipmentReadError::EncodeRawRecords(_) | EquipmentReadError::IncompleteDetails(_) => {
            AppErrorCode::FullCheckFailed
        }
    };
    let contexts = match &source {
        EquipmentReadError::Runtime(source) => runtime_error_context(source),
        EquipmentReadError::Mapping(source) => equipment_mapping_context(source),
        EquipmentReadError::Protocol(source) => vec![("protocol_code", source.code.to_owned())],
        EquipmentReadError::IncompleteDetails(source) => {
            let mut contexts = vec![
                ("detail_kind", source.kind().to_owned()),
                ("failure_count", source.failures().len().to_string()),
            ];
            if let Some(first) = source.failures().first() {
                contexts.push(("first_failure_key", first.key().to_owned()));
                contexts.push((
                    "first_failure_diagnostic_count",
                    first.diagnostics().len().to_string(),
                ));
            }
            contexts
        }
        EquipmentReadError::EncodeRawRecords(_) => {
            vec![("operation", "encode_raw_records".to_owned())]
        }
    };
    let mut error =
        AppError::from_source(EQUIPMENT_STAGE, code, "读取完整装备目录及详情失败", source)
            .with_context("component", "equipment_catalog");
    for (key, value) in contexts {
        error = error.with_context(key, value);
    }
    error
}

fn map_ship_catalog_read_error(source: ShipCatalogReadError) -> AppError {
    let code = match &source {
        ShipCatalogReadError::PageRuntime { source, .. } => classify_runtime_error(source),
        ShipCatalogReadError::PageProtocol { .. } | ShipCatalogReadError::Protocol(_) => {
            AppErrorCode::RuntimeIncompatible
        }
        ShipCatalogReadError::IncompletePage { .. }
        | ShipCatalogReadError::TotalCountChanged { .. }
        | ShipCatalogReadError::DuplicateRecord { .. }
        | ShipCatalogReadError::RecordCountMismatch { .. }
        | ShipCatalogReadError::EncodeContentDigest(_) => AppErrorCode::FullCheckFailed,
    };
    AppError::from_source(
        SHIP_CATALOG_STAGE,
        code,
        "完整舰船静态目录读取或校验失败",
        source,
    )
}

fn equipment_mapping_context(source: &EquipmentMappingError) -> Vec<(&'static str, String)> {
    match source {
        EquipmentMappingError::Protocol(source) => {
            vec![("protocol_code", source.code.to_owned())]
        }
        EquipmentMappingError::PagesMissing { kind }
        | EquipmentMappingError::TotalCountMissing { kind } => {
            vec![("catalog_kind", (*kind).to_owned())]
        }
        EquipmentMappingError::PageGap {
            kind,
            expected,
            actual,
        }
        | EquipmentMappingError::TotalCountMismatch {
            kind,
            expected,
            actual,
        } => vec![
            ("catalog_kind", (*kind).to_owned()),
            ("expected_count", expected.to_string()),
            ("actual_count", actual.to_string()),
        ],
        EquipmentMappingError::UnexpectedPage { kind, start_index }
        | EquipmentMappingError::PageIncomplete { kind, start_index } => vec![
            ("catalog_kind", (*kind).to_owned()),
            ("start_index", start_index.to_string()),
        ],
        EquipmentMappingError::CatalogOrderInvalid {
            kind,
            previous,
            actual,
        } => vec![
            ("catalog_kind", (*kind).to_owned()),
            ("previous_id", previous.to_string()),
            ("actual_id", actual.to_string()),
        ],
        EquipmentMappingError::CatalogCoverage {
            kind,
            expected,
            actual,
        } => vec![
            ("catalog_kind", (*kind).to_owned()),
            ("expected_count", expected.to_string()),
            ("actual_count", actual.to_string()),
        ],
        EquipmentMappingError::IncompleteConfig { config_id, errors } => vec![
            ("config_id", config_id.to_string()),
            ("read_error_count", errors.len().to_string()),
        ],
        EquipmentMappingError::InvalidConfigField {
            config_id, field, ..
        } => vec![
            ("config_id", config_id.to_string()),
            ("field", (*field).to_owned()),
        ],
        EquipmentMappingError::RootConfigMismatch {
            config_id,
            field,
            expected,
            actual,
        } => {
            let mut contexts = vec![
                ("config_id", config_id.to_string()),
                ("field", (*field).to_owned()),
                ("expected_config_id", expected.to_string()),
            ];
            if let Some(actual) = actual {
                contexts.push(("actual_config_id", actual.to_string()));
            }
            contexts
        }
        EquipmentMappingError::MissingReferenceName { namespace, key, .. }
        | EquipmentMappingError::ReferenceNameState { namespace, key } => vec![
            ("reference_namespace", (*namespace).to_owned()),
            ("reference_key", key.clone()),
        ],
        EquipmentMappingError::FamilyChainInvalid { family_id, .. } => {
            vec![("family_id", family_id.to_string())]
        }
        EquipmentMappingError::RecipeTargetMissing {
            recipe_id,
            equipment_id,
        } => vec![
            ("recipe_id", recipe_id.to_string()),
            ("equipment_id", equipment_id.to_string()),
        ],
        EquipmentMappingError::Model(_) | EquipmentMappingError::Encode(_) => Vec::new(),
    }
}

fn map_capability_error(source: FullStateCapabilityError) -> AppError {
    let capability = source.capability();
    let available = source.available();
    let reason_code = source.reason_code().map(str::to_owned);
    let code = match &source {
        FullStateCapabilityError::Missing { .. } => AppErrorCode::CapabilityMissing,
        FullStateCapabilityError::NotReady { reason_code, .. }
            if reason_code.ends_with("_not_ready") || reason_code == "awaiting_tolua_update" =>
        {
            AppErrorCode::GameNotReady
        }
        FullStateCapabilityError::NotReady { .. } => AppErrorCode::CapabilityMissing,
    };
    let mut error = AppError::from_source(
        CAPABILITIES_AFTER_STAGE,
        code,
        "运行态未提供可用的完整状态读取能力",
        source,
    )
    .with_context("capability", capability);
    if let Some(available) = available {
        error = error.with_context("available", available.to_string());
    }
    if let Some(reason_code) = reason_code {
        error = error.with_context("reason_code", reason_code);
    }
    error
}

fn map_state_error(source: GameStateMappingError) -> AppError {
    let code = match &source {
        GameStateMappingError::Protocol(_)
        | GameStateMappingError::Ship(ShipMappingError::Protocol(_))
        | GameStateMappingError::EquipmentDetails(_)
        | GameStateMappingError::ModuleMismatch { .. } => AppErrorCode::RuntimeIncompatible,
        GameStateMappingError::EquipmentConfigMissing { .. }
        | GameStateMappingError::EquipmentDetailMissing { .. } => AppErrorCode::EquipmentNotFound,
        GameStateMappingError::EnhanceLevelMismatch { .. } => AppErrorCode::EquipmentStateChanged,
        GameStateMappingError::DuplicateCatalogConfig { .. }
        | GameStateMappingError::DuplicateWarehouseConfig { .. } => AppErrorCode::EquipmentConflict,
        GameStateMappingError::EnhanceLevelOutOfRange { .. } => AppErrorCode::EnhanceInvalid,
        _ => AppErrorCode::FullCheckFailed,
    };
    let contexts = state_error_context(&source);
    let mut error = AppError::from_source(
        MAP_STATE_STAGE,
        code,
        "运行态数据无法形成一致的完整游戏状态",
        source,
    )
    .with_context("component", "game_state");
    for (key, value) in contexts {
        error = error.with_context(key, value);
    }
    error
}

fn state_error_context(source: &GameStateMappingError) -> Vec<(&'static str, String)> {
    match source {
        GameStateMappingError::Protocol(source) => {
            vec![("protocol_code", source.code.to_owned())]
        }
        GameStateMappingError::Ship(ShipMappingError::Protocol(source)) => {
            vec![("protocol_code", source.code.to_owned())]
        }
        GameStateMappingError::ShipCatalog(source) => {
            vec![("ship_catalog_error", source.to_string())]
        }
        GameStateMappingError::ModuleMismatch {
            component,
            expected,
            actual,
        } => vec![
            ("source_component", (*component).to_owned()),
            ("expected_module_sha256", expected.clone()),
            ("actual_module_sha256", actual.clone()),
        ],
        GameStateMappingError::OwnedStateChanged {
            before_sha256,
            after_sha256,
        } => vec![
            ("owned_before_sha256", before_sha256.clone()),
            ("owned_after_sha256", after_sha256.clone()),
        ],
        GameStateMappingError::DuplicateCatalogConfig { config_id }
        | GameStateMappingError::DuplicateWarehouseConfig { config_id } => {
            vec![("config_id", config_id.to_string())]
        }
        GameStateMappingError::EquipmentConfigMissing { context, config_id } => vec![
            ("object", context.clone()),
            ("config_id", config_id.to_string()),
        ],
        GameStateMappingError::EnhanceLevelOutOfRange { context, value } => vec![
            ("object", context.clone()),
            ("enhance_level", value.to_string()),
        ],
        GameStateMappingError::EnhanceLevelMismatch {
            context,
            config_id,
            actual,
            expected,
        } => vec![
            ("object", context.clone()),
            ("config_id", config_id.to_string()),
            ("actual_enhance_level", actual.to_string()),
            ("expected_enhance_level", expected.to_string()),
        ],
        GameStateMappingError::EquipmentDetailMissing { config_id, detail } => vec![
            ("config_id", config_id.to_string()),
            ("detail", detail.clone()),
        ],
        GameStateMappingError::BagRecipeMismatch { recipe_id } => {
            vec![("recipe_id", recipe_id.to_string())]
        }
        GameStateMappingError::Ship(source) => ship_mapping_context(source),
        GameStateMappingError::EquipmentDetails(source) => equipment_detail_context(source),
        GameStateMappingError::Model(_) | GameStateMappingError::Encode(_) => Vec::new(),
    }
}

fn ship_mapping_context(source: &ShipMappingError) -> Vec<(&'static str, String)> {
    match source {
        ShipMappingError::Protocol(source) => vec![("protocol_code", source.code.to_owned())],
        ShipMappingError::OwnedStateIncomplete {
            dock_errors,
            warehouse_errors,
            bag_errors,
        } => vec![
            ("dock_error_count", dock_errors.to_string()),
            ("warehouse_error_count", warehouse_errors.to_string()),
            ("bag_error_count", bag_errors.to_string()),
        ],
        ShipMappingError::DetailsIncomplete {
            read_errors,
            truncated,
        } => vec![
            ("read_error_count", read_errors.to_string()),
            ("truncated", truncated.to_string()),
        ],
        ShipMappingError::ShipCountMismatch {
            owned_count,
            detail_count,
        } => vec![
            ("owned_ship_count", owned_count.to_string()),
            ("detail_ship_count", detail_count.to_string()),
        ],
        ShipMappingError::ShipOrderMismatch {
            owned_ship_id,
            detail_ship_id,
        } => vec![
            ("owned_ship_id", owned_ship_id.to_string()),
            ("detail_ship_id", detail_ship_id.to_string()),
        ],
        ShipMappingError::ShipFieldMismatch {
            ship_id,
            field,
            owned_value,
            detail_value,
        } => vec![
            ("ship_id", ship_id.to_string()),
            ("field", (*field).to_owned()),
            ("owned_value", owned_value.to_string()),
            ("detail_value", detail_value.to_string()),
        ],
        ShipMappingError::SkillCountMismatch {
            ship_id,
            owned_count,
            detail_count,
        } => vec![
            ("ship_id", ship_id.to_string()),
            ("owned_skill_count", owned_count.to_string()),
            ("detail_skill_count", detail_count.to_string()),
        ],
        ShipMappingError::SkillOrderMismatch {
            ship_id,
            owned_skill_id,
            detail_skill_id,
        } => vec![
            ("ship_id", ship_id.to_string()),
            ("owned_skill_id", owned_skill_id.to_string()),
            ("detail_skill_id", detail_skill_id.to_string()),
        ],
        ShipMappingError::SkillProgressMismatch {
            ship_id,
            skill_id,
            owned_level,
            detail_level,
            owned_experience,
            detail_experience,
        } => vec![
            ("ship_id", ship_id.to_string()),
            ("skill_id", skill_id.to_string()),
            ("owned_level", owned_level.to_string()),
            ("detail_level", detail_level.to_string()),
            ("owned_experience", owned_experience.to_string()),
            ("detail_experience", detail_experience.to_string()),
        ],
        ShipMappingError::SlotFieldOutOfRange {
            ship_id,
            slot_index,
            field,
            value,
        } => vec![
            ("ship_id", ship_id.to_string()),
            ("slot_index", slot_index.to_string()),
            ("field", (*field).to_owned()),
            ("value", value.to_string()),
        ],
        ShipMappingError::DuplicateSkillEffectEvidence { skill_id, level }
        | ShipMappingError::EncodeSkillEffectEvidence {
            skill_id, level, ..
        } => vec![
            ("skill_id", skill_id.to_string()),
            ("skill_level", level.to_string()),
        ],
        ShipMappingError::Model(_) | ShipMappingError::Encode(_) => Vec::new(),
    }
}

fn equipment_detail_context(source: &EquipmentDetailMappingError) -> Vec<(&'static str, String)> {
    match source {
        EquipmentDetailMappingError::WeaponField {
            weapon_id, field, ..
        } => vec![
            ("weapon_id", weapon_id.to_string()),
            ("field", (*field).to_owned()),
        ],
        EquipmentDetailMappingError::SkillField {
            skill_id,
            level,
            field,
            ..
        } => vec![
            ("skill_id", skill_id.to_string()),
            ("skill_level", level.to_string()),
            ("field", (*field).to_owned()),
        ],
        EquipmentDetailMappingError::WeaponOrder { previous, current } => {
            let mut contexts = vec![("weapon_id", current.to_string())];
            if let Some(previous) = previous {
                contexts.push(("previous_weapon_id", previous.to_string()));
            }
            contexts
        }
        EquipmentDetailMappingError::SkillOrder { previous, current } => {
            let mut contexts = vec![
                ("skill_id", current.0.to_string()),
                ("skill_level", current.1.to_string()),
            ];
            if let Some(previous) = previous {
                contexts.push(("previous_skill_id", previous.0.to_string()));
                contexts.push(("previous_skill_level", previous.1.to_string()));
            }
            contexts
        }
        EquipmentDetailMappingError::RawRecordKey { .. } => {
            vec![("object", "raw_record_key".to_owned())]
        }
        EquipmentDetailMappingError::EncodeRawRecord { key, .. }
        | EquipmentDetailMappingError::DuplicateRawRecord { key } => raw_record_context(key),
    }
}

fn raw_record_context(key: &RawRecordKey) -> Vec<(&'static str, String)> {
    match key {
        RawRecordKey::EquipmentConfig(config_id) => vec![
            ("record_kind", "equipment_config".to_owned()),
            ("config_id", config_id.get().to_string()),
        ],
        RawRecordKey::EquipmentRecipe(recipe_id) => vec![
            ("record_kind", "equipment_recipe".to_owned()),
            ("recipe_id", recipe_id.to_string()),
        ],
        RawRecordKey::EquipmentReferenceNames => {
            vec![("record_kind", "equipment_reference_names".to_owned())]
        }
        RawRecordKey::EquipmentWeapon(weapon_id) => vec![
            ("record_kind", "equipment_weapon".to_owned()),
            ("weapon_id", weapon_id.to_string()),
        ],
        RawRecordKey::EquipmentSkill { skill_id, level } => vec![
            ("record_kind", "equipment_skill".to_owned()),
            ("skill_id", skill_id.to_string()),
            ("skill_level", level.to_string()),
        ],
        RawRecordKey::ShipCatalog {
            table_key,
            record_id,
        } => vec![
            ("record_kind", "ship_catalog".to_owned()),
            ("table_key", table_key.clone()),
            ("record_id", record_id.to_string()),
        ],
        RawRecordKey::ShipSkill { skill_id, level } => vec![
            ("record_kind", "ship_skill".to_owned()),
            ("skill_id", skill_id.to_string()),
            ("skill_level", level.to_string()),
        ],
    }
}

#[cfg(test)]
mod tests;

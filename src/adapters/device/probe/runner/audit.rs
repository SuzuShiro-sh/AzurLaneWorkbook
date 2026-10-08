//! 在已建立的生产会话上执行审计探针。日常读取不调用这里。

use serde_json::json;

use super::super::super::runtime::AgentClient;
#[cfg(target_os = "windows")]
use super::super::finish_probe_execution_after_unload;
#[cfg(target_os = "windows")]
use super::super::journal::journal_error_summary;
use super::super::process_evidence::validate_preserved_process_identity;
use super::super::readiness::{
    AgentReadiness, summarize_ship_growth, validate_complete_owned_state,
    validate_complete_ship_details,
};
use super::super::{RuntimeProbeError, ShipGrowthSummary, sha256_json};
use super::{AuthenticatedAgent, ProbeCoreResult, ProductionSession};
use crate::adapters::device::mapping::ship::map_ship_roster_with_skill_effects;
use crate::adapters::device::reading::game_state::read_game_state_with_runtime_evidence;
use crate::adapters::device::runtime::{
    LiveProtocolProbeResult, SnapshotBagResult, SnapshotOwnedStateResult, SnapshotShipDetailsResult,
};
use std::net::{IpAddr, SocketAddr};

/// 执行只读验证，并在认证成功后的所有退出路径上优先安全卸载 Agent。
pub(in crate::adapters::device::probe) fn execute(
    session: &mut ProductionSession,
) -> Result<ProbeCoreResult, RuntimeProbeError> {
    session.observe_target()?;
    #[cfg(target_os = "windows")]
    session.reject_live_resident()?;
    let prepared = session.prepare_deployed_handshake()?;
    let address = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), prepared.forward_port);
    AgentClient::probe_oversized_unauthenticated_frame(
        address,
        session.session_id,
        &session.session_secret,
        session.options.connect_timeout,
    )?;
    let mut authenticated = session.finish_agent_handshake(prepared, true)?;
    let execution: Result<ProbeCoreResult, RuntimeProbeError> = session
        .wait_for_agent_readiness(&mut authenticated)
        .and_then(|readiness| execute_authenticated(session, &mut authenticated, readiness));

    #[cfg(target_os = "windows")]
    {
        let mut journal_errors: Vec<String> = Vec::new();
        let unload = session.unload_agent_gracefully(authenticated, &mut journal_errors);
        if let Err(source) = &unload {
            let message: String = journal_error_summary(source);
            if let Err(journal_error) = session.journal.record(
                "session.shutdown.failure",
                "error",
                json!({"message": message}),
            ) {
                journal_errors.push(journal_error.to_string());
            }
        }
        finish_probe_execution_after_unload(
            execution,
            unload,
            session.journal.path.clone(),
            journal_errors,
        )
    }

    #[cfg(not(target_os = "windows"))]
    {
        drop(authenticated);
        execution
    }
}

fn execute_authenticated(
    session: &mut ProductionSession,
    authenticated: &mut AuthenticatedAgent,
    readiness: AgentReadiness,
) -> Result<ProbeCoreResult, RuntimeProbeError> {
    let AgentReadiness { health, sample } = readiness;
    let bag_readiness_retries = sample.bag_retries;
    let _ready_bag = sample.bag.complete;
    let loader_receipt = authenticated.loader_receipt.clone();
    let forward_port: u16 = authenticated.forward_port;
    let handshake_attempts: u32 = authenticated.handshake_attempts;
    let oversized_frame_closed_connection: bool = authenticated.oversized_frame_closed_connection;
    let before_load = authenticated.before_load.clone();
    let client = &mut authenticated.client;
    let expected_module_sha256: String = session.profile.bootstrap().module_sha256().to_owned();
    let game_read_options = session.game_read_options(1)?;
    let (game_state, equipment, _ship_catalog, capabilities, capture, account_retries) =
        read_game_state_with_runtime_evidence(client, &game_read_options, None, None, &mut |_| {})?;
    let owned_state_readiness_retries = account_retries.owned_before;
    let ship_details_readiness_retries = account_retries.ship_details;
    if let Some(capture) = &capture {
        session.journal.record(
            "full_state.capture.complete",
            "ok",
            json!({
                "schema_version": capture.schema_version(),
                "path": capture.path(),
                "size_bytes": capture.size_bytes(),
                "sha256": capture.sha256(),
                "session_id": capture.session_id(),
                "read_index": capture.read_index(),
            }),
        )?;
    }
    let game_state_content_sha256 = game_state.source().content_sha256().to_owned();

    let mut snapshots: Vec<SnapshotBagResult> = Vec::with_capacity(3);
    let mut snapshot_hashes: Vec<String> = Vec::with_capacity(3);
    // 准备阶段可能物化客户端静态表，最终证据只比较全部读取就绪后的连续样本。
    for _ in 0..3 {
        snapshots.push(client.snapshot_bag(session.options.timeout_ms, session.options.max_items)?);
    }
    for snapshot in &snapshots {
        if !snapshot.complete {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "rpc.snapshot_bag",
                message: format!(
                    "快照不完整: truncated={}, read_errors={}",
                    snapshot.truncated,
                    snapshot.read_errors.len()
                ),
            });
        }
        snapshot_hashes.push(sha256_json(&snapshot, "snapshot.semantic_hash")?);
    }
    if !snapshot_hashes
        .windows(2)
        .all(|window: &[String]| window[0] == window[1])
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "rpc.snapshot_consistency",
            message: format!("连续三次背包语义哈希不一致: {snapshot_hashes:?}"),
        });
    }
    let snapshot_count: u32 = snapshots[0].count;
    let snapshots_complete: bool = snapshots.iter().all(|snapshot| snapshot.complete);

    let mut owned_states: Vec<SnapshotOwnedStateResult> = Vec::with_capacity(3);
    let mut owned_state_hashes: Vec<String> = Vec::with_capacity(3);
    let mut ship_details: Vec<SnapshotShipDetailsResult> = Vec::with_capacity(3);
    let mut ship_detail_hashes: Vec<String> = Vec::with_capacity(3);
    let mut ship_roster_hashes: Vec<String> = Vec::with_capacity(3);
    for _ in 0..3 {
        let account_before = client.snapshot_account_before(
            session.options.timeout_ms,
            session.options.max_items,
            session.options.max_items,
            session.options.max_items,
            &expected_module_sha256,
        )?;
        let owned_state = account_before.owned_state;
        let details = account_before.ship_details;
        validate_complete_owned_state(&owned_state)?;
        validate_complete_ship_details(&details)?;
        let roster = map_ship_roster_with_skill_effects(&owned_state, &details, &[])?;
        ship_roster_hashes.push(roster.source().content_sha256().to_owned());
        owned_states.push(owned_state);
        ship_details.push(details);
    }
    for snapshot in &owned_states {
        validate_complete_owned_state(snapshot)?;
        owned_state_hashes.push(sha256_json(snapshot, "owned_state_snapshot.semantic_hash")?);
    }
    if !owned_state_hashes
        .windows(2)
        .all(|window: &[String]| window[0] == window[1])
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "rpc.owned_state_consistency",
            message: format!("连续三次完整运行态语义哈希不一致: {owned_state_hashes:?}"),
        });
    }
    for snapshot in &ship_details {
        validate_complete_ship_details(snapshot)?;
        ship_detail_hashes.push(sha256_json(snapshot, "ship_details.semantic_hash")?);
    }
    if !ship_detail_hashes
        .windows(2)
        .all(|window: &[String]| window[0] == window[1])
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "rpc.ship_details_consistency",
            message: format!("连续三次舰船详情语义哈希不一致: {ship_detail_hashes:?}"),
        });
    }
    if !ship_roster_hashes
        .windows(2)
        .all(|window: &[String]| window[0] == window[1])
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "rpc.ship_roster_consistency",
            message: format!("连续三次规范舰船名册哈希不一致: {ship_roster_hashes:?}"),
        });
    }
    let dock_count: u32 = owned_states[0].dock.count;
    let ship_detail_count: u32 = ship_details[0].count;
    let ship_growth_summary: ShipGrowthSummary =
        summarize_ship_growth(&owned_states[0].dock.ships)?;
    let warehouse_count: u32 = owned_states[0].warehouse.count;
    let owned_state_bag_count: u32 = owned_states[0].bag.count;
    let owned_state_complete: bool = owned_states.iter().all(|snapshot| snapshot.complete);
    let after_load = session
        .bridge
        .collect_process_evidence(session.target_pid)?;
    validate_preserved_process_identity(
        &after_load,
        session.target_pid,
        session.expected_process_start_time,
        "target.after_load_identity",
    )?;
    session.journal.record(
        "target.after_load",
        "ok",
        serde_json::to_value(&after_load).map_err(|source| RuntimeProbeError::Json {
            stage: "journal.after_load",
            source,
        })?,
    )?;

    let protocol_failure_probes: LiveProtocolProbeResult =
        client.run_live_protocol_probes(oversized_frame_closed_connection)?;
    session.journal.record(
        "rpc.complete",
        "ok",
        json!({
            "snapshot_hashes": snapshot_hashes,
            "snapshot_count": snapshot_count,
            "handshake_attempts": handshake_attempts,
            "bag_readiness_retries": bag_readiness_retries,
            "owned_state_readiness_retries": owned_state_readiness_retries,
            "owned_state_hashes": owned_state_hashes,
            "ship_details_readiness_retries": ship_details_readiness_retries,
            "ship_detail_hashes": ship_detail_hashes,
            "ship_roster_hashes": ship_roster_hashes,
            "game_state_content_sha256": game_state_content_sha256,
            "dock_count": dock_count,
            "ship_detail_count": ship_detail_count,
            "ship_growth_summary": ship_growth_summary,
            "warehouse_count": warehouse_count,
            "owned_state_bag_count": owned_state_bag_count,
            "equipment": {
                "catalog_schema_version": equipment.catalog().schema_version(),
                "catalog_content_sha256": equipment.catalog().source().content_sha256(),
                "raw_content_sha256": equipment.raw_content_sha256(),
                "family_count": equipment.catalog().families().len(),
                "config_count": equipment.catalog().config_count(),
                "recipe_count": equipment.catalog().recipes().len(),
            },
            "protocol_failure_probes": protocol_failure_probes,
        }),
    )?;

    Ok(ProbeCoreResult {
        loader_receipt,
        forward_port,
        handshake_attempts,
        health,
        capabilities,
        bag_readiness_retries,
        snapshot_hashes,
        snapshot_count,
        snapshots_complete,
        owned_state_readiness_retries,
        owned_state_hashes,
        ship_details_readiness_retries,
        ship_detail_hashes,
        ship_roster_hashes,
        game_state_content_sha256,
        dock_count,
        ship_detail_count,
        ship_growth_summary,
        warehouse_count,
        owned_state_bag_count,
        owned_state_complete,
        equipment,
        full_state_capture: capture,
        protocol_failure_probes,
        before_load,
        after_load,
    })
}

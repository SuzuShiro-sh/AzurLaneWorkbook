//! 覆盖便携运行时会话、资产发现和清理语义的单元测试。
#[cfg(not(target_os = "windows"))]
use super::validate_instance_index;
#[cfg(target_os = "windows")]
use suzushiro_emulator::validate_instance_index;

#[cfg(target_os = "windows")]
use std::env;
#[cfg(target_os = "windows")]
use std::error::Error;
use std::fs;
#[cfg(target_os = "windows")]
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::{Duration, Instant};

use serde_json::Value;
#[cfg(target_os = "windows")]
use serde_json::json;

use super::{
    AdbBundleInspection, ManagerEvidence, PortableAdbEvidence, PortableCleanupEvidence,
    PortableMode, PortableProbeError, PortableProbeOptions, PortableProbeReport, TARGET_PACKAGE,
    TargetEvidence, adb_game_launch_arguments, load_and_inspect_configured_adb_bundle,
    map_runtime_error, select_profile_package, validate_serial,
};
#[cfg(target_os = "windows")]
use super::{PortableRuntimeSession, PortableSessionShutdownEvidence};
use crate::adapters::device::EquipmentSampleEvidence;
use crate::adapters::device::probe::RuntimeProbeError;
#[cfg(target_os = "windows")]
use crate::adapters::device::probe::{
    CleanupEvidence, GracefulUnloadEvidence, ProcessEvidence, RuntimeSessionShutdownEvidence,
    RuntimeShutdownMethodEvidence,
};
#[cfg(target_os = "windows")]
use crate::adapters::device::runtime::{
    EquipmentCommandAction, EquipmentCommandEquipment, EquipmentCommandPhase,
    EquipmentCommandReceipt, EquipmentCommandStatus, RuntimeEquipmentCommand,
};
#[cfg(target_os = "windows")]
use crate::adapters::device::session::SessionId;
#[cfg(target_os = "windows")]
use crate::adapters::settings::Settings;
use crate::application::{AppError, AppErrorCode};
#[cfg(target_os = "windows")]
use crate::domain::GameState;
#[cfg(target_os = "windows")]
use suzushiro_content_digest::sha256_sorted_json;

#[cfg(target_os = "windows")]
type LiveEquipmentResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// 真机回环只保存构造严格局部前态所需的固定字段。
#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
struct LiveEquipmentCandidate {
    ship_instance_id: u64,
    ship_name: String,
    slot_index: u32,
    equipment_id: u64,
    config_id: u64,
    enhance_level: u32,
    warehouse_quantity: u64,
    equipment_capacity: u64,
    equipment_limit: u64,
}

/// 真机写入前由调用命令显式锁定的候选，不允许测试重新选择其他对象。
#[cfg(target_os = "windows")]
#[derive(Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LiveEquipmentExpectation {
    schema_version: u32,
    instance_index: String,
    serial: String,
    package_name: String,
    process_id: u32,
    process_start_time: u64,
    state_content_sha256: String,
    owned_state_content_sha256: String,
    module_sha256: String,
    ship_instance_id: u64,
    slot_index: u32,
    equipment_id: u64,
    config_id: u64,
    enhance_level: u32,
    warehouse_quantity: u64,
    equipment_capacity: u64,
    equipment_limit: u64,
}

/// 模式名只接受稳定小写值，并可无损返回 CLI 文本。
#[test]
fn mode_parser_accepts_only_stable_values() {
    assert_eq!(PortableMode::from_str("auto").unwrap().as_str(), "auto");
    assert_eq!(PortableMode::from_str("manual").unwrap().as_str(), "manual");
    assert!(PortableMode::from_str("AUTO").is_err());
}

/// 已完成关闭的便携会话重复 shutdown 时返回同一证据，不再访问已经释放的资源。
#[cfg(target_os = "windows")]
#[test]
fn completed_runtime_session_shutdown_is_idempotent() {
    let expected: PortableSessionShutdownEvidence = PortableSessionShutdownEvidence {
        runtime: RuntimeSessionShutdownEvidence {
            session_id: "00112233445566778899aabbccddeeff"
                .parse::<SessionId>()
                .unwrap(),
            read_count: 2,
            method: RuntimeShutdownMethodEvidence::Graceful {
                unload: GracefulUnloadEvidence {
                    process_id: 12_345,
                    process_start_time: 77_777,
                    worker_tid: 12_400,
                    worker_start_time: 77_888,
                    agent_mapping_name: "/memfd:fixture (deleted)".to_owned(),
                    inert_anonymous_overlap_count: 0,
                    reused_address_overlap_count: 0,
                },
                journal_errors: Vec::new(),
            },
            process: ProcessEvidence {
                process_id: 12_345,
                process_start_time: 77_777,
                tracer_pid: 0,
                thread_count: 8,
                maps_sha256: "0".repeat(64),
                azlw_map_lines: Vec::new(),
                azlw_thread_names: Vec::new(),
                azlw_socket_lines: Vec::new(),
                azlw_process_lines: Vec::new(),
            },
            cleanup: CleanupEvidence {
                forward_removed: true,
                forward_list_restored: true,
                device_session_removed: true,
                host_session_removed: true,
                old_process_stopped: false,
                game_restarted: false,
            },
            journal_path: PathBuf::from("data/logs/runtime-fixture.jsonl"),
        },
        adb: PortableCleanupEvidence {
            adb_process_stopped: true,
            adb_port_released: true,
            adb_temporary_root_removed: true,
        },
    };
    let mut session = PortableRuntimeSession {
        runtime: None,
        runtime_shutdown: None,
        prepared: None,
        shutdown_evidence: Some(expected.clone()),
    };

    assert_eq!(session.shutdown().unwrap(), &expected);
    assert_eq!(session.shutdown().unwrap(), &expected);
    let mut recovered = expected.clone();
    recovered.runtime.method = RuntimeShutdownMethodEvidence::RestartFallback {
        unload_error: "固定卸载失败".to_owned(),
        journal_errors: vec!["固定日志失败".to_owned()],
    };
    recovered.runtime.cleanup.game_restarted = true;
    recovered.runtime.cleanup.old_process_stopped = true;
    let mut recovered_session = PortableRuntimeSession {
        runtime: None,
        runtime_shutdown: None,
        prepared: None,
        shutdown_evidence: Some(recovered.clone()),
    };
    for _ in 0..2 {
        let error = recovered_session.shutdown().unwrap_err();
        assert!(matches!(error, PortableProbeError::RuntimeShutdownFailed {
            message, journal_path, game_restarted: true,
        } if message.contains("固定卸载失败") && message.contains("固定日志失败")
            && journal_path == recovered.runtime.journal_path));
        assert!(recovered_session.runtime.is_none());
        assert!(recovered_session.prepared.is_none());
        assert_eq!(
            recovered_session.shutdown_evidence.as_ref(),
            Some(&recovered)
        );
    }
    for error in [
        session
            .query_equipment_command(&"0".repeat(64), Duration::from_secs(1))
            .unwrap_err(),
        session
            .cancel_equipment_command(&"0".repeat(64), Duration::from_secs(1))
            .unwrap_err(),
    ] {
        assert!(matches!(
            error,
            PortableProbeError::Runtime(RuntimeProbeError::InvalidOutput {
                stage: "session.equipment_command",
                ..
            })
        ));
    }
    assert!(matches!(
        session.read_full_state().unwrap_err(),
        PortableProbeError::Runtime(RuntimeProbeError::InvalidOutput {
            stage: "session.read",
            ..
        })
    ));
}

#[cfg(target_os = "windows")]
fn live_equipment_error(message: impl Into<String>) -> Box<dyn Error + Send + Sync> {
    Box::new(io::Error::other(message.into()))
}

/// 候选必须让槽位装备与同运行 ID 的仓库聚合对象保持配置和强化等级一致。
#[cfg(target_os = "windows")]
fn select_live_equipment_candidate(
    state: &GameState,
) -> LiveEquipmentResult<(usize, LiveEquipmentCandidate)> {
    let resources = state.resources();
    if resources.equipment_capacity() >= resources.equipment_limit() {
        return Err(live_equipment_error("装备仓库没有卸下操作所需的空位"));
    }

    let mut candidates: Vec<LiveEquipmentCandidate> = Vec::new();
    for ship in state.ships().ships() {
        for slot in ship.slots() {
            let Some(equipment) = slot.equipment() else {
                continue;
            };
            if equipment.runtime_id() != equipment.config_id().get() {
                continue;
            }
            let warehouse = state
                .equipment_inventory()
                .warehouse()
                .iter()
                .copied()
                .find(|stack| stack.runtime_group_id() == equipment.runtime_id());
            if warehouse.is_some_and(|stack| {
                stack.config_id() != equipment.config_id()
                    || stack.enhance_level() != equipment.enhance_level()
            }) {
                continue;
            }
            candidates.push(LiveEquipmentCandidate {
                ship_instance_id: ship.identity().instance_id().get(),
                ship_name: ship.identity().name().to_owned(),
                slot_index: u32::from(slot.index().get()),
                equipment_id: equipment.runtime_id(),
                config_id: equipment.config_id().get(),
                enhance_level: u32::from(equipment.enhance_level().get()),
                warehouse_quantity: warehouse.map_or(0, |stack| stack.quantity()),
                equipment_capacity: resources.equipment_capacity(),
                equipment_limit: resources.equipment_limit(),
            });
        }
    }

    let candidate_count = candidates.len();
    let preferred_index = candidates
        .iter()
        .position(|candidate| candidate.warehouse_quantity > 0)
        .unwrap_or(0);
    if candidates.is_empty() {
        return Err(live_equipment_error(
            "未找到槽位与仓库局部身份均可严格核对的普通装备",
        ));
    }
    Ok((candidate_count, candidates.swap_remove(preferred_index)))
}

/// 核对回环步骤只改变目标槽、同型仓库数量和仓库总容量。
#[cfg(target_os = "windows")]
fn verify_live_equipment_state(
    state: &GameState,
    candidate: &LiveEquipmentCandidate,
    expected_equipped: bool,
    expected_warehouse_quantity: u64,
    expected_capacity: u64,
) -> LiveEquipmentResult<()> {
    let ship = state
        .ships()
        .ships()
        .iter()
        .find(|ship| ship.identity().instance_id().get() == candidate.ship_instance_id)
        .ok_or_else(|| live_equipment_error("回读状态缺少目标舰船实例"))?;
    let slot = ship
        .slots()
        .iter()
        .find(|slot| u32::from(slot.index().get()) == candidate.slot_index)
        .ok_or_else(|| live_equipment_error("回读状态缺少目标装备槽"))?;
    match (expected_equipped, slot.equipment()) {
        (true, Some(equipment))
            if equipment.runtime_id() == candidate.equipment_id
                && equipment.config_id().get() == candidate.config_id
                && u32::from(equipment.enhance_level().get()) == candidate.enhance_level => {}
        (false, None) => {}
        _ => {
            return Err(live_equipment_error("目标装备槽的回读状态与回环预期不一致"));
        }
    }

    let warehouse = state
        .equipment_inventory()
        .warehouse()
        .iter()
        .copied()
        .find(|stack| stack.runtime_group_id() == candidate.equipment_id);
    if expected_warehouse_quantity == 0 {
        if warehouse.is_some() {
            return Err(live_equipment_error(
                "预期同型仓库数量为零时聚合对象仍然存在",
            ));
        }
    } else {
        let stack =
            warehouse.ok_or_else(|| live_equipment_error("回读状态缺少同型仓库聚合对象"))?;
        if stack.config_id().get() != candidate.config_id
            || u32::from(stack.enhance_level().get()) != candidate.enhance_level
            || stack.quantity() != expected_warehouse_quantity
        {
            return Err(live_equipment_error(
                "同型仓库聚合对象的配置、强化等级或数量不符合预期",
            ));
        }
    }

    let resources = state.resources();
    if resources.equipment_capacity() != expected_capacity
        || resources.equipment_limit() != candidate.equipment_limit
    {
        return Err(live_equipment_error(
            "装备仓库容量或容量上限与回环预期不一致",
        ));
    }
    Ok(())
}

/// 把已认证目标、当前完整 D0 和局部候选组合成下一次写测试可直接复用的环境锁。
#[cfg(target_os = "windows")]
fn build_live_equipment_expectation(
    session: &PortableRuntimeSession,
    state: &GameState,
    candidate: &LiveEquipmentCandidate,
) -> LiveEquipmentResult<LiveEquipmentExpectation> {
    let prepared = session
        .prepared
        .as_ref()
        .ok_or_else(|| live_equipment_error("便携会话已经开始关闭，无法建立候选环境锁"))?;
    let runtime = session
        .runtime
        .as_ref()
        .ok_or_else(|| live_equipment_error("运行态会话已经开始关闭，无法建立候选环境锁"))?;
    let (process_id, process_start_time) = runtime.target_process_identity()?;
    if prepared.target.process_id != process_id {
        return Err(live_equipment_error(format!(
            "发现目标与认证会话的进程不一致: discovered={}, authenticated={process_id}",
            prepared.target.process_id
        )));
    }
    Ok(LiveEquipmentExpectation {
        schema_version: 1,
        instance_index: prepared.manager.instance_index.clone(),
        serial: prepared.target.serial.clone(),
        package_name: prepared.target.package_name.clone(),
        process_id,
        process_start_time,
        state_content_sha256: state.source().content_sha256().to_owned(),
        owned_state_content_sha256: state.source().owned_state_content_sha256().to_owned(),
        module_sha256: state.source().module_sha256().to_owned(),
        ship_instance_id: candidate.ship_instance_id,
        slot_index: candidate.slot_index,
        equipment_id: candidate.equipment_id,
        config_id: candidate.config_id,
        enhance_level: candidate.enhance_level,
        warehouse_quantity: candidate.warehouse_quantity,
        equipment_capacity: candidate.equipment_capacity,
        equipment_limit: candidate.equipment_limit,
    })
}

/// 将刚才只读确认的候选与当前 D0 严格比较，任一变化都在发送命令前终止。
#[cfg(target_os = "windows")]
fn verify_live_equipment_expectation(
    session: &PortableRuntimeSession,
    state: &GameState,
    candidate: &LiveEquipmentCandidate,
) -> LiveEquipmentResult<()> {
    let encoded = env::var("AZLW_LIVE_EXPECTED_CANDIDATE").map_err(|_| {
        live_equipment_error("必须通过 AZLW_LIVE_EXPECTED_CANDIDATE 锁定已确认候选")
    })?;
    let expected: LiveEquipmentExpectation = serde_json::from_str(&encoded)?;
    let actual = build_live_equipment_expectation(session, state, candidate)?;
    if actual != expected {
        return Err(live_equipment_error(format!(
            "当前候选与只读确认不一致，已在写入前停止: expected={}, actual={}",
            serde_json::to_string(&expected)?,
            serde_json::to_string(&actual)?,
        )));
    }
    Ok(())
}

/// 使用共享规范 JSON 摘要构造测试专用命令标识，不复制生产执行器的摘要实现。
#[cfg(target_os = "windows")]
fn build_live_equipment_command(
    target_fingerprint_sha256: &str,
    plan_hash: &str,
    sequence: u32,
    pre_state_content_sha256: &str,
    action: EquipmentCommandAction,
) -> LiveEquipmentResult<RuntimeEquipmentCommand> {
    let command_id = sha256_sorted_json(&json!({
        "domain": "azlw.live_equipment_round_trip.command",
        "schema_version": 1,
        "target_fingerprint_sha256": target_fingerprint_sha256,
        "plan_hash": plan_hash,
        "sequence": sequence,
        "pre_state_content_sha256": pre_state_content_sha256,
        "action": &action,
    }))?;
    Ok(RuntimeEquipmentCommand::new(
        command_id,
        target_fingerprint_sha256,
        plan_hash,
        sequence,
        pre_state_content_sha256,
        action,
    )?)
}

/// 只轮询 observing；failed 和 uncertain 都是终态，不能自动发送补偿写入。
#[cfg(target_os = "windows")]
fn wait_for_live_equipment_receipt(
    session: &mut PortableRuntimeSession,
    mut receipt: EquipmentCommandReceipt,
) -> LiveEquipmentResult<EquipmentCommandReceipt> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match receipt.phase {
            EquipmentCommandPhase::Succeeded => {
                if receipt.status != EquipmentCommandStatus::Success {
                    return Err(live_equipment_error("成功阶段没有携带 success 状态"));
                }
                return Ok(receipt);
            }
            EquipmentCommandPhase::Failed => {
                return Err(live_equipment_error(format!(
                    "装备命令明确失败: code={:?}, message={:?}",
                    receipt.error_code, receipt.message
                )));
            }
            EquipmentCommandPhase::Uncertain => {
                return Err(live_equipment_error(format!(
                    "装备命令结果不确定，停止发送后续写入: code={:?}, message={:?}",
                    receipt.error_code, receipt.message
                )));
            }
            EquipmentCommandPhase::Observing => {
                if Instant::now() >= deadline {
                    return Err(live_equipment_error(
                        "装备命令在 15 秒内没有离开 observing 阶段",
                    ));
                }
                thread::sleep(Duration::from_millis(100));
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(live_equipment_error(
                        "装备命令在 15 秒内没有离开 observing 阶段",
                    ));
                }
                receipt = session.query_equipment_command(&receipt.command_id, remaining)?;
            }
        }
    }
}

/// 在一条认证连接中执行卸下与装回，并对 D0、D1、D2 做完整状态回读。
#[cfg(target_os = "windows")]
fn run_live_equipment_round_trip(
    session: &mut PortableRuntimeSession,
) -> LiveEquipmentResult<Value> {
    let before = session.read_full_state()?;
    let (candidate_count, candidate) = select_live_equipment_candidate(&before)?;
    verify_live_equipment_expectation(session, &before, &candidate)?;
    verify_live_equipment_state(
        &before,
        &candidate,
        true,
        candidate.warehouse_quantity,
        candidate.equipment_capacity,
    )?;
    let target_fingerprint_sha256 = sha256_sorted_json(&json!({
        "domain": "azlw.live_equipment_round_trip.target",
        "module_sha256": before.source().module_sha256(),
        "owned_state_content_sha256": before.source().owned_state_content_sha256(),
        "ship_instance_id": candidate.ship_instance_id,
    }))?;
    let plan_hash = sha256_sorted_json(&json!({
        "domain": "azlw.live_equipment_round_trip.plan",
        "target_fingerprint_sha256": &target_fingerprint_sha256,
        "ship_instance_id": candidate.ship_instance_id,
        "slot_index": candidate.slot_index,
        "equipment_id": candidate.equipment_id,
        "config_id": candidate.config_id,
        "enhance_level": candidate.enhance_level,
    }))?;
    let command_equipment = EquipmentCommandEquipment::new(
        candidate.equipment_id,
        candidate.config_id,
        candidate.enhance_level,
    )?;
    let unequip_action = EquipmentCommandAction::unequip(
        candidate.ship_instance_id,
        candidate.slot_index,
        command_equipment,
        candidate.warehouse_quantity,
        candidate.equipment_capacity,
        candidate.equipment_limit,
    )?;
    let unequip_command = build_live_equipment_command(
        &target_fingerprint_sha256,
        &plan_hash,
        1,
        before.source().content_sha256(),
        unequip_action,
    )?;
    let initial_unequip_receipt = session.execute_equipment_command(&unequip_command)?;
    let unequip_receipt = wait_for_live_equipment_receipt(session, initial_unequip_receipt)?;

    let unequipped = session.read_full_state()?;
    let warehouse_after_unequip = candidate
        .warehouse_quantity
        .checked_add(1)
        .ok_or_else(|| live_equipment_error("卸下后的仓库数量溢出"))?;
    let capacity_after_unequip = candidate
        .equipment_capacity
        .checked_add(1)
        .ok_or_else(|| live_equipment_error("卸下后的仓库容量溢出"))?;
    verify_live_equipment_state(
        &unequipped,
        &candidate,
        false,
        warehouse_after_unequip,
        capacity_after_unequip,
    )?;

    let restore_action = EquipmentCommandAction::equip(
        candidate.ship_instance_id,
        candidate.slot_index,
        None,
        command_equipment,
        warehouse_after_unequip,
        0,
        capacity_after_unequip,
        candidate.equipment_limit,
    )?;
    let restore_command = build_live_equipment_command(
        &target_fingerprint_sha256,
        &plan_hash,
        2,
        unequipped.source().content_sha256(),
        restore_action,
    )?;
    let initial_restore_receipt = session.execute_equipment_command(&restore_command)?;
    let restore_receipt = wait_for_live_equipment_receipt(session, initial_restore_receipt)?;

    let restored = session.read_full_state()?;
    verify_live_equipment_state(
        &restored,
        &candidate,
        true,
        candidate.warehouse_quantity,
        candidate.equipment_capacity,
    )?;

    Ok(json!({
        "candidate_count": candidate_count,
        "candidate": {
            "ship_instance_id": candidate.ship_instance_id,
            "ship_name": candidate.ship_name,
            "slot_index": candidate.slot_index,
            "equipment_id": candidate.equipment_id,
            "config_id": candidate.config_id,
            "enhance_level": candidate.enhance_level,
            "warehouse_quantity_before": candidate.warehouse_quantity,
            "equipment_capacity_before": candidate.equipment_capacity,
            "equipment_limit": candidate.equipment_limit,
        },
        "state_hashes": {
            "before": before.source().content_sha256(),
            "unequipped": unequipped.source().content_sha256(),
            "restored": restored.source().content_sha256(),
        },
        "unequip_receipt": unequip_receipt,
        "restore_receipt": restore_receipt,
    }))
}

/// 严格核对真机会话使用优雅卸载，并清除了运行态与独立 ADB 的全部资源。
#[cfg(target_os = "windows")]
fn validate_live_equipment_shutdown(
    shutdown: &PortableSessionShutdownEvidence,
) -> LiveEquipmentResult<Value> {
    let (unload, journal_errors) = match &shutdown.runtime.method {
        RuntimeShutdownMethodEvidence::Graceful {
            unload,
            journal_errors,
        } => (unload, journal_errors),
        RuntimeShutdownMethodEvidence::Retained { .. } => panic!("严格审计不得保留代理"),
        RuntimeShutdownMethodEvidence::RestartFallback {
            unload_error,
            journal_errors,
        } => {
            let journal_suffix: String = if journal_errors.is_empty() {
                String::new()
            } else {
                format!("; 日志错误: {}", journal_errors.join(" | "))
            };
            return Err(live_equipment_error(format!(
                "运行态没有完成优雅卸载: {unload_error}{journal_suffix}"
            )));
        }
    };
    if !journal_errors.is_empty() {
        return Err(live_equipment_error(format!(
            "运行态日志存在 {} 个写入错误: {}",
            journal_errors.len(),
            journal_errors.join(" | ")
        )));
    }
    let process = &shutdown.runtime.process;
    if process.process_id != unload.process_id
        || process.process_start_time != unload.process_start_time
        || process.tracer_pid != 0
    {
        return Err(live_equipment_error(
            "优雅卸载前后的游戏进程身份或 TracerPid 不一致",
        ));
    }
    let cleanup = &shutdown.runtime.cleanup;
    if !cleanup.forward_removed
        || !cleanup.forward_list_restored
        || !cleanup.device_session_removed
        || !cleanup.host_session_removed
        || cleanup.old_process_stopped
        || cleanup.game_restarted
    {
        return Err(live_equipment_error(
            "运行态资源未完整清理，或清理过程重启了游戏",
        ));
    }
    if !shutdown.adb.adb_process_stopped
        || !shutdown.adb.adb_port_released
        || !shutdown.adb.adb_temporary_root_removed
    {
        return Err(live_equipment_error("独立 ADB 资源未完整清理"));
    }

    Ok(json!({
        "process_id": unload.process_id,
        "process_start_time": unload.process_start_time,
        "game_restarted": false,
        "adb_process_stopped": true,
        "adb_port_released": true,
        "adb_temporary_root_removed": true,
        "journal_path": &shutdown.runtime.journal_path,
    }))
}

/// 无论运行操作成功与否都先关闭会话，并在双重失败时保留两个原因。
#[cfg(target_os = "windows")]
fn finish_live_equipment_operation(
    session: &mut PortableRuntimeSession,
    operation: LiveEquipmentResult<Value>,
) -> LiveEquipmentResult<Value> {
    let shutdown = session.shutdown().cloned();
    match (operation, shutdown) {
        (Ok(result), Ok(shutdown)) => {
            let cleanup = validate_live_equipment_shutdown(&shutdown)?;
            Ok(json!({"result": result, "cleanup": cleanup}))
        }
        (Err(operation_error), Ok(shutdown)) => match validate_live_equipment_shutdown(&shutdown) {
            Ok(_) => Err(operation_error),
            Err(cleanup_error) => Err(live_equipment_error(format!(
                "运行操作失败: {operation_error}; 会话清理证据也无效: {cleanup_error}"
            ))),
        },
        (Ok(_), Err(cleanup_error)) => Err(live_equipment_error(format!(
            "运行操作成功，但会话清理失败: {cleanup_error}"
        ))),
        (Err(operation_error), Err(cleanup_error)) => Err(live_equipment_error(format!(
            "运行操作失败: {operation_error}; 会话清理也失败: {cleanup_error}"
        ))),
    }
}

/// 只读发现一个可原样装回的普通装备槽，并在输出候选证据前完成优雅卸载。
#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 MuMu12、已登录游戏和包含最新 Agent 的 Windows 发布目录"]
fn live_equipment_command_candidate_is_read_only() -> LiveEquipmentResult<()> {
    let root_path: PathBuf = env::var_os("AZLW_LIVE_TOOL_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| live_equipment_error("必须通过 AZLW_LIVE_TOOL_ROOT 指定完整发布目录"))?;
    let settings = Settings::load(&root_path)?;
    let options = PortableProbeOptions::from_settings(root_path, &settings)?;
    let mut session = PortableRuntimeSession::open(options, None)?;
    let operation: LiveEquipmentResult<Value> = (|| {
        let state = session.read_full_state()?;
        let (candidate_count, candidate) = select_live_equipment_candidate(&state)?;
        verify_live_equipment_state(
            &state,
            &candidate,
            true,
            candidate.warehouse_quantity,
            candidate.equipment_capacity,
        )?;
        let write_expectation = build_live_equipment_expectation(&session, &state, &candidate)?;
        Ok(json!({
            "candidate_count": candidate_count,
            "state_content_sha256": state.source().content_sha256(),
            "owned_state_content_sha256": state.source().owned_state_content_sha256(),
            "module_sha256": state.source().module_sha256(),
            "ship_instance_id": candidate.ship_instance_id,
            "ship_name": candidate.ship_name,
            "slot_index": candidate.slot_index,
            "equipment_id": candidate.equipment_id,
            "config_id": candidate.config_id,
            "enhance_level": candidate.enhance_level,
            "target_warehouse_quantity_before": candidate.warehouse_quantity,
            "equipment_capacity_before": candidate.equipment_capacity,
            "equipment_limit_before": candidate.equipment_limit,
            "write_expectation": write_expectation,
        }))
    })();
    let evidence = finish_live_equipment_operation(&mut session, operation)?;
    let write_expectation: &Value = evidence
        .pointer("/result/write_expectation")
        .ok_or_else(|| live_equipment_error("只读候选证据缺少可复用的写入环境锁"))?;

    println!(
        "AZLW_LIVE_EXPECTED_CANDIDATE {}",
        serde_json::to_string(write_expectation)?
    );

    println!(
        "AZLW_EQUIPMENT_CANDIDATE {}",
        serde_json::to_string(&evidence)?
    );
    Ok(())
}

/// 在同一认证会话中卸下并装回装备，最终必须保留原游戏进程且恢复局部前态。
#[cfg(target_os = "windows")]
#[test]
#[ignore = "会对已登录游戏执行一次可逆装备写入，必须在运行前单独确认"]
fn live_equipment_command_round_trip_restores_state() -> LiveEquipmentResult<()> {
    let root_path: PathBuf = env::var_os("AZLW_LIVE_TOOL_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| live_equipment_error("必须通过 AZLW_LIVE_TOOL_ROOT 指定完整发布目录"))?;
    let settings = Settings::load(&root_path)?;
    let options = PortableProbeOptions::from_settings(root_path, &settings)?;
    let mut session = PortableRuntimeSession::open(options, None)?;
    let operation = run_live_equipment_round_trip(&mut session);
    let evidence = finish_live_equipment_operation(&mut session, operation)?;

    println!(
        "AZLW_EQUIPMENT_ROUND_TRIP {}",
        serde_json::to_string(&evidence)?
    );
    Ok(())
}

/// 自动模式接受目标提示，手工模式严格要求实例和地址并兼容 profile 默认包名。
#[test]
fn option_validation_keeps_auto_and_manual_contracts_separate() {
    let auto: PortableProbeOptions = PortableProbeOptions::new(Path::new("."), PortableMode::Auto)
        .with_instance_hint("stale")
        .with_serial_hint("not-a-loopback-address")
        .with_game_package_hint("not a package");
    assert!(auto.validate().is_ok());
    let manual: PortableProbeOptions =
        PortableProbeOptions::new(Path::new("."), PortableMode::Manual)
            .with_instance_hint("mumu12:0")
            .with_serial_hint("127.0.0.1:16384");
    assert!(manual.validate().is_ok());

    let incomplete: PortableProbeOptions =
        PortableProbeOptions::new(Path::new("."), PortableMode::Manual)
            .with_instance_hint("mumu12:0");
    assert!(incomplete.validate().is_err());

    let invalid_manual: PortableProbeOptions =
        PortableProbeOptions::new(Path::new("."), PortableMode::Manual)
            .with_instance_hint("stale")
            .with_serial_hint("not-a-loopback-address");
    assert!(invalid_manual.validate().is_err());
}

/// 已存在但内容为空的自定义 ADB 在自动模式回退固定闭包，手工模式明确失败。
#[test]
fn auto_mode_falls_back_when_custom_adb_fails_content_inspection() {
    let root: PathBuf = test_root("custom-adb-inspection");
    write_adb_bundle(&root, "runtime/adb", b"default-adb");
    write_adb_bundle(&root, "runtime/custom-adb", b"");
    let relative: &str = "runtime/custom-adb/adb.exe";

    let auto: PortableProbeOptions =
        PortableProbeOptions::new(&root, PortableMode::Auto).with_adb_relative_path_hint(relative);
    let (_, inspection) = load_and_inspect_configured_adb_bundle(&auto, None).unwrap();
    assert_eq!(inspection.files[0].relative_path, "runtime/adb/adb.exe");

    let manual: PortableProbeOptions = PortableProbeOptions::new(&root, PortableMode::Manual)
        .with_adb_relative_path_hint(relative);
    let error: PortableProbeError =
        load_and_inspect_configured_adb_bundle(&manual, None).unwrap_err();
    assert_eq!(
        error
            .to_string()
            .matches("隔离 ADB 阶段 adb.bundle 失败")
            .count(),
        1
    );
    fs::remove_dir_all(root).unwrap();
}

fn write_adb_bundle(root: &Path, relative: &str, executable: &[u8]) {
    let directory: PathBuf = root.join(relative);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("adb.exe"), executable).unwrap();
    fs::write(directory.join("AdbWinApi.dll"), b"api").unwrap();
    fs::write(directory.join("NOTICE.txt"), b"notice").unwrap();
    fs::write(
        directory.join("source.properties"),
        b"Pkg.Revision=37.0.1\n",
    )
    .unwrap();
}

fn test_root(label: &str) -> PathBuf {
    let home: PathBuf = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("测试需要 HOME 或 USERPROFILE");
    let mut bytes: [u8; 16] = [0; 16];
    getrandom::fill(&mut bytes).unwrap();
    home.join("suzushiro/scratch/azlw-portable-tests")
        .join(format!(
            "{label}-{}-{:032x}",
            std::process::id(),
            u128::from_le_bytes(bytes)
        ))
}

/// 包名提示遵循自动回退和手工精确匹配，不会静默改变运行态 profile。
#[test]
fn package_hint_keeps_auto_fallback_and_manual_identity() {
    assert_eq!(
        select_profile_package(
            PortableMode::Auto,
            Some("com.example.stale"),
            TARGET_PACKAGE,
        )
        .unwrap(),
        TARGET_PACKAGE
    );
    assert_eq!(
        select_profile_package(PortableMode::Manual, None, TARGET_PACKAGE).unwrap(),
        TARGET_PACKAGE
    );
    assert!(
        select_profile_package(
            PortableMode::Manual,
            Some("com.example.wrong"),
            TARGET_PACKAGE,
        )
        .is_err()
    );
}

/// 设置文件使用的秒级连接和启动期限保持相同的非零上限。
#[test]
fn settings_timeouts_keep_the_bounded_seconds_contract() {
    let options: PortableProbeOptions =
        PortableProbeOptions::new(Path::new("."), PortableMode::Auto)
            .with_connect_timeout_seconds(30)
            .unwrap()
            .with_startup_timeout_seconds(180)
            .unwrap();
    assert!(options.validate().is_ok());
    assert!(
        PortableProbeOptions::new(Path::new("."), PortableMode::Auto)
            .with_connect_timeout_seconds(0)
            .is_err()
    );
    assert!(
        PortableProbeOptions::new(Path::new("."), PortableMode::Auto)
            .with_startup_timeout_seconds(3_601)
            .is_err()
    );
}

/// 完整状态采集默认关闭，显式目录必须存在且位于发布根之外。
#[test]
fn full_state_capture_root_is_explicit_and_external() {
    let base = test_root("full-state-capture-root");
    let tool_root = base.join("release");
    let capture_root = base.join("capture");
    let inside_root = tool_root.join("private");
    fs::create_dir_all(&capture_root).unwrap();
    fs::create_dir_all(&inside_root).unwrap();

    let default = PortableProbeOptions::new(&tool_root, PortableMode::Auto);
    let external = PortableProbeOptions::new(&tool_root, PortableMode::Auto)
        .with_full_state_capture_root(&capture_root);
    let inside = PortableProbeOptions::new(&tool_root, PortableMode::Auto)
        .with_full_state_capture_root(&inside_root);

    assert!(default.full_state_capture_root.is_none());
    assert!(default.validate().is_ok());
    assert!(external.validate().is_ok());
    let error = inside.validate().unwrap_err();
    assert!(matches!(
        error,
        PortableProbeError::InvalidOption {
            field: "full_state_capture_root",
            ..
        }
    ));
    fs::remove_dir_all(base).unwrap();
}

/// 实例和序列号只接受受限十进制索引与非零回环地址。
#[test]
fn target_validators_reject_ambiguous_or_remote_values() {
    assert!(validate_instance_index("0").is_ok());
    assert!(validate_instance_index("-1").is_err());
    assert!(validate_serial("127.0.0.1:16384").is_ok());
    assert!(validate_serial("192.0.2.1:16384").is_err());
}

/// 游戏启动只绑定已验证 serial 和固定包名。
#[test]
fn launch_commands_are_capability_bounded_and_target_bound() {
    assert_eq!(
        adb_game_launch_arguments(TARGET_PACKAGE),
        [
            "shell",
            "monkey",
            "-p",
            "com.bilibili.azurlane",
            "-c",
            "android.intent.category.LAUNCHER",
            "1",
        ]
    );
}

/// BagProxy 等待失败即使被探针包装，也要给出登录提示并保留日志详情。
#[test]
fn bag_readiness_failure_maps_to_game_not_ready() {
    let source: RuntimeProbeError = RuntimeProbeError::InvalidOutput {
        stage: "rpc.wait_bag_proxy",
        message: "BagProxy 未就绪".to_owned(),
    };
    let wrapped: RuntimeProbeError = RuntimeProbeError::ProbeFailed {
        source: Box::new(source),
        journal_path: PathBuf::from("data/logs/session.jsonl"),
        cleanup_error: None,
    };
    let mapped: PortableProbeError = map_runtime_error(wrapped);
    let message: String = mapped.to_string();
    assert!(matches!(mapped, PortableProbeError::GameNotReady { .. }));
    assert!(message.contains("完成登录并进入港区"));
    assert!(message.contains("data/logs/session.jsonl"));
    assert!(!message.contains("BagProxy 未就绪"));
}

/// 完整状态读取端口的稳定未就绪错误经过探针包装后仍保留可恢复分类。
#[test]
fn full_state_readiness_failure_maps_to_game_not_ready() {
    let source: AppError = AppError::from_source(
        "runtime.capabilities_after",
        AppErrorCode::GameNotReady,
        "游戏运行态尚未完成完整读取准备",
        std::io::Error::other("PRIVATE_CAPABILITY_REASON"),
    );
    let wrapped: RuntimeProbeError = RuntimeProbeError::ProbeFailed {
        source: Box::new(RuntimeProbeError::GameStateRead(source)),
        journal_path: PathBuf::from("data/logs/full-state.jsonl"),
        cleanup_error: None,
    };

    let mapped: PortableProbeError = map_runtime_error(wrapped);
    let message: String = mapped.to_string();
    assert!(matches!(mapped, PortableProbeError::GameNotReady { .. }));
    assert!(message.contains("完成登录并进入港区"));
    assert!(message.contains("data/logs/full-state.jsonl"));
    assert!(!message.contains("PRIVATE_CAPABILITY_REASON"));
}

/// 其他运行态错误保留原始 source，但公开文本不能泄漏底层诊断正文。
#[test]
fn unrelated_runtime_failure_keeps_source_and_redacts_public_message() {
    let error: RuntimeProbeError = RuntimeProbeError::InvalidOutput {
        stage: "target.discover_pid",
        message: "LUA_PRIVATE_DIAGNOSTIC".to_owned(),
    };
    let mapped: PortableProbeError = map_runtime_error(error);
    assert!(matches!(
        &mapped,
        PortableProbeError::Runtime(RuntimeProbeError::InvalidOutput {
            stage: "target.discover_pid",
            ..
        })
    ));
    let message: String = mapped.to_string();
    assert_eq!(message, "运行态验证失败，请查看工具目录内的运行态日志");
    assert!(!message.contains("LUA_PRIVATE_DIAGNOSTIC"));
}

/// 便携收据版本 3 必须直接携带装备样本的完整定位与摘要证据。
#[test]
fn portable_receipt_exposes_equipment_sample_contract() {
    let report = PortableProbeReport {
        schema_version: super::PORTABLE_PROBE_SCHEMA_VERSION,
        status: "passed",
        mode: PortableMode::Manual,
        manager: ManagerEvidence {
            executable: PathBuf::from("MuMuManager.exe"),
            instance_index: "0".to_owned(),
            instance_name: "实例-0".to_owned(),
            android_version: "12.0".to_owned(),
            instance_started_by_probe: false,
        },
        target: TargetEvidence {
            serial: "127.0.0.1:16384".to_owned(),
            abi: "x86_64".to_owned(),
            abi_list: "x86_64".to_owned(),
            package_name: "com.bilibili.azurlane".to_owned(),
            package_paths: vec!["/data/app/base.apk".to_owned()],
            process_id: 1234,
            root_identity: "uid=0(root)".to_owned(),
            game_started_by_probe: false,
        },
        adb: PortableAdbEvidence {
            bundle: AdbBundleInspection {
                revision: "37.0.1".to_owned(),
                state_root: PathBuf::from("data/runtime/adb"),
                files: Vec::new(),
            },
            server_port: 5038,
            server_process_id: 5678,
            server_log_path: PathBuf::from("data/logs/adb.log"),
        },
        runtime_session_id: "0123456789abcdef0123456789abcdef".to_owned(),
        runtime_snapshot_count: 3,
        runtime_receipt_path: PathBuf::from("data/history/002-probe.json"),
        runtime_journal_path: PathBuf::from("data/logs/001-runtime.jsonl"),
        equipment_sample: equipment_sample_evidence(),
        cleanup: PortableCleanupEvidence {
            adb_process_stopped: true,
            adb_port_released: true,
            adb_temporary_root_removed: true,
        },
    };

    let value: Value = serde_json::to_value(report).unwrap();
    assert_eq!(value["schema_version"], 3);
    assert!(value["manager"].get("version").is_none());
    assert!(value["adb"].get("version_output").is_none());
    assert_eq!(
        value["equipment_sample"]["relative_path"],
        "data/history/001-equipment.json"
    );
    assert_eq!(value["equipment_sample"]["config_count"], 12);
    assert_eq!(value["equipment_sample"]["raw_schema_version"], 2);
    assert_eq!(
        value["equipment_sample"]["catalog_content_sha256"],
        "catalog-sha256"
    );
}

fn equipment_sample_evidence() -> EquipmentSampleEvidence {
    EquipmentSampleEvidence {
        schema_version: 1,
        relative_path: "data/history/001-equipment.json".to_owned(),
        size_bytes: 4096,
        file_sha256: "file-sha256".to_owned(),
        module_sha256: "module-sha256".to_owned(),
        catalog_schema_version: 2,
        catalog_content_sha256: "catalog-sha256".to_owned(),
        raw_schema_version: 2,
        raw_content_sha256: "raw-sha256".to_owned(),
        family_count: 4,
        config_count: 12,
        recipe_count: 3,
        reference_count: 7,
        weapon_count: 5,
        skill_count: 6,
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::collections::BTreeMap;
    use std::fs;

    use super::super::{
        EmulatorInstance, PortableMode, PortableProbeOptions, ResolvedManager, resolve_manager,
        running_manager_candidates, select_instance, validate_manager_candidate,
    };
    use crate::adapters::device::test_support::sample_instances;
    use suzushiro_emulator::TargetState;

    /// 本机安装验证只读执行 info 与进程映像查询，不启动实例、ADB 或游戏。
    #[test]
    #[ignore = "需要 Windows 主机已安装并运行 MuMu12"]
    fn installed_mumu_manager_passes_read_only_discovery() {
        let options =
            PortableProbeOptions::new(".", PortableMode::Auto).with_instance_hint("mumu12:0");
        let ResolvedManager {
            executable: manager,
            instances,
        } = resolve_manager(&options).unwrap();
        assert_eq!(
            manager.file_name().and_then(|name| name.to_str()),
            Some("MuMuManager.exe")
        );
        assert!(!instances.is_empty());
        let (process_candidates, _diagnostics) = running_manager_candidates().unwrap();
        assert!(process_candidates.iter().any(|candidate| {
            fs::canonicalize(candidate).is_ok_and(|process_manager| process_manager == manager)
        }));
        let catalog =
            crate::adapters::device::emulator_instance_catalog::read_emulator_instance_catalog(
                None, None,
            )
            .unwrap();
        assert!(!catalog.candidates().is_empty());
    }

    /// Manager 命令不可执行时从同一安装根的 `.nemu` 恢复实例，不写入模拟器目录。
    #[test]
    fn invalid_manager_command_falls_back_to_nemu_catalog() {
        let root = super::test_root("nemu-fallback");
        let manager = root.join("nx_main/MuMuManager.exe");
        let instance_root = root.join("vms/MuMuPlayer-12.0-0");
        fs::create_dir_all(manager.parent().unwrap()).unwrap();
        fs::create_dir_all(&instance_root).unwrap();
        let command_shell = std::env::var_os("ComSpec").expect("Windows 测试需要 ComSpec");
        fs::copy(command_shell, &manager).unwrap();
        fs::write(
                instance_root.join("MuMuPlayer-12.0-0.nemu"),
                br#"<Nemu><Forwarding name="ADB_PORT" hostip="127.0.0.1" hostport="16384" guestport="5555"/></Nemu>"#,
            )
            .unwrap();

        let resolved = validate_manager_candidate(&manager).unwrap();

        assert_eq!(resolved.instances.len(), 1);
        assert_eq!(resolved.instances.get("0").unwrap().adb_port, Some(16_384));
        fs::remove_dir_all(root).unwrap();
    }

    /// GUI 只以运行能力和回环 ADB 目标为门槛，Android 版本仅供展示。
    #[test]
    fn catalog_state_uses_runtime_capability_and_rejects_invalid_targets() {
        let instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        let ready = instances.get("0").unwrap();
        assert_eq!(ready.catalog_state(), TargetState::Ready);

        let mut remote = ready.clone();
        remote.adb_host_ip = Some("192.0.2.1".to_owned());
        assert_eq!(remote.catalog_state(), TargetState::Unavailable);

        let mut zero_port = ready.clone();
        zero_port.adb_port = Some(0);
        assert_eq!(zero_port.catalog_state(), TargetState::Unavailable);

        let mut other_android = ready.clone();
        other_android.android_version = Some("11.0".to_owned());
        assert_eq!(other_android.catalog_state(), TargetState::Ready);

        let mut manager_error = ready.clone();
        manager_error.available = false;
        assert_eq!(manager_error.catalog_state(), TargetState::Unavailable);

        let starting = instances.get("1").unwrap();
        let mut starting = starting.clone();
        starting.is_process_started = true;
        assert_eq!(starting.catalog_state(), TargetState::Starting);
    }

    /// 唯一运行实例会被自动选择，已停止实例不会制造歧义。
    #[test]
    fn stopped_instances_do_not_make_auto_selection_ambiguous() {
        let instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        let options: PortableProbeOptions = PortableProbeOptions::new(".", PortableMode::Auto);
        let selected: EmulatorInstance = select_instance(&instances, &options).unwrap();
        assert_eq!(selected.index, "0");
    }

    /// 没有运行实例时要求用户先启动，不会静默启动任意已登记实例。
    #[test]
    fn no_running_instance_requires_user_start() {
        let mut instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        for instance in instances.values_mut() {
            instance.is_process_started = false;
            instance.is_android_started = false;
        }
        let options: PortableProbeOptions = PortableProbeOptions::new(".", PortableMode::Auto);
        let message: String = select_instance(&instances, &options)
            .unwrap_err()
            .to_string();
        assert!(message.contains("没有运行中的模拟器实例"));
    }

    /// 两个运行实例仍保持显式选择门禁，错误只列出运行候选。
    #[test]
    fn multiple_running_instances_require_explicit_selection() {
        let mut instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        let second: &mut EmulatorInstance = instances.get_mut("1").unwrap();
        second.adb_host_ip = Some("127.0.0.1".to_owned());
        second.adb_port = Some(16_416);
        second.is_process_started = true;
        second.is_android_started = true;

        let options: PortableProbeOptions = PortableProbeOptions::new(".", PortableMode::Auto);
        let message: String = select_instance(&instances, &options)
            .unwrap_err()
            .to_string();
        assert!(message.contains("多个正在运行"));
        assert!(message.contains("0:实例-0"));
        assert!(message.contains("1:实例-1"));
    }

    #[test]
    fn selected_instance_is_exact_and_must_be_ready() {
        let mut instances = sample_instances();
        for mode in [PortableMode::Auto, PortableMode::Manual] {
            for index in ["mumu12:1", "mumu12:99"] {
                let options = PortableProbeOptions::new(".", mode)
                    .with_instance_hint("mumu12:0")
                    .with_serial_hint("127.0.0.1:16384")
                    .with_selected_instance(index);
                assert!(options.validate().is_ok());
                assert_eq!(options.mode, mode);
                assert!(options.serial_hint.is_none());
                assert!(select_instance(&instances, &options).is_err());
            }
            let options = PortableProbeOptions::new(".", mode).with_selected_instance("mumu12:0");
            assert_eq!(
                crate::adapters::device::portable::select_instance(&instances, &options)
                    .unwrap()
                    .index,
                "0"
            );
        }
        let options =
            PortableProbeOptions::new(".", PortableMode::Auto).with_selected_instance("mumu12:0");
        instances.get_mut("0").unwrap().is_android_started = false;
        assert!(select_instance(&instances, &options).is_err());
        assert!(
            PortableProbeOptions::new(".", PortableMode::Auto)
                .with_selected_instance("invalid")
                .validate()
                .is_err()
        );
    }

    /// 自动模式的实例提示失效后回到唯一运行候选，不会启动提示指向的停止实例。
    #[test]
    fn auto_hint_falls_back_from_stopped_instance() {
        let instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        let options: PortableProbeOptions =
            PortableProbeOptions::new(".", PortableMode::Auto).with_instance_hint("mumu12:1");
        let selected: EmulatorInstance = select_instance(&instances, &options).unwrap();
        assert_eq!(selected.index, "0");
    }

    /// 自动模式优先采用与序列号提示匹配的已验证运行实例。
    #[test]
    fn auto_serial_hint_prioritizes_matching_running_instance() {
        let mut instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        let second: &mut EmulatorInstance = instances.get_mut("1").unwrap();
        second.adb_host_ip = Some("127.0.0.1".to_owned());
        second.adb_port = Some(16_416);
        second.is_process_started = true;
        second.is_android_started = true;

        let options: PortableProbeOptions =
            PortableProbeOptions::new(".", PortableMode::Auto).with_serial_hint("127.0.0.1:16416");

        let selected: EmulatorInstance = select_instance(&instances, &options).unwrap();

        assert_eq!(selected.index, "1");
    }

    /// 显式实例提示优先于串号提示，确保连接目标由实例配置决定。
    #[test]
    fn auto_instance_hint_wins_over_a_conflicting_serial_hint() {
        let mut instances: BTreeMap<String, EmulatorInstance> = sample_instances();
        let second: &mut EmulatorInstance = instances.get_mut("1").unwrap();
        second.adb_host_ip = Some("127.0.0.1".to_owned());
        second.adb_port = Some(16_416);
        second.is_process_started = true;
        second.is_android_started = true;

        let options: PortableProbeOptions = PortableProbeOptions::new(".", PortableMode::Auto)
            .with_instance_hint("mumu12:1")
            .with_serial_hint("127.0.0.1:16384");

        let selected: EmulatorInstance = select_instance(&instances, &options).unwrap();

        assert_eq!(selected.index, "1");
    }
}

#[test]
fn incomplete_player_and_ship_readiness_requires_entering_the_game() {
    for stage in [
        "rpc.wait_bag_proxy",
        "rpc.wait_owned_state",
        "rpc.wait_ship_details",
    ] {
        let mapped = map_runtime_error(RuntimeProbeError::InvalidOutput {
            stage,
            message: "代理数据尚未就绪".into(),
        });
        assert!(
            matches!(mapped, PortableProbeError::GameNotReady { .. }),
            "{stage}"
        );
    }
}

#[cfg(target_os = "windows")]
#[test]
fn explicit_provider_selection_passes_interactive_gate() {
    let root = test_root("provider-selection");
    fs::create_dir_all(&root).unwrap();
    let options =
        PortableProbeOptions::new(&root, PortableMode::Auto).with_selected_instance("ldplayer:0");
    options.validate().unwrap();
    let instances = crate::adapters::device::test_support::sample_instances();
    assert_eq!(
        crate::adapters::device::portable::select_instance(&instances, &options)
            .unwrap()
            .index,
        "0"
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn package_validation_requires_a_value_and_preserves_error_field() {
    super::validate_package_name("Com.Example_1").unwrap();
    for value in ["", "single", "a..b", "a.b;id"] {
        let error = super::validate_package_name(value).unwrap_err();
        assert!(matches!(error, super::PortableProbeError::InvalidOption {
            field: "game_package", ref message
        } if message == "必须是受限的 Android 包名"));
    }
}

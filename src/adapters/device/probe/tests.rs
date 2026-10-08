//! 覆盖探针编排、证据汇总和失败清理的单元测试。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::adapters::device::runtime::{
    AgentError, HealthResult, RetryDirective, RuntimeShip, RuntimeShipSkill, SessionEffect,
};
use crate::adapters::device::{
    mapping::equipment::EquipmentMappingError, reading::equipment::EquipmentReadError,
};
use crate::adapters::tool_root::ToolRoot;
use crate::application::{AppError, AppErrorCode};

#[cfg(target_os = "windows")]
use super::DeviceBridge;
use super::{
    LoaderReceipt, OriginalProcessPresence, PostUnloadMappingEvidence, ProcessRecoveryAction,
    RUNTIME_PROBE_SCHEMA_VERSION, RuntimeProbeError, RuntimeProbeOptions, TargetRecoveryState,
    cleanup_device_unreachable, finish_probe_execution_after_unload, guarded_loader_command,
    guarded_unloader_command, is_retryable_bag_readiness_error,
    is_retryable_owned_state_readiness_error, is_retryable_ship_details_readiness_error,
    journal_error_summary, loader_failure_decision, loader_failure_journal_details,
    loader_failure_requires_restart, module_map_probe_command, parse_boot_id, parse_loader_receipt,
    parse_proc_stat_start_time, parse_single_pid, parse_unload_receipt, probe_failed_after_cleanup,
    probe_failed_after_ordered_cleanup, probe_finalization_failed, process_recovery_action,
    reconcile_owned_forward, reconcile_preserved_process_evidence, record_cleanup_completion,
    restart_after_process_stop, summarize_read_errors, summarize_ship_growth,
    thread_name_probe_command, validate_post_unload_mappings, validate_preserved_process_identity,
    validate_unloader_result, wait_for_main_thread_with,
};
#[cfg(target_os = "windows")]
use super::{
    RuntimeShutdownMethodEvidence, attempt_runtime_session_cleanup,
    merge_runtime_session_journal_errors,
};
#[cfg(target_os = "windows")]
use suzushiro_adb::OwnedAdbServer;

#[test]
/// 验证配置拒绝多实例索引和非回环设备地址。
fn options_reject_non_loopback_and_ambiguous_instance() {
    let root: PathBuf = std::env::current_dir().unwrap();
    assert!(matches!(
        RuntimeProbeOptions::new(&root, "manager", "adb", "0,1", "127.0.0.1:16384"),
        Err(RuntimeProbeError::InvalidOption {
            field: "vm_index",
            ..
        })
    ));
    assert!(matches!(
        RuntimeProbeOptions::new(&root, "manager", "adb", "0", "192.0.2.1:16384"),
        Err(RuntimeProbeError::InvalidOption {
            field: "serial",
            ..
        })
    ));
    #[cfg(target_os = "windows")]
    assert!(
        RuntimeProbeOptions::new(&root, "manager", "adb", "0", "127.0.0.1:16384")
            .unwrap()
            .with_adb_server_port(0)
            .is_err()
    );
}

#[test]
fn boot_id_parser_normalizes_uuid_and_rejects_unstructured_output() {
    assert_eq!(
        parse_boot_id("01234567-89AB-CDEF-0123-456789ABCDEF\n", "fixture.boot_id").unwrap(),
        "01234567-89ab-cdef-0123-456789abcdef"
    );
    assert!(parse_boot_id("ok", "fixture.boot_id").is_err());
    assert!(parse_boot_id("01234567-89ab-cdef-0123-456789abcdeg", "fixture.boot_id").is_err());
}

/// 保留进程的成功证据必须同时匹配 PID 和 proc 启动时刻。
#[test]
fn preserved_process_identity_rejects_missing_or_reused_processes() {
    let mut evidence = super::empty_process_evidence(42);
    evidence.process_start_time = 99;
    validate_preserved_process_identity(&evidence, 42, Some(99), "fixture.identity").unwrap();

    evidence.process_id = 43;
    assert!(
        validate_preserved_process_identity(&evidence, 42, Some(99), "fixture.identity").is_err()
    );
    evidence.process_id = 42;
    evidence.process_start_time = 100;
    assert!(
        validate_preserved_process_identity(&evidence, 42, Some(99), "fixture.identity").is_err()
    );
    assert!(validate_preserved_process_identity(&evidence, 42, None, "fixture.identity").is_err());
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 AZLW_MUMU_MANAGER、AZLW_MUMU_INSTANCE 和 AZLW_MUMU_SERIAL"]
fn installed_mumu_cross_channel_boot_identity_matches() {
    let tool_root = std::env::var_os("AZLW_RUNTIME_TOOL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let manager = std::env::var("AZLW_MUMU_MANAGER").expect("需要 AZLW_MUMU_MANAGER");
    let instance = std::env::var("AZLW_MUMU_INSTANCE").expect("需要 AZLW_MUMU_INSTANCE");
    let serial = std::env::var("AZLW_MUMU_SERIAL").expect("需要 AZLW_MUMU_SERIAL");
    let bundle = crate::adapters::device::adb_config::load_adb_bundle(&tool_root, None).unwrap();
    let mut server = OwnedAdbServer::start(bundle.clone(), serial.clone()).unwrap();
    server.connect_target().unwrap();
    let options = RuntimeProbeOptions::new(
        &tool_root,
        manager,
        bundle.executable().to_str().unwrap(),
        instance,
        serial,
    )
    .unwrap()
    .with_adb_server_port(server.port())
    .unwrap()
    .with_adb_process_policy(bundle.process_policy().clone());
    let bridge = DeviceBridge::new(&options);

    bridge.verify_cross_channel_boot_identity().unwrap();

    let cleanup = server.shutdown().unwrap();
    assert!(cleanup.process_stopped);
    assert!(cleanup.port_released);
    assert!(cleanup.temporary_root_removed);
}

/// 最终收据新增完整状态摘要时必须使用独立结构版本。
#[test]
fn full_state_evidence_uses_runtime_receipt_schema_five() {
    assert_eq!(RUNTIME_PROBE_SCHEMA_VERSION, 5);
}

/// 主线程队列短暂未就绪时沿用同一连接重试，不把可恢复状态误判为会话失效。
#[test]
fn main_thread_readiness_retries_same_session() {
    let mut attempts: u32 = 0;
    let health: HealthResult = wait_for_main_thread_with(
        || {
            attempts += 1;
            serde_json::from_value(serde_json::json!({
                "agent_version": "1.0.0",
                "process_id": 12_345,
                "package_name": "com.bilibili.azurlane",
                "abi": "x86_64",
                "session_state": "ready",
                "main_thread_queue_ready": attempts == 2,
                "catalog_generation": 1,
            }))
            .map_err(|source| RuntimeProbeError::Json {
                stage: "test.health",
                source,
            })
        },
        Duration::from_secs(1),
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(attempts, 2);
    assert!(health.main_thread_queue_ready);
}

/// forward 删除命令即使返回成功，也要等列表确认端口消失后才能放弃重试所有权。
#[test]
fn forward_owner_is_retained_until_list_confirms_removal() {
    let still_present = vec!["127.0.0.1:16384 tcp:43123 localabstract:fixture".to_owned()];
    assert_eq!(
        reconcile_owned_forward(
            Some(43_123),
            true,
            Some(&still_present),
            "localabstract:fixture",
        ),
        (Some(43_123), false)
    );
    let reused_by_another_session =
        vec!["127.0.0.1:16384 tcp:43123 localabstract:someone-else".to_owned()];
    assert_eq!(
        reconcile_owned_forward(
            Some(43_123),
            true,
            Some(&reused_by_another_session),
            "localabstract:fixture",
        ),
        (None, true)
    );
    assert_eq!(
        reconcile_owned_forward(Some(43_123), true, Some(&[]), "localabstract:fixture",),
        (None, true)
    );
    assert_eq!(
        reconcile_owned_forward(Some(43_123), true, None, "localabstract:fixture"),
        (Some(43_123), false)
    );
    assert_eq!(
        reconcile_owned_forward(None, true, Some(&still_present), "localabstract:fixture",),
        (Some(43_123), false)
    );
    assert_eq!(
        reconcile_owned_forward(None, true, None, "localabstract:fixture"),
        (None, false)
    );
}

/// 清理失败作为附加诊断保留，不能覆盖最先发生的运行态错误和日志位置。
#[test]
fn probe_failure_preserves_operation_and_cleanup_errors() {
    let journal_path: PathBuf = PathBuf::from("data/logs/runtime-fixture.jsonl");
    let error: RuntimeProbeError = probe_failed_after_cleanup(
        RuntimeProbeError::InvalidOutput {
            stage: "session.ready",
            message: "固定日志失败".to_owned(),
        },
        journal_path.clone(),
        || -> Result<(), RuntimeProbeError> {
            Err(RuntimeProbeError::Cleanup {
                messages: "固定清理失败".to_owned(),
            })
        },
    );

    match error {
        RuntimeProbeError::ProbeFailed {
            source,
            journal_path: actual_path,
            cleanup_error,
        } => {
            assert!(matches!(
                source.as_ref(),
                RuntimeProbeError::InvalidOutput {
                    stage: "session.ready",
                    ..
                }
            ));
            assert_eq!(actual_path, journal_path);
            assert_eq!(
                cleanup_error.as_deref(),
                Some("清理未完全成功: 固定清理失败")
            );
        }
        other => panic!("预期探针失败包装，实际为 {other:?}"),
    }
}

/// 只读验证失败时仍须保留随后发生的安全卸载错误。
#[test]
fn probe_execution_preserves_graceful_unload_failure() {
    let journal_path: PathBuf = PathBuf::from("data/logs/runtime-fixture.jsonl");
    let error = finish_probe_execution_after_unload(
        Err::<(), RuntimeProbeError>(RuntimeProbeError::InvalidOutput {
            stage: "rpc.snapshot_consistency",
            message: "固定读取失败".to_owned(),
        }),
        Err::<(), RuntimeProbeError>(RuntimeProbeError::Cleanup {
            messages: "固定卸载失败".to_owned(),
        }),
        journal_path.clone(),
        Vec::new(),
    )
    .unwrap_err();

    match error {
        RuntimeProbeError::ProbeFailed {
            source,
            journal_path: actual_path,
            cleanup_error,
        } => {
            assert!(matches!(
                source.as_ref(),
                RuntimeProbeError::InvalidOutput {
                    stage: "rpc.snapshot_consistency",
                    ..
                }
            ));
            assert_eq!(actual_path, journal_path);
            assert_eq!(
                cleanup_error.as_deref(),
                Some("安全卸载失败: 清理未完全成功: 固定卸载失败")
            );
        }
        other => panic!("预期探针失败包装，实际为 {other:?}"),
    }
}

/// 握手后的失败必须先安全卸载再清理，并同时保留两个动作的诊断。
#[test]
fn authenticated_failure_runs_ordered_cleanup_and_preserves_errors() {
    let journal_path: PathBuf = PathBuf::from("data/logs/runtime-fixture.jsonl");
    let mut actions: Vec<&str> = Vec::new();
    let error: RuntimeProbeError = probe_failed_after_ordered_cleanup(
        RuntimeProbeError::InvalidOutput {
            stage: "rpc.wait_bag_proxy",
            message: "固定启动失败".to_owned(),
        },
        journal_path.clone(),
        &mut actions,
        |actions| -> Result<(), String> {
            actions.push("graceful_unload");
            Err("固定安全卸载失败".to_owned())
        },
        |actions| -> Result<(), String> {
            actions.push("cleanup");
            Err("固定资源清理失败".to_owned())
        },
    );

    assert_eq!(actions, ["graceful_unload", "cleanup"]);
    match error {
        RuntimeProbeError::ProbeFailed {
            source,
            journal_path: actual_path,
            cleanup_error,
        } => {
            assert!(matches!(
                source.as_ref(),
                RuntimeProbeError::InvalidOutput {
                    stage: "rpc.wait_bag_proxy",
                    ..
                }
            ));
            assert_eq!(actual_path, journal_path);
            assert_eq!(
                cleanup_error.as_deref(),
                Some("固定安全卸载失败 | 固定资源清理失败")
            );
        }
        other => panic!("预期探针失败包装，实际为 {other:?}"),
    }
}

/// 安全卸载和资源清理均成功时，原始启动错误不应被标成清理失败。
#[test]
fn authenticated_failure_with_successful_cleanup_has_no_cleanup_error() {
    let mut actions: Vec<&str> = Vec::new();
    let error: RuntimeProbeError = probe_failed_after_ordered_cleanup(
        RuntimeProbeError::InvalidOutput {
            stage: "session.ready",
            message: "固定日志失败".to_owned(),
        },
        PathBuf::from("data/logs/runtime-fixture.jsonl"),
        &mut actions,
        |actions| -> Result<(), String> {
            actions.push("graceful_unload");
            Ok(())
        },
        |actions| -> Result<(), String> {
            actions.push("cleanup");
            Ok(())
        },
    );

    assert_eq!(actions, ["graceful_unload", "cleanup"]);
    assert!(matches!(
        error,
        RuntimeProbeError::ProbeFailed {
            cleanup_error: None,
            ..
        }
    ));
}

/// 生产关闭状态在资源清理失败后保留同一方式证据，外部条件恢复后只重试清理。
#[cfg(target_os = "windows")]
#[test]
fn runtime_cleanup_failure_retains_method_for_retry() {
    let mut method = RuntimeShutdownMethodEvidence::RestartFallback {
        unload_error: "固定安全卸载失败".to_owned(),
        journal_errors: vec!["原关闭证据日志失败".to_owned()],
    };
    let mut pending_errors = vec!["命令收据日志失败".to_owned(), "卸载失败日志失败".to_owned()];
    merge_runtime_session_journal_errors(&mut method, &mut pending_errors);
    assert!(pending_errors.is_empty());
    let (method, first_error) = attempt_runtime_session_cleanup(method, || {
        Err::<(), RuntimeProbeError>(RuntimeProbeError::Cleanup {
            messages: "固定资源清理失败".to_owned(),
        })
    })
    .unwrap_err();
    assert!(matches!(*first_error, RuntimeProbeError::Cleanup { .. }));

    let (method, result) =
        attempt_runtime_session_cleanup(method, || Ok::<u32, RuntimeProbeError>(7)).unwrap();
    assert_eq!(result, 7);
    assert!(matches!(
        method,
        RuntimeShutdownMethodEvidence::RestartFallback {
            unload_error,
            journal_errors,
        } if unload_error == "固定安全卸载失败"
            && journal_errors == [
                "命令收据日志失败",
                "卸载失败日志失败",
                "原关闭证据日志失败",
            ]
    ));
}

#[test]
/// 完成日志失败时不得缓存结果，下一次关闭应重新写入并只在成功后完成。
fn cleanup_completion_is_cached_only_after_journal_success() {
    let mut process = super::empty_process_evidence(42);
    process.process_start_time = 99;
    let result = super::CleanupResult {
        evidence: process,
        cleanup: super::CleanupEvidence {
            forward_removed: true,
            forward_list_restored: true,
            device_session_removed: true,
            host_session_removed: true,
            old_process_stopped: false,
            game_restarted: false,
        },
    };
    let details = super::cleanup_journal_details(&result);
    assert_eq!(details["old_process_stopped"], false);
    assert_eq!(details["process"]["process_id"], 42);
    assert_eq!(details["process"]["process_start_time"], 99);
    let mut cached = None;
    let mut writes: u32 = 0;

    let first = record_cleanup_completion(&mut cached, result.clone(), |_| {
        writes += 1;
        Err(RuntimeProbeError::Cleanup {
            messages: "固定日志写入失败".to_owned(),
        })
    });
    assert!(first.is_err());
    assert!(cached.is_none());

    let completed = record_cleanup_completion(&mut cached, result.clone(), |_| {
        writes += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(writes, 2);
    assert_eq!(completed, result);
    assert_eq!(cached, Some(result));
}

#[test]
/// 旧 PID 未确认消失时不得执行重启动作，确认后只执行一次。
fn game_restart_waits_for_confirmed_process_stop() {
    let mut restart_count: u32 = 0;
    let error = restart_after_process_stop(false, 42, || {
        restart_count += 1;
        Ok::<u32, RuntimeProbeError>(43)
    })
    .unwrap_err();
    assert_eq!(restart_count, 0);
    assert!(error.to_string().contains("PID 42"));

    let process_id = restart_after_process_stop(true, 42, || {
        restart_count += 1;
        Ok::<u32, RuntimeProbeError>(43)
    })
    .unwrap();
    assert_eq!(process_id, 43);
    assert_eq!(restart_count, 1);
}

#[test]
/// 原进程已消失或进程等待已失败时，清理不再进入重启空等。
fn process_recovery_skips_restart_when_original_instance_is_gone() {
    assert_eq!(
        process_recovery_action(false, OriginalProcessPresence::Absent),
        ProcessRecoveryAction::PreserveStoppedProcess
    );
    assert_eq!(
        process_recovery_action(false, OriginalProcessPresence::Present),
        ProcessRecoveryAction::RestartGame
    );
    assert_eq!(
        process_recovery_action(false, OriginalProcessPresence::Unconfirmed),
        ProcessRecoveryAction::SkipProcessWait
    );
    assert_eq!(
        process_recovery_action(true, OriginalProcessPresence::Absent),
        ProcessRecoveryAction::PreserveStoppedProcess
    );
    assert_eq!(
        process_recovery_action(true, OriginalProcessPresence::Present),
        ProcessRecoveryAction::SkipProcessWait
    );
}

#[test]
fn preserved_process_evidence_confirms_exit_after_collection_failure() {
    for stage in [
        "evidence.process_identity_start",
        "evidence.process_status",
        "evidence.azlw_maps",
    ] {
        let mut state = TargetRecoveryState::PreserveOriginal;
        let mut checks = 0;
        let evidence = reconcile_preserved_process_evidence(
            &mut state,
            Err(RuntimeProbeError::DeviceCommand {
                stage,
                exit_code: 1,
                output: "proc missing".to_owned(),
            }),
            42,
            Some(123),
            || {
                checks += 1;
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(checks, 1);
        assert_eq!(evidence, super::empty_process_evidence(0));
        assert_eq!(state, TargetRecoveryState::OriginalTerminated);
    }
}

#[test]
fn preserved_process_evidence_keeps_live_identity_and_residuals() {
    let mut state = TargetRecoveryState::PreserveOriginal;
    let mut evidence = super::empty_process_evidence(42);
    evidence.process_start_time = 123;
    evidence.azlw_map_lines.push("azlw mapping".to_owned());
    let result = reconcile_preserved_process_evidence(
        &mut state,
        Ok(evidence.clone()),
        42,
        Some(123),
        || panic!("同一实例的完整证据无需复核"),
    )
    .unwrap();
    assert_eq!(result, evidence);
    assert_eq!(state, TargetRecoveryState::PreserveOriginal);
}

#[test]
fn preserved_process_evidence_rechecks_missing_or_reused_pid() {
    for (pid, start) in [(0, 0), (42, 456), (43, 456)] {
        let mut evidence = super::empty_process_evidence(pid);
        evidence.process_start_time = start;
        let mut state = TargetRecoveryState::PreserveOriginal;
        let result =
            reconcile_preserved_process_evidence(&mut state, Ok(evidence), 42, Some(123), || {
                Ok(true)
            })
            .unwrap();
        assert_eq!(state, TargetRecoveryState::OriginalTerminated);
        assert_eq!(result.process_id, 0);
    }
}

#[test]
fn preserved_process_collection_errors_keep_retry_state() {
    let mut state = TargetRecoveryState::PreserveOriginal;
    let error = reconcile_preserved_process_evidence(
        &mut state,
        Err(RuntimeProbeError::HostCommand {
            stage: "evidence.process_status",
            message: "offline".to_owned(),
        }),
        42,
        Some(123),
        || panic!("通信失败后本轮不再探测"),
    )
    .unwrap_err();
    assert!(cleanup_device_unreachable(&error));
    assert_eq!(state, TargetRecoveryState::PreserveOriginal);

    let error = reconcile_preserved_process_evidence(
        &mut state,
        Err(RuntimeProbeError::DeviceCommand {
            stage: "evidence.azlw_maps",
            exit_code: 1,
            output: "permission denied".to_owned(),
        }),
        42,
        Some(123),
        || Ok(false),
    )
    .unwrap_err();
    assert!(error.to_string().contains("permission denied"));
    assert_eq!(state, TargetRecoveryState::PreserveOriginal);

    let evidence = reconcile_preserved_process_evidence(
        &mut state,
        Ok(super::empty_process_evidence(0)),
        42,
        Some(123),
        || Ok(true),
    )
    .unwrap();
    assert_eq!(state, TargetRecoveryState::OriginalTerminated);
    assert_eq!(evidence.process_id, 0);
}

#[test]
fn preserved_process_exit_probe_failure_keeps_both_diagnostics() {
    let mut state = TargetRecoveryState::PreserveOriginal;
    let error = reconcile_preserved_process_evidence(
        &mut state,
        Err(RuntimeProbeError::DeviceCommand {
            stage: "evidence.azlw_maps",
            exit_code: 1,
            output: "read failed".to_owned(),
        }),
        42,
        Some(123),
        || {
            Err(RuntimeProbeError::HostCommand {
                stage: "cleanup.wait_old_process_identity",
                message: "offline".to_owned(),
            })
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("read failed"));
    assert!(error.to_string().contains("offline"));
    assert_eq!(state, TargetRecoveryState::PreserveOriginal);
}

#[test]
fn cleanup_host_failure_preserves_pending_state_across_attempts() {
    let directory = TestDirectory::new("cleanup-offline");
    for (path, bytes) in [
        (super::LOADER_RELATIVE_PATH, b"loader fixture".as_slice()),
        (super::AGENT_RELATIVE_PATH, b"agent fixture".as_slice()),
        (
            super::PROFILE_RELATIVE_PATH,
            include_bytes!("../../../../runtime/resources/profiles/default.json").as_slice(),
        ),
    ] {
        let path = directory.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let missing = directory
        .root
        .join("unavailable-command.exe")
        .to_string_lossy()
        .into_owned();
    fs::write(&missing, b"invalid executable").unwrap();
    for state in [
        TargetRecoveryState::PreserveOriginal,
        TargetRecoveryState::RestartRequired,
        TargetRecoveryState::RestartStarted,
    ] {
        for forward_attempted in [false, true] {
            let options = RuntimeProbeOptions::new(
                &directory.root,
                &missing,
                &missing,
                "0",
                "127.0.0.1:16384",
            )
            .unwrap();
            let mut runner = super::ProductionSession::new(options, None).unwrap();
            runner.target_pid = 42;
            runner.expected_process_start_time = Some(123);
            runner.target_recovery_state = state;
            runner.forward_creation_attempted = forward_attempted;
            for _ in 0..2 {
                assert!(runner.cleanup().is_err());
                assert_eq!(runner.target_recovery_state, state);
                assert!(!runner.process_wait_exhausted);
                assert!(runner.cleanup_result.is_none());
            }
        }
    }
}

#[test]
/// 清理阶段宿主命令失败后不再继续向设备发等待类命令。
fn host_command_failure_marks_cleanup_device_unreachable() {
    assert!(cleanup_device_unreachable(
        &RuntimeProbeError::HostCommand {
            stage: "cleanup.remove_staging",
            message: "超过 15 秒仍未退出".to_owned(),
        }
    ));
    assert!(!cleanup_device_unreachable(
        &RuntimeProbeError::DeviceCommand {
            stage: "cleanup.remove_staging",
            exit_code: 1,
            output: "busy".to_owned(),
        }
    ));
}

/// Windows 上每条 ADB 路径都复用调用方持有的动态服务端口。
#[cfg(target_os = "windows")]
#[test]
fn device_bridge_prepends_owned_port_to_adb_commands() {
    let root: PathBuf = std::env::current_dir().unwrap();
    let options: RuntimeProbeOptions =
        RuntimeProbeOptions::new(&root, "manager", "adb", "0", "127.0.0.1:16384")
            .unwrap()
            .with_adb_server_port(61_234)
            .unwrap();
    let bridge: DeviceBridge = DeviceBridge::new(&options);

    assert_eq!(
        bridge.adb_complete_arguments(&["shell".to_owned(), "id".to_owned()]),
        ["-P", "61234", "shell", "id"]
    );
}

#[test]
/// 验证 PID 解析仅接受一个非零十进制值。
fn pid_parser_requires_exactly_one_positive_value() {
    assert_eq!(parse_single_pid("12345\n", "test").unwrap(), 12_345);
    assert!(parse_single_pid("123 456", "test").is_err());
    assert!(parse_single_pid("0", "test").is_err());
}

/// 模块就绪探针只引用已校验模块名和确定 PID，不扩大设备搜索范围。
#[test]
fn module_probe_targets_exact_process_map() {
    assert_eq!(
        module_map_probe_command(12_345, "libtolua.so"),
        "grep -F -q /libtolua.so /proc/12345/maps"
    );
}

/// loader 必须在同一 root shell 调用中复核唯一 PID 和目标模块后才执行。
#[test]
fn loader_command_guards_final_target_binding() {
    let command: String = guarded_loader_command(
        "com.bilibili.azurlane",
        "libtolua.so",
        12_345,
        "/session/loader",
        "/session/agent.so",
        "/session/bootstrap.bin",
    );

    assert_eq!(
        command,
        "azlw_pid=\"$(pidof com.bilibili.azurlane)\" || { echo 'AZLW_TARGET_NOT_READY'; exit 70; }; \
             test \"$azlw_pid\" = \"12345\" || { echo \"AZLW_TARGET_CHANGED expected=12345 actual=$azlw_pid\"; exit 71; }; \
             grep -F -q /libtolua.so /proc/12345/maps || { echo 'AZLW_TARGET_MODULE_NOT_READY'; exit 72; }; \
             echo AZLW_LOADER_ENTERED; \
             exec /session/loader --pid 12345 --agent /session/agent.so --session-file /session/bootstrap.bin"
    );
}

/// 线程名读取只忽略枚举后消失的单个线程文件，并确认目标进程始终存在。
#[test]
fn thread_name_probe_tolerates_only_vanished_task_files() {
    assert_eq!(
        thread_name_probe_command(12_345),
        "test -d /proc/12345/task || exit 1; \
             for azlw_task in /proc/12345/task/*/comm; do \
             cat \"$azlw_task\" 2>/dev/null || test ! -e \"$azlw_task\" || exit 1; \
             done; test -d /proc/12345/task"
    );
}

/// 错误摘要最多保留前三条内容，并明确给出未展开的剩余数量。
#[test]
fn read_error_summary_is_bounded() {
    let errors = [
        ("first", "第一项"),
        ("second", "第二项"),
        ("third", "第三项"),
        ("fourth", "第四项"),
        ("fifth", "第五项"),
    ];
    assert_eq!(
        summarize_read_errors(errors.len(), errors.into_iter()),
        "first: 第一项 | second: 第二项 | third: 第三项 | 其余 2 项"
    );
    assert_eq!(summarize_read_errors(0, std::iter::empty()), "无");
}

/// JSONL 编码负责转义换行，错误正文完整保留。
#[test]
fn journal_error_summary_preserves_complete_details() {
    let error = RuntimeProbeError::InvalidOutput {
        stage: "test",
        message: format!("第一行\n第二行\r{}", "详情".repeat(300)),
    };

    let summary = journal_error_summary(&error);

    assert!(summary.contains("第一行\n第二行\r"));
    assert!(summary.ends_with(&"详情".repeat(300)));
    assert_eq!(summary, error.to_string());
}

/// 完整状态失败日志保留稳定分类、对象上下文和底层原因。
#[test]
fn journal_game_state_error_keeps_structured_diagnostics() {
    let source = AppError::from_source(
        "game.read.owned_before",
        AppErrorCode::GameNotReady,
        "账号状态尚未就绪",
        io::Error::other("fixture\nnot ready"),
    )
    .with_context("ship_id", "9001");
    let error = RuntimeProbeError::GameStateRead(source);

    let summary = journal_error_summary(&error);

    assert!(summary.contains("GAME_NOT_READY"));
    assert!(summary.contains("game.read.owned_before"));
    assert!(summary.contains("ship_id"));
    assert!(summary.contains("9001"));
    assert!(summary.contains("fixture\nnot ready"));
}

/// 装备映射日志只公开固定来源标签，不写入原生或 Lua 诊断正文。
#[test]
fn journal_equipment_mapping_summary_redacts_dynamic_diagnostics() {
    let error = RuntimeProbeError::EquipmentRead(EquipmentReadError::Mapping(
        EquipmentMappingError::IncompleteConfig {
            config_id: 12_345,
            errors: vec![
                "GetSkill: LUA_PRIVATE_DIAGNOSTIC".to_owned(),
                "UNRECOGNIZED_PRIVATE_DIAGNOSTIC".to_owned(),
            ],
        },
    ));

    let summary = journal_error_summary(&error);

    assert!(summary.contains("装备配置 12345 不完整"));
    assert!(summary.contains("GetSkill"));
    assert!(summary.contains("未分类"));
    assert!(!summary.contains("LUA_PRIVATE_DIAGNOSTIC"));
    assert!(!summary.contains("UNRECOGNIZED_PRIVATE_DIAGNOSTIC"));

    let invalid = RuntimeProbeError::EquipmentRead(EquipmentReadError::Mapping(
        EquipmentMappingError::InvalidConfigField {
            config_id: 500,
            field: "attributes",
            message: "MAPPER_PRIVATE_DIAGNOSTIC".to_owned(),
        },
    ));
    let invalid_summary = journal_error_summary(&invalid);
    assert!(invalid_summary.contains("装备配置 500 的字段 attributes 无效"));
    assert!(invalid_summary.contains("未分类规则"));
    assert!(!invalid_summary.contains("MAPPER_PRIVATE_DIAGNOSTIC"));
}

/// 收尾失败只删除显式登记为本次已发布的文件，不误删同会话的既有冲突路径。
#[test]
fn finalization_cleanup_removes_only_confirmed_publications() {
    let fixture = TestDirectory::new("finalization-cleanup");
    let tool_root = ToolRoot::open(&fixture.root).unwrap();
    tool_root
        .ensure_directory(Path::new("data/history"))
        .unwrap();
    let sample = Path::new("data/history/001-equipment.json");
    let conflicting_receipt = Path::new("data/history/002-probe.json");
    fs::write(fixture.root.join(sample), b"sample").unwrap();
    fs::write(fixture.root.join(conflicting_receipt), b"existing").unwrap();

    let error = probe_finalization_failed(
        RuntimeProbeError::InvalidOutput {
            stage: "test",
            message: "收尾失败".to_owned(),
        },
        fixture.root.join("data/logs/001-runtime.jsonl"),
        &tool_root,
        &[("装备维护样本", sample)],
    );

    assert!(matches!(
        error,
        RuntimeProbeError::ProbeFailed {
            cleanup_error: None,
            ..
        }
    ));
    assert!(!fixture.root.join(sample).exists());
    assert_eq!(
        fs::read(fixture.root.join(conflicting_receipt)).unwrap(),
        b"existing"
    );
}

#[test]
/// 验证实机收据只保留计数和范围，并正确表达零经验与无技能舰船。
fn ship_growth_summary_is_deidentified_and_complete() {
    let ships: Vec<RuntimeShip> = vec![
        RuntimeShip {
            ship_id: 9_001,
            config_id: 101_174,
            level: 100,
            experience_in_level: 3_000_000,
            intimacy_raw: 10_000,
            energy: 150,
            proficiency: 0,
            fleet_memberships: Vec::new(),
            skills: vec![RuntimeShipSkill {
                skill_id: 10_410,
                level: 1,
                experience: 0,
            }],
            slots: Vec::new(),
        },
        RuntimeShip {
            ship_id: 9_002,
            config_id: 201_174,
            level: 125,
            experience_in_level: 0,
            intimacy_raw: 20_000,
            energy: 119,
            proficiency: 0,
            fleet_memberships: Vec::new(),
            skills: Vec::new(),
            slots: Vec::new(),
        },
    ];

    let summary = summarize_ship_growth(&ships).unwrap();

    assert_eq!(summary.distinct_config_count, 2);
    assert_eq!(summary.skill_count, 1);
    assert_eq!(summary.ships_without_skills, 1);
    assert_eq!(summary.level.unwrap().minimum, 100);
    assert_eq!(summary.level.unwrap().maximum, 125);
    assert_eq!(summary.experience_in_level.unwrap().minimum, 0);
    assert_eq!(summary.intimacy_raw.unwrap().maximum, 20_000);
    assert_eq!(summary.energy.unwrap().minimum, 119);
    assert_eq!(summary.proficiency.unwrap().maximum, 0);
    assert_eq!(summary.skill_level.unwrap().maximum, 1);
    assert_eq!(summary.skill_experience.unwrap().minimum, 0);
}

#[test]
/// 验证加载器收据严格拒绝未知字段并保留目标 PID。
fn loader_receipt_is_strict_and_pid_bound() {
    const RECEIPT: &str = "AZLW_RECEIPT {\"status\":\"ok\",\"code\":\"loaded\",\"message\":\"ok\",\"process_id\":42,\"process_start_time\":99,\"agent_handle\":\"0000000000007000\",\"agent_base\":\"0000000000100000\",\"agent_load_size\":131072,\"finalize_address\":\"0000000000101000\",\"agent_mapping_name\":\"/memfd:fixture (deleted)\",\"agent_mapping_mode\":\"memfd\",\"agent_visibility_mode\":\"normal\",\"agent_soinfo_address\":null,\"agent_protected_elf_header\":null,\"anonymous_segment_count\":0,\"anonymous_byte_count\":0}";
    let parsed: LoaderReceipt = parse_loader_receipt(RECEIPT).unwrap();
    assert_eq!(parsed.process_id, 42);
    parsed.validate_loaded(42).unwrap();
    let with_extra: String = RECEIPT.replace("}", ",\"extra\":true}");
    assert!(parse_loader_receipt(&with_extra).is_err());
    assert!(parse_loader_receipt(&format!("{RECEIPT}\n{RECEIPT}")).is_err());
    assert!(parsed.validate_loaded(43).is_err());

    let anonymous = RECEIPT
        .replace(
            "\"agent_mapping_mode\":\"memfd\"",
            "\"agent_mapping_mode\":\"anonymous_remap\"",
        )
        .replace(
            "\"anonymous_segment_count\":0",
            "\"anonymous_segment_count\":4",
        )
        .replace(
            "\"anonymous_byte_count\":0",
            "\"anonymous_byte_count\":65536",
        );
    parse_loader_receipt(&anonymous)
        .unwrap()
        .validate_loaded(42)
        .unwrap();
    let hidden = anonymous
        .replace(
            "\"agent_visibility_mode\":\"normal\"",
            "\"agent_visibility_mode\":\"solist_hidden\"",
        )
        .replace(
            "\"agent_soinfo_address\":null",
            "\"agent_soinfo_address\":\"0000000000770000\"",
        );
    parse_loader_receipt(&hidden)
        .unwrap()
        .validate_loaded(42)
        .unwrap();
    let strongest = hidden
        .replace(
            "\"agent_visibility_mode\":\"solist_hidden\"",
            "\"agent_visibility_mode\":\"solist_and_elf_header\"",
        )
        .replace(
            "\"agent_protected_elf_header\":null",
            &format!("\"agent_protected_elf_header\":\"{}\"", "a5".repeat(64)),
        );
    parse_loader_receipt(&strongest)
        .unwrap()
        .validate_loaded(42)
        .unwrap();
    let strongest_without_header = strongest.replace(
        &format!("\"agent_protected_elf_header\":\"{}\"", "a5".repeat(64)),
        "\"agent_protected_elf_header\":null",
    );
    assert!(
        parse_loader_receipt(&strongest_without_header)
            .unwrap()
            .validate_loaded(42)
            .is_err()
    );
    let strongest_with_uppercase = strongest.replace(&"a5".repeat(64), &"A5".repeat(64));
    assert!(
        parse_loader_receipt(&strongest_with_uppercase)
            .unwrap()
            .validate_loaded(42)
            .is_err()
    );
    let strongest_with_zero_header = strongest.replace(&"a5".repeat(64), &"00".repeat(64));
    assert!(
        parse_loader_receipt(&strongest_with_zero_header)
            .unwrap()
            .validate_loaded(42)
            .is_err()
    );
    let hidden_with_header = strongest.replace(
        "\"agent_visibility_mode\":\"solist_and_elf_header\"",
        "\"agent_visibility_mode\":\"solist_hidden\"",
    );
    assert!(
        parse_loader_receipt(&hidden_with_header)
            .unwrap()
            .validate_loaded(42)
            .is_err()
    );
    let hidden_without_soinfo = hidden.replace(
        "\"agent_soinfo_address\":\"0000000000770000\"",
        "\"agent_soinfo_address\":null",
    );
    assert!(
        parse_loader_receipt(&hidden_without_soinfo)
            .unwrap()
            .validate_loaded(42)
            .is_err()
    );
    let invalid_anonymous = anonymous.replace(
        "\"anonymous_segment_count\":4",
        "\"anonymous_segment_count\":0",
    );
    assert!(
        parse_loader_receipt(&invalid_anonymous)
            .unwrap()
            .validate_loaded(42)
            .is_err()
    );
}

#[test]
/// 验证失败清理只对未知现场、错误 PID 或不可信收据执行重启兜底。
fn loader_failure_receipt_controls_restart_without_guessing() {
    let receipt = |state: &str, process_id: u32| {
        format!(
            "AZLW_LOADER_ENTERED\nAZLW_RECEIPT {{\"status\":\"error\",\"code\":\"ptrace_attach_failed\",\"message\":\"fixture\",\"process_id\":{process_id},\"target_state\":\"{state}\"}}"
        )
    };

    assert!(!loader_failure_requires_restart(&receipt("restored", 42), 12, true, 42).unwrap());
    assert!(loader_failure_requires_restart(&receipt("unknown", 42), 12, true, 42).unwrap());
    let scratch_cleanup =
        receipt("unknown", 42).replace("ptrace_attach_failed", "remote_scratch_cleanup_failed");
    assert!(loader_failure_requires_restart(&scratch_cleanup, 15, true, 42).unwrap());
    assert!(loader_failure_requires_restart(&receipt("restored", 43), 12, true, 42).is_err());
    assert!(loader_failure_requires_restart(&receipt("unchanged", 42), 12, true, 42).is_err());
    let future_code = receipt("restored", 42).replace("ptrace_attach_failed", "future_error");
    assert!(loader_failure_requires_restart(&future_code, 12, true, 42).is_err());
    assert!(
        loader_failure_requires_restart(
            "AZLW_LOADER_ENTERED\nAZLW_RECEIPT {malformed}",
            12,
            true,
            42
        )
        .is_err()
    );
    let duplicate = format!("{}\n{}", receipt("restored", 42), receipt("restored", 42));
    assert!(loader_failure_requires_restart(&duplicate, 12, true, 42).is_err());
    assert!(!loader_failure_requires_restart("", 70, false, 42).unwrap());
    assert!(!loader_failure_requires_restart("", 71, false, 42).unwrap());
    assert!(!loader_failure_requires_restart("", 72, false, 42).unwrap());
    assert!(loader_failure_requires_restart("", 73, false, 42).unwrap());

    let unknown_field = receipt("restored", 42).replace(
        "\"target_state\":\"restored\"",
        "\"target_state\":\"restored\",\"extra\":true",
    );
    assert!(loader_failure_requires_restart(&unknown_field, 12, true, 42).is_err());

    let oversized_message = receipt("restored", 42).replace(
        "\"message\":\"fixture\"",
        &format!("\"message\":\"{}\"", "x".repeat(4 * 1024 + 1)),
    );
    assert!(loader_failure_requires_restart(&oversized_message, 12, true, 42).is_err());
}

#[test]
/// 诊断前导文本再长也不能挤掉独立 JSONL 中的完整失败收据。
fn loader_failure_receipt_remains_structured_after_verbose_output() {
    let diagnostic = "分离普通线程失败".repeat(120);
    let receipt = serde_json::json!({
        "status": "error",
        "code": "ptrace_detach_failed",
        "message": diagnostic,
        "process_id": 42,
        "target_state": "unknown",
    });
    let output = format!(
        "AZLW_LOADER_ENTERED\n{}\nAZLW_RECEIPT {receipt}",
        "I: injector diagnostic\n".repeat(200)
    );

    let decision = loader_failure_decision(&output, 15, true, 42).unwrap();
    assert!(decision.requires_restart);
    let parsed = decision.receipt.expect("已进入 loader 必须保留失败收据");
    let details = loader_failure_journal_details(15, &parsed);

    assert_eq!(details["exit_code"], 15);
    assert_eq!(details["receipt"]["code"], "ptrace_detach_failed");
    assert_eq!(details["receipt"]["message"], diagnostic);
    assert_eq!(details["receipt"]["process_id"], 42);
    assert_eq!(details["receipt"]["target_state"], "unknown");

    let summary = journal_error_summary(&RuntimeProbeError::LoaderRejected {
        exit_code: 15,
        code: parsed.code,
        message: parsed.message,
        process_id: parsed.process_id,
        target_state: parsed.target_state.as_str().to_owned(),
    });
    assert!(summary.starts_with(
            "加载器拒绝本次加载: code=ptrace_detach_failed, process_id=42, exit_code=15, target_state=unknown"
        ));
    assert!(summary.ends_with(&diagnostic));
}

#[test]
/// 验证卸载收据只接受固定成功码、原 PID 和完整字段集合。
fn unload_receipt_is_strict_and_pid_bound() {
    let parsed = parse_unload_receipt(
            "AZLW_RECEIPT {\"status\":\"ok\",\"code\":\"unloaded\",\"message\":\"ok\",\"process_id\":42}",
        )
        .unwrap();
    parsed.validate_unloaded(42).unwrap();
    assert!(parsed.validate_unloaded(43).is_err());
    assert!(parse_unload_receipt(
            "AZLW_RECEIPT {\"status\":\"ok\",\"code\":\"unloaded\",\"message\":\"ok\",\"process_id\":42,\"extra\":true}"
        )
        .is_err());
}

#[test]
/// 验证全部卸载稳定码保留失败身份，并拒绝命令状态或收据结构不一致。
fn unloader_failure_result_preserves_strict_receipt() {
    const CASES: &[(i32, &str, u32)] = &[
        (10, "unload_config_invalid", 0),
        (11, "unload_identity_rejected", 42),
        (11, "unload_identity_changed", 42),
        (12, "unload_ptrace_attach_failed", 42),
        (15, "unload_ptrace_detach_failed", 42),
        (15, "unload_remote_stack_cleanup_failed", 42),
        (15, "unload_ptrace_detach_except_carrier_failed", 42),
        (15, "unload_carrier_trace_lost", 42),
        (15, "unload_wait_capture_failed", 42),
        (15, "unload_carrier_exit_failed", 42),
        (15, "unload_carrier_exit_unverified", 42),
        (15, "unload_file_cleanup_failed", 42),
        (16, "unload_deadline_exceeded", 42),
        (16, "unload_memory_init_failed", 42),
        (16, "unload_injector_init_failed", 42),
        (16, "unload_remote_stack_prepare_failed", 42),
        (16, "agent_finalize_call_failed", 42),
        (16, "agent_finalize_rejected", 42),
        (16, "agent_finalize_incomplete", 42),
        (16, "agent_elf_header_restore_failed", 42),
        (16, "unload_carrier_primitives_missing", 42),
        (16, "unload_wait_state_conflict", 42),
        (16, "agent_dlclose_failed", 42),
        (16, "unload_carrier_exit_invalid", 42),
        (16, "agent_mapping_remains", 42),
        (16, "agent_not_quiescent", 42),
    ];
    let failure = |code: &str, message: &str, process_id: u32| {
        format!(
            "AZLW_UNLOADER_ENTERED\nAZLW_RECEIPT {{\"status\":\"error\",\"code\":\"{code}\",\"message\":\"{message}\",\"process_id\":{process_id}}}"
        )
    };

    for &(exit_code, code, process_id) in CASES {
        let output: String = failure(code, "fixture failure", process_id);
        let error = validate_unloader_result(&output, exit_code, true, 42).unwrap_err();
        match error {
            RuntimeProbeError::UnloaderRejected {
                exit_code: actual_exit_code,
                code: actual_code,
                message,
                process_id: actual_process_id,
            } => {
                assert_eq!(actual_exit_code, exit_code);
                assert_eq!(actual_code, code);
                assert_eq!(message, "fixture failure");
                assert_eq!(actual_process_id, process_id);
            }
            other => panic!("预期结构化卸载失败，实际为 {other:?}"),
        }

        let mismatched_exit_code: i32 = if exit_code == 11 { 12 } else { 11 };
        assert!(
            validate_unloader_result(&output, mismatched_exit_code, true, 42).is_err(),
            "稳定码 {code} 不得接受退出码 {mismatched_exit_code}"
        );
        let mismatched_process_id: u32 = if process_id == 0 { 42 } else { 43 };
        let mismatched_process: String = failure(code, "fixture failure", mismatched_process_id);
        assert!(
            validate_unloader_result(&mismatched_process, exit_code, true, 42).is_err(),
            "稳定码 {code} 不得接受进程 {mismatched_process_id}"
        );
    }

    let diagnostic_failure: String = failure(
        "agent_dlclose_failed",
        "remote dlclose failed, status=5, stage=return_regs_read, errno=4",
        42,
    );
    match validate_unloader_result(&diagnostic_failure, 16, true, 42).unwrap_err() {
        RuntimeProbeError::UnloaderRejected { message, .. } => {
            assert_eq!(
                message,
                "remote dlclose failed, status=5, stage=return_regs_read, errno=4"
            );
        }
        other => panic!("预期保留卸载阶段诊断，实际为 {other:?}"),
    }

    let identity_failure: String = failure("unload_identity_changed", "fixture failure", 42);
    assert!(matches!(
        validate_unloader_result(&identity_failure, 11, false, 42),
        Err(RuntimeProbeError::DeviceCommand { .. })
    ));
    assert!(validate_unloader_result("AZLW_UNLOADER_ENTERED", 11, true, 42).is_err());
    assert!(
        validate_unloader_result(
            "AZLW_UNLOADER_ENTERED\nAZLW_RECEIPT {malformed}",
            11,
            true,
            42,
        )
        .is_err()
    );
    assert!(
        validate_unloader_result(
            &format!("{identity_failure}\n{identity_failure}"),
            11,
            true,
            42,
        )
        .is_err()
    );
    assert!(
        validate_unloader_result(&failure("unload_identity_changed", "", 42), 11, true, 42,)
            .is_err()
    );
    assert!(
        validate_unloader_result(
            &failure(
                "unload_identity_changed",
                &"x".repeat(super::MAXIMUM_FAILURE_RECEIPT_MESSAGE_BYTES + 1),
                42,
            ),
            11,
            true,
            42,
        )
        .is_err()
    );
    assert!(
        validate_unloader_result(
            &identity_failure.replace("}", ",\"extra\":true}"),
            11,
            true,
            42,
        )
        .is_err()
    );
}

#[test]
/// 验证 proc stat 解析不受线程名中的空格和右括号影响。
fn proc_stat_start_time_uses_field_twenty_two() {
    let stat = "42 (azlw worker) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 20";
    assert_eq!(
        parse_proc_stat_start_time(stat, 42, "test.proc_stat").unwrap(),
        98_765
    );
    assert!(parse_proc_stat_start_time(stat, 43, "test.proc_stat").is_err());
    assert!(parse_proc_stat_start_time("42 malformed", 42, "test.proc_stat").is_err());
}

#[test]
/// 验证宿主拒绝精确 Agent 身份，并区分不可访问占位与稍后的地址复用。
fn unloaded_mapping_validation_checks_identity_and_address_reuse() {
    let clean = concat!(
        "1000-2000 r-xp 00000000 00:00 0 /system/lib64/libc.so\n",
        "3000-4000 rw-p 00000000 00:00 0 [anon:other]\n"
    );
    assert_eq!(
        validate_post_unload_mappings(clean, 0x5000, 0x1000, "/memfd:fixture").unwrap(),
        PostUnloadMappingEvidence {
            inert_anonymous_overlap_count: 0,
            reused_address_overlap_count: 0,
        }
    );
    assert!(validate_post_unload_mappings("", 0x5000, 0x1000, "/memfd:fixture").is_err());
    assert_eq!(
        validate_post_unload_mappings(
            "5800-5900 ---p 00000000 00:00 0 \n",
            0x5000,
            0x1000,
            "/memfd:fixture"
        )
        .unwrap(),
        PostUnloadMappingEvidence {
            inert_anonymous_overlap_count: 1,
            reused_address_overlap_count: 0,
        }
    );
    assert_eq!(
        validate_post_unload_mappings(
            "5800-5900 rw-p 00000000 00:00 0 [anon:scudo:secondary]\n",
            0x5000,
            0x1000,
            "/memfd:fixture"
        )
        .unwrap(),
        PostUnloadMappingEvidence {
            inert_anonymous_overlap_count: 0,
            reused_address_overlap_count: 1,
        }
    );
    assert!(
        validate_post_unload_mappings(
            "7000-8000 r-xp 00000000 00:00 0 /memfd:fixture (deleted)\n",
            0x5000,
            0x1000,
            "/memfd:fixture"
        )
        .is_err()
    );
    assert!(
        validate_post_unload_mappings(
            "7000-8000 r-xp 00000000 00:00 0 /memfd:fixture\n",
            0x5000,
            0x1000,
            "/memfd:fixture (deleted)"
        )
        .is_err()
    );
    for reused_overlap in [
        "5800-5900 r--p 00000000 00:00 0 \n",
        "5800-5900 ---s 00000000 00:00 0 \n",
        "5800-5900 ---p 00001000 00:00 0 \n",
        "5800-5900 ---p 00000000 08:06 0 \n",
        "5800-5900 ---p 00000000 00:00 1 \n",
        "5800-5900 ---p 00000000 00:00 0 [anon:guard]\n",
    ] {
        assert_eq!(
            validate_post_unload_mappings(reused_overlap, 0x5000, 0x1000, "/memfd:fixture")
                .unwrap(),
            PostUnloadMappingEvidence {
                inert_anonymous_overlap_count: 0,
                reused_address_overlap_count: 1,
            },
            "应记录不同身份的地址复用: {reused_overlap:?}"
        );
    }
    for malformed_overlap in [
        "5800-5900 rw-p invalid 00:00 0 [anon:reused]\n",
        "5800-5900 rw-p 00000000 00:00 invalid [anon:reused]\n",
    ] {
        assert!(
            validate_post_unload_mappings(malformed_overlap, 0x5000, 0x1000, "/memfd:fixture")
                .is_err(),
            "应拒绝不可解析的映射证据: {malformed_overlap:?}"
        );
    }
    assert_eq!(
        validate_post_unload_mappings(
            concat!(
                "4000-5000 rw-p 00000000 00:00 0 [anon:before]\n",
                "6000-7000 rw-p 00000000 00:00 0 [anon:after]\n"
            ),
            0x5000,
            0x1000,
            "/memfd:fixture"
        )
        .unwrap(),
        PostUnloadMappingEvidence {
            inert_anonymous_overlap_count: 0,
            reused_address_overlap_count: 0,
        }
    );
}

#[test]
/// 卸载命令必须在执行 loader 前复核原 PID 和 profile 模块。
fn guarded_unloader_rechecks_target_before_exec() {
    let command = guarded_unloader_command(
        "com.bilibili.azurlane",
        "libtolua.so",
        12_345,
        "/session/loader",
        "/session/agent.so",
        "/session/unload.bin",
    );
    assert!(command.contains("test \"$azlw_pid\" = \"12345\""));
    assert!(command.contains("grep -F -q /libtolua.so /proc/12345/maps"));
    assert!(command.contains("echo AZLW_UNLOADER_ENTERED"));
    assert!(command.contains(
        "exec /session/loader unload --agent /session/agent.so --session-file /session/unload.bin"
    ));
}

#[test]
/// 验证背包就绪重试同时受指令、会话影响和错误码白名单约束。
fn bag_readiness_retry_requires_directive_session_and_code_allowlist() {
    let mut error = AgentError {
        code: "lua_bag_proxy_invalid".to_owned(),
        stage: "agent.lua".to_owned(),
        message: "BagProxy 尚未就绪".to_owned(),
        retry: RetryDirective::SameRequest,
        session_effect: SessionEffect::Unchanged,
        details: BTreeMap::new(),
    };
    assert!(is_retryable_bag_readiness_error(&error));

    error.retry = RetryDirective::Never;
    assert!(!is_retryable_bag_readiness_error(&error));
    error.retry = RetryDirective::SameRequest;
    error.code = "lua_get_proxy_failed".to_owned();
    assert!(is_retryable_bag_readiness_error(&error));
    error.code = "lua_bag_data_lookup_failed".to_owned();
    assert!(!is_retryable_bag_readiness_error(&error));
    error.code = "lua_bag_proxy_invalid".to_owned();
    error.session_effect = SessionEffect::MustClose;
    assert!(!is_retryable_bag_readiness_error(&error));
}

#[test]
/// 舰船详情轮询只接受当前实现会明确标记为同会话可重试的初始化错误。
fn ship_details_retry_requires_directive_session_and_code_allowlist() {
    let mut error = AgentError {
        code: "lua_bay_data_invalid".to_owned(),
        stage: "agent.lua".to_owned(),
        message: "BayProxy.data 尚未就绪".to_owned(),
        retry: RetryDirective::SameRequest,
        session_effect: SessionEffect::Unchanged,
        details: BTreeMap::new(),
    };

    assert!(is_retryable_ship_details_readiness_error(&error));
    error.code = "ship_detail_growth_invalid".to_owned();
    assert!(!is_retryable_ship_details_readiness_error(&error));
    error.code = "lua_bay_data_invalid".to_owned();
    error.retry = RetryDirective::Never;
    assert!(!is_retryable_ship_details_readiness_error(&error));
}

#[test]
/// 验证完整运行态只为已知代理初始化状态开放同会话轮询。
fn owned_state_readiness_retry_uses_explicit_proxy_allowlist() {
    let mut error = AgentError {
        code: "lua_bay_proxy_invalid".to_owned(),
        stage: "agent.lua".to_owned(),
        message: "BayProxy 尚未就绪".to_owned(),
        retry: RetryDirective::SameRequest,
        session_effect: SessionEffect::Unchanged,
        details: BTreeMap::new(),
    };
    assert!(is_retryable_owned_state_readiness_error(&error));

    error.code = "lua_player_data_invalid".to_owned();
    assert!(is_retryable_owned_state_readiness_error(&error));
    error.code = "lua_player_gold_invalid".to_owned();
    assert!(!is_retryable_owned_state_readiness_error(&error));
    error.code = "lua_equipment_proxy_invalid".to_owned();
    error.retry = RetryDirective::Never;
    assert!(!is_retryable_owned_state_readiness_error(&error));
}

struct TestDirectory {
    root: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).expect("测试需要操作系统随机源");
        let root = home
            .join("suzushiro/scratch/azlw-probe-tests")
            .join(format!(
                "{label}-{}-{:016x}",
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

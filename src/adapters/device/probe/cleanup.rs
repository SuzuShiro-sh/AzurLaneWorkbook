//! 定义探针清理证据、资源所有权状态和无副作用的收敛规则。

use serde::Serialize;
use serde_json::json;

use super::device_bridge::PROCESS_PROBE_COMMAND_TIMEOUT;
use super::journal::{ProbeJournal, journal_error_summary};
use super::process_evidence::{
    ProcessEvidence, ProcessIdentity, empty_process_evidence, parse_single_pid,
    validate_preserved_process_identity,
};
use super::{ProductionSession, RuntimeProbeError};

/// 清理当前会话资源后，记录游戏进程是被保留还是由失败兜底重新启动。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CleanupEvidence {
    pub forward_removed: bool,
    pub forward_list_restored: bool,
    pub device_session_removed: bool,
    pub host_session_removed: bool,
    pub old_process_stopped: bool,
    pub game_restarted: bool,
}

/// 汇总清理后进程证据和当前会话资源清理结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CleanupResult {
    pub(super) evidence: ProcessEvidence,
    pub(super) cleanup: CleanupEvidence,
}

/// 区分原进程保留、确认退出和重启阶段，避免清理重试重复启动游戏。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TargetRecoveryState {
    PreserveOriginal,
    OriginalTerminated,
    RestartRequired,
    RestartStarted,
    Restarted(ProcessIdentity),
}

/// 清理前对原游戏进程实例的一次观察结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OriginalProcessPresence {
    Absent,
    Present,
    Unconfirmed,
}

/// 原进程已消失、仍需重启、或设备不可达时跳过进程等待。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProcessRecoveryAction {
    PreserveStoppedProcess,
    RestartGame,
    SkipProcessWait,
}

/// 按原进程是否仍在、以及进程等待是否已经失败，决定清理是否启动游戏。
pub(super) fn process_recovery_action(
    wait_exhausted: bool,
    presence: OriginalProcessPresence,
) -> ProcessRecoveryAction {
    if presence == OriginalProcessPresence::Absent {
        ProcessRecoveryAction::PreserveStoppedProcess
    } else if wait_exhausted || presence == OriginalProcessPresence::Unconfirmed {
        ProcessRecoveryAction::SkipProcessWait
    } else {
        ProcessRecoveryAction::RestartGame
    }
}

/// 采集失败或身份变化时复核原实例；只有确认退出才提交终态，通信故障保留重试状态。
pub(super) fn reconcile_preserved_process_evidence(
    state: &mut TargetRecoveryState,
    evidence: Result<ProcessEvidence, RuntimeProbeError>,
    process_id: u32,
    process_start_time: Option<u64>,
    confirm_absent: impl FnOnce() -> Result<bool, RuntimeProbeError>,
) -> Result<ProcessEvidence, RuntimeProbeError> {
    let error = match evidence {
        Ok(evidence) => match validate_preserved_process_identity(
            &evidence,
            process_id,
            process_start_time,
            "cleanup.verify_process_identity",
        ) {
            Ok(()) => return Ok(evidence),
            Err(error) => error,
        },
        Err(error) => error,
    };
    if cleanup_device_unreachable(&error) {
        return Err(error);
    }
    match confirm_absent() {
        Ok(true) => {
            *state = TargetRecoveryState::OriginalTerminated;
            Ok(empty_process_evidence(0))
        }
        Ok(false) => Err(error),
        Err(verification_error) => Err(RuntimeProbeError::Cleanup {
            messages: format!("{error} | 复核原进程退出失败: {verification_error}"),
        }),
    }
}

/// 宿主命令失败表示本轮无法继续向设备发清理命令。
pub(super) fn cleanup_device_unreachable(error: &RuntimeProbeError) -> bool {
    matches!(error, RuntimeProbeError::HostCommand { .. })
}

fn record_cleanup_device_error(
    errors: &mut Vec<String>,
    device_unavailable: &mut bool,
    error: RuntimeProbeError,
) {
    if cleanup_device_unreachable(&error) {
        *device_unavailable = true;
    }
    errors.push(error.to_string());
}

/// 完成事件持久化成功后才缓存清理结果，使日志故障仍可由下一次关闭重试。
pub(super) fn record_cleanup_completion(
    cached: &mut Option<CleanupResult>,
    result: CleanupResult,
    record: impl FnOnce(&CleanupResult) -> Result<(), RuntimeProbeError>,
) -> Result<CleanupResult, RuntimeProbeError> {
    record(&result)?;
    *cached = Some(result.clone());
    Ok(result)
}

/// 只有旧 PID 已经确认消失，才允许执行会重新启动游戏的动作。
pub(super) fn restart_after_process_stop<T>(
    old_process_stopped: bool,
    process_id: u32,
    restart: impl FnOnce() -> Result<T, RuntimeProbeError>,
) -> Result<T, RuntimeProbeError> {
    if !old_process_stopped {
        return Err(RuntimeProbeError::Cleanup {
            messages: format!("旧游戏进程 PID {process_id} 尚未确认停止，已跳过重启"),
        });
    }
    restart()
}

/// 按远端会话身份查找唯一有效的动态 TCP 映射。
pub(super) fn find_owned_forward_port(
    forwards: &[String],
    expected_remote_endpoint: &str,
) -> Result<Option<u16>, RuntimeProbeError> {
    let mut ports: Vec<u16> = Vec::new();
    for entry in forwards {
        let mut fields = entry.split_whitespace();
        let _serial = fields.next();
        let local_endpoint: Option<&str> = fields.next();
        let remote_endpoint: Option<&str> = fields.next();
        if remote_endpoint != Some(expected_remote_endpoint) {
            continue;
        }
        let port: u16 = local_endpoint
            .and_then(|value| value.strip_prefix("tcp:"))
            .and_then(|value| value.parse().ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "forward.list",
                message: format!("本会话 forward 的本地端点无效: {entry:?}"),
            })?;
        ports.push(port);
    }
    ports.sort_unstable();
    ports.dedup();
    match ports.as_slice() {
        [] => Ok(None),
        [port] => Ok(Some(*port)),
        _ => Err(RuntimeProbeError::InvalidOutput {
            stage: "forward.list",
            message: format!(
                "本会话远端端点 {expected_remote_endpoint:?} 对应多个本地端口: {ports:?}"
            ),
        }),
    }
}

/// 只有 forward 列表明确不再包含当前会话的完整映射时才释放清理所有权。
pub(super) fn reconcile_owned_forward(
    owned_port: Option<u16>,
    creation_attempted: bool,
    forwards_after: Option<&[String]>,
    expected_remote_endpoint: &str,
) -> (Option<u16>, bool) {
    let Some(forwards) = forwards_after else {
        return match owned_port {
            Some(port) => (Some(port), false),
            None => (None, !creation_attempted),
        };
    };
    let Some(port) = owned_port else {
        return match find_owned_forward_port(forwards, expected_remote_endpoint) {
            Ok(Some(port)) => (Some(port), false),
            Ok(None) => (None, true),
            Err(_) => (None, false),
        };
    };
    let local_endpoint: String = format!("tcp:{port}");
    let still_present: bool = forwards.iter().any(|entry: &String| {
        let mut fields = entry.split_whitespace();
        let _serial = fields.next();
        fields.next() == Some(local_endpoint.as_str())
            && fields.next() == Some(expected_remote_endpoint)
    });
    if still_present {
        (Some(port), false)
    } else {
        (None, true)
    }
}

/// 保留既有清理字段，并附加最终进程实例和残留检测证据。
pub(super) fn cleanup_journal_details(result: &CleanupResult) -> serde_json::Value {
    json!({
        "forward_removed": result.cleanup.forward_removed,
        "forward_list_restored": result.cleanup.forward_list_restored,
        "device_session_removed": result.cleanup.device_session_removed,
        "host_session_removed": result.cleanup.host_session_removed,
        "old_process_stopped": result.cleanup.old_process_stopped,
        "game_restarted": result.cleanup.game_restarted,
        "process": &result.evidence,
    })
}

impl ProductionSession {
    /// 每次删除前按远端身份刷新端口，避免重试时误删已被复用的本地端口。
    fn refresh_forward_owner_for_cleanup(
        &mut self,
        remote_endpoint: &str,
    ) -> Result<(), RuntimeProbeError> {
        if !self.forward_creation_attempted {
            return Ok(());
        }
        let forwards: Vec<String> = self
            .bridge
            .forward_list_with_timeout(Some(PROCESS_PROBE_COMMAND_TIMEOUT))?;
        self.forward_port = find_owned_forward_port(&forwards, remote_endpoint)?;
        Ok(())
    }

    /// 驻留时仅释放宿主连接；严格卸载失败且原进程仍在时执行停止与重启。
    pub(super) fn cleanup(&mut self) -> Result<CleanupResult, RuntimeProbeError> {
        #[cfg(target_os = "windows")]
        if self.resident_preserved {
            return self.detach_resident();
        }
        if let Some(cleanup) = self.cleanup_result.as_ref() {
            return Ok(cleanup.clone());
        }
        let mut errors: Vec<String> = Vec::new();
        let mut device_unavailable = false;
        let remote_endpoint: String = self.remote_endpoint.clone();
        let forward_owner_verified: bool =
            match self.refresh_forward_owner_for_cleanup(&remote_endpoint) {
                Ok(()) => true,
                Err(error) => {
                    record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                    false
                }
            };
        let mut forward_remove_error: Option<String> = None;
        if !device_unavailable
            && forward_owner_verified
            && let Some(port) = self.forward_port
        {
            match self.bridge.adb_checked_with_timeout(
                &[
                    "forward".to_owned(),
                    "--remove".to_owned(),
                    format!("tcp:{port}"),
                ],
                "cleanup.remove_forward",
                Some(PROCESS_PROBE_COMMAND_TIMEOUT),
            ) {
                Ok(_) => {}
                Err(error) => {
                    if cleanup_device_unreachable(&error) {
                        device_unavailable = true;
                    }
                    forward_remove_error = Some(error.to_string());
                }
            }
        }

        for (command, stage) in [
            (
                format!(
                    "rm -f {} {} {} {}",
                    self.stage_loader_path,
                    self.stage_agent_path,
                    self.stage_bootstrap_path,
                    self.stage_unload_path
                ),
                "cleanup.remove_staging",
            ),
            (
                format!("rm -rf {}", self.device_session_dir),
                "cleanup.remove_session",
            ),
        ] {
            if device_unavailable {
                break;
            }
            if let Err(error) =
                self.bridge
                    .root_checked_timed(&command, stage, PROCESS_PROBE_COMMAND_TIMEOUT)
            {
                record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
            }
        }
        if let Err(error) = self
            .tool_root
            .remove_directory_if_exists(&self.host_session_relative)
        {
            errors.push(error.to_string());
        }

        let mut old_process_stopped: bool = false;
        let mut game_restarted: bool = false;
        let mut after_pid: u32 = 0;
        let mut expected_final_identity: Option<ProcessIdentity> = None;
        let preserve_original_process: bool = matches!(
            self.target_recovery_state,
            TargetRecoveryState::PreserveOriginal
        );
        if let TargetRecoveryState::Restarted(identity) = self.target_recovery_state {
            old_process_stopped = true;
            game_restarted = true;
            after_pid = identity.process_id;
            expected_final_identity = Some(identity);
        } else if !device_unavailable
            && self.target_recovery_state == TargetRecoveryState::RestartStarted
        {
            old_process_stopped = true;
            if self.process_wait_exhausted {
                errors.push("等待重启后的游戏进程已超时，跳过重复等待".to_owned());
            } else {
                let package_name: String = self.profile.bootstrap().package_name().to_owned();
                match self
                    .bridge
                    .wait_for_single_process_identity(&package_name, self.options.startup_timeout)
                {
                    Ok(identity) => {
                        self.target_recovery_state = TargetRecoveryState::Restarted(identity);
                        game_restarted = true;
                        after_pid = identity.process_id;
                        expected_final_identity = Some(identity);
                    }
                    Err(error) => {
                        self.process_wait_exhausted |= matches!(
                            error,
                            RuntimeProbeError::InvalidOutput {
                                stage: "cleanup.wait_game_identity",
                                ..
                            }
                        );
                        record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                    }
                }
            }
        } else if self.target_recovery_state == TargetRecoveryState::OriginalTerminated {
            old_process_stopped = true;
        } else if !device_unavailable
            && self.target_recovery_state == TargetRecoveryState::RestartRequired
        {
            let presence = match self.bridge.process_instance_absent(
                self.target_pid,
                self.expected_process_start_time,
                PROCESS_PROBE_COMMAND_TIMEOUT,
            ) {
                Ok(true) => OriginalProcessPresence::Absent,
                Ok(false) => OriginalProcessPresence::Present,
                Err(error) => {
                    record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                    OriginalProcessPresence::Unconfirmed
                }
            };
            match process_recovery_action(self.process_wait_exhausted, presence) {
                ProcessRecoveryAction::PreserveStoppedProcess => {
                    old_process_stopped = true;
                    self.target_recovery_state = TargetRecoveryState::OriginalTerminated;
                }
                ProcessRecoveryAction::SkipProcessWait => {
                    errors.push("游戏进程状态未确认，跳过重启等待".to_owned());
                }
                ProcessRecoveryAction::RestartGame => {
                    let package_name: String = self.profile.bootstrap().package_name().to_owned();
                    if let Err(error) = self.bridge.root_checked_timed(
                        &format!("am force-stop {package_name}"),
                        "cleanup.force_stop_game",
                        PROCESS_PROBE_COMMAND_TIMEOUT,
                    ) {
                        after_pid = self.target_pid;
                        record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                    } else {
                        match self.bridge.wait_for_process_instance_absent(
                            self.target_pid,
                            self.expected_process_start_time,
                            self.options.startup_timeout,
                        ) {
                            Ok(stopped) => {
                                old_process_stopped = stopped;
                                self.process_wait_exhausted |= !stopped;
                            }
                            Err(error) => record_cleanup_device_error(
                                &mut errors,
                                &mut device_unavailable,
                                error,
                            ),
                        }
                        if !old_process_stopped {
                            after_pid = self.target_pid;
                        }
                    }
                    match restart_after_process_stop(old_process_stopped, self.target_pid, || {
                        let component = self.launch_component.as_ref().ok_or_else(|| {
                            RuntimeProbeError::Cleanup {
                                messages: "缺少清理后重启游戏所需的已验证启动组件".to_owned(),
                            }
                        })?;
                        self.bridge.root_checked_timed(
                            &format!("am start -n {component}"),
                            "cleanup.restart_game",
                            PROCESS_PROBE_COMMAND_TIMEOUT,
                        )?;
                        self.target_recovery_state = TargetRecoveryState::RestartStarted;
                        self.bridge.wait_for_single_process_identity(
                            &package_name,
                            self.options.startup_timeout,
                        )
                    }) {
                        Ok(identity) => {
                            self.target_recovery_state = TargetRecoveryState::Restarted(identity);
                            after_pid = identity.process_id;
                            game_restarted = true;
                            expected_final_identity = Some(identity);
                        }
                        Err(error) => {
                            self.process_wait_exhausted |= matches!(
                                error,
                                RuntimeProbeError::InvalidOutput {
                                    stage: "cleanup.wait_game_identity",
                                    ..
                                }
                            );
                            record_cleanup_device_error(
                                &mut errors,
                                &mut device_unavailable,
                                error,
                            );
                        }
                    }
                }
            }
        } else if !device_unavailable {
            let package_name: &str = self.profile.bootstrap().package_name();
            match self.bridge.root_status_with_timeout(
                &format!("pidof {package_name}"),
                "cleanup.discover_game_pid",
                PROCESS_PROBE_COMMAND_TIMEOUT,
            ) {
                Ok(output) if output.exit_code == 0 => {
                    match parse_single_pid(&output.stdout, "cleanup.discover_game_pid") {
                        Ok(process_id) => after_pid = process_id,
                        Err(error) => errors.push(error.to_string()),
                    }
                }
                Ok(output) if output.exit_code == 1 && output.stdout.is_empty() => {}
                Ok(output) => errors.push(
                    RuntimeProbeError::DeviceCommand {
                        stage: "cleanup.discover_game_pid",
                        exit_code: output.exit_code,
                        output: output.stdout,
                    }
                    .to_string(),
                ),
                Err(error) => {
                    record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                }
            }
        }

        let forward_list_after: Option<Vec<String>> = if device_unavailable {
            None
        } else {
            match self
                .bridge
                .forward_list_with_timeout(Some(PROCESS_PROBE_COMMAND_TIMEOUT))
            {
                Ok(value) => Some(value),
                Err(error) => {
                    record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                    None
                }
            }
        };
        let (remaining_forward_port, forward_removed): (Option<u16>, bool) =
            reconcile_owned_forward(
                self.forward_port,
                self.forward_creation_attempted,
                forward_list_after.as_deref(),
                &remote_endpoint,
            );
        self.forward_port = remaining_forward_port;
        if self.forward_port.is_some()
            && let Some(error) = forward_remove_error.take()
        {
            errors.push(error);
        }
        if let Some(port) = self.forward_port {
            errors.push(format!(
                "ADB forward tcp:{port} 未确认删除，保留所有权供下次清理"
            ));
        }
        let forward_list_restored: bool = match (&self.forwards_before, &forward_list_after) {
            (Some(before), Some(after)) => before == after,
            (None, _) => true,
            (Some(_), None) => false,
        };
        if !forward_list_restored {
            errors.push(format!(
                "ADB forward 列表未恢复，before={:?}, after={forward_list_after:?}",
                self.forwards_before
            ));
        }

        let mut device_session_removed: bool = true;
        if device_unavailable {
            device_session_removed = false;
        } else {
            for path in [
                &self.device_session_dir,
                &self.stage_loader_path,
                &self.stage_agent_path,
                &self.stage_bootstrap_path,
                &self.stage_unload_path,
            ] {
                match self.bridge.root_status_with_timeout(
                    &format!("test ! -e {path}"),
                    "cleanup.verify_device_file",
                    PROCESS_PROBE_COMMAND_TIMEOUT,
                ) {
                    Ok(output) if output.exit_code == 0 => {}
                    Ok(_) => {
                        device_session_removed = false;
                        errors.push(format!("设备路径仍然存在: {path}"));
                    }
                    Err(error) => {
                        device_session_removed = false;
                        record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                        break;
                    }
                }
            }
        }
        if !device_session_removed {
            errors.push("当前会话设备文件仍然存在".to_owned());
        }
        let host_session_removed: bool = !self.host_session_dir.exists();
        if !host_session_removed {
            errors.push(format!(
                "当前会话宿主临时目录仍然存在: {}",
                self.host_session_dir.display()
            ));
        }

        let collected = if after_pid > 0 && !device_unavailable {
            self.bridge.collect_process_evidence_with_timeout(
                after_pid,
                Some(PROCESS_PROBE_COMMAND_TIMEOUT),
            )
        } else {
            Ok(empty_process_evidence(if device_unavailable {
                0
            } else {
                after_pid
            }))
        };
        let collected = if preserve_original_process && self.target_pid > 0 && !device_unavailable {
            reconcile_preserved_process_evidence(
                &mut self.target_recovery_state,
                collected,
                self.target_pid,
                self.expected_process_start_time,
                || {
                    self.bridge.process_instance_absent(
                        self.target_pid,
                        self.expected_process_start_time,
                        PROCESS_PROBE_COMMAND_TIMEOUT,
                    )
                },
            )
        } else {
            collected
        };
        let evidence = match collected {
            Ok(evidence) => {
                old_process_stopped |=
                    self.target_recovery_state == TargetRecoveryState::OriginalTerminated;
                evidence
            }
            Err(error) => {
                record_cleanup_device_error(&mut errors, &mut device_unavailable, error);
                empty_process_evidence(after_pid)
            }
        };
        if let Some(identity) = expected_final_identity
            && let Err(error) = validate_preserved_process_identity(
                &evidence,
                identity.process_id,
                Some(identity.process_start_time),
                "cleanup.verify_restarted_process_identity",
            )
        {
            errors.push(error.to_string());
        }
        if !evidence.azlw_map_lines.is_empty()
            || !evidence.azlw_thread_names.is_empty()
            || !evidence.azlw_socket_lines.is_empty()
            || !evidence.azlw_process_lines.is_empty()
        {
            errors.push("清理后仍检测到 azlw 映射、线程、socket 或进程".to_owned());
        }
        if evidence.tracer_pid != 0 {
            errors.push(format!(
                "清理后游戏 TracerPid={}，期望为 0",
                evidence.tracer_pid
            ));
        }

        if !errors.is_empty() {
            let messages: String = errors.join(" | ");
            let summary: String = journal_error_summary(&RuntimeProbeError::Cleanup {
                messages: messages.clone(),
            });
            if let Err(error) =
                self.journal
                    .record("cleanup.failure", "error", json!({"message": summary}))
            {
                errors.push(error.to_string());
            }
            return Err(RuntimeProbeError::Cleanup {
                messages: errors.join(" | "),
            });
        }

        #[cfg(target_os = "windows")]
        self.remove_resident_record()?;
        let cleanup: CleanupEvidence = CleanupEvidence {
            forward_removed,
            forward_list_restored,
            device_session_removed,
            host_session_removed,
            old_process_stopped,
            game_restarted,
        };
        let result = CleanupResult { evidence, cleanup };
        let journal: &mut ProbeJournal = &mut self.journal;
        record_cleanup_completion(&mut self.cleanup_result, result, |completed| {
            journal.record("cleanup.complete", "ok", cleanup_journal_details(completed))
        })
    }
}

impl Drop for ProductionSession {
    /// 在调用方提前返回时执行兜底清理，避免遗留当前会话资源。
    fn drop(&mut self) {
        if self.cleanup_result.is_none()
            && let Err(error) = self.cleanup()
        {
            eprintln!("只读运行态兜底清理失败: {error}");
        }
    }
}

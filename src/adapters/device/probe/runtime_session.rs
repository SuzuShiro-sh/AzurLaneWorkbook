//! 持有认证运行态连接，并统一处理连续读取、装备命令与幂等关闭。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use super::super::capture::full_state::FullStateCaptureEvidence;
use super::super::capture::ship_catalog::{
    ShipCatalogCaptureEvidence, ShipCatalogCaptureRequest, write_ship_catalog_capture,
};
use super::super::reading::equipment::EquipmentReadResult;
use super::super::reading::game_state::{GameReadOptions, read_game_state_with_runtime_evidence};
use super::super::reading::ship_catalog::read_ship_catalog;
use super::super::runtime::{
    AgentClient, CapabilitiesResult, EquipmentCommandReceipt, RuntimeClientError,
    RuntimeEquipmentCommand,
};
use super::super::session::SessionId;
use super::process_evidence::ProcessEvidence;
use super::readiness::{
    bag_probe_retries_for_session_read, wait_for_full_state_readiness, wait_for_main_thread,
};
use super::{
    AuthenticatedAgent, CleanupEvidence, CleanupResult, ProductionSession, RuntimeProbeError,
    RuntimeProbeOptions, authenticated_session_start_failed, journal_error_summary,
    probe_failed_after_cleanup, session_start_failed,
};
use crate::domain::GameState;

mod query;

/// 持久运行态会话显式关闭后的读取次数和资源清理证据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSessionShutdownEvidence {
    pub(crate) session_id: SessionId,
    pub(crate) read_count: u64,
    pub(crate) method: RuntimeShutdownMethodEvidence,
    pub(crate) process: ProcessEvidence,
    pub(crate) cleanup: CleanupEvidence,
    pub(crate) journal_path: PathBuf,
}

/// 区分代理驻留、正常卸载和安全卸载失败后的重启兜底。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeShutdownMethodEvidence {
    Retained {
        journal_errors: Vec<String>,
    },
    Graceful {
        unload: GracefulUnloadEvidence,
        journal_errors: Vec<String>,
    },
    RestartFallback {
        unload_error: String,
        journal_errors: Vec<String>,
    },
}

/// Agent 在保留原游戏进程的前提下完成排空和卸载的严格证据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GracefulUnloadEvidence {
    pub(crate) process_id: u32,
    pub(crate) process_start_time: u64,
    pub(crate) worker_tid: u32,
    pub(crate) worker_start_time: u64,
    pub(crate) agent_mapping_name: String,
    pub(crate) inert_anonymous_overlap_count: usize,
    pub(crate) reused_address_overlap_count: usize,
}

/// 持有唯一认证连接及其设备资源，允许在最终清理前连续读取完整状态。
pub(crate) struct RuntimeSession {
    state: RuntimeSessionState,
    resources: ProductionSession,
    read_count: u64,
    catalog_generation: u64,
    readiness_confirmed: bool,
    equipment_catalog: Option<EquipmentReadResult>,
    ship_catalog: Option<crate::adapters::device::reading::ship_catalog::ShipCatalogReadResult>,
    last_capabilities: Option<CapabilitiesResult>,
    last_full_state_capture: Option<FullStateCaptureEvidence>,
    journal_errors: Vec<String>,
}

/// 持久会话只在资源清理未完成时保留重试所有权。
enum RuntimeSessionState {
    Active(Box<AuthenticatedAgent>),
    CleanupPending(RuntimeShutdownMethodEvidence),
    Completed(Box<RuntimeSessionShutdownEvidence>),
    Closing,
}

/// 清理失败时把关闭方式原样归还，调用方据此保留唯一重试所有权。
pub(super) fn attempt_runtime_session_cleanup<T>(
    method: RuntimeShutdownMethodEvidence,
    cleanup: impl FnOnce() -> Result<T, RuntimeProbeError>,
) -> Result<
    (RuntimeShutdownMethodEvidence, T),
    (RuntimeShutdownMethodEvidence, Box<RuntimeProbeError>),
> {
    match cleanup() {
        Ok(result) => Ok((method, result)),
        Err(error) => Err((method, Box::new(error))),
    }
}

/// 把命令阶段和卸载阶段的日志错误并入最终关闭方式，清理重试时继续原样保留。
pub(super) fn merge_runtime_session_journal_errors(
    method: &mut RuntimeShutdownMethodEvidence,
    pending_errors: &mut Vec<String>,
) {
    if pending_errors.is_empty() {
        return;
    }
    let method_errors: &mut Vec<String> = match method {
        RuntimeShutdownMethodEvidence::Retained { journal_errors }
        | RuntimeShutdownMethodEvidence::Graceful { journal_errors, .. }
        | RuntimeShutdownMethodEvidence::RestartFallback { journal_errors, .. } => journal_errors,
    };
    let mut collected_errors: Vec<String> = std::mem::take(pending_errors);
    collected_errors.append(method_errors);
    *method_errors = collected_errors;
}

impl RuntimeSession {
    /// 建立可复用连接；握手后启动失败先安全卸载，握手前失败保留重启兜底。
    pub(crate) fn open(
        options: RuntimeProbeOptions,
        related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Self, RuntimeProbeError> {
        let mut runner: ProductionSession = ProductionSession::new(options, related)?;
        let journal_path: PathBuf = runner.journal.path.clone();
        if let Err(source) = runner.journal.record(
            "session.start",
            "ok",
            json!({
                "message": "开始持久运行态会话",
                "host_session_id": runner.host_session_id,
                "session_id": runner.session_id,
            }),
        ) {
            return Err(probe_failed_after_cleanup(source, journal_path, || {
                runner.cleanup()
            }));
        }

        let mut authenticated: AuthenticatedAgent = match runner.open_authenticated_agent() {
            Ok(authenticated) => authenticated,
            Err(source) => return Err(session_start_failed(&mut runner, source, journal_path)),
        };
        let health = match wait_for_main_thread(&mut authenticated.client) {
            Ok(health) => health,
            Err(source) => {
                if runner.resident_preserved {
                    drop(authenticated);
                    return Err(session_start_failed(&mut runner, source, journal_path));
                }
                return Err(authenticated_session_start_failed(
                    &mut runner,
                    authenticated,
                    source,
                    journal_path,
                ));
            }
        };
        if let Err(source) = runner.journal.record(
            "session.ready",
            "ok",
            json!({
                "session_id": runner.session_id,
                "process_id": runner.target_pid,
                "forward_port": authenticated.forward_port,
                "handshake_attempts": authenticated.handshake_attempts,
                "health": health,
            }),
        ) {
            if runner.resident_preserved {
                drop(authenticated);
                return Err(session_start_failed(&mut runner, source, journal_path));
            }
            return Err(authenticated_session_start_failed(
                &mut runner,
                authenticated,
                source,
                journal_path,
            ));
        }

        authenticated
            .client
            .enable_catalog_cache(runner.tool_root.clone(), health.catalog_generation);
        Ok(Self {
            state: RuntimeSessionState::Active(Box::new(authenticated)),
            resources: runner,
            read_count: 0,
            catalog_generation: health.catalog_generation,
            readiness_confirmed: false,
            equipment_catalog: None,
            ship_catalog: None,
            last_capabilities: None,
            last_full_state_capture: None,
            journal_errors: Vec::new(),
        })
    }

    /// 返回当前认证会话锁定的进程身份，供真机回环在任何写入前建立严格环境锁。
    #[cfg(test)]
    pub(in crate::adapters::device) fn target_process_identity(
        &self,
    ) -> Result<(u32, u64), RuntimeProbeError> {
        let process_start_time: u64 =
            self.resources.expected_process_start_time.ok_or_else(|| {
                RuntimeProbeError::InvalidOutput {
                    stage: "session.target_process_identity",
                    message: "当前认证会话缺少目标进程启动时刻".to_owned(),
                }
            })?;
        Ok((self.resources.target_pid, process_start_time))
    }

    /// 返回最近一次完整读取验证过的能力报告，供同一会话的写入预检复用。
    pub(crate) fn last_capabilities(&self) -> Result<CapabilitiesResult, RuntimeProbeError> {
        self.last_capabilities
            .clone()
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "session.capabilities",
                message: "当前会话还没有通过完整读取验证的能力报告".to_owned(),
            })
    }

    /// 返回最近一次成功完整读取同时发布的捕获身份。
    pub(crate) fn last_full_state_capture(&self) -> Option<FullStateCaptureEvidence> {
        self.last_full_state_capture.clone()
    }

    /// 在同一认证连接上读取一次完整状态，失败时不尝试重新握手或重载 agent。
    pub(crate) fn read_full_state_with_progress(
        &mut self,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameState, RuntimeProbeError> {
        self.read_state_with_scope(crate::domain::GameReadScope::full(), progress)
    }

    pub(crate) fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameState, RuntimeProbeError> {
        self.refresh_catalog_generation()?;
        let read_index: Option<u64> = self.read_count.checked_add(1);
        let cached_equipment = self.equipment_catalog.take();
        let equipment_catalog_source = if cached_equipment.is_some() {
            "session_cache"
        } else {
            "runtime"
        };
        let cached_ship_catalog = self.ship_catalog.take();
        let ship_catalog_source = if cached_ship_catalog.is_some() {
            "session_cache"
        } else {
            "runtime"
        };
        let result: Result<
            (
                GameState,
                EquipmentReadResult,
                crate::adapters::device::reading::ship_catalog::ShipCatalogReadResult,
                CapabilitiesResult,
                Option<FullStateCaptureEvidence>,
            ),
            RuntimeProbeError,
        > = (|| {
            let read_index: u64 = read_index.ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "session.read_count",
                message: "持久会话读取次数超过 u64".to_owned(),
            })?;
            self.resources.journal.record(
                "session.read.start",
                "ok",
                json!({"read_index": read_index}),
            )?;
            let options: GameReadOptions = self
                .resources
                .game_read_options(read_index)?
                .with_read_scope(scope);
            let request_timeout_ms: u32 = self.resources.options.timeout_ms;
            let max_items: u32 = self.resources.options.max_items;
            let expected_module_sha256: String = self
                .resources
                .profile
                .bootstrap()
                .module_sha256()
                .to_owned();
            let connection: &mut AuthenticatedAgent = match &mut self.state {
                RuntimeSessionState::Active(connection) => connection.as_mut(),
                RuntimeSessionState::CleanupPending(_)
                | RuntimeSessionState::Completed(_)
                | RuntimeSessionState::Closing => {
                    return Err(RuntimeProbeError::InvalidOutput {
                        stage: "session.read",
                        message: "持久会话已经开始关闭，不能继续读取".to_owned(),
                    });
                }
            };
            progress(crate::application::OperationProgress::stage(
                "正在等待游戏主线程与背包数据就绪",
            ));
            wait_for_main_thread(&mut connection.client)?;
            let bag_retries =
                bag_probe_retries_for_session_read(u64::from(self.readiness_confirmed), || {
                    let sample = wait_for_full_state_readiness(
                        &mut connection.client,
                        request_timeout_ms,
                        max_items,
                        &expected_module_sha256,
                    )?;
                    Ok((sample.bag, sample.bag_retries))
                })?;
            let (state, equipment, ship_catalog, capabilities, capture, account_retries) =
                read_game_state_with_runtime_evidence(
                    &mut connection.client,
                    &options,
                    cached_equipment,
                    cached_ship_catalog,
                    progress,
                )?;
            let ship_catalog = ship_catalog.static_tables_only().map_err(|source| {
                RuntimeProbeError::InvalidOutput {
                    stage: "session.ship_catalog_cache",
                    message: source.to_string(),
                }
            })?;
            let final_health = connection.client.health(request_timeout_ms)?;
            if final_health.catalog_generation != self.catalog_generation {
                return Err(RuntimeProbeError::InvalidOutput {
                    stage: "session.catalog_generation",
                    message: "采集期间 Lua 运行态发生变化，需要重新读取".to_owned(),
                });
            }
            let (cache_hits, cache_misses) = connection.client.catalog_cache_counts();
            let owned_state_retries = account_retries.owned_before;
            let ship_details_retries = account_retries.ship_details;
            let mut read_evidence = json!({
                "read_index": read_index,
                "game_state_content_sha256": state.source().content_sha256(),
                "equipment_catalog_source": equipment_catalog_source,
                "ship_catalog_source": ship_catalog_source,
                "read_scope": state.source().read_scope(),
                "catalog_cache_hits": cache_hits,
                "catalog_cache_misses": cache_misses,
                "bag_readiness_retries": bag_retries,
                "owned_state_readiness_retries": owned_state_retries,
                "ship_details_readiness_retries": ship_details_retries,
            });
            if let Some(capture) = &capture {
                read_evidence
                    .as_object_mut()
                    .expect("读取证据固定为 JSON 对象")
                    .insert(
                        "full_state_capture".to_owned(),
                        json!({
                            "schema_version": capture.schema_version(),
                            "path": capture.path(),
                            "size_bytes": capture.size_bytes(),
                            "sha256": capture.sha256(),
                            "session_id": capture.session_id(),
                            "read_index": capture.read_index(),
                        }),
                    );
            }
            self.resources
                .journal
                .record("session.read.complete", "ok", read_evidence)?;
            Ok((state, equipment, ship_catalog, capabilities, capture))
        })();

        match result {
            Ok((state, equipment, ship_catalog, capabilities, capture)) => {
                self.read_count = read_index.expect("成功读取必须具有有效读取序号");
                self.readiness_confirmed = true;
                self.equipment_catalog = Some(equipment);
                self.ship_catalog = Some(ship_catalog);
                self.last_capabilities = Some(capabilities);
                self.last_full_state_capture = capture;
                Ok(state)
            }
            Err(source) => {
                self.equipment_catalog = None;
                self.ship_catalog = None;
                self.last_capabilities = None;
                self.last_full_state_capture = None;
                let message: String = journal_error_summary(&source);
                if let Err(journal_error) = self.resources.journal.record(
                    "session.read.failure",
                    "error",
                    json!({"read_index": read_index, "message": message}),
                ) {
                    eprintln!("持久运行态读取失败日志写入失败: {journal_error}");
                }
                Err(RuntimeProbeError::ProbeFailed {
                    source: Box::new(source),
                    journal_path: self.resources.journal.path.clone(),
                    cleanup_error: None,
                })
            }
        }
    }

    /// 在当前认证连接上读取固定白名单静态目录，并只向显式外部目录发布原始正文。
    pub(crate) fn capture_ship_catalog(
        &mut self,
        capture_root: &Path,
    ) -> Result<ShipCatalogCaptureEvidence, RuntimeProbeError> {
        let request = ShipCatalogCaptureRequest::new(
            self.resources.tool_root.as_path(),
            capture_root,
            self.resources.session_id,
        )?;
        let result: Result<ShipCatalogCaptureEvidence, RuntimeProbeError> = (|| {
            self.resources.journal.record(
                "session.ship_catalog.start",
                "ok",
                json!({"session_id": self.resources.session_id}),
            )?;
            let timeout_ms = self.resources.options.timeout_ms;
            let expected_module_sha256 = self.resources.profile.bootstrap().module_sha256();
            let connection: &mut AuthenticatedAgent = match &mut self.state {
                RuntimeSessionState::Active(connection) => connection.as_mut(),
                RuntimeSessionState::CleanupPending(_)
                | RuntimeSessionState::Completed(_)
                | RuntimeSessionState::Closing => {
                    return Err(RuntimeProbeError::InvalidOutput {
                        stage: "session.ship_catalog",
                        message: "持久会话已经开始关闭，不能读取舰船静态目录".to_owned(),
                    });
                }
            };
            wait_for_main_thread(&mut connection.client)?;
            let catalog =
                read_ship_catalog(&mut connection.client, timeout_ms, expected_module_sha256)?;
            let evidence = write_ship_catalog_capture(&request, &catalog)?;
            self.resources.journal.record(
                "session.ship_catalog.complete",
                "ok",
                json!({
                    "schema_version": evidence.schema_version,
                    "path": &evidence.path,
                    "size_bytes": evidence.size_bytes,
                    "file_sha256": &evidence.file_sha256,
                    "module_sha256": &evidence.module_sha256,
                    "content_sha256": &evidence.content_sha256,
                    "table_count": evidence.table_count,
                    "record_count": evidence.record_count,
                }),
            )?;
            Ok(evidence)
        })();

        match result {
            Ok(evidence) => Ok(evidence),
            Err(source) => {
                let message = journal_error_summary(&source);
                if let Err(journal_error) = self.resources.journal.record(
                    "session.ship_catalog.failure",
                    "error",
                    json!({"message": message}),
                ) {
                    eprintln!("舰船静态目录读取失败日志写入失败: {journal_error}");
                    self.journal_errors.push(journal_error.to_string());
                }
                Err(RuntimeProbeError::ProbeFailed {
                    source: Box::new(source),
                    journal_path: self.resources.journal.path.clone(),
                    cleanup_error: None,
                })
            }
        }
    }

    /// 在当前认证连接上最多派发一次装备命令，不创建第二条会话或隐式重试。
    pub(crate) fn execute_equipment_command(
        &mut self,
        command: &RuntimeEquipmentCommand,
    ) -> Result<EquipmentCommandReceipt, RuntimeProbeError> {
        self.run_equipment_command_request(
            "execute",
            command.command_id(),
            None,
            |client, timeout_ms, _budget| client.execute_equipment_command(timeout_ms, command),
        )
    }

    /// 查询当前会话账本中的原装备命令，不重新派发写入。
    pub(crate) fn query_equipment_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<EquipmentCommandReceipt, RuntimeProbeError> {
        self.run_equipment_command_request(
            "query",
            command_id,
            Some(budget),
            |client, timeout_ms, budget| {
                client.query_equipment_command(timeout_ms, command_id, budget)
            },
        )
    }

    /// 请求停止观察原装备命令，并原样返回设备端能够确认的状态。
    pub(crate) fn cancel_equipment_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<EquipmentCommandReceipt, RuntimeProbeError> {
        self.run_equipment_command_request(
            "cancel",
            command_id,
            Some(budget),
            |client, timeout_ms, budget| {
                client.cancel_equipment_command(timeout_ms, command_id, budget)
            },
        )
    }

    /// 统一执行单连接装备 RPC，并保证成功收据不因后置日志失败而丢失。
    fn run_equipment_command_request<F>(
        &mut self,
        operation: &'static str,
        command_id: &str,
        budget: Option<Duration>,
        request: F,
    ) -> Result<EquipmentCommandReceipt, RuntimeProbeError>
    where
        F: FnOnce(
            &mut AgentClient,
            u32,
            Duration,
        ) -> Result<EquipmentCommandReceipt, RuntimeClientError>,
    {
        let result: Result<EquipmentCommandReceipt, RuntimeProbeError> = (|| {
            self.resources.journal.record(
                "session.equipment_command.start",
                "ok",
                json!({
                    "operation": operation,
                    "command_id": command_id,
                }),
            )?;
            let configured_timeout_ms: u32 = self.resources.options.timeout_ms;
            let (timeout_ms, socket_budget) = match budget {
                Some(budget) if budget.is_zero() => {
                    return Err(RuntimeProbeError::InvalidOutput {
                        stage: "session.equipment_command",
                        message: "剩余观察预算为 0，未发送查询或取消".to_owned(),
                    });
                }
                Some(budget) => {
                    let budget_ms = u32::try_from(budget.as_millis()).unwrap_or(u32::MAX);
                    let timeout_ms = configured_timeout_ms.min(budget_ms).clamp(1, 30_000);
                    (timeout_ms, budget)
                }
                None => (
                    configured_timeout_ms,
                    Duration::from_millis(u64::from(configured_timeout_ms)),
                ),
            };
            let connection: &mut AuthenticatedAgent = match &mut self.state {
                RuntimeSessionState::Active(connection) => connection.as_mut(),
                RuntimeSessionState::CleanupPending(_)
                | RuntimeSessionState::Completed(_)
                | RuntimeSessionState::Closing => {
                    return Err(RuntimeProbeError::InvalidOutput {
                        stage: "session.equipment_command",
                        message: "持久会话已经开始关闭，不能继续发送装备命令".to_owned(),
                    });
                }
            };
            let receipt: EquipmentCommandReceipt =
                request(&mut connection.client, timeout_ms, socket_budget)?;
            if let Err(error) = self.resources.journal.record(
                "session.equipment_command.complete",
                "ok",
                json!({
                    "operation": operation,
                    "command_id": receipt.command_id.as_str(),
                    "status": receipt.status,
                    "phase": receipt.phase,
                    "write_dispatched": receipt.write_dispatched,
                    "cancel_requested": receipt.cancel_requested,
                    "observation_count": receipt.observation_count,
                    "error_code": receipt.error_code.as_deref(),
                }),
            ) {
                eprintln!("装备命令成功收据日志写入失败: {error}");
                self.journal_errors.push(error.to_string());
            }
            Ok(receipt)
        })();

        match result {
            Ok(receipt) => Ok(receipt),
            Err(source) => {
                let message: String = journal_error_summary(&source);
                if let Err(journal_error) = self.resources.journal.record(
                    "session.equipment_command.failure",
                    "error",
                    json!({
                        "operation": operation,
                        "command_id": command_id,
                        "message": message,
                    }),
                ) {
                    eprintln!("装备命令失败日志写入失败: {journal_error}");
                    self.journal_errors.push(journal_error.to_string());
                }
                Err(RuntimeProbeError::ProbeFailed {
                    source: Box::new(source),
                    journal_path: self.resources.journal.path.clone(),
                    cleanup_error: None,
                })
            }
        }
    }

    /// 按会话策略保留代理或严格卸载，清理失败时保留唯一重试所有权。
    pub(crate) fn shutdown(&mut self) -> Result<RuntimeSessionShutdownEvidence, RuntimeProbeError> {
        let journal_path: PathBuf = self.resources.journal.path.clone();
        let state: RuntimeSessionState =
            std::mem::replace(&mut self.state, RuntimeSessionState::Closing);
        let (mut method, unload_failure): (
            RuntimeShutdownMethodEvidence,
            Option<RuntimeProbeError>,
        ) = match state {
            RuntimeSessionState::Active(connection) if self.resources.options.retain_agent => {
                drop(connection);
                (
                    RuntimeShutdownMethodEvidence::Retained {
                        journal_errors: Vec::new(),
                    },
                    None,
                )
            }
            RuntimeSessionState::Active(connection) => {
                self.resources.resident_preserved = false;
                match self
                    .resources
                    .unload_agent_gracefully(*connection, &mut self.journal_errors)
                {
                    Ok(unload) => (
                        RuntimeShutdownMethodEvidence::Graceful {
                            unload,
                            journal_errors: Vec::new(),
                        },
                        None,
                    ),
                    Err(source) => {
                        let unload_error: String = journal_error_summary(&source);
                        if let Err(journal_error) = self.resources.journal.record(
                            "session.shutdown.failure",
                            "error",
                            json!({"message": &unload_error}),
                        ) {
                            eprintln!("安全卸载失败日志写入失败: {journal_error}");
                            self.journal_errors.push(journal_error.to_string());
                        }
                        (
                            RuntimeShutdownMethodEvidence::RestartFallback {
                                unload_error,
                                journal_errors: Vec::new(),
                            },
                            Some(source),
                        )
                    }
                }
            }
            RuntimeSessionState::CleanupPending(method) => (method, None),
            RuntimeSessionState::Completed(evidence) => {
                let result: RuntimeSessionShutdownEvidence = evidence.as_ref().clone();
                self.state = RuntimeSessionState::Completed(evidence);
                return Ok(result);
            }
            RuntimeSessionState::Closing => {
                self.state = RuntimeSessionState::Closing;
                return Err(RuntimeProbeError::InvalidOutput {
                    stage: "session.shutdown",
                    message: "持久会话关闭事务正在执行".to_owned(),
                });
            }
        };
        merge_runtime_session_journal_errors(&mut method, &mut self.journal_errors);

        let (method, cleanup): (RuntimeShutdownMethodEvidence, CleanupResult) =
            match attempt_runtime_session_cleanup(method, || self.resources.cleanup()) {
                Ok(result) => result,
                Err((method, cleanup_error)) => {
                    let cleanup_message: String = cleanup_error.to_string();
                    let pending_unload_error: Option<String> = match &method {
                        RuntimeShutdownMethodEvidence::Retained { .. }
                        | RuntimeShutdownMethodEvidence::Graceful { .. } => None,
                        RuntimeShutdownMethodEvidence::RestartFallback { unload_error, .. } => {
                            Some(unload_error.clone())
                        }
                    };
                    self.state = RuntimeSessionState::CleanupPending(method);
                    return Err(match unload_failure {
                        Some(source) => RuntimeProbeError::ProbeFailed {
                            source: Box::new(source),
                            journal_path,
                            cleanup_error: Some(cleanup_error.to_string()),
                        },
                        None => match pending_unload_error {
                            Some(unload_error) => RuntimeProbeError::ProbeFailed {
                                source: Box::new(RuntimeProbeError::Cleanup {
                                    messages: format!("安全卸载失败: {unload_error}"),
                                }),
                                journal_path,
                                cleanup_error: Some(cleanup_message),
                            },
                            None => RuntimeProbeError::ProbeFailed {
                                source: cleanup_error,
                                journal_path,
                                cleanup_error: None,
                            },
                        },
                    });
                }
            };
        let evidence = RuntimeSessionShutdownEvidence {
            session_id: self.resources.session_id,
            read_count: self.read_count,
            method,
            process: cleanup.evidence,
            cleanup: cleanup.cleanup,
            journal_path: self.resources.journal.path.clone(),
        };
        self.state = RuntimeSessionState::Completed(Box::new(evidence.clone()));
        Ok(evidence)
    }
}

impl Drop for RuntimeSession {
    /// 调用方提前释放会话时沿用已选关闭策略，未完成的资源由 ProductionSession 接管。
    fn drop(&mut self) {
        if !matches!(&self.state, RuntimeSessionState::Completed(_))
            && let Err(error) = self.shutdown()
        {
            eprintln!("持久运行态析构时安全关闭失败: {error}");
        }
    }
}

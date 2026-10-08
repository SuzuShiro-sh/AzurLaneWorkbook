//! 编排便携资源会话、运行态探针、收据发布和有序清理。

#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::time::Duration;

#[cfg(target_os = "windows")]
use super::super::capture::full_state::FullStateCaptureEvidence;
#[cfg(target_os = "windows")]
use super::super::capture::ship_catalog::ShipCatalogCaptureEvidence;
#[cfg(any(target_os = "windows", test))]
use super::super::probe::RuntimeProbeError;
#[cfg(target_os = "windows")]
use super::super::probe::RuntimeProbeOutcome;
#[cfg(target_os = "windows")]
use super::super::probe::{
    PROFILE_RELATIVE_PATH, RuntimeProbeOptions, RuntimeSession, RuntimeSessionShutdownEvidence,
    run_runtime_probe,
};
#[cfg(target_os = "windows")]
use super::super::profile::RuntimeProfile;
#[cfg(target_os = "windows")]
use super::super::runtime::{CapabilitiesResult, EquipmentCommandReceipt, RuntimeEquipmentCommand};
#[cfg(target_os = "windows")]
use super::adb::{finish_portable_operation, load_and_inspect_configured_adb_bundle};
#[cfg(target_os = "windows")]
use super::discovery::{ResolvedManager, resolve_manager};
#[cfg(target_os = "windows")]
use super::instances::select_instance;
#[cfg(target_os = "windows")]
use super::startup::{
    launch_manager_instance, verify_target_and_start_game, wait_for_ready_instance,
};
#[cfg(target_os = "windows")]
use super::{
    AdbBundleInspection, MAXIMUM_PORTABLE_RECEIPT_BYTES, ManagerEvidence,
    PORTABLE_PROBE_SCHEMA_VERSION, PORTABLE_SHIP_CATALOG_CAPTURE_SCHEMA_VERSION,
    PortableAdbEvidence, PortableCleanupEvidence, PortableProbeOptions, PortableProbeOutcome,
    PortableProbeReport, PortableRuntimeSession, PortableSessionShutdownEvidence,
    PortableShipCatalogCaptureOutcome, PortableShipCatalogCaptureReport, PreparedPortableSession,
    TargetEvidence, UNREPORTED_ANDROID_VERSION, adb_failure,
};
#[cfg(any(target_os = "windows", test))]
use super::{PortableMode, PortableProbeError};
#[cfg(target_os = "windows")]
use crate::adapters::json_artifact::{PublishedJson, write_new_pretty_json};
#[cfg(target_os = "windows")]
use crate::adapters::tool_root::ToolRoot;
#[cfg(any(target_os = "windows", test))]
use crate::application::AppErrorCode;
#[cfg(target_os = "windows")]
use suzushiro_adb::{AdbBundle, AdbShutdownEvidence, OwnedAdbServer};
#[cfg(target_os = "windows")]
use suzushiro_emulator::EmulatorInstance;
#[cfg(target_os = "windows")]
use suzushiro_emulator::manager_command::windows_path;

/// 把已确认的标题页状态转换为用户可执行的提示，其余运行态错误保持原分类。
#[cfg(any(target_os = "windows", test))]
pub(super) fn map_runtime_error(error: RuntimeProbeError) -> PortableProbeError {
    if is_game_not_ready(&error) {
        PortableProbeError::GameNotReady {
            detail: runtime_journal_hint(&error),
            source: Box::new(error),
        }
    } else {
        PortableProbeError::Runtime(error)
    }
}

/// 兼容早期 BagProxy 等待阶段，并识别完整状态端口的稳定未就绪分类。
#[cfg(any(target_os = "windows", test))]
fn is_game_not_ready(error: &RuntimeProbeError) -> bool {
    match error {
        RuntimeProbeError::InvalidOutput { stage, .. } => matches!(
            *stage,
            "rpc.wait_bag_proxy" | "rpc.wait_owned_state" | "rpc.wait_ship_details"
        ),
        RuntimeProbeError::GameStateRead(source) => source.code() == AppErrorCode::GameNotReady,
        RuntimeProbeError::ProbeFailed { source, .. } => is_game_not_ready(source),
        _ => false,
    }
}

/// 就绪提示只公开受控日志路径，不展开 Lua、原生 RPC 或映射错误正文。
#[cfg(any(target_os = "windows", test))]
fn runtime_journal_hint(error: &RuntimeProbeError) -> String {
    match error {
        RuntimeProbeError::ProbeFailed { journal_path, .. } => {
            format!("请查看 {}", journal_path.display())
        }
        _ => "请查看工具目录内的运行态日志".to_owned(),
    }
}

/// 原生 Windows 上执行完整便携流程，并保证 ADB 清理覆盖所有返回路径。
#[cfg(target_os = "windows")]
pub(super) fn run_portable_probe_windows(
    options: PortableProbeOptions,
) -> Result<PortableProbeOutcome, PortableProbeError> {
    let prepared: PreparedPortableSession = prepare_portable_session(options, true, None)?;
    let mode: PortableMode = prepared.options.mode;
    let tool_root: PathBuf = prepared.options.tool_root.clone();
    let manager: ManagerEvidence = prepared.manager.clone();
    let target: TargetEvidence = prepared.target.clone();
    let adb: PortableAdbEvidence = prepared.adb.clone();
    let runtime_result: Result<RuntimeProbeOutcome, PortableProbeError> =
        run_runtime_probe(prepared.runtime_options.clone()).map_err(map_runtime_error);
    let (runtime, cleanup): (RuntimeProbeOutcome, PortableCleanupEvidence) =
        prepared.finish(runtime_result)?;
    let report: PortableProbeReport = PortableProbeReport {
        schema_version: PORTABLE_PROBE_SCHEMA_VERSION,
        status: "passed",
        mode,
        manager,
        target,
        adb,
        runtime_session_id: runtime.report.session_id.to_string(),
        runtime_snapshot_count: runtime.report.snapshot_count,
        runtime_receipt_path: runtime.receipt_path.clone(),
        runtime_journal_path: runtime.journal_path.clone(),
        equipment_sample: runtime.report.equipment_sample.clone(),
        cleanup,
    };
    let receipt_path: PathBuf = write_portable_receipt(&tool_root, &report)?;
    Ok(PortableProbeOutcome {
        report,
        receipt_path,
        runtime,
    })
}

/// 专用捕获只执行认证、静态目录读取和既有会话关闭，不运行普通探针负向请求。
#[cfg(target_os = "windows")]
pub(super) fn run_portable_ship_catalog_capture_windows(
    options: PortableProbeOptions,
    capture_root: PathBuf,
) -> Result<PortableShipCatalogCaptureOutcome, PortableProbeError> {
    let tool_root = options.tool_root.clone();
    let mode = options.mode;
    let mut session = PortableRuntimeSession::open(options, None)?;
    let prepared = session
        .prepared
        .as_ref()
        .expect("刚建立的便携会话必须保留目标证据");
    let manager = prepared.manager.clone();
    let target = prepared.target.clone();
    let adb = prepared.adb.clone();
    let operation = session.capture_ship_catalog(&capture_root);
    let shutdown = session.shutdown().cloned();
    let (capture, shutdown) = match (operation, shutdown) {
        (Ok(capture), Ok(shutdown)) => (capture, shutdown),
        (Err(operation), Ok(_)) => return Err(operation),
        (Ok(_), Err(cleanup)) => return Err(cleanup),
        (Err(operation), Err(cleanup)) => {
            return Err(PortableProbeError::OperationAndSessionCleanup {
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            });
        }
    };
    let report = PortableShipCatalogCaptureReport {
        schema_version: PORTABLE_SHIP_CATALOG_CAPTURE_SCHEMA_VERSION,
        status: "passed",
        mode,
        manager,
        target,
        adb,
        capture,
        runtime_journal_path: shutdown.runtime.journal_path.clone(),
        process_after_cleanup: shutdown.runtime.process.clone(),
        runtime_cleanup: shutdown.runtime.cleanup.clone(),
        adb_cleanup: shutdown.adb,
    };
    let receipt_path = write_portable_ship_catalog_receipt(&tool_root, &report)?;
    Ok(PortableShipCatalogCaptureOutcome {
        report,
        receipt_path,
    })
}

#[cfg(target_os = "windows")]
impl PreparedPortableSession {
    /// 无论运行操作是否成功都关闭当前对象持有的 ADB，并保留双重失败。
    fn finish<T>(
        mut self,
        operation: Result<T, PortableProbeError>,
    ) -> Result<(T, PortableCleanupEvidence), PortableProbeError> {
        let server: OwnedAdbServer = self
            .server
            .take()
            .expect("尚未关闭的便携会话必须持有独立 ADB 服务");
        finish_portable_operation(server, operation)
    }

    /// 关闭仍由持久会话持有的 ADB；失败时保留所有者供下一次清理重试。
    fn shutdown_adb(&mut self) -> Result<PortableCleanupEvidence, PortableProbeError> {
        let shutdown: AdbShutdownEvidence = self
            .server
            .as_mut()
            .expect("尚未关闭的便携会话必须持有独立 ADB 服务")
            .shutdown()
            .map_err(|source| adb_failure("adb.cleanup", source))?;
        self.server.take();
        Ok(PortableCleanupEvidence {
            adb_process_stopped: shutdown.process_stopped,
            adb_port_released: shutdown.port_released,
            adb_temporary_root_removed: shutdown.temporary_root_removed,
        })
    }
}

#[cfg(target_os = "windows")]
impl PortableRuntimeSession {
    /// 完成便携发现、独立 ADB、目标门禁和 agent 握手，但不执行一次性失败探测。
    pub(crate) fn open(
        options: PortableProbeOptions,
        related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Self, PortableProbeError> {
        let prepared: PreparedPortableSession =
            prepare_portable_session(options, true, related.as_ref())?;
        match RuntimeSession::open(prepared.runtime_options.clone(), related) {
            Ok(runtime) => Ok(Self {
                runtime: Some(runtime),
                runtime_shutdown: None,
                prepared: Some(prepared),
                shutdown_evidence: None,
            }),
            Err(source) => {
                let operation: Result<RuntimeSession, PortableProbeError> =
                    Err(map_runtime_error(source));
                match prepared.finish(operation) {
                    Err(error) => Err(error),
                    Ok(_) => unreachable!("失败的运行态启动不能产生便携成功结果"),
                }
            }
        }
    }

    /// 判断当前连接是否仍对应完全相同的发现和读取配置。
    pub(crate) fn matches_options(&self, options: &PortableProbeOptions) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|prepared| prepared.options == *options)
    }

    /// 返回当前认证会话锁定的设备序列号和游戏包名。
    pub(crate) fn target_scope(&self) -> Result<(String, String), PortableProbeError> {
        let prepared = self.prepared.as_ref().ok_or_else(|| {
            map_runtime_error(RuntimeProbeError::InvalidOutput {
                stage: "session.target_scope",
                message: "便携会话已经开始关闭，不能读取目标范围".to_owned(),
            })
        })?;
        Ok((
            prepared.target.serial.clone(),
            prepared.target.package_name.clone(),
        ))
    }

    /// 返回最近一次完整读取验证过的能力报告。
    pub(crate) fn last_capabilities(&self) -> Result<CapabilitiesResult, PortableProbeError> {
        self.runtime
            .as_ref()
            .ok_or_else(|| {
                map_runtime_error(RuntimeProbeError::InvalidOutput {
                    stage: "session.capabilities",
                    message: "便携会话已经开始关闭，不能读取能力报告".to_owned(),
                })
            })?
            .last_capabilities()
            .map_err(map_runtime_error)
    }

    /// 返回最近一次成功完整读取同时发布的捕获身份。
    pub(crate) fn last_full_state_capture(&self) -> Option<FullStateCaptureEvidence> {
        self.runtime
            .as_ref()
            .and_then(RuntimeSession::last_full_state_capture)
    }

    /// 在唯一认证连接上查询对象，复用已建立的设备资源。
    pub(crate) fn query(
        &mut self,
        query: &crate::application::GameQuery,
    ) -> Result<crate::application::GameQueryReport, PortableProbeError> {
        self.active_runtime("session.query")?
            .query(query)
            .map_err(map_runtime_error)
    }

    /// 在唯一认证连接上读取完整游戏状态，不重新发现实例或启动 ADB。
    pub(crate) fn read_full_state(
        &mut self,
    ) -> Result<crate::domain::GameState, PortableProbeError> {
        self.read_full_state_with_progress(&mut |_| {})
    }

    pub(crate) fn read_full_state_with_progress(
        &mut self,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<crate::domain::GameState, PortableProbeError> {
        self.active_runtime("session.read")?
            .read_full_state_with_progress(progress)
            .map_err(map_runtime_error)
    }

    pub(crate) fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<crate::domain::GameState, PortableProbeError> {
        self.active_runtime("session.read")?
            .read_state_with_scope(scope, progress)
            .map_err(map_runtime_error)
    }

    /// 在当前便携会话的唯一认证连接上捕获固定白名单舰船静态目录。
    pub(crate) fn capture_ship_catalog(
        &mut self,
        capture_root: &Path,
    ) -> Result<ShipCatalogCaptureEvidence, PortableProbeError> {
        self.active_runtime("session.ship_catalog")?
            .capture_ship_catalog(capture_root)
            .map_err(map_runtime_error)
    }

    /// 在当前便携会话持有的唯一认证连接上派发装备命令。
    pub(crate) fn execute_equipment_command(
        &mut self,
        command: &RuntimeEquipmentCommand,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        self.active_runtime("session.equipment_command")?
            .execute_equipment_command(command)
            .map_err(map_runtime_error)
    }

    /// 查询当前便携会话中原装备命令的状态。
    pub(crate) fn query_equipment_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        self.active_runtime("session.equipment_command")?
            .query_equipment_command(command_id, budget)
            .map_err(map_runtime_error)
    }

    /// 停止观察当前便携会话中的原装备命令。
    pub(crate) fn cancel_equipment_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        self.active_runtime("session.equipment_command")?
            .cancel_equipment_command(command_id, budget)
            .map_err(map_runtime_error)
    }

    /// 关闭或部分关闭后的会话只允许继续清理，不得再次进入运行态操作。
    fn active_runtime(
        &mut self,
        stage: &'static str,
    ) -> Result<&mut RuntimeSession, PortableProbeError> {
        self.runtime.as_mut().ok_or_else(|| {
            map_runtime_error(RuntimeProbeError::InvalidOutput {
                stage,
                message: "便携会话已经开始关闭，不能继续执行运行态操作".to_owned(),
            })
        })
    }

    /// 先按运行态策略断开或卸载代理，再停止独立 ADB，两个阶段均生成证据。
    pub(crate) fn shutdown(
        &mut self,
    ) -> Result<&PortableSessionShutdownEvidence, PortableProbeError> {
        if self.shutdown_evidence.is_none() {
            if self.runtime_shutdown.is_none() {
                let runtime: RuntimeSessionShutdownEvidence = self
                    .runtime
                    .as_mut()
                    .expect("尚未关闭的便携会话必须持有运行态")
                    .shutdown()
                    .map_err(map_runtime_error)?;
                self.runtime.take();
                self.runtime_shutdown = Some(runtime);
            }
            let adb: PortableCleanupEvidence = self
                .prepared
                .as_mut()
                .expect("尚未关闭的便携会话必须持有便携资源")
                .shutdown_adb()?;
            self.prepared.take();
            let runtime: RuntimeSessionShutdownEvidence = self
                .runtime_shutdown
                .take()
                .expect("运行态成功关闭后必须保留清理证据");
            self.shutdown_evidence = Some(PortableSessionShutdownEvidence { runtime, adb });
        }
        let evidence = self
            .shutdown_evidence
            .as_ref()
            .expect("成功关闭后必须保存完整清理证据");
        evidence.verify_graceful_shutdown()?;
        Ok(evidence)
    }
}

#[cfg(target_os = "windows")]
impl Drop for PortableRuntimeSession {
    /// 提前返回时严格按运行态、独立 ADB 的顺序释放资源。
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            drop(runtime);
        }
        if let Some(prepared) = self.prepared.take() {
            drop(prepared);
        }
    }
}

/// 建立尚未运行具体 RPC 的便携资源所有者，启动后的失败也显式关闭 ADB。
#[cfg(target_os = "windows")]
fn prepare_portable_session(
    options: PortableProbeOptions,
    allow_start: bool,
    related: Option<&crate::adapters::RelatedLogSink>,
) -> Result<PreparedPortableSession, PortableProbeError> {
    options.validate()?;
    let (bundle, bundle_inspection): (AdbBundle, AdbBundleInspection) =
        load_and_inspect_configured_adb_bundle(&options, related)?;
    let target_package: String = resolve_target_package(&options)?;
    let ResolvedManager {
        executable: manager_path,
        instances,
    } = resolve_manager(&options)?;
    let mut instance: EmulatorInstance = select_instance(&instances, &options)?;
    let instance_started_by_probe: bool = if instance.is_ready() {
        false
    } else {
        if !allow_start {
            return Err(PortableProbeError::Target {
                stage: "resident.require_ready_instance",
                message: "模拟器实例未就绪".to_owned(),
            });
        }
        let should_launch: bool = !instance.is_process_started;
        if should_launch {
            launch_manager_instance(&manager_path, &instance.index)?;
        }
        instance = wait_for_ready_instance(
            &manager_path,
            &instance.index,
            Duration::from_secs(u64::from(options.startup_timeout_seconds)),
        )?;
        should_launch
    };
    let serial: String = instance.serial()?;
    if options.mode == PortableMode::Manual
        && !options.require_ready_instance
        && options.serial_hint.as_deref() != Some(serial.as_str())
    {
        return Err(PortableProbeError::TargetMismatch {
            message: format!(
                "manual serial {:?} 与管理器证明的 {serial} 不一致",
                options.serial_hint
            ),
        });
    }

    let server: OwnedAdbServer = OwnedAdbServer::start(bundle.clone(), serial.clone())
        .map_err(|source| adb_failure("adb.start", source))?;
    let operation: Result<
        (TargetEvidence, PortableAdbEvidence, RuntimeProbeOptions),
        PortableProbeError,
    > = (|| {
        server
            .connect_target()
            .map_err(|source| adb_failure("adb.target", source))?;
        let target: TargetEvidence = verify_target_and_start_game(
            &manager_path,
            &instance.index,
            &serial,
            &target_package,
            Duration::from_secs(u64::from(options.startup_timeout_seconds)),
            allow_start && !options.require_ready_instance,
            &server,
        )?;
        let adb: PortableAdbEvidence = PortableAdbEvidence {
            bundle: bundle_inspection,
            server_port: server.port(),
            server_process_id: server.process_id(),
            server_log_path: server.log_path().to_path_buf(),
        };
        let manager_text: String = windows_path(&manager_path, "manager")?;
        let adb_text: String = windows_path(bundle.executable(), "adb")?;
        let runtime_options = RuntimeProbeOptions::from_resolved(
            &options.tool_root,
            super::super::probe::ResolvedRuntimeTarget {
                manager_executable: manager_text,
                adb_executable: adb_text,
                vm_index: instance.index.clone(),
                serial: serial.clone(),
                adb_server_port: server.port(),
                adb_process_policy: bundle.process_policy().clone(),
                connect_timeout_seconds: options.connect_timeout_seconds,
                startup_timeout_seconds: options.startup_timeout_seconds,
                agent_mapping_mode: options.agent_mapping_mode,
                agent_visibility_mode: options.agent_visibility_mode,
                retain_agent: options.retain_agent,
                timeout_ms: options.timeout_ms,
                max_items: options.max_items,
                full_state_capture_root: options.full_state_capture_root.clone(),
            },
        )?;
        Ok((target, adb, runtime_options))
    })();

    let (target, adb, runtime_options) = match operation {
        Ok(result) => result,
        Err(operation) => {
            return match finish_portable_operation::<()>(server, Err(operation)) {
                Err(error) => Err(error),
                Ok(_) => unreachable!("失败的目标准备不能产生便携成功结果"),
            };
        }
    };
    let manager: ManagerEvidence = ManagerEvidence {
        executable: manager_path,
        instance_index: instance.index,
        instance_name: instance.name,
        android_version: instance
            .android_version
            .unwrap_or_else(|| UNREPORTED_ANDROID_VERSION.to_owned()),
        instance_started_by_probe,
    };
    Ok(PreparedPortableSession {
        options,
        manager,
        target,
        adb,
        runtime_options,
        server: Some(server),
    })
}

/// 以当前运行态 profile 为支持边界；自动提示可失效回退，手工提示必须精确一致。
#[cfg(target_os = "windows")]
fn resolve_target_package(options: &PortableProbeOptions) -> Result<String, PortableProbeError> {
    let tool_root: ToolRoot = ToolRoot::open(&options.tool_root)?;
    let profile_path: PathBuf = tool_root.existing_file(Path::new(PROFILE_RELATIVE_PATH))?;
    let profile: RuntimeProfile = RuntimeProfile::load(&profile_path)
        .map_err(RuntimeProbeError::from)
        .map_err(PortableProbeError::Runtime)?;
    select_profile_package(
        options.mode,
        options.game_package_hint.as_deref(),
        profile.bootstrap().package_name(),
    )
}

/// 自动提示不匹配时回到已验证 profile；手工提示一旦提供就必须精确匹配。
#[cfg(any(target_os = "windows", test))]
pub(super) fn select_profile_package(
    mode: PortableMode,
    configured: Option<&str>,
    profile: &str,
) -> Result<String, PortableProbeError> {
    if mode == PortableMode::Manual
        && let Some(configured) = configured
        && configured != profile
    {
        return Err(PortableProbeError::ProfilePackageMismatch {
            configured: configured.to_owned(),
            profile: profile.to_owned(),
        });
    }
    Ok(profile.to_owned())
}

/// 先同步同目录临时文件，再原子发布便携流程收据且不覆盖既有证据。
#[cfg(target_os = "windows")]
fn write_portable_receipt(
    tool_root: &Path,
    report: &PortableProbeReport,
) -> Result<PathBuf, PortableProbeError> {
    let tool_root: ToolRoot = ToolRoot::open(tool_root)?;
    let map_sequence_error = |source| {
        PortableProbeError::Runtime(RuntimeProbeError::Io {
            stage: "portable.receipt.sequence",
            path: tool_root.as_path().join("data/history"),
            source,
        })
    };
    let directory = crate::adapters::numbered_files::NumberedDirectory::open(
        &tool_root,
        Path::new("data/history"),
    )
    .map_err(map_sequence_error)?;
    let target_relative = directory
        .next_path("portable", "json", true)
        .map_err(map_sequence_error)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let published: PublishedJson = write_new_pretty_json(
        &tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_PORTABLE_RECEIPT_BYTES,
        report,
    )?;
    Ok(published.into_path())
}

/// 工具目录只保存不含原始配置正文的专用静态目录收据。
#[cfg(target_os = "windows")]
fn write_portable_ship_catalog_receipt(
    tool_root: &Path,
    report: &PortableShipCatalogCaptureReport,
) -> Result<PathBuf, PortableProbeError> {
    let tool_root = ToolRoot::open(tool_root)?;
    let map_sequence_error = |source| {
        PortableProbeError::Runtime(RuntimeProbeError::Io {
            stage: "portable.receipt.sequence",
            path: tool_root.as_path().join("data/history"),
            source,
        })
    };
    let directory = crate::adapters::numbered_files::NumberedDirectory::open(
        &tool_root,
        Path::new("data/history"),
    )
    .map_err(map_sequence_error)?;
    let target_relative = directory
        .next_path("ships", "json", true)
        .map_err(map_sequence_error)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let published = write_new_pretty_json(
        &tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_PORTABLE_RECEIPT_BYTES,
        report,
    )?;
    Ok(published.into_path())
}

/// 清理完成与正常卸载分别判定，重复查询同一关闭结果不重做资源操作。
#[cfg(target_os = "windows")]
impl PortableSessionShutdownEvidence {
    pub(super) fn verify_graceful_shutdown(&self) -> Result<(), PortableProbeError> {
        if let super::super::probe::RuntimeShutdownMethodEvidence::RestartFallback {
            unload_error,
            journal_errors,
        } = &self.runtime.method
        {
            let mut message = unload_error.clone();
            if !journal_errors.is_empty() {
                message.push_str("；日志错误: ");
                message.push_str(&journal_errors.join("；"));
            }
            return Err(PortableProbeError::RuntimeShutdownFailed {
                message,
                journal_path: self.runtime.journal_path.clone(),
                game_restarted: self.runtime.cleanup.game_restarted,
            });
        }
        Ok(())
    }
}

/// 管理操作复用便携发现和独立 ADB，查询与卸载明确禁止启动实例或游戏。
#[cfg(target_os = "windows")]
pub(in crate::adapters::device) fn manage_portable_agent(
    options: PortableProbeOptions,
    action: super::super::game_port::AgentAction,
    related: Option<&crate::adapters::RelatedLogSink>,
) -> Result<super::super::game_port::AgentStatusReport, PortableProbeError> {
    use super::super::game_port::{AgentAction, AgentStatusReport};
    let selected = options.instance_hint.clone();
    let prepared = match prepare_portable_session(options, action == AgentAction::Inject, related) {
        Ok(prepared) => prepared,
        Err(PortableProbeError::Target {
            stage: "target.require_running_game" | "resident.require_ready_instance",
            message,
        }) => {
            return Ok(AgentStatusReport {
                status: "unavailable".to_owned(),
                available: false,
                pid: None,
                instance: selected,
                message,
                agent_version: None,
                catalog_generation: None,
            });
        }
        Err(error) => return Err(error),
    };
    let instance = Some(format!(
        "{}:{}",
        suzushiro_emulator::adapter_for_manager(&prepared.manager.executable)?.id(),
        prepared.manager.instance_index
    ));
    let result =
        super::super::probe::manage_runtime_agent(prepared.runtime_options.clone(), action)
            .map_err(map_runtime_error);
    let (health, _) = match prepared.finish(result) {
        Ok(result) => result,
        Err(PortableProbeError::Runtime(RuntimeProbeError::Io {
            stage: "resident.lock",
            source,
            ..
        })) if source.raw_os_error()
            == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32) =>
        {
            return Ok(AgentStatusReport {
                status: "busy".to_owned(),
                available: false,
                pid: None,
                instance,
                message: "使用中".to_owned(),
                agent_version: None,
                catalog_generation: None,
            });
        }
        Err(error) => return Err(error),
    };
    let pid = health.as_ref().map(|h| h.process_id);
    let (status, message, available) = match (action, pid) {
        (AgentAction::Unload, Some(_)) => ("unloaded", "已卸载", false),
        (_, None) => ("absent", "未加载", false),
        (AgentAction::Inject, Some(_)) => ("ready", "已加载", true),
        (AgentAction::Status, Some(_)) => ("ready", "已连接", true),
    };
    Ok(AgentStatusReport {
        status: status.to_owned(),
        available,
        pid,
        instance,
        message: message.to_owned(),
        catalog_generation: health.as_ref().map(|h| h.catalog_generation),
        agent_version: health.map(|h| h.agent_version),
    })
}

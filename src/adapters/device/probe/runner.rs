//! 持有单次探针资源，并编排部署、认证、卸载和 forward 生命周期。

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::super::bootstrap::encode_bootstrap;
#[cfg(target_os = "windows")]
use super::super::bootstrap::{RuntimeUnloadIdentity, encode_unload};
use super::super::capture::full_state::{FullStateCaptureEvidence, FullStateCaptureRequest};
use super::super::profile::{MAX_PROFILE_BYTES, RuntimeProfile};
use super::super::reading::equipment::EquipmentReadResult;
use super::super::reading::game_state::GameReadOptions;
#[cfg(target_os = "windows")]
use super::super::runtime::ShutdownPreparedResult;
use super::super::runtime::{
    AgentClient, CapabilitiesResult, ExpectedAgent, HealthResult, LiveProtocolProbeResult,
    RuntimeAbi,
};
use super::super::session::{SessionId, SessionSecret};
use super::cleanup::{CleanupResult, TargetRecoveryState, find_owned_forward_port};
use super::device_bridge::DeviceBridge;
use super::journal::{ProbeJournal, journal_error_summary};
#[cfg(target_os = "windows")]
use super::process_evidence::PostUnloadMappingEvidence;
#[cfg(target_os = "windows")]
use super::process_evidence::validate_graceful_unload_evidence;
use super::process_evidence::{
    ModuleReadiness, ProcessEvidence, non_empty_lines, validate_clean_baseline,
    validate_preserved_process_identity,
};
use super::readiness::{AgentReadiness, wait_for_full_state_readiness, wait_for_main_thread};
#[cfg(target_os = "windows")]
use super::receipts::UnloadReceipt;
#[cfg(target_os = "windows")]
use super::receipts::validate_unloader_result;
use super::receipts::{
    LoaderFailureDecision, LoaderReceipt, loader_failure_decision, loader_failure_journal_details,
    parse_loader_receipt,
};
#[cfg(target_os = "windows")]
use super::runtime_session::GracefulUnloadEvidence;
use super::{
    AGENT_RELATIVE_PATH, LOADER_ENTERED_MARKER, LOADER_RELATIVE_PATH, PROFILE_RELATIVE_PATH,
    RuntimeProbeError, RuntimeProbeOptions, ShipGrowthSummary, guarded_loader_command,
    probe_failed_after_cleanup, sha256_json, write_secret_file,
};
#[cfg(target_os = "windows")]
use super::{UNLOADER_ENTERED_MARKER, guarded_unloader_command};
use crate::adapters::file_snapshot::{FileSnapshotError, read_bounded_file_snapshot};
use crate::adapters::tool_root::ToolRoot;
use crate::adapters::{MAXIMUM_AGENT_FILE_BYTES, MAXIMUM_RELEASE_FILE_BYTES};
use suzushiro_emulator::RemoteShellOutput;

pub(in crate::adapters::device::probe) mod audit;
#[cfg(target_os = "windows")]
mod resident;

/// 保存清理前已经完成的探针结果，供最终收据与清理证据合并。
pub(super) struct ProbeCoreResult {
    pub(super) loader_receipt: LoaderReceipt,
    pub(super) forward_port: u16,
    pub(super) handshake_attempts: u32,
    pub(super) health: HealthResult,
    pub(super) capabilities: CapabilitiesResult,
    pub(super) bag_readiness_retries: u32,
    pub(super) snapshot_hashes: Vec<String>,
    pub(super) snapshot_count: u32,
    pub(super) snapshots_complete: bool,
    pub(super) owned_state_readiness_retries: u32,
    pub(super) owned_state_hashes: Vec<String>,
    pub(super) ship_details_readiness_retries: u32,
    pub(super) ship_detail_hashes: Vec<String>,
    pub(super) ship_roster_hashes: Vec<String>,
    pub(super) game_state_content_sha256: String,
    pub(super) dock_count: u32,
    pub(super) ship_detail_count: u32,
    pub(super) ship_growth_summary: ShipGrowthSummary,
    pub(super) warehouse_count: u32,
    pub(super) owned_state_bag_count: u32,
    pub(super) owned_state_complete: bool,
    pub(super) equipment: EquipmentReadResult,
    pub(super) full_state_capture: Option<FullStateCaptureEvidence>,
    pub(super) protocol_failure_probes: LiveProtocolProbeResult,
    pub(super) before_load: ProcessEvidence,
    pub(super) after_load: ProcessEvidence,
}

pub(super) struct PreparedHandshake {
    pub(super) loader_receipt: LoaderReceipt,
    pub(super) forward_port: u16,
    pub(super) before_load: ProcessEvidence,
}

/// 保存已经完成唯一握手、仍可发送 RPC 的 agent 所有权和加载证据。
pub(super) struct AuthenticatedAgent {
    pub(super) client: AgentClient,
    pub(super) loader_receipt: LoaderReceipt,
    pub(super) forward_port: u16,
    pub(super) handshake_attempts: u32,
    pub(super) oversized_frame_closed_connection: bool,
    pub(super) before_load: ProcessEvidence,
}

/// 生产会话持有目标、连接、日志和清理资源。
/// 审计在这个会话上追加一致性样本和负向探针，日常读取不调用审计入口。
pub(super) struct ProductionSession {
    #[cfg(target_os = "windows")]
    resident_store: resident::ResidentStore,
    pub(super) resident_preserved: bool,
    resident_record_owned: bool,
    pub(super) options: RuntimeProbeOptions,
    pub(super) tool_root: ToolRoot,
    pub(super) bridge: DeviceBridge,
    pub(super) profile: RuntimeProfile,
    pub(super) session_id: SessionId,
    pub(super) host_session_id: SessionId,
    session_secret: SessionSecret,
    channel_id: SessionId,
    mapping_id: SessionId,
    pub(super) remote_endpoint: String,
    loader_path: PathBuf,
    agent_path: PathBuf,
    pub(super) profile_sha256: String,
    pub(super) loader_sha256: String,
    pub(super) agent_sha256: String,
    pub(super) host_session_dir: PathBuf,
    pub(super) host_session_relative: PathBuf,
    host_bootstrap_path: PathBuf,
    host_bootstrap_relative: PathBuf,
    pub(super) device_session_dir: String,
    pub(super) device_loader_path: String,
    pub(super) device_agent_path: String,
    pub(super) device_bootstrap_path: String,
    pub(super) stage_loader_path: String,
    pub(super) stage_agent_path: String,
    pub(super) stage_bootstrap_path: String,
    pub(super) stage_unload_path: String,
    pub(super) target_pid: u32,
    pub(super) launch_component: Option<String>,
    pub(super) forward_port: Option<u16>,
    pub(super) forward_creation_attempted: bool,
    pub(super) target_recovery_state: TargetRecoveryState,
    pub(super) process_wait_exhausted: bool,
    pub(super) expected_process_start_time: Option<u64>,
    #[cfg(target_os = "windows")]
    pub(super) agent_unloaded: bool,
    pub(super) cleanup_result: Option<CleanupResult>,
    pub(super) forwards_before: Option<Vec<String>>,
    pub(super) journal: ProbeJournal,
}

fn read_runtime_input_snapshot(
    tool_root: &ToolRoot,
    relative_path: &'static str,
    maximum_bytes: u64,
) -> Result<(Vec<u8>, String), RuntimeProbeError> {
    let snapshot = read_bounded_file_snapshot(tool_root, Path::new(relative_path), maximum_bytes)
        .map_err(|source| map_runtime_input_snapshot_error(relative_path, source))?;
    let (bytes, sha256) = snapshot.into_parts();
    if bytes.is_empty() {
        return Err(RuntimeProbeError::InvalidToolAsset {
            path: PathBuf::from(relative_path),
            message: "运行态文件不能为空".to_owned(),
        });
    }
    Ok((bytes, sha256))
}

fn map_runtime_input_snapshot_error(
    relative_path: &'static str,
    source: FileSnapshotError,
) -> RuntimeProbeError {
    match source {
        FileSnapshotError::Path(source) => RuntimeProbeError::ToolRoot(source),
        FileSnapshotError::Io { source, .. } => RuntimeProbeError::Io {
            stage: "runtime_input.read_snapshot",
            path: PathBuf::from(relative_path),
            source,
        },
        FileSnapshotError::TooLarge { actual, maximum } => RuntimeProbeError::InvalidToolAsset {
            path: PathBuf::from(relative_path),
            message: format!("运行态文件大小 {actual} 超过 {maximum} 字节上限"),
        },
        FileSnapshotError::Changed => RuntimeProbeError::InvalidToolAsset {
            path: PathBuf::from(relative_path),
            message: "运行态文件在读取期间发生变化".to_owned(),
        },
    }
}

impl ProductionSession {
    /// 校验固定资产，获得目标排他所有权并建立宿主日志与候选会话路径。
    pub(super) fn new(
        options: RuntimeProbeOptions,
        related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Self, RuntimeProbeError> {
        let tool_root: ToolRoot = ToolRoot::open(&options.tool_root)?;
        let loader_path: PathBuf = tool_root.existing_file(Path::new(LOADER_RELATIVE_PATH))?;
        let agent_path: PathBuf = tool_root.existing_file(Path::new(AGENT_RELATIVE_PATH))?;
        let (profile_bytes, profile_sha256) =
            read_runtime_input_snapshot(&tool_root, PROFILE_RELATIVE_PATH, MAX_PROFILE_BYTES)?;
        let (_, loader_sha256) = read_runtime_input_snapshot(
            &tool_root,
            LOADER_RELATIVE_PATH,
            MAXIMUM_RELEASE_FILE_BYTES,
        )?;
        let (_, agent_sha256) =
            read_runtime_input_snapshot(&tool_root, AGENT_RELATIVE_PATH, MAXIMUM_AGENT_FILE_BYTES)?;
        let profile: RuntimeProfile = RuntimeProfile::from_slice(&profile_bytes)?;
        if profile.abi() != RuntimeAbi::X86_64 {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "profile.abi",
                message: "只读运行态只支持 x86_64 profile".to_owned(),
            });
        }

        #[cfg(target_os = "windows")]
        let resident_store = resident::ResidentStore::acquire(
            &tool_root,
            &options,
            profile.bootstrap().package_name(),
        )?;
        let session_id: SessionId = SessionId::generate()?;
        let session_secret: SessionSecret = SessionSecret::generate()?;
        let channel_id: SessionId = SessionId::generate()?;
        let mapping_id: SessionId = SessionId::generate()?;
        let session_hex: String = session_id.to_string();
        let remote_endpoint: String = format!("localabstract:{channel_id}");
        let device_session_dir: String = format!("/data/local/tmp/.{session_hex}");
        let device_loader_path: String = format!("{device_session_dir}/0");
        let device_agent_path: String = format!("{device_session_dir}/1");
        let device_bootstrap_path: String = format!("{device_session_dir}/2");
        let stage_loader_path: String = format!("{device_session_dir}.0");
        let stage_agent_path: String = format!("{device_session_dir}.1");
        let stage_bootstrap_path: String = format!("{device_session_dir}.2");
        let stage_unload_path: String = format!("{device_session_dir}.3");
        let bridge: DeviceBridge = DeviceBridge::new(&options);

        tool_root.ensure_directory(Path::new("data/logs"))?;
        tool_root.ensure_directory(Path::new("data/temp"))?;
        let host_session_relative: PathBuf = Path::new("data/temp").join(&session_hex);
        let host_bootstrap_relative: PathBuf = host_session_relative.join("bootstrap.bin");
        let mut journal: ProbeJournal = ProbeJournal::create(&tool_root, related.as_ref())?;
        let host_resources: Result<(PathBuf, PathBuf), RuntimeProbeError> = (|| {
            let host_session_dir: PathBuf = tool_root.ensure_directory(&host_session_relative)?;
            let host_bootstrap_path: PathBuf =
                tool_root.prepare_new_file(&host_bootstrap_relative)?;
            Ok((host_session_dir, host_bootstrap_path))
        })();
        let (host_session_dir, host_bootstrap_path): (PathBuf, PathBuf) = match host_resources {
            Ok(resources) => resources,
            Err(source) => {
                let message: String = journal_error_summary(&source);
                let _ = journal.record(
                    "session.initialize.failure",
                    "error",
                    json!({"message": message}),
                );
                return Err(probe_failed_after_cleanup(
                    source,
                    journal.path.clone(),
                    || {
                        tool_root
                            .remove_directory_if_exists(&host_session_relative)
                            .map(|_| ())
                            .map_err(RuntimeProbeError::from)
                    },
                ));
            }
        };

        Ok(Self {
            #[cfg(target_os = "windows")]
            resident_store,
            resident_preserved: false,
            resident_record_owned: false,
            options: RuntimeProbeOptions {
                tool_root: tool_root.as_path().to_path_buf(),
                ..options
            },
            tool_root,
            bridge,
            profile,
            session_id,
            host_session_id: session_id,
            session_secret,
            channel_id,
            mapping_id,
            remote_endpoint,
            loader_path,
            agent_path,
            profile_sha256,
            loader_sha256,
            agent_sha256,
            host_session_dir,
            host_session_relative,
            host_bootstrap_path,
            host_bootstrap_relative,
            device_session_dir,
            device_loader_path,
            device_agent_path,
            device_bootstrap_path,
            stage_loader_path,
            stage_agent_path,
            stage_bootstrap_path,
            stage_unload_path,
            target_pid: 0,
            launch_component: None,
            forward_port: None,
            forward_creation_attempted: false,
            target_recovery_state: TargetRecoveryState::PreserveOriginal,
            process_wait_exhausted: false,
            expected_process_start_time: None,
            #[cfg(target_os = "windows")]
            agent_unloaded: false,
            cleanup_result: None,
            forwards_before: None,
            journal,
        })
    }

    /// 优先验证并重连驻留代理，否则部署；认证连接保留严格卸载所需的证据。
    pub(super) fn open_authenticated_agent(
        &mut self,
    ) -> Result<AuthenticatedAgent, RuntimeProbeError> {
        self.observe_target()?;
        #[cfg(target_os = "windows")]
        if let Some(agent) = self.reconnect_resident(false)? {
            return Ok(agent);
        }
        let prepared = self.prepare_deployed_handshake()?;
        self.finish_agent_handshake(prepared, false)
    }

    pub(super) fn observe_target(&mut self) -> Result<(), RuntimeProbeError> {
        self.forwards_before = Some(self.bridge.forward_list()?);
        let identity = self.bridge.verify_target()?;
        self.journal.record(
            "target.identity",
            "ok",
            json!({
                "manager_info": identity.manager_info,
                "cross_channel_boot_id_matched": identity.cross_channel_boot_id_matched,
            }),
        )?;
        Ok(())
    }

    pub(super) fn prepare_deployed_handshake(
        &mut self,
    ) -> Result<PreparedHandshake, RuntimeProbeError> {
        let package_name: String = self.profile.bootstrap().package_name().to_owned();
        let module_name: String = self.profile.bootstrap().module_name().to_owned();
        let initial_process_id: u32 = self
            .bridge
            .discover_single_pid(&package_name, "target.discover_pid")?;
        let module_readiness: ModuleReadiness = self.bridge.wait_for_loaded_module(
            &package_name,
            &module_name,
            initial_process_id,
            self.options.startup_timeout,
        )?;
        self.target_pid = module_readiness.process_id;
        let module_process_start_time = self
            .bridge
            .read_process_start_time(self.target_pid, "target.module_ready_identity")?;
        self.expected_process_start_time = Some(module_process_start_time);
        self.journal.record(
            "target.module_ready",
            "ok",
            json!({
                "initial_process_id": initial_process_id,
                "process_id": module_readiness.process_id,
                "process_start_time": module_process_start_time,
                "module_name": module_name,
                "retries": module_readiness.retries,
            }),
        )?;
        self.launch_component = Some(
            self.bridge
                .resolve_launch_component(self.profile.bootstrap().package_name())?,
        );

        self.deploy_runtime_assets()?;
        self.verify_deployed_hashes()?;
        let load_readiness: ModuleReadiness = self.bridge.wait_for_loaded_module(
            &package_name,
            &module_name,
            self.target_pid,
            self.options.startup_timeout,
        )?;
        let previous_process_id: u32 = self.target_pid;
        self.target_pid = load_readiness.process_id;
        let load_process_start_time = self
            .bridge
            .read_process_start_time(self.target_pid, "target.load_ready_identity")?;
        self.expected_process_start_time = Some(load_process_start_time);
        let module_sha256 = self
            .bridge
            .read_module_sha256(self.target_pid, &module_name)?;
        self.profile.bind_module_sha256(module_sha256.clone())?;
        self.journal.record(
            "target.load_ready",
            "ok",
            json!({
                "previous_process_id": previous_process_id,
                "process_id": self.target_pid,
                "process_start_time": load_process_start_time,
                "process_changed": previous_process_id != self.target_pid,
                "module_sha256": module_sha256,
                "module_name": module_name,
                "retries": load_readiness.retries,
            }),
        )?;
        let before_load: ProcessEvidence = self.bridge.collect_process_evidence(self.target_pid)?;
        validate_preserved_process_identity(
            &before_load,
            self.target_pid,
            self.expected_process_start_time,
            "target.baseline_identity",
        )?;
        validate_clean_baseline(&before_load)?;
        self.journal.record(
            "target.baseline",
            "ok",
            serde_json::to_value(&before_load).map_err(|source| RuntimeProbeError::Json {
                stage: "journal.baseline",
                source,
            })?,
        )?;

        self.write_bootstrap()?;
        self.deploy_bootstrap()?;
        #[cfg(target_os = "windows")]
        if self.options.retain_agent {
            self.save_resident(&before_load, None)?;
            self.resident_preserved = true;
        }
        let loader_receipt: LoaderReceipt =
            self.run_loader(&package_name, &module_name, &before_load)?;
        let forward_port: u16 = self.create_forward()?;
        Ok(PreparedHandshake {
            loader_receipt,
            forward_port,
            before_load,
        })
    }

    pub(super) fn finish_agent_handshake(
        &mut self,
        prepared: PreparedHandshake,
        oversized_frame_closed_connection: bool,
    ) -> Result<AuthenticatedAgent, RuntimeProbeError> {
        let PreparedHandshake {
            loader_receipt,
            forward_port,
            before_load,
        } = prepared;
        let expected: ExpectedAgent = ExpectedAgent::new(
            self.session_id,
            self.target_pid,
            self.profile.bootstrap().package_name(),
            self.profile.abi(),
        )?;
        let address: SocketAddr = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), forward_port);
        let client: AgentClient = AgentClient::connect(
            address,
            expected,
            self.session_secret.clone(),
            self.options.connect_timeout,
        )?;
        let handshake_attempts: u32 = client.handshake_attempts();
        Ok(AuthenticatedAgent {
            client,
            loader_receipt,
            forward_port,
            handshake_attempts,
            oversized_frame_closed_connection,
            before_load,
        })
    }

    /// 等待主线程与完整业务数据可读，同时把认证连接的所有权留给调用方。
    pub(super) fn wait_for_agent_readiness(
        &self,
        authenticated: &mut AuthenticatedAgent,
    ) -> Result<AgentReadiness, RuntimeProbeError> {
        let health: HealthResult = wait_for_main_thread(&mut authenticated.client)?;
        let sample = wait_for_full_state_readiness(
            &mut authenticated.client,
            self.options.timeout_ms,
            self.options.max_items,
            self.profile.bootstrap().module_sha256(),
        )?;

        Ok(AgentReadiness { health, sample })
    }

    /// 使用当前 profile 和有界选项构造一次完整状态读取参数。
    pub(super) fn game_read_options(
        &self,
        read_index: u64,
    ) -> Result<GameReadOptions, RuntimeProbeError> {
        let mut options = GameReadOptions::new(
            self.options.timeout_ms,
            self.options.max_items,
            self.options.max_items,
            self.options.max_items,
            self.profile.bootstrap().module_sha256(),
        )?;
        if let Some(capture_root) = &self.options.full_state_capture_root {
            options = options.with_full_state_capture(FullStateCaptureRequest::new(
                &self.options.tool_root,
                capture_root,
                self.host_session_id,
                read_index,
            )?);
        }
        Ok(options)
    }

    /// 写入仅供本次会话使用的启动文件，并在落盘后擦除内存中的编码副本。
    fn write_bootstrap(&mut self) -> Result<(), RuntimeProbeError> {
        let target_pid: i32 =
            i32::try_from(self.target_pid).map_err(|_| RuntimeProbeError::InvalidOutput {
                stage: "bootstrap.target_pid",
                message: format!("PID {} 无法表示为 i32", self.target_pid),
            })?;
        let mut encoded = encode_bootstrap(
            target_pid,
            self.options.timeout_ms,
            self.session_id,
            &self.session_secret,
            self.channel_id,
            self.mapping_id,
            self.options.agent_mapping_mode,
            self.options.agent_visibility_mode,
            self.profile.bootstrap(),
        )?;
        let result: Result<(), RuntimeProbeError> = write_secret_file(
            &self.host_bootstrap_path,
            &encoded,
            "host.create_bootstrap",
            "host.write_bootstrap",
        );
        encoded.fill(0);
        result
    }

    /// 合并加载收据与排空收据，生成不含会话密钥的一次性卸载文件。
    #[cfg(target_os = "windows")]
    fn write_unload(
        &mut self,
        loader: &LoaderReceipt,
        prepared: &ShutdownPreparedResult,
    ) -> Result<(), RuntimeProbeError> {
        let host_unload_relative: PathBuf = self.host_session_relative.join("unload.bin");
        let host_unload_path: PathBuf = self.tool_root.prepare_new_file(&host_unload_relative)?;
        let target_pid: i32 =
            i32::try_from(self.target_pid).map_err(|_| RuntimeProbeError::InvalidOutput {
                stage: "unload.target_pid",
                message: format!("PID {} 无法表示为 i32", self.target_pid),
            })?;
        let identity = RuntimeUnloadIdentity {
            process_start_time: loader.process_start_time,
            agent_handle: loader.agent_handle.get(),
            agent_base: loader.agent_base.get(),
            agent_load_size: loader.agent_load_size,
            finalize_address: loader.finalize_address.get(),
            agent_mapping_name: loader.agent_mapping_name.clone(),
            hook_target: prepared.hook_target.get(),
            trampoline_start: prepared.trampoline_start.get(),
            trampoline_size: prepared.trampoline_size,
            rpc_worker_tid: prepared.worker_tid,
            rpc_worker_start_time: prepared.worker_start_time,
            agent_mapping_mode: loader.agent_mapping_mode,
            agent_visibility_mode: loader.agent_visibility_mode,
            agent_soinfo_address: loader
                .agent_soinfo_address
                .map_or(0, |address| address.get()),
            protected_elf_header: loader.protected_elf_header_bytes()?.unwrap_or([0; 64]),
        };
        let mut encoded: [u8; 512] = encode_unload(
            target_pid,
            self.options.timeout_ms,
            self.session_id,
            self.profile.bootstrap(),
            &self.agent_sha256,
            &identity,
        )?;
        let result: Result<(), RuntimeProbeError> = write_secret_file(
            &host_unload_path,
            &encoded,
            "host.create_unload",
            "host.write_unload",
        );
        encoded.fill(0);
        result
    }

    /// 先部署与进程无关的固定运行资产，再确认最终 PID 并生成启动文件。
    fn deploy_runtime_assets(&mut self) -> Result<(), RuntimeProbeError> {
        self.bridge.adb_push(
            &self.loader_path,
            &self.stage_loader_path,
            "deploy.push_loader",
        )?;
        self.bridge.adb_push(
            &self.agent_path,
            &self.stage_agent_path,
            "deploy.push_agent",
        )?;

        self.bridge.root_checked(
            &format!("mkdir {}", self.device_session_dir),
            "deploy.create_session",
        )?;
        self.bridge.root_checked(
            &format!("chown 0:0 {}", self.device_session_dir),
            "deploy.chown_session",
        )?;
        self.bridge.root_checked(
            &format!("chmod 700 {}", self.device_session_dir),
            "deploy.chmod_session",
        )?;
        self.bridge.root_checked(
            &format!("mv {} {}", self.stage_loader_path, self.device_loader_path),
            "deploy.move_loader",
        )?;
        self.bridge.root_checked(
            &format!("mv {} {}", self.stage_agent_path, self.device_agent_path),
            "deploy.move_agent",
        )?;
        self.bridge.root_checked(
            &format!(
                "chown 0:0 {} {}",
                self.device_loader_path, self.device_agent_path
            ),
            "deploy.chown_assets",
        )?;
        self.bridge.root_checked(
            &format!("chmod 700 {}", self.device_loader_path),
            "deploy.chmod_loader",
        )?;
        self.bridge.root_checked(
            &format!("chmod 600 {}", self.device_agent_path),
            "deploy.chmod_agent",
        )?;
        Ok(())
    }

    /// 发布绑定最终目标 PID 的 root-only 启动文件，并删除宿主明文副本。
    fn deploy_bootstrap(&mut self) -> Result<(), RuntimeProbeError> {
        self.bridge.adb_push(
            &self.host_bootstrap_path,
            &self.stage_bootstrap_path,
            "deploy.push_bootstrap",
        )?;
        self.bridge.root_checked(
            &format!(
                "mv {} {}",
                self.stage_bootstrap_path, self.device_bootstrap_path
            ),
            "deploy.move_bootstrap",
        )?;
        self.bridge.root_checked(
            &format!("chown 0:0 {}", self.device_bootstrap_path),
            "deploy.chown_bootstrap",
        )?;
        self.bridge.root_checked(
            &format!("chmod 600 {}", self.device_bootstrap_path),
            "deploy.chmod_bootstrap",
        )?;
        self.tool_root
            .remove_file_if_exists(&self.host_bootstrap_relative, None)?;
        Ok(())
    }

    /// 原子发布 root-only 卸载文件，并删除宿主侧明文副本。
    #[cfg(target_os = "windows")]
    fn deploy_unload(&mut self) -> Result<(), RuntimeProbeError> {
        let host_unload_relative: PathBuf = self.host_session_relative.join("unload.bin");
        let host_unload_path: PathBuf = self.host_session_dir.join("unload.bin");
        let device_unload_path: String = format!("{}/3", self.device_session_dir);
        self.bridge.adb_push(
            &host_unload_path,
            &self.stage_unload_path,
            "deploy.push_unload",
        )?;
        self.bridge.root_checked(
            &format!("mv {} {}", self.stage_unload_path, device_unload_path),
            "deploy.move_unload",
        )?;
        self.bridge.root_checked(
            &format!("chown 0:0 {device_unload_path}"),
            "deploy.chown_unload",
        )?;
        self.bridge.root_checked(
            &format!("chmod 600 {device_unload_path}"),
            "deploy.chmod_unload",
        )?;
        self.tool_root
            .remove_file_if_exists(&host_unload_relative, None)?;
        Ok(())
    }

    /// 对比宿主与设备端资产摘要，拒绝传输损坏或错误文件。
    fn verify_remote_asset_hashes(&self) -> Result<(), RuntimeProbeError> {
        let output: String = self.bridge.root_checked(
            &format!(
                "sha256sum {} {}",
                self.device_loader_path, self.device_agent_path
            ),
            "deploy.verify_hashes",
        )?;
        let hashes: Vec<String> = output
            .lines()
            .filter_map(|line: &str| line.split_whitespace().next().map(str::to_owned))
            .collect();
        if hashes != [self.loader_sha256.clone(), self.agent_sha256.clone()] {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "deploy.verify_hashes",
                message: format!("设备资产摘要与宿主不一致: {hashes:?}"),
            });
        }
        Ok(())
    }

    /// 部署成功后将已复核的资产摘要写入审计日志。
    fn verify_deployed_hashes(&mut self) -> Result<(), RuntimeProbeError> {
        self.verify_remote_asset_hashes()?;
        self.journal.record(
            "deploy.runtime_assets_ready",
            "ok",
            json!({
                "profile_sha256": self.profile_sha256,
                "loader_sha256": self.loader_sha256,
                "agent_sha256": self.agent_sha256,
            }),
        )?;
        Ok(())
    }

    /// 在同一设备命令内复核最终目标，再执行一次性加载器并校验严格收据。
    fn run_loader(
        &mut self,
        package_name: &str,
        module_name: &str,
        _before_load: &ProcessEvidence,
    ) -> Result<LoaderReceipt, RuntimeProbeError> {
        let command: String = guarded_loader_command(
            package_name,
            module_name,
            self.target_pid,
            &self.device_loader_path,
            &self.device_agent_path,
            &self.device_bootstrap_path,
        );
        // 宿主命令中断时无法取得设备端回执，清理必须保守重启。
        self.target_recovery_state = TargetRecoveryState::RestartRequired;
        let output: RemoteShellOutput = self.bridge.root_status(&command, "loader.execute")?;
        let loader_entered: bool = non_empty_lines(&output.stdout)
            .iter()
            .any(|line: &String| line == LOADER_ENTERED_MARKER);
        if output.exit_code != 0 {
            let decision: LoaderFailureDecision = match loader_failure_decision(
                &output.stdout,
                output.exit_code,
                loader_entered,
                self.target_pid,
            ) {
                Ok(decision) => decision,
                Err(source) => {
                    self.target_recovery_state = TargetRecoveryState::RestartRequired;
                    return Err(source);
                }
            };
            self.target_recovery_state = if decision.requires_restart {
                TargetRecoveryState::RestartRequired
            } else {
                TargetRecoveryState::PreserveOriginal
            };
            if let Some(receipt) = decision.receipt {
                if let Err(error) = self.journal.record(
                    "loader.failure_receipt",
                    "error",
                    loader_failure_journal_details(output.exit_code, &receipt),
                ) {
                    eprintln!("加载失败收据日志写入失败: {error}");
                }
                return Err(RuntimeProbeError::LoaderRejected {
                    exit_code: output.exit_code,
                    code: receipt.code,
                    message: receipt.message,
                    process_id: receipt.process_id,
                    target_state: receipt.target_state.as_str().to_owned(),
                });
            }
            return Err(RuntimeProbeError::DeviceCommand {
                stage: "loader.execute",
                exit_code: output.exit_code,
                output: output.stdout,
            });
        }
        if !loader_entered {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.execute",
                message: "设备命令成功但未声明进入 loader".to_owned(),
            });
        }
        let receipt: LoaderReceipt = parse_loader_receipt(&output.stdout)?;
        receipt.validate_loaded(self.target_pid)?;
        if receipt.agent_mapping_mode != self.options.agent_mapping_mode {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: format!(
                    "loader 收据映射模式与本次配置不一致: expected={}, actual={}",
                    self.options.agent_mapping_mode.as_str(),
                    receipt.agent_mapping_mode.as_str()
                ),
            });
        }
        if receipt.agent_visibility_mode != self.options.agent_visibility_mode {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: format!(
                    "loader 收据可见性模式与本次配置不一致: expected={}, actual={}",
                    self.options.agent_visibility_mode.as_str(),
                    receipt.agent_visibility_mode.as_str()
                ),
            });
        }
        if Some(receipt.process_start_time) != self.expected_process_start_time {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: format!(
                    "loader 收据进程启动时刻与加载前证据不一致: expected={:?}, actual={}",
                    self.expected_process_start_time, receipt.process_start_time
                ),
            });
        }
        #[cfg(target_os = "windows")]
        if self.options.retain_agent {
            // 完整收据先持久化，后续日志或握手失败仍保留唯一卸载身份。
            self.save_resident(_before_load, Some(&receipt))?;
        }
        self.journal.record(
            "loader.complete",
            "ok",
            serde_json::to_value(&receipt).map_err(|source| RuntimeProbeError::Json {
                stage: "journal.loader",
                source,
            })?,
        )?;
        Ok(receipt)
    }

    /// 在 Agent 已排空且工作线程消失后执行冻结卸载，并严格核对成功收据。
    #[cfg(target_os = "windows")]
    fn run_unloader(
        &mut self,
        package_name: &str,
        module_name: &str,
        journal_errors: &mut Vec<String>,
    ) -> Result<UnloadReceipt, RuntimeProbeError> {
        let command: String = guarded_unloader_command(
            package_name,
            module_name,
            self.target_pid,
            &self.device_loader_path,
            &self.device_agent_path,
            &format!("{}/3", self.device_session_dir),
        );
        let output: RemoteShellOutput = self.bridge.root_status(&command, "unloader.execute")?;
        let unloader_entered: bool = non_empty_lines(&output.stdout)
            .iter()
            .any(|line: &String| line == UNLOADER_ENTERED_MARKER);
        let result: Result<UnloadReceipt, RuntimeProbeError> = validate_unloader_result(
            &output.stdout,
            output.exit_code,
            unloader_entered,
            self.target_pid,
        );
        if let Err(RuntimeProbeError::UnloaderRejected {
            exit_code,
            code,
            message,
            process_id,
        }) = &result
        {
            self.target_recovery_state = TargetRecoveryState::RestartRequired;
            if let Err(error) = self.journal.record(
                "unloader.failure_receipt",
                "error",
                json!({
                    "exit_code": exit_code,
                    "receipt": {
                        "status": "error",
                        "code": code,
                        "message": message,
                        "process_id": process_id,
                    },
                }),
            ) {
                eprintln!("卸载失败收据日志写入失败: {error}");
                journal_errors.push(error.to_string());
            }
        }
        result
    }

    /// 完成 RPC 排空、载体停驻、冻结卸载及宿主侧独立残留复核。
    #[cfg(target_os = "windows")]
    pub(super) fn unload_agent_gracefully(
        &mut self,
        authenticated: AuthenticatedAgent,
        journal_errors: &mut Vec<String>,
    ) -> Result<GracefulUnloadEvidence, RuntimeProbeError> {
        let AuthenticatedAgent {
            mut client,
            loader_receipt,
            ..
        } = authenticated;
        // 已进入明确卸载事务；恢复的连接与首次加载采用相同失败恢复策略。
        self.target_recovery_state = TargetRecoveryState::RestartRequired;
        if let Err(error) = self.journal.record(
            "session.shutdown.start",
            "ok",
            json!({
                "session_id": self.session_id,
                "process_id": self.target_pid,
            }),
        ) {
            eprintln!("安全卸载证据日志写入失败: {error}");
            journal_errors.push(error.to_string());
        }
        let prepared: ShutdownPreparedResult = client.shutdown(self.options.timeout_ms)?;
        drop(client);
        let prepared_payload =
            serde_json::to_value(&prepared).map_err(|source| RuntimeProbeError::Json {
                stage: "journal.shutdown_prepared",
                source,
            });
        match prepared_payload {
            Ok(payload) => {
                if let Err(error) = self
                    .journal
                    .record("session.shutdown.prepared", "ok", payload)
                {
                    eprintln!("安全卸载证据日志写入失败: {error}");
                    journal_errors.push(error.to_string());
                }
            }
            Err(error) => {
                eprintln!("安全卸载证据编码失败: {error}");
                journal_errors.push(error.to_string());
            }
        }

        if let Err(error) = self.journal.record(
            "session.shutdown.carrier_assigned",
            "ok",
            json!({
                "process_id": self.target_pid,
                "worker_tid": prepared.worker_tid,
                "worker_start_time": prepared.worker_start_time,
            }),
        ) {
            eprintln!("安全卸载证据日志写入失败: {error}");
            journal_errors.push(error.to_string());
        }

        self.write_unload(&loader_receipt, &prepared)?;
        self.deploy_unload()?;
        let package_name: String = self.profile.bootstrap().package_name().to_owned();
        let module_name: String = self.profile.bootstrap().module_name().to_owned();
        let unload_receipt: UnloadReceipt =
            self.run_unloader(&package_name, &module_name, journal_errors)?;
        let unload_payload =
            serde_json::to_value(&unload_receipt).map_err(|source| RuntimeProbeError::Json {
                stage: "journal.unloader",
                source,
            });
        match unload_payload {
            Ok(payload) => {
                if let Err(error) = self.journal.record("unloader.complete", "ok", payload) {
                    eprintln!("安全卸载证据日志写入失败: {error}");
                    journal_errors.push(error.to_string());
                }
            }
            Err(error) => {
                eprintln!("安全卸载证据编码失败: {error}");
                journal_errors.push(error.to_string());
            }
        }
        let mapping_evidence: PostUnloadMappingEvidence = self.bridge.verify_agent_unloaded(
            self.target_pid,
            loader_receipt.process_start_time,
            loader_receipt.agent_base.get(),
            loader_receipt.agent_load_size,
            &loader_receipt.agent_mapping_name,
        )?;

        let process: ProcessEvidence = self.bridge.collect_process_evidence(self.target_pid)?;
        validate_preserved_process_identity(
            &process,
            self.target_pid,
            Some(loader_receipt.process_start_time),
            "shutdown.verify_process_identity",
        )?;
        validate_graceful_unload_evidence(&process)?;
        self.agent_unloaded = true;
        self.target_recovery_state = TargetRecoveryState::PreserveOriginal;
        let evidence = GracefulUnloadEvidence {
            process_id: self.target_pid,
            process_start_time: loader_receipt.process_start_time,
            worker_tid: prepared.worker_tid,
            worker_start_time: prepared.worker_start_time,
            agent_mapping_name: loader_receipt.agent_mapping_name,
            inert_anonymous_overlap_count: mapping_evidence.inert_anonymous_overlap_count,
            reused_address_overlap_count: mapping_evidence.reused_address_overlap_count,
        };
        if let Err(error) = self.journal.record(
            "session.shutdown.unloaded",
            "ok",
            json!({
                "process_id": evidence.process_id,
                "process_start_time": evidence.process_start_time,
                "worker_tid": evidence.worker_tid,
                "worker_start_time": evidence.worker_start_time,
                "agent_mapping_name": evidence.agent_mapping_name,
                "inert_anonymous_overlap_count": evidence.inert_anonymous_overlap_count,
                "reused_address_overlap_count": evidence.reused_address_overlap_count,
                "tracer_pid": process.tracer_pid,
            }),
        ) {
            eprintln!("安全卸载证据日志写入失败: {error}");
            journal_errors.push(error.to_string());
        }
        Ok(evidence)
    }

    /// 为当前会话申请动态本地端口，并记录端口供定向回收。
    fn create_forward(&mut self) -> Result<u16, RuntimeProbeError> {
        let remote_endpoint: String = self.remote_endpoint.clone();
        self.forward_creation_attempted = true;
        let output: String = match self.bridge.adb_checked(
            &[
                "forward".to_owned(),
                "tcp:0".to_owned(),
                remote_endpoint.clone(),
            ],
            "forward.create",
        ) {
            Ok(output) => output,
            Err(source) => {
                self.try_recover_forward_owner(&remote_endpoint);
                return Err(source);
            }
        };
        let port: u16 = match output.trim().parse() {
            Ok(port) if port != 0 => port,
            _ => {
                self.try_recover_forward_owner(&remote_endpoint);
                return Err(RuntimeProbeError::InvalidOutput {
                    stage: "forward.create",
                    message: format!("ADB 未返回有效的动态本地端口: {output:?}"),
                });
            }
        };
        self.forward_port = Some(port);
        Ok(port)
    }

    /// 创建命令结果不可信时按本会话远端身份恢复清理所有权。
    fn try_recover_forward_owner(&mut self, remote_endpoint: &str) {
        let recovery: Result<Option<u16>, RuntimeProbeError> = self
            .bridge
            .forward_list()
            .and_then(|forwards| find_owned_forward_port(&forwards, remote_endpoint));
        match recovery {
            Ok(Some(port)) => {
                self.forward_port = Some(port);
                let _ = self.journal.record(
                    "forward.owner_recovered",
                    "ok",
                    json!({"forward_port": port, "remote_endpoint": remote_endpoint}),
                );
            }
            Ok(None) => {}
            Err(source) => {
                let message: String = journal_error_summary(&source);
                let _ = self.journal.record(
                    "forward.owner_recovery.failure",
                    "error",
                    json!({"message": message}),
                );
            }
        }
    }
}

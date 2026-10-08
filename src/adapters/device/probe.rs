//! 模拟器 专用运行态的 Rust 宿主部署、真机探针和幂等清理。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use suzushiro_content_digest::sha256_compact_json;
use thiserror::Error;

use super::bootstrap::BootstrapEncodingError;
use super::capture::equipment_sample::{
    EquipmentSampleError, EquipmentSampleEvidence, write_equipment_sample,
};
use super::capture::full_state::{
    FullStateCaptureError, FullStateCaptureEvidence, validate_external_capture_root,
};
use super::capture::ship_catalog::ShipCatalogCaptureError;
use super::profile::RuntimeProfileError;
use super::reading::equipment::EquipmentReadError;
use super::reading::ship_catalog::ShipCatalogReadError;
use super::runtime::{
    CapabilitiesResult, HealthResult, LiveProtocolProbeResult, RuntimeClientError,
    RuntimeProtocolError,
};
use super::session::{SessionGenerationError, SessionId};
use crate::adapters::json_artifact::{JsonArtifactError, PublishedJson, write_new_pretty_json};
use crate::adapters::settings::{AgentMappingMode, AgentVisibilityMode};
use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::application::AppError;
use suzushiro_host_command::NativeCommandError;
#[cfg(target_os = "windows")]
use suzushiro_host_command::NativeCommandPolicy;

mod cleanup;
mod device_bridge;
mod journal;
mod process_evidence;
pub(crate) mod readiness;
mod receipts;
mod runner;
#[cfg(target_os = "windows")]
mod runtime_session;

pub use cleanup::CleanupEvidence;
use cleanup::CleanupResult;
#[cfg(test)]
use cleanup::{
    OriginalProcessPresence, ProcessRecoveryAction, TargetRecoveryState,
    cleanup_device_unreachable, cleanup_journal_details, process_recovery_action,
    reconcile_owned_forward, reconcile_preserved_process_evidence, record_cleanup_completion,
    restart_after_process_stop,
};
#[cfg(all(test, target_os = "windows"))]
use device_bridge::DeviceBridge;
#[cfg(test)]
use device_bridge::parse_boot_id;
use journal::journal_error_summary;
#[cfg(test)]
use process_evidence::PostUnloadMappingEvidence;
pub use process_evidence::ProcessEvidence;
#[cfg(test)]
use process_evidence::{
    empty_process_evidence, module_map_probe_command, parse_proc_stat_start_time, parse_single_pid,
    thread_name_probe_command, validate_post_unload_mappings, validate_preserved_process_identity,
};
#[cfg(test)]
use readiness::{
    is_retryable_bag_readiness_error, is_retryable_owned_state_readiness_error,
    is_retryable_ship_details_readiness_error, summarize_read_errors, summarize_ship_growth,
    wait_for_main_thread_with,
};
pub use receipts::LoaderReceipt;
#[cfg(test)]
use receipts::validate_unloader_result;
#[cfg(test)]
use receipts::{
    MAXIMUM_FAILURE_RECEIPT_MESSAGE_BYTES, loader_failure_decision, loader_failure_journal_details,
    loader_failure_requires_restart, parse_loader_receipt, parse_unload_receipt,
};
#[cfg(target_os = "windows")]
use runner::AuthenticatedAgent;
use runner::{ProbeCoreResult, ProductionSession};
#[cfg(all(test, target_os = "windows"))]
pub(crate) use runtime_session::GracefulUnloadEvidence;
#[cfg(target_os = "windows")]
pub(crate) use runtime_session::{
    RuntimeSession, RuntimeSessionShutdownEvidence, RuntimeShutdownMethodEvidence,
};
#[cfg(all(test, target_os = "windows"))]
use runtime_session::{attempt_runtime_session_cleanup, merge_runtime_session_journal_errors};

pub(crate) const PROFILE_RELATIVE_PATH: &str = "runtime/resources/profiles/default.json";
const LOADER_RELATIVE_PATH: &str = "runtime/inject/azlw-loader-x86_64";
const AGENT_RELATIVE_PATH: &str = "runtime/inject/libazlw-agent-x86_64.so";
pub(crate) const DEFAULT_TIMEOUT_MS: u32 = 10_000;
const DEFAULT_MAX_ITEMS: u32 = 2_000;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const LOADER_ENTERED_MARKER: &str = "AZLW_LOADER_ENTERED";
#[cfg(any(target_os = "windows", test))]
const UNLOADER_ENTERED_MARKER: &str = "AZLW_UNLOADER_ENTERED";
const RUNTIME_PROBE_SCHEMA_VERSION: u32 = 5;
const MAXIMUM_RUNTIME_RECEIPT_BYTES: u64 = 4 * 1024 * 1024;

/// 真机运行态探针需要的已发现 模拟器 目标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeProbeOptions {
    tool_root: PathBuf,
    manager_executable: String,
    adb_executable: String,
    #[cfg(target_os = "windows")]
    adb_server_port: Option<u16>,
    #[cfg(target_os = "windows")]
    adb_process_policy: Option<NativeCommandPolicy>,
    vm_index: String,
    serial: String,
    connect_timeout: Duration,
    startup_timeout: Duration,
    agent_mapping_mode: AgentMappingMode,
    agent_visibility_mode: AgentVisibilityMode,
    timeout_ms: u32,
    max_items: u32,
    full_state_capture_root: Option<PathBuf>,
    retain_agent: bool,
}

impl RuntimeProbeOptions {
    /// 关闭宿主连接时保留设备代理，供后续操作重新认证。
    pub fn with_retain_agent(mut self, retain: bool) -> Self {
        self.retain_agent = retain;
        self
    }

    /// 构造只允许单个回环 模拟器 实例的探针配置。
    pub fn new(
        tool_root: impl Into<PathBuf>,
        manager_executable: impl Into<String>,
        adb_executable: impl Into<String>,
        vm_index: impl Into<String>,
        serial: impl Into<String>,
    ) -> Result<Self, RuntimeProbeError> {
        let tool_root: PathBuf = tool_root.into();
        let manager_executable: String = manager_executable.into();
        let adb_executable: String = adb_executable.into();
        let vm_index: String = vm_index.into();
        let serial: String = serial.into();

        if !tool_root.is_dir() {
            return Err(RuntimeProbeError::InvalidOption {
                field: "tool_root",
                message: "必须是现有目录".to_owned(),
            });
        }
        validate_executable_option("manager_executable", &manager_executable)?;
        validate_executable_option("adb_executable", &adb_executable)?;
        if vm_index.is_empty()
            || vm_index.len() > 8
            || !vm_index.bytes().all(|byte: u8| byte.is_ascii_digit())
        {
            return Err(RuntimeProbeError::InvalidOption {
                field: "vm_index",
                message: "必须是单个非负十进制实例索引".to_owned(),
            });
        }
        let address: SocketAddr = serial
            .parse()
            .map_err(|_| RuntimeProbeError::InvalidOption {
                field: "serial",
                message: "必须是 HOST:PORT 格式的回环地址".to_owned(),
            })?;
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(RuntimeProbeError::InvalidOption {
                field: "serial",
                message: "只读运行态只允许非零端口的回环 模拟器 地址".to_owned(),
            });
        }

        Ok(Self {
            tool_root,
            manager_executable,
            adb_executable,
            #[cfg(target_os = "windows")]
            adb_server_port: None,
            #[cfg(target_os = "windows")]
            adb_process_policy: None,
            vm_index,
            serial,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            agent_mapping_mode: AgentMappingMode::Memfd,
            agent_visibility_mode: AgentVisibilityMode::Normal,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_items: DEFAULT_MAX_ITEMS,
            full_state_capture_root: None,
            retain_agent: false,
        })
    }

    /// 设置 loader 与 RPC 的设备端相对超时。
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Result<Self, RuntimeProbeError> {
        if !(1..=30_000).contains(&timeout_ms) {
            return Err(RuntimeProbeError::InvalidOption {
                field: "timeout_ms",
                message: "只允许 1 至 30000".to_owned(),
            });
        }
        self.timeout_ms = timeout_ms;
        Ok(self)
    }

    /// 设置背包快照的明确条目上限。
    pub fn with_max_items(mut self, max_items: u32) -> Result<Self, RuntimeProbeError> {
        if !(1..=2_000).contains(&max_items) {
            return Err(RuntimeProbeError::InvalidOption {
                field: "max_items",
                message: "只允许 1 至 2000".to_owned(),
            });
        }
        self.max_items = max_items;
        Ok(self)
    }

    /// 显式启用发布目录外的完整原始状态采集；普通运行保持关闭。
    pub fn with_full_state_capture_root(
        mut self,
        capture_root: impl Into<PathBuf>,
    ) -> Result<Self, RuntimeProbeError> {
        let capture_root: PathBuf = capture_root.into();
        self.full_state_capture_root = Some(validate_external_capture_root(
            &self.tool_root,
            &capture_root,
        )?);
        Ok(self)
    }

    /// 将全部 ADB 客户端命令绑定到调用方持有的非默认服务端口。
    #[cfg(target_os = "windows")]
    pub(crate) fn with_adb_server_port(
        mut self,
        adb_server_port: u16,
    ) -> Result<Self, RuntimeProbeError> {
        if adb_server_port == 0 {
            return Err(RuntimeProbeError::InvalidOption {
                field: "adb_server_port",
                message: "必须是非零端口".to_owned(),
            });
        }
        self.adb_server_port = Some(adb_server_port);
        Ok(self)
    }

    /// 将全部随包 ADB 客户端绑定到服务端相同的最小宿主进程环境。
    #[cfg(target_os = "windows")]
    pub(crate) fn with_adb_process_policy(
        mut self,
        adb_process_policy: NativeCommandPolicy,
    ) -> Self {
        self.adb_process_policy = Some(adb_process_policy);
        self
    }

    /// 用已经解析的目标、服务端口和运行策略一次建立会话配置。
    #[cfg(target_os = "windows")]
    pub(crate) fn from_resolved(
        tool_root: &Path,
        resolved: ResolvedRuntimeTarget,
    ) -> Result<Self, RuntimeProbeError> {
        let mut options = Self::new(
            tool_root,
            resolved.manager_executable,
            resolved.adb_executable,
            resolved.vm_index,
            resolved.serial,
        )?;
        options.connect_timeout =
            timeout_from_seconds("connect_timeout_seconds", resolved.connect_timeout_seconds)?;
        options.startup_timeout =
            timeout_from_seconds("startup_timeout_seconds", resolved.startup_timeout_seconds)?;
        options.agent_mapping_mode = resolved.agent_mapping_mode;
        options.agent_visibility_mode = resolved.agent_visibility_mode;
        options.retain_agent = resolved.retain_agent;
        options = options.with_timeout_ms(resolved.timeout_ms)?;
        options = options.with_max_items(resolved.max_items)?;
        options = options.with_adb_server_port(resolved.adb_server_port)?;
        options = options.with_adb_process_policy(resolved.adb_process_policy);
        if let Some(capture_root) = resolved.full_state_capture_root {
            options = options.with_full_state_capture_root(capture_root)?;
        }
        Ok(options)
    }
}

/// 目标发现完成后的运行配置。发现阶段的提示参数不再继续传递。
#[cfg(target_os = "windows")]
pub(crate) struct ResolvedRuntimeTarget {
    pub(crate) manager_executable: String,
    pub(crate) adb_executable: String,
    pub(crate) vm_index: String,
    pub(crate) serial: String,
    pub(crate) adb_server_port: u16,
    pub(crate) adb_process_policy: NativeCommandPolicy,
    pub(crate) connect_timeout_seconds: u32,
    pub(crate) startup_timeout_seconds: u32,
    pub(crate) agent_mapping_mode: AgentMappingMode,
    pub(crate) agent_visibility_mode: AgentVisibilityMode,
    pub(crate) retain_agent: bool,
    pub(crate) timeout_ms: u32,
    pub(crate) max_items: u32,
    pub(crate) full_state_capture_root: Option<PathBuf>,
}

/// 将设置文件的秒级等待转换成有上限的运行时期限。
#[cfg(target_os = "windows")]
fn timeout_from_seconds(field: &'static str, seconds: u32) -> Result<Duration, RuntimeProbeError> {
    if !(1..=3_600).contains(&seconds) {
        return Err(RuntimeProbeError::InvalidOption {
            field,
            message: format!("只允许 1 至 3600 秒，实际为 {seconds}"),
        });
    }
    Ok(Duration::from_secs(u64::from(seconds)))
}

/// 一次完整真机验证的结构化结果和收据位置。
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeProbeOutcome {
    /// 已通过全部断言的结构化报告。
    pub report: RuntimeProbeReport,
    /// 工具目录内的原子 JSON 收据。
    pub receipt_path: PathBuf,
    /// 工具目录内逐阶段 JSONL 日志。
    pub journal_path: PathBuf,
    /// 工具目录内包含原始与规范化装备数据的维护样本。
    pub equipment_sample_path: PathBuf,
    /// 仅在显式启用时返回发布目录外的完整原始状态证据，不包含正文副本。
    pub full_state_capture: Option<FullStateCaptureEvidence>,
}

/// 模拟器 专用运行态的可重复真机证据。
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeProbeReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub session_id: SessionId,
    pub profile_id: String,
    pub serial: String,
    pub process_id_before_load: u32,
    pub process_id_after_cleanup: u32,
    pub loader_sha256: String,
    pub agent_sha256: String,
    pub loader_receipt: LoaderReceipt,
    pub forward_port: u16,
    pub handshake_attempts: u32,
    pub health: HealthResult,
    pub capabilities: CapabilitiesResult,
    pub bag_readiness_retries: u32,
    pub snapshot_hashes: Vec<String>,
    pub snapshot_count: u32,
    pub snapshots_complete: bool,
    /// 首次完整运行态快照成功前发生的同会话重试次数。
    pub owned_state_readiness_retries: u32,
    /// 连续三次完整运行态快照的规范 JSON SHA-256。
    pub owned_state_hashes: Vec<String>,
    /// 首次完整舰船详情成功前发生的同会话重试次数。
    pub ship_details_readiness_retries: u32,
    /// 连续三次舰船详情快照的规范 JSON SHA-256。
    pub ship_detail_hashes: Vec<String>,
    /// 连续三组运行态与详情严格关联后的规范名册 SHA-256。
    pub ship_roster_hashes: Vec<String>,
    /// 本次读取通过完整关联和领域不变量校验后的游戏状态 SHA-256。
    pub game_state_content_sha256: String,
    /// 首次完整运行态快照中的舰船数。
    pub dock_count: u32,
    /// 首次完整舰船详情快照中的舰船数。
    pub ship_detail_count: u32,
    /// 不保存任何标识的舰船养成与自身技能范围摘要。
    pub ship_growth_summary: ShipGrowthSummary,
    /// 首次完整运行态快照中的仓库装备配置数。
    pub warehouse_count: u32,
    /// 首次完整运行态快照中的背包物品数。
    pub owned_state_bag_count: u32,
    /// 连续三次运行态快照是否全部完整。
    pub owned_state_complete: bool,
    /// 独立维护样本的相对路径、数量与内容身份，不包含原始正文。
    pub equipment_sample: EquipmentSampleEvidence,
    pub protocol_failure_probes: LiveProbeEvidence,
    pub before_load: ProcessEvidence,
    pub after_load: ProcessEvidence,
    pub after_cleanup: ProcessEvidence,
    pub cleanup: CleanupEvidence,
}

/// 一组非负整数在当前完整快照中的闭区间。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct UnsignedRangeSummary {
    pub minimum: u64,
    pub maximum: u64,
}

impl UnsignedRangeSummary {
    /// 把单值并入已有闭区间；首次出现时同时建立上下界。
    fn include(range: &mut Option<Self>, value: u64) {
        match range {
            Some(range) => {
                range.minimum = range.minimum.min(value);
                range.maximum = range.maximum.max(value);
            }
            None => {
                *range = Some(Self {
                    minimum: value,
                    maximum: value,
                });
            }
        }
    }
}

/// 去除舰船、配置和技能标识后的当前账号养成数据证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShipGrowthSummary {
    pub distinct_config_count: u32,
    pub skill_count: u64,
    pub ships_without_skills: u32,
    pub level: Option<UnsignedRangeSummary>,
    pub experience_in_level: Option<UnsignedRangeSummary>,
    pub intimacy_raw: Option<UnsignedRangeSummary>,
    pub energy: Option<UnsignedRangeSummary>,
    pub proficiency: Option<UnsignedRangeSummary>,
    pub skill_level: Option<UnsignedRangeSummary>,
    pub skill_experience: Option<UnsignedRangeSummary>,
}

/// 协议失败路径没有被宿主默认值掩盖的证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LiveProbeEvidence {
    pub timeout_code: String,
    pub unsupported_code: String,
    pub duplicate_code: String,
    pub oversized_frame_closed_connection: bool,
}

/// Rust 宿主、模拟器 命令、原生加载或 RPC 任一阶段失败。
#[derive(Debug, Error)]
pub enum RuntimeProbeError {
    /// 用户提供的发现结果不满足单实例回环目标边界。
    #[error("探针选项 {field} 无效: {message}")]
    InvalidOption {
        field: &'static str,
        message: String,
    },
    /// 固定工具内路径不存在、越界或不是普通文件。
    #[error("工具资产 {path} 无效: {message}")]
    InvalidToolAsset { path: PathBuf, message: String },
    /// 文件系统操作失败。
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 原生宿主命令无法启动、等待或解码。
    #[error("主机命令阶段 {stage} 失败: {message}")]
    HostCommand {
        stage: &'static str,
        message: String,
    },
    /// 模拟器 root shell 返回非零状态。
    #[error("设备命令阶段 {stage} 返回 {exit_code}: {output}")]
    DeviceCommand {
        stage: &'static str,
        exit_code: i32,
        output: String,
    },
    /// 加载器已进入并返回可验证的结构化失败收据。
    #[error(
        "加载器拒绝本次加载: code={code}, process_id={process_id}, exit_code={exit_code}, target_state={target_state}, message={message}"
    )]
    LoaderRejected {
        exit_code: i32,
        code: String,
        message: String,
        process_id: u32,
        target_state: String,
    },
    /// 卸载器已进入并返回可验证的结构化失败收据。
    #[error(
        "卸载器拒绝本次卸载: code={code}, message={message}, process_id={process_id}, exit_code={exit_code}"
    )]
    UnloaderRejected {
        exit_code: i32,
        code: String,
        message: String,
        process_id: u32,
    },
    /// 外部命令输出不满足固定契约。
    #[error("阶段 {stage} 的输出无效: {message}")]
    InvalidOutput {
        stage: &'static str,
        message: String,
    },
    /// profile 无法形成固定目标身份。
    #[error(transparent)]
    Profile(#[from] RuntimeProfileError),
    /// 会话随机源不可用。
    #[error(transparent)]
    Session(#[from] SessionGenerationError),
    /// 启动或卸载会话文件编码失败。
    #[error(transparent)]
    Bootstrap(#[from] BootstrapEncodingError),
    /// 宿主协议或 agent RPC 失败。
    #[error(transparent)]
    RuntimeClient(#[from] RuntimeClientError),
    /// 宿主在建立预期身份时发现非法值。
    #[error(transparent)]
    RuntimeProtocol(#[from] RuntimeProtocolError),
    /// 两份只读快照无法严格关联为同一时刻的舰船名册。
    #[error(transparent)]
    ShipMapping(#[from] super::mapping::ship::ShipMappingError),
    /// 完整装备目录读取、详情关联或原始证据编码失败。
    #[error(transparent)]
    EquipmentRead(#[from] EquipmentReadError),
    /// 固定白名单舰船静态目录未能完整分页、校验或计算内容身份。
    #[error(transparent)]
    ShipCatalogRead(#[from] ShipCatalogReadError),
    /// 完整原始状态证据目录或发布过程不符合黄金验证边界。
    #[error("完整状态黄金证据失败: {source}")]
    FullStateCapture {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 完整舰船静态目录未能发布到显式外部证据目录。
    #[error("舰船静态目录证据失败: {source}")]
    ShipCatalogCapture {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 应用读取端口无法生成一致的完整游戏状态。
    #[error(transparent)]
    GameStateRead(#[from] AppError),
    /// JSON 日志、收据或语义哈希编码失败。
    #[error("JSON 阶段 {stage} 失败: {source}")]
    Json {
        stage: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// 工具根目录或其受控相对路径不满足零越界约束。
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
    /// JSON 证据文件未能完成有界编码、同步、发布或失败清理。
    #[error("JSON 证据文件发布失败: {source}")]
    JsonArtifact {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 主验证失败，已保留日志并尝试清理。
    #[error("{source}; 验证日志: {journal_path}; 清理错误: {cleanup_error:?}")]
    ProbeFailed {
        #[source]
        source: Box<RuntimeProbeError>,
        journal_path: PathBuf,
        cleanup_error: Option<String>,
    },
    /// 多个清理动作失败，所有错误均被保留。
    #[error("清理未完全成功: {messages}")]
    Cleanup { messages: String },
}

impl From<NativeCommandError> for RuntimeProbeError {
    /// 将内部命令错误恢复为探针原有的公开错误分类。
    fn from(error: NativeCommandError) -> Self {
        match error {
            NativeCommandError::Failed { stage, message } => Self::HostCommand { stage, message },
            #[cfg(not(target_os = "windows"))]
            NativeCommandError::Json { stage, source } => Self::Json { stage, source },
        }
    }
}

impl From<JsonArtifactError> for RuntimeProbeError {
    fn from(source: JsonArtifactError) -> Self {
        Self::JsonArtifact {
            source: Box::new(source),
        }
    }
}

impl From<FullStateCaptureError> for RuntimeProbeError {
    fn from(source: FullStateCaptureError) -> Self {
        Self::FullStateCapture {
            source: Box::new(source),
        }
    }
}

impl From<ShipCatalogCaptureError> for RuntimeProbeError {
    fn from(source: ShipCatalogCaptureError) -> Self {
        Self::ShipCatalogCapture {
            source: Box::new(source),
        }
    }
}

impl From<EquipmentSampleError> for RuntimeProbeError {
    fn from(source: EquipmentSampleError) -> Self {
        match source {
            EquipmentSampleError::Artifact(source) => Self::from(source),
            EquipmentSampleError::Sequence(source) => Self::Io {
                stage: "equipment.sample_sequence",
                path: PathBuf::from("data/history"),
                source,
            },
            EquipmentSampleError::CountOverflow { field, actual } => Self::InvalidOutput {
                stage: "equipment.sample_counts",
                message: format!("装备样本字段 {field} 的数量 {actual} 超出 u32 范围"),
            },
        }
    }
}

/// 执行一次部署、只读 RPC、失败路径和清理的完整真机验证。
pub fn run_runtime_probe(
    options: RuntimeProbeOptions,
) -> Result<RuntimeProbeOutcome, RuntimeProbeError> {
    let mut runner: ProductionSession = ProductionSession::new(options, None)?;
    let journal_path: PathBuf = runner.journal.path.clone();
    let execution: Result<ProbeCoreResult, RuntimeProbeError> = (|| {
        runner.journal.record(
            "probe.start",
            "ok",
            json!({"message": "开始只读运行态真机验证"}),
        )?;
        runner::audit::execute(&mut runner)
    })();
    let core: ProbeCoreResult = match execution {
        Ok(core) => core,
        Err(source) => {
            let message = journal_error_summary(&source);
            let _ = runner
                .journal
                .record("probe.failure", "error", json!({"message": message}));
            return Err(probe_failed_after_cleanup(source, journal_path, || {
                runner.cleanup()
            }));
        }
    };

    let cleanup: CleanupResult =
        runner
            .cleanup()
            .map_err(|source| RuntimeProbeError::ProbeFailed {
                source: Box::new(source),
                journal_path: journal_path.clone(),
                cleanup_error: None,
            })?;
    let equipment_sample: EquipmentSampleEvidence =
        match write_equipment_sample(&runner.tool_root, runner.session_id, &core.equipment) {
            Ok(evidence) => evidence,
            Err(source) => {
                let source = RuntimeProbeError::from(source);
                let message = journal_error_summary(&source);
                let _ =
                    runner
                        .journal
                        .record("equipment.sample", "error", json!({"message": message}));
                return Err(RuntimeProbeError::ProbeFailed {
                    source: Box::new(source),
                    journal_path,
                    cleanup_error: None,
                });
            }
        };
    let equipment_sample_path = runner
        .tool_root
        .as_path()
        .join(Path::new(&equipment_sample.relative_path));
    let equipment_sample_relative = PathBuf::from(&equipment_sample.relative_path);
    let full_state_capture: Option<FullStateCaptureEvidence> = core.full_state_capture;
    if let Err(source) = runner.journal.record(
        "equipment.sample",
        "ok",
        json!({
            "relative_path": equipment_sample.relative_path,
            "size_bytes": equipment_sample.size_bytes,
            "file_sha256": equipment_sample.file_sha256,
            "catalog_content_sha256": equipment_sample.catalog_content_sha256,
            "raw_content_sha256": equipment_sample.raw_content_sha256,
            "config_count": equipment_sample.config_count,
            "recipe_count": equipment_sample.recipe_count,
            "reference_count": equipment_sample.reference_count,
            "weapon_count": equipment_sample.weapon_count,
            "skill_count": equipment_sample.skill_count,
        }),
    ) {
        return Err(probe_finalization_failed(
            source,
            journal_path,
            &runner.tool_root,
            &[("装备维护样本", &equipment_sample_relative)],
        ));
    }
    let report = RuntimeProbeReport {
        schema_version: RUNTIME_PROBE_SCHEMA_VERSION,
        status: "passed",
        session_id: runner.session_id,
        profile_id: runner.profile.profile_id().to_owned(),
        serial: runner.options.serial.clone(),
        process_id_before_load: runner.target_pid,
        process_id_after_cleanup: cleanup.evidence.process_id,
        loader_sha256: runner.loader_sha256.clone(),
        agent_sha256: runner.agent_sha256.clone(),
        loader_receipt: core.loader_receipt,
        forward_port: core.forward_port,
        handshake_attempts: core.handshake_attempts,
        health: core.health,
        capabilities: core.capabilities,
        bag_readiness_retries: core.bag_readiness_retries,
        snapshot_hashes: core.snapshot_hashes,
        snapshot_count: core.snapshot_count,
        snapshots_complete: core.snapshots_complete,
        owned_state_readiness_retries: core.owned_state_readiness_retries,
        owned_state_hashes: core.owned_state_hashes,
        ship_details_readiness_retries: core.ship_details_readiness_retries,
        ship_detail_hashes: core.ship_detail_hashes,
        ship_roster_hashes: core.ship_roster_hashes,
        game_state_content_sha256: core.game_state_content_sha256,
        dock_count: core.dock_count,
        ship_detail_count: core.ship_detail_count,
        ship_growth_summary: core.ship_growth_summary,
        warehouse_count: core.warehouse_count,
        owned_state_bag_count: core.owned_state_bag_count,
        owned_state_complete: core.owned_state_complete,
        equipment_sample,
        protocol_failure_probes: LiveProbeEvidence::from(core.protocol_failure_probes),
        before_load: core.before_load,
        after_load: core.after_load,
        after_cleanup: cleanup.evidence,
        cleanup: cleanup.cleanup,
    };
    let (receipt_path, receipt_relative) = match write_receipt(&runner.tool_root, &report) {
        Ok(receipt_path) => receipt_path,
        Err(source) => {
            return Err(probe_finalization_failed(
                source,
                journal_path,
                &runner.tool_root,
                &[("装备维护样本", &equipment_sample_relative)],
            ));
        }
    };
    if let Err(source) = runner.journal.record(
        "probe.complete",
        "ok",
        json!({"receipt_path": receipt_path, "status": "passed"}),
    ) {
        return Err(probe_finalization_failed(
            source,
            journal_path,
            &runner.tool_root,
            &[
                ("运行态回执", &receipt_relative),
                ("装备维护样本", &equipment_sample_relative),
            ],
        ));
    }

    Ok(RuntimeProbeOutcome {
        report,
        receipt_path,
        journal_path,
        equipment_sample_path,
        full_state_capture,
    })
}

impl From<LiveProtocolProbeResult> for LiveProbeEvidence {
    /// 将客户端内部结果转换为可写入真机收据的公开证据。
    fn from(value: LiveProtocolProbeResult) -> Self {
        Self {
            timeout_code: value.timeout_code,
            unsupported_code: value.unsupported_code,
            duplicate_code: value.duplicate_code,
            oversized_frame_closed_connection: value.oversized_frame_closed_connection,
        }
    }
}

/// 以新建语义和最小权限持久化一次性会话文件，再同步到存储介质。
fn write_secret_file(
    path: &Path,
    bytes: &[u8],
    create_stage: &'static str,
    write_stage: &'static str,
) -> Result<(), RuntimeProbeError> {
    let mut options: OpenOptions = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options.open(path).map_err(|source| RuntimeProbeError::Io {
        stage: create_stage,
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| RuntimeProbeError::Io {
            stage: write_stage,
            path: path.to_path_buf(),
            source,
        })
}

/// 对规范序列化后的结构计算语义摘要，用于连续快照一致性比较。
pub(super) fn sha256_json<T: Serialize>(
    value: &T,
    stage: &'static str,
) -> Result<String, RuntimeProbeError> {
    sha256_compact_json(value).map_err(|source| RuntimeProbeError::Json { stage, source })
}

/// 先写入同目录临时文件并同步，再原子发布最终 JSON 收据。
fn write_receipt(
    tool_root: &ToolRoot,
    report: &RuntimeProbeReport,
) -> Result<(PathBuf, PathBuf), RuntimeProbeError> {
    let map_sequence_error = |source| RuntimeProbeError::Io {
        stage: "runtime.receipt.sequence",
        path: tool_root.as_path().join("data/history"),
        source,
    };
    let directory = crate::adapters::numbered_files::NumberedDirectory::open(
        tool_root,
        Path::new("data/history"),
    )
    .map_err(map_sequence_error)?;
    let target_relative = directory
        .next_path("probe", "json", true)
        .map_err(map_sequence_error)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let published: PublishedJson = write_new_pretty_json(
        tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_RUNTIME_RECEIPT_BYTES,
        report,
    )?;
    Ok((published.into_path(), target_relative))
}

/// 合并只读验证和安全卸载结果，确保原始失败与卸载诊断都不会丢失。
#[cfg(any(target_os = "windows", test))]
pub(super) fn finish_probe_execution_after_unload<T, U>(
    execution: Result<T, RuntimeProbeError>,
    unload: Result<U, RuntimeProbeError>,
    journal_path: PathBuf,
    mut journal_errors: Vec<String>,
) -> Result<T, RuntimeProbeError> {
    match execution {
        Ok(value) => match unload {
            Ok(_) if journal_errors.is_empty() => Ok(value),
            Ok(_) => Err(RuntimeProbeError::Cleanup {
                messages: format!("安全卸载证据日志失败: {}", journal_errors.join(" | ")),
            }),
            Err(source) if journal_errors.is_empty() => Err(source),
            Err(source) => Err(RuntimeProbeError::ProbeFailed {
                source: Box::new(source),
                journal_path,
                cleanup_error: Some(format!(
                    "安全卸载证据日志失败: {}",
                    journal_errors.join(" | ")
                )),
            }),
        },
        Err(source) => {
            if let Err(unload_error) = unload {
                journal_errors.insert(0, format!("安全卸载失败: {unload_error}"));
            }
            if journal_errors.is_empty() {
                Err(source)
            } else {
                Err(RuntimeProbeError::ProbeFailed {
                    source: Box::new(source),
                    journal_path,
                    cleanup_error: Some(journal_errors.join(" | ")),
                })
            }
        }
    }
}

/// 将主操作失败与随后发生的清理失败保存在同一条可追溯错误链中。
fn probe_failed_after_cleanup<T>(
    source: RuntimeProbeError,
    journal_path: PathBuf,
    cleanup: impl FnOnce() -> Result<T, RuntimeProbeError>,
) -> RuntimeProbeError {
    let cleanup_error: Option<String> = cleanup().err().map(|error| error.to_string());
    RuntimeProbeError::ProbeFailed {
        source: Box::new(source),
        journal_path,
        cleanup_error,
    }
}

/// 固定按安全卸载、资源清理的顺序执行，并把两者的错误附加到原始失败之后。
#[cfg(any(target_os = "windows", test))]
fn probe_failed_after_ordered_cleanup<State, GracefulValue, CleanupValue>(
    source: RuntimeProbeError,
    journal_path: PathBuf,
    state: &mut State,
    graceful: impl FnOnce(&mut State) -> Result<GracefulValue, String>,
    cleanup: impl FnOnce(&mut State) -> Result<CleanupValue, String>,
) -> RuntimeProbeError {
    let mut cleanup_errors: Vec<String> = Vec::new();
    if let Err(error) = graceful(state) {
        cleanup_errors.push(error);
    }
    if let Err(error) = cleanup(state) {
        cleanup_errors.push(error);
    }
    RuntimeProbeError::ProbeFailed {
        source: Box::new(source),
        journal_path,
        cleanup_error: (!cleanup_errors.is_empty()).then(|| cleanup_errors.join(" | ")),
    }
}

/// 记录持久会话启动的首个失败；日志本身失败时不能覆盖真实原因。
#[cfg(target_os = "windows")]
fn record_session_start_failure(runner: &mut ProductionSession, source: &RuntimeProbeError) {
    let message: String = journal_error_summary(source);
    if let Err(journal_error) =
        runner
            .journal
            .record("session.failure", "error", json!({"message": message}))
    {
        eprintln!("持久运行态失败日志写入失败: {journal_error}");
    }
}

/// 握手前启动失败没有可用 RPC，只能执行保守的资源清理和重启兜底。
#[cfg(target_os = "windows")]
fn session_start_failed(
    runner: &mut ProductionSession,
    source: RuntimeProbeError,
    journal_path: PathBuf,
) -> RuntimeProbeError {
    record_session_start_failure(runner, &source);
    probe_failed_after_cleanup(source, journal_path, || runner.cleanup())
}

/// 握手后启动失败优先卸载 agent；只有卸载未完成时资源清理才会重启游戏。
#[cfg(target_os = "windows")]
fn authenticated_session_start_failed(
    runner: &mut ProductionSession,
    authenticated: AuthenticatedAgent,
    source: RuntimeProbeError,
    journal_path: PathBuf,
) -> RuntimeProbeError {
    record_session_start_failure(runner, &source);
    probe_failed_after_ordered_cleanup(
        source,
        journal_path,
        runner,
        move |runner| {
            let mut journal_errors: Vec<String> = Vec::new();
            match runner.unload_agent_gracefully(authenticated, &mut journal_errors) {
                Ok(_) => {
                    if !journal_errors.is_empty() {
                        eprintln!(
                            "启动失败后的安全卸载存在日志错误: {}",
                            journal_errors.join(" | ")
                        );
                    }
                    Ok(())
                }
                Err(error) => {
                    let unload_completed: bool = runner.agent_unloaded;
                    let label: &str = if unload_completed {
                        "安全卸载已完成，但证据记录失败"
                    } else {
                        "安全卸载失败"
                    };
                    let message: String = journal_error_summary(&error);
                    if let Err(journal_error) = runner.journal.record(
                        "session.startup_unload.failure",
                        "error",
                        json!({
                            "message": message,
                            "agent_unloaded": unload_completed,
                        }),
                    ) {
                        eprintln!("持久运行态卸载失败日志写入失败: {journal_error}");
                        journal_errors.push(journal_error.to_string());
                    }
                    let journal_suffix: String = if journal_errors.is_empty() {
                        String::new()
                    } else {
                        format!("; 日志错误: {}", journal_errors.join(" | "))
                    };
                    Err(format!("{label}: {error}{journal_suffix}"))
                }
            }
        },
        |runner| {
            runner
                .cleanup()
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )
}

/// 最终收尾失败时只回收当前会话刚发布的证据，并保留所有删除错误。
fn probe_finalization_failed(
    source: RuntimeProbeError,
    journal_path: PathBuf,
    tool_root: &ToolRoot,
    published_artifacts: &[(&str, &Path)],
) -> RuntimeProbeError {
    let mut errors = Vec::new();
    for (kind, relative_path) in published_artifacts {
        if let Err(error) = tool_root.remove_file_if_exists(relative_path, None) {
            errors.push(format!(
                "删除{kind} {} 失败: {error}",
                relative_path.display()
            ));
        }
    }
    RuntimeProbeError::ProbeFailed {
        source: Box::new(source),
        journal_path,
        cleanup_error: (!errors.is_empty()).then(|| errors.join("；")),
    }
}

/// 拒绝空白或含 NUL 的原生命令路径选项。
fn validate_executable_option(field: &'static str, value: &str) -> Result<(), RuntimeProbeError> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err(RuntimeProbeError::InvalidOption {
            field,
            message: "不得为空或包含 NUL".to_owned(),
        });
    }
    Ok(())
}

/// 把最终 PID、模块映射检查和 loader 执行放进同一个 root shell 调用。
fn guarded_loader_command(
    package_name: &str,
    module_name: &str,
    process_id: u32,
    loader_path: &str,
    agent_path: &str,
    bootstrap_path: &str,
) -> String {
    format!(
        "azlw_pid=\"$(pidof {package_name})\" || {{ echo 'AZLW_TARGET_NOT_READY'; exit 70; }}; \
         test \"$azlw_pid\" = \"{process_id}\" || {{ echo \"AZLW_TARGET_CHANGED expected={process_id} actual=$azlw_pid\"; exit 71; }}; \
         grep -F -q /{module_name} /proc/{process_id}/maps || {{ echo 'AZLW_TARGET_MODULE_NOT_READY'; exit 72; }}; \
         echo {LOADER_ENTERED_MARKER}; \
         exec {loader_path} --pid {process_id} --agent {agent_path} --session-file {bootstrap_path}"
    )
}

/// 在同一 root shell 内复核原 PID 和目标模块，再执行一次性卸载事务。
#[cfg(any(target_os = "windows", test))]
fn guarded_unloader_command(
    package_name: &str,
    module_name: &str,
    process_id: u32,
    loader_path: &str,
    agent_path: &str,
    unload_path: &str,
) -> String {
    format!(
        "azlw_pid=\"$(pidof {package_name})\" || {{ echo 'AZLW_TARGET_NOT_READY'; exit 70; }}; \
         test \"$azlw_pid\" = \"{process_id}\" || {{ echo \"AZLW_TARGET_CHANGED expected={process_id} actual=$azlw_pid\"; exit 71; }}; \
         grep -F -q /{module_name} /proc/{process_id}/maps || {{ echo 'AZLW_TARGET_MODULE_NOT_READY'; exit 72; }}; \
         echo {UNLOADER_ENTERED_MARKER}; \
         exec {loader_path} unload --agent {agent_path} --session-file {unload_path}"
    )
}

#[cfg(test)]
mod tests;

/// 状态查询只重连已有代理；显式卸载复用严格排空、卸载和清理证据。
#[cfg(target_os = "windows")]
pub(super) fn manage_runtime_agent(
    options: RuntimeProbeOptions,
    action: super::game_port::AgentAction,
) -> Result<Option<super::runtime::HealthResult>, RuntimeProbeError> {
    ProductionSession::manage_resident(options, action)
}

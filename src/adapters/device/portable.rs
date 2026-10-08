//! 编排便携 ADB、模拟器发现、目标门禁和既有运行态探针。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
#[cfg(target_os = "windows")]
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

use super::capture::equipment_sample::EquipmentSampleEvidence;
use super::capture::full_state::validate_external_capture_root;
use super::capture::ship_catalog::ShipCatalogCaptureEvidence;
use super::probe::{CleanupEvidence, ProcessEvidence, RuntimeProbeError, RuntimeProbeOutcome};
#[cfg(target_os = "windows")]
use super::probe::{RuntimeProbeOptions, RuntimeSession, RuntimeSessionShutdownEvidence};
#[cfg(target_os = "windows")]
use crate::adapters::json_artifact::JsonArtifactError;
use crate::adapters::settings::{AgentMappingMode, AgentVisibilityMode, DeviceMode, Settings};
use crate::adapters::tool_root::ToolRootError;
#[cfg(target_os = "windows")]
use suzushiro_adb::OwnedAdbServer;

#[cfg(any(target_os = "windows", test))]
mod adb;
mod discovery;
mod instances;
mod session;
#[cfg(target_os = "windows")]
pub(super) use session::manage_portable_agent;
mod startup;

#[cfg(test)]
use adb::load_and_inspect_configured_adb_bundle;
#[cfg(all(test, target_os = "windows"))]
use discovery::{ResolvedManager, resolve_manager};
#[cfg(all(test, target_os = "windows"))]
use instances::select_instance;
#[cfg(test)]
use session::{map_runtime_error, select_profile_package};
#[cfg(target_os = "windows")]
use session::{run_portable_probe_windows, run_portable_ship_catalog_capture_windows};
#[cfg(test)]
use startup::adb_game_launch_arguments;
#[cfg(all(test, target_os = "windows"))]
use suzushiro_emulator::EmulatorInstance;
#[cfg(all(test, target_os = "windows"))]
use suzushiro_emulator::discovery::{running_manager_candidates, validate_manager_candidate};

#[cfg(test)]
const TARGET_PACKAGE: &str = "com.bilibili.azurlane";
#[cfg(target_os = "windows")]
const UNREPORTED_ANDROID_VERSION: &str = "未报告";
#[cfg(target_os = "windows")]
const EXPECTED_ABI: &str = "x86_64";
const DEFAULT_TIMEOUT_MS: u32 = 10_000;
const DEFAULT_MAX_ITEMS: u32 = 2_000;
const DEFAULT_CONNECT_TIMEOUT_SECONDS: u32 = 15;
const DEFAULT_STARTUP_TIMEOUT_SECONDS: u32 = 90;
const MAX_TIMEOUT_SECONDS: u32 = 3_600;
#[cfg(any(target_os = "windows", test))]
const PORTABLE_PROBE_SCHEMA_VERSION: u32 = 3;
#[cfg(any(target_os = "windows", test))]
const PORTABLE_SHIP_CATALOG_CAPTURE_SCHEMA_VERSION: u32 = 1;
#[cfg(target_os = "windows")]
const MAXIMUM_PORTABLE_RECEIPT_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(target_os = "windows")]
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 便携探针选择安装、实例和序列号的方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortableMode {
    /// 从登记安装和模拟器管理器信息中发现目标，提示只缩小候选范围。
    Auto,
    /// 严格使用指定实例和序列号；管理器未指定时只接受唯一登记安装。
    Manual,
}

impl PortableMode {
    /// 返回命令行和收据共用的稳定小写模式名。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

impl FromStr for PortableMode {
    type Err = PortableProbeError;

    /// 只接受稳定的 auto 与 manual 小写值。
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "manual" => Ok(Self::Manual),
            _ => Err(PortableProbeError::InvalidOption {
                field: "mode",
                message: format!("只允许 auto 或 manual，实际为 {value:?}"),
            }),
        }
    }
}

/// 便携探针的发现提示和有界读取参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableProbeOptions {
    tool_root: PathBuf,
    mode: PortableMode,
    adb_relative_path_hint: Option<PathBuf>,
    manager_hint: Option<PathBuf>,
    instance_hint: Option<String>,
    require_ready_instance: bool,
    serial_hint: Option<String>,
    game_package_hint: Option<String>,
    connect_timeout_seconds: u32,
    startup_timeout_seconds: u32,
    agent_mapping_mode: AgentMappingMode,
    agent_visibility_mode: AgentVisibilityMode,
    timeout_ms: u32,
    max_items: u32,
    full_state_capture_root: Option<PathBuf>,
    retain_agent: bool,
}

impl PortableProbeOptions {
    /// 把严格设置映射到便携探针；空值保留自动发现，非空值不经文本猜测。
    pub(crate) fn from_settings(
        tool_root: PathBuf,
        settings: &Settings,
    ) -> Result<PortableProbeOptions, PortableProbeError> {
        let device = settings.device();
        let mode: PortableMode = match device.mode() {
            DeviceMode::Auto => PortableMode::Auto,
            DeviceMode::Manual => PortableMode::Manual,
        };
        let mut options: PortableProbeOptions = PortableProbeOptions::new(tool_root, mode);
        if !device.adb_path().is_empty() {
            options = options.with_adb_relative_path_hint(device.adb_path());
        }
        if !device.manager_path().is_empty() {
            options = options.with_manager_hint(device.manager_path());
        }
        if !device.serial().is_empty() {
            options = options.with_serial_hint(device.serial());
        }
        if !device.instance().is_empty() {
            options = options.with_instance_hint(device.instance());
        }
        if !device.game_package().is_empty() {
            options = options.with_game_package_hint(device.game_package());
        }
        options
            .with_connect_timeout_seconds(settings.runtime().connect_timeout_seconds())?
            .with_startup_timeout_seconds(settings.runtime().startup_timeout_seconds())
            .map(|options| {
                options
                    .with_agent_mapping_mode(settings.runtime().agent_mapping_mode())
                    .with_agent_visibility_mode(settings.runtime().agent_visibility_mode())
            })
    }

    /// 关闭宿主连接时保留设备代理，供后续操作重新认证。
    pub fn with_retain_agent(mut self, retain: bool) -> Self {
        self.retain_agent = retain;
        self
    }

    /// 建立默认有界参数，目标提示由后续构建方法显式附加。
    pub fn new(tool_root: impl Into<PathBuf>, mode: PortableMode) -> Self {
        Self {
            tool_root: tool_root.into(),
            mode,
            adb_relative_path_hint: None,
            manager_hint: None,
            instance_hint: None,
            require_ready_instance: false,
            serial_hint: None,
            game_package_hint: None,
            connect_timeout_seconds: DEFAULT_CONNECT_TIMEOUT_SECONDS,
            startup_timeout_seconds: DEFAULT_STARTUP_TIMEOUT_SECONDS,
            agent_mapping_mode: AgentMappingMode::Memfd,
            agent_visibility_mode: AgentVisibilityMode::Normal,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_items: DEFAULT_MAX_ITEMS,
            full_state_capture_root: None,
            retain_agent: false,
        }
    }

    /// 设置工具根目录内的 ADB 可执行文件提示，其运行闭包必须位于同一目录。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn with_adb_relative_path_hint(
        mut self,
        adb_relative_path_hint: impl Into<PathBuf>,
    ) -> Self {
        self.adb_relative_path_hint = Some(adb_relative_path_hint.into());
        self
    }

    /// 设置模拟器管理器可执行文件提示；自动模式会验证后再决定是否采用。
    pub fn with_manager_hint(mut self, manager_hint: impl Into<PathBuf>) -> Self {
        self.manager_hint = Some(manager_hint.into());
        self
    }

    /// 设置唯一实例索引；多实例环境必须显式提供。
    pub fn with_instance_hint(mut self, instance_hint: impl Into<String>) -> Self {
        self.instance_hint = Some(instance_hint.into());
        self
    }

    /// 将当前操作锁定到已就绪实例，串号由该实例的管理器证据确定。
    pub(crate) fn with_selected_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance_hint = Some(instance.into());
        self.serial_hint = None;
        self.require_ready_instance = true;
        self
    }

    /// 设置回环序列号提示；手工模式要求与管理器证据精确一致。
    pub fn with_serial_hint(mut self, serial_hint: impl Into<String>) -> Self {
        self.serial_hint = Some(serial_hint.into());
        self
    }

    /// 设置目标包名提示；手工模式提供时要求与当前运行态 profile 精确一致。
    pub fn with_game_package_hint(mut self, game_package_hint: impl Into<String>) -> Self {
        self.game_package_hint = Some(game_package_hint.into());
        self
    }

    /// 设置宿主连接和握手使用的最长等待秒数。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn with_connect_timeout_seconds(
        mut self,
        connect_timeout_seconds: u32,
    ) -> Result<Self, PortableProbeError> {
        validate_timeout_seconds("connect_timeout_seconds", connect_timeout_seconds)?;
        self.connect_timeout_seconds = connect_timeout_seconds;
        Ok(self)
    }

    /// 设置实例、游戏和目标模块进入可读状态的最长等待秒数。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn with_startup_timeout_seconds(
        mut self,
        startup_timeout_seconds: u32,
    ) -> Result<Self, PortableProbeError> {
        validate_timeout_seconds("startup_timeout_seconds", startup_timeout_seconds)?;
        self.startup_timeout_seconds = startup_timeout_seconds;
        Ok(self)
    }

    /// 设置 Agent 映射策略；默认 memfd，匿名重映射必须由设置显式选择。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn with_agent_mapping_mode(mut self, mode: AgentMappingMode) -> Self {
        self.agent_mapping_mode = mode;
        self
    }

    /// 设置 Agent 可见性策略；默认不修改 linker 状态。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn with_agent_visibility_mode(mut self, mode: AgentVisibilityMode) -> Self {
        self.agent_visibility_mode = mode;
        self
    }

    /// 设置设备 loader 和 RPC 的相对超时。
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Result<Self, PortableProbeError> {
        if !(1..=30_000).contains(&timeout_ms) {
            return Err(PortableProbeError::InvalidOption {
                field: "timeout_ms",
                message: "只允许 1 至 30000".to_owned(),
            });
        }
        self.timeout_ms = timeout_ms;
        Ok(self)
    }

    /// 设置背包快照的明确条目上限。
    pub fn with_max_items(mut self, max_items: u32) -> Result<Self, PortableProbeError> {
        if !(1..=2_000).contains(&max_items) {
            return Err(PortableProbeError::InvalidOption {
                field: "max_items",
                message: "只允许 1 至 2000".to_owned(),
            });
        }
        self.max_items = max_items;
        Ok(self)
    }

    /// 显式启用发布目录外的完整原始状态采集；普通调用不会设置该目录。
    pub fn with_full_state_capture_root(mut self, capture_root: impl Into<PathBuf>) -> Self {
        self.full_state_capture_root = Some(capture_root.into());
        self
    }

    /// 模式执行前完成组合约束校验，避免自动与手工语义混用。
    pub fn validate(&self) -> Result<(), PortableProbeError> {
        if !self.tool_root.is_dir() {
            return Err(PortableProbeError::InvalidOption {
                field: "tool_root",
                message: "必须是现有目录".to_owned(),
            });
        }
        if self
            .adb_relative_path_hint
            .as_ref()
            .is_some_and(|path: &PathBuf| path.as_os_str().is_empty() || path.is_absolute())
        {
            return Err(PortableProbeError::InvalidOption {
                field: "adb_path",
                message: "必须是工具根目录内的非空相对路径".to_owned(),
            });
        }
        validate_timeout_seconds("connect_timeout_seconds", self.connect_timeout_seconds)?;
        validate_timeout_seconds("startup_timeout_seconds", self.startup_timeout_seconds)?;
        if let Some(capture_root) = &self.full_state_capture_root {
            validate_external_capture_root(&self.tool_root, capture_root).map_err(|source| {
                PortableProbeError::InvalidOption {
                    field: "full_state_capture_root",
                    message: source.to_string(),
                }
            })?;
        }
        if self.require_ready_instance {
            #[cfg(target_os = "windows")]
            suzushiro_emulator::split_selection(self.instance_hint.as_deref().unwrap_or_default())?;
            #[cfg(not(target_os = "windows"))]
            validate_instance_index(
                self.instance_hint
                    .as_deref()
                    .unwrap_or_default()
                    .rsplit(':')
                    .next()
                    .unwrap_or_default(),
            )?;
        }
        match self.mode {
            PortableMode::Auto => {}
            PortableMode::Manual => {
                if let Some(instance) = &self.instance_hint {
                    #[cfg(target_os = "windows")]
                    suzushiro_emulator::split_selection(instance)?;
                    #[cfg(not(target_os = "windows"))]
                    validate_instance_index(instance.rsplit(':').next().unwrap_or_default())?;
                }
                if let Some(serial) = &self.serial_hint {
                    validate_serial(serial)?;
                }
                if let Some(package) = &self.game_package_hint {
                    validate_package_name(package)?;
                }
                if self.instance_hint.is_none()
                    || (self.serial_hint.is_none() && !self.require_ready_instance)
                {
                    return Err(PortableProbeError::InvalidOption {
                        field: "manual",
                        message: "manual 模式必须同时提供 instance 和 serial".to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// 随包 Platform-Tools 单个文件的发布证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BundleFileEvidence {
    pub relative_path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// 随包 ADB 的版本、工具内状态根和完整固定文件闭包。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdbBundleInspection {
    pub revision: String,
    pub state_root: PathBuf,
    pub files: Vec<BundleFileEvidence>,
}

/// 模拟器管理器安装和实例选择证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagerEvidence {
    pub executable: PathBuf,
    pub instance_index: String,
    pub instance_name: String,
    pub android_version: String,
    pub instance_started_by_probe: bool,
}

/// 唯一目标通过 ADB 门禁后的包、ABI、PID 和权限证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetEvidence {
    pub serial: String,
    pub abi: String,
    pub abi_list: String,
    pub package_name: String,
    pub package_paths: Vec<String>,
    pub process_id: u32,
    pub root_identity: String,
    pub game_started_by_probe: bool,
}

/// 当前工具直接持有的 ADB 服务和随包文件证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PortableAdbEvidence {
    pub bundle: AdbBundleInspection,
    pub server_port: u16,
    pub server_process_id: u32,
    pub server_log_path: PathBuf,
}

/// ADB 子进程退出后的精确定向清理证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PortableCleanupEvidence {
    pub adb_process_stopped: bool,
    pub adb_port_released: bool,
    pub adb_temporary_root_removed: bool,
}

/// 持久运行态关闭后形成的内层会话和独立 ADB 清理证据。
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PortableSessionShutdownEvidence {
    pub(crate) runtime: RuntimeSessionShutdownEvidence,
    pub(crate) adb: PortableCleanupEvidence,
}

/// 便携探针的完整发现、运行态引用和隔离清理收据。
#[derive(Clone, Debug, Serialize)]
pub struct PortableProbeReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub mode: PortableMode,
    pub manager: ManagerEvidence,
    pub target: TargetEvidence,
    pub adb: PortableAdbEvidence,
    pub runtime_session_id: String,
    pub runtime_snapshot_count: u32,
    pub runtime_receipt_path: PathBuf,
    pub runtime_journal_path: PathBuf,
    /// 独立装备维护样本的路径、摘要和数量，不包含原始正文。
    pub equipment_sample: EquipmentSampleEvidence,
    pub cleanup: PortableCleanupEvidence,
}

/// 便携验证收据和既有运行态探针的完整成功结果。
pub struct PortableProbeOutcome {
    pub report: PortableProbeReport,
    pub receipt_path: PathBuf,
    pub runtime: RuntimeProbeOutcome,
}

/// 专用舰船静态目录捕获的目标、外部文件和完整清理收据。
#[derive(Clone, Debug, Serialize)]
pub struct PortableShipCatalogCaptureReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub mode: PortableMode,
    pub manager: ManagerEvidence,
    pub target: TargetEvidence,
    pub adb: PortableAdbEvidence,
    pub capture: ShipCatalogCaptureEvidence,
    pub runtime_journal_path: PathBuf,
    pub process_after_cleanup: ProcessEvidence,
    pub runtime_cleanup: CleanupEvidence,
    pub adb_cleanup: PortableCleanupEvidence,
}

/// 外部静态目录和工具内脱敏收据均成功发布后的结果。
pub struct PortableShipCatalogCaptureOutcome {
    pub report: PortableShipCatalogCaptureReport,
    pub receipt_path: PathBuf,
}

/// 已完成发现和目标门禁、但尚未关闭运行态与独立 ADB 的便携会话。
#[cfg(target_os = "windows")]
pub(crate) struct PortableRuntimeSession {
    runtime: Option<RuntimeSession>,
    runtime_shutdown: Option<RuntimeSessionShutdownEvidence>,
    prepared: Option<PreparedPortableSession>,
    shutdown_evidence: Option<PortableSessionShutdownEvidence>,
}

/// 保存一次便携会话的固定配置、发现证据和仍由当前进程持有的 ADB 服务。
#[cfg(target_os = "windows")]
struct PreparedPortableSession {
    options: PortableProbeOptions,
    manager: ManagerEvidence,
    target: TargetEvidence,
    adb: PortableAdbEvidence,
    runtime_options: RuntimeProbeOptions,
    server: Option<OwnedAdbServer>,
}

/// 执行发现、独立 ADB、目标门禁、既有运行态探针和定向清理。
pub fn run_portable_probe(
    options: PortableProbeOptions,
) -> Result<PortableProbeOutcome, PortableProbeError> {
    options.validate()?;
    #[cfg(not(target_os = "windows"))]
    {
        let _ = options;
        Err(PortableProbeError::UnsupportedPlatform)
    }
    #[cfg(target_os = "windows")]
    {
        run_portable_probe_windows(options)
    }
}

/// 执行专用舰船静态目录捕获；不运行普通探针的多次快照和负向协议探测。
pub fn run_portable_ship_catalog_capture(
    options: PortableProbeOptions,
    capture_root: impl Into<PathBuf>,
) -> Result<PortableShipCatalogCaptureOutcome, PortableProbeError> {
    options.validate()?;
    if options.full_state_capture_root.is_some() {
        return Err(PortableProbeError::InvalidOption {
            field: "ship_catalog_capture_root",
            message: "不得与 full_state_capture_root 同时启用".to_owned(),
        });
    }
    let capture_root = validate_external_capture_root(&options.tool_root, &capture_root.into())
        .map_err(|source| PortableProbeError::InvalidOption {
            field: "ship_catalog_capture_root",
            message: source.to_string(),
        })?;
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (options, capture_root);
        Err(PortableProbeError::UnsupportedPlatform)
    }
    #[cfg(target_os = "windows")]
    {
        run_portable_ship_catalog_capture_windows(options, capture_root)
    }
}

/// 便携探针无法形成唯一、受控且完成清理的 Windows + 模拟器 会话。
#[derive(Debug, Error)]
pub enum PortableProbeError {
    /// 选项自身或模式组合不满足明确边界。
    #[error("便携探针选项 {field} 无效: {message}")]
    InvalidOption {
        field: &'static str,
        message: String,
    },
    /// 便携 ADB 子进程所有权只在原生 Windows 进程中成立。
    #[error("便携 ADB 真机验证只允许从原生 Windows 可执行文件运行")]
    UnsupportedPlatform,
    /// 模拟器安装、实例或运行状态无法形成唯一目标。
    #[error("模拟器发现失败: {message}")]
    Discovery { message: String },
    /// 多个已经验证的模拟器安装或运行实例无法唯一选择。
    #[error("模拟器目标不唯一: {message}")]
    AmbiguousTarget { message: String },
    /// 多个已经验证的 模拟器管理器安装无法唯一选择。
    #[error("模拟器管理器安装不唯一: {message}")]
    AmbiguousManager { message: String },
    /// 手工设置的精确目标与模拟器管理器提供的只读证据不一致。
    #[error("手工目标不匹配: {message}")]
    TargetMismatch { message: String },
    /// 模拟器、Android 或游戏目标超出当前运行态 profile 的验证范围。
    #[error("目标运行环境不兼容: {message}")]
    IncompatibleTarget { message: String },
    /// 注册表读取失败且不能可靠判断安装候选。
    #[cfg(target_os = "windows")]
    #[error("读取模拟器安装登记失败: {message}")]
    Registry { message: String },
    /// 厂商管理器命令或解析失败。
    #[cfg(target_os = "windows")]
    #[error("模拟器管理器 阶段 {stage} 失败: {message}")]
    ManagerCommand {
        stage: &'static str,
        message: String,
    },
    /// ADB 目标输出未通过包、ABI、PID 或 root 门禁。
    #[cfg(target_os = "windows")]
    #[error("目标门禁阶段 {stage} 失败: {message}")]
    Target {
        stage: &'static str,
        message: String,
    },
    /// 隔离 ADB 在资源、启动、目标、版本或清理阶段失败。
    #[error("隔离 ADB 阶段 {stage} 失败: {source}")]
    Adb {
        stage: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 既有运行态探针失败；底层错误仅作为 source 保留，公开文本不展开原始数据。
    #[error("运行态验证失败，请查看工具目录内的运行态日志")]
    Runtime(#[from] RuntimeProbeError),
    /// 会话资源已清理，但正常卸载失败，保留恢复方式与原始日志。
    #[error(
        "游戏运行态正常卸载失败: {message}；会话资源已清理，游戏重启: {game_restarted}；日志: {journal_path}"
    )]
    RuntimeShutdownFailed {
        message: String,
        journal_path: PathBuf,
        game_restarted: bool,
    },
    /// 游戏进程存在，但账号流程尚未进入可读取背包的港区状态。
    #[error("游戏尚未就绪: 请在碧蓝航线中完成登录并进入港区后重试；运行态详情: {detail}")]
    GameNotReady {
        detail: String,
        #[source]
        source: Box<RuntimeProbeError>,
    },
    /// 手工目标包与当前运行态 profile 的目标身份不一致。
    #[error("manual game_package {configured:?} 与当前运行态 profile 的 {profile:?} 不一致")]
    ProfilePackageMismatch { configured: String, profile: String },
    /// 运行态探针和 ADB 子进程清理同时失败。
    #[cfg(target_os = "windows")]
    #[error("便携探针操作失败: {operation}; 隔离 ADB 清理同时失败: {adb}")]
    OperationAndAdbCleanup { operation: String, adb: String },
    /// 持久读取失败且关闭运行态或独立 ADB 时又发生清理错误。
    #[error("持久会话操作失败: {operation}; 会话清理同时失败: {cleanup}")]
    OperationAndSessionCleanup {
        #[source]
        operation: Box<dyn std::error::Error + Send + Sync>,
        cleanup: Box<PortableProbeError>,
    },
    /// 便携收据或随包摘要文件系统访问失败。
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// JSON 证据文件未能完成有界编码、同步、发布或失败清理。
    #[error("便携收据发布失败: {source}")]
    JsonArtifact {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 工具根目录或其受控相对路径不满足零越界约束。
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
}

/// 在不暴露私有 ADB 类型的前提下保留精确阶段和完整原因链。
fn adb_failure<E>(stage: &'static str, source: E) -> PortableProbeError
where
    E: std::error::Error + Send + Sync + 'static,
{
    PortableProbeError::Adb {
        stage,
        source: Box::new(source),
    }
}

/// 便携 ADB 文件通过路径边界后仍违反大小或闭包约束。
#[derive(Debug, Error)]
#[error("{message}")]
struct AdbContractError {
    message: String,
}

/// 自动提示和固定随包 ADB 同时失效时保留两份诊断及最终原因链。
#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Error)]
#[error("ADB 提示 {configured_path} 无效: {configured}; 固定随包 ADB 同样无效: {fallback}")]
struct AdbFallbackError {
    configured_path: PathBuf,
    configured: String,
    #[source]
    fallback: Box<dyn std::error::Error + Send + Sync>,
}

#[cfg(target_os = "windows")]
impl From<JsonArtifactError> for PortableProbeError {
    fn from(source: JsonArtifactError) -> Self {
        Self::JsonArtifact {
            source: Box::new(source),
        }
    }
}

#[cfg(not(target_os = "windows"))]
use suzushiro_emulator::validate_instance_index;

fn validate_serial(serial: &str) -> Result<SocketAddr, PortableProbeError> {
    Ok(suzushiro_emulator::validate_serial(serial)?)
}

/// 配置文件中的秒级等待必须保持非零且有明确上限。
fn validate_timeout_seconds(field: &'static str, seconds: u32) -> Result<(), PortableProbeError> {
    if !(1..=MAX_TIMEOUT_SECONDS).contains(&seconds) {
        return Err(PortableProbeError::InvalidOption {
            field,
            message: format!("只允许 1 至 {MAX_TIMEOUT_SECONDS} 秒，实际为 {seconds}"),
        });
    }
    Ok(())
}

/// 目标包名只允许进入受控设备命令的 ASCII 标识字符。
fn validate_package_name(package: &str) -> Result<(), PortableProbeError> {
    if !suzushiro_text_format::is_ascii_package_name(package) {
        return Err(PortableProbeError::InvalidOption {
            field: "game_package",
            message: "必须是受限的 Android 包名".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;

impl From<suzushiro_emulator::EmulatorError> for PortableProbeError {
    fn from(error: suzushiro_emulator::EmulatorError) -> Self {
        use suzushiro_emulator::EmulatorError;
        match error {
            EmulatorError::InvalidOption { field, message } => {
                Self::InvalidOption { field, message }
            }
            EmulatorError::Discovery { message } => Self::Discovery { message },
            EmulatorError::AmbiguousManager { message } => Self::AmbiguousManager { message },
            #[cfg(target_os = "windows")]
            EmulatorError::Registry { message } => Self::Registry { message },
            #[cfg(target_os = "windows")]
            EmulatorError::ManagerCommand { stage, message } => {
                Self::ManagerCommand { stage, message }
            }
            EmulatorError::Io {
                stage,
                path,
                source,
            } => Self::Io {
                stage,
                path,
                source,
            },
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod emulator_live_tests;

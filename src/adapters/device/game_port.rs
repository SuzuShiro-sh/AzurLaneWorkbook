//! 将严格运行设置和可复用便携会话组合为生产游戏状态读写端口。

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::capture::full_state::FullStateCaptureEvidence;
#[cfg(target_os = "windows")]
use super::portable::PortableRuntimeSession;
use super::portable::{PortableProbeError, PortableProbeOptions};
use super::probe::RuntimeProbeError;
use super::runtime::{CapabilitiesResult, EquipmentCommandReceipt, RuntimeEquipmentCommand};
use crate::adapters::settings::Settings;
use crate::adapters::tool_root::ToolRoot;
use crate::application::{AppError, FullStateCaptureBinding, GameObservation, GamePort};
use crate::domain::GameState;

mod errors;
mod execution;

#[cfg(test)]
use errors::{
    classify_portable_error, classify_runtime_probe_error, map_portable_error, runtime_context,
};
use errors::{
    execution_bound_session_unusable_error, execution_session_configuration_changed_error,
    map_portable, map_session_cleanup_error, map_settings,
};
#[cfg(test)]
use execution::{
    find_warehouse_quantity_by_runtime_id, require_write_capability, runtime_action,
    runtime_client_error_is_not_sent, validate_preflight_action,
};

const FULL_STATE_CAPTURE_ROOT_ENV: &str = "AZLW_FULL_STATE_CAPTURE_ROOT";

/// 空值和缺失值保持默认关闭，非空值才作为显式黄金采集目录传入读取链。
fn full_state_capture_root_from_environment(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// 应用运行端口内部使用的最小会话边界，避免暴露设备和协议实现。
trait GameStateSession {
    fn query(
        &mut self,
        _query: &crate::application::GameQuery,
    ) -> Result<crate::application::GameQueryReport, PortableProbeError> {
        Err(PortableProbeError::Runtime(
            RuntimeProbeError::InvalidOutput {
                stage: "session.query",
                message: "会话未提供对象查询能力".to_owned(),
            },
        ))
    }
    /// 判断现有连接是否仍对应当前设置。
    fn matches_options(&self, options: &PortableProbeOptions) -> bool;

    /// 在现有认证连接上读取一次完整状态。
    fn read_full_state(&mut self) -> Result<GameState, PortableProbeError>;

    fn read_full_state_with_progress(
        &mut self,
        _progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameState, PortableProbeError> {
        self.read_full_state()
    }

    fn read_state_with_scope(
        &mut self,
        _scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameState, PortableProbeError> {
        self.read_full_state_with_progress(progress)
    }

    /// 返回最近一次成功读取同时发布的完整捕获身份。
    fn last_full_state_capture(&self) -> Option<FullStateCaptureEvidence> {
        None
    }

    /// 返回当前认证会话锁定的设备范围。
    fn target_scope(&self) -> Result<(String, String), PortableProbeError> {
        Err(PortableProbeError::Runtime(
            RuntimeProbeError::InvalidOutput {
                stage: "session.target_scope",
                message: "会话没有提供目标设备范围".to_owned(),
            },
        ))
    }

    /// 返回最近一次完整读取验证过的能力报告。
    fn last_capabilities(&self) -> Result<CapabilitiesResult, PortableProbeError> {
        Err(PortableProbeError::Runtime(
            RuntimeProbeError::InvalidOutput {
                stage: "session.capabilities",
                message: "会话没有提供能力报告".to_owned(),
            },
        ))
    }

    /// 在同一认证会话上最多派发一次装备命令。
    fn execute_equipment_command(
        &mut self,
        _command: &RuntimeEquipmentCommand,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        Err(PortableProbeError::Runtime(
            RuntimeProbeError::InvalidOutput {
                stage: "session.equipment_command",
                message: "会话没有提供装备命令能力".to_owned(),
            },
        ))
    }

    /// 查询同一会话中的原装备命令。
    fn query_equipment_command(
        &mut self,
        _command_id: &str,
        _budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        Err(PortableProbeError::Runtime(
            RuntimeProbeError::InvalidOutput {
                stage: "session.equipment_command",
                message: "会话没有提供装备命令查询能力".to_owned(),
            },
        ))
    }

    /// 请求停止观察同一会话中的原装备命令。
    fn cancel_equipment_command(
        &mut self,
        _command_id: &str,
        _budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        Err(PortableProbeError::Runtime(
            RuntimeProbeError::InvalidOutput {
                stage: "session.equipment_command",
                message: "会话没有提供装备命令取消能力".to_owned(),
            },
        ))
    }

    /// 清理运行态与独立 ADB；失败时保留所有权，允许调用方再次尝试。
    fn shutdown(&mut self) -> Result<(), PortableProbeError>;
}

/// 隔离会话构造，便于验证应用端不会为连续读取重复启动资源。
trait GameStateSessionFactory {
    /// 按已经严格校验的便携选项建立唯一会话。
    fn open(
        &self,
        options: PortableProbeOptions,
        related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Box<dyn GameStateSession>, PortableProbeError>;
}

/// 使用原生 Windows 便携资源建立生产会话。
struct ProductionSessionFactory;

impl GameStateSessionFactory for ProductionSessionFactory {
    fn open(
        &self,
        options: PortableProbeOptions,
        related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Box<dyn GameStateSession>, PortableProbeError> {
        #[cfg(target_os = "windows")]
        {
            Ok(Box::new(PortableRuntimeSession::open(options, related)?))
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = options;
            let _ = related;
            Err(PortableProbeError::UnsupportedPlatform)
        }
    }
}

#[cfg(target_os = "windows")]
impl GameStateSession for PortableRuntimeSession {
    fn query(
        &mut self,
        query: &crate::application::GameQuery,
    ) -> Result<crate::application::GameQueryReport, PortableProbeError> {
        PortableRuntimeSession::query(self, query)
    }
    fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameState, PortableProbeError> {
        PortableRuntimeSession::read_state_with_scope(self, scope, progress)
    }
    fn read_full_state_with_progress(
        &mut self,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameState, PortableProbeError> {
        PortableRuntimeSession::read_full_state_with_progress(self, progress)
    }

    fn matches_options(&self, options: &PortableProbeOptions) -> bool {
        PortableRuntimeSession::matches_options(self, options)
    }

    fn read_full_state(&mut self) -> Result<GameState, PortableProbeError> {
        PortableRuntimeSession::read_full_state(self)
    }

    fn last_full_state_capture(&self) -> Option<FullStateCaptureEvidence> {
        PortableRuntimeSession::last_full_state_capture(self)
    }

    fn target_scope(&self) -> Result<(String, String), PortableProbeError> {
        PortableRuntimeSession::target_scope(self)
    }

    fn last_capabilities(&self) -> Result<CapabilitiesResult, PortableProbeError> {
        PortableRuntimeSession::last_capabilities(self)
    }

    fn execute_equipment_command(
        &mut self,
        command: &RuntimeEquipmentCommand,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        PortableRuntimeSession::execute_equipment_command(self, command)
    }

    fn query_equipment_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        PortableRuntimeSession::query_equipment_command(self, command_id, budget)
    }

    fn cancel_equipment_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        PortableRuntimeSession::cancel_equipment_command(self, command_id, budget)
    }

    fn shutdown(&mut self) -> Result<(), PortableProbeError> {
        PortableRuntimeSession::shutdown(self).map(|_evidence| ())
    }
}

struct LiveConnection {
    session: Box<dyn GameStateSession>,
    options: PortableProbeOptions,
    observation: Option<GameObservation>,
}

/// 未连接、活动、执行绑定和清理未完成各自持有自己的资源。
enum SessionConnection {
    Disconnected,
    Active(LiveConnection),
    Bound(LiveConnection),
    CleanupPending {
        connection: LiveConnection,
        bound: bool,
    },
}

/// 本次操作交给设备端口的设置。未绑定时才允许端口自己读取。
enum OperationSettingsSource {
    Snapshot(Settings),
    Unavailable(String),
}

/// 读写操作复用同一认证运行态；执行绑定后禁止隐式换会话，端口释放时关闭全部资源。
pub(crate) struct PortableGamePort {
    tool_root: ToolRoot,
    selected_instance: Option<String>,
    session_factory: Box<dyn GameStateSessionFactory>,
    connection: SessionConnection,
    synchronizing: bool,
    full_state_capture_root: Option<PathBuf>,
    operation_settings: Option<OperationSettingsSource>,
    related_logs: Option<crate::adapters::RelatedLogSink>,
}

impl PortableGamePort {
    /// 保存已经通过工具根目录边界检查的发布位置。未绑定读取使用本次操作的设置快照。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self {
            tool_root,
            selected_instance: None,
            session_factory: Box::new(ProductionSessionFactory),
            connection: SessionConnection::Disconnected,
            synchronizing: false,
            full_state_capture_root: full_state_capture_root_from_environment(std::env::var_os(
                FULL_STATE_CAPTURE_ROOT_ENV,
            )),
            operation_settings: None,
            related_logs: None,
        }
    }

    /// 固定本次操作已经校验的设置。执行绑定后的读取仍会重新打开设置文件。
    pub(crate) fn with_operation_settings(mut self, settings: Settings) -> Self {
        self.operation_settings = Some(OperationSettingsSource::Snapshot(settings));
        self
    }

    /// 保留本次操作已经失败的设置诊断，避免同一操作稍后重新打开文件。
    pub(crate) fn with_unavailable_settings(mut self, summary: String) -> Self {
        self.operation_settings = Some(OperationSettingsSource::Unavailable(summary));
        self
    }

    /// 把这一次操作的相关日志收集器交给随后建立的设备会话。
    pub(crate) fn with_related_logs(mut self, related: crate::adapters::RelatedLogSink) -> Self {
        self.related_logs = Some(related);
        self
    }

    /// 固定本次服务使用的界面实例选择，配置文件保持只读。
    pub(crate) fn with_selected_instance(mut self, instance: String) -> Self {
        self.selected_instance = Some(instance);
        self
    }

    /// 测试只替换会话资源，不绕过设置解析或应用错误映射。
    #[cfg(test)]
    fn with_session_factory(
        tool_root: ToolRoot,
        session_factory: Box<dyn GameStateSessionFactory>,
    ) -> Self {
        Self {
            tool_root,
            selected_instance: None,
            session_factory,
            connection: SessionConnection::Disconnected,
            synchronizing: false,
            full_state_capture_root: None,
            operation_settings: None,
            related_logs: None,
        }
    }

    /// 执行绑定期间重新打开设置文件，用来拒绝连接参数漂移。
    fn settings_for_read(&self) -> Result<Settings, crate::adapters::settings::SettingsError> {
        if self.execution_binding_open() {
            return Settings::load(self.tool_root.as_path());
        }
        match &self.operation_settings {
            Some(OperationSettingsSource::Snapshot(settings)) => Ok(settings.clone()),
            Some(OperationSettingsSource::Unavailable(summary)) => {
                Err(crate::adapters::settings::SettingsError::InvalidValue {
                    field: "settings.json",
                    message: summary.clone(),
                })
            }
            None => Settings::load(self.tool_root.as_path()),
        }
    }

    #[cfg(test)]
    pub(crate) fn connect_timeout_seconds_for_test(
        &self,
    ) -> Result<u32, crate::adapters::settings::SettingsError> {
        Ok(self
            .settings_for_read()?
            .runtime()
            .connect_timeout_seconds())
    }

    fn execution_binding_open(&self) -> bool {
        matches!(
            self.connection,
            SessionConnection::Bound(_) | SessionConnection::CleanupPending { bound: true, .. }
        )
    }

    /// 使用当前设置复用或建立会话；绑定执行会话失效后只能由应用边界显式关闭。
    #[cfg(test)]
    fn read_with_options(
        &mut self,
        options: PortableProbeOptions,
    ) -> Result<GameObservation, AppError> {
        self.read_with_options_and_progress(
            options,
            crate::domain::GameReadScope::full(),
            &mut |_| {},
        )
    }

    fn read_with_options_and_progress(
        &mut self,
        options: PortableProbeOptions,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameObservation, AppError> {
        self.ensure_session(options, progress)?;

        let result: Result<GameState, PortableProbeError> = self
            .live_mut()
            .expect("会话已在读取前建立")
            .session
            .read_state_with_scope(scope, progress);
        match result {
            Ok(state) => {
                let evidence = self
                    .live()
                    .and_then(|live| live.session.last_full_state_capture());
                let observation = GameObservation::from_shared(
                    Arc::new(state),
                    evidence.as_ref().map(capture_binding),
                );
                if let Some(live) = self.live_mut() {
                    live.observation = Some(observation.clone());
                }
                Ok(observation)
            }
            Err(operation) => {
                if let Some(live) = self.live_mut() {
                    live.observation = None;
                }
                progress(crate::application::OperationProgress::stage(
                    "读取失败，正在清理游戏会话",
                ));
                match self.shutdown_active_session() {
                    Ok(()) => Err(map_portable(operation)),
                    Err(cleanup) => Err(map_portable(
                        PortableProbeError::OperationAndSessionCleanup {
                            operation: Box::new(operation),
                            cleanup: Box::new(cleanup),
                        },
                    )),
                }
            }
        }
    }

    fn ensure_session(
        &mut self,
        options: PortableProbeOptions,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<(), AppError> {
        match self.prepare_read(&options) {
            ReadPreparation::Ready => {}
            ReadPreparation::ShutdownAfterFailure => {
                self.shutdown_active_session()
                    .map_err(map_session_cleanup_error)?;
            }
            ReadPreparation::ShutdownForNewOptions => {
                self.shutdown_active_session().map_err(map_portable)?;
            }
            ReadPreparation::ConfigurationChanged => {
                return Err(execution_session_configuration_changed_error());
            }
            ReadPreparation::BoundUnusable => {
                return Err(execution_bound_session_unusable_error());
            }
        }
        if self.live().is_none() {
            progress(crate::application::OperationProgress::stage(
                "正在连接模拟器并建立游戏运行态",
            ));
            let session = self
                .session_factory
                .open(options.clone(), self.related_logs.clone())
                .map_err(map_portable)?;
            self.connection = SessionConnection::Active(LiveConnection {
                session,
                options,
                observation: None,
            });
        }

        Ok(())
    }

    pub(crate) fn query(
        &mut self,
        query: &crate::application::GameQuery,
    ) -> Result<crate::application::GameQueryReport, AppError> {
        let settings = self.settings_for_read().map_err(map_settings)?;
        let mut options =
            PortableProbeOptions::from_settings(self.tool_root.as_path().to_path_buf(), &settings)
                .map_err(map_portable)?
                .with_retain_agent(true);
        if let Some(instance) = &self.selected_instance {
            options = options.with_selected_instance(instance.clone());
        }
        let operation = self.ensure_session(options, &mut |_| {}).and_then(|()| {
            self.live_mut()
                .expect("查询会话已建立")
                .session
                .query(query)
                .map_err(map_portable)
        });
        let cleanup = self.shutdown_active_session();
        match (operation, cleanup) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(map_session_cleanup_error(error)),
            (Err(operation), Err(cleanup)) => Err(map_portable(
                PortableProbeError::OperationAndSessionCleanup {
                    operation: Box::new(operation),
                    cleanup: Box::new(cleanup),
                },
            )),
        }
    }

    fn prepare_read(&self, options: &PortableProbeOptions) -> ReadPreparation {
        match &self.connection {
            SessionConnection::Bound(live) => {
                if &live.options != options {
                    ReadPreparation::ConfigurationChanged
                } else if live.session.matches_options(&live.options) {
                    ReadPreparation::Ready
                } else {
                    ReadPreparation::BoundUnusable
                }
            }
            SessionConnection::CleanupPending {
                connection,
                bound: true,
            } => {
                if &connection.options != options {
                    ReadPreparation::ConfigurationChanged
                } else {
                    ReadPreparation::BoundUnusable
                }
            }
            SessionConnection::CleanupPending { bound: false, .. } => {
                ReadPreparation::ShutdownAfterFailure
            }
            SessionConnection::Active(live) if !live.session.matches_options(options) => {
                ReadPreparation::ShutdownForNewOptions
            }
            SessionConnection::Active(_) | SessionConnection::Disconnected => {
                ReadPreparation::Ready
            }
        }
    }

    fn live(&self) -> Option<&LiveConnection> {
        match &self.connection {
            SessionConnection::Active(live) | SessionConnection::Bound(live) => Some(live),
            SessionConnection::CleanupPending { connection, .. } => Some(connection),
            SessionConnection::Disconnected => None,
        }
    }

    fn live_mut(&mut self) -> Option<&mut LiveConnection> {
        match &mut self.connection {
            SessionConnection::Active(live) | SessionConnection::Bound(live) => Some(live),
            SessionConnection::CleanupPending { connection, .. } => Some(connection),
            SessionConnection::Disconnected => None,
        }
    }

    #[cfg(test)]
    fn has_session(&self) -> bool {
        !matches!(self.connection, SessionConnection::Disconnected)
    }

    #[cfg(test)]
    fn is_execution_bound(&self) -> bool {
        match &self.connection {
            SessionConnection::Bound(_) => true,
            SessionConnection::CleanupPending { bound, .. } => *bound,
            SessionConnection::Active(_) | SessionConnection::Disconnected => false,
        }
    }

    fn cleanup_pending(&self) -> bool {
        matches!(self.connection, SessionConnection::CleanupPending { .. })
    }

    #[cfg(test)]
    fn active_options(&self) -> Option<&PortableProbeOptions> {
        self.live().map(|live| &live.options)
    }

    fn observed_state(&self) -> Option<&GameState> {
        self.live()
            .and_then(|live| live.observation.as_ref())
            .map(|observation| observation.state())
    }

    #[cfg(test)]
    fn replace_observed_state(&mut self, state: GameState) {
        if let Some(live) = self.live_mut() {
            match &mut live.observation {
                Some(observation) => observation.replace_state(state),
                None => live.observation = Some(GameObservation::from_state(state)),
            }
        }
    }

    /// 消费当前会话并显式报告清理错误；没有会话时保持幂等成功。
    fn shutdown_active_session(&mut self) -> Result<(), PortableProbeError> {
        let current = std::mem::replace(&mut self.connection, SessionConnection::Disconnected);
        let (mut live, bound) = match current {
            SessionConnection::Disconnected => return Ok(()),
            SessionConnection::Active(live) => (live, false),
            SessionConnection::Bound(live) => (live, true),
            SessionConnection::CleanupPending { connection, bound } => (connection, bound),
        };
        live.observation = None;
        match live.session.shutdown() {
            Ok(()) => Ok(()),
            Err(error) => {
                if !matches!(&error, PortableProbeError::RuntimeShutdownFailed { .. }) {
                    self.connection = SessionConnection::CleanupPending {
                        connection: live,
                        bound,
                    };
                }
                Err(error)
            }
        }
    }

    /// 设置无法继续解析时先关闭旧会话，避免失效配置长期占用运行态资源。
    fn close_active_after_error(&mut self, operation: AppError) -> AppError {
        match self.shutdown_active_session() {
            Ok(()) => operation,
            Err(cleanup) => map_portable(PortableProbeError::OperationAndSessionCleanup {
                operation: Box::new(operation),
                cleanup: Box::new(cleanup),
            }),
        }
    }
}

enum ReadPreparation {
    Ready,
    ShutdownAfterFailure,
    ShutdownForNewOptions,
    ConfigurationChanged,
    BoundUnusable,
}

fn capture_binding(capture: &FullStateCaptureEvidence) -> FullStateCaptureBinding {
    FullStateCaptureBinding::new(
        capture.schema_version(),
        capture.size_bytes(),
        capture.sha256().to_owned(),
        capture.session_id().to_string(),
        capture.read_index(),
        capture.game_state_content_sha256().to_owned(),
    )
}

impl GamePort for PortableGamePort {
    fn prepare_synchronization(&mut self) {
        self.synchronizing = true;
    }

    fn read_full_state(&mut self) -> Result<GameObservation, AppError> {
        self.read_full_state_with_progress(&mut |_| {})
    }

    fn read_full_state_with_progress(
        &mut self,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameObservation, AppError> {
        self.read_state_with_scope(crate::domain::GameReadScope::full(), progress)
    }

    fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<GameObservation, AppError> {
        let settings: Settings = match self.settings_for_read() {
            Ok(settings) => settings,
            Err(source) => return Err(self.close_active_after_error(map_settings(source))),
        };
        let mut options: PortableProbeOptions = match PortableProbeOptions::from_settings(
            self.tool_root.as_path().to_path_buf(),
            &settings,
        ) {
            Ok(options) => options.with_retain_agent(
                !self.synchronizing || !settings.preferences().unload_after_sync,
            ),
            Err(source) => return Err(self.close_active_after_error(map_portable(source))),
        };
        if let Some(instance) = &self.selected_instance {
            options = options.with_selected_instance(instance.clone());
        }
        if let Some(capture_root) = &self.full_state_capture_root {
            options = options.with_full_state_capture_root(capture_root.clone());
        }
        self.read_with_options_and_progress(options, scope, progress)
    }

    fn shutdown_session(&mut self) -> Result<(), AppError> {
        let result = self
            .shutdown_active_session()
            .map_err(map_session_cleanup_error);
        if result.is_ok() {
            self.synchronizing = false;
        }
        result
    }
}

#[cfg(test)]
mod tests;

/// 显式管理设备内代理，不将状态查询隐式转换为注入。
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentAction {
    Status,
    Inject,
    Unload,
}

/// 供 GUI 和 CLI 共用的代理操作结果。
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, serde::Serialize)]
pub struct AgentStatusReport {
    pub status: String,
    pub available: bool,
    pub pid: Option<u32>,
    pub instance: Option<String>,
    pub message: String,
    pub agent_version: Option<String>,
    pub catalog_generation: Option<u64>,
}

/// 复用生产设置和实例选择，状态、注入与卸载共用同一设备会话所有权。
#[cfg(target_os = "windows")]
pub fn manage_agent(
    tool_root: &std::path::Path,
    selected_instance: Option<&str>,
    action: AgentAction,
    related: Option<&crate::adapters::RelatedLogSink>,
    settings: Result<&Settings, &str>,
) -> Result<AgentStatusReport, AppError> {
    let settings = settings.cloned().map_err(|summary| {
        map_settings(crate::adapters::settings::SettingsError::InvalidValue {
            field: "settings.json",
            message: summary.to_owned(),
        })
    })?;
    let mut options = PortableProbeOptions::from_settings(tool_root.to_path_buf(), &settings)
        .map_err(map_portable)?;
    if let Some(instance) = selected_instance {
        options = options.with_selected_instance(instance);
    }
    super::portable::manage_portable_agent(options.with_retain_agent(true), action, related)
        .map_err(map_portable)
}

/// 把一次已校验设置绑定到应用层代理端口。设备探测留在这个适配器里。
#[cfg(target_os = "windows")]
pub struct ConfiguredAgentManagement {
    tool_root: std::path::PathBuf,
    settings: Result<Settings, String>,
    related: Option<crate::adapters::RelatedLogSink>,
}

#[cfg(target_os = "windows")]
impl ConfiguredAgentManagement {
    pub fn new(
        tool_root: std::path::PathBuf,
        settings: Result<Settings, String>,
        related: Option<crate::adapters::RelatedLogSink>,
    ) -> Self {
        Self {
            tool_root,
            settings,
            related,
        }
    }
}

#[cfg(target_os = "windows")]
impl crate::application::AgentManagementPort for ConfiguredAgentManagement {
    fn manage_agent(
        &self,
        instance: Option<&str>,
        action: crate::application::AgentManagementAction,
    ) -> Result<crate::application::AgentManagementReport, AppError> {
        let adapter_action = match action {
            crate::application::AgentManagementAction::Status => AgentAction::Status,
            crate::application::AgentManagementAction::Inject => AgentAction::Inject,
            crate::application::AgentManagementAction::Unload => AgentAction::Unload,
        };
        let report = manage_agent(
            &self.tool_root,
            instance,
            adapter_action,
            self.related.as_ref(),
            self.settings.as_ref().map_err(String::as_str),
        )?;
        Ok(crate::application::AgentManagementReport {
            status: report.status,
            available: report.available,
            pid: report.pid,
            instance: report.instance,
            message: report.message,
            agent_version: report.agent_version,
            catalog_generation: report.catalog_generation,
        })
    }
}

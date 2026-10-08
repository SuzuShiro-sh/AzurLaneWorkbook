//! 将设备发现、运行态和会话清理失败映射为稳定应用错误。

use super::super::portable::PortableProbeError;
use super::super::probe::RuntimeProbeError;
use super::super::reading::game_state::classify_runtime_error;
use crate::adapters::settings::SettingsError;
use crate::application::{AppError, AppErrorCode, CleanupFact};

/// 设置文件错误保持稳定字段分类和完整底层原因。
pub(super) fn map_settings(source: SettingsError) -> AppError {
    AppError::from_source(
        "game.settings",
        AppErrorCode::SettingsInvalid,
        "settings.json 未通过游戏连接设置校验",
        source,
    )
    .with_context("component", "game")
}

/// 构建选项阶段使用与实际便携执行相同的错误分类。
pub(super) fn map_portable(source: PortableProbeError) -> AppError {
    map_portable_error(source)
}

/// 显式会话关闭使用独立稳定语义，同时保留运行态日志和底层适配上下文。
pub(super) fn map_session_cleanup_error(source: PortableProbeError) -> AppError {
    let contexts = portable_context(&source);
    let cleanup_completed = matches!(&source, PortableProbeError::RuntimeShutdownFailed { .. });
    let mut error = AppError::from_source(
        "game.cleanup",
        AppErrorCode::RuntimeBootstrapFailed,
        if cleanup_completed {
            "游戏运行态正常卸载失败，恢复清理已完成"
        } else {
            "游戏运行态会话清理未完整确认"
        },
        source,
    )
    .with_context("component", "game")
    .with_context("access", "read_only")
    .with_cleanup_fact(if cleanup_completed {
        CleanupFact::Recovered
    } else {
        CleanupFact::Failed {
            owner_retained: true,
        }
    });
    for (key, value) in contexts {
        error = error.with_context(key, value);
    }
    error
}

/// 将发现、ADB 和运行态内部错误映射到应用层稳定契约。
pub(super) fn map_portable_error(source: PortableProbeError) -> AppError {
    let nested: Option<NestedAppError> =
        nested_application_error(&source).map(NestedAppError::from);
    let code: AppErrorCode = nested
        .as_ref()
        .map_or_else(|| classify_portable_error(&source), |error| error.code);
    let stage: &'static str = nested
        .as_ref()
        .map_or_else(|| portable_stage(&source), |error| error.stage);
    let message: String = nested.as_ref().map_or_else(
        || portable_message(code).to_owned(),
        |error| error.message.clone(),
    );
    let mut contexts: Vec<(String, String)> = portable_context(&source);
    if let Some(nested) = nested {
        contexts.extend(nested.context);
    }
    let fact = session_cleanup_fact(&source);
    let mut error: AppError = AppError::from_source(stage, code, message, source)
        .with_context("component", "game")
        .with_context("access", "read_only");
    for (key, value) in contexts {
        error = error.with_context(key, value);
    }
    if let Some(fact) = fact {
        error = error.with_cleanup_fact(fact);
    }
    error
}

fn session_cleanup_fact(source: &PortableProbeError) -> Option<CleanupFact> {
    match source {
        PortableProbeError::RuntimeShutdownFailed { .. } => Some(CleanupFact::Recovered),
        PortableProbeError::OperationAndSessionCleanup { cleanup, .. } => Some(
            if matches!(
                cleanup.as_ref(),
                PortableProbeError::RuntimeShutdownFailed { .. }
            ) {
                CleanupFact::Recovered
            } else {
                CleanupFact::Failed {
                    owner_retained: true,
                }
            },
        ),
        _ => None,
    }
}

/// 只借用嵌套应用错误的公开信息，最终 source 仍保留整个探针失败链。
struct NestedAppError {
    stage: &'static str,
    code: AppErrorCode,
    message: String,
    context: Vec<(String, String)>,
}

impl From<&AppError> for NestedAppError {
    fn from(error: &AppError) -> Self {
        Self {
            stage: error.stage(),
            code: error.code(),
            message: error.message().to_owned(),
            context: error
                .context()
                .iter()
                .map(|(key, value): (&String, &String)| (key.clone(), value.clone()))
                .collect(),
        }
    }
}

fn nested_application_error(source: &PortableProbeError) -> Option<&AppError> {
    match source {
        PortableProbeError::Runtime(source) => nested_runtime_application_error(source),
        PortableProbeError::GameNotReady { source, .. } => {
            nested_runtime_application_error(source.as_ref())
        }
        PortableProbeError::OperationAndSessionCleanup { operation, .. } => {
            session_operation_app_error(operation.as_ref()).or_else(|| {
                session_operation_portable_error(operation.as_ref())
                    .and_then(nested_application_error)
            })
        }
        _ => None,
    }
}

/// 从双重失败中恢复最先发生的应用错误，保留其稳定分类和用户说明。
fn session_operation_app_error<'a>(
    operation: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> Option<&'a AppError> {
    operation.downcast_ref::<AppError>()
}

/// 从双重失败中恢复最先发生的便携适配错误，供分类、阶段和上下文递归使用。
fn session_operation_portable_error<'a>(
    operation: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> Option<&'a PortableProbeError> {
    operation.downcast_ref::<PortableProbeError>()
}

fn nested_runtime_application_error(source: &RuntimeProbeError) -> Option<&AppError> {
    match source {
        RuntimeProbeError::GameStateRead(source) => Some(source),
        RuntimeProbeError::ProbeFailed { source, .. } => nested_runtime_application_error(source),
        _ => None,
    }
}

pub(super) fn classify_portable_error(source: &PortableProbeError) -> AppErrorCode {
    match source {
        PortableProbeError::InvalidOption { .. } => AppErrorCode::SettingsInvalid,
        PortableProbeError::UnsupportedPlatform => AppErrorCode::RuntimeBootstrapFailed,
        PortableProbeError::Discovery { .. } => AppErrorCode::EmulatorNotFound,
        PortableProbeError::AmbiguousManager { .. } => AppErrorCode::EmulatorManagerAmbiguous,
        PortableProbeError::AmbiguousTarget { .. } => AppErrorCode::EmulatorInstanceAmbiguous,
        PortableProbeError::TargetMismatch { .. } => AppErrorCode::AdbTargetUnavailable,
        PortableProbeError::Adb { stage, .. } => classify_adb_stage(stage),
        PortableProbeError::ProfilePackageMismatch { .. } => AppErrorCode::SettingsInvalid,
        PortableProbeError::IncompatibleTarget { .. } => AppErrorCode::RuntimeIncompatible,
        PortableProbeError::Runtime(source) => classify_runtime_probe_error(source),
        PortableProbeError::GameNotReady { .. } => AppErrorCode::GameNotReady,
        PortableProbeError::RuntimeShutdownFailed { .. } => AppErrorCode::RuntimeBootstrapFailed,
        PortableProbeError::Io { .. }
        | PortableProbeError::JsonArtifact { .. }
        | PortableProbeError::ToolRoot(_) => AppErrorCode::RuntimeBootstrapFailed,
        PortableProbeError::OperationAndSessionCleanup { operation, .. } => {
            if let Some(source) = session_operation_app_error(operation.as_ref()) {
                source.code()
            } else if let Some(source) = session_operation_portable_error(operation.as_ref()) {
                classify_portable_error(source)
            } else {
                AppErrorCode::RuntimeBootstrapFailed
            }
        }
        #[cfg(target_os = "windows")]
        PortableProbeError::Registry { .. } | PortableProbeError::ManagerCommand { .. } => {
            AppErrorCode::RuntimeBootstrapFailed
        }
        #[cfg(target_os = "windows")]
        PortableProbeError::Target { stage, .. } => match *stage {
            "target.require_running_game" => AppErrorCode::GameNotReady,
            "target.verify_abi" => AppErrorCode::RuntimeIncompatible,
            "target.verify_package" | "target.read_pid" | "target.wait_pid" => {
                AppErrorCode::GameNotReady
            }
            "target.get_serial" => AppErrorCode::AdbTargetUnavailable,
            _ => AppErrorCode::RuntimeBootstrapFailed,
        },
        #[cfg(target_os = "windows")]
        PortableProbeError::OperationAndAdbCleanup { .. } => AppErrorCode::RuntimeBootstrapFailed,
    }
}

fn classify_adb_stage(stage: &str) -> AppErrorCode {
    match stage {
        "adb.target" => AppErrorCode::AdbTargetUnavailable,
        _ => AppErrorCode::RuntimeBootstrapFailed,
    }
}

pub(super) fn classify_runtime_probe_error(source: &RuntimeProbeError) -> AppErrorCode {
    match source {
        RuntimeProbeError::ProbeFailed { source, .. } => classify_runtime_probe_error(source),
        RuntimeProbeError::GameStateRead(source) => source.code(),
        RuntimeProbeError::InvalidOption { .. } => AppErrorCode::SettingsInvalid,
        RuntimeProbeError::RuntimeClient(source) => classify_runtime_error(source),
        RuntimeProbeError::Profile(_)
        | RuntimeProbeError::RuntimeProtocol(_)
        | RuntimeProbeError::InvalidToolAsset { .. }
        | RuntimeProbeError::InvalidOutput { .. }
        | RuntimeProbeError::Json { .. } => AppErrorCode::RuntimeIncompatible,
        RuntimeProbeError::ShipMapping(_)
        | RuntimeProbeError::EquipmentRead(_)
        | RuntimeProbeError::ShipCatalogRead(_)
        | RuntimeProbeError::FullStateCapture { .. }
        | RuntimeProbeError::ShipCatalogCapture { .. } => AppErrorCode::FullCheckFailed,
        RuntimeProbeError::Io { .. }
        | RuntimeProbeError::HostCommand { .. }
        | RuntimeProbeError::DeviceCommand { .. }
        | RuntimeProbeError::LoaderRejected { .. }
        | RuntimeProbeError::UnloaderRejected { .. }
        | RuntimeProbeError::Session(_)
        | RuntimeProbeError::Bootstrap(_)
        | RuntimeProbeError::ToolRoot(_)
        | RuntimeProbeError::JsonArtifact { .. }
        | RuntimeProbeError::Cleanup { .. } => AppErrorCode::RuntimeBootstrapFailed,
    }
}

fn portable_stage(source: &PortableProbeError) -> &'static str {
    match source {
        PortableProbeError::InvalidOption { .. } => "game.settings",
        PortableProbeError::RuntimeShutdownFailed { .. } => "game.cleanup",
        PortableProbeError::Discovery { .. } => "game.discover",
        PortableProbeError::AmbiguousManager { .. } => "game.discover.manager",
        PortableProbeError::AmbiguousTarget { .. } => "game.discover.instance",
        PortableProbeError::TargetMismatch { .. } => "game.target",
        PortableProbeError::Adb { stage, .. } => match *stage {
            "adb.bundle" => "game.bootstrap.adb_bundle",
            "adb.start" => "game.bootstrap.adb_server",
            "adb.target" => "game.target",
            "adb.cleanup" => "game.cleanup",
            _ => "game.bootstrap",
        },
        PortableProbeError::ProfilePackageMismatch { .. } => "game.settings",
        PortableProbeError::IncompatibleTarget { .. } => "game.compatibility",
        PortableProbeError::Runtime(_) | PortableProbeError::GameNotReady { .. } => "game.runtime",
        PortableProbeError::UnsupportedPlatform
        | PortableProbeError::Io { .. }
        | PortableProbeError::JsonArtifact { .. }
        | PortableProbeError::ToolRoot(_) => "game.bootstrap",
        PortableProbeError::OperationAndSessionCleanup { operation, .. } => {
            if let Some(source) = session_operation_app_error(operation.as_ref()) {
                source.stage()
            } else if let Some(source) = session_operation_portable_error(operation.as_ref()) {
                portable_stage(source)
            } else {
                "game.cleanup"
            }
        }
        #[cfg(target_os = "windows")]
        PortableProbeError::Registry { .. } => "game.discover.manager",
        #[cfg(target_os = "windows")]
        PortableProbeError::ManagerCommand { .. } => "game.bootstrap.manager",
        #[cfg(target_os = "windows")]
        PortableProbeError::Target { .. } => "game.target",
        #[cfg(target_os = "windows")]
        PortableProbeError::OperationAndAdbCleanup { .. } => "game.cleanup",
    }
}

fn portable_message(code: AppErrorCode) -> &'static str {
    match code {
        AppErrorCode::SettingsInvalid => "游戏连接设置未通过严格校验",
        AppErrorCode::EmulatorNotFound => "没有找到符合设置且可验证的 模拟器 目标",
        AppErrorCode::EmulatorManagerAmbiguous => {
            "发现多个可验证的 模拟器管理器 安装，无法唯一选择"
        }
        AppErrorCode::EmulatorInstanceAmbiguous => "发现多个可用的 模拟器 目标，需要指定唯一实例",
        AppErrorCode::AdbTargetUnavailable => "工具持有的 ADB 无法连接或验证目标实例",
        AppErrorCode::GameNotReady => "请启动碧蓝航线、完成登录并进入港区后重试",
        AppErrorCode::RuntimeIncompatible => "当前游戏运行环境与只读运行态不兼容",
        AppErrorCode::RuntimeBootstrapFailed => "游戏只读运行态连接未能建立",
        AppErrorCode::CapabilityMissing => "当前游戏运行态缺少完整读取能力",
        AppErrorCode::FullCheckFailed => "游戏状态未通过完整一致性检查",
        _ => "完整游戏状态读取失败",
    }
}

fn portable_context(source: &PortableProbeError) -> Vec<(String, String)> {
    match source {
        PortableProbeError::InvalidOption { field, .. } => {
            vec![("field".to_owned(), (*field).to_owned())]
        }
        PortableProbeError::ProfilePackageMismatch {
            configured,
            profile,
        } => vec![
            ("configured_package".to_owned(), configured.clone()),
            ("profile_package".to_owned(), profile.clone()),
        ],
        PortableProbeError::GameNotReady { detail, source } => {
            let mut context = runtime_context(source.as_ref());
            context.push(("runtime_detail".to_owned(), detail.clone()));
            context
        }
        PortableProbeError::Runtime(source) => runtime_context(source),
        PortableProbeError::RuntimeShutdownFailed {
            journal_path,
            game_restarted,
            ..
        } => vec![
            ("adapter_stage".to_owned(), "session.shutdown".to_owned()),
            (
                "runtime_journal".to_owned(),
                journal_path.display().to_string(),
            ),
            ("game_restarted".to_owned(), game_restarted.to_string()),
        ],
        PortableProbeError::Adb { stage, .. } => {
            vec![("adapter_stage".to_owned(), (*stage).to_owned())]
        }
        PortableProbeError::Io { stage, path, .. } => vec![
            ("adapter_stage".to_owned(), (*stage).to_owned()),
            ("path".to_owned(), path.display().to_string()),
        ],
        #[cfg(target_os = "windows")]
        PortableProbeError::ManagerCommand { stage, .. }
        | PortableProbeError::Target { stage, .. } => {
            vec![("adapter_stage".to_owned(), (*stage).to_owned())]
        }
        #[cfg(target_os = "windows")]
        PortableProbeError::OperationAndAdbCleanup { .. } => {
            vec![("cleanup".to_owned(), "failed".to_owned())]
        }
        PortableProbeError::OperationAndSessionCleanup { operation, cleanup } => {
            let mut context: Vec<(String, String)> =
                session_operation_portable_error(operation.as_ref())
                    .map(portable_context)
                    .unwrap_or_default();
            context.push(("cleanup_stage".to_owned(), "game.cleanup".to_owned()));
            context.push((
                "cleanup_code".to_owned(),
                classify_portable_error(cleanup.as_ref())
                    .as_str()
                    .to_owned(),
            ));
            for (key, value) in portable_context(cleanup.as_ref()) {
                if key != "cleanup" {
                    context.push((format!("cleanup_{key}"), value));
                }
            }
            context
        }
        _ => Vec::new(),
    }
}

pub(super) fn runtime_context(source: &RuntimeProbeError) -> Vec<(String, String)> {
    match source {
        RuntimeProbeError::ProbeFailed {
            source,
            journal_path,
            cleanup_error,
        } => {
            let mut context: Vec<(String, String)> = runtime_context(source);
            context.push((
                "runtime_journal".to_owned(),
                journal_path.display().to_string(),
            ));
            if cleanup_error.is_some()
                || matches!(source.as_ref(), RuntimeProbeError::Cleanup { .. })
            {
                context.push(("cleanup".to_owned(), "failed".to_owned()));
            }
            context
        }
        RuntimeProbeError::RuntimeClient(source) => {
            vec![("runtime_code".to_owned(), source.code().to_owned())]
        }
        RuntimeProbeError::InvalidOption { field, .. } => {
            vec![("field".to_owned(), (*field).to_owned())]
        }
        RuntimeProbeError::InvalidToolAsset { path, .. } => {
            vec![("path".to_owned(), path.display().to_string())]
        }
        RuntimeProbeError::Io { stage, path, .. } => vec![
            ("adapter_stage".to_owned(), (*stage).to_owned()),
            ("path".to_owned(), path.display().to_string()),
        ],
        RuntimeProbeError::HostCommand { stage, .. }
        | RuntimeProbeError::DeviceCommand { stage, .. }
        | RuntimeProbeError::InvalidOutput { stage, .. }
        | RuntimeProbeError::Json { stage, .. } => {
            vec![("adapter_stage".to_owned(), (*stage).to_owned())]
        }
        RuntimeProbeError::LoaderRejected {
            exit_code,
            code,
            process_id,
            target_state,
            ..
        } => vec![
            ("adapter_stage".to_owned(), "loader.execute".to_owned()),
            ("runtime_code".to_owned(), code.clone()),
            ("exit_code".to_owned(), exit_code.to_string()),
            ("process_id".to_owned(), process_id.to_string()),
            ("target_state".to_owned(), target_state.clone()),
        ],
        RuntimeProbeError::UnloaderRejected {
            exit_code,
            code,
            process_id,
            ..
        } => vec![
            ("adapter_stage".to_owned(), "unloader.execute".to_owned()),
            ("runtime_code".to_owned(), code.clone()),
            ("exit_code".to_owned(), exit_code.to_string()),
            ("process_id".to_owned(), process_id.to_string()),
        ],
        _ => Vec::new(),
    }
}

pub(super) fn execution_session_error(message: &'static str) -> AppError {
    AppError::from_source(
        "plan.execute.session",
        AppErrorCode::RuntimeBootstrapFailed,
        message,
        std::io::Error::other("execution session is not active"),
    )
    .with_context("component", "game")
    .with_context("access", "write")
}

pub(super) fn execution_session_configuration_changed_error() -> AppError {
    AppError::from_source(
        "plan.execute.session",
        AppErrorCode::SettingsInvalid,
        "执行期间游戏连接设置发生变化，已阻止切换认证会话",
        std::io::Error::other("execution session options changed while bound"),
    )
    .with_context("component", "game")
    .with_context("access", "write")
    .with_context("configuration_changed", "true")
}

pub(super) fn execution_bound_session_unusable_error() -> AppError {
    AppError::from_source(
        "plan.execute.session",
        AppErrorCode::RuntimeBootstrapFailed,
        "执行期间的游戏连接已经失效，已阻止自动建立新认证会话",
        std::io::Error::other("bound execution session is not usable"),
    )
    .with_context("component", "game")
    .with_context("access", "write")
    .with_context("session_unusable", "true")
    .with_context("automatic_reconnect", "blocked")
}

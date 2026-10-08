//! 组合应用服务与具体适配器，并固定工具根目录内的资源位置。

use std::path::Path;

use thiserror::Error;

use crate::adapters::check_history::JsonCheckHistoryPort;
#[cfg(target_os = "windows")]
use crate::adapters::device::PortableGamePort;
#[cfg(target_os = "windows")]
use crate::adapters::device::emulator_instance_catalog::EmulatorManagerInstanceCatalogPort;
use crate::adapters::diagnostic_open::ValidatedDiagnosticOpenPort;
use crate::adapters::execution_history::JsonExecutionHistoryPort;
use crate::adapters::history_catalog::JsonHistoryCatalogPort;
use crate::adapters::log_catalog::JsonLogCatalogPort;
use crate::adapters::settings_port::JsonSettingsPort;
use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::adapters::workbook::{
    XlsxExecutionResultsPort, XlsxLayoutPreviewPort, XlsxLayoutUpgradePort, XlsxWorkbookBackupPort,
    XlsxWorkbookCatalogPort, XlsxWorkbookOpenPort, XlsxWorkbookPort,
    create_workbook_generation_port,
};
#[cfg(target_os = "windows")]
use crate::application::ExecutionPort;
use crate::application::{
    AgentManagementService, AppErrorCode, LayoutModelError, LayoutService, WorkbookCheckService,
    WorkbookExecuteService, WorkbookProjectionV4, WorkbookSyncService, WorkspaceService,
};

mod doctor;
mod gui_tasks;
mod operation_log;

pub use crate::adapters::release::verify_release;
pub use doctor::open_offline_doctor;

pub use operation_log::{
    OperationConfiguration, OperationContext, OperationIdentity, OperationRecord,
    OperationSettings, OperationSignals,
};

pub use gui_tasks::bootstrap_gui_task_factory;

const WORKBOOK_LAYOUT_PATH: &str = "workbook-layout.xlsx";

/// 从受控历史目录读取单个文件，可选择已有顶层字段。
pub fn read_history_details(
    root: &Path,
    filename: &str,
    fields: Option<&[String]>,
) -> Result<serde_json::Value, crate::application::AppError> {
    let (root, relative) = detail_source(
        root,
        filename,
        crate::application::HISTORY_DIRECTORY,
        AppErrorCode::HistoryReadFailed,
    )?;
    JsonHistoryCatalogPort::new(root).read_details(&relative, fields)
}

/// 从受控日志目录读取单个文件，按状态筛选后保留末尾记录。
pub fn read_log_details(
    root: &Path,
    filename: &str,
    tail: Option<usize>,
    status: Option<&str>,
) -> Result<serde_json::Value, crate::application::AppError> {
    let (root, relative) = detail_source(
        root,
        filename,
        crate::application::LOG_DIRECTORY,
        AppErrorCode::LogReadFailed,
    )?;
    JsonLogCatalogPort::new(root).read_details(&relative, tail, status)
}

fn detail_source(
    root: &Path,
    filename: &str,
    directory: &str,
    code: AppErrorCode,
) -> Result<(ToolRoot, std::path::PathBuf), crate::application::AppError> {
    let resolve = || {
        if filename.is_empty() || filename.trim() != filename || filename.contains(['/', '\\']) {
            return Err(ToolRootError::InvalidRelativePath {
                path: filename.into(),
                message: format!("必须指定 {directory} 内的单个文件名"),
            });
        }
        let root = ToolRoot::open(root)?;
        let relative = root.validated_relative_path(&Path::new(directory).join(filename))?;
        Ok((root, relative))
    };
    resolve().map_err(|source| {
        crate::application::AppError::from_source(
            "diagnostics.show",
            code,
            "诊断文件路径校验失败",
            source,
        )
        .with_context("file", filename)
    })
}

/// 直接查询游戏对象，不加载布局、不创建工作簿。
pub fn query_game(
    tool_root_path: &Path,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
    query: &crate::application::GameQuery,
) -> Result<crate::application::GameQueryReport, crate::application::AppError> {
    #[cfg(target_os = "windows")]
    {
        let root = open_tool_root(tool_root_path).map_err(|error| {
            crate::application::AppError::from_source(
                "game.query.bootstrap",
                AppErrorCode::ApplicationInitializationFailed,
                "游戏查询初始化失败",
                error,
            )
        })?;
        game_port_for_operation(root, selected_instance, related_logs, configuration).query(query)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (
            tool_root_path,
            selected_instance,
            related_logs,
            configuration,
            query,
        );
        Err(crate::application::AppError::from_source(
            "game.query.bootstrap",
            AppErrorCode::RuntimeIncompatible,
            "游戏对象查询需要 Windows 设备运行环境",
            std::io::Error::new(std::io::ErrorKind::Unsupported, "仅支持 Windows"),
        ))
    }
}

/// 建立应用服务前可能出现的组合根错误。
#[derive(Debug, Error)]
pub enum BootstrapError {
    /// 可执行文件所在目录不满足工具根目录约束。
    #[error("初始化工具根目录失败: {source}")]
    ToolRoot {
        #[source]
        source: ToolRootError,
    },
    /// 程序内置的布局投影注册表自身不完整。
    #[error("初始化工作簿布局注册表失败: {source}")]
    WorkbookLayoutRegistry {
        #[source]
        source: LayoutModelError,
    },
}

impl BootstrapError {
    /// 返回 CLI 和日志可以稳定匹配的初始化错误码。
    pub const fn code(&self) -> AppErrorCode {
        AppErrorCode::ApplicationInitializationFailed
    }
}

fn open_tool_root(tool_root_path: &Path) -> Result<ToolRoot, BootstrapError> {
    ToolRoot::open(tool_root_path).map_err(|source| BootstrapError::ToolRoot { source })
}

fn workbook_layout_registry() -> Result<crate::application::WorkbookLayoutRegistry, BootstrapError>
{
    WorkbookProjectionV4::layout_registry()
        .map_err(|source| BootstrapError::WorkbookLayoutRegistry { source })
}

struct OperationAssembly {
    tool_root: ToolRoot,
    #[cfg(target_os = "windows")]
    execution: Box<dyn ExecutionPort>,
    #[cfg(not(target_os = "windows"))]
    _execution_absent: (),
}

#[cfg(target_os = "windows")]
fn game_port_for_operation(
    tool_root: ToolRoot,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> PortableGamePort {
    let port = PortableGamePort::new(tool_root);
    let port = match configuration.settings() {
        Ok(settings) => port.with_operation_settings(settings.clone()),
        Err(summary) => port.with_unavailable_settings(summary.to_owned()),
    };
    let port = match selected_instance {
        Some(instance) => port.with_selected_instance(instance.to_owned()),
        None => port,
    };
    match related_logs {
        Some(related) => port.with_related_logs(related.clone()),
        None => port,
    }
}

fn assemble_operation(
    tool_root_path: &Path,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<OperationAssembly, BootstrapError> {
    let tool_root = open_tool_root(tool_root_path)?;
    #[cfg(target_os = "windows")]
    let execution: Box<dyn ExecutionPort> = Box::new(game_port_for_operation(
        tool_root.clone(),
        selected_instance,
        related_logs,
        configuration,
    ));
    #[cfg(not(target_os = "windows"))]
    {
        let _ = selected_instance;
        let _ = related_logs;
        let _ = configuration;
    }
    Ok(OperationAssembly {
        tool_root,
        #[cfg(target_os = "windows")]
        execution,
        #[cfg(not(target_os = "windows"))]
        _execution_absent: (),
    })
}

fn workbook_port(tool_root: &ToolRoot) -> Result<XlsxWorkbookPort, BootstrapError> {
    let registry = workbook_layout_registry()?;
    let layout_path = tool_root.as_path().join(WORKBOOK_LAYOUT_PATH);
    Ok(XlsxWorkbookPort::new(
        tool_root.clone(),
        layout_path,
        registry,
    ))
}

/// 组合直接装备操作，只装载游戏连接和现有操作日志。
pub fn bootstrap_direct_action_service(
    tool_root_path: &Path,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<crate::application::DirectActionService, BootstrapError> {
    let assembly = assemble_operation(
        tool_root_path,
        selected_instance,
        related_logs,
        configuration,
    )?;
    #[cfg(target_os = "windows")]
    let execution = Some(assembly.execution);
    #[cfg(not(target_os = "windows"))]
    let execution = {
        let _ = assembly;
        None
    };
    Ok(crate::application::DirectActionService::new(execution))
}

/// 组合生成用例。不装载执行备份、历史或结果写回。
pub fn bootstrap_sync_service(
    tool_root_path: &Path,
) -> Result<WorkbookSyncService, BootstrapError> {
    let configuration = OperationConfiguration::load(tool_root_path);
    bootstrap_sync_service_with_instance(tool_root_path, None, None, &configuration)
}

/// 为单次生成组合服务；具体实例选择只作用于该服务的游戏连接。
pub fn bootstrap_sync_service_with_instance(
    tool_root_path: &Path,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<WorkbookSyncService, BootstrapError> {
    let assembly = assemble_operation(
        tool_root_path,
        selected_instance,
        related_logs,
        configuration,
    )?;
    let generation_preferences = configuration.preferences();
    let acquisition = std::sync::Arc::new(crate::adapters::workbook::ShipAcquisition::new(
        assembly.tool_root.clone(),
    ));
    let mut service = WorkbookSyncService::new(
        Box::new(workbook_port(&assembly.tool_root)?),
        None,
        create_workbook_generation_port(
            assembly.tool_root.clone(),
            acquisition,
            generation_preferences,
        ),
    );
    #[cfg(target_os = "windows")]
    {
        service = service.with_execution_port(assembly.execution);
    }
    Ok(service)
}

/// 组合计划检查用例。不装载生成器、执行备份或执行历史。
pub fn bootstrap_check_service_with_instance(
    tool_root_path: &Path,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<WorkbookCheckService, BootstrapError> {
    let assembly = assemble_operation(
        tool_root_path,
        selected_instance,
        related_logs,
        configuration,
    )?;
    let documents = crate::adapters::workbook::WorkbookDocuments::new();
    let mut service = WorkbookCheckService::new(
        Box::new(workbook_port(&assembly.tool_root)?.with_documents(documents.clone())),
        None,
        Box::new(JsonCheckHistoryPort::new(assembly.tool_root.clone())),
        Box::new(
            XlsxExecutionResultsPort::new(assembly.tool_root.clone())
                .with_acquisition(std::sync::Arc::new(
                    crate::adapters::workbook::ShipAcquisition::new(assembly.tool_root.clone()),
                ))
                .with_documents(documents),
        ),
    );
    #[cfg(target_os = "windows")]
    {
        service = service.with_execution_port(assembly.execution);
    }
    Ok(service)
}

/// 组合执行用例。不装载工作簿生成器或检查历史。
pub fn bootstrap_execute_service_with_instance(
    tool_root_path: &Path,
    selected_instance: Option<&str>,
    related_logs: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<WorkbookExecuteService, BootstrapError> {
    let assembly = assemble_operation(
        tool_root_path,
        selected_instance,
        related_logs,
        configuration,
    )?;
    let acquisition = std::sync::Arc::new(crate::adapters::workbook::ShipAcquisition::new(
        assembly.tool_root.clone(),
    ));
    #[cfg(target_os = "windows")]
    let execution = Some(assembly.execution);
    #[cfg(not(target_os = "windows"))]
    let execution = None;
    let documents = crate::adapters::workbook::WorkbookDocuments::new();
    Ok(WorkbookExecuteService::new(
        Box::new(workbook_port(&assembly.tool_root)?.with_documents(documents.clone())),
        execution,
        Box::new(XlsxWorkbookBackupPort::new(assembly.tool_root.clone())),
        Box::new(JsonExecutionHistoryPort::new(assembly.tool_root.clone())),
        Box::new(
            XlsxExecutionResultsPort::new(assembly.tool_root)
                .with_acquisition(acquisition)
                .with_documents(documents),
        ),
    ))
}

/// 用同一次操作已经校验的设置组合代理管理。设备探测由适配器端口执行。
#[cfg(target_os = "windows")]
pub fn bootstrap_agent_management(
    tool_root_path: &Path,
    related: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> AgentManagementService {
    let settings = configuration.settings().cloned().map_err(ToOwned::to_owned);
    AgentManagementService::new(Box::new(
        crate::adapters::device::ConfiguredAgentManagement::new(
            tool_root_path.to_path_buf(),
            settings,
            related.cloned(),
        ),
    ))
}

/// 组装获取方式缓存更新。名称来自布局稳定键，网络和进度交给共享用例。
pub fn update_ship_acquisition_cache(
    tool_root_path: &Path,
    workbook_name: &str,
    mode: crate::application::AcquisitionUpdateMode,
) -> Result<Vec<crate::application::AcquisitionCacheUpdate>, String> {
    update_ship_acquisition_cache_with(tool_root_path, workbook_name, mode, &mut |_| {}, &|| false)
}

/// 命令行资料更新。进度写入当前操作日志，取消读取调用方信号。
pub fn update_ship_acquisition_cache_logged(
    tool_root_path: &Path,
    workbook_name: &str,
    mode: crate::application::AcquisitionUpdateMode,
    context: &OperationContext<'_>,
) -> Result<Vec<crate::application::AcquisitionCacheUpdate>, String> {
    let cancelled = context.cancellation_flag();
    let mut progress_note = None;
    let report = update_ship_acquisition_cache_with(
        tool_root_path,
        workbook_name,
        mode,
        &mut |progress| {
            if progress_note.is_some() {
                return;
            }
            if let Err(error) = context.report_activity(progress.units, progress.message.clone()) {
                progress_note = Some(error.to_string());
            }
        },
        &|| cancelled.load(std::sync::atomic::Ordering::Acquire),
    )?;
    if let Some(note) = progress_note {
        eprintln!("进度记录失败：{note}");
    }
    Ok(report)
}

pub(crate) fn update_ship_acquisition_cache_with(
    tool_root_path: &Path,
    workbook_name: &str,
    mode: crate::application::AcquisitionUpdateMode,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<crate::application::AcquisitionCacheUpdate>, String> {
    let tool_root = open_tool_root(tool_root_path).map_err(|error| error.to_string())?;
    let workbook = tool_root
        .existing_workbook(workbook_name)
        .map_err(|error| error.to_string())?;
    let path = tool_root
        .existing_file(workbook.relative_path())
        .map_err(|error| error.to_string())?;
    let port = crate::adapters::workbook::ShipAcquisition::new(tool_root);
    let names = port.ship_titles(&path).map_err(error_chain)?;
    crate::application::update_ship_acquisition_cache(&port, &names, mode, progress, is_cancelled)
        .map_err(error_chain)
}

fn error_chain(error: impl std::error::Error) -> String {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(current) = source {
        detail.push_str("\n原因：");
        detail.push_str(&current.to_string());
        source = current.source();
    }
    detail
}

/// 组合工作区查询服务，不加载布局注册表、生成器或游戏端口。
pub fn bootstrap_workspace_service(
    tool_root_path: &Path,
) -> Result<WorkspaceService, BootstrapError> {
    let configuration = OperationConfiguration::load(tool_root_path);
    bootstrap_workspace_service_with_configuration(tool_root_path, &configuration)
}

/// 用同一次操作已经校验的设置组合工作区查询，不再打开设置文件。
pub fn bootstrap_workspace_service_with_configuration(
    tool_root_path: &Path,
    configuration: &OperationConfiguration,
) -> Result<WorkspaceService, BootstrapError> {
    let tool_root = open_tool_root(tool_root_path)?;
    #[cfg(target_os = "windows")]
    let emulator_instance_catalog = Some(Box::new(match configuration.settings() {
        Ok(settings) => EmulatorManagerInstanceCatalogPort::from_device(settings.device()),
        Err(summary) => EmulatorManagerInstanceCatalogPort::unavailable(summary),
    })
        as Box<dyn crate::application::EmulatorInstanceCatalogPort>);
    #[cfg(not(target_os = "windows"))]
    let emulator_instance_catalog = None;
    Ok(WorkspaceService::with_ports(
        Box::new(XlsxWorkbookOpenPort::new(tool_root.clone())),
        Box::new(XlsxWorkbookCatalogPort::new(tool_root.clone())),
        Box::new(JsonHistoryCatalogPort::new(tool_root.clone())),
        Box::new(match configuration.settings() {
            Ok(settings) => JsonSettingsPort::from_snapshot(tool_root.clone(), settings.clone()),
            Err(summary) => {
                JsonSettingsPort::from_read_failure(tool_root.clone(), summary.to_owned())
            }
        }),
        Box::new(JsonLogCatalogPort::new(tool_root.clone())),
        Box::new(ValidatedDiagnosticOpenPort::new(tool_root)),
        emulator_instance_catalog,
    ))
}

/// 组合布局检查、升级和预览服务。
pub fn bootstrap_layout_service(tool_root_path: &Path) -> Result<LayoutService, BootstrapError> {
    let tool_root = open_tool_root(tool_root_path)?;
    let registry = workbook_layout_registry()?;
    let layout_path = tool_root.as_path().join(WORKBOOK_LAYOUT_PATH);
    Ok(LayoutService::with_ports(
        Box::new(XlsxWorkbookPort::new(
            tool_root.clone(),
            layout_path,
            registry.clone(),
        )),
        Box::new(XlsxLayoutUpgradePort::new(tool_root.clone(), registry)),
        Box::new(XlsxLayoutPreviewPort::new(tool_root)),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::adapters::tool_root::ToolRootError;
    use crate::application::{AppErrorCode, LayoutModelError};

    use super::{BootstrapError, bootstrap_layout_service, bootstrap_workspace_service};

    #[test]
    fn uses_the_shared_initialization_code_for_each_boundary() {
        let tool_root_error = BootstrapError::ToolRoot {
            source: ToolRootError::UnsafePath {
                path: PathBuf::from("tool-root"),
                message: "测试路径不安全".to_owned(),
            },
        };
        assert_eq!(
            tool_root_error.code(),
            AppErrorCode::ApplicationInitializationFailed
        );

        let registry_error = BootstrapError::WorkbookLayoutRegistry {
            source: LayoutModelError::RegistryEmpty {
                component: "工作表",
            },
        };
        assert_eq!(
            registry_error.code(),
            AppErrorCode::ApplicationInitializationFailed
        );
    }

    #[test]
    fn composes_the_production_layout_port_from_the_tool_root() {
        let service = bootstrap_layout_service(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();

        let report = service.check_layout().unwrap();

        assert_eq!(report.sheet_count(), 10);
        assert_eq!(report.field_count(), 403);
        assert_eq!(report.enum_option_count(), 55);
        assert_eq!(report.style_count(), 6);
    }

    #[test]
    fn workspace_catalog_does_not_require_the_layout_file() {
        let root =
            std::env::temp_dir().join(format!("azlw-workspace-boundary-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let workspace = bootstrap_workspace_service(&root).unwrap();
        assert_eq!(workspace.list_workbooks().unwrap().count(), 0);
        assert!(!root.join("workbook-layout.xlsx").exists());
        let layout = bootstrap_layout_service(&root).unwrap();
        assert!(layout.check_layout().is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn generation_assembly_keeps_one_verified_settings_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "azlw-operation-settings-snapshot-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let initial = include_str!("../settings.json");
        std::fs::write(root.join("settings.json"), initial).unwrap();
        let configuration = super::OperationConfiguration::load(&root);
        let identity = configuration.source_sha256().unwrap().to_owned();
        let updated = initial
            .replace(
                "\"connect_timeout_seconds\": 30",
                "\"connect_timeout_seconds\": 60",
            )
            .replace(
                "\"ship_acquisition_enabled\": false",
                "\"ship_acquisition_enabled\": true",
            )
            .replace("\"detailed\": true", "\"detailed\": false");
        std::fs::write(root.join("settings.json"), updated).unwrap();

        let log_settings = configuration.operation_settings();
        assert!(log_settings.detailed_diagnostics);
        assert!(log_settings.read_error.is_none());
        assert!(
            !configuration
                .preferences()
                .unwrap()
                .ship_acquisition_enabled
        );
        assert_eq!(
            configuration
                .settings()
                .unwrap()
                .runtime()
                .connect_timeout_seconds(),
            30
        );
        let workspace =
            super::bootstrap_workspace_service_with_configuration(&root, &configuration).unwrap();
        assert!(workspace.read_preferences().unwrap().detailed_diagnostics);
        let service =
            super::bootstrap_sync_service_with_instance(&root, None, None, &configuration).unwrap();
        assert_eq!(service.ship_acquisition_enabled_for_test(), Ok(false));
        #[cfg(target_os = "windows")]
        {
            let tool_root = crate::adapters::tool_root::ToolRoot::open(&root).unwrap();
            let port = super::game_port_for_operation(tool_root, None, None, &configuration);
            assert_eq!(port.connect_timeout_seconds_for_test().unwrap(), 30);
        }

        let next = super::OperationConfiguration::load(&root);
        assert_ne!(next.source_sha256().unwrap(), identity);
        assert!(!next.operation_settings().detailed_diagnostics);
        assert!(next.preferences().unwrap().ship_acquisition_enabled);
        assert_eq!(
            next.settings().unwrap().runtime().connect_timeout_seconds(),
            60
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(all(test, target_os = "windows"))]
mod emulator_live_tests {
    /// 显式选择实例并复用桌面应用服务，只读取游戏并发布一个新工作簿。
    #[test]
    #[ignore = "需要已登录实例及 AZLW_TOOL_ROOT、AZLW_EMULATOR_SELECTION、AZLW_GENERATED_NAME；执行真实同步"]
    fn selected_emulator_generates_workbook() {
        let root =
            std::path::PathBuf::from(std::env::var_os("AZLW_TOOL_ROOT").expect("AZLW_TOOL_ROOT"));
        let selection = std::env::var("AZLW_EMULATOR_SELECTION").expect("AZLW_EMULATOR_SELECTION");
        let name = std::env::var("AZLW_GENERATED_NAME").expect("AZLW_GENERATED_NAME");
        let configuration = super::OperationConfiguration::load(&root);
        let mut service = super::bootstrap_sync_service_with_instance(
            &root,
            Some(&selection),
            None,
            &configuration,
        )
        .unwrap();
        let outcome = service
            .generate_workbook_with_progress(
                Some(&name),
                &mut |progress| {
                    println!("{}", progress.message);
                },
                &|| false,
            )
            .unwrap_or_else(|error| panic!("同步失败: {error:?}"));
        let crate::application::WorkbookGenerationOutcome::Completed(report) = outcome else {
            panic!("同步后的会话清理未完成");
        };
        let output = root.join(report.output_path());
        assert!(output.is_file(), "工作簿未发布: {}", output.display());
        assert!(std::fs::metadata(&output).unwrap().len() > 0);
        println!(
            "workbook={} package_sha256={}",
            output.display(),
            report.output_package_sha256()
        );
    }
}

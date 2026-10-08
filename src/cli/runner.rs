//! 路由主程序命令并输出稳定的命令结果。

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};

use azur_lane_workbook::adapters::release::{ReleaseVerificationReport, ensure_extracted_release};
use azur_lane_workbook::adapters::tool_root::open_or_create_resource_root;
use azur_lane_workbook::application::{
    CheckAndSaveOutcome, DoctorReport, ExecutionReportStatus, HistoryCatalogReport,
    LayoutCheckReport, LayoutPreviewReport, LayoutUpgradeReport, LogCatalogReport,
    OperationTerminal, SettingsSummaryReport, WorkbookCatalogReport, WorkbookExecuteOutcome,
    WorkbookGenerationOutcome, WorkbookOpenReport,
};
use azur_lane_workbook::bootstrap::{
    OperationConfiguration, OperationContext, OperationIdentity, OperationRecord,
    bootstrap_check_service_with_instance, bootstrap_execute_service_with_instance,
    bootstrap_gui_task_factory, bootstrap_layout_service, bootstrap_sync_service_with_instance,
    bootstrap_workspace_service, bootstrap_workspace_service_with_configuration,
    open_offline_doctor, update_ship_acquisition_cache_logged, verify_release,
};
use azur_lane_workbook::interfaces::native_gui::run_native_gui;

use super::command::{
    AgentCommand, CHECK_COMMAND, CHECK_SAVE_COMMAND, EXECUTE_COMMAND, OPEN_COMMAND, StartupCommand,
};
use super::diagnostics::{
    finished_execution_suggestion, format_bootstrap_error, format_check_and_save_error,
    format_check_save_incomplete, format_doctor_error, format_doctor_tool_root_error,
    format_execution_error, format_generation_error, format_history_catalog_error,
    format_layout_check_bootstrap_error, format_layout_check_error, format_layout_preview_error,
    format_layout_upgrade_error, format_log_catalog_error, format_operation_log_failure,
    format_published_generation_cleanup, format_release_error, format_settings_error,
    format_workbook_catalog_error, format_workbook_check_error, format_workbook_open_error,
    format_workbook_selection_app_error,
};

const EXECUTION_NOT_SUCCESSFUL_EXIT_CODE: u8 = 2;

pub(crate) fn run(command: StartupCommand) -> Result<CommandOutcome, String> {
    // 帮助可独立运行，不解包发布资源、不读取配置或建立游戏连接。
    if let StartupCommand::Help(topic) = &command {
        println!("{}", super::help::render(topic.as_deref())?);
        return Ok(CommandOutcome::Successful);
    }
    let tool_root: PathBuf = executable_tool_root()?;
    if matches!(command, StartupCommand::Mcp) {
        return azur_lane_workbook::interfaces::mcp::serve(tool_root)
            .map(|()| CommandOutcome::Successful);
    }
    let (command, instance) = match command {
        StartupCommand::WithInstance(instance, command) => (*command, Some(instance)),
        command => (command, None),
    };
    let instance = instance.as_deref();
    // 这些命令的结果约定不发布 data 下的文件，因此不创建操作日志。
    let log_command = !matches!(
        command,
        StartupCommand::Gui
            | StartupCommand::WithInstance(_, _)
            | StartupCommand::Doctor
            | StartupCommand::History
            | StartupCommand::ArtifactDetails(_)
            | StartupCommand::Workbooks
            | StartupCommand::Logs
            | StartupCommand::Open(_)
    );
    let (operation_name, operation_workbook) = cli_operation_identity(&command);
    let operation_name = operation_name.to_owned();
    let operation_workbook = operation_workbook.map(str::to_owned);
    let configuration = log_command.then(|| OperationConfiguration::load(&tool_root));
    let context = configuration.as_ref().map(|configuration| {
        OperationContext::new(
            &tool_root,
            OperationIdentity {
                name: &operation_name,
                workbook: operation_workbook.as_deref(),
            },
            instance,
            &configuration.operation_settings(),
            &CLI_SIGNALS,
        )
    });
    install_cli_cancel();
    let related_logs = context.as_ref().and_then(OperationContext::related_logs);
    let mut terminal = OperationTerminal::Succeeded;
    let outcome = (|| match command {
        StartupCommand::ArtifactDetails(request) => {
            super::artifact_details::run(&tool_root, request).map(|()| CommandOutcome::Successful)
        }
        StartupCommand::Help(_) => unreachable!("帮助已在初始化前处理"),
        StartupCommand::Mcp => unreachable!("MCP 已在命令日志初始化前处理"),
        StartupCommand::WithInstance(_, _) => Err("实例参数不能嵌套".to_owned()),
        StartupCommand::Gui => run_native_gui(bootstrap_gui_task_factory(tool_root))
            .map_err(|error| error.to_string())
            .map(|()| CommandOutcome::Successful),
        StartupCommand::VerifyRelease => {
            let report: ReleaseVerificationReport =
                verify_release(&tool_root).map_err(|error| format_release_error(&error))?;
            let json: String = serde_json::to_string(&report)
                .map_err(|error| format!("编码发布校验结果失败: {error}"))?;
            println!("{json}");
            Ok(CommandOutcome::Successful)
        }
        StartupCommand::Doctor => run_doctor(&tool_root).map(|()| CommandOutcome::Successful),
        StartupCommand::LayoutCheck => {
            run_layout_check(&tool_root).map(|()| CommandOutcome::Successful)
        }
        StartupCommand::LayoutUpgrade => {
            run_layout_upgrade(&tool_root).map(|()| CommandOutcome::Successful)
        }
        StartupCommand::LayoutPreview => {
            run_layout_preview(&tool_root).map(|()| CommandOutcome::Successful)
        }
        StartupCommand::Generate(requested_name) => run_generate(
            &tool_root,
            requested_name.as_deref(),
            instance,
            related_logs.as_ref(),
            operation_configuration(&configuration)?,
            &mut terminal,
        )
        .map(|()| CommandOutcome::Successful),
        StartupCommand::Check(workbook_name) => run_check(
            &tool_root,
            &workbook_name,
            instance,
            related_logs.as_ref(),
            operation_configuration(&configuration)?,
        )
        .map(|()| CommandOutcome::Successful),
        StartupCommand::CheckSave(workbook_name) => run_check_save(
            &tool_root,
            &workbook_name,
            instance,
            related_logs.as_ref(),
            operation_configuration(&configuration)?,
            &mut terminal,
        )
        .map(|()| CommandOutcome::Successful),
        StartupCommand::Execute(workbook_name) => run_execute(
            &tool_root,
            &workbook_name,
            instance,
            related_logs.as_ref(),
            operation_configuration(&configuration)?,
            &mut terminal,
        ),
        StartupCommand::Open(workbook_name) => {
            run_open(&tool_root, &workbook_name).map(|()| CommandOutcome::Successful)
        }
        StartupCommand::Workbooks => run_workbooks(&tool_root).map(|()| CommandOutcome::Successful),
        StartupCommand::History => run_history(&tool_root).map(|()| CommandOutcome::Successful),
        StartupCommand::Settings => {
            run_settings(&tool_root, operation_configuration(&configuration)?)
                .map(|()| CommandOutcome::Successful)
        }
        StartupCommand::Preferences => {
            print_preferences(&tool_root, operation_configuration(&configuration)?)
                .map(|()| CommandOutcome::Successful)
        }
        StartupCommand::SetPreference(key, value) => set_preference(
            &tool_root,
            &key,
            &value,
            operation_configuration(&configuration)?,
        )
        .map(|()| CommandOutcome::Successful),
        StartupCommand::Instances => {
            let service = bootstrap_workspace_service_with_configuration(
                &tool_root,
                operation_configuration(&configuration)?,
            )
            .map_err(|error| format_bootstrap_error("实例查询", &error))?;
            let report = service
                .list_emulator_instances()
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&report).map_err(|error| error.to_string())?
            );
            Ok(CommandOutcome::Successful)
        }
        StartupCommand::Agent(action, instance) => run_agent(
            &tool_root,
            action,
            instance.as_deref(),
            related_logs.as_ref(),
            operation_configuration(&configuration)?,
        ),
        StartupCommand::Logs => run_logs(&tool_root).map(|()| CommandOutcome::Successful),
        StartupCommand::Query(query) => {
            let report = azur_lane_workbook::bootstrap::query_game(
                &tool_root,
                instance,
                related_logs.as_ref(),
                operation_configuration(&configuration)?,
                &query,
            )
            .map_err(|error| {
                super::diagnostics::format_game_operation_error("游戏查询失败", &error)
            })?;
            println!(
                "{}",
                serde_json::to_string(&report)
                    .map_err(|error| format!("编码查询结果失败: {error}"))?
            );
            Ok(CommandOutcome::Successful)
        }
        StartupCommand::EquipmentActions(input, apply) => {
            let batch = input.load()?;
            let mut service = azur_lane_workbook::bootstrap::bootstrap_direct_action_service(
                &tool_root,
                instance,
                related_logs.as_ref(),
                operation_configuration(&configuration)?,
            )
            .map_err(|error| format_bootstrap_error("装备操作", &error))?;
            let result = if apply {
                service.apply_with_cancellation(&batch, &CLI_SIGNALS)
            } else {
                service.check(&batch)
            }
            .map_err(|error| {
                super::diagnostics::format_game_operation_error("装备操作失败", &error)
            })?;
            terminal = result.log_terminal();
            let cleanup = match &result.cleanup {
                azur_lane_workbook::application::SessionCleanup::Completed => {
                    serde_json::json!({"status": "completed"})
                }
                azur_lane_workbook::application::SessionCleanup::Recovered(error) => {
                    serde_json::json!({"status": "recovered", "error": error.to_string()})
                }
                azur_lane_workbook::application::SessionCleanup::Failed(error) => {
                    serde_json::json!({"status": "failed", "error": error.to_string()})
                }
            };
            let output = serde_json::json!({"schema_version": 1, "applied": apply, "check": result.check, "execution": result.execution, "cleanup": cleanup});
            println!(
                "{}",
                serde_json::to_string(&output)
                    .map_err(|error| format!("编码装备操作结果失败: {error}"))?
            );
            Ok(if terminal == OperationTerminal::Succeeded {
                CommandOutcome::Successful
            } else {
                CommandOutcome::ExecutionNotSuccessful
            })
        }
        StartupCommand::UpdateAcquisition(workbook_name, mode) => run_update_acquisition(
            &tool_root,
            &workbook_name,
            mode,
            &mut terminal,
            context.as_ref(),
        ),
    })();
    if outcome.is_err() && terminal == OperationTerminal::Succeeded {
        terminal = OperationTerminal::Failed;
    }
    if let Some(context) = context.as_ref() {
        let detail = match &outcome {
            Ok(_) => String::new(),
            Err(error) => error.clone(),
        };
        let log_errors = context.finish(&OperationRecord {
            terminal,
            message: match terminal {
                OperationTerminal::Succeeded => "完成",
                OperationTerminal::Failed => "失败",
                OperationTerminal::Unknown => "结果无法确认",
                OperationTerminal::Cancelled => "已取消",
                OperationTerminal::Incomplete => "收尾未完成",
            }
            .to_owned(),
            detail,
        });
        for error in log_errors {
            eprintln!("{}", format_operation_log_failure(&error));
        }
    }
    outcome
}

fn cli_operation_identity(command: &StartupCommand) -> (&'static str, Option<&str>) {
    match command {
        StartupCommand::VerifyRelease => ("verify-release", None),
        StartupCommand::Doctor => ("doctor", None),
        StartupCommand::LayoutCheck => ("layout-check", None),
        StartupCommand::LayoutUpgrade => ("layout-upgrade", None),
        StartupCommand::LayoutPreview => ("layout-preview", None),
        StartupCommand::Generate(_) => ("generate", None),
        StartupCommand::Check(name) => ("check", Some(name.as_str())),
        StartupCommand::CheckSave(name) => ("check-save", Some(name.as_str())),
        StartupCommand::Execute(name) => ("execute", Some(name.as_str())),
        StartupCommand::Open(name) => ("open", Some(name.as_str())),
        StartupCommand::Workbooks => ("workbooks", None),
        StartupCommand::History => ("history", None),
        StartupCommand::Settings => ("settings", None),
        StartupCommand::Preferences => ("preferences", None),
        StartupCommand::SetPreference(_, _) => ("set-preference", None),
        StartupCommand::Instances => ("instances", None),
        StartupCommand::Agent(AgentCommand::Status, _) => ("agent-status", None),
        StartupCommand::Agent(AgentCommand::Inject, _) => ("agent-inject", None),
        StartupCommand::Agent(AgentCommand::Unload, _) => ("agent-unload", None),
        StartupCommand::Logs => ("logs", None),
        StartupCommand::Query(_) => ("query", None),
        StartupCommand::EquipmentActions(_, false) => ("equipment-check", None),
        StartupCommand::EquipmentActions(_, true) => ("equipment-apply", None),
        StartupCommand::UpdateAcquisition(name, _) => ("update-acquisition", Some(name.as_str())),
        StartupCommand::Gui
        | StartupCommand::WithInstance(_, _)
        | StartupCommand::Help(_)
        | StartupCommand::Mcp
        | StartupCommand::ArtifactDetails(_) => ("", None),
    }
}

struct CliOperationSignals;

impl azur_lane_workbook::application::ExecutionCancellation for CliOperationSignals {
    fn is_cancelled(&self) -> bool {
        cli_cancel_flag().load(Ordering::Acquire)
    }
}

fn cli_cancel_flag() -> Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

#[cfg(windows)]
unsafe extern "system" fn on_console_ctrl(control: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    if control == CTRL_C_EVENT || control == CTRL_BREAK_EVENT {
        cli_cancel_flag().store(true, Ordering::Release);
        1
    } else {
        0
    }
}

fn install_cli_cancel() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_console_ctrl), 1);
        }
    });
}

impl azur_lane_workbook::bootstrap::OperationSignals for CliOperationSignals {
    fn notify_activity(
        &self,
        _units: Option<(usize, usize)>,
        _message: String,
    ) -> Result<(), suzushiro_task_runtime::TaskSignalError> {
        Ok(())
    }

    fn notify_progress(
        &self,
        _percent: u8,
        _message: String,
    ) -> Result<(), suzushiro_task_runtime::TaskSignalError> {
        Ok(())
    }

    fn cancelled(&self) -> bool {
        cli_cancel_flag().load(Ordering::Acquire)
    }

    fn share_cancellation(&self) -> Arc<AtomicBool> {
        cli_cancel_flag()
    }
}

const CLI_SIGNALS: CliOperationSignals = CliOperationSignals;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandOutcome {
    Successful,
    ExecutionNotSuccessful,
}

impl CommandOutcome {
    pub(crate) const fn exit_code_value(self) -> u8 {
        match self {
            Self::Successful => 0,
            Self::ExecutionNotSuccessful => EXECUTION_NOT_SUCCESSFUL_EXIT_CODE,
        }
    }
}

/// 只读取工具根、设置、布局和发布清单，明确不建立游戏运行态。
fn run_doctor(tool_root: &Path) -> Result<(), String> {
    let doctor =
        open_offline_doctor(tool_root).map_err(|error| format_doctor_tool_root_error(&error))?;
    let report: DoctorReport = doctor
        .diagnose()
        .map_err(|error| format_doctor_error(&error))?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码 doctor 结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务检查根布局，成功时输出便于脚本读取的稳定摘要。
fn run_layout_check(tool_root: &std::path::Path) -> Result<(), String> {
    let service = bootstrap_layout_service(tool_root)
        .map_err(|error| format_layout_check_bootstrap_error(&error))?;
    let report: LayoutCheckReport = service
        .check_layout()
        .map_err(|error| format_layout_check_error(&error))?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码布局检查结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务非覆盖升级根布局，成功时输出完整变化和摘要报告。
fn run_layout_upgrade(tool_root: &std::path::Path) -> Result<(), String> {
    let service = bootstrap_layout_service(tool_root)
        .map_err(|error| format_bootstrap_error("布局升级", &error))?;
    let report: LayoutUpgradeReport = service
        .upgrade_layout()
        .map_err(|error| format_layout_upgrade_error(&error))?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码布局升级结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务原子刷新固定布局预览，成功时输出生成范围和摘要报告。
fn run_layout_preview(tool_root: &std::path::Path) -> Result<(), String> {
    let service = bootstrap_layout_service(tool_root)
        .map_err(|error| format_bootstrap_error("布局预览", &error))?;
    let report: LayoutPreviewReport = service
        .preview_layout()
        .map_err(|error| format_layout_preview_error(&error))?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码布局预览结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务生成数据工作簿；未建立游戏会话时只返回稳定错误，不使用静态状态。
fn operation_configuration(
    configuration: &Option<OperationConfiguration>,
) -> Result<&OperationConfiguration, String> {
    configuration
        .as_ref()
        .ok_or_else(|| "操作入口没有读取设置".to_owned())
}

fn run_generate(
    tool_root: &std::path::Path,
    requested_name: Option<&str>,
    instance: Option<&str>,
    related_logs: Option<&azur_lane_workbook::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
    terminal: &mut OperationTerminal,
) -> Result<(), String> {
    let mut service =
        bootstrap_sync_service_with_instance(tool_root, instance, related_logs, configuration)
            .map_err(|error| format_bootstrap_error("工作簿生成", &error))?;
    let outcome = service
        .generate_workbook(requested_name, &|| {
            cli_cancel_flag().load(Ordering::Acquire)
        })
        .map_err(|error| {
            if error.is_cancelled() {
                *terminal = OperationTerminal::Cancelled;
                return error.message().to_owned();
            }
            format_generation_error(&error)
        })?;
    let WorkbookGenerationOutcome::Completed(report) = outcome else {
        let WorkbookGenerationOutcome::CleanupIncomplete { cleanup, .. } = outcome else {
            unreachable!("生成终态只有完成和清理未完成");
        };
        *terminal = OperationTerminal::Incomplete;
        return Err(format_published_generation_cleanup(&cleanup));
    };
    // 完整捕获的验收回执绑定外部资料快照；日常命令只显示生成摘要。
    let output = if report.full_state_capture().is_some() {
        serde_json::to_value(&report)
    } else {
        Ok(serde_json::json!({
            "message": report.message(),
            "output_path": report.output_path(),
            "reused_existing": report.reused_existing(),
            "generated_sheets": report.generated_sheets(),
            "generated_fields": report.generated_fields(),
            "projected_rows": report.projected_rows(),
        }))
    }
    .map_err(|error| format!("编码工作簿生成结果失败: {error}"))?;
    let json: String = serde_json::to_string(&output)
        .map_err(|error| format!("编码工作簿生成结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务读取受控工作簿目录，供界面和脚本选择既有数据文件。
fn run_workbooks(tool_root: &std::path::Path) -> Result<(), String> {
    let service = bootstrap_workspace_service(tool_root)
        .map_err(|error| format_bootstrap_error("工作簿目录读取", &error))?;
    let report: WorkbookCatalogReport = service
        .list_workbooks()
        .map_err(|error| format_workbook_catalog_error(&error))?;
    let json: String = serde_json::to_string(&report)
        .map_err(|error| format!("编码工作簿目录结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务读取并校验全部配装检查和执行历史。
fn run_history(tool_root: &std::path::Path) -> Result<(), String> {
    let service = bootstrap_workspace_service(tool_root)
        .map_err(|error| format_bootstrap_error("历史目录读取", &error))?;
    let report: HistoryCatalogReport = service
        .list_history(&|| false)
        .map_err(format_history_catalog_error)?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码历史目录结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 通过应用服务读取严格设置，并只输出脱敏摘要。
fn run_settings(
    tool_root: &std::path::Path,
    configuration: &OperationConfiguration,
) -> Result<(), String> {
    let service = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| format_bootstrap_error("运行设置读取", &error))?;
    let report: SettingsSummaryReport = service.read_settings().map_err(format_settings_error)?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码运行设置结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

fn print_preferences(
    tool_root: &Path,
    configuration: &OperationConfiguration,
) -> Result<(), String> {
    let service = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| format_bootstrap_error("读取偏好", &error))?;
    let preferences = service.read_preferences().map_err(format_settings_error)?;
    println!(
        "{}",
        serde_json::to_string(&preferences).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn set_preference(
    tool_root: &Path,
    key: &str,
    value: &str,
    configuration: &OperationConfiguration,
) -> Result<(), String> {
    use azur_lane_workbook::application::AcquisitionUpdatePolicy;
    let service = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| format_bootstrap_error("保存偏好", &error))?;
    let original = service.read_preferences().map_err(format_settings_error)?;
    let mut preferences = original;
    let boolean = || {
        value
            .parse::<bool>()
            .map_err(|_| "开关值只接受 true 或 false".to_owned())
    };
    match key {
        "ship_acquisition_enabled" => preferences.ship_acquisition_enabled = boolean()?,
        "detailed_diagnostics" => preferences.detailed_diagnostics = boolean()?,
        "unload_after_sync" => preferences.unload_after_sync = boolean()?,
        "acquisition_update_policy" => {
            preferences.acquisition_update_policy = match value {
                "use_cache" => AcquisitionUpdatePolicy::UseCache,
                "refresh" => AcquisitionUpdatePolicy::Refresh,
                _ => return Err("获取方式更新策略只接受 use_cache 或 refresh".to_owned()),
            }
        }
        _ => {
            return Err(format!(
                "未知设置 {key:?}，可用设置: ship_acquisition_enabled、acquisition_update_policy、detailed_diagnostics、unload_after_sync"
            ));
        }
    }
    service
        .save_preferences(original, preferences)
        .map_err(format_settings_error)?;
    let preferences = service.read_preferences().map_err(format_settings_error)?;
    println!(
        "{}",
        serde_json::to_string(&preferences).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_agent(
    tool_root: &Path,
    command: super::command::AgentCommand,
    instance: Option<&str>,
    related_logs: Option<&azur_lane_workbook::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<CommandOutcome, String> {
    use super::command::AgentCommand;
    use azur_lane_workbook::application::{AgentManagementAction, AgentManagementOutcome};
    use azur_lane_workbook::bootstrap::bootstrap_agent_management;
    let action = match command {
        AgentCommand::Status => AgentManagementAction::Status,
        AgentCommand::Inject => AgentManagementAction::Inject,
        AgentCommand::Unload => AgentManagementAction::Unload,
    };
    let service = bootstrap_agent_management(tool_root, related_logs, configuration);
    let outcome = service
        .manage(instance, action)
        .map_err(|error| error.to_string())?;
    let report = outcome.report();
    println!(
        "{}",
        serde_json::to_string(report).map_err(|error| error.to_string())?
    );
    match outcome {
        AgentManagementOutcome::NeedsAttention(report) => Err(report.message),
        AgentManagementOutcome::Ready(_) => Ok(CommandOutcome::Successful),
    }
}

#[cfg(not(target_os = "windows"))]
fn run_agent(
    _: &Path,
    _: super::command::AgentCommand,
    _: Option<&str>,
    _: Option<&azur_lane_workbook::adapters::RelatedLogSink>,
    _: &OperationConfiguration,
) -> Result<CommandOutcome, String> {
    Err("代理管理需要 Windows 模拟器环境".to_owned())
}

/// 只更新所选工作簿对应的获取方式缓存，并逐项打印结果。
fn run_update_acquisition(
    tool_root: &Path,
    workbook_name: &str,
    mode: azur_lane_workbook::application::AcquisitionUpdateMode,
    terminal: &mut OperationTerminal,
    context: Option<&OperationContext<'_>>,
) -> Result<CommandOutcome, String> {
    let context = context.ok_or("资料更新缺少操作上下文")?;
    let report = update_ship_acquisition_cache_logged(tool_root, workbook_name, mode, context)?;
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|error| format!("编码资料更新结果失败: {error}"))?
    );
    *terminal =
        azur_lane_workbook::application::AcquisitionCacheUpdate::operation_terminal(&report);
    if *terminal == OperationTerminal::Succeeded {
        Ok(CommandOutcome::Successful)
    } else {
        Ok(CommandOutcome::ExecutionNotSuccessful)
    }
}

/// 通过应用服务读取日志文件元数据，不输出日志正文或绝对路径。
fn run_logs(tool_root: &std::path::Path) -> Result<(), String> {
    let service = bootstrap_workspace_service(tool_root)
        .map_err(|error| format_bootstrap_error("日志目录读取", &error))?;
    let report: LogCatalogReport = service
        .list_logs(&|| false)
        .map_err(format_log_catalog_error)?;
    let json: String =
        serde_json::to_string(&report).map_err(|error| format!("编码日志目录结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 检查工具目录内的一份既有数据工作簿，并输出基于最新游戏状态的只读计划。
fn run_check(
    tool_root: &std::path::Path,
    workbook_name: &str,
    instance: Option<&str>,
    related_logs: Option<&azur_lane_workbook::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<(), String> {
    let mut service =
        bootstrap_check_service_with_instance(tool_root, instance, related_logs, configuration)
            .map_err(|error| format_bootstrap_error("工作簿检查", &error))?;
    let workbook = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| format_bootstrap_error("工作簿检查", &error))?
        .select_workbook(workbook_name)
        .map_err(|error| {
            format_workbook_selection_app_error("工作簿检查", CHECK_COMMAND, &error)
        })?;
    let report = service
        .check_workbook_plan(&workbook)
        .map_err(|error| format_workbook_check_error(&error))?;
    let json: String = serde_json::to_string(&report)
        .map_err(|error| format!("编码工作簿检查结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 更新工作簿中的检查结论，并将成功报告排他发布到 data/history。
fn run_check_save(
    tool_root: &std::path::Path,
    workbook_name: &str,
    instance: Option<&str>,
    related_logs: Option<&azur_lane_workbook::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
    terminal: &mut OperationTerminal,
) -> Result<(), String> {
    let mut service =
        bootstrap_check_service_with_instance(tool_root, instance, related_logs, configuration)
            .map_err(|error| format_bootstrap_error("工作簿检查记录", &error))?;
    let workbook = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| format_bootstrap_error("工作簿检查记录", &error))?
        .select_workbook(workbook_name)
        .map_err(|error| {
            format_workbook_selection_app_error("工作簿检查记录", CHECK_SAVE_COMMAND, &error)
        })?;
    let outcome = service
        .check_and_save_workbook_plan(&workbook)
        .map_err(format_check_and_save_error)?;
    *terminal = outcome.log_terminal();
    let CheckAndSaveOutcome::Saved(report) = outcome else {
        return Err(format_check_save_incomplete(outcome));
    };
    let json: String = serde_json::to_string(&report)
        .map_err(|error| format!("编码工作簿检查记录结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

/// 执行受控工作簿计划，并在退出前完成备份、审计历史和执行结果写回。
fn run_execute(
    tool_root: &std::path::Path,
    workbook_name: &str,
    instance: Option<&str>,
    related_logs: Option<&azur_lane_workbook::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
    terminal: &mut OperationTerminal,
) -> Result<CommandOutcome, String> {
    let mut service =
        bootstrap_execute_service_with_instance(tool_root, instance, related_logs, configuration)
            .map_err(|error| format_bootstrap_error("工作簿执行", &error))?;
    let workbook = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| format_bootstrap_error("工作簿执行", &error))?
        .select_workbook(workbook_name)
        .map_err(|error| {
            format_workbook_selection_app_error("工作簿执行", EXECUTE_COMMAND, &error)
        })?;
    let report = service
        .execute_workbook(&workbook)
        .map_err(|error| format_execution_error(&error))?;
    *terminal = report.log_terminal();
    match report {
        WorkbookExecuteOutcome::NoModifications => {
            println!("{{\"message\":\"修改列没有需要执行的操作\"}}");
            Ok(CommandOutcome::Successful)
        }
        WorkbookExecuteOutcome::Completed(report) => {
            let status = report.execution().status();
            let outcome = command_outcome_for_execution_status(status);
            let json = serde_json::to_string(&report)
                .map_err(|error| format!("编码工作簿执行结果失败: {error}"))?;
            println!("{json}");
            Ok(outcome)
        }
        WorkbookExecuteOutcome::Finished { history, .. } => {
            Err(finished_execution_suggestion(&history))
        }
    }
}

/// 在不建立游戏会话或修改数据的前提下，把受控工作簿交给系统默认程序。
fn run_open(tool_root: &std::path::Path, workbook_name: &str) -> Result<(), String> {
    let service = bootstrap_workspace_service(tool_root)
        .map_err(|error| format_bootstrap_error("工作簿打开", &error))?;
    let workbook = service
        .select_workbook(workbook_name)
        .map_err(|error| format_workbook_selection_app_error("工作簿打开", OPEN_COMMAND, &error))?;
    let report: WorkbookOpenReport = service
        .open_workbook(&workbook)
        .map_err(format_workbook_open_error)?;
    let json: String = serde_json::to_string(&report)
        .map_err(|error| format!("编码工作簿打开结果失败: {error}"))?;
    println!("{json}");
    Ok(())
}

fn command_outcome_for_execution_status(status: ExecutionReportStatus) -> CommandOutcome {
    if status == ExecutionReportStatus::Success {
        CommandOutcome::Successful
    } else {
        CommandOutcome::ExecutionNotSuccessful
    }
}

/// 只从当前可执行文件位置确定工具根目录，不使用进程工作目录。
fn executable_tool_root() -> Result<PathBuf, String> {
    let executable: PathBuf =
        env::current_exe().map_err(|error| format!("读取当前程序路径失败: {error}"))?;
    let install = executable
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "当前程序路径缺少父目录".to_owned())?;
    let opened = open_or_create_resource_root(&install)
        .map_err(|error| format!("打开工具根目录失败: {error}"))?;
    ensure_extracted_release(&opened, &executable).map_err(|error| format_release_error(&error))?;
    Ok(opened.as_path().to_path_buf())
}

#[cfg(test)]
mod tests {
    use azur_lane_workbook::application::ExecutionReportStatus;

    use super::{CommandOutcome, command_outcome_for_execution_status};

    #[test]
    fn settings_save_reports_the_lock_path_failure() {
        let root =
            std::env::temp_dir().join(format!("azlw-cli-settings-error-{}", std::process::id()));
        std::fs::create_dir_all(root.join("settings.lock")).unwrap();
        let path = root.join("settings.json");
        let original = include_str!("../../settings.json");
        std::fs::write(&path, original).unwrap();
        let configuration = super::OperationConfiguration::load(&root);
        let error =
            super::set_preference(&root, "ship_acquisition_enabled", "true", &configuration)
                .unwrap_err();
        assert!(error.contains("设置保存失败"), "{error}");
        assert!(error.contains("settings.lock"), "{error}");
        assert!(error.contains("普通文件"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn set_preference_rejects_changes_since_operation_configuration_was_loaded() {
        use azur_lane_workbook::adapters::settings::Settings;
        let root =
            std::env::temp_dir().join(format!("azlw-cli-settings-conflict-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path, include_str!("../../settings.json")).unwrap();
        let configuration = super::OperationConfiguration::load(&root);
        let original = configuration.preferences().unwrap();
        Settings::save_preferences(
            &root,
            original,
            azur_lane_workbook::application::UserPreferences {
                detailed_diagnostics: !original.detailed_diagnostics,
                ..original
            },
        )
        .unwrap();
        let saved = std::fs::read(&path).unwrap();
        let error =
            super::set_preference(&root, "ship_acquisition_enabled", "true", &configuration)
                .unwrap_err();
        assert!(error.contains("重新打开设置"));
        assert_eq!(std::fs::read(&path).unwrap(), saved);
        let configuration = super::OperationConfiguration::load(&root);
        super::set_preference(&root, "ship_acquisition_enabled", "true", &configuration).unwrap();
        let actual = Settings::load(&root).unwrap().preferences();
        assert!(actual.ship_acquisition_enabled);
        assert_eq!(actual.detailed_diagnostics, !original.detailed_diagnostics);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execution_outcomes_use_the_confirmed_process_exit_contract() {
        assert_eq!(
            command_outcome_for_execution_status(ExecutionReportStatus::Success),
            CommandOutcome::Successful
        );
        for status in [
            ExecutionReportStatus::Failed,
            ExecutionReportStatus::Unknown,
            ExecutionReportStatus::Cancelled,
        ] {
            let outcome = command_outcome_for_execution_status(status);
            assert_eq!(outcome, CommandOutcome::ExecutionNotSuccessful);
            assert_eq!(outcome.exit_code_value(), 2);
        }
        use azur_lane_workbook::application::OperationTerminal;
        assert_eq!(
            OperationTerminal::from_execution(ExecutionReportStatus::Unknown).status(),
            "unknown"
        );
        assert_eq!(
            OperationTerminal::from_execution(ExecutionReportStatus::Cancelled).status(),
            "cancelled"
        );
        assert_eq!(
            OperationTerminal::from_execution(ExecutionReportStatus::Failed).status(),
            "failed"
        );
        assert!(OperationTerminal::from_execution(ExecutionReportStatus::Unknown).uncertain());
        assert!(!OperationTerminal::from_execution(ExecutionReportStatus::Cancelled).uncertain());
        assert_eq!(CommandOutcome::Successful.exit_code_value(), 0);
    }
}

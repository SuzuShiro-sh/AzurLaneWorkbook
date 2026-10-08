//! 将 GUI 操作适配为独立应用服务任务，并统一进度与失败输出。

use std::error::Error;
use std::path::{Path, PathBuf};

use crate::application::{
    DiagnosticArtifactRef, ExecutionCancellation, ExecutionReportStatus, WorkbookExecuteOutcome,
};
use crate::interfaces::gui_controller::{
    GuiDiagnosticSnapshot, GuiOperation, GuiOperationOutput, GuiTaskFactory, GuiTaskOutput,
    app_failure, append_error_chain, check_failed_task, finished_execution_message,
    generation_error_output, initialized_operation_output, ordered_diagnostic_items,
    present_check_tail, workbook_name_from_relative_path,
};
use suzushiro_task_runtime::{TaskContext, TaskFailure};

use super::operation_log::{
    OperationConfiguration, OperationContext, OperationIdentity, OperationRecord, OperationSignals,
};
use super::{
    BootstrapError, bootstrap_check_service_with_instance, bootstrap_execute_service_with_instance,
    bootstrap_sync_service_with_instance, bootstrap_workspace_service,
    bootstrap_workspace_service_with_configuration,
};

/// 为原生窗口建立生产任务工厂；每次操作都在后台线程内组合独立应用服务。
pub fn bootstrap_gui_task_factory(tool_root: PathBuf) -> GuiTaskFactory {
    GuiTaskFactory::from_handler(move |operation, instance, context| {
        run_gui_operation(&tool_root, operation, instance.as_deref(), &context)
    })
}

impl OperationSignals for TaskContext<GuiTaskOutput> {
    fn notify_activity(
        &self,
        units: Option<(usize, usize)>,
        message: String,
    ) -> Result<(), suzushiro_task_runtime::TaskSignalError> {
        TaskContext::report_activity(self, units, message)
    }

    fn notify_progress(
        &self,
        percent: u8,
        message: String,
    ) -> Result<(), suzushiro_task_runtime::TaskSignalError> {
        TaskContext::report_progress(self, percent, message)
    }

    fn cancelled(&self) -> bool {
        TaskContext::is_cancelled(self)
    }

    fn share_cancellation(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.cancellation_flag()
    }
}

fn operation_identity(operation: &GuiOperation) -> OperationIdentity<'_> {
    match operation {
        GuiOperation::Initialize => OperationIdentity {
            name: "刷新信息",
            workbook: None,
        },
        GuiOperation::AgentStatus => OperationIdentity {
            name: "查询代理",
            workbook: None,
        },
        GuiOperation::UnloadAgent => OperationIdentity {
            name: "卸载代理",
            workbook: None,
        },
        GuiOperation::LoadSettings => OperationIdentity {
            name: "读取设置",
            workbook: None,
        },
        GuiOperation::SaveSettings { .. } => OperationIdentity {
            name: "保存设置",
            workbook: None,
        },
        GuiOperation::SynchronizeAndGenerate => OperationIdentity {
            name: "同步并生成",
            workbook: None,
        },
        GuiOperation::CheckPlan(name) => OperationIdentity {
            name: "检查计划",
            workbook: Some(name.as_str()),
        },
        GuiOperation::ExecutePlan(name) => OperationIdentity {
            name: "执行计划",
            workbook: Some(name.as_str()),
        },
        GuiOperation::OpenWorkbook(name) => OperationIdentity {
            name: "打开工作簿",
            workbook: Some(name.as_str()),
        },
        GuiOperation::OpenDiagnostic(_) => OperationIdentity {
            name: "打开日志",
            workbook: None,
        },
    }
}

fn operation_record(result: &Result<GuiOperationOutput, TaskFailure>) -> OperationRecord {
    match result {
        Ok(output) => OperationRecord {
            terminal: output.terminal(),
            message: output.summary().to_owned(),
            detail: output.diagnostic_detail().unwrap_or("").to_owned(),
        },
        Err(error) => OperationRecord {
            terminal: if error.is_cancelled() {
                crate::application::OperationTerminal::Cancelled
            } else if error.is_uncertain() {
                crate::application::OperationTerminal::Unknown
            } else {
                crate::application::OperationTerminal::Failed
            },
            message: error.user_message().to_owned(),
            detail: error.detail().to_owned(),
        },
    }
}

struct TaskExecutionCancellation<'a> {
    context: &'a OperationContext<'a>,
}

impl ExecutionCancellation for TaskExecutionCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.context.is_cancelled()
    }
}

fn run_gui_operation(
    tool_root: &Path,
    operation: GuiOperation,
    instance: Option<&str>,
    context: &TaskContext<GuiTaskOutput>,
) -> Result<GuiTaskOutput, TaskFailure> {
    match &operation {
        GuiOperation::OpenWorkbook(name) => {
            return Ok(GuiTaskOutput::new(
                open_workbook(
                    tool_root,
                    name,
                    &OperationContext::unlogged(context as &dyn OperationSignals),
                ),
                None,
            ));
        }
        GuiOperation::OpenDiagnostic(source) => {
            return Ok(GuiTaskOutput::new(
                open_diagnostic(
                    tool_root,
                    source,
                    &OperationContext::unlogged(context as &dyn OperationSignals),
                ),
                None,
            ));
        }
        _ => {}
    }
    let append_final_diagnostic_count = matches!(operation, GuiOperation::Initialize);
    let refresh_diagnostics = !matches!(
        operation,
        GuiOperation::LoadSettings | GuiOperation::SaveSettings { .. }
    );
    let configuration = OperationConfiguration::load(tool_root);
    let logged = OperationContext::new(
        tool_root,
        operation_identity(&operation),
        instance,
        &configuration.operation_settings(),
        context,
    );
    let context = &logged;
    let refresh_agent = matches!(
        operation,
        GuiOperation::SynchronizeAndGenerate
            | GuiOperation::CheckPlan(_)
            | GuiOperation::ExecutePlan(_)
    );
    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match operation {
        GuiOperation::Initialize => initialize_gui(tool_root, instance, context, &configuration),
        GuiOperation::LoadSettings => read_gui_settings(tool_root, &configuration),
        GuiOperation::AgentStatus => manage_gui_agent(
            tool_root,
            require_instance(instance)?,
            false,
            context.related_logs().as_ref(),
            &configuration,
        ),
        GuiOperation::UnloadAgent => manage_gui_agent(
            tool_root,
            require_instance(instance)?,
            true,
            context.related_logs().as_ref(),
            &configuration,
        ),
        GuiOperation::SaveSettings {
            original,
            preferences,
        } => {
            let service = bootstrap_workspace_service_with_configuration(tool_root, &configuration)
                .map_err(|error| TaskFailure::new("设置保存失败", error.to_string()))?;
            service
                .save_preferences(original, preferences)
                .map_err(|error| app_failure("设置保存", error))?;
            Ok(GuiOperationOutput::success("设置已保存，下次同步生成生效")
                .with_preferences(preferences))
        }
        GuiOperation::SynchronizeAndGenerate => synchronize_and_generate(
            tool_root,
            require_instance(instance)?,
            context,
            &configuration,
        ),
        GuiOperation::CheckPlan(name) => check_workbook(
            tool_root,
            &name,
            require_instance(instance)?,
            context,
            &configuration,
        ),
        GuiOperation::ExecutePlan(name) => execute_workbook(
            tool_root,
            &name,
            require_instance(instance)?,
            context,
            &configuration,
        ),
        GuiOperation::OpenWorkbook(name) => open_workbook(tool_root, &name, context),
        GuiOperation::OpenDiagnostic(source) => open_diagnostic(tool_root, &source, context),
    }))
    .unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("非文本 panic");
        Err(TaskFailure::unexpected_termination(
            "操作意外终止",
            format!("后台任务异常: {message}"),
        ))
    });
    let cancelled = match &result {
        Ok(output) => output.terminal() == crate::application::OperationTerminal::Cancelled,
        Err(error) => error.is_cancelled(),
    };
    if refresh_agent
        && !cancelled
        && let Some(instance) = instance
    {
        result = result.map(|output| {
            output.with_agent_status(agent_status_text(
                tool_root,
                instance,
                logged.related_logs().as_ref(),
                &configuration,
            ))
        });
    }
    let log_errors = logged.finish(&operation_record(&result));
    let log_failure = if log_errors.is_empty() {
        None
    } else {
        Some(TaskFailure::new(
            "操作日志未完整保存，请检查日志目录和磁盘状态",
            log_errors.join("\n"),
        ))
    };
    // 业务和日志均已收尾，再取得目录的最终内容身份；取消保持原业务终态。
    let diagnostics = if cancelled || !refresh_diagnostics {
        None
    } else {
        Some(
            match bootstrap_workspace_service_with_configuration(tool_root, &configuration) {
                Ok(service) => read_diagnostics(&service, &|| context.is_cancelled()),
                Err(error) => Err(bootstrap_failure("历史与日志刷新", &error)),
            },
        )
    };
    if append_final_diagnostic_count
        && let (Ok(output), Some(Ok(snapshot))) = (&mut result, diagnostics.as_ref())
    {
        output.append_summary(&format!("，{} 条历史或日志", snapshot.items.len()));
    }
    let mut output = GuiTaskOutput::new(result, diagnostics);
    if let Some(error) = log_failure {
        output = output.with_auxiliary(error);
    }
    Ok(output)
}

#[cfg(target_os = "windows")]
fn manage_gui_agent(
    tool_root: &Path,
    instance: &str,
    unload: bool,
    related: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    use crate::application::{AgentManagementAction, AgentManagementOutcome};
    let action = if unload {
        AgentManagementAction::Unload
    } else {
        AgentManagementAction::Status
    };
    let service = super::bootstrap_agent_management(tool_root, related, configuration);
    let outcome = service
        .manage(Some(instance), action)
        .map_err(|error| app_failure("代理管理", error))?;
    let message = outcome.report().message.clone();
    let output = match outcome {
        AgentManagementOutcome::NeedsAttention(_) => GuiOperationOutput::attention(&message),
        AgentManagementOutcome::Ready(_) => GuiOperationOutput::success(&message),
    };
    Ok(output.with_agent_status(message))
}

#[cfg(not(target_os = "windows"))]
fn manage_gui_agent(
    _: &Path,
    _: &str,
    _: bool,
    _: Option<&crate::adapters::RelatedLogSink>,
    _: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    Err(TaskFailure::new(
        "代理管理不可用",
        "代理管理需要 Windows 模拟器环境",
    ))
}

fn agent_status_text(
    tool_root: &Path,
    instance: &str,
    related: Option<&crate::adapters::RelatedLogSink>,
    configuration: &OperationConfiguration,
) -> String {
    match manage_gui_agent(tool_root, instance, false, related, configuration) {
        Ok(output) => output.summary().to_owned(),
        Err(error) => format!("查询失败：{}", error.detail()),
    }
}

fn require_instance(instance: Option<&str>) -> Result<&str, TaskFailure> {
    instance.ok_or_else(|| TaskFailure::new("请选择可用实例", "游戏操作缺少具体实例选择"))
}

fn read_gui_settings(
    tool_root: &Path,
    configuration: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    let service = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| TaskFailure::new("设置读取失败", error.to_string()))?;
    let preferences = service
        .read_preferences()
        .map_err(|error| app_failure("设置读取", error))?;
    Ok(GuiOperationOutput::success("设置已读取").with_preferences(preferences))
}

fn open_diagnostic(
    tool_root: &Path,
    source: &DiagnosticArtifactRef,
    context: &OperationContext<'_>,
) -> Result<GuiOperationOutput, TaskFailure> {
    report_progress(context, 10, "正在核对并打开所选日志")?;
    let service = bootstrap_workspace_service(tool_root)
        .map_err(|error| bootstrap_failure("日志打开", &error))?;
    service
        .open_diagnostic(source)
        .map_err(|error| app_failure("日志打开", error))?;
    Ok(GuiOperationOutput::success(format!(
        "已打开：{}",
        source.relative_path()
    )))
}

fn read_diagnostics(
    service: &crate::application::WorkspaceService,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<GuiDiagnosticSnapshot, TaskFailure> {
    if is_cancelled() {
        return Err(TaskFailure::from_signal(
            suzushiro_task_runtime::TaskSignalError::Cancelled,
        ));
    }
    let mut warnings = Vec::new();
    let history = match service.list_history(is_cancelled) {
        Err(error) if error.stage() == "catalog.cancelled" => {
            return Err(TaskFailure::from_signal(
                suzushiro_task_runtime::TaskSignalError::Cancelled,
            ));
        }
        Ok(report) => {
            warnings.extend(report.failures().iter().map(|failure| {
                TaskFailure::new(
                    "部分历史无法读取",
                    format!(
                        "{}：{}（{}）",
                        failure.relative_path(),
                        failure.message(),
                        failure.error_code()
                    ),
                )
            }));
            Some(report)
        }
        Err(error) => {
            warnings.push(app_failure("历史目录读取", error));
            None
        }
    };
    if is_cancelled() {
        return Err(TaskFailure::from_signal(
            suzushiro_task_runtime::TaskSignalError::Cancelled,
        ));
    }
    let logs = match service.list_logs(is_cancelled) {
        Err(error) if error.stage() == "catalog.cancelled" => {
            return Err(TaskFailure::from_signal(
                suzushiro_task_runtime::TaskSignalError::Cancelled,
            ));
        }
        Ok(report) => {
            warnings.extend(report.failures().iter().map(|failure| {
                TaskFailure::new(
                    "部分日志无法读取",
                    format!(
                        "{}：{}（{}）",
                        failure.relative_path(),
                        failure.message(),
                        failure.error_code()
                    ),
                )
            }));
            Some(report)
        }
        Err(error) => {
            warnings.push(app_failure("日志目录读取", error));
            None
        }
    };
    let mut failed_sources = Vec::new();
    if history.is_none() {
        failed_sources.push(crate::application::DiagnosticArtifactKind::History);
    }
    if logs.is_none() {
        failed_sources.push(crate::application::DiagnosticArtifactKind::Log);
    }
    Ok(GuiDiagnosticSnapshot {
        items: ordered_diagnostic_items(
            history.as_ref().map_or(&[], |report| report.entries()),
            logs.as_ref().map_or(&[], |report| report.entries()),
        ),
        warnings,
        failed_sources,
    })
}

fn initialize_gui(
    tool_root: &Path,
    selected_instance: Option<&str>,
    context: &OperationContext<'_>,
    configuration: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    report_progress(context, 10, "正在初始化应用服务")?;
    let service = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| bootstrap_failure("应用初始化", &error))?;
    report_progress(context, 25, "正在读取工作簿目录")?;
    let startup = service
        .load_startup()
        .map_err(|error| app_failure("工作簿目录读取", error))?;
    report_progress(context, 35, "正在只读查询 模拟器 实例")?;
    report_progress(context, 60, "正在读取模拟器实例")?;
    let (output, selected) = initialized_operation_output(startup, selected_instance);
    let output = match selected.as_deref() {
        Some(instance) => output.with_agent_status(agent_status_text(
            tool_root,
            instance,
            context.related_logs().as_ref(),
            configuration,
        )),
        None => output,
    };
    Ok(output)
}

fn synchronize_and_generate(
    tool_root: &Path,
    instance: &str,
    context: &OperationContext<'_>,
    configuration: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    if context.is_cancelled() {
        return Err(TaskFailure::from_signal(
            suzushiro_task_runtime::TaskSignalError::Cancelled,
        ));
    }
    context
        .report_activity(None, "正在初始化游戏运行态")
        .map_err(TaskFailure::from_signal)?;
    let mut service = bootstrap_sync_service_with_instance(
        tool_root,
        Some(instance),
        context.related_logs().as_ref(),
        configuration,
    )
    .map_err(|error| bootstrap_failure("同步并生成", &error))?;
    let cancellation = context.cancellation_flag();
    let driven = crate::application::drive_progress(
        |progress| {
            service.generate_workbook_with_progress(None, progress, &|| {
                cancellation.load(std::sync::atomic::Ordering::Acquire)
            })
        },
        |progress| context.report_activity(progress.units, progress.message.clone()),
    );
    let progress_error = driven.progress_error;
    let result = driven.value;
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) if error.is_cancelled() => {
            return Err(TaskFailure::from_signal(
                suzushiro_task_runtime::TaskSignalError::Cancelled,
            ));
        }
        Err(error) => return Err(app_failure("同步并生成", error)),
    };
    let crate::application::WorkbookGenerationOutcome::Completed(report) = outcome else {
        let crate::application::WorkbookGenerationOutcome::CleanupIncomplete { report, cleanup } =
            outcome
        else {
            unreachable!("生成终态只有完成和清理未完成");
        };
        return generation_error_output(report.output_path(), cleanup);
    };
    let workbook_name = workbook_name_from_relative_path(report.output_path())?;
    let output =
        GuiOperationOutput::success(format!("{}：{}", report.message(), report.output_path()))
            .with_selected_workbook(workbook_name);
    Ok(note_progress(output, progress_error))
}

fn check_workbook(
    tool_root: &Path,
    workbook_name: &str,
    instance: &str,
    context: &OperationContext<'_>,
    configuration: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    context
        .report_activity(None, "正在初始化计划检查服务")
        .map_err(TaskFailure::from_signal)?;
    let mut service = bootstrap_check_service_with_instance(
        tool_root,
        Some(instance),
        context.related_logs().as_ref(),
        configuration,
    )
    .map_err(|error| bootstrap_failure("计划检查", &error))?;
    let workbook = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| bootstrap_failure("计划检查", &error))?
        .select_workbook(workbook_name)
        .map_err(|error| app_failure("工作簿选择", error))?;
    let driven = crate::application::drive_progress(
        |progress| service.check_and_save_workbook_plan_with_progress(&workbook, progress),
        |progress| context.report_activity(progress.units, progress.message.clone()),
    );
    let progress_error = driven.progress_error;
    let outcome = driven
        .value
        .map_err(|error| app_failure("计划检查", error))?;
    let terminal = outcome.log_terminal();
    match outcome {
        crate::application::CheckAndSaveOutcome::Saved(report) => {
            let output = GuiOperationOutput::success(format!(
                "{}：{}",
                report.message(),
                report.relative_path()
            ));
            Ok(note_progress(output, progress_error))
        }
        crate::application::CheckAndSaveOutcome::CheckFailed { error, writeback } => {
            Err(check_failed_task(error, writeback))
        }
        outcome => {
            let (summary, detail) = present_check_tail(&outcome);
            Ok(note_progress(
                GuiOperationOutput::business_note(summary, terminal).with_diagnostic_detail(detail),
                progress_error,
            ))
        }
    }
}

fn execute_workbook(
    tool_root: &Path,
    workbook_name: &str,
    instance: &str,
    context: &OperationContext<'_>,
    configuration: &OperationConfiguration,
) -> Result<GuiOperationOutput, TaskFailure> {
    context
        .report_activity(None, "正在初始化计划执行服务")
        .map_err(TaskFailure::from_signal)?;
    let mut service = bootstrap_execute_service_with_instance(
        tool_root,
        Some(instance),
        context.related_logs().as_ref(),
        configuration,
    )
    .map_err(|error| bootstrap_failure("计划执行", &error))?;
    let workbook = bootstrap_workspace_service_with_configuration(tool_root, configuration)
        .map_err(|error| bootstrap_failure("计划执行", &error))?
        .select_workbook(workbook_name)
        .map_err(|error| app_failure("工作簿选择", error))?;
    let cancellation = TaskExecutionCancellation { context };
    let driven = crate::application::drive_progress(
        |progress| service.execute_workbook_with_progress(&workbook, &cancellation, progress),
        |progress| context.report_activity(progress.units, progress.message.clone()),
    );
    let progress_error = driven.progress_error;
    let report = driven
        .value
        .map_err(|error| app_failure("计划执行", error))?;
    let report = match report {
        WorkbookExecuteOutcome::NoModifications => {
            return Ok(GuiOperationOutput::success(
                "修改列没有需要执行的操作".to_owned(),
            ));
        }
        WorkbookExecuteOutcome::Finished {
            execution,
            backup,
            history,
            writeback,
            cleanup,
        } => {
            return Ok(finished_execution_output(
                &execution,
                &backup,
                &history,
                &writeback,
                &cleanup,
                progress_error,
            ));
        }
        WorkbookExecuteOutcome::Completed(report) => report,
    };
    let execution = report.execution();
    let history_path = report.history().relative_path();
    let output = match execution.status() {
        ExecutionReportStatus::Success => GuiOperationOutput::from_execution(
            format!(
                "执行完成，已核验 {} 个写步骤；记录：{}",
                execution.verified_write_count(),
                history_path
            ),
            execution.status(),
            false,
        ),
        ExecutionReportStatus::Failed => GuiOperationOutput::from_execution(
            format!("执行结果为失败，记录已保存到 {history_path}；请先核对工作簿结果"),
            execution.status(),
            false,
        ),
        ExecutionReportStatus::Unknown => GuiOperationOutput::from_execution(
            format!("执行结果无法确认，记录已保存到 {history_path}；请人工核对，禁止直接重试"),
            execution.status(),
            false,
        ),
        ExecutionReportStatus::Cancelled => GuiOperationOutput::from_execution(
            format!("执行已在安全边界停止，记录已保存到 {history_path}；请核对后再继续"),
            execution.status(),
            false,
        ),
    };
    Ok(note_progress(output, progress_error))
}

fn finished_execution_output(
    execution: &crate::application::ExecutionReport,
    backup: &crate::application::WorkbookBackupReport,
    history: &crate::application::StageResult<crate::application::ExecutionHistoryReport>,
    writeback: &crate::application::StageResult<crate::application::ExecutionResultsWriteReport>,
    cleanup: &crate::application::SessionCleanup,
    progress_error: Option<String>,
) -> GuiOperationOutput {
    note_progress(
        GuiOperationOutput::from_execution(
            finished_execution_message(backup.backup_path(), history, writeback, cleanup),
            execution.status(),
            true,
        ),
        progress_error,
    )
}

fn note_progress(output: GuiOperationOutput, progress_error: Option<String>) -> GuiOperationOutput {
    match progress_error {
        Some(error) => output.with_auxiliary_notice(format!("进度通知失败：{error}")),
        None => output,
    }
}

fn open_workbook(
    tool_root: &Path,
    workbook_name: &str,
    context: &OperationContext<'_>,
) -> Result<GuiOperationOutput, TaskFailure> {
    report_progress(context, 10, "正在校验并打开所选工作簿")?;
    let service = bootstrap_workspace_service(tool_root)
        .map_err(|error| bootstrap_failure("工作簿打开", &error))?;
    let workbook = service
        .select_workbook(workbook_name)
        .map_err(|error| app_failure("工作簿选择", error))?;
    let report = service
        .open_workbook(&workbook)
        .map_err(|error| app_failure("工作簿打开", error))?;
    Ok(GuiOperationOutput::success(format!(
        "{}：{}",
        report.message(),
        report.workbook_path()
    )))
}

fn report_progress(
    context: &OperationContext<'_>,
    percent: u8,
    message: &'static str,
) -> Result<(), TaskFailure> {
    context
        .report_progress(percent, message)
        .map_err(TaskFailure::from_signal)
}

fn bootstrap_failure(operation: &str, error: &BootstrapError) -> TaskFailure {
    task_failure(operation, "程序目录或配置未能完成初始化", error)
}

fn task_failure(operation: &str, user_message: &str, error: &(dyn Error + 'static)) -> TaskFailure {
    let mut detail = format!("{operation}失败：{error}");
    append_error_chain(&mut detail, error);
    TaskFailure::new(user_message, detail)
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_operation_reads_completed_log_files_without_a_manual_refresh() {
        use crate::interfaces::gui_controller::{
            GuiAction, GuiController, GuiInstanceItem, GuiOperation, GuiOperationOutput,
            GuiSupportSnapshot, GuiTaskFactory, GuiTaskOutput,
        };
        use std::fs;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        let root = std::path::PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-completed-log-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("data/logs")).unwrap();
        let task_root = root.clone();
        let factory = GuiTaskFactory::from_handler(move |operation, _, context| {
            if matches!(operation, GuiOperation::Initialize) {
                return Ok(GuiTaskOutput::new(
                    Ok(GuiOperationOutput::success("ready").with_support_snapshot(
                        GuiSupportSnapshot::new(
                            vec![GuiInstanceItem::explicit(
                                "1".into(),
                                "fixture".into(),
                                crate::application::EmulatorInstanceState::Ready,
                            )],
                            Some("1".into()),
                        ),
                    )),
                    None,
                ));
            }
            fs::write(task_root.join("data/logs/001-adb.log"), b"completed log").unwrap();
            // 缺少具体目标在任何设备调用之前失败，同时走正式任务收尾和目录读取。
            super::run_gui_operation(&task_root, operation, None, &context)
        });
        let mut controller = GuiController::new(factory);
        for action in [GuiAction::Refresh, GuiAction::SynchronizeAndGenerate] {
            controller.start(action, || {}).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while controller.view().is_running() {
                controller.drain_events().unwrap();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(controller.view().status(), "操作失败");
        assert!(!controller.view().is_running());
        assert!(
            controller
                .view()
                .last_failure_detail()
                .unwrap()
                .contains("缺少具体实例")
        );
        assert_eq!(controller.view().diagnostics().len(), 2);
        assert!(
            controller
                .view()
                .selected_diagnostic()
                .unwrap()
                .source()
                .relative_path()
                .contains("-app.log")
        );
        let operation_log = fs::read_dir(root.join("data/logs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("-app.log")
            })
            .unwrap();
        let text = fs::read_to_string(operation_log).unwrap();
        assert!(text.contains("operation.start"));
        assert!(text.contains("operation.complete"));
        assert!(text.contains("缺少具体实例"));
        assert_eq!(
            fs::read(root.join("data/logs/001-adb.log")).unwrap(),
            b"completed log"
        );
        assert_eq!(controller.view().selected_instance(), Some("1"));
        drop(controller);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_reloads_workbooks_and_reports_each_unavailable_support_source() {
        use std::fs;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        use crate::interfaces::gui_controller::{GuiAction, GuiController};

        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .expect("测试环境必须提供用户目录");
        let root = std::path::PathBuf::from(home)
            .join("suzushiro/scratch/azlw-refresh-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("data/workbooks")).unwrap();
        // 无效设置在实例目录读取前失败，测试不调用管理器或连接游戏。
        fs::write(root.join("settings.json"), b"{").unwrap();
        fs::write(root.join("data/history"), b"invalid directory").unwrap();
        fs::write(root.join("data/logs"), b"invalid directory").unwrap();
        fs::write(root.join("data/workbooks/first.xlsx"), b"catalog fixture").unwrap();
        let mut controller = GuiController::new(super::bootstrap_gui_task_factory(root.clone()));
        controller.start_initialization(|| {}).unwrap();
        for iteration in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(5);
            while controller.view().is_running() {
                controller.drain_events().unwrap();
                assert!(Instant::now() < deadline, "刷新任务未结束");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(controller.view().status(), "已就绪");
            assert_eq!(controller.view().workbooks().len(), iteration + 1);
            assert!(!controller.view().controls().synchronize_enabled);
            assert!(!controller.view().is_running());
            assert!(controller.view().message().contains("部分辅助信息读取失败"));
            let view = controller.view();
            let detail = view.last_failure_detail().unwrap();
            for source in ["模拟器 实例目录读取", "历史目录读取", "日志目录读取"]
            {
                assert!(detail.contains(source), "缺少来源错误: {source}: {detail}");
            }
            assert_eq!(fs::read(root.join("settings.json")).unwrap(), b"{");
            if iteration == 0 {
                fs::write(root.join("data/workbooks/second.xlsx"), b"catalog fixture").unwrap();
                controller.start(GuiAction::Refresh, || {}).unwrap();
            }
        }
        drop(controller);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operation_log_failure_stays_with_success_and_failure() {
        use std::fs;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        use crate::interfaces::gui_controller::{GuiAction, GuiController};

        let root = std::path::PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-log-failure-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(
            root.join("settings.json"),
            include_str!("../../settings.json"),
        )
        .unwrap();
        fs::write(root.join("data/logs"), b"not a directory").unwrap();
        let mut controller = GuiController::new(super::bootstrap_gui_task_factory(root.clone()));
        controller.start(GuiAction::LoadSettings, || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while controller.view().is_running() {
            controller.drain_events().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.view().status(), "操作完成");
        assert!(controller.view().message().contains("操作日志未完整保存"));
        assert!(
            controller
                .view()
                .last_failure_detail()
                .unwrap()
                .contains("创建操作日志失败")
        );
        fs::write(root.join("settings.json"), b"{").unwrap();
        controller.start(GuiAction::LoadSettings, || {}).unwrap();
        while controller.view().is_running() {
            controller.drain_events().unwrap();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.view().status(), "操作失败");
        assert!(
            controller
                .view()
                .message()
                .contains("settings.json 未通过严格校验")
        );
        assert!(
            controller
                .view()
                .last_failure_detail()
                .unwrap()
                .contains("设置读取失败")
        );
        assert!(controller.view().message().contains("操作日志未完整保存"));
        assert!(
            controller
                .view()
                .last_failure_detail()
                .unwrap()
                .contains("创建操作日志失败")
        );
        drop(controller);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_log_directory_keeps_the_previous_catalog() {
        use std::fs;
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        use crate::interfaces::gui_controller::{GuiAction, GuiController};

        let root = std::path::PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-catalog-keep-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("data/history")).unwrap();
        fs::create_dir_all(root.join("data/logs")).unwrap();
        fs::create_dir_all(root.join("data/workbooks")).unwrap();
        fs::write(
            root.join("settings.json"),
            include_str!("../../settings.json"),
        )
        .unwrap();
        let line =
            b"{\"timestamp_ms\":1,\"stage\":\"measure\",\"status\":\"Succeeded\",\"details\":{}}\n";
        fs::write(root.join("data/logs/001-app.log"), line).unwrap();
        let mut controller = GuiController::new(super::bootstrap_gui_task_factory(root.clone()));
        controller.start_initialization(|| {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while controller.view().is_running()
            || controller
                .view()
                .diagnostics()
                .iter()
                .all(|item| !item.source().relative_path().contains("001-app.log"))
        {
            controller.drain_events().unwrap();
            assert!(Instant::now() < deadline, "初始目录未出现");
            std::thread::sleep(Duration::from_millis(5));
        }
        fs::remove_dir_all(root.join("data/logs")).unwrap();
        fs::write(root.join("data/logs"), b"not a directory").unwrap();
        controller.start(GuiAction::Refresh, || {}).unwrap();
        while controller.view().is_running()
            || controller
                .view()
                .last_failure_detail()
                .is_none_or(|detail| !detail.contains("日志目录读取"))
        {
            controller.drain_events().unwrap();
            assert!(Instant::now() < deadline, "目录失败未显示");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            controller
                .view()
                .diagnostics()
                .iter()
                .any(|item| item.source().relative_path().contains("001-app.log"))
        );
        assert_eq!(controller.view().status(), "已就绪");
        drop(controller);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn execution_tail_and_progress_keep_the_business_terminal_in_the_log() {
        use crate::application::{
            AppError, AppErrorCode, ExecutionReportStatus, OperationTerminal, SessionCleanup,
            StageResult, WorkbookBackupReport,
        };
        use suzushiro_task_runtime::TaskSignalError;

        struct Signals;
        impl super::OperationSignals for Signals {
            fn notify_activity(
                &self,
                _: Option<(usize, usize)>,
                _: String,
            ) -> Result<(), TaskSignalError> {
                Ok(())
            }
            fn notify_progress(&self, _: u8, _: String) -> Result<(), TaskSignalError> {
                Ok(())
            }
            fn cancelled(&self) -> bool {
                false
            }
        }

        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .expect("测试环境必须提供用户目录");
        let root = std::path::PathBuf::from(home).join(format!(
            "suzushiro/scratch/azlw-terminal-log-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            include_str!("../../settings.json"),
        )
        .unwrap();
        let execution = crate::application::test_support::empty_execution_report();
        let backup = WorkbookBackupReport::new(
            "data/workbooks/plan.xlsx".to_owned(),
            "data/backups/plan.xlsx".to_owned(),
            0,
            1,
            "a".repeat(64),
            "b".repeat(64),
        );
        let history = StageResult::Failed(AppError::from_source(
            "execution.history",
            AppErrorCode::WorkbookInvalid,
            "历史保存失败",
            std::io::Error::other("history"),
        ));
        let writeback = StageResult::NotAttempted;
        let cleanup = SessionCleanup::Failed(AppError::from_source(
            "session.cleanup",
            AppErrorCode::RuntimeBootstrapFailed,
            "会话清理失败",
            std::io::Error::other("cleanup"),
        ));
        let cases = [
            (ExecutionReportStatus::Unknown, false, "unknown", true),
            (ExecutionReportStatus::Unknown, true, "unknown", true),
            (ExecutionReportStatus::Cancelled, false, "cancelled", false),
            (ExecutionReportStatus::Success, false, "ok", false),
            (ExecutionReportStatus::Success, true, "incomplete", false),
        ];
        for (status, tail_incomplete, expected_status, uncertain) in cases {
            let execution = execution.clone().with_status(status);
            let output = if tail_incomplete {
                super::finished_execution_output(
                    &execution,
                    &backup,
                    &history,
                    &writeback,
                    &cleanup,
                    Some("任务事件接收端已经关闭".to_owned()),
                )
            } else {
                super::note_progress(
                    super::GuiOperationOutput::from_execution("执行完成", status, false),
                    Some("任务事件接收端已经关闭".to_owned()),
                )
            };
            let output = output.with_agent_status("代理状态已刷新".to_owned());
            assert!(
                output
                    .diagnostic_detail()
                    .is_some_and(|detail| detail.contains("进度通知失败")),
                "{status:?} 追加代理状态后应仍能读取诊断"
            );
            assert_eq!(output.agent_status(), Some("代理状态已刷新"));
            let record = super::operation_record(&Ok(output));
            assert_eq!(record.terminal.status(), expected_status);
            assert_eq!(record.terminal.uncertain(), uncertain);
            assert!(record.message.contains("进度通知失败"));
            assert_ne!(record.terminal, OperationTerminal::Failed);
            if status == ExecutionReportStatus::Success && !tail_incomplete {
                assert_eq!(record.terminal, OperationTerminal::Succeeded);
            }
            let signals = Signals;
            let logged = super::OperationContext::new(
                &root,
                super::OperationIdentity {
                    name: "执行计划",
                    workbook: Some("plan.xlsx"),
                },
                None,
                &super::super::operation_log::OperationSettings {
                    detailed_diagnostics: true,
                    read_error: None,
                },
                &signals,
            );
            assert!(logged.finish(&record).is_empty(), "{}", record.message);
        }
        let text = std::fs::read_dir(root.join("data/logs"))
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("-app.log")
                    .then(|| std::fs::read_to_string(path).unwrap())
            })
            .collect::<String>();
        assert!(text.contains("\"status\":\"unknown\""));
        assert!(text.contains("\"uncertain\":true"));
        assert!(text.contains("\"status\":\"cancelled\""));
        assert!(text.contains("\"uncertain\":false"));
        assert!(text.contains("进度通知失败"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn check_failure_keeps_both_error_chains_on_the_gui_log() {
        use crate::application::{
            AppError, AppErrorCode, CheckAndSaveOutcome, OperationTerminal, SessionCleanup,
        };
        use crate::interfaces::gui_controller::{check_failed_task, present_check_tail};

        let check = AppError::from_source(
            "plan.check",
            AppErrorCode::WorkbookInvalid,
            "计划不成立",
            std::io::Error::other("row 4"),
        )
        .with_context("sheet", "loadout_plan");
        let writeback = AppError::from_source(
            "check.workbook.writeback",
            AppErrorCode::WorkbookLocked,
            "工作簿被占用",
            std::io::Error::other("sharing violation"),
        )
        .with_context("path", "data/workbooks/plan.xlsx");
        let failed = check_failed_task(check, Err(writeback));
        let record = super::operation_record(&Err(failed));
        assert_eq!(record.terminal, OperationTerminal::Failed);
        assert!(
            record.detail.contains("阶段：plan.check"),
            "{}",
            record.detail
        );
        assert!(
            record.detail.contains("sheet=loadout_plan"),
            "{}",
            record.detail
        );
        assert!(record.detail.contains("原因：row 4"), "{}", record.detail);
        assert!(
            record.detail.contains("阶段：check.workbook.writeback"),
            "{}",
            record.detail
        );
        assert!(
            record.detail.contains("path=data/workbooks/plan.xlsx"),
            "{}",
            record.detail
        );
        assert!(
            record.detail.contains("原因：sharing violation"),
            "{}",
            record.detail
        );

        let cleanup = AppError::from_source(
            "game.cleanup.fixture",
            AppErrorCode::RuntimeBootstrapFailed,
            "清理失败",
            std::io::Error::other("socket closed"),
        )
        .with_context("check_stage", "read_without_check");
        let unread = CheckAndSaveOutcome::ReadWithoutCheck {
            cleanup: SessionCleanup::Failed(cleanup),
        };
        assert_eq!(unread.log_terminal(), OperationTerminal::Incomplete);
        let (summary, detail) = present_check_tail(&unread);
        let output = super::GuiOperationOutput::business_note(summary, unread.log_terminal())
            .with_diagnostic_detail(detail);
        let record = super::operation_record(&Ok(output));
        assert_eq!(record.terminal, OperationTerminal::Incomplete);
        assert!(
            record.detail.contains("阶段：game.cleanup.fixture"),
            "{}",
            record.detail
        );
        assert!(
            record.detail.contains("check_stage=read_without_check"),
            "{}",
            record.detail
        );
        assert!(
            record.detail.contains("原因：socket closed"),
            "{}",
            record.detail
        );

        let history = AppError::from_source(
            "check.history.save",
            AppErrorCode::HistoryWriteFailed,
            "历史保存失败",
            std::io::Error::other("history locked"),
        )
        .with_context("history_path", "data/history/plan.json");
        let writeback = AppError::from_source(
            "check.workbook.writeback",
            AppErrorCode::WorkbookLocked,
            "结果写回失败",
            std::io::Error::other("workbook locked"),
        )
        .with_context("path", "data/workbooks/plan.xlsx");
        let desired = crate::domain::DesiredState::new(Vec::new()).unwrap();
        let inventory = crate::domain::EquipmentInventoryPlan::new(Vec::new()).unwrap();
        let incomplete = CheckAndSaveOutcome::SaveIncomplete {
            report: crate::application::compile_workbook_without_modifications(
                &desired, &inventory,
            )
            .unwrap(),
            history: Err(history),
            writeback: Err(writeback),
        };
        assert_eq!(incomplete.log_terminal(), OperationTerminal::Incomplete);
        let (summary, detail) = present_check_tail(&incomplete);
        assert!(detail.contains("阶段：check.history.save"), "{detail}");
        assert!(
            detail.contains("history_path=data/history/plan.json"),
            "{detail}"
        );
        assert!(detail.contains("原因：history locked"), "{detail}");
        assert!(
            detail.contains("阶段：check.workbook.writeback"),
            "{detail}"
        );
        assert!(detail.contains("原因：workbook locked"), "{detail}");
        let output = super::GuiOperationOutput::business_note(summary, incomplete.log_terminal())
            .with_diagnostic_detail(detail);
        let record = super::operation_record(&Ok(output));
        assert_eq!(record.terminal, OperationTerminal::Incomplete);
    }
}

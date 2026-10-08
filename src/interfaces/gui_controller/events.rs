//! 消费后台任务事件，统一更新成功、失败、取消和结果待核对状态。

use super::{
    GuiCompletion, GuiController, GuiControllerError, GuiInstanceItem, GuiOperation,
    GuiOperationKind, GuiOperationOutput, GuiSupportSnapshot, GuiTaskOutput,
};
use std::sync::mpsc::TryRecvError;
use suzushiro_task_runtime::{TaskEvent, TaskFailure};

impl GuiController {
    /// 非阻塞消费当前任务全部已入队事件，并在终态回收线程。
    pub fn drain_events(&mut self) -> Result<bool, GuiControllerError> {
        let mut changed = false;
        let mut terminal = false;
        loop {
            let event = match self.activity.task().map(|running| running.task.try_recv()) {
                Some(Ok(event)) => event,
                Some(Err(TryRecvError::Empty)) | None => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    let running = self
                        .activity
                        .take_task()
                        .ok_or(GuiControllerError::MissingRunningTask)?;
                    let operation = running.operation.clone();
                    let join_result = running.task.finish();

                    let detail = match join_result {
                        Ok(()) => GuiControllerError::TaskChannelClosed.to_string(),
                        Err(error) => GuiControllerError::Join(error).to_string(),
                    };
                    self.apply_internal_failure(&operation, detail);
                    return Ok(true);
                }
            };
            changed = true;
            let operation = self
                .activity
                .task()
                .map(|running| running.operation.clone())
                .ok_or(GuiControllerError::MissingRunningTask)?;
            let kind = operation.kind();
            match event {
                TaskEvent::Progress(progress) => {
                    self.view.ready_indicator = false;
                    self.view.status = match (progress.units(), progress.percent()) {
                        (Some((completed, total)), Some(percent)) => format!(
                            "{} · 当前阶段 {completed}/{total}（{percent}%）",
                            kind.running_label()
                        ),
                        (None, Some(percent)) => format!("{} {percent}%", kind.running_label()),
                        _ => format!("{} · {}", kind.running_label(), progress.message()),
                    };
                    self.view.message = progress.message().to_owned();
                    self.view.progress = Some(progress);
                }
                TaskEvent::Succeeded { output } => {
                    let GuiTaskOutput {
                        operation: result,
                        diagnostics,
                        auxiliary,
                    } = output;
                    self.apply_operation_result(&operation, result);
                    if let Some(failure) = auxiliary {
                        self.note_auxiliary_failure(&failure);
                    }
                    if let Some(diagnostics) = diagnostics {
                        self.apply_diagnostics(diagnostics);
                    }
                    terminal = true;
                }
                TaskEvent::Failed { failure } => {
                    self.apply_operation_result(&operation, Err(failure));
                    terminal = true;
                }
                TaskEvent::Cancelled => {
                    self.apply_cancelled(kind);
                    terminal = true;
                }
            }
            if terminal {
                break;
            }
        }
        if terminal {
            let running = self
                .activity
                .take_task()
                .ok_or(GuiControllerError::MissingRunningTask)?;
            let operation = running.operation;
            let join_result = running.task.finish();

            if let Err(error) = join_result {
                self.apply_internal_failure(
                    &operation,
                    GuiControllerError::Join(error).to_string(),
                );
            }
        }
        Ok(changed)
    }

    fn note_auxiliary_failure(&mut self, failure: &TaskFailure) {
        if !self.view.message.is_empty() {
            self.view.message.push('；');
        }
        self.view.message.push_str(failure.user_message());
        let detail = self
            .view
            .last_failure_detail
            .get_or_insert_with(String::new);
        if !detail.is_empty() {
            detail.push_str("\n\n");
        }
        detail.push_str(failure.detail());
    }

    fn apply_diagnostics(
        &mut self,
        diagnostics: Result<super::GuiDiagnosticSnapshot, TaskFailure>,
    ) {
        let warnings = match diagnostics {
            Ok(mut snapshot) => {
                let previous = self
                    .view
                    .selected_diagnostic()
                    .map(|item| item.source().relative_path().to_owned());
                snapshot.items.extend(
                    self.view
                        .diagnostics
                        .drain(..)
                        .filter(|item| snapshot.failed_sources.contains(&item.source().kind())),
                );
                self.view.diagnostics = super::presentation::sort_diagnostic_items(snapshot.items);
                self.view.selected_diagnostic_index = previous
                    .as_deref()
                    .and_then(|path| {
                        self.view
                            .diagnostics
                            .iter()
                            .position(|item| item.source().relative_path() == path)
                    })
                    .or_else(|| (!self.view.diagnostics.is_empty()).then_some(0));
                snapshot.warnings
            }
            Err(error) => vec![error],
        };
        if !warnings.is_empty() {
            self.view
                .message
                .push_str("；历史与日志刷新失败，请查看错误详情");
            let detail = self
                .view
                .last_failure_detail
                .get_or_insert_with(String::new);
            for error in warnings {
                if !detail.is_empty() {
                    detail.push_str("\n\n");
                }
                detail.push_str(error.detail());
            }
        }
    }

    fn apply_operation_result(
        &mut self,
        operation: &GuiOperation,
        result: Result<GuiOperationOutput, TaskFailure>,
    ) {
        match result {
            Ok(output) => self.apply_success(operation, output),
            Err(failure) if failure.is_cancelled() => self.apply_cancelled(operation.kind()),
            Err(failure) => {
                let uncertain = failure.is_uncertain();
                self.apply_failure(operation, failure);
                if uncertain {
                    self.apply_uncertain_operation(operation.kind());
                }
            }
        }
    }

    fn apply_success(&mut self, operation: &GuiOperation, output: GuiOperationOutput) {
        if let Some(status) = output.agent_status() {
            self.view.agent_status = Some(status.to_owned());
        }
        if let Some(preferences) = output.preferences() {
            self.view.preferences = Some(*preferences);
        }
        let kind = operation.kind();
        let completion = output.completion();
        self.view.progress_finished = completion == GuiCompletion::Success;
        if kind == GuiOperationKind::Initialize {
            self.view.application_ready = completion == GuiCompletion::Success;
            if completion == GuiCompletion::Success {
                self.view.generation_recheck_required = false;
            }
            if completion == GuiCompletion::Attention {
                self.view.workbooks.clear();
                self.view.selected_workbook_index = None;
            }
        }
        if kind == GuiOperationKind::OpenDiagnostic && completion == GuiCompletion::Attention {
            self.invalidate_support();
        }
        match (operation, completion) {
            (GuiOperation::ExecutePlan(_), GuiCompletion::Attention) => {
                self.require_execution_check();
            }
            (GuiOperation::SynchronizeAndGenerate, GuiCompletion::Attention) => {
                self.view.generation_recheck_required = true;
                self.require_execution_check();
            }
            (GuiOperation::CheckPlan(workbook_name), GuiCompletion::Success)
                if self.view.execution_check_required =>
            {
                self.view.checked_execution_workbook = Some(workbook_name.clone());
            }
            _ => {}
        }
        let initialization_attention =
            kind == GuiOperationKind::Initialize && completion == GuiCompletion::Attention;
        if !initialization_attention {
            if let Some(workbooks) = output.workbooks() {
                self.replace_workbooks(workbooks.to_vec(), output.selected_workbook());
            } else if let Some(selected_workbook) = output.selected_workbook() {
                self.ensure_workbook_selected(selected_workbook.to_owned());
            }
        }
        if let Some(snapshot) = output.support_snapshot() {
            self.replace_support(snapshot.clone());
        }
        self.view.ready_indicator =
            kind == GuiOperationKind::Initialize && completion == GuiCompletion::Success;
        self.view.status = if output.terminal() == crate::application::OperationTerminal::Cancelled
        {
            "已取消"
        } else {
            match (kind, completion) {
                (GuiOperationKind::Initialize, GuiCompletion::Success) => "已就绪",
                (_, GuiCompletion::Success) => "操作完成",
                (_, GuiCompletion::Attention) => "需要核对",
            }
        }
        .to_owned();
        self.view.message = output.summary().to_owned();
        self.view.last_failure_detail = output.diagnostic_detail().map(str::to_owned);
    }

    fn apply_failure(&mut self, operation: &GuiOperation, failure: TaskFailure) {
        let kind = operation.kind();
        if kind == GuiOperationKind::Initialize {
            self.view.application_ready = false;
            self.view.workbooks.clear();
            self.view.selected_workbook_index = None;
        }
        if kind == GuiOperationKind::ExecutePlan {
            self.require_execution_check();
        }
        self.view.ready_indicator = false;
        self.view.status = if kind == GuiOperationKind::Initialize {
            "检查未通过"
        } else {
            "操作失败"
        }
        .to_owned();
        self.view.message = if kind == GuiOperationKind::ExecutePlan {
            format!(
                "{}；请先核对执行证据并成功检查当前工作簿计划，禁止直接重试",
                failure.user_message()
            )
        } else {
            failure.user_message().to_owned()
        };
        self.view.last_failure_detail = Some(failure.detail().to_owned());
    }

    fn apply_internal_failure(&mut self, operation: &GuiOperation, detail: String) {
        self.apply_failure(
            operation,
            TaskFailure::unexpected_termination(
                "后台操作意外终止，请刷新后再试",
                format!("worker_recovery: {detail}"),
            ),
        );
        self.apply_uncertain_operation(operation.kind());
    }

    fn apply_uncertain_operation(&mut self, operation: GuiOperationKind) {
        match operation {
            GuiOperationKind::SynchronizeAndGenerate => {
                self.view.generation_recheck_required = true;
                self.require_execution_check();
                self.view
                    .message
                    .push_str("；生成结果不确定，必须刷新应用状态后再操作");
            }
            GuiOperationKind::ExecutePlan => {
                self.require_execution_check();
            }
            GuiOperationKind::OpenDiagnostic => {
                self.invalidate_support();
                self.view.message.push_str("；结果不确定，必须刷新后再操作");
            }
            GuiOperationKind::Initialize
            | GuiOperationKind::AgentStatus
            | GuiOperationKind::UnloadAgent
            | GuiOperationKind::LoadSettings
            | GuiOperationKind::SaveSettings
            | GuiOperationKind::CheckPlan
            | GuiOperationKind::OpenWorkbook => {}
        }
    }

    pub(super) fn require_execution_check(&mut self) {
        self.view.execution_check_required = true;
        self.view.checked_execution_workbook = None;
    }

    fn apply_cancelled(&mut self, operation: GuiOperationKind) {
        if operation == GuiOperationKind::Initialize {
            self.view.application_ready = false;
            self.view.workbooks.clear();
            self.view.selected_workbook_index = None;
        }
        if matches!(
            operation,
            GuiOperationKind::SynchronizeAndGenerate | GuiOperationKind::ExecutePlan
        ) {
            self.require_execution_check();
        }
        self.view.ready_indicator = false;
        self.view.status = "已取消".to_owned();
        self.view.message = format!("{}已在安全检查点停止", operation.label());
        self.view.last_failure_detail = None;
    }

    pub(super) fn invalidate_support(&mut self) {
        self.invalidate_instances();
        self.view.diagnostics.clear();
        self.view.selected_diagnostic_index = None;
    }

    pub(super) fn invalidate_instances(&mut self) {
        self.view.instances.clear();
        self.view.selected_instance_index = None;
        self.view.support_loaded = false;
    }

    fn replace_workbooks(&mut self, mut workbooks: Vec<String>, preferred: Option<&str>) {
        let previous = preferred.or_else(|| self.view.selected_workbook());
        workbooks.sort();
        workbooks.dedup();
        let selected = previous
            .and_then(|name| workbooks.iter().position(|candidate| candidate == name))
            .or_else(|| (!workbooks.is_empty()).then_some(0));
        self.view.workbooks = workbooks;
        self.view.selected_workbook_index = selected;
    }

    fn ensure_workbook_selected(&mut self, workbook_name: String) {
        if !self.view.workbooks.contains(&workbook_name) {
            self.view.workbooks.push(workbook_name.clone());
            self.view.workbooks.sort();
        }
        self.view.selected_workbook_index = self
            .view
            .workbooks
            .iter()
            .position(|candidate| candidate == &workbook_name);
    }

    fn replace_support(&mut self, mut snapshot: GuiSupportSnapshot) {
        snapshot
            .instances
            .sort_by(|left, right| left.instance_id().cmp(right.instance_id()));
        let selected_index = match snapshot.selected_instance.as_deref() {
            Some(selected) => snapshot
                .instances
                .iter()
                .position(|item| item.instance_id() == selected),
            None => snapshot
                .instances
                .iter()
                .position(GuiInstanceItem::is_available)
                .or_else(|| (!snapshot.instances.is_empty()).then_some(0)),
        };
        self.view.instances = snapshot.instances;
        self.view.selected_instance_index = selected_index;
        self.view.support_loaded = true;
    }
}

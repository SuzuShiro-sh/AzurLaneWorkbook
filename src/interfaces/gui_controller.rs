//! 管理原生窗口状态和后台任务协议，不依赖具体业务适配器。

use std::sync::Arc;

use thiserror::Error;

use crate::application::{DiagnosticArtifactRef, OperationTerminal};

use suzushiro_task_runtime::{
    BackgroundTask, RunningTask, TaskContext, TaskFailure, TaskJoinError,
};
mod events;
mod presentation;
mod view_state;

pub(crate) use presentation::{
    app_failure, append_error_chain, check_failed_task, finished_execution_message,
    generation_error_output, initialized_operation_output, ordered_diagnostic_items,
    present_check_tail, workbook_name_from_relative_path,
};

pub use view_state::{GuiControlsState, GuiDiagnosticItem, GuiInstanceItem, GuiView, GuiViewState};
pub(crate) use view_state::{GuiDiagnosticSnapshot, GuiSupportSnapshot};

type GuiTaskHandler = dyn Fn(
        GuiOperation,
        Option<String>,
        TaskContext<GuiTaskOutput>,
    ) -> Result<GuiTaskOutput, TaskFailure>
    + Send
    + Sync
    + 'static;

/// 原生窗口允许用户触发的稳定操作集合。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuiAction {
    Refresh,
    LoadSettings,
    AgentStatus,
    UnloadAgent,
    SaveSettings {
        original: crate::application::UserPreferences,
        preferences: crate::application::UserPreferences,
    },
    SynchronizeAndGenerate,
    CheckPlan,
    ExecutePlan,
    OpenWorkbook,
}

/// 为每次 GUI 操作建立独立后台任务，避免跨线程共享非线程安全的服务端口。
#[derive(Clone)]
pub struct GuiTaskFactory {
    handler: Arc<GuiTaskHandler>,
}

/// 为窗口生命周期测试提供可取消的后台任务，不访问设备或业务文件。
#[cfg(feature = "native-gui-test")]
pub fn native_gui_fixture_factory() -> GuiTaskFactory {
    GuiTaskFactory::from_handler(|_, _, context| {
        while !context.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("任务已回收")),
            None,
        ))
    })
}

impl GuiTaskFactory {
    /// 从组合根提供的线程安全处理器建立任务工厂。
    pub(crate) fn from_handler<F>(handler: F) -> Self
    where
        F: Fn(
                GuiOperation,
                Option<String>,
                TaskContext<GuiTaskOutput>,
            ) -> Result<GuiTaskOutput, TaskFailure>
            + Send
            + Sync
            + 'static,
    {
        Self {
            handler: Arc::new(handler),
        }
    }

    fn task(
        &self,
        operation: GuiOperation,
        instance: Option<String>,
    ) -> BackgroundTask<GuiTaskOutput> {
        let handler = Arc::clone(&self.handler);
        BackgroundTask::new(move |context| handler(operation.clone(), instance.clone(), context))
    }
}

/// 拥有唯一后台任务并向界面暴露确定性状态的控制器。
pub struct GuiController {
    task_factory: GuiTaskFactory,
    view: GuiViewState,
    activity: GuiActivity,
}

impl GuiController {
    /// 建立尚未开始应用初始化的控制器。
    pub fn new(task_factory: GuiTaskFactory) -> Self {
        Self {
            task_factory,
            view: GuiViewState::new(),
            activity: GuiActivity::Idle,
        }
    }

    /// 返回窗口当前应完整渲染的不可变状态。是否运行只来自任务句柄。
    pub fn view(&self) -> GuiView<'_> {
        GuiView::new(&self.view, self.activity.is_active())
    }

    /// 启动首次应用初始化和工作簿目录读取。
    pub fn start_initialization<N>(&mut self, notifier: N) -> Result<(), GuiControllerError>
    where
        N: Fn() + Send + Sync + 'static,
    {
        self.start_operation(GuiOperation::Initialize, notifier)
    }

    /// 把用户动作转换成绑定当前受控工作簿的后台操作。
    pub fn start<N>(&mut self, action: GuiAction, notifier: N) -> Result<(), GuiControllerError>
    where
        N: Fn() + Send + Sync + 'static,
    {
        let active = self.activity.is_active();
        let operation = match action {
            GuiAction::Refresh => GuiOperation::Initialize,
            GuiAction::LoadSettings => GuiOperation::LoadSettings,
            GuiAction::AgentStatus => {
                self.view.allow_game_action(active)?;
                GuiOperation::AgentStatus
            }
            GuiAction::UnloadAgent => {
                self.view.allow_game_action(active)?;
                GuiOperation::UnloadAgent
            }
            GuiAction::SaveSettings {
                original,
                preferences,
            } => GuiOperation::SaveSettings {
                original,
                preferences,
            },
            GuiAction::SynchronizeAndGenerate => {
                self.view.allow_synchronize(active)?;
                GuiOperation::SynchronizeAndGenerate
            }
            GuiAction::CheckPlan => {
                GuiOperation::CheckPlan(self.view.allow_check(active)?.to_owned())
            }
            GuiAction::ExecutePlan => {
                GuiOperation::ExecutePlan(self.view.allow_execute(active)?.to_owned())
            }
            GuiAction::OpenWorkbook => {
                GuiOperation::OpenWorkbook(self.view.allow_open(active)?.to_owned())
            }
        };
        self.start_operation(operation, notifier)
    }

    /// 将当前选中的诊断文件交给后台校验并打开。
    pub fn start_diagnostic_open<N>(&mut self, notifier: N) -> Result<(), GuiControllerError>
    where
        N: Fn() + Send + Sync + 'static,
    {
        let source = self
            .view
            .allow_diagnostic_open(self.activity.is_active())?
            .clone();
        self.start_operation(GuiOperation::OpenDiagnostic(source), notifier)
    }

    /// 选择目录中已经通过应用边界校验的工作簿。
    pub fn select_workbook(&mut self, index: usize) -> Result<(), GuiControllerError> {
        if self.activity.is_active() {
            return Err(GuiControllerError::Busy);
        }
        if index >= self.view.workbooks.len() {
            return Err(GuiControllerError::InvalidWorkbookSelection { index });
        }
        self.view.selected_workbook_index = Some(index);
        Ok(())
    }

    /// 选择只含元数据的历史或日志项。
    pub fn select_diagnostic(&mut self, index: usize) -> Result<(), GuiControllerError> {
        if self.activity.is_active() {
            return Err(GuiControllerError::Busy);
        }
        if index >= self.view.diagnostics.len() {
            return Err(GuiControllerError::InvalidDiagnosticSelection { index });
        }
        self.view.selected_diagnostic_index = Some(index);
        Ok(())
    }

    /// 选择本次连接目标，并使先前目标的计划检查结果失效。
    pub fn select_instance(&mut self, index: usize) -> Result<(), GuiControllerError> {
        self.require_application_ready()?;
        if !self.view.support_loaded || index >= self.view.instances.len() {
            return Err(GuiControllerError::InvalidInstanceSelection { index });
        }
        if self.view.selected_instance_index != Some(index) {
            self.view.selected_instance_index = Some(index);
            self.view.agent_status = None;
            self.require_execution_check();
        }
        Ok(())
    }

    /// 请求当前操作在下一个安全检查点停止。
    pub fn request_cancel(&mut self) -> bool {
        match std::mem::replace(&mut self.activity, GuiActivity::Idle) {
            GuiActivity::Running(running) => {
                running.task.cancel();
                self.activity = GuiActivity::Cancelling(running);
                self.view.ready_indicator = false;
                self.view.status = "正在取消".to_owned();
                self.view.message = "等待当前安全检查点；已经完成的结果仍会保留".to_owned();
                true
            }
            other => {
                self.activity = other;
                false
            }
        }
    }

    fn start_operation<N>(
        &mut self,
        operation: GuiOperation,
        notifier: N,
    ) -> Result<(), GuiControllerError>
    where
        N: Fn() + Send + Sync + 'static,
    {
        if self.activity.is_active() {
            return Err(GuiControllerError::Busy);
        }
        let instance = self.view.selected_instance().map(str::to_owned);
        if matches!(
            operation,
            GuiOperation::Initialize
                | GuiOperation::AgentStatus
                | GuiOperation::UnloadAgent
                | GuiOperation::SynchronizeAndGenerate
                | GuiOperation::CheckPlan(_)
                | GuiOperation::ExecutePlan(_)
        ) {
            self.view.agent_status = None;
        }
        if matches!(operation, GuiOperation::LoadSettings) {
            self.view.preferences = None;
        }
        let kind = operation.kind();
        if kind == GuiOperationKind::Initialize {
            self.invalidate_instances();
        }
        if self.view.execution_check_required
            && matches!(
                kind,
                GuiOperationKind::Initialize
                    | GuiOperationKind::SynchronizeAndGenerate
                    | GuiOperationKind::CheckPlan
            )
        {
            self.view.checked_execution_workbook = None;
        }
        self.view.ready_indicator = false;
        self.view.status = kind.starting_status().to_owned();
        self.view.message = kind.starting_message().to_owned();
        self.view.progress = None;
        self.view.progress_finished = false;
        self.view.last_failure_detail = None;
        let task = self
            .task_factory
            .task(operation.clone(), instance)
            .spawn(notifier);
        self.activity = GuiActivity::Running(RunningGuiTask { operation, task });
        Ok(())
    }

    fn require_application_ready(&self) -> Result<(), GuiControllerError> {
        if self.activity.is_active() {
            return Err(GuiControllerError::Busy);
        }
        if !self.view.application_ready {
            return Err(GuiControllerError::ApplicationNotReady);
        }
        Ok(())
    }
}

struct RunningGuiTask {
    operation: GuiOperation,
    task: RunningTask<GuiTaskOutput>,
}

/// 后台任务句柄只存在于运行或取消中。空闲时没有句柄，也不能单独记下取消。
enum GuiActivity {
    Idle,
    Running(RunningGuiTask),
    Cancelling(RunningGuiTask),
}

impl GuiActivity {
    fn is_active(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    fn task(&self) -> Option<&RunningGuiTask> {
        match self {
            Self::Idle => None,
            Self::Running(task) | Self::Cancelling(task) => Some(task),
        }
    }

    fn take_task(&mut self) -> Option<RunningGuiTask> {
        match std::mem::replace(self, Self::Idle) {
            Self::Idle => None,
            Self::Running(task) | Self::Cancelling(task) => Some(task),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GuiOperation {
    Initialize,
    LoadSettings,
    AgentStatus,
    UnloadAgent,
    SaveSettings {
        original: crate::application::UserPreferences,
        preferences: crate::application::UserPreferences,
    },
    SynchronizeAndGenerate,
    CheckPlan(String),
    ExecutePlan(String),
    OpenWorkbook(String),
    OpenDiagnostic(DiagnosticArtifactRef),
}

impl GuiOperation {
    const fn kind(&self) -> GuiOperationKind {
        match self {
            Self::Initialize => GuiOperationKind::Initialize,
            Self::LoadSettings => GuiOperationKind::LoadSettings,
            Self::AgentStatus => GuiOperationKind::AgentStatus,
            Self::UnloadAgent => GuiOperationKind::UnloadAgent,
            Self::SaveSettings { .. } => GuiOperationKind::SaveSettings,
            Self::SynchronizeAndGenerate => GuiOperationKind::SynchronizeAndGenerate,
            Self::CheckPlan(_) => GuiOperationKind::CheckPlan,
            Self::ExecutePlan(_) => GuiOperationKind::ExecutePlan,
            Self::OpenWorkbook(_) => GuiOperationKind::OpenWorkbook,
            Self::OpenDiagnostic(_) => GuiOperationKind::OpenDiagnostic,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuiOperationKind {
    Initialize,
    LoadSettings,
    AgentStatus,
    UnloadAgent,
    SaveSettings,
    SynchronizeAndGenerate,
    CheckPlan,
    ExecutePlan,
    OpenWorkbook,
    OpenDiagnostic,
}

impl GuiOperationKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Initialize => "应用初始化",
            Self::LoadSettings => "设置读取",
            Self::AgentStatus => "代理状态",
            Self::UnloadAgent => "代理卸载",
            Self::SaveSettings => "设置保存",
            Self::SynchronizeAndGenerate => "同步并生成",
            Self::CheckPlan => "计划检查",
            Self::ExecutePlan => "计划执行",
            Self::OpenWorkbook => "工作簿打开",
            Self::OpenDiagnostic => "日志打开",
        }
    }

    const fn running_label(self) -> &'static str {
        match self {
            Self::Initialize => "刷新中",
            Self::LoadSettings => "读取中",
            Self::AgentStatus => "查询中",
            Self::UnloadAgent => "卸载中",
            Self::SaveSettings => "保存中",
            Self::SynchronizeAndGenerate => "同步中",
            Self::CheckPlan => "检查中",
            Self::ExecutePlan => "执行中",
            Self::OpenWorkbook => "打开中",
            Self::OpenDiagnostic => "打开中",
        }
    }

    const fn starting_status(self) -> &'static str {
        self.running_label()
    }

    const fn starting_message(self) -> &'static str {
        match self {
            Self::Initialize => "正在初始化应用并刷新工作簿、实例、历史和日志",
            Self::LoadSettings => "正在读取设置",
            Self::AgentStatus => "正在检查所选实例的代理状态",
            Self::UnloadAgent => "正在卸载所选实例的代理",
            Self::SaveSettings => "正在保存设置",
            Self::SynchronizeAndGenerate => "正在准备读取游戏状态",
            Self::CheckPlan => "正在准备检查所选工作簿",
            Self::ExecutePlan => "正在准备执行所选工作簿",
            Self::OpenWorkbook => "正在准备打开所选工作簿",
            Self::OpenDiagnostic => "正在核对并打开所选日志",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GuiCompletion {
    Success,
    Attention,
}

/// 一次界面操作的业务结果。每种操作只携带自己的字段。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GuiOperationOutput {
    Initialized {
        summary: String,
        completion: GuiCompletion,
        workbooks: Vec<String>,
        support: Option<GuiSupportSnapshot>,
        agent_status: Option<String>,
        diagnostic_detail: Option<String>,
    },
    Settings {
        summary: String,
        preferences: crate::application::UserPreferences,
    },
    Agent {
        summary: String,
        status: String,
        completion: GuiCompletion,
    },
    Workbook {
        summary: String,
        completion: GuiCompletion,
        terminal: OperationTerminal,
        selected_workbook: Option<String>,
        diagnostic_detail: Option<String>,
        agent_status: Option<String>,
    },
    Support {
        summary: String,
        completion: GuiCompletion,
        terminal: OperationTerminal,
        snapshot: GuiSupportSnapshot,
        diagnostic_detail: Option<String>,
    },
    Noted {
        summary: String,
        completion: GuiCompletion,
        terminal: OperationTerminal,
        diagnostic_detail: Option<String>,
        agent_status: Option<String>,
    },
}

/// 一次后台操作交付的业务终态，以及独立的诊断目录结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GuiTaskOutput {
    operation: Result<GuiOperationOutput, TaskFailure>,
    diagnostics: Option<Result<GuiDiagnosticSnapshot, TaskFailure>>,
    auxiliary: Option<TaskFailure>,
}

impl GuiTaskOutput {
    pub(crate) fn new(
        operation: Result<GuiOperationOutput, TaskFailure>,
        diagnostics: Option<Result<GuiDiagnosticSnapshot, TaskFailure>>,
    ) -> Self {
        Self {
            operation,
            diagnostics,
            auxiliary: None,
        }
    }
    /// 日志等辅助故障附在本次主结果上，不改变业务终态。
    pub(crate) fn with_auxiliary(mut self, failure: TaskFailure) -> Self {
        self.auxiliary = Some(failure);
        self
    }
}

impl GuiOperationOutput {
    pub(crate) fn success(summary: impl Into<String>) -> Self {
        Self::Noted {
            summary: summary.into(),
            completion: GuiCompletion::Success,
            terminal: OperationTerminal::Succeeded,
            diagnostic_detail: None,
            agent_status: None,
        }
    }

    pub(crate) fn attention(summary: impl Into<String>) -> Self {
        Self::Noted {
            summary: summary.into(),
            completion: GuiCompletion::Attention,
            terminal: OperationTerminal::Failed,
            diagnostic_detail: None,
            agent_status: None,
        }
    }

    /// 用已经确定的业务终态建立说明结果，诊断由调用方另行附上。
    pub(crate) fn business_note(summary: impl Into<String>, terminal: OperationTerminal) -> Self {
        let completion = if terminal == OperationTerminal::Succeeded {
            GuiCompletion::Success
        } else {
            GuiCompletion::Attention
        };
        Self::Noted {
            summary: summary.into(),
            completion,
            terminal,
            diagnostic_detail: None,
            agent_status: None,
        }
    }

    /// 由执行状态和收尾是否完成得到界面结果，进度提示不能改写这个终态。
    pub(crate) fn from_execution(
        summary: impl Into<String>,
        status: crate::application::ExecutionReportStatus,
        tail_incomplete: bool,
    ) -> Self {
        let mut terminal = OperationTerminal::from_execution(status);
        if tail_incomplete {
            terminal = terminal.with_incomplete_tail();
        }
        let completion = if terminal == OperationTerminal::Succeeded {
            GuiCompletion::Success
        } else {
            GuiCompletion::Attention
        };
        Self::Noted {
            summary: summary.into(),
            completion,
            terminal,
            diagnostic_detail: None,
            agent_status: None,
        }
    }

    pub(crate) fn with_agent_status(self, status: String) -> Self {
        match self {
            Self::Initialized {
                summary,
                completion,
                workbooks,
                support,
                diagnostic_detail,
                ..
            } => Self::Initialized {
                summary,
                completion,
                workbooks,
                support,
                agent_status: Some(status),
                diagnostic_detail,
            },
            Self::Agent {
                summary,
                completion,
                ..
            } => Self::Agent {
                summary,
                status,
                completion,
            },
            Self::Workbook {
                summary,
                completion,
                terminal,
                selected_workbook,
                diagnostic_detail,
                ..
            } => Self::Workbook {
                summary,
                completion,
                terminal,
                selected_workbook,
                diagnostic_detail,
                agent_status: Some(status),
            },
            Self::Noted {
                summary,
                completion,
                terminal,
                diagnostic_detail,
                ..
            } => Self::Noted {
                summary,
                completion,
                terminal,
                diagnostic_detail,
                agent_status: Some(status),
            },
            other => other,
        }
    }

    pub(crate) fn with_workbooks(self, workbooks: Vec<String>) -> Self {
        match self {
            Self::Initialized {
                summary,
                completion,
                support,
                agent_status,
                diagnostic_detail,
                ..
            } => Self::Initialized {
                summary,
                completion,
                workbooks,
                support,
                agent_status,
                diagnostic_detail,
            },
            Self::Support {
                summary,
                completion,
                snapshot,
                diagnostic_detail,
                ..
            } => Self::Initialized {
                summary,
                completion,
                workbooks,
                support: Some(snapshot),
                agent_status: None,
                diagnostic_detail,
            },
            Self::Noted {
                summary,
                completion,
                diagnostic_detail,
                agent_status,
                ..
            } => Self::Initialized {
                summary,
                completion,
                workbooks,
                support: None,
                agent_status,
                diagnostic_detail,
            },
            other => other,
        }
    }

    pub(crate) fn with_preferences(self, preferences: crate::application::UserPreferences) -> Self {
        match self {
            Self::Noted { summary, .. } | Self::Settings { summary, .. } => Self::Settings {
                summary,
                preferences,
            },
            other => other,
        }
    }

    pub(crate) fn with_selected_workbook(self, workbook_name: String) -> Self {
        match self {
            Self::Workbook {
                summary,
                completion,
                terminal,
                diagnostic_detail,
                agent_status,
                ..
            } => Self::Workbook {
                summary,
                completion,
                terminal,
                selected_workbook: Some(workbook_name),
                diagnostic_detail,
                agent_status,
            },
            Self::Noted {
                summary,
                completion,
                terminal,
                diagnostic_detail,
                agent_status,
            } => Self::Workbook {
                summary,
                completion,
                terminal,
                selected_workbook: Some(workbook_name),
                diagnostic_detail,
                agent_status,
            },
            other => other,
        }
    }

    pub(crate) fn diagnostic_detail(&self) -> Option<&str> {
        match self {
            Self::Initialized {
                diagnostic_detail, ..
            }
            | Self::Workbook {
                diagnostic_detail, ..
            }
            | Self::Support {
                diagnostic_detail, ..
            }
            | Self::Noted {
                diagnostic_detail, ..
            } => diagnostic_detail.as_deref(),
            Self::Settings { .. } | Self::Agent { .. } => None,
        }
    }

    pub(crate) fn with_diagnostic_detail(self, detail: String) -> Self {
        match self {
            Self::Initialized {
                summary,
                completion,
                workbooks,
                support,
                agent_status,
                ..
            } => Self::Initialized {
                summary,
                completion,
                workbooks,
                support,
                agent_status,
                diagnostic_detail: Some(detail),
            },
            Self::Workbook {
                summary,
                completion,
                terminal,
                selected_workbook,
                agent_status,
                ..
            } => Self::Workbook {
                summary,
                completion,
                terminal,
                selected_workbook,
                diagnostic_detail: Some(detail),
                agent_status,
            },
            Self::Support {
                summary,
                completion,
                terminal,
                snapshot,
                ..
            } => Self::Support {
                summary,
                completion,
                terminal,
                snapshot,
                diagnostic_detail: Some(detail),
            },
            Self::Noted {
                summary,
                completion,
                terminal,
                agent_status,
                ..
            } => Self::Noted {
                summary,
                completion,
                terminal,
                diagnostic_detail: Some(detail),
                agent_status,
            },
            other => other,
        }
    }

    pub(crate) fn with_support_snapshot(self, snapshot: GuiSupportSnapshot) -> Self {
        match self {
            Self::Initialized {
                summary,
                completion,
                workbooks,
                agent_status,
                diagnostic_detail,
                ..
            } => Self::Initialized {
                summary,
                completion,
                workbooks,
                support: Some(snapshot),
                agent_status,
                diagnostic_detail,
            },
            Self::Noted {
                summary,
                completion,
                terminal,
                diagnostic_detail,
                ..
            }
            | Self::Support {
                summary,
                completion,
                terminal,
                diagnostic_detail,
                ..
            } => Self::Support {
                summary,
                completion,
                terminal,
                snapshot,
                diagnostic_detail,
            },
            other => other,
        }
    }

    #[cfg(test)]
    pub(crate) const fn is_attention(&self) -> bool {
        matches!(self.completion(), GuiCompletion::Attention)
    }

    /// 返回写入操作日志的终态。界面文案不参与这个判断。
    pub(crate) const fn terminal(&self) -> OperationTerminal {
        match self {
            Self::Noted { terminal, .. }
            | Self::Workbook { terminal, .. }
            | Self::Support { terminal, .. } => *terminal,
            Self::Agent { completion, .. } => match completion {
                GuiCompletion::Success => OperationTerminal::Succeeded,
                GuiCompletion::Attention => OperationTerminal::Failed,
            },
            Self::Initialized { .. } | Self::Settings { .. } => OperationTerminal::Succeeded,
        }
    }

    pub(crate) fn summary(&self) -> &str {
        match self {
            Self::Initialized { summary, .. }
            | Self::Settings { summary, .. }
            | Self::Agent { summary, .. }
            | Self::Workbook { summary, .. }
            | Self::Support { summary, .. }
            | Self::Noted { summary, .. } => summary,
        }
    }

    pub(crate) const fn completion(&self) -> GuiCompletion {
        match self {
            Self::Initialized { completion, .. }
            | Self::Agent { completion, .. }
            | Self::Workbook { completion, .. }
            | Self::Support { completion, .. }
            | Self::Noted { completion, .. } => *completion,
            Self::Settings { .. } => GuiCompletion::Success,
        }
    }

    pub(crate) fn agent_status(&self) -> Option<&str> {
        match self {
            Self::Initialized { agent_status, .. }
            | Self::Workbook { agent_status, .. }
            | Self::Noted { agent_status, .. } => agent_status.as_deref(),
            Self::Agent { status, .. } => Some(status),
            Self::Settings { .. } | Self::Support { .. } => None,
        }
    }

    pub(crate) fn preferences(&self) -> Option<&crate::application::UserPreferences> {
        match self {
            Self::Settings { preferences, .. } => Some(preferences),
            _ => None,
        }
    }

    pub(crate) fn workbooks(&self) -> Option<&[String]> {
        match self {
            Self::Initialized { workbooks, .. } => Some(workbooks),
            _ => None,
        }
    }

    pub(crate) fn selected_workbook(&self) -> Option<&str> {
        match self {
            Self::Workbook {
                selected_workbook, ..
            } => selected_workbook.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn support_snapshot(&self) -> Option<&GuiSupportSnapshot> {
        match self {
            Self::Initialized { support, .. } => support.as_ref(),
            Self::Support { snapshot, .. } => Some(snapshot),
            _ => None,
        }
    }

    /// 进度或诊断提示只追加说明，不改业务终态和完成分类。
    pub(crate) fn with_auxiliary_notice(mut self, notice: impl Into<String>) -> Self {
        let notice = notice.into();
        let (summary, detail) = match &mut self {
            Self::Initialized {
                summary,
                diagnostic_detail,
                ..
            }
            | Self::Workbook {
                summary,
                diagnostic_detail,
                ..
            }
            | Self::Support {
                summary,
                diagnostic_detail,
                ..
            }
            | Self::Noted {
                summary,
                diagnostic_detail,
                ..
            } => (summary, Some(diagnostic_detail)),
            Self::Settings { summary, .. } | Self::Agent { summary, .. } => (summary, None),
        };
        if !summary.is_empty() {
            summary.push('；');
        }
        summary.push_str(&notice);
        if let Some(detail) = detail {
            match detail {
                Some(existing) => {
                    existing.push_str("\n\n");
                    existing.push_str(&notice);
                }
                None => *detail = Some(notice),
            }
        }
        self
    }

    /// 在操作收尾后的最终目录计数可用时补上摘要。
    pub(crate) fn append_summary(&mut self, suffix: &str) {
        let summary = match self {
            Self::Initialized { summary, .. }
            | Self::Settings { summary, .. }
            | Self::Agent { summary, .. }
            | Self::Workbook { summary, .. }
            | Self::Support { summary, .. }
            | Self::Noted { summary, .. } => summary,
        };
        summary.push_str(suffix);
    }
}

/// 控制器拒绝不一致动作或无法回收后台线程时的稳定错误。
#[derive(Debug, Error)]
pub enum GuiControllerError {
    #[error("已有后台操作正在运行")]
    Busy,
    #[error("应用尚未完成初始化")]
    ApplicationNotReady,
    #[error("请选择已启动且可用的模拟器实例")]
    InstanceUnavailable,
    #[error("实例选择下标 {index} 不在当前目录中")]
    InvalidInstanceSelection { index: usize },
    #[error("尚未选择工作簿")]
    WorkbookNotSelected,
    #[error("上次执行结果仍待核对，请先成功检查当前工作簿计划")]
    ExecutionCheckRequired,
    #[error("上次工作簿生成结果仍待核对，请先刷新应用状态")]
    GenerationRecheckRequired,
    #[error("工作簿选择下标 {index} 超出目录范围")]
    InvalidWorkbookSelection { index: usize },
    #[error("尚未刷新模拟器实例和诊断目录")]
    SupportNotLoaded,
    #[error("尚未选择历史或日志诊断项")]
    DiagnosticNotSelected,
    #[error("诊断选择下标 {index} 超出目录范围")]
    InvalidDiagnosticSelection { index: usize },
    #[error("后台任务通道在终态前关闭")]
    TaskChannelClosed,
    #[error("后台任务状态缺少运行句柄")]
    MissingRunningTask,
    #[error("后台工作线程回收失败: {0}")]
    Join(TaskJoinError),
}

#[cfg(test)]
mod tests;

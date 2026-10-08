//! 定义窗口展示模型，并从当前状态计算控件可用性。

use crate::application::{DiagnosticArtifactRef, EmulatorInstanceState};

/// 原生控件是否可交互的完整快照。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuiControlsState {
    pub refresh_enabled: bool,
    pub instance_selection_enabled: bool,
    pub diagnostic_selection_enabled: bool,
    pub diagnostic_open_enabled: bool,
    pub failure_detail_enabled: bool,
    pub workbook_selection_enabled: bool,
    pub synchronize_enabled: bool,
    pub check_enabled: bool,
    pub execute_enabled: bool,
    pub open_enabled: bool,
}

/// 原生窗口实例下拉框中的单个脱敏选项。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuiInstanceItem {
    instance_id: String,
    label: String,
    state: EmulatorInstanceState,
}

impl GuiInstanceItem {
    /// 建立已通过管理器目录校验的显式实例项。
    pub(crate) fn explicit(
        instance_id: String,
        label: String,
        state: EmulatorInstanceState,
    ) -> Self {
        Self {
            instance_id,
            label,
            state,
        }
    }

    /// 返回带提供方命名空间的实例标识。
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// 实例已启动且端点信息有效时允许连接。
    pub const fn is_available(&self) -> bool {
        matches!(self.state, EmulatorInstanceState::Ready)
    }

    /// 返回下拉框显示文本。
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// 原生窗口诊断下拉框中的元数据项。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuiDiagnosticItem {
    label: String,
    kind_label: String,
    status_label: Option<String>,
    workbook_name: Option<String>,
    source: DiagnosticArtifactRef,
    pub(super) timestamp_unix_millis: i64,
    pub(super) sequence: Option<u64>,
}

impl GuiDiagnosticItem {
    /// 从已核对的诊断身份建立展示项。路径、大小和摘要只保留在身份里。
    pub(crate) fn new(
        label: String,
        kind_label: String,
        status_label: Option<String>,
        workbook_name: Option<String>,
        source: DiagnosticArtifactRef,
        timestamp_unix_millis: i64,
        sequence: Option<u64>,
    ) -> Self {
        Self {
            label,
            kind_label,
            status_label,
            workbook_name,
            source,
            timestamp_unix_millis,
            sequence,
        }
    }

    /// 返回诊断列表的短标签。
    pub fn label(&self) -> &str {
        &self.label
    }

    /// 返回历史或日志的展示类型。
    pub fn kind_label(&self) -> &str {
        &self.kind_label
    }

    /// 返回历史记录状态；日志没有该状态。
    pub fn status_label(&self) -> Option<&str> {
        self.status_label.as_deref()
    }

    /// 返回历史所属工作簿；日志没有该名称。
    pub fn workbook_name(&self) -> Option<&str> {
        self.workbook_name.as_deref()
    }

    /// 返回打开前必须重新核对的文件身份。
    pub fn source(&self) -> &DiagnosticArtifactRef {
        &self.source
    }
}

/// 一次日志与历史读取的可用目录项及独立读取失败信息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GuiDiagnosticSnapshot {
    pub(crate) items: Vec<GuiDiagnosticItem>,
    pub(crate) warnings: Vec<suzushiro_task_runtime::TaskFailure>,
    /// 读取失败的来源保留上次可见项，其他来源使用本次结果。
    pub(crate) failed_sources: Vec<crate::application::DiagnosticArtifactKind>,
}

/// 一次实例目录刷新产生的 GUI 快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GuiSupportSnapshot {
    pub(super) instances: Vec<GuiInstanceItem>,
    pub(super) selected_instance: Option<String>,
}

impl GuiSupportSnapshot {
    pub(crate) fn selected_instance(&self) -> Option<&str> {
        self.selected_instance.as_deref().or_else(|| {
            self.instances
                .iter()
                .filter(|item| item.is_available())
                .min_by(|left, right| left.instance_id().cmp(right.instance_id()))
                .map(GuiInstanceItem::instance_id)
        })
    }

    /// 绑定实例候选和当前选择。
    pub(crate) fn new(instances: Vec<GuiInstanceItem>, selected_instance: Option<String>) -> Self {
        Self {
            instances,
            selected_instance,
        }
    }

    /// 返回具体实例候选数量。
    pub(crate) const fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// 窗口可以直接渲染、但不能自行改写的界面状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuiViewState {
    pub(super) agent_status: Option<String>,
    pub(super) preferences: Option<crate::application::UserPreferences>,
    pub(super) status: String,
    pub(super) ready_indicator: bool,
    pub(super) message: String,
    pub(super) progress: Option<suzushiro_task_runtime::TaskProgress>,
    pub(super) progress_finished: bool,
    pub(super) application_ready: bool,
    pub(super) workbooks: Vec<String>,
    pub(super) selected_workbook_index: Option<usize>,
    pub(super) instances: Vec<GuiInstanceItem>,
    pub(super) selected_instance_index: Option<usize>,
    pub(super) support_loaded: bool,
    pub(super) diagnostics: Vec<GuiDiagnosticItem>,
    pub(super) selected_diagnostic_index: Option<usize>,
    pub(super) generation_recheck_required: bool,
    pub(super) execution_check_required: bool,
    pub(super) checked_execution_workbook: Option<String>,
    pub(super) last_failure_detail: Option<String>,
}

impl GuiViewState {
    pub(super) fn new() -> Self {
        Self {
            preferences: None,
            agent_status: None,
            status: "准备检查".to_owned(),
            ready_indicator: false,
            message: "正在建立后台任务".to_owned(),
            progress: None,
            progress_finished: false,
            application_ready: false,
            workbooks: Vec::new(),
            selected_workbook_index: None,
            instances: Vec::new(),
            selected_instance_index: None,
            support_loaded: false,
            diagnostics: Vec::new(),
            selected_diagnostic_index: None,
            generation_recheck_required: false,
            execution_check_required: false,
            checked_execution_workbook: None,
            last_failure_detail: None,
        }
    }

    /// 返回当前实例最近一次代理状态查询的结果。
    pub fn agent_status(&self) -> Option<&str> {
        self.agent_status.as_deref()
    }

    /// 返回最近成功读取或保存的用户偏好。
    pub fn preferences(&self) -> Option<crate::application::UserPreferences> {
        self.preferences
    }

    /// 当前阶段的进度；阶段计数不代表整个任务的耗时比例。
    pub fn progress(&self) -> Option<&suzushiro_task_runtime::TaskProgress> {
        self.progress.as_ref()
    }

    /// 只有任务成功终态才将整个进度条置满。
    pub fn progress_percent(&self) -> Option<u8> {
        if self.progress_finished {
            Some(100)
        } else {
            self.progress
                .as_ref()
                .and_then(|progress| progress.percent())
        }
    }

    /// 返回窗口顶部的简短状态。
    pub fn status(&self) -> &str {
        &self.status
    }

    /// 初始化成功且当前没有运行中的操作时，状态点表示可以开始操作。
    pub const fn shows_ready_indicator(&self) -> bool {
        self.ready_indicator
    }

    /// 返回面向普通用户的当前操作说明。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 返回按文件名稳定排序的受控工作簿。
    pub fn workbooks(&self) -> &[String] {
        &self.workbooks
    }

    /// 返回当前选中工作簿的下标。
    pub const fn selected_workbook_index(&self) -> Option<usize> {
        self.selected_workbook_index
    }

    /// 返回当前选中工作簿的文件名。
    pub fn selected_workbook(&self) -> Option<&str> {
        self.selected_workbook_index
            .and_then(|index| self.workbooks.get(index))
            .map(String::as_str)
    }

    /// 返回当前实例下拉框的全部脱敏选项。
    pub fn instances(&self) -> &[GuiInstanceItem] {
        &self.instances
    }

    /// 返回当前实例选项下标。
    pub const fn selected_instance_index(&self) -> Option<usize> {
        self.selected_instance_index
    }

    /// 返回当前选中的具体实例索引。
    pub fn selected_instance(&self) -> Option<&str> {
        self.selected_instance_index
            .and_then(|index| self.instances.get(index))
            .map(GuiInstanceItem::instance_id)
    }

    /// 返回当前目录快照中所选实例是否已就绪。
    pub fn selected_instance_available(&self) -> bool {
        self.selected_instance_index
            .and_then(|index| self.instances.get(index))
            .is_some_and(GuiInstanceItem::is_available)
    }

    /// 返回已经安全读取的历史和日志元数据项。
    pub fn diagnostics(&self) -> &[GuiDiagnosticItem] {
        &self.diagnostics
    }

    /// 返回当前诊断选项下标。
    pub const fn selected_diagnostic_index(&self) -> Option<usize> {
        self.selected_diagnostic_index
    }

    /// 返回当前诊断项；该对象不包含文件正文。
    pub fn selected_diagnostic(&self) -> Option<&GuiDiagnosticItem> {
        self.selected_diagnostic_index
            .and_then(|index| self.diagnostics.get(index))
    }

    /// 返回最近一次普通失败的完整诊断文本；该文本不直接展示在主界面。
    pub fn last_failure_detail(&self) -> Option<&str> {
        self.last_failure_detail.as_deref()
    }

    /// 根据和应用启动校验相同的规则计算控件能力。
    fn controls_for(&self, running: bool) -> GuiControlsState {
        let idle = !running;
        GuiControlsState {
            refresh_enabled: idle,
            instance_selection_enabled: idle && self.application_ready && self.support_loaded,
            diagnostic_selection_enabled: idle
                && self.application_ready
                && !self.diagnostics.is_empty(),
            diagnostic_open_enabled: self.allow_diagnostic_open(running).is_ok(),
            failure_detail_enabled: idle && self.last_failure_detail.is_some(),
            workbook_selection_enabled: idle
                && self.application_ready
                && self.selected_workbook().is_some(),
            synchronize_enabled: self.allow_synchronize(running).is_ok(),
            check_enabled: self.allow_check(running).is_ok(),
            execute_enabled: self.allow_execute(running).is_ok(),
            open_enabled: self.allow_open(running).is_ok(),
        }
    }

    pub(super) fn allow_game_action(&self, active: bool) -> Result<(), super::GuiControllerError> {
        self.allow_ready(active)?;
        if self.support_loaded && self.selected_instance_available() {
            Ok(())
        } else {
            Err(super::GuiControllerError::InstanceUnavailable)
        }
    }

    pub(super) fn allow_synchronize(&self, active: bool) -> Result<(), super::GuiControllerError> {
        self.allow_game_action(active)?;
        if self.generation_recheck_required {
            Err(super::GuiControllerError::GenerationRecheckRequired)
        } else {
            Ok(())
        }
    }

    pub(super) fn allow_check(&self, active: bool) -> Result<&str, super::GuiControllerError> {
        self.allow_game_action(active)?;
        self.selected_workbook()
            .ok_or(super::GuiControllerError::WorkbookNotSelected)
    }

    pub(super) fn allow_execute(&self, active: bool) -> Result<&str, super::GuiControllerError> {
        let workbook_name = self.allow_check(active)?;
        if self.execution_check_required
            && self.checked_execution_workbook.as_deref() != Some(workbook_name)
        {
            Err(super::GuiControllerError::ExecutionCheckRequired)
        } else {
            Ok(workbook_name)
        }
    }

    pub(super) fn allow_open(&self, active: bool) -> Result<&str, super::GuiControllerError> {
        self.allow_ready(active)?;
        self.selected_workbook()
            .ok_or(super::GuiControllerError::WorkbookNotSelected)
    }

    pub(super) fn allow_diagnostic_open(
        &self,
        active: bool,
    ) -> Result<&DiagnosticArtifactRef, super::GuiControllerError> {
        self.allow_ready(active)?;
        self.selected_diagnostic()
            .map(GuiDiagnosticItem::source)
            .ok_or(super::GuiControllerError::DiagnosticNotSelected)
    }

    fn allow_ready(&self, active: bool) -> Result<(), super::GuiControllerError> {
        if active {
            Err(super::GuiControllerError::Busy)
        } else if !self.application_ready {
            Err(super::GuiControllerError::ApplicationNotReady)
        } else {
            Ok(())
        }
    }
}

/// 一次界面读取。运行标记来自控制器持有的任务句柄，不在展示模型里再存一份。
pub struct GuiView<'a> {
    state: &'a GuiViewState,
    running: bool,
}

impl<'a> std::ops::Deref for GuiView<'a> {
    type Target = GuiViewState;

    fn deref(&self) -> &Self::Target {
        self.state
    }
}

impl<'a> GuiView<'a> {
    pub(super) fn new(state: &'a GuiViewState, running: bool) -> Self {
        Self { state, running }
    }

    /// 返回当前是否有后台操作尚未收敛。
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// 返回当前任务互斥状态下的控件能力。
    pub fn controls(&self) -> GuiControlsState {
        self.state.controls_for(self.running)
    }
}

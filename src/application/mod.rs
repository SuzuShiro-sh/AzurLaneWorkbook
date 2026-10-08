//! 定义应用用例使用的端口和跨适配器稳定错误契约。

mod acquisition_update;
mod agent;
mod diagnostics;
mod emulator_instance;
mod execution;
mod history;
mod layout;
pub(crate) use layout::layout_content_sha256;
mod outcome;
mod plan;
mod query;
pub use query::{GameQuery, GameQueryKind, GameQueryOptions, GameQueryReport};
mod service;
pub use acquisition_update::{
    AcquisitionCacheUpdate, AcquisitionCacheUpdateOutcome, AcquisitionUpdateMode,
};
pub(crate) use acquisition_update::{ShipAcquisitionCachePort, update_ship_acquisition_cache};
pub(crate) use agent::AgentManagementPort;
pub use agent::{
    AgentManagementAction, AgentManagementOutcome, AgentManagementReport, AgentManagementService,
};
pub use outcome::{OperationTerminal, SessionCleanup, StageResult};
pub use service::{
    DirectAction, DirectActionBatch, DirectActionOutcome, DirectActionService,
    DirectEquipmentSource, DirectShipSlot,
};

mod settings;
#[cfg(test)]
pub(crate) mod test_support;
mod workbook;
pub(crate) use workbook::generation::check_generation_cancelled;
pub(crate) use workbook::progress::{OperationProgress, drive_progress};

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::domain::{DesiredState, EquipmentInventoryPlan, GameState};

pub(crate) use diagnostics::doctor::OfflineDoctorSources;
pub use diagnostics::doctor::{
    DOCTOR_SCHEMA_VERSION, DoctorCapabilities, DoctorCapability, DoctorCapabilityStatus,
    DoctorCheck, DoctorCheckStatus, DoctorChecks, DoctorLayoutCheck, DoctorReleaseCheck,
    DoctorReport, DoctorSettingsCheck, DoctorStatus, OfflineDoctor,
};
pub use diagnostics::log_catalog::{
    DirectoryEntryFailure, LOG_CATALOG_SCHEMA_VERSION, LOG_DIRECTORY, LogCatalogEntry,
    LogCatalogReport, LogRecordKind,
};
pub use diagnostics::open::{DiagnosticArtifactKind, DiagnosticArtifactRef};
pub use emulator_instance::{
    EMULATOR_INSTANCE_CATALOG_SCHEMA_VERSION, EmulatorInstanceCandidate,
    EmulatorInstanceCatalogReport, EmulatorInstanceState,
};
pub use execution::persistence::{
    ExecutionHistoryReport, ExecutionResultsWriteReport, WorkbookExecutionReport,
};
pub use execution::workbook::project_execution_report_rows;
pub use execution::{
    EXECUTION_SCHEMA_VERSION, ExecutionAction, ExecutionCancellation, ExecutionCommand,
    ExecutionEquipmentState, ExecutionFinalVerificationStatus, ExecutionReadbackEvidence,
    ExecutionReport, ExecutionReportStatus, ExecutionSlotReadback, ExecutionSlotState,
    ExecutionStateMismatch, ExecutionStatus, ExecutionStepResult, ExecutionStopReason,
    ExecutionTargetIdentity, ExecutionWarehouseReadback, ExecutionWriteEffect,
    NoExecutionCancellation,
};
#[cfg(any(target_os = "windows", test))]
pub(crate) use execution::{
    ExecutionCommandReceipt, ExecutionPort, ExecutionPreflight, ExecutionSendResult,
};
pub use history::catalog::{
    HISTORY_CATALOG_SCHEMA_VERSION, HISTORY_DIRECTORY, HistoryCatalogEntry, HistoryCatalogReport,
    HistoryExecutionStatus, HistoryRecordKind,
};
pub use history::check::{CHECK_HISTORY_SCHEMA_VERSION, CheckHistoryReport};
pub use layout::preview::LayoutPreviewReport;
pub use layout::upgrade::{LayoutUpgradeItemCounts, LayoutUpgradeReport};
pub use layout::{
    LAYOUT_SCHEMA_VERSION, LayoutColumnWidth, LayoutEditor, LayoutGenerationMode,
    LayoutHorizontalAlignment, LayoutModelError, LayoutValueFormat, LayoutVerticalAlignment,
    RegisteredLayoutEnumOption, RegisteredLayoutField, RegisteredLayoutSheet, WorkbookFieldLayout,
    WorkbookLayout, WorkbookLayoutEnumOption, WorkbookLayoutRegistry, WorkbookLayoutStyle,
    WorkbookSheetLayout,
};
pub(crate) use plan::workbook::project_check_result_rows;
pub use plan::{
    CheckReport, CompiledPlan, PLAN_SCHEMA_VERSION, PlanCheckError, PlanEnhanceCost,
    PlanEnhanceMaterialCost, PlanEquipment, PlanSlot, PlanSource, PlanStep, ResourceChange,
    ResourceConstraint, ResourceDelta, ResourceKey, compile_plan, compile_plan_with_inventory,
    compile_workbook_without_modifications, workbook_plan_has_modifications,
};
pub use service::{
    CheckAndSaveOutcome, LayoutCheckReport, LayoutService, WorkbookCheckService,
    WorkbookExecuteOutcome, WorkbookExecuteService, WorkbookSyncService, WorkspaceService,
    WorkspaceStartupReport,
};
pub use settings::{
    AcquisitionUpdatePolicy, SETTINGS_SUMMARY_SCHEMA_VERSION, SettingsSummaryReport,
    UserPreferences,
};
pub use workbook::backup::WorkbookBackupReport;
pub use workbook::catalog::{
    WORKBOOK_CATALOG_SCHEMA_VERSION, WORKBOOK_DIRECTORY, WorkbookCatalogEntry,
    WorkbookCatalogReport,
};
pub use workbook::generation::{
    AcquisitionGenerationOutcome, AcquisitionGenerationState, AcquisitionGenerationSummary,
    AcquisitionGenerationWarning, FullStateCaptureBinding, WorkbookGenerationOutcome,
    WorkbookGenerationReport,
};
pub use workbook::open::WorkbookOpenReport;
pub use workbook::projection::{
    WORKBOOK_PROJECTION_SCHEMA_VERSION, WorkbookProjectionError, WorkbookProjectionRow,
    WorkbookProjectionSheet, WorkbookProjectionSource, WorkbookProjectionV4,
    WorkbookProjectionValue,
};

/// 测试专用入口让跨适配器回归样本复用正式投影器；发布构建不暴露该调用面。
#[cfg(test)]
pub(crate) fn project_game_state_to_workbook(
    state: &GameState,
) -> Result<WorkbookProjectionV4, WorkbookProjectionError> {
    workbook::projection_mapper::project_game_state_to_workbook(state)
}

/// 应用服务对外暴露的稳定错误分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AppErrorCode {
    /// 操作在安全停止点响应用户取消，尚未发布结果。
    OperationCancelled,
    /// 用户设置或由设置派生的运行参数不符合冻结范围。
    SettingsInvalid,
    /// 工具根目录或程序内置契约未能完成应用组合。
    ApplicationInitializationFailed,
    /// 发布清单、文件闭包或发布配置未通过离线校验。
    ManifestInvalid,
    /// 没有找到符合设置且通过身份验证的模拟器安装或实例。
    EmulatorNotFound,
    /// 多个模拟器管理器安装同时通过验证，无法唯一选择管理器。
    EmulatorManagerAmbiguous,
    /// 多个模拟器候选同时满足发现条件，无法唯一选择目标实例。
    EmulatorInstanceAmbiguous,
    /// 工具持有的 ADB 无法连接或验证设置指定的唯一目标。
    AdbTargetUnavailable,
    /// 游戏进程或主线程数据尚未准备完成。
    GameNotReady,
    /// 设备端运行时、协议或当前客户端数据结构不兼容。
    RuntimeIncompatible,
    /// 设备端运行时连接或身份握手未能建立。
    RuntimeBootstrapFailed,
    /// 当前 agent 没有提供完整读取需要的能力。
    CapabilityMissing,
    /// 布局配置结构、稳定键或显示设置不符合当前契约。
    LayoutInvalid,
    /// 布局缺少当前程序已经注册的工作表、字段、枚举或样式。
    LayoutUpgradeRequired,
    /// 布局升级缺少显式迁移规则或未能安全发布新文件。
    LayoutMigrationFailed,
    /// 数据工作簿内容或生成产物不符合当前投影契约。
    WorkbookInvalid,
    /// 用户编辑的工作簿值不能构成合法的配装输入。
    InputInvalid,
    /// 工作簿正被其他程序占用，不能完成原子替换。
    WorkbookLocked,
    /// 系统默认程序未能打开受控工作簿。
    WorkbookOpenFailed,
    /// 原工作簿未能完整备份到工具备份目录。
    WorkbookBackupFailed,
    /// 历史记录未能安全发布到工具历史目录。
    HistoryWriteFailed,
    /// 历史记录目录未能安全读取或校验。
    HistoryReadFailed,
    /// 运行日志目录未能安全读取或校验。
    LogReadFailed,
    /// 执行结果未能在保留原工作簿的前提下完成原子写回。
    ExecutionResultsWriteFailed,
    /// 装备配置或其详情不存在。
    EquipmentNotFound,
    /// 装备配置、等级、数量或位置与完整读取依据不一致。
    EquipmentStateChanged,
    /// 装备目录或仓库存在重复、冲突记录。
    EquipmentConflict,
    /// 目标装备类型或舰种约束与舰船槽位不兼容。
    EquipmentIncompatible,
    /// 强化等级或强化链不符合领域范围。
    EnhanceInvalid,
    /// 输入虽然可读，但不能通过完整关联和不变量检查。
    FullCheckFailed,
}

impl AppErrorCode {
    /// 返回日志、CLI 和 GUI 可以稳定匹配的错误码。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OperationCancelled => "OPERATION_CANCELLED",
            Self::SettingsInvalid => "SETTINGS_INVALID",
            Self::ApplicationInitializationFailed => "APPLICATION_INITIALIZATION_FAILED",
            Self::ManifestInvalid => "MANIFEST_INVALID",
            Self::EmulatorNotFound => "EMULATOR_NOT_FOUND",
            Self::EmulatorManagerAmbiguous => "EMULATOR_MANAGER_AMBIGUOUS",
            Self::EmulatorInstanceAmbiguous => "EMULATOR_INSTANCE_AMBIGUOUS",
            Self::AdbTargetUnavailable => "ADB_TARGET_UNAVAILABLE",
            Self::GameNotReady => "GAME_NOT_READY",
            Self::RuntimeIncompatible => "RUNTIME_INCOMPATIBLE",
            Self::RuntimeBootstrapFailed => "RUNTIME_BOOTSTRAP_FAILED",
            Self::CapabilityMissing => "CAPABILITY_MISSING",
            Self::LayoutInvalid => "LAYOUT_INVALID",
            Self::LayoutUpgradeRequired => "LAYOUT_UPGRADE_REQUIRED",
            Self::LayoutMigrationFailed => "LAYOUT_MIGRATION_FAILED",
            Self::WorkbookInvalid => "WORKBOOK_INVALID",
            Self::InputInvalid => "INPUT_INVALID",
            Self::WorkbookLocked => "WORKBOOK_LOCKED",
            Self::WorkbookOpenFailed => "WORKBOOK_OPEN_FAILED",
            Self::WorkbookBackupFailed => "WORKBOOK_BACKUP_FAILED",
            Self::HistoryWriteFailed => "HISTORY_WRITE_FAILED",
            Self::HistoryReadFailed => "HISTORY_READ_FAILED",
            Self::LogReadFailed => "LOG_READ_FAILED",
            Self::ExecutionResultsWriteFailed => "EXECUTION_RESULTS_WRITE_FAILED",
            Self::EquipmentNotFound => "EQUIPMENT_NOT_FOUND",
            Self::EquipmentStateChanged => "EQUIPMENT_STATE_CHANGED",
            Self::EquipmentConflict => "EQUIPMENT_CONFLICT",
            Self::EquipmentIncompatible => "EQUIPMENT_INCOMPATIBLE",
            Self::EnhanceInvalid => "ENHANCE_INVALID",
            Self::FullCheckFailed => "FULL_CHECK_FAILED",
        }
    }
}

/// 由受控路径模块创建、只能指向工具目录内既有数据工作簿的引用。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkbookRef {
    relative_path: PathBuf,
}

impl WorkbookRef {
    /// 仅供路径边界模块和同 crate 测试建立已经校验过的相对路径。
    pub(crate) fn new(relative_path: PathBuf) -> Self {
        Self { relative_path }
    }

    /// 返回工具根目录内的规范相对路径。
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

impl Display for AppErrorCode {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 关闭所有者已经确定的会话清理事实。诊断键由这个类型写出。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CleanupFact {
    /// 正常卸载失败，恢复清理已经完成，不再持有会话。
    Recovered,
    /// 清理没有完成。仍持有资源时可以重试。
    Failed { owner_retained: bool },
}

/// 保留应用阶段、稳定分类、用户说明、对象上下文和完整底层原因链。
#[derive(Debug, Error)]
#[error("[{code}] {stage}: {message}")]
pub struct AppError {
    stage: &'static str,
    code: AppErrorCode,
    message: String,
    context: BTreeMap<String, String>,
    cleanup_fact: Option<CleanupFact>,
    #[source]
    source: Box<dyn Error + Send + Sync>,
}

impl AppError {
    /// 使用不可丢失的底层原因创建应用错误。
    pub fn from_source<E>(
        stage: &'static str,
        code: AppErrorCode,
        message: impl Into<String>,
        source: E,
    ) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            stage,
            code,
            message: message.into(),
            context: BTreeMap::new(),
            cleanup_fact: None,
            source: Box::new(source),
        }
    }

    /// 记录关闭所有者给出的清理事实，并写出对应诊断键。
    pub(crate) fn with_cleanup_fact(mut self, fact: CleanupFact) -> Self {
        self.cleanup_fact = Some(fact);
        match fact {
            CleanupFact::Recovered => self
                .with_context("cleanup", "recovered")
                .with_context("cleanup_owner_retained", "false")
                .with_context("cleanup_recovered", "true"),
            CleanupFact::Failed { owner_retained } => {
                self.with_context("cleanup", "failed").with_context(
                    "cleanup_owner_retained",
                    if owner_retained { "true" } else { "false" },
                )
            }
        }
    }

    /// 返回关闭所有者给出的清理事实。
    pub(crate) const fn cleanup_fact(&self) -> Option<CleanupFact> {
        self.cleanup_fact
    }

    /// 补充定位对象或底层稳定错误码，不把诊断信息拼进用户说明。
    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    /// 返回失败发生的稳定应用阶段。
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    /// 返回可供调用方分类处理的稳定错误码。
    pub const fn code(&self) -> AppErrorCode {
        self.code
    }

    /// 取消且会话清理没有失败时，入口可显示正常取消终态。
    pub fn is_cancelled(&self) -> bool {
        self.code == AppErrorCode::OperationCancelled
            && self.context.get("cleanup").map(String::as_str) != Some("failed")
    }

    /// 返回不依赖底层实现术语的中文说明。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 返回按键稳定排序的对象和底层诊断上下文。
    pub const fn context(&self) -> &BTreeMap<String, String> {
        &self.context
    }
}

/// 一次读取交还的共享游戏状态，以及同一次发布的捕获身份。
#[derive(Clone, Debug)]
pub(crate) struct GameObservation {
    state: Arc<GameState>,
    capture: Option<FullStateCaptureBinding>,
}

impl GameObservation {
    #[cfg(test)]
    pub(crate) fn from_state(state: GameState) -> Self {
        Self {
            state: Arc::new(state),
            capture: None,
        }
    }

    pub(crate) fn from_shared(
        state: Arc<GameState>,
        capture: Option<FullStateCaptureBinding>,
    ) -> Self {
        Self { state, capture }
    }

    pub(crate) fn state(&self) -> &GameState {
        &self.state
    }

    pub(crate) fn capture(&self) -> Option<&FullStateCaptureBinding> {
        self.capture.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn replace_state(&mut self, state: GameState) {
        self.state = Arc::new(state);
    }
}

impl Deref for GameObservation {
    type Target = GameState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

/// 应用层访问当前游戏状态的最小可替换边界。
pub(crate) trait GamePort {
    /// 标记下一次同步生成，使设备端口采用同步专属的会话结束设置。
    fn prepare_synchronization(&mut self) {}

    /// 读取并严格校验一次不可变完整状态。
    fn read_full_state(&mut self) -> Result<GameObservation, AppError>;

    /// 观察读取阶段，无内部计数的实现保持阶段等待状态。
    fn read_full_state_with_progress(
        &mut self,
        _progress: &mut dyn FnMut(OperationProgress),
    ) -> Result<GameObservation, AppError> {
        self.read_full_state()
    }

    /// 按范围读取。返回的状态与捕获身份来自同一次读取。
    fn read_state_with_scope(
        &mut self,
        _scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(OperationProgress),
    ) -> Result<GameObservation, AppError> {
        self.read_full_state_with_progress(progress)
    }

    /// 显式关闭当前游戏会话；无状态端口和已经关闭的端口保持幂等成功。
    fn shutdown_session(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

/// 统一应用调用点，界面和后续服务不直接依赖具体设备适配器。
#[cfg(test)]
pub(crate) fn read_full_state(port: &mut dyn GamePort) -> Result<GameObservation, AppError> {
    port.read_full_state()
}

/// 同一份数据工作簿快照解析出的布局、配装目标和库存处理。
#[derive(Debug)]
pub(crate) struct WorkbookPlanInputs {
    pub(crate) layout: WorkbookLayout,
    pub(crate) desired: DesiredState,
    pub(crate) inventory: EquipmentInventoryPlan,
    /// 解析计划时那一份工作簿字节的 SHA-256。
    pub(crate) source_package_sha256: String,
}

/// 应用层读取当前工作簿布局的最小可替换边界。
pub(crate) trait WorkbookPort {
    /// 读取当前根布局模板。数据工作簿里的布局快照由计划读取一并返回。
    fn load_layout(&self) -> Result<WorkbookLayout, AppError>;

    /// 从一份受控字节快照同时读取布局选择、配装目标和库存处理。
    fn load_plan_inputs(&self, _workbook: &WorkbookRef) -> Result<WorkbookPlanInputs, AppError> {
        Err(AppError::from_source(
            "workbook.plan.load",
            AppErrorCode::CapabilityMissing,
            "工作簿计划读取端口未配置",
            std::io::Error::other("workbook plan reader is not configured"),
        ))
    }
}

/// 应用层将布局和稳定投影物化为全新数据工作簿的独立写入边界。
pub(crate) trait WorkbookGenerationPort {
    /// 写出、重载并排他发布一份数据工作簿，不覆盖任何既有文件。
    fn generate_workbook(
        &self,
        requested_name: Option<&str>,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
    ) -> Result<WorkbookGenerationReport, AppError>;

    /// 报告生成阶段；无内部计数的端口保持不定进度。
    fn generate_workbook_with_progress(
        &self,
        requested_name: Option<&str>,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkbookGenerationReport, AppError> {
        progress(OperationProgress::stage("正在生成工作簿"));
        workbook::generation::check_generation_cancelled(is_cancelled)?;
        self.generate_workbook(requested_name, layout, projection)
    }

    /// 返回这次装配已经固定的获取方式开关。
    #[cfg(test)]
    fn ship_acquisition_enabled_for_test(&self) -> Result<bool, String> {
        Err("生成端口没有本次操作的用户偏好".to_owned())
    }
}

/// 应用层把受控工作簿交给系统默认程序的独立边界。
pub(crate) trait WorkbookOpenPort {
    /// 再次校验工作簿引用后启动默认程序，不修改工作簿内容。
    fn open_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookOpenReport, AppError>;
}

/// 应用层读取受控工作簿目录并选择不透明工作簿引用的独立边界。
pub(crate) trait WorkbookCatalogPort {
    /// 返回已经完成路径和普通文件校验的工作簿目录。
    fn list_workbooks(&self) -> Result<WorkbookCatalogReport, AppError>;

    /// 根据单个文件名建立受控工作簿引用，不接受目录跳转或外部路径。
    fn select_workbook(&self, workbook_name: &str) -> Result<WorkbookRef, AppError>;
}

/// 应用层将成功的配装检查保存为不可覆盖历史的独立边界。
pub(crate) trait CheckHistoryPort {
    /// 保存完整检查报告并返回已经重读核验的历史文件摘要。
    fn save_check_history(
        &self,
        workbook: &WorkbookRef,
        report: &CheckReport,
    ) -> Result<CheckHistoryReport, AppError>;
}

/// 应用层读取受控历史目录并校验每条记录外壳的独立边界。
pub(crate) trait HistoryCatalogPort {
    /// 返回按时间从新到旧排列的历史摘要。
    fn list_history(
        &self,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<HistoryCatalogReport, AppError>;
}

/// 应用层读取并严格校验运行设置、只返回脱敏摘要的独立边界。
pub(crate) trait SettingsPort {
    /// 返回不包含路径、设备地址和包名原文的设置摘要。
    fn read_settings(&self) -> Result<SettingsSummaryReport, AppError>;

    /// 返回当前已校验设置中的用户偏好。
    fn read_preferences(&self) -> Result<UserPreferences, AppError>;

    /// 校验编辑基线后原子保存用户偏好；偏好已变化时拒绝覆盖。
    fn save_preferences(
        &self,
        original: UserPreferences,
        preferences: UserPreferences,
    ) -> Result<(), AppError>;
}

/// 应用层读取受控日志目录并校验内容身份的独立边界。
pub(crate) trait LogCatalogPort {
    /// 返回不包含日志正文的稳定文件摘要。
    fn list_logs(&self, is_cancelled: &dyn Fn() -> bool) -> Result<LogCatalogReport, AppError>;
}

/// 应用层只读发现 模拟器 候选实例的独立边界。
pub(crate) trait EmulatorInstanceCatalogPort {
    /// 只调用管理器的版本和信息查询，不启动实例、ADB 或游戏。
    fn list_emulator_instances(&self) -> Result<EmulatorInstanceCatalogReport, AppError>;
}

/// 应用层打开已登记诊断原文件的独立边界。
pub(crate) trait DiagnosticOpenPort {
    /// 重新核对选中文件后使用系统默认程序打开，不生成副本。
    fn open_diagnostic(&self, source: &DiagnosticArtifactRef) -> Result<(), AppError>;
}

/// 应用层在发送游戏写命令前保存原工作簿的独立边界。
pub(crate) trait WorkbookBackupPort {
    /// 将当前工作簿字节不可覆盖地备份并重读核验。
    fn backup_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookBackupReport, AppError>;
}

/// 应用层保存完整执行审计的独立边界。
pub(crate) trait ExecutionHistoryPort {
    /// 将一份已经返回的执行报告排他发布为不可覆盖历史文件。
    fn save_execution_history(
        &self,
        workbook: &WorkbookRef,
        report: &ExecutionReport,
    ) -> Result<ExecutionHistoryReport, AppError>;
}

/// 应用层将检查结论和执行结果写回受控工作簿的独立边界。
pub(crate) trait ExecutionResultsPort {
    /// 将最近一次检查结论写入工作簿，保留其余工作表和用户输入。
    fn write_check_results(
        &self,
        _workbook: &WorkbookRef,
        _expected_source_package_sha256: &str,
        _layout: &WorkbookLayout,
        _result: Result<&CheckReport, &AppError>,
        _checked_at: Option<i64>,
    ) -> Result<(), AppError> {
        Err(AppError::from_source(
            "check.workbook.writeback",
            AppErrorCode::CapabilityMissing,
            "检查结果写回端口未配置",
            std::io::Error::other("check results writer is not configured"),
        ))
    }

    /// 在游戏写操作前检查当前工作簿能否承接快照写回，不修改文件。
    fn validate_writeback(
        &self,
        workbook: &WorkbookRef,
        expected_source_package_sha256: &str,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
    ) -> Result<(), AppError>;

    /// 只在工作簿仍与执行前备份摘要一致时原子写回执行结果工作表。
    fn write_execution_results(
        &self,
        workbook: &WorkbookRef,
        expected_source_package_sha256: &str,
        layout: &WorkbookLayout,
        rows: &[WorkbookProjectionRow],
        final_projection: Option<WorkbookProjectionV4>,
        recorded_at_unix_millis: i64,
    ) -> Result<ExecutionResultsWriteReport, AppError>;
}

/// 应用层执行非覆盖布局升级的独立写入边界。
pub(crate) trait LayoutUpgradePort {
    /// 读取根布局并排他发布一份经过严格重载的新布局。
    fn upgrade_layout(&self) -> Result<LayoutUpgradeReport, AppError>;
}

/// 应用层执行固定路径布局预览刷新的独立写入边界。
pub(crate) trait LayoutPreviewPort {
    /// 根据已经严格校验的布局写出、重载并原子刷新预览工作簿。
    fn preview_layout(&self, layout: &WorkbookLayout) -> Result<LayoutPreviewReport, AppError>;
}

/// 将计划器的纯业务错误转换为带稳定阶段和对象上下文的应用错误。
pub(crate) fn map_plan_check_error(error: PlanCheckError) -> AppError {
    let (code, context) = plan_check_error_context(&error);
    let mut application_error =
        AppError::from_source("plan.check", code, "配装计划检查未通过", error);
    for (key, value) in context {
        application_error = application_error.with_context(key, value);
    }
    application_error
}

fn plan_check_error_context(error: &PlanCheckError) -> (AppErrorCode, Vec<(&'static str, String)>) {
    match error {
        PlanCheckError::ShipNotFound { ship_instance_id } => (
            AppErrorCode::EquipmentNotFound,
            vec![("ship_instance_id", ship_instance_id.get().to_string())],
        ),
        PlanCheckError::EquipmentFamilyNotFound { family_id } => (
            AppErrorCode::EquipmentNotFound,
            vec![("family_id", family_id.get().to_string())],
        ),
        PlanCheckError::EquipmentConfigNotFound { config_id } => (
            AppErrorCode::EquipmentNotFound,
            vec![("config_id", config_id.get().to_string())],
        ),
        PlanCheckError::EquipmentIncompatible {
            slot,
            config_id,
            equipment_type_id,
            ship_type_id,
            allowed_equipment_type_ids,
            forbidden_ship_type_ids,
        } => (
            AppErrorCode::EquipmentIncompatible,
            vec![
                ("slot", slot.to_string()),
                ("config_id", config_id.get().to_string()),
                ("equipment_type_id", equipment_type_id.to_string()),
                ("ship_type_id", ship_type_id.to_string()),
                (
                    "allowed_equipment_type_ids",
                    allowed_equipment_type_ids
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                (
                    "forbidden_ship_type_ids",
                    forbidden_ship_type_ids
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ],
        ),
        PlanCheckError::ExactSourceMissing => (AppErrorCode::FullCheckFailed, Vec::new()),
        PlanCheckError::SourceNotFound { source_ref } => (
            AppErrorCode::EquipmentStateChanged,
            vec![("source", source_ref.to_string())],
        ),
        PlanCheckError::SourceFamilyMismatch {
            source_ref,
            actual_family_id,
            expected_family_id,
        } => (
            AppErrorCode::EquipmentConflict,
            vec![
                ("source", source_ref.to_string()),
                ("actual_family_id", actual_family_id.get().to_string()),
                ("expected_family_id", expected_family_id.get().to_string()),
            ],
        ),
        PlanCheckError::SourceUnavailable {
            source_ref,
            available,
            required,
        } => (
            AppErrorCode::EquipmentStateChanged,
            vec![
                ("source", source_ref.to_string()),
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::InventorySourceNotFound { source_ref } => (
            AppErrorCode::EquipmentStateChanged,
            vec![("source", source_ref.to_string())],
        ),
        PlanCheckError::InventorySourceUnavailable {
            source_ref,
            available,
            required,
        } => (
            AppErrorCode::EquipmentStateChanged,
            vec![
                ("source", source_ref.to_string()),
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::InventorySourceConflict { source_ref } => (
            AppErrorCode::EquipmentConflict,
            vec![("source", source_ref.to_string())],
        ),
        PlanCheckError::InventoryDismantleProtected {
            source_ref,
            important,
            protected_variant,
            rarity_confirmation_required,
            enhanced,
        } => (
            AppErrorCode::InputInvalid,
            vec![
                ("source", source_ref.to_string()),
                ("important", important.to_string()),
                ("protected_variant", protected_variant.to_string()),
                (
                    "rarity_confirmation_required",
                    rarity_confirmation_required.to_string(),
                ),
                ("enhanced", enhanced.to_string()),
            ],
        ),
        PlanCheckError::TemporaryEquipmentCapacityUnavailable {
            available,
            required,
        } => (
            AppErrorCode::EquipmentStateChanged,
            vec![
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::ComposeEquipmentCapacityUnavailable {
            available,
            required,
        } => (
            AppErrorCode::FullCheckFailed,
            vec![
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::EquipmentCapacitySnapshotMismatch {
            available,
            required,
        } => (
            AppErrorCode::FullCheckFailed,
            vec![
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::NoWarehouseSource { family_id } => (
            AppErrorCode::EquipmentStateChanged,
            vec![("family_id", family_id.get().to_string())],
        ),
        PlanCheckError::ComposeRecipeNotFound { family_id } => (
            AppErrorCode::EquipmentNotFound,
            vec![("family_id", family_id.get().to_string())],
        ),
        PlanCheckError::ComposeRecipeUnavailable { recipe_id } => (
            AppErrorCode::EquipmentStateChanged,
            vec![("recipe_id", recipe_id.to_string())],
        ),
        PlanCheckError::ComposeRecipeMismatch { recipe_id } => (
            AppErrorCode::FullCheckFailed,
            vec![("recipe_id", recipe_id.to_string())],
        ),
        PlanCheckError::ComposeResourceUnavailable {
            recipe_id,
            key,
            available,
            required,
        } => (
            AppErrorCode::FullCheckFailed,
            vec![
                ("recipe_id", recipe_id.to_string()),
                ("resource", format!("{key:?}")),
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::ComposeTargetReservationMismatch {
            recipe_id,
            reserved,
            targets,
        } => (
            AppErrorCode::FullCheckFailed,
            vec![
                ("recipe_id", recipe_id.to_string()),
                ("reserved", reserved.to_string()),
                ("targets", targets.to_string()),
            ],
        ),
        PlanCheckError::SourceTargetConflict { source_ref } => (
            AppErrorCode::EquipmentConflict,
            vec![("source_slot", source_ref.to_string())],
        ),
        PlanCheckError::SourceEqualsTarget { slot } => (
            AppErrorCode::EquipmentConflict,
            vec![("slot", slot.to_string())],
        ),
        PlanCheckError::TargetEnhanceLevelUnavailable {
            family_id,
            target_level,
        } => (
            AppErrorCode::EnhanceInvalid,
            vec![
                ("family_id", family_id.get().to_string()),
                ("target_level", target_level.to_string()),
            ],
        ),
        PlanCheckError::EnhanceDowngrade {
            source_level,
            target_level,
        } => (
            AppErrorCode::EnhanceInvalid,
            vec![
                ("source_level", source_level.to_string()),
                ("target_level", target_level.to_string()),
            ],
        ),
        PlanCheckError::EnhanceChainMismatch {
            source_config_id,
            target_config_id,
        } => (
            AppErrorCode::EnhanceInvalid,
            vec![
                ("source_config_id", source_config_id.to_string()),
                ("target_config_id", target_config_id.to_string()),
            ],
        ),
        PlanCheckError::EnhanceResourceUnavailable {
            source_config_id,
            key,
            available,
            required,
        } => (
            AppErrorCode::FullCheckFailed,
            vec![
                ("source_config_id", source_config_id.to_string()),
                ("resource", format!("{key:?}")),
                ("available", available.to_string()),
                ("required", required.to_string()),
            ],
        ),
        PlanCheckError::QuantityOverflow { quantity } => (
            AppErrorCode::FullCheckFailed,
            vec![("quantity", quantity.to_string())],
        ),
        PlanCheckError::ResourceChangeOverflow { key } => (
            AppErrorCode::FullCheckFailed,
            vec![("resource", format!("{key:?}"))],
        ),
        PlanCheckError::StepSequenceOverflow => (AppErrorCode::FullCheckFailed, Vec::new()),
        PlanCheckError::DigestEncoding { .. } => (AppErrorCode::FullCheckFailed, Vec::new()),
    }
}

pub(crate) use workbook::technology::{TECHNOLOGY_FIELDS, ship_technology_summaries};

pub(crate) use workbook::technology::{
    TECHNOLOGY_VIEW_FIELDS, TECHNOLOGY_VIEW_KEY, excel_sheet_tab_name_is_legal,
    technology_bonus_summary, technology_category_layout, technology_template_key,
};

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::io;

    use super::{
        AppError, AppErrorCode, GamePort, PlanCheckError, map_plan_check_error, read_full_state,
    };
    use crate::domain::{
        EquipmentConfigId, EquipmentSourceRef, ShipInstanceId, ShipSlotRef, SlotIndex,
    };

    struct FailingGamePort {
        calls: usize,
    }

    impl GamePort for FailingGamePort {
        fn read_full_state(&mut self) -> Result<super::GameObservation, AppError> {
            self.calls += 1;
            Err(AppError::from_source(
                "game.read.fixture",
                AppErrorCode::FullCheckFailed,
                "测试状态未通过完整检查",
                io::Error::other("fixture failure"),
            )
            .with_context("ship_id", "9001"))
        }
    }

    #[test]
    fn delegates_to_game_port_without_losing_error_contract() {
        let mut port = FailingGamePort { calls: 0 };

        let error = read_full_state(&mut port).unwrap_err();

        assert_eq!(port.calls, 1);
        assert_eq!(error.stage(), "game.read.fixture");
        assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
        assert_eq!(error.code().as_str(), "FULL_CHECK_FAILED");
        assert_eq!(error.message(), "测试状态未通过完整检查");
        assert_eq!(error.context().get("ship_id").unwrap(), "9001");
        assert_eq!(
            error.to_string(),
            "[FULL_CHECK_FAILED] game.read.fixture: 测试状态未通过完整检查"
        );
        assert_eq!(error.source().unwrap().to_string(), "fixture failure");
    }

    #[test]
    fn exposes_a_stable_string_for_every_application_error_code() {
        let cases = [
            (AppErrorCode::SettingsInvalid, "SETTINGS_INVALID"),
            (
                AppErrorCode::ApplicationInitializationFailed,
                "APPLICATION_INITIALIZATION_FAILED",
            ),
            (AppErrorCode::ManifestInvalid, "MANIFEST_INVALID"),
            (AppErrorCode::EmulatorNotFound, "EMULATOR_NOT_FOUND"),
            (
                AppErrorCode::EmulatorManagerAmbiguous,
                "EMULATOR_MANAGER_AMBIGUOUS",
            ),
            (
                AppErrorCode::EmulatorInstanceAmbiguous,
                "EMULATOR_INSTANCE_AMBIGUOUS",
            ),
            (AppErrorCode::AdbTargetUnavailable, "ADB_TARGET_UNAVAILABLE"),
            (AppErrorCode::GameNotReady, "GAME_NOT_READY"),
            (AppErrorCode::RuntimeIncompatible, "RUNTIME_INCOMPATIBLE"),
            (
                AppErrorCode::RuntimeBootstrapFailed,
                "RUNTIME_BOOTSTRAP_FAILED",
            ),
            (AppErrorCode::CapabilityMissing, "CAPABILITY_MISSING"),
            (AppErrorCode::LayoutInvalid, "LAYOUT_INVALID"),
            (
                AppErrorCode::LayoutUpgradeRequired,
                "LAYOUT_UPGRADE_REQUIRED",
            ),
            (
                AppErrorCode::LayoutMigrationFailed,
                "LAYOUT_MIGRATION_FAILED",
            ),
            (AppErrorCode::WorkbookInvalid, "WORKBOOK_INVALID"),
            (AppErrorCode::InputInvalid, "INPUT_INVALID"),
            (AppErrorCode::WorkbookLocked, "WORKBOOK_LOCKED"),
            (AppErrorCode::WorkbookOpenFailed, "WORKBOOK_OPEN_FAILED"),
            (AppErrorCode::WorkbookBackupFailed, "WORKBOOK_BACKUP_FAILED"),
            (AppErrorCode::HistoryWriteFailed, "HISTORY_WRITE_FAILED"),
            (AppErrorCode::HistoryReadFailed, "HISTORY_READ_FAILED"),
            (AppErrorCode::LogReadFailed, "LOG_READ_FAILED"),
            (
                AppErrorCode::ExecutionResultsWriteFailed,
                "EXECUTION_RESULTS_WRITE_FAILED",
            ),
            (AppErrorCode::EquipmentNotFound, "EQUIPMENT_NOT_FOUND"),
            (
                AppErrorCode::EquipmentStateChanged,
                "EQUIPMENT_STATE_CHANGED",
            ),
            (AppErrorCode::EquipmentConflict, "EQUIPMENT_CONFLICT"),
            (
                AppErrorCode::EquipmentIncompatible,
                "EQUIPMENT_INCOMPATIBLE",
            ),
            (AppErrorCode::EnhanceInvalid, "ENHANCE_INVALID"),
            (AppErrorCode::FullCheckFailed, "FULL_CHECK_FAILED"),
        ];

        for (code, expected) in cases {
            assert_eq!(code.as_str(), expected);
            assert_eq!(code.to_string(), expected);
        }
    }

    #[test]
    fn maps_protected_dismantle_errors_with_source_context() {
        let source = EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap());
        let error = map_plan_check_error(PlanCheckError::InventoryDismantleProtected {
            source_ref: source,
            important: false,
            protected_variant: true,
            rarity_confirmation_required: false,
            enhanced: true,
        });

        assert_eq!(error.code(), AppErrorCode::InputInvalid);
        assert_eq!(error.stage(), "plan.check");
        assert_eq!(
            error.context().get("source").map(String::as_str),
            Some("仓库配置 1001")
        );
        assert_eq!(
            error.context().get("important").map(String::as_str),
            Some("false")
        );
        assert_eq!(
            error.context().get("protected_variant").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            error
                .context()
                .get("rarity_confirmation_required")
                .map(String::as_str),
            Some("false")
        );
        assert_eq!(
            error.context().get("enhanced").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn maps_temporary_equipment_capacity_with_required_context() {
        let error = map_plan_check_error(PlanCheckError::TemporaryEquipmentCapacityUnavailable {
            available: 1,
            required: 2,
        });

        assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
        assert_eq!(error.stage(), "plan.check");
        assert_eq!(
            error.context().get("available").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            error.context().get("required").map(String::as_str),
            Some("2")
        );
    }

    #[test]
    fn maps_equipment_incompatibility_with_slot_and_rule_context() {
        let error = map_plan_check_error(PlanCheckError::EquipmentIncompatible {
            slot: ShipSlotRef::new(
                ShipInstanceId::new(9001).unwrap(),
                SlotIndex::new(2).unwrap(),
            ),
            config_id: EquipmentConfigId::new(1001).unwrap(),
            equipment_type_id: 1,
            ship_type_id: 2,
            allowed_equipment_type_ids: vec![2, 3],
            forbidden_ship_type_ids: vec![2, 8],
        });

        assert_eq!(error.code(), AppErrorCode::EquipmentIncompatible);
        assert_eq!(error.stage(), "plan.check");
        assert_eq!(
            error.context().get("slot").map(String::as_str),
            Some("9001:2")
        );
        assert_eq!(
            error.context().get("config_id").map(String::as_str),
            Some("1001")
        );
        assert_eq!(
            error.context().get("equipment_type_id").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            error.context().get("ship_type_id").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            error
                .context()
                .get("allowed_equipment_type_ids")
                .map(String::as_str),
            Some("2,3")
        );
        assert_eq!(
            error
                .context()
                .get("forbidden_ship_type_ids")
                .map(String::as_str),
            Some("2,8")
        );
    }

    #[test]
    fn maps_compose_capacity_as_a_plan_feasibility_failure() {
        let error = map_plan_check_error(PlanCheckError::ComposeEquipmentCapacityUnavailable {
            available: 0,
            required: 1,
        });

        assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
        assert_eq!(error.stage(), "plan.check");
        assert_eq!(
            error.context().get("available").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            error.context().get("required").map(String::as_str),
            Some("1")
        );
    }
}

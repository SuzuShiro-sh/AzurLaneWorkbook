//! 受控工作区工件查询、打开和启动支持查询。

use crate::application::{
    AppError, AppErrorCode, DiagnosticArtifactRef, DiagnosticOpenPort, EmulatorInstanceCatalogPort,
    EmulatorInstanceCatalogReport, HistoryCatalogPort, HistoryCatalogReport, LogCatalogPort,
    LogCatalogReport, SettingsPort, SettingsSummaryReport, WorkbookCatalogPort,
    WorkbookCatalogReport, WorkbookOpenPort, WorkbookOpenReport, WorkbookRef,
};

/// 一次界面初始化读到的工作簿目录和实例目录。工作簿失败则整次失败；实例失败保留空目录。
pub struct WorkspaceStartupReport {
    workbook_names: Vec<String>,
    instances: EmulatorInstanceCatalogReport,
    instance_catalog_error: Option<AppError>,
}

impl WorkspaceStartupReport {
    pub fn workbook_names(&self) -> &[String] {
        &self.workbook_names
    }

    pub const fn workbook_count(&self) -> usize {
        self.workbook_names.len()
    }

    pub fn instances(&self) -> &EmulatorInstanceCatalogReport {
        &self.instances
    }

    pub fn instance_catalog_error(&self) -> Option<&AppError> {
        self.instance_catalog_error.as_ref()
    }

    pub fn into_parts(self) -> (Vec<String>, EmulatorInstanceCatalogReport, Option<AppError>) {
        (
            self.workbook_names,
            self.instances,
            self.instance_catalog_error,
        )
    }
}

fn missing_emulator_instance_catalog_port_error() -> AppError {
    AppError::from_source(
        "mumu.instances.catalog",
        AppErrorCode::CapabilityMissing,
        "当前应用未提供 模拟器 实例目录读取能力",
        std::io::Error::other("MuMu instance catalog port is not configured"),
    )
    .with_context("missing_port", "emulator_instance_catalog")
}

/// 查询和打开工作区工件，不接触游戏会话或布局注册表。
pub struct WorkspaceService {
    workbook_open: Box<dyn WorkbookOpenPort>,
    workbook_catalog: Box<dyn WorkbookCatalogPort>,
    history_catalog: Box<dyn HistoryCatalogPort>,
    settings: Box<dyn SettingsPort>,
    log_catalog: Box<dyn LogCatalogPort>,
    diagnostic_open: Box<dyn DiagnosticOpenPort>,
    emulator_instance_catalog: Option<Box<dyn EmulatorInstanceCatalogPort>>,
}

impl WorkspaceService {
    /// 一次接收工作区查询所需的七项端口；非 Windows 的实例目录为 None。
    pub(crate) fn with_ports(
        workbook_open: Box<dyn WorkbookOpenPort>,
        workbook_catalog: Box<dyn WorkbookCatalogPort>,
        history_catalog: Box<dyn HistoryCatalogPort>,
        settings: Box<dyn SettingsPort>,
        log_catalog: Box<dyn LogCatalogPort>,
        diagnostic_open: Box<dyn DiagnosticOpenPort>,
        emulator_instance_catalog: Option<Box<dyn EmulatorInstanceCatalogPort>>,
    ) -> Self {
        Self {
            workbook_open,
            workbook_catalog,
            history_catalog,
            settings,
            log_catalog,
            diagnostic_open,
            emulator_instance_catalog,
        }
    }

    /// 读取受控目录中的全部普通 XLSX 工作簿，供 CLI 和 GUI 共用。
    pub fn list_workbooks(&self) -> Result<WorkbookCatalogReport, AppError> {
        self.workbook_catalog.list_workbooks()
    }

    /// 通过应用端口选择一份工作簿，调用方不直接接触文件系统路径。
    pub fn select_workbook(&self, workbook_name: &str) -> Result<WorkbookRef, AppError> {
        self.workbook_catalog.select_workbook(workbook_name)
    }

    /// 把受控工作簿交给系统默认程序，不读取游戏状态也不修改工作簿。
    pub fn open_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookOpenReport, AppError> {
        self.workbook_open.open_workbook(workbook)
    }

    /// 读取并校验工具根目录内全部历史记录，供 CLI 和 GUI 共用。
    pub fn list_history(
        &self,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<HistoryCatalogReport, AppError> {
        self.history_catalog.list_history(is_cancelled)
    }

    /// 严格读取运行设置并返回不包含敏感原文的摘要。
    pub fn read_settings(&self) -> Result<SettingsSummaryReport, AppError> {
        self.settings.read_settings()
    }

    /// 读取当前已校验设置中的用户偏好。
    pub fn read_preferences(&self) -> Result<crate::application::UserPreferences, AppError> {
        self.settings.read_preferences()
    }

    /// 校验编辑基线后原子保存用户偏好，保留设备与运行参数。
    pub fn save_preferences(
        &self,
        original: crate::application::UserPreferences,
        preferences: crate::application::UserPreferences,
    ) -> Result<(), AppError> {
        self.settings.save_preferences(original, preferences)
    }

    /// 读取受控日志目录并只返回文件元数据，不向调用方暴露日志正文。
    pub fn list_logs(&self, is_cancelled: &dyn Fn() -> bool) -> Result<LogCatalogReport, AppError> {
        self.log_catalog.list_logs(is_cancelled)
    }

    /// 打开当前选中的受控诊断文件，不生成副本。
    pub fn open_diagnostic(&self, source: &DiagnosticArtifactRef) -> Result<(), AppError> {
        self.diagnostic_open.open_diagnostic(source)
    }

    /// 同一次初始化读取工作簿目录和实例目录。实例目录失败不撤销已经读到的工作簿。
    pub fn load_startup(&self) -> Result<WorkspaceStartupReport, AppError> {
        let catalog = self.list_workbooks()?;
        let (instances, instance_catalog_error) = match self.list_emulator_instances() {
            Ok(report) => (report, None),
            Err(error) => (
                EmulatorInstanceCatalogReport::new(None, Vec::new()),
                Some(error),
            ),
        };
        Ok(WorkspaceStartupReport {
            workbook_names: catalog
                .workbooks()
                .iter()
                .map(|workbook| workbook.name().to_owned())
                .collect(),
            instances,
            instance_catalog_error,
        })
    }

    /// 只读刷新 模拟器 实例候选，不启动实例、ADB 或游戏。
    pub fn list_emulator_instances(&self) -> Result<EmulatorInstanceCatalogReport, AppError> {
        self.emulator_instance_catalog
            .as_deref()
            .ok_or_else(missing_emulator_instance_catalog_port_error)?
            .list_emulator_instances()
    }
}

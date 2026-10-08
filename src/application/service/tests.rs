//! 覆盖应用服务门面、端口组合和清理语义的单元测试。

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{LayoutCheckReport, WorkbookCheckService, WorkbookExecuteService, WorkbookSyncService};
use crate::application::execution::{
    ExecutionCommandReceipt, ExecutionPort, ExecutionPreflight, ExecutionSendResult,
};
use crate::application::test_support::{
    Events, FakeGamePort, FakeWorkbookGenerationPort, FakeWorkbookPort, empty_game_state,
    empty_layout, generation_report, plan_game_state,
};
use crate::application::{
    AppError, AppErrorCode, CheckHistoryPort, CheckHistoryReport, CheckReport, DiagnosticOpenPort,
    EmulatorInstanceCatalogPort, EmulatorInstanceCatalogReport, ExecutionCancellation,
    ExecutionCommand, ExecutionHistoryPort, ExecutionHistoryReport, ExecutionReport,
    ExecutionReportStatus, ExecutionResultsPort, ExecutionResultsWriteReport, ExecutionStatus,
    ExecutionTargetIdentity, GamePort, HistoryCatalogPort, HistoryCatalogReport,
    LAYOUT_SCHEMA_VERSION, LayoutPreviewPort, LayoutPreviewReport, LayoutUpgradeItemCounts,
    LayoutUpgradePort, LayoutUpgradeReport, LogCatalogPort, LogCatalogReport, SettingsPort,
    SettingsSummaryReport, WORKBOOK_PROJECTION_SCHEMA_VERSION, WorkbookBackupPort,
    WorkbookBackupReport, WorkbookCatalogEntry, WorkbookCatalogPort, WorkbookCatalogReport,
    WorkbookExecuteOutcome, WorkbookExecutionReport, WorkbookGenerationPort,
    WorkbookGenerationReport, WorkbookLayout, WorkbookOpenPort, WorkbookOpenReport, WorkbookPort,
    WorkbookProjectionRow, WorkbookProjectionV4, WorkbookRef,
};
use crate::domain::{DesiredState, EquipmentInventoryPlan, GameState, GameStateSource};

struct StubWorkbookPort {
    layout: WorkbookLayout,
    calls: Arc<AtomicUsize>,
}

struct FailingWorkbookPort {
    events: Events,
}

/// 模拟已经建立的游戏会话，单独验证显式关闭、访问分类和双重失败契约。
struct ShutdownTrackingGamePort {
    state: Option<GameState>,
    read_error: Option<AppError>,
    shutdown_error: Option<AppError>,
    active: bool,
    events: Events,
    seen_scope: Arc<std::sync::Mutex<Option<crate::domain::GameReadScope>>>,
}

impl ShutdownTrackingGamePort {
    fn new(
        state: Option<GameState>,
        read_error: Option<AppError>,
        shutdown_error: Option<AppError>,
        events: Events,
    ) -> Self {
        Self {
            state,
            read_error,
            shutdown_error,
            active: false,
            events,
            seen_scope: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn with_scope_slot(
        mut self,
        seen_scope: Arc<std::sync::Mutex<Option<crate::domain::GameReadScope>>>,
    ) -> Self {
        self.seen_scope = seen_scope;
        self
    }
}

/// 模拟会话建立前失败：读取返回探针错误，但显式关闭没有任何可重试所有权。
struct FailedOpenGamePort {
    error: Option<AppError>,
    events: Events,
}

fn fixture_shutdown_error() -> AppError {
    AppError::from_source(
        "game.cleanup.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试游戏会话清理失败",
        std::io::Error::other("fixture game cleanup failure"),
    )
    .with_context("runtime_code", "fixture_cleanup_failed")
}

impl GamePort for ShutdownTrackingGamePort {
    fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
        self.events.lock().unwrap().push("game");
        self.active = true;
        if let Some(error) = self.read_error.take() {
            return Err(error);
        }
        Ok(crate::application::GameObservation::from_state(
            self.state.take().expect("成功读取夹具必须持有完整状态"),
        ))
    }

    fn read_full_state_with_progress(
        &mut self,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<crate::application::GameObservation, AppError> {
        progress(crate::application::OperationProgress::counted(
            "正在读取舰船详情",
            2,
            5,
        ));
        self.read_full_state()
    }

    fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<crate::application::GameObservation, AppError> {
        *self.seen_scope.lock().unwrap() = Some(scope);
        self.read_full_state_with_progress(progress)
    }

    fn shutdown_session(&mut self) -> Result<(), AppError> {
        if !self.active {
            return Ok(());
        }
        self.events.lock().unwrap().push("shutdown");
        if let Some(error) = self.shutdown_error.take() {
            return Err(error);
        }
        self.active = false;
        Ok(())
    }
}

impl GamePort for FailedOpenGamePort {
    fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
        self.events.lock().unwrap().push("game");
        Err(self.error.take().expect("启动失败夹具只能读取一次"))
    }

    fn shutdown_session(&mut self) -> Result<(), AppError> {
        self.events.lock().unwrap().push("shutdown_noop");
        Ok(())
    }
}

impl ExecutionPort for ShutdownTrackingGamePort {
    fn bind_current_session(&mut self) -> Result<(), AppError> {
        panic!("当前清理测试不应绑定执行会话")
    }

    fn target_identity(&mut self, _state: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
        panic!("当前清理测试不应读取执行目标身份")
    }

    fn preflight_plan(&mut self, _preflight: &ExecutionPreflight) -> Result<(), AppError> {
        panic!("当前清理测试不应执行写入预检")
    }

    fn send_command(&mut self, _command: &ExecutionCommand) -> ExecutionSendResult {
        panic!("当前清理测试不应发送命令")
    }

    fn query_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("当前清理测试不应查询命令")
    }

    fn cancel_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("当前清理测试不应取消命令")
    }
}

impl WorkbookPort for FailingWorkbookPort {
    fn load_layout(&self) -> Result<WorkbookLayout, AppError> {
        self.events.lock().unwrap().push("layout");
        Err(AppError::from_source(
            "workbook.layout.fixture",
            crate::application::AppErrorCode::LayoutInvalid,
            "测试布局无效",
            std::io::Error::other("fixture layout failure"),
        ))
    }
}

impl WorkbookPort for StubWorkbookPort {
    fn load_layout(&self) -> Result<WorkbookLayout, crate::application::AppError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self.layout.clone())
    }
}

struct StubDesiredWorkbookPort {
    desired: DesiredState,
    inventory_plan: EquipmentInventoryPlan,
    events: Events,
}

fn workbook_with_modification(events: Events) -> StubDesiredWorkbookPort {
    StubDesiredWorkbookPort {
        desired: warehouse_equip_desired_state(),
        inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
        events,
    }
}

impl WorkbookPort for StubDesiredWorkbookPort {
    fn load_layout(&self) -> Result<WorkbookLayout, crate::application::AppError> {
        self.events.lock().unwrap().push("layout");
        Ok(empty_layout())
    }

    fn load_plan_inputs(
        &self,
        _workbook: &WorkbookRef,
    ) -> Result<crate::application::WorkbookPlanInputs, crate::application::AppError> {
        self.events.lock().unwrap().push("plan");
        Ok(crate::application::WorkbookPlanInputs {
            layout: empty_layout(),
            desired: self.desired.clone(),
            inventory: self.inventory_plan.clone(),
            source_package_sha256: "a".repeat(64),
        })
    }
}

struct StubLayoutUpgradePort;

struct StubHistoryCatalogPort {
    report: HistoryCatalogReport,
}

impl HistoryCatalogPort for StubHistoryCatalogPort {
    fn list_history(
        &self,
        _is_cancelled: &dyn Fn() -> bool,
    ) -> Result<HistoryCatalogReport, AppError> {
        Ok(self.report.clone())
    }
}

struct StubSettingsPort {
    report: SettingsSummaryReport,
}

impl SettingsPort for StubSettingsPort {
    fn read_settings(&self) -> Result<SettingsSummaryReport, AppError> {
        Ok(self.report.clone())
    }

    fn read_preferences(&self) -> Result<crate::application::UserPreferences, AppError> {
        Ok(crate::application::UserPreferences::default())
    }

    fn save_preferences(
        &self,
        _original: crate::application::UserPreferences,
        _preferences: crate::application::UserPreferences,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

struct StubLogCatalogPort {
    report: LogCatalogReport,
}

impl LogCatalogPort for StubLogCatalogPort {
    fn list_logs(&self, _is_cancelled: &dyn Fn() -> bool) -> Result<LogCatalogReport, AppError> {
        Ok(self.report.clone())
    }
}

impl LayoutUpgradePort for StubLayoutUpgradePort {
    fn upgrade_layout(&self) -> Result<LayoutUpgradeReport, AppError> {
        Ok(LayoutUpgradeReport::new(
            "workbook-layout.xlsx".to_owned(),
            "data/workbooks/workbook-layout.updated.xlsx".to_owned(),
            LAYOUT_SCHEMA_VERSION,
            LAYOUT_SCHEMA_VERSION,
            LayoutUpgradeItemCounts::new(1, 2, 3, 4),
            LayoutUpgradeItemCounts::default(),
            Vec::new(),
            "1".repeat(64),
            "2".repeat(64),
            "3".repeat(64),
        ))
    }
}

struct StubLayoutPreviewPort {
    calls: Arc<AtomicUsize>,
}

struct StubWorkbookBackupPort {
    calls: Arc<AtomicUsize>,
}

struct StubWorkbookOpenPort {
    calls: Arc<AtomicUsize>,
}

struct StubWorkbookCatalogPort {
    report: WorkbookCatalogReport,
    workbook: WorkbookRef,
}

impl WorkbookCatalogPort for StubWorkbookCatalogPort {
    fn list_workbooks(&self) -> Result<WorkbookCatalogReport, AppError> {
        Ok(self.report.clone())
    }

    fn select_workbook(&self, _workbook_name: &str) -> Result<WorkbookRef, AppError> {
        Ok(self.workbook.clone())
    }
}

struct StubCheckHistoryPort {
    events: Events,
    fails: bool,
}

impl CheckHistoryPort for StubCheckHistoryPort {
    fn save_check_history(
        &self,
        workbook: &WorkbookRef,
        report: &CheckReport,
    ) -> Result<CheckHistoryReport, AppError> {
        self.events.lock().unwrap().push("history");
        if self.fails {
            return Err(AppError::from_source(
                "check.history.save",
                AppErrorCode::HistoryWriteFailed,
                "检查历史保存失败",
                std::io::Error::other("history permission denied"),
            ));
        }
        Ok(CheckHistoryReport::new(
            report.plan().schema_version(),
            workbook
                .relative_path()
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap()
                .to_owned(),
            1_700_000_000_123,
            "data/history/check.json".to_owned(),
            42,
            "a".repeat(64),
            report.plan().content_sha256().to_owned(),
        ))
    }
}

impl WorkbookOpenPort for StubWorkbookOpenPort {
    fn open_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookOpenReport, AppError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(WorkbookOpenReport::new(
            workbook
                .relative_path()
                .to_string_lossy()
                .replace('\\', "/"),
            "fixture-launcher",
        ))
    }
}

struct OrderedWorkbookPort {
    events: Events,
    desired: DesiredState,
    inventory_plan: EquipmentInventoryPlan,
}

impl WorkbookPort for OrderedWorkbookPort {
    fn load_layout(&self) -> Result<WorkbookLayout, AppError> {
        self.events.lock().unwrap().push("layout");
        Ok(empty_layout())
    }

    fn load_plan_inputs(
        &self,
        _workbook: &WorkbookRef,
    ) -> Result<crate::application::WorkbookPlanInputs, AppError> {
        self.events.lock().unwrap().push("plan");
        Ok(crate::application::WorkbookPlanInputs {
            layout: empty_layout(),
            desired: self.desired.clone(),
            inventory: self.inventory_plan.clone(),
            source_package_sha256: "a".repeat(64),
        })
    }
}

struct OrderedExecutionPort {
    state: GameState,
    target_identity: ExecutionTargetIdentity,
    events: Events,
    send_status: Option<ExecutionStatus>,
    active: bool,
    bound: bool,
    read_count: usize,
    drift_on_second_read: bool,
    shutdown_fails: bool,
}

impl GamePort for OrderedExecutionPort {
    fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
        self.events.lock().unwrap().push("game");
        self.active = true;
        if self.bound && self.drift_on_second_read && self.read_count > 0 {
            return Err(AppError::from_source(
                "plan.execute.session",
                AppErrorCode::SettingsInvalid,
                "测试执行会话配置发生变化",
                std::io::Error::other("fixture execution session drift"),
            )
            .with_context("component", "game")
            .with_context("configuration_changed", "true"));
        }
        self.read_count += 1;
        Ok(crate::application::GameObservation::from_state(
            self.state.clone(),
        ))
    }

    fn shutdown_session(&mut self) -> Result<(), AppError> {
        if !self.active {
            return Ok(());
        }
        self.events.lock().unwrap().push("shutdown");
        if self.shutdown_fails {
            return Err(fixture_shutdown_error());
        }
        self.active = false;
        self.bound = false;
        Ok(())
    }
}

impl ExecutionPort for OrderedExecutionPort {
    fn bind_current_session(&mut self) -> Result<(), AppError> {
        if !self.active || self.bound {
            return Err(AppError::from_source(
                "plan.execute.session",
                AppErrorCode::RuntimeBootstrapFailed,
                "测试执行会话绑定需要尚未绑定的活动连接",
                std::io::Error::other("fixture execution session is not active or already bound"),
            ));
        }
        self.events.lock().unwrap().push("bind");
        self.bound = true;
        Ok(())
    }

    fn target_identity(&mut self, _state: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
        self.events.lock().unwrap().push("target");
        Ok(self.target_identity.clone())
    }

    fn preflight_plan(&mut self, preflight: &ExecutionPreflight) -> Result<(), AppError> {
        self.events.lock().unwrap().push("preflight");
        assert_eq!(
            preflight.steps().len(),
            usize::from(self.send_status.is_some())
        );
        Ok(())
    }

    fn send_command(&mut self, command: &ExecutionCommand) -> ExecutionSendResult {
        self.events.lock().unwrap().push("send");
        let status = self
            .send_status
            .take()
            .expect("测试执行端口只允许发送一条命令");
        ExecutionSendResult::Receipt(ExecutionCommandReceipt::new(
            command.command_id(),
            status,
            Some(format!("fixture {}", status.as_str())),
            None,
            None,
            BTreeMap::new(),
        ))
    }

    fn query_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("空计划不得查询命令")
    }

    fn cancel_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("空计划不得取消命令")
    }
}

struct OrderedBackupPort {
    events: Events,
    fails: bool,
}

impl WorkbookBackupPort for OrderedBackupPort {
    fn backup_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookBackupReport, AppError> {
        self.events.lock().unwrap().push("backup");
        if self.fails {
            return Err(AppError::from_source(
                "execution.workbook.backup",
                AppErrorCode::WorkbookBackupFailed,
                "测试备份失败",
                std::io::Error::other("fixture backup failure"),
            ));
        }
        Ok(WorkbookBackupReport::new(
            workbook
                .relative_path()
                .to_string_lossy()
                .replace('\\', "/"),
            "data/backups/backup.xlsx".to_owned(),
            1_700_000_000_000,
            42,
            "a".repeat(64),
            "a".repeat(64),
        ))
    }
}

struct OrderedHistoryPort {
    events: Events,
    fails: bool,
}

impl ExecutionHistoryPort for OrderedHistoryPort {
    fn save_execution_history(
        &self,
        _workbook: &WorkbookRef,
        report: &ExecutionReport,
    ) -> Result<ExecutionHistoryReport, AppError> {
        self.events.lock().unwrap().push("history");
        if self.fails {
            return Err(AppError::from_source(
                "execution.history.save",
                AppErrorCode::HistoryWriteFailed,
                "测试历史保存失败",
                std::io::Error::other("fixture history failure"),
            ));
        }
        Ok(ExecutionHistoryReport::new(
            1,
            report.schema_version(),
            report.plan_schema_version(),
            "plan.xlsx".to_owned(),
            1_700_000_000_123,
            "data/history/execution.json".to_owned(),
            42,
            "b".repeat(64),
            report.target_identity().fingerprint_sha256().to_owned(),
            report.plan_hash().to_owned(),
            report.content_sha256().to_owned(),
            report.status(),
            report.may_have_writes(),
        ))
    }
}

struct OrderedResultsPort {
    events: Events,
    fails: bool,
    expected_row_count: usize,
}

impl ExecutionResultsPort for OrderedResultsPort {
    fn write_check_results(
        &self,
        _workbook: &WorkbookRef,
        source: &str,
        _layout: &WorkbookLayout,
        result: Result<&CheckReport, &AppError>,
        checked_at: Option<i64>,
    ) -> Result<(), AppError> {
        assert_eq!(source, "a".repeat(64));
        if result.is_err() {
            assert!(checked_at.is_none());
        }
        self.events.lock().unwrap().push(if result.is_ok() {
            "check_passed"
        } else {
            "check_failed"
        });
        if self.fails {
            return Err(AppError::from_source(
                "check.workbook.writeback",
                AppErrorCode::WorkbookLocked,
                "工作簿被占用",
                std::io::Error::other("locked"),
            ));
        }
        Ok(())
    }

    fn validate_writeback(
        &self,
        _workbook: &WorkbookRef,
        _expected_source_package_sha256: &str,
        _layout: &WorkbookLayout,
        _projection: crate::application::WorkbookProjectionV4,
    ) -> Result<(), AppError> {
        Ok(())
    }

    fn write_execution_results(
        &self,
        workbook: &WorkbookRef,
        expected_source_package_sha256: &str,
        _layout: &WorkbookLayout,
        rows: &[WorkbookProjectionRow],
        _final_projection: Option<crate::application::WorkbookProjectionV4>,
        _recorded_at_unix_millis: i64,
    ) -> Result<ExecutionResultsWriteReport, AppError> {
        self.events.lock().unwrap().push("writeback");
        assert_eq!(expected_source_package_sha256, "a".repeat(64));
        assert_eq!(rows.len(), self.expected_row_count);
        if self.fails {
            return Err(AppError::from_source(
                "execution.workbook.writeback",
                AppErrorCode::ExecutionResultsWriteFailed,
                "测试写回失败",
                std::io::Error::other("fixture writeback failure"),
            ));
        }
        Ok(ExecutionResultsWriteReport::new(
            workbook
                .relative_path()
                .to_string_lossy()
                .replace('\\', "/"),
            "执行结果".to_owned(),
            "xl/worksheets/sheet14.xml".to_owned(),
            "xl/tables/table14.xml".to_owned(),
            rows.len(),
            expected_source_package_sha256.to_owned(),
            "c".repeat(64),
            42,
            40,
            vec![
                "xl/tables/table14.xml".to_owned(),
                "xl/worksheets/sheet14.xml".to_owned(),
            ],
            "rename(same-directory)".to_owned(),
            true,
        ))
    }
}

impl WorkbookBackupPort for StubWorkbookBackupPort {
    fn backup_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookBackupReport, AppError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(WorkbookBackupReport::new(
            workbook
                .relative_path()
                .to_string_lossy()
                .replace('\\', "/"),
            "data/backups/backup.xlsx".to_owned(),
            1_700_000_000_123,
            42,
            "a".repeat(64),
            "a".repeat(64),
        ))
    }
}

impl LayoutPreviewPort for StubLayoutPreviewPort {
    fn preview_layout(&self, layout: &WorkbookLayout) -> Result<LayoutPreviewReport, AppError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(LayoutPreviewReport::new(
            "data/workbooks/layout-preview.xlsx".to_owned(),
            layout.schema_version(),
            WORKBOOK_PROJECTION_SCHEMA_VERSION,
            1,
            0,
            0,
            2,
            0,
            0,
            1,
            layout.content_sha256().to_owned(),
            "4".repeat(64),
        ))
    }
}

fn stub_preview() -> StubLayoutPreviewPort {
    StubLayoutPreviewPort {
        calls: Arc::new(AtomicUsize::new(0)),
    }
}

struct UnusedGenerationPort;

struct KeepOnlyExecutionPort {
    states: VecDeque<GameState>,
    last_state: Option<GameState>,
    target_identities: VecDeque<ExecutionTargetIdentity>,
    last_target_identity: Option<ExecutionTargetIdentity>,
}

impl KeepOnlyExecutionPort {
    fn new(states: Vec<GameState>) -> Self {
        Self {
            states: states.into(),
            last_state: None,
            target_identities: VecDeque::from([
                ExecutionTargetIdentity::new("a".repeat(64)).unwrap()
            ]),
            last_target_identity: None,
        }
    }

    fn with_target_identities(mut self, identities: Vec<ExecutionTargetIdentity>) -> Self {
        self.target_identities = identities.into();
        self
    }
}

impl GamePort for KeepOnlyExecutionPort {
    fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
        if let Some(state) = self.states.pop_front() {
            self.last_state = Some(state.clone());
            return Ok(crate::application::GameObservation::from_state(state));
        }
        Ok(crate::application::GameObservation::from_state(
            self.last_state
                .clone()
                .expect("零步骤执行端口缺少可复用状态"),
        ))
    }
}

impl ExecutionPort for KeepOnlyExecutionPort {
    fn bind_current_session(&mut self) -> Result<(), AppError> {
        Ok(())
    }

    fn target_identity(&mut self, _state: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
        if let Some(identity) = self.target_identities.pop_front() {
            self.last_target_identity = Some(identity.clone());
            return Ok(identity);
        }
        Ok(self
            .last_target_identity
            .clone()
            .expect("零步骤执行端口缺少可复用目标身份"))
    }

    fn preflight_plan(&mut self, preflight: &ExecutionPreflight) -> Result<(), AppError> {
        assert!(preflight.steps().is_empty());
        Ok(())
    }

    fn send_command(&mut self, _command: &ExecutionCommand) -> ExecutionSendResult {
        panic!("零步骤计划不应发送命令")
    }

    fn query_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("零步骤计划不应查询命令")
    }

    fn cancel_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("零步骤计划不应取消命令")
    }
}

fn with_state_content_digest(state: &GameState, byte: char) -> GameState {
    let source = state.source();
    GameState::new(
        GameStateSource::new(
            source.module_sha256().to_owned(),
            source.owned_state_schema_version(),
            source.ship_details_schema_version(),
            source.ship_catalog_schema_version(),
            source.equipment_catalog_schema_version(),
            source.raw_records_schema_version(),
            source.owned_state_content_sha256().to_owned(),
            source.ship_roster_content_sha256().to_owned(),
            source.ship_catalog_content_sha256().to_owned(),
            source.equipment_catalog_content_sha256().to_owned(),
            source.raw_records_content_sha256().to_owned(),
            byte.to_string().repeat(64),
        ),
        state.ships().clone(),
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    )
}

impl WorkbookGenerationPort for UnusedGenerationPort {
    fn generate_workbook(
        &self,
        _requested_name: Option<&str>,
        _layout: &WorkbookLayout,
        _projection: WorkbookProjectionV4,
    ) -> Result<WorkbookGenerationReport, AppError> {
        panic!("当前测试不应调用工作簿生成端口")
    }
}

fn unused_generation() -> Box<dyn WorkbookGenerationPort> {
    Box::new(UnusedGenerationPort)
}

fn test_sync_service(
    workbook: Box<dyn WorkbookPort>,
    game: Option<Box<dyn GamePort>>,
    generation: Box<dyn WorkbookGenerationPort>,
) -> WorkbookSyncService {
    WorkbookSyncService::new(workbook, game, generation)
}

fn test_check_service(
    workbook: Box<dyn WorkbookPort>,
    game: Option<Box<dyn GamePort>>,
    _generation: Box<dyn WorkbookGenerationPort>,
) -> WorkbookCheckService {
    WorkbookCheckService::new(
        workbook,
        game,
        Box::new(IdleCheckHistory),
        Box::new(IdleResults),
    )
}

fn test_execute_service(
    workbook: Box<dyn WorkbookPort>,
    execution: Option<Box<dyn ExecutionPort>>,
) -> WorkbookExecuteService {
    WorkbookExecuteService::new(
        workbook,
        execution,
        Box::new(IdleBackup),
        Box::new(IdleExecutionHistory),
        Box::new(IdleResults),
    )
}

struct IdleOpen;
struct IdleCatalog;
struct IdleCheckHistory;
struct IdleHistory;
struct IdleSettings;
struct IdleLogs;
struct IdleDiagnostics;
struct IdleBackup;
struct IdleExecutionHistory;
struct IdleResults;

impl WorkbookOpenPort for IdleOpen {
    fn open_workbook(&self, _: &WorkbookRef) -> Result<WorkbookOpenReport, AppError> {
        panic!("当前测试不应打开工作簿")
    }
}
impl WorkbookCatalogPort for IdleCatalog {
    fn list_workbooks(&self) -> Result<WorkbookCatalogReport, AppError> {
        panic!("当前测试不应列出工作簿")
    }
    fn select_workbook(&self, _: &str) -> Result<WorkbookRef, AppError> {
        panic!("当前测试不应选择工作簿")
    }
}
impl CheckHistoryPort for IdleCheckHistory {
    fn save_check_history(
        &self,
        _: &WorkbookRef,
        _: &CheckReport,
    ) -> Result<CheckHistoryReport, AppError> {
        panic!("当前测试不应保存检查历史")
    }
}
impl HistoryCatalogPort for IdleHistory {
    fn list_history(
        &self,
        _is_cancelled: &dyn Fn() -> bool,
    ) -> Result<HistoryCatalogReport, AppError> {
        panic!("当前测试不应读取历史目录")
    }
}
impl SettingsPort for IdleSettings {
    fn read_settings(&self) -> Result<SettingsSummaryReport, AppError> {
        panic!("当前测试不应读取设置")
    }

    fn read_preferences(&self) -> Result<crate::application::UserPreferences, AppError> {
        panic!("当前测试不应读取设置")
    }

    fn save_preferences(
        &self,
        _original: crate::application::UserPreferences,
        _preferences: crate::application::UserPreferences,
    ) -> Result<(), AppError> {
        panic!("当前测试不应保存设置")
    }
}
impl LogCatalogPort for IdleLogs {
    fn list_logs(&self, _is_cancelled: &dyn Fn() -> bool) -> Result<LogCatalogReport, AppError> {
        panic!("当前测试不应读取日志目录")
    }
}
impl DiagnosticOpenPort for IdleDiagnostics {
    fn open_diagnostic(
        &self,
        _: &crate::application::DiagnosticArtifactRef,
    ) -> Result<(), AppError> {
        panic!("当前测试不应打开诊断文件")
    }
}
impl WorkbookBackupPort for IdleBackup {
    fn backup_workbook(&self, _: &WorkbookRef) -> Result<WorkbookBackupReport, AppError> {
        panic!("当前测试不应备份工作簿")
    }
}
impl ExecutionHistoryPort for IdleExecutionHistory {
    fn save_execution_history(
        &self,
        _: &WorkbookRef,
        _: &ExecutionReport,
    ) -> Result<crate::application::ExecutionHistoryReport, AppError> {
        panic!("当前测试不应保存执行历史")
    }
}
impl ExecutionResultsPort for IdleResults {
    fn validate_writeback(
        &self,
        _: &WorkbookRef,
        _: &str,
        _: &WorkbookLayout,
        _: WorkbookProjectionV4,
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn write_execution_results(
        &self,
        _: &WorkbookRef,
        _: &str,
        _: &WorkbookLayout,
        _: &[crate::application::WorkbookProjectionRow],
        _: Option<WorkbookProjectionV4>,
        _: i64,
    ) -> Result<crate::application::ExecutionResultsWriteReport, AppError> {
        panic!("当前测试不应写回执行结果")
    }
}

fn ordered_execution_service(
    events: Events,
    backup_fails: bool,
    history_fails: bool,
    writeback_fails: bool,
) -> WorkbookExecuteService {
    ordered_execution_service_with_input(
        events,
        OrderedExecutionScenario {
            state: empty_game_state(),
            desired: DesiredState::new(Vec::new()).unwrap(),
            send_status: None,
            expected_row_count: 0,
            backup_fails,
            history_fails,
            writeback_fails,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    )
}

struct OrderedExecutionScenario {
    state: GameState,
    desired: DesiredState,
    send_status: Option<ExecutionStatus>,
    expected_row_count: usize,
    backup_fails: bool,
    history_fails: bool,
    writeback_fails: bool,
    shutdown_fails: bool,
    drift_on_second_read: bool,
}

fn ordered_execution_service_with_input(
    events: Events,
    scenario: OrderedExecutionScenario,
) -> WorkbookExecuteService {
    WorkbookExecuteService::new(
        Box::new(OrderedWorkbookPort {
            events: Arc::clone(&events),
            desired: scenario.desired,
            inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
        }),
        Some(Box::new(OrderedExecutionPort {
            state: scenario.state,
            target_identity: ExecutionTargetIdentity::new("a".repeat(64)).unwrap(),
            events: Arc::clone(&events),
            send_status: scenario.send_status,
            active: false,
            bound: false,
            read_count: 0,
            drift_on_second_read: scenario.drift_on_second_read,
            shutdown_fails: scenario.shutdown_fails,
        })),
        Box::new(OrderedBackupPort {
            events: Arc::clone(&events),
            fails: scenario.backup_fails,
        }),
        Box::new(OrderedHistoryPort {
            events: Arc::clone(&events),
            fails: scenario.history_fails,
        }),
        Box::new(OrderedResultsPort {
            events: Arc::clone(&events),
            fails: scenario.writeback_fails,
            expected_row_count: scenario.expected_row_count,
        }),
    )
}

fn warehouse_equip_desired_state() -> DesiredState {
    let equipment = crate::domain::DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        crate::domain::SourcePolicy::ExactSource,
        Some(crate::domain::EquipmentSourceRef::Warehouse(
            crate::domain::EquipmentConfigId::new(1001).unwrap(),
        )),
        None,
    )
    .unwrap();
    DesiredState::new(vec![crate::domain::DesiredSlotState::new(
        crate::domain::ShipSlotRef::new(
            crate::domain::ShipInstanceId::new(9001).unwrap(),
            crate::domain::SlotIndex::new(2).unwrap(),
        ),
        crate::domain::SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap()
}

fn completed_execution(outcome: WorkbookExecuteOutcome) -> WorkbookExecutionReport {
    outcome.completed().cloned().expect("应完成工作簿执行")
}

struct AlwaysCancelled;

impl ExecutionCancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn delegates_execution_backup_without_exposing_file_operations() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut service = test_execute_service(
        Box::new(FakeWorkbookPort::new(
            empty_layout(),
            Arc::new(std::sync::Mutex::new(Vec::new())),
        )),
        None,
    );
    service.workbook_backup = Box::new(StubWorkbookBackupPort {
        calls: Arc::clone(&calls),
    });
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let report = service.backup_workbook(&workbook).unwrap();

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(report.source_path(), "data/workbooks/plan.xlsx");
    assert_eq!(report.backup_path(), "data/backups/backup.xlsx");
    assert_eq!(report.source_package_sha256(), "a".repeat(64));
    assert_eq!(
        report.source_package_sha256(),
        report.backup_package_sha256()
    );
}

#[test]
fn execute_workbook_without_modifications_skips_game_backup_and_writeback() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service(Arc::clone(&events), false, false, false);
    let mut progress = Vec::new();

    assert!(matches!(
        service
            .execute_workbook_with_progress(
                &workbook,
                &crate::application::NoExecutionCancellation,
                &mut |event| progress.push(event),
            )
            .unwrap(),
        WorkbookExecuteOutcome::NoModifications
    ));
    assert_eq!(events.lock().unwrap().as_slice(), ["plan"]);
    assert_eq!(
        progress
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>(),
        ["正在读取工作簿计划", "修改列没有需要执行的操作",]
    );
    assert!(progress.iter().all(|event| event.units.is_none()));
}

#[test]
fn completed_execution_requires_a_successful_game_session_shutdown() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Success),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: false,
            writeback_fails: false,
            shutdown_fails: true,
            drift_on_second_read: false,
        },
    );

    let outcome = service
        .execute_workbook(&workbook)
        .expect("执行完成后的清理失败仍返回执行事实");
    let WorkbookExecuteOutcome::Finished {
        execution,
        history,
        writeback,
        cleanup,
        ..
    } = outcome
    else {
        panic!("清理失败应保留已经结束的执行");
    };
    let cleanup = cleanup.error().expect("清理失败");
    let history = history.completed().expect("历史已保存");
    let writeback = writeback.completed().expect("写回已完成");
    assert_eq!(cleanup.stage(), "game.cleanup.fixture");
    assert_eq!(cleanup.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert!(execution.may_have_writes());
    assert!(cleanup.context().get("execution_completed").is_none());
    assert_eq!(history.relative_path(), "data/history/execution.json");
    assert_eq!(writeback.workbook_path(), "data/workbooks/plan.xlsx");
    assert!(!execution.content_sha256().is_empty());
    assert!(!writeback.output_package_sha256().is_empty());
    assert_eq!(
        cleanup.context().get("access").map(String::as_str),
        Some("write")
    );
    assert_eq!(events.lock().unwrap().last(), Some(&"shutdown"));
}

#[test]
fn checked_workbook_rejects_changed_plan_before_binding_and_backup() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Success),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: false,
            writeback_fails: false,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    );
    let error = service
        .execute_workbook_checked(
            &workbook,
            Some(&"0".repeat(64)),
            &crate::application::NoExecutionCancellation,
            &mut |_| {},
        )
        .unwrap_err();
    assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
    let events = events.lock().unwrap();
    assert!(!events.contains(&"backup"));
    assert!(!events.contains(&"bind"));
    assert!(!events.contains(&"send"));
    assert_eq!(events.last(), Some(&"shutdown"));
}

#[test]
fn execution_session_configuration_drift_stops_before_send_and_closes_once() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Success),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: false,
            writeback_fails: false,
            shutdown_fails: false,
            drift_on_second_read: true,
        },
    );

    let error = service.execute_workbook(&workbook).unwrap_err();

    assert_eq!(error.stage(), "plan.execute.session");
    assert_eq!(error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(
        error
            .context()
            .get("configuration_changed")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        error.context().get("access").map(String::as_str),
        Some("write")
    );
    let recorded = events.lock().unwrap();
    assert_eq!(recorded.iter().filter(|event| **event == "bind").count(), 1);
    assert_eq!(recorded.iter().filter(|event| **event == "game").count(), 2);
    assert!(!recorded.contains(&"send"));
    assert!(!recorded.contains(&"preflight"));
    assert!(!recorded.contains(&"history"));
    assert!(!recorded.contains(&"writeback"));
    assert_eq!(
        recorded
            .iter()
            .filter(|event| **event == "shutdown")
            .count(),
        1
    );
    assert_eq!(recorded.last(), Some(&"shutdown"));
}

#[test]
fn failed_non_empty_command_is_sent_once_after_backup_and_still_persisted() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Failed),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: false,
            writeback_fails: false,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    );

    let report = completed_execution(service.execute_workbook(&workbook).unwrap());

    assert_eq!(report.execution().status(), ExecutionReportStatus::Failed);
    assert_eq!(report.execution().steps().len(), 1);
    assert_eq!(report.execution_results().row_count(), 1);
    let recorded = events.lock().unwrap();
    assert_eq!(recorded.iter().filter(|event| **event == "bind").count(), 1);
    assert_eq!(recorded.iter().filter(|event| **event == "send").count(), 1);
    let backup_index = recorded
        .iter()
        .position(|event| *event == "backup")
        .unwrap();
    let send_index = recorded.iter().position(|event| *event == "send").unwrap();
    let history_index = recorded
        .iter()
        .position(|event| *event == "history")
        .unwrap();
    let writeback_index = recorded
        .iter()
        .position(|event| *event == "writeback")
        .unwrap();
    let shutdown_index = recorded
        .iter()
        .position(|event| *event == "shutdown")
        .unwrap();
    assert!(backup_index < send_index);
    assert!(send_index < history_index);
    assert!(history_index < writeback_index);
    assert!(writeback_index < shutdown_index);
}

#[test]
fn cancelled_workbook_execution_stops_before_send_and_persists_the_result() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Success),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: false,
            writeback_fails: false,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    );

    let report = completed_execution(
        service
            .execute_workbook_with_cancellation(&workbook, &AlwaysCancelled)
            .unwrap(),
    );

    assert_eq!(
        report.execution().status(),
        ExecutionReportStatus::Cancelled
    );
    assert_eq!(report.execution_results().row_count(), 1);
    let recorded = events.lock().unwrap();
    assert!(!recorded.contains(&"send"));
    assert!(recorded.contains(&"history"));
    assert!(recorded.contains(&"writeback"));
    assert_eq!(recorded.last(), Some(&"shutdown"));
}

#[test]
fn backup_failure_stops_before_the_executing_snapshot_and_persistence() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: None,
            expected_row_count: 1,
            backup_fails: true,
            history_fails: false,
            writeback_fails: false,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    );

    let error = service.execute_workbook(&workbook).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::WorkbookBackupFailed);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["plan", "game", "target", "bind", "backup", "shutdown",]
    );
}

#[test]
fn history_failure_after_execution_is_marked_non_retryable_and_skips_writeback() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Success),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: true,
            writeback_fails: false,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    );

    let outcome = service
        .execute_workbook(&workbook)
        .expect("历史失败后仍保留已经结束的执行");
    let WorkbookExecuteOutcome::Finished {
        execution,
        backup,
        history,
        writeback,
        ..
    } = outcome
    else {
        panic!("历史失败不应表现为尚未执行");
    };
    assert_eq!(
        history.failed().expect("历史失败").code(),
        AppErrorCode::HistoryWriteFailed
    );
    assert!(matches!(
        writeback,
        crate::application::StageResult::NotAttempted
    ));
    assert!(execution.may_have_writes());
    assert!(!execution.content_sha256().is_empty());
    assert_eq!(backup.backup_path(), "data/backups/backup.xlsx");
    assert_eq!(
        backup.source_package_sha256(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert_eq!(
        backup.backup_package_sha256(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    let recorded = events.lock().unwrap();
    assert!(recorded.contains(&"history"));
    assert_eq!(recorded.last(), Some(&"shutdown"));
    assert!(!recorded.contains(&"writeback"));
}

#[test]
fn writeback_failure_keeps_the_published_history_reference() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = ordered_execution_service_with_input(
        Arc::clone(&events),
        OrderedExecutionScenario {
            state: plan_game_state(),
            desired: warehouse_equip_desired_state(),
            send_status: Some(ExecutionStatus::Success),
            expected_row_count: 1,
            backup_fails: false,
            history_fails: false,
            writeback_fails: true,
            shutdown_fails: false,
            drift_on_second_read: false,
        },
    );

    let outcome = service
        .execute_workbook(&workbook)
        .expect("写回失败后仍保留已经结束的执行");
    let WorkbookExecuteOutcome::Finished {
        backup,
        history,
        writeback,
        ..
    } = outcome
    else {
        panic!("写回失败不应表现为尚未执行");
    };
    assert_eq!(
        writeback.failed().expect("写回失败").code(),
        AppErrorCode::ExecutionResultsWriteFailed
    );
    assert_eq!(
        history.completed().expect("历史已保存").relative_path(),
        "data/history/execution.json"
    );
    assert_eq!(backup.backup_path(), "data/backups/backup.xlsx");
    let recorded = events.lock().unwrap();
    assert!(recorded.contains(&"writeback"));
    assert_eq!(recorded.last(), Some(&"shutdown"));
}

#[test]
fn opens_a_controlled_workbook_through_the_configured_port() {
    let calls = Arc::new(AtomicUsize::new(0));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let service = super::WorkspaceService::with_ports(
        Box::new(StubWorkbookOpenPort {
            calls: Arc::clone(&calls),
        }),
        Box::new(IdleCatalog),
        Box::new(IdleHistory),
        Box::new(IdleSettings),
        Box::new(IdleLogs),
        Box::new(IdleDiagnostics),
        None,
    );

    let report = service.open_workbook(&workbook).unwrap();

    assert_eq!(report.message(), "工作簿已交给系统默认程序打开");
    assert_eq!(report.workbook_path(), "data/workbooks/plan.xlsx");
    assert_eq!(report.launcher(), "fixture-launcher");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn lists_and_selects_workbooks_through_one_application_port() {
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let report = WorkbookCatalogReport::new(vec![WorkbookCatalogEntry::new(
        "plan.xlsx".to_owned(),
        "data/workbooks/plan.xlsx".to_owned(),
        42,
    )]);
    let service = super::WorkspaceService::with_ports(
        Box::new(IdleOpen),
        Box::new(StubWorkbookCatalogPort {
            report: report.clone(),
            workbook: workbook.clone(),
        }),
        Box::new(IdleHistory),
        Box::new(IdleSettings),
        Box::new(IdleLogs),
        Box::new(IdleDiagnostics),
        None,
    );

    assert_eq!(service.list_workbooks().unwrap(), report);
    assert_eq!(service.select_workbook("plan.xlsx").unwrap(), workbook);

    let startup = service.load_startup().unwrap();
    assert_eq!(startup.workbook_names(), ["plan.xlsx"]);
    assert!(startup.instances().candidates().is_empty());
    assert!(startup.instance_catalog_error().is_some());

    let instances = EmulatorInstanceCatalogReport::new(Some("0".to_owned()), Vec::new());
    let service = super::WorkspaceService::with_ports(
        Box::new(IdleOpen),
        Box::new(StubWorkbookCatalogPort {
            report: report.clone(),
            workbook,
        }),
        Box::new(IdleHistory),
        Box::new(IdleSettings),
        Box::new(IdleLogs),
        Box::new(IdleDiagnostics),
        Some(Box::new(StubInstanceCatalog {
            report: instances.clone(),
        })),
    );
    let startup = service.load_startup().unwrap();
    assert_eq!(startup.workbook_count(), 1);
    assert_eq!(startup.instances(), &instances);
    assert!(startup.instance_catalog_error().is_none());
}

struct StubInstanceCatalog {
    report: EmulatorInstanceCatalogReport,
}

impl EmulatorInstanceCatalogPort for StubInstanceCatalog {
    fn list_emulator_instances(&self) -> Result<EmulatorInstanceCatalogReport, AppError> {
        Ok(self.report.clone())
    }
}

#[test]
fn exposes_history_and_settings_as_independent_read_only_use_cases() {
    let history = HistoryCatalogReport::new(Vec::new(), Vec::new());
    let settings = SettingsSummaryReport::new(crate::application::DoctorSettingsCheck::new(
        "auto", false, false, false, false, 30, 180,
    ));
    let service = super::WorkspaceService::with_ports(
        Box::new(IdleOpen),
        Box::new(IdleCatalog),
        Box::new(StubHistoryCatalogPort {
            report: history.clone(),
        }),
        Box::new(StubSettingsPort {
            report: settings.clone(),
        }),
        Box::new(IdleLogs),
        Box::new(IdleDiagnostics),
        None,
    );

    assert_eq!(service.list_history(&|| false).unwrap(), history);
    assert_eq!(service.read_settings().unwrap(), settings);
}

#[test]
fn exposes_log_catalog_as_a_read_only_use_case() {
    let logs = LogCatalogReport::new(Vec::new(), Vec::new());
    let service = super::WorkspaceService::with_ports(
        Box::new(IdleOpen),
        Box::new(IdleCatalog),
        Box::new(IdleHistory),
        Box::new(IdleSettings),
        Box::new(StubLogCatalogPort {
            report: logs.clone(),
        }),
        Box::new(IdleDiagnostics),
        None,
    );

    assert_eq!(service.list_logs(&|| false).unwrap(), logs);
}

#[test]
fn checks_then_saves_history_through_one_application_use_case() {
    use crate::application::OperationProgress as P;
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = test_check_service(
        Box::new(StubDesiredWorkbookPort {
            desired: DesiredState::new(Vec::new()).unwrap(),
            inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
            events: Arc::clone(&events),
        }),
        Some(Box::new(FakeGamePort::success(
            empty_game_state(),
            Arc::clone(&events),
        ))),
        unused_generation(),
    );
    service.check_history = Box::new(StubCheckHistoryPort {
        events: Arc::clone(&events),
        fails: false,
    });
    service.execution_results = Box::new(OrderedResultsPort {
        events: Arc::clone(&events),
        fails: false,
        expected_row_count: 0,
    });

    let mut progress = Vec::new();
    let outcome = service
        .check_and_save_workbook_plan_with_progress(&workbook, &mut |event| progress.push(event))
        .unwrap();
    let crate::application::CheckAndSaveOutcome::Saved(report) = outcome else {
        panic!("检查、历史和写回都成功时应返回已保存记录");
    };

    assert_eq!(report.message(), "配装计划检查记录已保存");
    assert_eq!(report.workbook_name(), "plan.xlsx");
    assert_eq!(report.relative_path(), "data/history/check.json");
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["plan", "history", "check_passed"]
    );
    assert_eq!(
        progress,
        [
            P::stage("正在读取工作簿计划"),
            P::stage("修改列没有需要检查的操作"),
            P::stage("正在保存计划检查记录"),
            P::stage("正在写入检查结果工作表"),
        ]
    );
}

#[test]
fn reports_only_values_from_the_validated_layout() {
    let calls = Arc::new(AtomicUsize::new(0));
    let layout = WorkbookLayout::new(
        LAYOUT_SCHEMA_VERSION,
        "自定义布局".to_owned(),
        "验证应用服务摘要".to_owned(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "0".repeat(64),
    )
    .unwrap();
    let service = super::LayoutService::with_ports(
        Box::new(StubWorkbookPort {
            layout,
            calls: Arc::clone(&calls),
        }),
        Box::new(StubLayoutUpgradePort),
        Box::new(stub_preview()),
    );

    let report: LayoutCheckReport = service.check_layout().unwrap();

    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(report.message(), "工作簿布局检查通过");
    assert_eq!(report.schema_version(), LAYOUT_SCHEMA_VERSION);
    assert_eq!(report.template_name(), "自定义布局");
    assert_eq!(report.purpose(), "验证应用服务摘要");
    assert_eq!(report.sheet_count(), 0);
    assert_eq!(report.field_count(), 0);
    assert_eq!(report.enum_option_count(), 0);
    assert_eq!(report.style_count(), 0);
    assert_eq!(report.content_sha256(), "0".repeat(64));
}

#[test]
fn delegates_layout_upgrade_without_rebuilding_the_report() {
    let layout = WorkbookLayout::new(
        LAYOUT_SCHEMA_VERSION,
        "布局".to_owned(),
        "测试".to_owned(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "0".repeat(64),
    )
    .unwrap();
    let service = super::LayoutService::with_ports(
        Box::new(StubWorkbookPort {
            layout,
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Box::new(StubLayoutUpgradePort),
        Box::new(stub_preview()),
    );

    let report = service.upgrade_layout().unwrap();

    assert_eq!(report.message(), "工作簿布局升级完成");
    assert_eq!(report.preserved().fields(), 2);
    assert_eq!(report.output_package_sha256(), "2".repeat(64));
    assert_eq!(report.output_content_sha256(), "3".repeat(64));
}

#[test]
fn loads_the_strict_layout_before_delegating_preview_generation() {
    let workbook_calls = Arc::new(AtomicUsize::new(0));
    let preview_calls = Arc::new(AtomicUsize::new(0));
    let layout = WorkbookLayout::new(
        LAYOUT_SCHEMA_VERSION,
        "布局".to_owned(),
        "测试预览".to_owned(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "5".repeat(64),
    )
    .unwrap();
    let service = super::LayoutService::with_ports(
        Box::new(StubWorkbookPort {
            layout,
            calls: Arc::clone(&workbook_calls),
        }),
        Box::new(StubLayoutUpgradePort),
        Box::new(StubLayoutPreviewPort {
            calls: Arc::clone(&preview_calls),
        }),
    );

    let report = service.preview_layout().unwrap();

    assert_eq!(workbook_calls.load(Ordering::Relaxed), 1);
    assert_eq!(preview_calls.load(Ordering::Relaxed), 1);
    assert_eq!(report.message(), "工作簿布局预览已刷新");
    assert_eq!(report.layout_content_sha256(), "5".repeat(64));
    assert_eq!(report.output_package_sha256(), "4".repeat(64));
}

#[test]
fn generates_through_the_service_in_layout_game_generation_order() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
        .expect("空状态应能建立投影");
    let expected = generation_report(&layout, &projection);
    let mut service = test_sync_service(
        Box::new(FakeWorkbookPort::new(layout.clone(), Arc::clone(&events))),
        Some(Box::new(FakeGamePort::success(
            empty_game_state(),
            Arc::clone(&events),
        ))),
        Box::new(FakeWorkbookGenerationPort::new(
            expected.clone(),
            Arc::clone(&events),
        )),
    );

    let mut progress = Vec::new();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let outcome = service
        .generate_workbook_with_progress(
            Some("fixture.xlsx"),
            &mut |event| {
                if event.message == "正在结束本次连接" {
                    cancelled.store(true, Ordering::Release);
                }
                progress.push(event);
            },
            &|| cancelled.load(Ordering::Acquire),
        )
        .expect("服务应透传生成端口报告");
    assert_eq!(
        progress
            .iter()
            .map(|event| event.message.as_str())
            .collect::<Vec<_>>(),
        [
            "正在读取工作簿布局",
            "正在连接游戏并按模板读取状态",
            "正在转换工作簿数据",
            "正在生成工作簿",
            "正在结束本次连接",
        ]
    );
    assert!(progress.iter().all(|event| event.units.is_none()));

    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["layout", "game", "generation"]
    );
    assert_eq!(outcome.report(), &expected);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(matches!(
        outcome,
        crate::application::WorkbookGenerationOutcome::Completed(_)
    ));
}

#[test]
fn cancelled_generation_stops_at_safe_stages_and_closes_the_session() {
    for (stage, expected_events) in [
        ("正在读取工作簿布局", vec![]),
        ("正在连接游戏并按模板读取状态", vec!["layout"]),
        ("正在转换工作簿数据", vec!["layout", "game", "shutdown"]),
        ("正在生成工作簿", vec!["layout", "game", "shutdown"]),
    ] {
        let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let layout = empty_layout();
        let projection =
            crate::application::project_game_state_to_workbook(&empty_game_state()).unwrap();
        let mut service = test_sync_service(
            Box::new(FakeWorkbookPort::new(layout.clone(), Arc::clone(&events))),
            None,
            Box::new(FakeWorkbookGenerationPort::new(
                generation_report(&layout, &projection),
                Arc::clone(&events),
            )),
        )
        .with_execution_port(Box::new(ShutdownTrackingGamePort::new(
            Some(empty_game_state()),
            None,
            None,
            Arc::clone(&events),
        )));
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let error = service
            .generate_workbook_with_progress(
                None,
                &mut |event| {
                    if event.message == stage {
                        cancelled.store(true, Ordering::Release);
                    }
                },
                &|| cancelled.load(Ordering::Acquire),
            )
            .unwrap_err();
        assert!(error.is_cancelled(), "{stage}: {error:?}");
        assert_eq!(*events.lock().unwrap(), expected_events, "{stage}");
    }
}

#[test]
fn cancelled_generation_preserves_session_cleanup_failure() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection =
        crate::application::project_game_state_to_workbook(&empty_game_state()).unwrap();
    let mut service = test_sync_service(
        Box::new(FakeWorkbookPort::new(layout.clone(), Arc::clone(&events))),
        None,
        Box::new(FakeWorkbookGenerationPort::new(
            generation_report(&layout, &projection),
            Arc::clone(&events),
        )),
    )
    .with_execution_port(Box::new(ShutdownTrackingGamePort::new(
        Some(empty_game_state()),
        None,
        Some(fixture_shutdown_error()),
        Arc::clone(&events),
    )));
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let error = service
        .generate_workbook_with_progress(
            None,
            &mut |event| {
                if event.message == "正在转换工作簿数据" {
                    cancelled.store(true, Ordering::Release);
                }
            },
            &|| cancelled.load(Ordering::Acquire),
        )
        .unwrap_err();
    assert_eq!(error.code(), AppErrorCode::OperationCancelled);
    assert!(!error.is_cancelled());
    assert_eq!(
        error
            .context()
            .get("game_cleanup_stage")
            .map(String::as_str),
        Some("game.cleanup.fixture")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["layout", "game", "shutdown"]
    );
}

#[test]
fn generated_workbook_requires_a_successful_game_session_shutdown() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
        .expect("空状态应能建立投影");
    let expected = generation_report(&layout, &projection);
    let mut service = test_sync_service(
        Box::new(FakeWorkbookPort::new(layout, Arc::clone(&events))),
        None,
        Box::new(FakeWorkbookGenerationPort::new(
            expected.clone(),
            Arc::clone(&events),
        )),
    )
    .with_execution_port(Box::new(ShutdownTrackingGamePort::new(
        Some(empty_game_state()),
        None,
        Some(fixture_shutdown_error()),
        Arc::clone(&events),
    )));

    let mut progress = Vec::new();
    let outcome = service
        .generate_workbook_with_progress(None, &mut |event| progress.push(event), &|| false)
        .expect("已发布工作簿后的清理失败仍返回生成终态");
    assert_eq!(progress.last().unwrap().message, "正在结束本次连接");
    assert!(progress.last().unwrap().units.is_none());
    let crate::application::WorkbookGenerationOutcome::CleanupIncomplete { report, cleanup } =
        outcome
    else {
        panic!("清理失败应保留已发布报告");
    };
    let crate::application::SessionCleanup::Failed(cleanup) = cleanup else {
        panic!("未恢复的关闭失败应保持失败事实");
    };
    assert_eq!(report, expected);
    assert_eq!(cleanup.stage(), "game.cleanup.fixture");
    assert_eq!(cleanup.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert!(cleanup.context().get("operation_completed").is_none());
    assert_eq!(
        cleanup.context().get("output_path").map(String::as_str),
        Some(expected.output_path())
    );
    assert_eq!(
        cleanup
            .context()
            .get("output_package_sha256")
            .map(String::as_str),
        Some(expected.output_package_sha256())
    );
    assert_eq!(
        cleanup.context().get("access").map(String::as_str),
        Some("read_only")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["layout", "game", "generation", "shutdown"]
    );
}

#[test]
fn generated_workbook_preserves_output_when_shutdown_recovery_completed() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
        .expect("空状态应能建立投影");
    let expected = generation_report(&layout, &projection);
    let mut service = test_sync_service(
        Box::new(FakeWorkbookPort::new(layout, Arc::clone(&events))),
        None,
        Box::new(FakeWorkbookGenerationPort::new(
            expected.clone(),
            Arc::clone(&events),
        )),
    )
    .with_execution_port(Box::new(ShutdownTrackingGamePort::new(
        Some(empty_game_state()),
        None,
        Some(
            fixture_shutdown_error()
                .with_cleanup_fact(crate::application::CleanupFact::Recovered)
                .with_context("game_restarted", "true"),
        ),
        Arc::clone(&events),
    )));

    let outcome = service
        .generate_workbook(None, &|| false)
        .expect("恢复清理后仍保留已发布报告");
    let crate::application::WorkbookGenerationOutcome::CleanupIncomplete { report, cleanup } =
        outcome
    else {
        panic!("恢复清理仍是未完成的清理终态");
    };
    let crate::application::SessionCleanup::Recovered(cleanup) = cleanup else {
        panic!("恢复清理应保持恢复事实");
    };
    assert_eq!(report, expected);
    assert_eq!(
        cleanup.context().get("cleanup").map(String::as_str),
        Some("recovered")
    );
    assert_eq!(
        cleanup.context().get("game_restarted").map(String::as_str),
        Some("true")
    );
    assert_eq!(cleanup.stage(), "game.cleanup.fixture");
    assert_eq!(cleanup.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert!(cleanup.context().get("operation_completed").is_none());
    assert_eq!(
        cleanup.context().get("output_path").map(String::as_str),
        Some(expected.output_path())
    );
    assert_eq!(
        cleanup
            .context()
            .get("output_package_sha256")
            .map(String::as_str),
        Some(expected.output_package_sha256())
    );
    assert_eq!(
        cleanup.context().get("access").map(String::as_str),
        Some("read_only")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["layout", "game", "generation", "shutdown"]
    );
}

#[test]
fn plan_check_requires_a_successful_game_session_shutdown() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(ShutdownTrackingGamePort::new(
            Some(empty_game_state()),
            None,
            Some(fixture_shutdown_error()),
            Arc::clone(&events),
        ))),
        unused_generation(),
    );
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(error.stage(), "game.cleanup.fixture");
    assert_eq!(error.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert_eq!(
        error.context().get("check_stage").map(String::as_str),
        Some("read_without_check")
    );
    assert_eq!(
        error.context().get("access").map(String::as_str),
        Some("read_only")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["plan", "game", "shutdown"]
    );
}

#[test]
fn operation_and_shutdown_failure_preserve_both_error_contracts() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(ShutdownTrackingGamePort::new(
            None,
            Some(
                AppError::from_source(
                    "game.read.fixture",
                    AppErrorCode::FullCheckFailed,
                    "测试游戏状态无效",
                    std::io::Error::other("fixture game read failure"),
                )
                .with_context("component", "game")
                .with_cleanup_fact(crate::application::CleanupFact::Failed {
                    owner_retained: true,
                })
                .with_context("cleanup_stage", "game.cleanup")
                .with_context("cleanup_code", "RUNTIME_BOOTSTRAP_FAILED")
                .with_context(
                    "cleanup_runtime_journal",
                    "data/logs/initial-cleanup-fixture.jsonl",
                ),
            ),
            Some(fixture_shutdown_error()),
            Arc::clone(&events),
        ))),
        unused_generation(),
    );

    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(error.stage(), "game.read.fixture");
    assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        error
            .context()
            .get("game_cleanup_stage")
            .map(String::as_str),
        Some("game.cleanup.fixture")
    );
    assert_eq!(
        error.context().get("game_cleanup_code").map(String::as_str),
        Some("RUNTIME_BOOTSTRAP_FAILED")
    );
    assert_eq!(
        error
            .context()
            .get("game_cleanup_runtime_code")
            .map(String::as_str),
        Some("fixture_cleanup_failed")
    );
    assert_eq!(
        error.context().get("access").map(String::as_str),
        Some("read_only")
    );
    assert_eq!(
        error
            .context()
            .get("game_cleanup_access")
            .map(String::as_str),
        Some("read_only")
    );
    assert!(!error.context().contains_key("cleanup_recovered"));
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["plan", "game", "shutdown"]
    );
}

#[test]
fn operation_and_cleanup_failure_keeps_both_cause_chains() {
    let operation = AppError::from_source(
        "plan.execute",
        AppErrorCode::FullCheckFailed,
        "业务失败",
        std::io::Error::other("业务底层原因"),
    );
    let cleanup = AppError::from_source(
        "game.cleanup",
        AppErrorCode::RuntimeBootstrapFailed,
        "清理失败",
        std::io::Error::other("清理底层原因"),
    );
    let error = super::game_operation_and_cleanup_error(operation, cleanup);
    let mut detail = String::new();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        detail.push_str("\n原因：");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    assert!(detail.contains("业务底层原因"), "{detail}");
    assert!(detail.contains("清理底层原因"), "{detail}");
    assert!(detail.contains("清理失败"), "{detail}");
    assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
}

#[test]
fn internal_cleanup_failure_recovered_by_service_retry_is_not_reported_as_final_failure() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(ShutdownTrackingGamePort::new(
            None,
            Some(
                AppError::from_source(
                    "game.read.fixture",
                    AppErrorCode::FullCheckFailed,
                    "测试游戏状态无效",
                    std::io::Error::other("fixture read and initial cleanup failure"),
                )
                .with_context("component", "game")
                .with_cleanup_fact(crate::application::CleanupFact::Failed {
                    owner_retained: true,
                })
                .with_context("cleanup_stage", "game.cleanup")
                .with_context("cleanup_code", "RUNTIME_BOOTSTRAP_FAILED")
                .with_context(
                    "cleanup_runtime_journal",
                    "data/logs/initial-cleanup-fixture.jsonl",
                ),
            ),
            None,
            Arc::clone(&events),
        ))),
        unused_generation(),
    );

    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(error.stage(), "game.read.fixture");
    assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("recovered")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_initial_attempt")
            .map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        error.context().get("cleanup_recovered").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_initial_owner_retained")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_owner_retained")
            .map(String::as_str),
        Some("false")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_initial_stage")
            .map(String::as_str),
        Some("game.cleanup")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_initial_code")
            .map(String::as_str),
        Some("RUNTIME_BOOTSTRAP_FAILED")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_runtime_journal")
            .map(String::as_str),
        Some("data/logs/initial-cleanup-fixture.jsonl")
    );
    assert_eq!(
        error.context().get("access").map(String::as_str),
        Some("read_only")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["plan", "game", "shutdown"]
    );
}

#[test]
fn failed_open_cleanup_without_session_owner_is_not_marked_recovered() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let error = AppError::from_source(
        "game.runtime",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试运行态连接未能建立",
        std::io::Error::other("fixture probe open and cleanup failure"),
    )
    .with_context("component", "game")
    .with_context("access", "read_only")
    .with_context("cleanup", "failed")
    .with_context(
        "runtime_journal",
        "data/logs/failed-open-cleanup-fixture.jsonl",
    );
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(FailedOpenGamePort {
            error: Some(error),
            events: Arc::clone(&events),
        })),
        unused_generation(),
    );

    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(error.stage(), "game.runtime");
    assert_eq!(error.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
    assert!(!error.context().contains_key("cleanup_recovered"));
    assert!(!error.context().contains_key("cleanup_initial_attempt"));
    assert!(!error.context().contains_key("cleanup_owner_retained"));
    assert_eq!(
        error.context().get("runtime_journal").map(String::as_str),
        Some("data/logs/failed-open-cleanup-fixture.jsonl")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["plan", "game", "shutdown_noop"]
    );
}

#[test]
fn game_failure_stops_generation_after_layout_read() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
        .expect("空状态应能建立投影");
    let generation = FakeWorkbookGenerationPort::new(
        generation_report(&layout, &projection),
        Arc::clone(&events),
    );
    let mut service = test_sync_service(
        Box::new(FakeWorkbookPort::new(layout, Arc::clone(&events))),
        Some(Box::new(FakeGamePort::failure(
            AppError::from_source(
                "game.read.fixture",
                crate::application::AppErrorCode::FullCheckFailed,
                "测试游戏状态无效",
                std::io::Error::other("fixture game failure"),
            ),
            Arc::clone(&events),
        ))),
        Box::new(generation),
    );

    let error = service.generate_workbook(None, &|| false).unwrap_err();

    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::FullCheckFailed
    );
    assert_eq!(events.lock().unwrap().as_slice(), ["layout", "game"]);
}

#[test]
fn missing_game_port_returns_a_stable_not_ready_error_without_fake_state() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
        .expect("空状态应能建立投影");
    let mut service = test_sync_service(
        Box::new(FakeWorkbookPort::new(layout.clone(), Arc::clone(&events))),
        None,
        Box::new(FakeWorkbookGenerationPort::new(
            generation_report(&layout, &projection),
            Arc::clone(&events),
        )),
    );

    let error = service.generate_workbook(None, &|| false).unwrap_err();

    assert_eq!(error.code(), crate::application::AppErrorCode::GameNotReady);
    assert_eq!(error.stage(), "workbook.generate");
    assert_eq!(
        error.context().get("missing_port").map(String::as_str),
        Some("game")
    );
    assert_eq!(events.lock().unwrap().as_slice(), ["layout"]);
}

#[test]
fn layout_failure_precedes_the_missing_game_port_error() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_sync_service(
        Box::new(FailingWorkbookPort {
            events: Arc::clone(&events),
        }),
        None,
        unused_generation(),
    );

    let error = service.generate_workbook(None, &|| false).unwrap_err();

    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::LayoutInvalid
    );
    assert!(!error.context().contains_key("missing_port"));
    assert_eq!(events.lock().unwrap().as_slice(), ["layout"]);
}

#[test]
fn checks_workbook_plan_with_one_snapshot_and_no_extra_layout_read() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(FakeGamePort::success(
            plan_game_state(),
            Arc::clone(&events),
        ))),
        unused_generation(),
    );
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let report = service.check_workbook_plan(&workbook).unwrap();

    assert_eq!(report.message(), "配装计划检查通过");
    assert!(report.checked_slots() > 0);
    assert_eq!(events.lock().unwrap().as_slice(), ["plan", "game"]);
}

#[test]
fn checks_workbook_plan_without_modifications_skips_game() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let layout = empty_layout();
    let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
        .expect("空状态应能建立投影");
    let mut service = test_check_service(
        Box::new(StubDesiredWorkbookPort {
            desired: DesiredState::new(Vec::new()).unwrap(),
            inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
            events: Arc::clone(&events),
        }),
        Some(Box::new(FakeGamePort::success(
            empty_game_state(),
            Arc::clone(&events),
        ))),
        Box::new(FakeWorkbookGenerationPort::new(
            generation_report(&layout, &projection),
            Arc::clone(&events),
        )),
    );
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let report = service
        .check_workbook_plan(&workbook)
        .expect("工作簿的两类计划输入都为空时应通过检查");

    assert_eq!(report.message(), "修改列没有需要检查的操作");
    assert_eq!(report.checked_slots(), 0);
    assert_eq!(report.checked_inventory_actions(), 0);
    assert_eq!(events.lock().unwrap().as_slice(), ["plan"]);
}

#[test]
fn plan_check_failure_stops_after_game_snapshot() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(FakeGamePort::failure(
            AppError::from_source(
                "game.read.fixture",
                crate::application::AppErrorCode::FullCheckFailed,
                "测试游戏状态无效",
                std::io::Error::other("fixture game failure"),
            ),
            Arc::clone(&events),
        ))),
        unused_generation(),
    );
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::FullCheckFailed
    );
    assert_eq!(error.stage(), "game.read.fixture");
    assert_eq!(events.lock().unwrap().as_slice(), ["plan", "game"]);
}

#[test]
fn missing_game_port_is_scoped_to_plan_check() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        None,
        unused_generation(),
    );
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(error.code(), crate::application::AppErrorCode::GameNotReady);
    assert_eq!(error.stage(), "plan.check");
    assert_eq!(
        error.context().get("missing_port").map(String::as_str),
        Some("game")
    );
    assert_eq!(events.lock().unwrap().as_slice(), ["plan"]);
}

#[test]
fn missing_execution_port_returns_a_stable_capability_error() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let mut service = test_execute_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        None,
    );

    let error = service.execute_workbook(&workbook).unwrap_err();

    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::CapabilityMissing
    );
    assert_eq!(error.stage(), "plan.execute");
    assert_eq!(
        error.context().get("missing_port").map(String::as_str),
        Some("execution")
    );
}

#[test]
fn changed_recompiled_plan_stops_before_sending_commands() {
    let initial_state = plan_game_state();
    let changed_state = with_state_content_digest(&initial_state, '9');
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let service = test_execute_service(
        Box::new(StubDesiredWorkbookPort {
            desired: warehouse_equip_desired_state(),
            inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
            events: Arc::clone(&events),
        }),
        Some(Box::new(KeepOnlyExecutionPort::new(vec![
            initial_state,
            changed_state,
        ]))),
    );
    let mut service = service;
    service.workbook_backup = Box::new(StubWorkbookBackupPort {
        calls: Arc::new(AtomicUsize::new(0)),
    });

    let error = service.execute_workbook(&workbook).unwrap_err();

    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::EquipmentStateChanged
    );
    assert_eq!(error.stage(), "plan.execute.confirmation");
    assert_eq!(events.lock().unwrap().as_slice(), ["plan", "plan"]);
}

#[test]
fn identical_state_on_a_different_target_is_rejected_before_execution() {
    let state = plan_game_state();
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
    let target_a = ExecutionTargetIdentity::new("a".repeat(64)).unwrap();
    let target_b = ExecutionTargetIdentity::new("b".repeat(64)).unwrap();
    let execution =
        KeepOnlyExecutionPort::new(vec![state]).with_target_identities(vec![target_a, target_b]);
    let service = test_execute_service(
        Box::new(StubDesiredWorkbookPort {
            desired: warehouse_equip_desired_state(),
            inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
            events: Arc::clone(&events),
        }),
        Some(Box::new(execution)),
    );
    let mut service = service;
    service.workbook_backup = Box::new(StubWorkbookBackupPort {
        calls: Arc::new(AtomicUsize::new(0)),
    });

    let error = service.execute_workbook(&workbook).unwrap_err();

    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::EquipmentStateChanged
    );
    assert_eq!(error.stage(), "plan.execute.confirmation");
    assert_eq!(
        error
            .context()
            .get("expected_target_fingerprint_sha256")
            .map(String::as_str),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        error
            .context()
            .get("actual_target_fingerprint_sha256")
            .map(String::as_str),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
}

#[test]
fn plan_model_failure_keeps_stage_code_and_object_context() {
    let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut service = test_check_service(
        Box::new(workbook_with_modification(Arc::clone(&events))),
        Some(Box::new(FakeGamePort::success(
            empty_game_state(),
            Arc::clone(&events),
        ))),
        unused_generation(),
    );
    let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));

    let error = service.check_workbook_plan(&workbook).unwrap_err();

    assert_eq!(error.stage(), "plan.check");
    assert_eq!(
        error.code(),
        crate::application::AppErrorCode::EquipmentNotFound
    );
    assert_eq!(
        error.context().get("ship_instance_id").map(String::as_str),
        Some("9001")
    );
    assert_eq!(events.lock().unwrap().as_slice(), ["plan", "game"]);
}

#[test]
fn writeback_preflight_failure_stops_before_game_commands() {
    struct RejectWriteback {
        call: AtomicUsize,
        fail_at: usize,
    }
    impl ExecutionResultsPort for RejectWriteback {
        fn validate_writeback(
            &self,
            _workbook: &WorkbookRef,
            _expected_source_package_sha256: &str,
            _layout: &WorkbookLayout,
            _projection: crate::application::WorkbookProjectionV4,
        ) -> Result<(), AppError> {
            if self.call.fetch_add(1, Ordering::Relaxed) + 1 == self.fail_at {
                Err(AppError::from_source(
                    "execution.workbook.preflight",
                    AppErrorCode::WorkbookInvalid,
                    "科技分类表缺失",
                    std::io::Error::other("missing technology table"),
                ))
            } else {
                Ok(())
            }
        }
        fn write_execution_results(
            &self,
            _workbook: &WorkbookRef,
            _hash: &str,
            _layout: &WorkbookLayout,
            _rows: &[WorkbookProjectionRow],
            _projection: Option<crate::application::WorkbookProjectionV4>,
            _time: i64,
        ) -> Result<ExecutionResultsWriteReport, AppError> {
            panic!("预检失败不应写回")
        }
    }
    for fail_at in [1, 2] {
        let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
        let mut service = ordered_execution_service_with_input(
            Arc::clone(&events),
            OrderedExecutionScenario {
                state: plan_game_state(),
                desired: warehouse_equip_desired_state(),
                send_status: Some(ExecutionStatus::Success),
                expected_row_count: 1,
                backup_fails: false,
                history_fails: false,
                writeback_fails: false,
                shutdown_fails: false,
                drift_on_second_read: false,
            },
        );
        service.execution_results = Box::new(RejectWriteback {
            call: AtomicUsize::new(0),
            fail_at,
        });
        let error = service.execute_workbook(&workbook).unwrap_err();
        assert_eq!(error.stage(), "execution.workbook.preflight");
        let recorded = events.lock().unwrap();
        assert!(!recorded.contains(&"send"));
        assert!(!recorded.contains(&"history"));
        assert!(!recorded.contains(&"writeback"));
        assert_eq!(recorded.contains(&"backup"), fail_at == 2);
        assert_eq!(recorded.last(), Some(&"shutdown"));
    }
}

#[test]
fn workbook_check_reports_real_stages_and_preserves_cleanup_on_failure() {
    use crate::application::OperationProgress as P;
    for (fails, writeback_fails, history_fails) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, false, true),
        (false, true, true),
    ] {
        let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_scope = Arc::new(std::sync::Mutex::new(None));
        let mut service = test_check_service(
            Box::new(StubDesiredWorkbookPort {
                desired: warehouse_equip_desired_state(),
                inventory_plan: EquipmentInventoryPlan::new(Vec::new()).unwrap(),
                events: Arc::clone(&events),
            }),
            Some(Box::new(
                ShutdownTrackingGamePort::new(
                    Some(plan_game_state()),
                    fails.then(|| {
                        AppError::from_source(
                            "game.read.fixture",
                            AppErrorCode::FullCheckFailed,
                            "读取失败",
                            std::io::Error::other("fixture read failure"),
                        )
                    }),
                    None,
                    Arc::clone(&events),
                )
                .with_scope_slot(Arc::clone(&seen_scope)),
            )),
            unused_generation(),
        );
        service.check_history = Box::new(StubCheckHistoryPort {
            events: Arc::clone(&events),
            fails: history_fails,
        });
        service.execution_results = Box::new(OrderedResultsPort {
            events: Arc::clone(&events),
            fails: writeback_fails,
            expected_row_count: 0,
        });
        let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
        let mut progress = Vec::new();
        let result = service.check_and_save_workbook_plan_with_progress(&workbook, &mut |event| {
            progress.push(event)
        });
        let mut expected = vec![
            P::stage("正在读取工作簿计划"),
            P::stage("正在连接游戏并按模板读取状态"),
            P::counted("正在读取舰船详情", 2, 5),
            P::stage("正在结束本次连接"),
        ];
        if fails {
            let crate::application::CheckAndSaveOutcome::CheckFailed { error, writeback } =
                result.expect("读取失败后仍返回检查和写回事实")
            else {
                panic!("读取失败应保留检查失败和写回结果");
            };
            assert_eq!(error.stage(), "game.read.fixture");
            assert_eq!(writeback.is_err(), writeback_fails);
        } else {
            let outcome = result.expect("检查完成后返回保存事实");
            if history_fails {
                let crate::application::CheckAndSaveOutcome::SaveIncomplete {
                    history,
                    writeback,
                    ..
                } = outcome
                else {
                    panic!("历史失败应保留检查通过后的写回事实");
                };
                assert_eq!(history.expect_err("历史失败").stage(), "check.history.save");
                assert_eq!(writeback.is_err(), writeback_fails);
            } else if writeback_fails {
                let crate::application::CheckAndSaveOutcome::SaveIncomplete {
                    history,
                    writeback,
                    ..
                } = outcome
                else {
                    panic!("写回失败应保留已保存的检查记录");
                };
                assert_eq!(
                    history.expect("历史已保存").relative_path(),
                    "data/history/check.json"
                );
                assert_eq!(
                    writeback.expect_err("写回失败").stage(),
                    "check.workbook.writeback"
                );
            } else {
                assert!(matches!(
                    outcome,
                    crate::application::CheckAndSaveOutcome::Saved(_)
                ));
            }
            expected.extend([
                P::stage("正在校验工作簿写回结构"),
                P::stage("正在检查装备来源、数量和合成资源"),
                P::stage("正在保存计划检查记录"),
            ]);
        }
        expected.push(P::stage("正在写入检查结果工作表"));
        assert_eq!(progress, expected);
        let events = events.lock().unwrap();
        assert!(events.contains(&"shutdown"));
        assert_eq!(events.contains(&"history"), !fails);
        assert!(!events.contains(&"layout"));
        assert_eq!(events.iter().filter(|event| **event == "plan").count(), 1);
        assert_eq!(
            *seen_scope.lock().unwrap(),
            Some(empty_layout().read_scope())
        );
    }
}

#[test]
fn workbook_execution_progress_covers_persistence_and_cleanup() {
    for writeback_fails in [false, true] {
        let events: Events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut service = ordered_execution_service_with_input(
            Arc::clone(&events),
            OrderedExecutionScenario {
                state: plan_game_state(),
                desired: warehouse_equip_desired_state(),
                send_status: Some(ExecutionStatus::Success),
                expected_row_count: 1,
                backup_fails: false,
                history_fails: false,
                writeback_fails,
                shutdown_fails: false,
                drift_on_second_read: false,
            },
        );
        let workbook = WorkbookRef::new(PathBuf::from("data/workbooks/plan.xlsx"));
        let mut progress = Vec::new();
        let result = service.execute_workbook_with_progress(
            &workbook,
            &crate::application::NoExecutionCancellation,
            &mut |event| progress.push(event),
        );
        if writeback_fails {
            let WorkbookExecuteOutcome::Finished { writeback, .. } =
                result.expect("写回失败后仍返回执行事实")
            else {
                panic!("写回失败应保留已经结束的执行");
            };
            assert_eq!(
                writeback.failed().expect("写回失败").code(),
                AppErrorCode::ExecutionResultsWriteFailed
            );
        } else {
            assert!(matches!(
                result.expect("执行应收尾完成"),
                WorkbookExecuteOutcome::Completed(_)
            ));
        }
        let messages: Vec<_> = progress
            .iter()
            .map(|event| event.message.as_str())
            .collect();
        let position = |text: &str| {
            messages
                .iter()
                .position(|message| *message == text)
                .unwrap()
        };
        assert!(position("正在备份工作簿") < position("正在预检执行目标、装备状态与资源"));
        assert!(position("正在读取并核验独立最终状态") < position("正在保存执行记录"));
        assert!(position("正在保存执行记录") < position("正在写回装备总表、配装计划与下拉选项"));
        assert_eq!(messages.last(), Some(&"正在结束本次连接"));
        assert_eq!(events.lock().unwrap().last(), Some(&"shutdown"));
    }
}

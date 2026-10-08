//! 覆盖游戏写入端口、动作映射和收据校验的单元测试。

use std::cell::RefCell;
#[cfg(target_os = "windows")]
use std::collections::BTreeSet;
#[cfg(target_os = "windows")]
use std::env;
use std::ffi::OsString;
#[cfg(target_os = "windows")]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use super::{
    GameStateSession, GameStateSessionFactory, PortableGamePort, classify_portable_error,
    classify_runtime_probe_error, find_warehouse_quantity_by_runtime_id,
    full_state_capture_root_from_environment, map_portable_error, map_session_cleanup_error,
    require_write_capability, runtime_action, runtime_client_error_is_not_sent, runtime_context,
    validate_preflight_action,
};
use crate::adapters::device::capabilities::ready_capabilities;
use crate::adapters::device::portable::{PortableMode, PortableProbeError, PortableProbeOptions};
use crate::adapters::device::probe::RuntimeProbeError;
use crate::adapters::device::runtime::{
    AgentError, CapabilitiesResult, ClientStage, EquipmentCommandAction,
    EquipmentCommandActionKind, EquipmentCommandPhase, EquipmentCommandReceipt,
    EquipmentCommandStatus, RequestId, RetryDirective, RuntimeClientError, RuntimeEquipmentCommand,
    SessionEffect,
};
use crate::adapters::settings::Settings;
use crate::adapters::tool_root::ToolRoot;
use crate::application::GamePort;
use crate::application::test_support::{
    empty_game_state, execute_compiled_plan, plan_game_state, plan_game_state_with_compose,
    plan_game_state_with_enhance,
};
use crate::application::{
    AppError, AppErrorCode, ExecutionAction, ExecutionCommand, ExecutionPort, ExecutionSendResult,
    ExecutionTargetIdentity, PlanEquipment, PlanSlot, PlanSource, PlanStep, compile_plan,
    compile_plan_with_inventory,
};
use crate::domain::{
    AccountResources, DesiredEquipment, DesiredSlotState, DesiredState, EnhanceLevel,
    EquipmentConfigId, EquipmentFamilyId, EquipmentInventory, EquipmentInventoryAction,
    EquipmentInventoryActionKind, EquipmentInventoryPlan, EquipmentSourceRef, GameState,
    GameStateSource, ShipInstanceId, ShipProfile, ShipRoster, ShipSlotRef, SlotIndex, SlotTarget,
    SourcePolicy, WarehouseEquipmentStack,
};

#[derive(Default)]
struct SessionStats {
    opens: u32,
    reads: u32,
    shutdowns: u32,
    fallback_drops: u32,
    events: Vec<&'static str>,
}

struct FakeSessionFactory {
    stats: Rc<RefCell<SessionStats>>,
    read_successes_before_failure: Option<u32>,
    shutdown_failure: Option<FakeShutdownFailure>,
}

impl FakeSessionFactory {
    fn new(stats: Rc<RefCell<SessionStats>>, fail_read: bool, fail_shutdown: bool) -> Self {
        Self {
            stats,
            read_successes_before_failure: fail_read.then_some(0),
            shutdown_failure: fail_shutdown.then_some(FakeShutdownFailure::Adb),
        }
    }

    fn failing_after_successful_reads(
        stats: Rc<RefCell<SessionStats>>,
        successful_reads: u32,
        fail_shutdown: bool,
    ) -> Self {
        Self {
            stats,
            read_successes_before_failure: Some(successful_reads),
            shutdown_failure: fail_shutdown.then_some(FakeShutdownFailure::Adb),
        }
    }

    fn with_runtime_cleanup_failure(stats: Rc<RefCell<SessionStats>>) -> Self {
        Self {
            stats,
            read_successes_before_failure: None,
            shutdown_failure: Some(FakeShutdownFailure::Runtime),
        }
    }
}

#[derive(Clone, Copy)]
enum FakeShutdownFailure {
    Adb,
    Runtime,
    Recovered,
}

impl GameStateSessionFactory for FakeSessionFactory {
    fn open(
        &self,
        options: PortableProbeOptions,
        _related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Box<dyn GameStateSession>, PortableProbeError> {
        let mut stats = self.stats.borrow_mut();
        stats.opens += 1;
        stats.events.push("open");
        drop(stats);
        Ok(Box::new(FakeSession {
            stats: Rc::clone(&self.stats),
            options,
            read_successes_before_failure: self.read_successes_before_failure,
            shutdown_failure: self.shutdown_failure,
            shutdown_failures_remaining: u32::from(self.shutdown_failure.is_some()),
            closed: false,
        }))
    }
}

struct FakeSession {
    stats: Rc<RefCell<SessionStats>>,
    options: PortableProbeOptions,
    read_successes_before_failure: Option<u32>,
    shutdown_failure: Option<FakeShutdownFailure>,
    shutdown_failures_remaining: u32,
    closed: bool,
}

impl GameStateSession for FakeSession {
    fn matches_options(&self, options: &PortableProbeOptions) -> bool {
        self.options == *options
    }

    fn read_full_state(&mut self) -> Result<GameState, PortableProbeError> {
        let mut stats = self.stats.borrow_mut();
        stats.reads += 1;
        stats.events.push("read");
        drop(stats);
        if let Some(successes_remaining) = self.read_successes_before_failure.as_mut() {
            if *successes_remaining == 0 {
                return Err(PortableProbeError::Runtime(
                    RuntimeProbeError::InvalidOutput {
                        stage: "fixture.read",
                        message: "固定读取失败".to_owned(),
                    },
                ));
            }
            *successes_remaining -= 1;
        }
        Ok(empty_game_state())
    }

    fn shutdown(&mut self) -> Result<(), PortableProbeError> {
        let mut stats = self.stats.borrow_mut();
        stats.shutdowns += 1;
        stats.events.push("shutdown");
        drop(stats);
        if self.shutdown_failures_remaining > 0 {
            self.shutdown_failures_remaining -= 1;
            return match self
                .shutdown_failure
                .expect("配置了清理失败次数就必须提供错误类型")
            {
                FakeShutdownFailure::Recovered => {
                    self.closed = true;
                    Err(PortableProbeError::RuntimeShutdownFailed {
                        message: "固定卸载失败".to_owned(),
                        journal_path: PathBuf::from("data/logs/shutdown-fixture.jsonl"),
                        game_restarted: true,
                    })
                }
                FakeShutdownFailure::Adb => Err(PortableProbeError::Adb {
                    stage: "adb.cleanup",
                    source: Box::new(io::Error::other("固定清理失败")),
                }),
                FakeShutdownFailure::Runtime => Err(PortableProbeError::Runtime(
                    RuntimeProbeError::ProbeFailed {
                        source: Box::new(RuntimeProbeError::Cleanup {
                            messages: "固定运行态清理失败".to_owned(),
                        }),
                        journal_path: PathBuf::from("data/logs/shutdown-fixture.jsonl"),
                        cleanup_error: None,
                    },
                )),
            };
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        if !self.closed {
            let mut stats = self.stats.borrow_mut();
            stats.fallback_drops += 1;
            stats.events.push("drop");
        }
    }
}

#[derive(Default)]
struct ExecutionSessionStats {
    opens: u32,
    reads: u32,
    executes: u32,
    queries: u32,
    cancels: u32,
    shutdowns: u32,
    commands: Vec<RuntimeEquipmentCommand>,
    command_ids: Vec<String>,
}

struct ExecutionSessionFactory {
    stats: Rc<RefCell<ExecutionSessionStats>>,
    state: GameState,
    capabilities: CapabilitiesResult,
}

impl GameStateSessionFactory for ExecutionSessionFactory {
    fn open(
        &self,
        options: PortableProbeOptions,
        _related: Option<crate::adapters::RelatedLogSink>,
    ) -> Result<Box<dyn GameStateSession>, PortableProbeError> {
        self.stats.borrow_mut().opens += 1;
        Ok(Box::new(ExecutionSession {
            stats: Rc::clone(&self.stats),
            options,
            state: self.state.clone(),
            capabilities: self.capabilities.clone(),
        }))
    }
}

struct ExecutionSession {
    stats: Rc<RefCell<ExecutionSessionStats>>,
    options: PortableProbeOptions,
    state: GameState,
    capabilities: CapabilitiesResult,
}

impl GameStateSession for ExecutionSession {
    fn matches_options(&self, options: &PortableProbeOptions) -> bool {
        self.options == *options
    }

    fn read_full_state(&mut self) -> Result<GameState, PortableProbeError> {
        self.stats.borrow_mut().reads += 1;
        Ok(self.state.clone())
    }

    fn target_scope(&self) -> Result<(String, String), PortableProbeError> {
        Ok(("fixture-device".to_owned(), "fixture.game".to_owned()))
    }

    fn last_capabilities(&self) -> Result<CapabilitiesResult, PortableProbeError> {
        Ok(self.capabilities.clone())
    }

    fn execute_equipment_command(
        &mut self,
        command: &RuntimeEquipmentCommand,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        let mut stats = self.stats.borrow_mut();
        stats.executes += 1;
        stats.commands.push(command.clone());
        Ok(success_receipt(command.command_id(), false))
    }

    fn query_equipment_command(
        &mut self,
        command_id: &str,
        _budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        let mut stats = self.stats.borrow_mut();
        stats.queries += 1;
        stats.command_ids.push(command_id.to_owned());
        Ok(success_receipt(command_id, false))
    }

    fn cancel_equipment_command(
        &mut self,
        command_id: &str,
        _budget: Duration,
    ) -> Result<EquipmentCommandReceipt, PortableProbeError> {
        let mut stats = self.stats.borrow_mut();
        stats.cancels += 1;
        stats.command_ids.push(command_id.to_owned());
        Ok(success_receipt(command_id, true))
    }

    fn shutdown(&mut self) -> Result<(), PortableProbeError> {
        self.stats.borrow_mut().shutdowns += 1;
        Ok(())
    }
}

fn success_receipt(command_id: &str, cancel_requested: bool) -> EquipmentCommandReceipt {
    EquipmentCommandReceipt {
        schema_version: 1,
        command_id: command_id.to_owned(),
        status: EquipmentCommandStatus::Success,
        phase: EquipmentCommandPhase::Succeeded,
        write_dispatched: true,
        cancel_requested,
        observation_count: 1,
        error_code: None,
        message: None,
    }
}

fn ready_write_capabilities() -> CapabilitiesResult {
    let mut capabilities = ready_capabilities();
    for name in [
        "write.equip",
        "write.unequip",
        "write.destroy",
        "write.compose",
        "write.enhance",
    ] {
        let status = capabilities
            .capabilities
            .get_mut(name)
            .expect("测试能力报告必须包含装备写能力");
        status.available = true;
        status.reason_code = "ready".to_owned();
    }
    capabilities
}

fn execution_port(
    root: ToolRoot,
    stats: Rc<RefCell<ExecutionSessionStats>>,
    state: GameState,
    capabilities: CapabilitiesResult,
) -> PortableGamePort {
    PortableGamePort::with_session_factory(
        root,
        Box::new(ExecutionSessionFactory {
            stats,
            state,
            capabilities,
        }),
    )
}

fn desired_slot(slot_index: u8, source: EquipmentSourceRef) -> DesiredState {
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(source),
        None,
    )
    .unwrap();
    DesiredState::new(vec![DesiredSlotState::new(
        ShipSlotRef::new(
            ShipInstanceId::new(9001).unwrap(),
            SlotIndex::new(slot_index).unwrap(),
        ),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap()
}

fn desired_compose_slot(slot_index: u8) -> DesiredState {
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    DesiredState::new(vec![DesiredSlotState::new(
        ShipSlotRef::new(
            ShipInstanceId::new(9001).unwrap(),
            SlotIndex::new(slot_index).unwrap(),
        ),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap()
}

const fn compose_action(quantity: u64) -> ExecutionAction {
    ExecutionAction::Compose {
        recipe_id: 5001,
        equipment: PlanEquipment::from_raw(1000, 1000, 0),
        quantity,
        material_id: 2001,
        material_quantity_per_unit: 5,
        gold_per_unit: 100,
    }
}

fn equip_action(state: &GameState, source: EquipmentSourceRef) -> ExecutionAction {
    let desired = desired_slot(2, source);
    compile_plan(state, &desired)
        .unwrap()
        .plan()
        .steps()
        .iter()
        .cloned()
        .find_map(|step| match step {
            PlanStep::Equip {
                slot,
                source,
                equipment,
                ..
            } => Some(ExecutionAction::Equip {
                slot,
                source,
                equipment,
            }),
            _ => None,
        })
        .expect("测试计划必须包含装上步骤")
}

fn enhance_action(step: &PlanStep) -> ExecutionAction {
    let PlanStep::Enhance {
        source,
        source_equipment,
        target_equipment,
        cost,
        ..
    } = step
    else {
        panic!("测试步骤必须是强化动作");
    };
    ExecutionAction::Enhance {
        source: *source,
        source_equipment: *source_equipment,
        target_equipment: *target_equipment,
        cost: cost.clone(),
    }
}

fn state_after_source_unequip(original: &GameState) -> GameState {
    let original_ship = &original.ships().ships()[0];
    let mut slots = (*original_ship.slots()).clone();
    let source = slots[0].equipment().expect("测试来源槽必须有装备");
    slots[0] = slots[0].with_equipment(None);
    let ship = ShipProfile::new(
        original_ship.identity().clone(),
        original_ship.growth(),
        original_ship.intimacy().clone(),
        original_ship.fleet_memberships().to_vec(),
        original_ship.classification().clone(),
        original_ship.performance().clone(),
        original_ship.skills().to_vec(),
        slots,
    );
    let warehouse: Vec<WarehouseEquipmentStack> = original
        .equipment_inventory()
        .warehouse()
        .iter()
        .copied()
        .map(|stack| {
            if stack.config_id() != source.config_id() {
                return stack;
            }
            WarehouseEquipmentStack::new(
                source.runtime_id(),
                stack.config_id(),
                stack.family_id(),
                stack.enhance_level(),
                stack.quantity() + 1,
            )
        })
        .collect();
    let resources = original.resources();
    let source_metadata = original.source();
    GameState::new(
        GameStateSource::new(
            source_metadata.module_sha256().to_owned(),
            source_metadata.owned_state_schema_version(),
            source_metadata.ship_details_schema_version(),
            source_metadata.ship_catalog_schema_version(),
            source_metadata.equipment_catalog_schema_version(),
            source_metadata.raw_records_schema_version(),
            source_metadata.owned_state_content_sha256().to_owned(),
            source_metadata.ship_roster_content_sha256().to_owned(),
            source_metadata.ship_catalog_content_sha256().to_owned(),
            source_metadata
                .equipment_catalog_content_sha256()
                .to_owned(),
            source_metadata.raw_records_content_sha256().to_owned(),
            "9".repeat(64),
        ),
        ShipRoster::new(original.ships().source().clone(), vec![ship]),
        original.ship_catalog().clone(),
        original.equipment_catalog().clone(),
        original.equipment_details().clone(),
        EquipmentInventory::new(warehouse),
        original.bag().clone(),
        AccountResources::new(
            resources.gold(),
            resources.equipment_capacity() + 1,
            resources.equipment_limit(),
        ),
        original.raw_records().clone(),
    )
}

fn state_after_warehouse_dismantle(original: &GameState) -> GameState {
    let warehouse: Vec<WarehouseEquipmentStack> = original
        .equipment_inventory()
        .warehouse()
        .iter()
        .copied()
        .filter(|stack| stack.config_id().get() != 1000)
        .collect();
    let resources = original.resources();
    let source_metadata = original.source();
    GameState::new(
        GameStateSource::new(
            source_metadata.module_sha256().to_owned(),
            source_metadata.owned_state_schema_version(),
            source_metadata.ship_details_schema_version(),
            source_metadata.ship_catalog_schema_version(),
            source_metadata.equipment_catalog_schema_version(),
            source_metadata.raw_records_schema_version(),
            source_metadata.owned_state_content_sha256().to_owned(),
            source_metadata.ship_roster_content_sha256().to_owned(),
            source_metadata.ship_catalog_content_sha256().to_owned(),
            source_metadata
                .equipment_catalog_content_sha256()
                .to_owned(),
            source_metadata.raw_records_content_sha256().to_owned(),
            "8".repeat(64),
        ),
        original.ships().clone(),
        original.ship_catalog().clone(),
        original.equipment_catalog().clone(),
        original.equipment_details().clone(),
        EquipmentInventory::new(warehouse),
        original.bag().clone(),
        AccountResources::new(
            resources.gold() + 10,
            resources.equipment_capacity() - 1,
            resources.equipment_limit(),
        ),
        original.raw_records().clone(),
    )
}

fn fixture_options(root: &ToolRoot) -> PortableProbeOptions {
    PortableProbeOptions::new(root.as_path().to_path_buf(), PortableMode::Auto)
}

fn fake_port(
    root: ToolRoot,
    stats: Rc<RefCell<SessionStats>>,
    fail_read: bool,
    fail_shutdown: bool,
) -> PortableGamePort {
    PortableGamePort::with_session_factory(
        root,
        Box::new(FakeSessionFactory::new(stats, fail_read, fail_shutdown)),
    )
}

fn fake_port_failing_after_successful_reads(
    root: ToolRoot,
    stats: Rc<RefCell<SessionStats>>,
    successful_reads: u32,
    fail_shutdown: bool,
) -> PortableGamePort {
    PortableGamePort::with_session_factory(
        root,
        Box::new(FakeSessionFactory::failing_after_successful_reads(
            stats,
            successful_reads,
            fail_shutdown,
        )),
    )
}

fn fake_port_with_runtime_cleanup_failure(
    root: ToolRoot,
    stats: Rc<RefCell<SessionStats>>,
) -> PortableGamePort {
    PortableGamePort::with_session_factory(
        root,
        Box::new(FakeSessionFactory::with_runtime_cleanup_failure(stats)),
    )
}

/// 仓库默认设置可以无损映射到生产探针参数，不依赖进程工作目录。
#[test]
fn default_settings_form_valid_portable_options() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let settings: Settings = Settings::load(root.as_path()).unwrap();

    let options: PortableProbeOptions =
        PortableProbeOptions::from_settings(root.as_path().to_path_buf(), &settings).unwrap();

    assert!(options.validate().is_ok());
    let _port: PortableGamePort = PortableGamePort::new(root);
}

/// 同一次未绑定操作沿用开始时的设置；执行绑定后的读取重新打开文件并拒绝连接参数变化。
#[test]
fn operation_settings_stay_cached_until_execution_binding_reloads_the_file() {
    let root_path =
        std::env::temp_dir().join(format!("azlw-operation-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root_path);
    std::fs::create_dir_all(&root_path).unwrap();
    let settings_path = root_path.join("settings.json");
    let initial = include_str!("../../../../settings.json");
    std::fs::write(&settings_path, initial).unwrap();
    let root = ToolRoot::open(&root_path).unwrap();
    let settings = Settings::load(&root_path).unwrap();
    let stats = Rc::new(RefCell::new(SessionStats::default()));
    let mut port =
        fake_port(root, Rc::clone(&stats), false, false).with_operation_settings(settings);

    std::fs::write(
        &settings_path,
        initial.replace(
            "\"connect_timeout_seconds\": 30",
            "\"connect_timeout_seconds\": 60",
        ),
    )
    .unwrap();
    GamePort::read_full_state(&mut port).unwrap();
    GamePort::read_full_state(&mut port).unwrap();
    assert_eq!(stats.borrow().opens, 1);
    assert_eq!(stats.borrow().events, ["open", "read", "read"]);

    ExecutionPort::bind_current_session(&mut port).unwrap();
    let error = GamePort::read_full_state(&mut port).unwrap_err();
    assert_eq!(error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(
        error
            .context()
            .get("configuration_changed")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(stats.borrow().opens, 1);
    assert_eq!(stats.borrow().events, ["open", "read", "read"]);
    std::fs::remove_dir_all(&root_path).unwrap();
}

/// 同一次操作已经失败的设置不再重新打开文件。
#[test]
fn unavailable_operation_settings_keep_the_original_diagnostic() {
    let root_path =
        std::env::temp_dir().join(format!("azlw-unavailable-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root_path);
    std::fs::create_dir_all(&root_path).unwrap();
    let root = ToolRoot::open(&root_path).unwrap();
    let stats = Rc::new(RefCell::new(SessionStats::default()));
    let mut port =
        fake_port(root, stats, false, false).with_unavailable_settings("字段 $ 无效".to_owned());

    let error = GamePort::read_full_state(&mut port).unwrap_err();
    let source = std::error::Error::source(&error).unwrap();

    assert_eq!(error.code(), AppErrorCode::SettingsInvalid);
    assert!(source.to_string().contains("字段 $ 无效"));
    assert!(!root_path.join("settings.json").exists());
    std::fs::remove_dir_all(&root_path).unwrap();
}

/// 生产环境开关缺失或为空时不得启用私人原始状态采集。
#[test]
fn full_state_capture_environment_is_explicit() {
    assert_eq!(full_state_capture_root_from_environment(None), None);
    assert_eq!(
        full_state_capture_root_from_environment(Some(OsString::new())),
        None
    );
    assert_eq!(
        full_state_capture_root_from_environment(Some(OsString::from("capture-root"))),
        Some(PathBuf::from("capture-root"))
    );
}

/// 相同配置的连续读取只建立一个会话，显式关闭保持幂等且不触发兜底析构。
#[test]
fn consecutive_reads_reuse_one_session_and_shutdown_once() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), false, false);

    let first = port.read_with_options(options.clone()).unwrap();
    let second = port.read_with_options(options).unwrap();

    assert_eq!(first.state(), second.state());
    assert_eq!(stats.borrow().opens, 1);
    assert_eq!(stats.borrow().reads, 2);
    port.shutdown_active_session().unwrap();
    port.shutdown_active_session().unwrap();
    drop(port);
    let stats = stats.borrow();
    assert_eq!(stats.shutdowns, 1);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "read", "shutdown"]);
}

#[test]
fn execution_commands_stay_on_the_read_session_and_preserve_local_preconditions() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let state = plan_game_state();
    let stats: Rc<RefCell<ExecutionSessionStats>> =
        Rc::new(RefCell::new(ExecutionSessionStats::default()));
    let mut port = execution_port(
        root,
        Rc::clone(&stats),
        state.clone(),
        ready_write_capabilities(),
    );

    let read = port.read_with_options(options).unwrap();
    let target =
        ExecutionTargetIdentity::from_runtime_scope("fixture-device", "fixture.game", &read)
            .unwrap();
    let command = ExecutionCommand::test_fixture(
        &target,
        &"a".repeat(64),
        1,
        read.source().content_sha256(),
        ExecutionAction::Unequip {
            slot: PlanSlot::from_raw(9001, 1),
        },
    )
    .unwrap();

    let receipt = port.send_command(&command);
    assert!(matches!(receipt, ExecutionSendResult::Receipt(_)));
    assert!(
        port.query_command(command.command_id(), Duration::from_secs(1))
            .is_ok()
    );
    assert!(
        port.cancel_command(command.command_id(), Duration::from_secs(1))
            .is_ok()
    );

    let stats = stats.borrow();
    assert_eq!(stats.opens, 1);
    assert_eq!(stats.reads, 1);
    assert_eq!(stats.executes, 1);
    assert_eq!(stats.queries, 1);
    assert_eq!(stats.cancels, 1);
    assert_eq!(
        stats.command_ids.as_slice(),
        &[
            command.command_id().to_owned(),
            command.command_id().to_owned()
        ]
    );
    let runtime_command = stats.commands.first().expect("应记录一次装备命令");
    assert_eq!(runtime_command.command_id(), command.command_id());
    assert_eq!(
        runtime_command.action().kind(),
        EquipmentCommandActionKind::Unequip
    );
    assert_eq!(runtime_command.action().ship_id(), Some(9001));
    assert_eq!(runtime_command.action().slot_index(), Some(1));
    assert_eq!(
        runtime_command
            .action()
            .target_before()
            .unwrap()
            .equipment_id(),
        1
    );
    assert_eq!(
        runtime_command
            .action()
            .target_before()
            .unwrap()
            .config_id(),
        1000
    );
    assert_eq!(runtime_command.action().equipment_capacity_before(), 3);
    assert_eq!(runtime_command.action().equipment_limit_before(), 300);
}

#[test]
fn command_build_continues_after_planned_dismantle_changes_owned_equipment() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let initial = plan_game_state();
    let stats: Rc<RefCell<ExecutionSessionStats>> =
        Rc::new(RefCell::new(ExecutionSessionStats::default()));
    let mut port = execution_port(root, stats, initial.clone(), ready_write_capabilities());

    let read = port.read_with_options(options).unwrap();
    let target =
        ExecutionTargetIdentity::from_runtime_scope("fixture-device", "fixture.game", &read)
            .unwrap();
    let after_dismantle = state_after_warehouse_dismantle(&initial);
    let action = equip_action(
        &after_dismantle,
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
    );
    port.replace_observed_state(after_dismantle.clone());
    let command = ExecutionCommand::test_fixture(
        &target,
        &"a".repeat(64),
        2,
        after_dismantle.source().content_sha256(),
        action,
    )
    .unwrap();

    let runtime = port.build_runtime_command(&command).unwrap();
    let value = serde_json::to_value(&runtime).unwrap();

    assert_eq!(value["sequence"], 2);
    assert_eq!(runtime.action().kind(), EquipmentCommandActionKind::Equip);
    assert_eq!(
        value["target_fingerprint_sha256"],
        target.fingerprint_sha256()
    );
}

#[test]
fn execution_precondition_check_rejects_disabled_write_capability() {
    let capabilities = ready_capabilities();

    let error = require_write_capability(&capabilities, "write.equip").unwrap_err();

    assert_eq!(error.code(), AppErrorCode::CapabilityMissing);
    assert_eq!(error.stage(), "plan.execute.capability");
    assert_eq!(
        error.context().get("capability").map(String::as_str),
        Some("write.equip")
    );
}

#[test]
fn compose_preflight_requires_the_dedicated_write_capability() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let desired = desired_compose_slot(2);
    let plan = compile_plan(&state, &desired).unwrap();
    let stats: Rc<RefCell<ExecutionSessionStats>> =
        Rc::new(RefCell::new(ExecutionSessionStats::default()));
    let mut capabilities = ready_write_capabilities();
    let compose = capabilities
        .capabilities
        .get_mut("write.compose")
        .expect("测试能力报告必须包含合成写能力");
    compose.available = false;
    compose.reason_code = "disabled_for_fixture".to_owned();
    let mut port = execution_port(root, Rc::clone(&stats), state, capabilities);

    let error = execute_compiled_plan(&mut port, plan.plan()).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::CapabilityMissing);
    assert_eq!(error.stage(), "plan.execute.capability");
    assert_eq!(
        error.context().get("capability").map(String::as_str),
        Some("write.compose")
    );
    assert_eq!(stats.borrow().executes, 0);
}

#[test]
fn enhance_preflight_requires_the_dedicated_write_capability() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        ShipSlotRef::new(
            ShipInstanceId::new(9001).unwrap(),
            SlotIndex::new(1).unwrap(),
        ),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    let plan = compile_plan(&state, &desired).unwrap();
    let stats: Rc<RefCell<ExecutionSessionStats>> =
        Rc::new(RefCell::new(ExecutionSessionStats::default()));
    let mut capabilities = ready_write_capabilities();
    let enhance = capabilities
        .capabilities
        .get_mut("write.enhance")
        .expect("测试能力报告必须包含强化写能力");
    enhance.available = false;
    enhance.reason_code = "disabled_for_fixture".to_owned();
    let mut port = execution_port(root, Rc::clone(&stats), state, capabilities);

    let error = execute_compiled_plan(&mut port, plan.plan()).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::CapabilityMissing);
    assert_eq!(error.stage(), "plan.execute.capability");
    assert_eq!(
        error.context().get("capability").map(String::as_str),
        Some("write.enhance")
    );
    assert_eq!(stats.borrow().executes, 0);
}

#[test]
fn multilevel_ship_enhance_preflight_defers_future_live_state_to_each_command() {
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(2)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        ShipSlotRef::new(
            ShipInstanceId::new(9001).unwrap(),
            SlotIndex::new(1).unwrap(),
        ),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    let plan = compile_plan(&state, &desired).unwrap();
    let second = enhance_action(&plan.plan().steps()[1]);

    validate_preflight_action(&state, second.clone())
        .expect("整批预检应接受由前一级强化产生的未来舰装前态");
    let error = runtime_action(&state, second).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
    assert_eq!(error.stage(), "plan.execute.precondition");
}

#[test]
fn replacement_warehouse_quantity_uses_runtime_group_identity() {
    let state = plan_game_state();

    assert_eq!(find_warehouse_quantity_by_runtime_id(&state, 1), 1);
    assert_eq!(find_warehouse_quantity_by_runtime_id(&state, 1000), 0);
}

#[test]
fn warehouse_equip_maps_the_exact_runtime_stack_and_quantities() {
    let state = plan_game_state();
    let action = equip_action(
        &state,
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1001).unwrap()),
    );

    let runtime = runtime_action(&state, action).unwrap();

    assert_eq!(runtime.kind(), EquipmentCommandActionKind::Equip);
    assert_eq!(runtime.ship_id(), Some(9001));
    assert_eq!(runtime.slot_index(), Some(2));
    assert!(runtime.target_before().is_none());
    let source = runtime.source_before().expect("仓库来源必须存在");
    assert_eq!(source.equipment_id(), 2);
    assert_eq!(source.config_id(), 1001);
    assert_eq!(source.enhance_level(), 1);
    assert_eq!(runtime.source_quantity_before(), 2);
    assert_eq!(runtime.target_warehouse_quantity_before(), 0);
    assert_eq!(runtime.equipment_capacity_before(), 3);
    assert_eq!(runtime.equipment_limit_before(), 300);
}

#[test]
fn warehouse_enhance_maps_exact_transition_resources_and_stack_prestate() {
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(1),
    )
    .unwrap();
    let inventory = EquipmentInventoryPlan::new(vec![action]).unwrap();
    let desired = DesiredState::new(Vec::new()).unwrap();
    let plan = compile_plan_with_inventory(&state, &desired, &inventory).unwrap();
    let action = enhance_action(&plan.plan().steps()[0]);

    let runtime = runtime_action(&state, action).unwrap();

    let EquipmentCommandAction::EnhanceWarehouse {
        source_before,
        source_quantity_before,
        target_config_id,
        target_enhance_level,
        target_before,
        target_warehouse_quantity_before,
        materials,
        gold_before,
        gold_cost,
        equipment_capacity_before,
        equipment_limit_before,
    } = runtime
    else {
        panic!("仓库强化计划必须映射为仓库强化命令");
    };
    assert_eq!(source_before.equipment_id(), 1);
    assert_eq!(source_before.config_id(), 1000);
    assert_eq!(source_before.enhance_level(), 0);
    assert_eq!(source_quantity_before, 1);
    assert_eq!(target_config_id, 1001);
    assert_eq!(target_enhance_level, 1);
    let target = target_before.expect("已有目标配置必须冻结其运行态聚合");
    assert_eq!(target.equipment_id(), 2);
    assert_eq!(target.config_id(), 1001);
    assert_eq!(target.enhance_level(), 1);
    assert_eq!(target_warehouse_quantity_before, 2);
    assert_eq!(materials.len(), 1);
    assert_eq!(materials[0].item_id(), 3001);
    assert_eq!(materials[0].quantity_before(), 10);
    assert_eq!(materials[0].cost(), 2);
    assert_eq!((gold_before, gold_cost), (100, 10));
    assert_eq!(
        (equipment_capacity_before, equipment_limit_before),
        (3, 300)
    );
}

#[test]
fn ship_enhance_maps_exact_slot_transition_and_resources() {
    let state = plan_game_state_with_enhance(100, 10, 3, 300);
    let equipment = DesiredEquipment::new(
        EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        ShipSlotRef::new(
            ShipInstanceId::new(9001).unwrap(),
            SlotIndex::new(1).unwrap(),
        ),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    let plan = compile_plan(&state, &desired).unwrap();
    let action = enhance_action(&plan.plan().steps()[0]);

    let runtime = runtime_action(&state, action).unwrap();

    let EquipmentCommandAction::EnhanceShip {
        ship_id,
        slot_index,
        source_before,
        target_config_id,
        target_enhance_level,
        materials,
        gold_before,
        gold_cost,
        equipment_capacity_before,
        equipment_limit_before,
    } = runtime
    else {
        panic!("当前舰装强化计划必须映射为舰上强化命令");
    };
    assert_eq!((ship_id, slot_index), (9001, 1));
    assert_eq!(source_before.equipment_id(), 1);
    assert_eq!(source_before.config_id(), 1000);
    assert_eq!(source_before.enhance_level(), 0);
    assert_eq!((target_config_id, target_enhance_level), (1001, 1));
    assert_eq!(materials.len(), 1);
    assert_eq!(materials[0].item_id(), 3001);
    assert_eq!(materials[0].quantity_before(), 10);
    assert_eq!(materials[0].cost(), 2);
    assert_eq!((gold_before, gold_cost), (100, 10));
    assert_eq!(
        (equipment_capacity_before, equipment_limit_before),
        (3, 300)
    );
}

#[test]
fn ship_slot_source_resolves_only_after_the_preceding_unequip() {
    let initial = plan_game_state();
    let source_slot = ShipSlotRef::new(
        ShipInstanceId::new(9001).unwrap(),
        SlotIndex::new(1).unwrap(),
    );
    let action = equip_action(&initial, EquipmentSourceRef::ShipSlot(source_slot));

    let before_error = runtime_action(&initial, action.clone()).unwrap_err();
    assert_eq!(before_error.code(), AppErrorCode::EquipmentStateChanged);

    let unequipped = state_after_source_unequip(&initial);
    let runtime = runtime_action(&unequipped, action).unwrap();

    assert_eq!(runtime.kind(), EquipmentCommandActionKind::Equip);
    assert!(runtime.target_before().is_none());
    let source = runtime.source_before().expect("卸下后的仓库来源必须存在");
    assert_eq!(source.equipment_id(), 1);
    assert_eq!(source.config_id(), 1000);
    assert_eq!(source.enhance_level(), 0);
    assert_eq!(runtime.source_quantity_before(), 2);
    assert_eq!(runtime.equipment_capacity_before(), 4);
}

#[test]
fn warehouse_dismantle_maps_the_exact_runtime_stack_and_quantity() {
    let state = plan_game_state();
    let action = ExecutionAction::Dismantle {
        source: PlanSource::Warehouse { config_id: 1000 },
        equipment: PlanEquipment::from_raw(1000, 1000, 0),
        quantity: 1,
    };

    let runtime = runtime_action(&state, action).unwrap();

    assert_eq!(runtime.kind(), EquipmentCommandActionKind::Dismantle);
    assert_eq!(runtime.ship_id(), None);
    assert_eq!(runtime.slot_index(), None);
    assert!(runtime.target_before().is_none());
    let source = runtime.source_before().expect("拆解来源必须存在");
    assert_eq!(source.equipment_id(), 1);
    assert_eq!(source.config_id(), 1000);
    assert_eq!(source.enhance_level(), 0);
    assert_eq!(runtime.source_quantity_before(), 1);
    assert_eq!(runtime.dismantle_quantity(), Some(1));
    assert_eq!(runtime.equipment_capacity_before(), 3);
    assert_eq!(runtime.equipment_limit_before(), 300);
}

#[test]
fn compose_maps_the_exact_recipe_resources_and_output_prestate() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));

    let runtime = runtime_action(&state, compose_action(2)).unwrap();

    match runtime {
        EquipmentCommandAction::Compose {
            recipe_id,
            compose_quantity,
            output_config_id,
            output_before,
            output_quantity_before,
            material_id,
            material_quantity_before,
            material_quantity_per_unit,
            gold_before,
            gold_per_unit,
            equipment_capacity_before,
            equipment_limit_before,
        } => {
            assert_eq!(recipe_id, 5001);
            assert_eq!(compose_quantity, 2);
            assert_eq!(output_config_id, 1000);
            let output = output_before.expect("已有同配置产物必须冻结其运行态身份");
            assert_eq!(output.equipment_id(), 1);
            assert_eq!(output.config_id(), 1000);
            assert_eq!(output.enhance_level(), 0);
            assert_eq!(output_quantity_before, 1);
            assert_eq!(material_id, 2001);
            assert_eq!(material_quantity_before, 20);
            assert_eq!(material_quantity_per_unit, 5);
            assert_eq!(gold_before, 1_000);
            assert_eq!(gold_per_unit, 100);
            assert_eq!(equipment_capacity_before, 3);
            assert_eq!(equipment_limit_before, 300);
        }
        _ => panic!("合成计划必须映射为合成运行态动作"),
    }
}

#[test]
fn compose_rejects_changed_material_or_gold_before_command_creation() {
    let state = plan_game_state_with_compose(199, 9, 3, 300, Some(4));

    let error = runtime_action(&state, compose_action(2)).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
    assert_eq!(error.stage(), "plan.execute.precondition");
    assert_eq!(
        error.context().get("material_required").map(String::as_str),
        Some("10")
    );
    assert_eq!(
        error.context().get("gold_required").map(String::as_str),
        Some("200")
    );
}

#[test]
fn compose_rejects_changed_equipment_capacity_before_command_creation() {
    let state = plan_game_state_with_compose(1_000, 20, 3, 4, Some(4));

    let error = runtime_action(&state, compose_action(2)).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
    assert_eq!(error.stage(), "plan.execute.precondition");
    assert_eq!(
        error.context().get("capacity_before").map(String::as_str),
        Some("3")
    );
    assert_eq!(
        error.context().get("capacity_limit").map(String::as_str),
        Some("4")
    );
}

#[test]
fn ship_dismantle_resolves_only_after_the_preceding_unequip() {
    let initial = plan_game_state();
    let action = ExecutionAction::Dismantle {
        source: PlanSource::ShipSlot {
            ship_instance_id: 9001,
            slot_index: 1,
        },
        equipment: PlanEquipment::from_raw(1000, 1000, 0),
        quantity: 1,
    };

    let before_error = runtime_action(&initial, action.clone()).unwrap_err();
    assert_eq!(before_error.code(), AppErrorCode::EquipmentStateChanged);

    let unequipped = state_after_source_unequip(&initial);
    let runtime = runtime_action(&unequipped, action).unwrap();

    assert_eq!(runtime.kind(), EquipmentCommandActionKind::Dismantle);
    let source = runtime.source_before().expect("卸下后的拆解来源必须存在");
    assert_eq!(source.equipment_id(), 1);
    assert_eq!(source.config_id(), 1000);
    assert_eq!(runtime.source_quantity_before(), 2);
    assert_eq!(runtime.dismantle_quantity(), Some(1));
    assert_eq!(runtime.equipment_capacity_before(), 4);
}

#[test]
fn enhanced_dismantle_is_rejected_before_runtime_command_creation() {
    let state = plan_game_state();
    let action = ExecutionAction::Dismantle {
        source: PlanSource::Warehouse { config_id: 1001 },
        equipment: PlanEquipment::from_raw(1000, 1001, 1),
        quantity: 1,
    };

    let error = runtime_action(&state, action).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
    assert_eq!(error.stage(), "plan.execute.precondition");
    assert!(error.message().contains("安全边界"));
}

#[test]
fn transport_classification_distinguishes_unsent_from_uncertain_dispatch() {
    assert!(runtime_client_error_is_not_sent(
        &RuntimeClientError::SessionUnusable
    ));
    assert!(runtime_client_error_is_not_sent(&RuntimeClientError::Io {
        stage: ClientStage::ConfigureSocket,
        source: io::Error::other("固定套接字配置失败"),
    }));
    assert!(!runtime_client_error_is_not_sent(&RuntimeClientError::Io {
        stage: ClientStage::WriteRequest,
        source: io::Error::other("固定请求写入失败"),
    }));
    assert!(!runtime_client_error_is_not_sent(&RuntimeClientError::Io {
        stage: ClientStage::ReadResponse,
        source: io::Error::other("固定响应读取失败"),
    }));

    let agent_error = |session_effect| RuntimeClientError::Agent {
        request_id: RequestId::FIRST,
        error: AgentError {
            code: "fixture_error".to_owned(),
            stage: "equipment.execute".to_owned(),
            message: "固定设备端错误".to_owned(),
            retry: RetryDirective::Never,
            session_effect,
            details: Default::default(),
        },
    };
    assert!(runtime_client_error_is_not_sent(&agent_error(
        SessionEffect::Unchanged
    )));
    assert!(!runtime_client_error_is_not_sent(&agent_error(
        SessionEffect::StateUnknown
    )));
}

/// 影响连接契约的设置变化必须先关闭旧会话，再建立并读取新会话。
#[test]
fn changed_options_shutdown_old_session_before_opening_new_one() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let first_options: PortableProbeOptions = fixture_options(&root);
    let second_options: PortableProbeOptions = fixture_options(&root)
        .with_connect_timeout_seconds(30)
        .unwrap();
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), false, false);

    port.read_with_options(first_options).unwrap();
    port.read_with_options(second_options).unwrap();

    assert_eq!(
        stats.borrow().events,
        ["open", "read", "shutdown", "open", "read"]
    );
    port.shutdown_active_session().unwrap();
    let stats = stats.borrow();
    assert_eq!(stats.opens, 2);
    assert_eq!(stats.shutdowns, 2);
    assert_eq!(stats.fallback_drops, 0);
}

/// 执行绑定使用完整选项身份；相同配置可复读，任一配置漂移都不得自动换会话。
#[test]
fn bound_execution_session_rejects_configuration_drift_without_reopening() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let first_options: PortableProbeOptions = fixture_options(&root);
    let changed_options: PortableProbeOptions = fixture_options(&root)
        .with_connect_timeout_seconds(30)
        .unwrap();
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), false, false);

    port.read_with_options(first_options.clone()).unwrap();
    ExecutionPort::bind_current_session(&mut port).unwrap();
    port.read_with_options(first_options).unwrap();
    let error = port.read_with_options(changed_options).unwrap_err();

    assert_eq!(error.stage(), "plan.execute.session");
    assert_eq!(error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(
        error
            .context()
            .get("configuration_changed")
            .map(String::as_str),
        Some("true")
    );
    assert!(port.has_session());
    assert!(port.is_execution_bound());
    assert_eq!(stats.borrow().events, ["open", "read", "read"]);

    GamePort::shutdown_session(&mut port).unwrap();
    let stats = stats.borrow();
    assert_eq!(stats.opens, 1);
    assert_eq!(stats.reads, 2);
    assert_eq!(stats.shutdowns, 1);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "read", "shutdown"]);
}

/// 绑定会话的读取和首次清理都失败后，任何后续读取都不得在显式收口前换会话。
#[test]
fn bound_failed_session_blocks_reconnect_until_explicit_cleanup() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let changed_options: PortableProbeOptions = fixture_options(&root)
        .with_connect_timeout_seconds(30)
        .unwrap();
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort =
        fake_port_failing_after_successful_reads(root, Rc::clone(&stats), 1, true);

    port.read_with_options(options.clone()).unwrap();
    ExecutionPort::bind_current_session(&mut port).unwrap();
    let initial_error = port.read_with_options(options.clone()).unwrap_err();
    assert_eq!(initial_error.stage(), "game.runtime");
    assert_eq!(
        initial_error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );

    let same_options_error = port.read_with_options(options).unwrap_err();
    assert_eq!(same_options_error.stage(), "plan.execute.session");
    assert_eq!(
        same_options_error
            .context()
            .get("session_unusable")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        same_options_error
            .context()
            .get("automatic_reconnect")
            .map(String::as_str),
        Some("blocked")
    );

    let changed_options_error = port.read_with_options(changed_options).unwrap_err();
    assert_eq!(changed_options_error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(
        changed_options_error
            .context()
            .get("configuration_changed")
            .map(String::as_str),
        Some("true")
    );
    assert!(port.has_session());
    assert!(port.is_execution_bound());
    assert_eq!(stats.borrow().events, ["open", "read", "read", "shutdown"]);

    GamePort::shutdown_session(&mut port).unwrap();
    drop(port);
    let stats = stats.borrow();
    assert_eq!(stats.opens, 1);
    assert_eq!(stats.reads, 2);
    assert_eq!(stats.shutdowns, 2);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(
        stats.events,
        ["open", "read", "read", "shutdown", "shutdown"]
    );
}

/// 读取错误立即消费并清理失效会话，后续幂等关闭不得重复执行清理。
#[test]
fn read_failure_closes_and_forgets_active_session() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), true, false);

    let error: AppError = port.read_with_options(options).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
    assert!(!port.has_session());
    port.shutdown_active_session().unwrap();
    let stats = stats.borrow();
    assert_eq!(stats.shutdowns, 1);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "shutdown"]);
}

/// 读取和清理同时失败时保留失效会话，下一次清理成功前不得新建连接。
#[test]
fn read_and_cleanup_failure_retains_owner_for_retry() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), true, true);

    let error: AppError = port.read_with_options(options).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
    assert_eq!(error.stage(), "game.runtime");
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        error.context().get("cleanup_stage").map(String::as_str),
        Some("game.cleanup")
    );
    assert_eq!(
        error.context().get("cleanup_code").map(String::as_str),
        Some("RUNTIME_BOOTSTRAP_FAILED")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_owner_retained")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_adapter_stage")
            .map(String::as_str),
        Some("adb.cleanup")
    );
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<PortableProbeError>())
        .expect("应用错误必须保留便携会话双重失败");
    match source {
        PortableProbeError::OperationAndSessionCleanup { operation, cleanup } => {
            assert!(matches!(
                operation.downcast_ref::<PortableProbeError>(),
                Some(PortableProbeError::Runtime(
                    RuntimeProbeError::InvalidOutput {
                        stage: "fixture.read",
                        ..
                    }
                ))
            ));
            assert!(matches!(
                cleanup.as_ref(),
                PortableProbeError::Adb {
                    stage: "adb.cleanup",
                    ..
                }
            ));
        }
        other => panic!("预期持久会话双重失败，实际为 {other:?}"),
    }
    assert!(port.has_session());
    assert!(port.cleanup_pending());
    port.shutdown_active_session().unwrap();
    assert!(!port.has_session());
    let stats = stats.borrow();
    assert_eq!(stats.opens, 1);
    assert_eq!(stats.shutdowns, 2);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "shutdown", "shutdown"]);
}

/// 应用端口的显式关闭保留失败标记和所有权，下一次调用可完成同一会话的清理。
#[test]
fn game_port_shutdown_failure_is_retryable_without_fallback_drop() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort =
        fake_port_with_runtime_cleanup_failure(root, Rc::clone(&stats));
    port.read_with_options(options).unwrap();
    ExecutionPort::bind_current_session(&mut port).unwrap();

    let error = GamePort::shutdown_session(&mut port).unwrap_err();

    assert_eq!(error.stage(), "game.cleanup");
    assert_eq!(error.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert_eq!(error.message(), "游戏运行态会话清理未完整确认");
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        error.context().get("runtime_journal").map(String::as_str),
        Some("data/logs/shutdown-fixture.jsonl")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_owner_retained")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        error.context().get("access").map(String::as_str),
        Some("read_only")
    );
    assert!(port.has_session());
    assert!(port.is_execution_bound());
    assert!(port.cleanup_pending());

    GamePort::shutdown_session(&mut port).unwrap();
    assert!(!port.is_execution_bound());
    drop(port);
    let stats = stats.borrow();
    assert_eq!(stats.shutdowns, 2);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "shutdown", "shutdown"]);
}

/// 设置解析失败时立即关闭已有会话，避免配置错误期间继续占用 ADB 和 agent。
#[test]
fn configuration_error_closes_existing_session() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), false, false);
    port.read_with_options(options).unwrap();
    let operation: AppError = AppError::from_source(
        "game.settings",
        AppErrorCode::SettingsInvalid,
        "固定设置错误",
        io::Error::other("固定设置解析失败"),
    );

    let error: AppError = port.close_active_after_error(operation);

    assert_eq!(error.code(), AppErrorCode::SettingsInvalid);
    assert!(!port.has_session());
    let stats = stats.borrow();
    assert_eq!(stats.shutdowns, 1);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "shutdown"]);
}

/// 设置和会话清理同时失败时保留设置错误分类，并附带可重试的清理状态。
#[test]
fn configuration_and_cleanup_failure_preserve_primary_error() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), false, true);
    port.read_with_options(options).unwrap();
    let operation: AppError = AppError::from_source(
        "game.settings",
        AppErrorCode::SettingsInvalid,
        "固定设置错误",
        io::Error::other("固定设置解析失败"),
    );

    let error: AppError = port.close_active_after_error(operation);

    assert_eq!(error.code(), AppErrorCode::SettingsInvalid);
    assert_eq!(error.stage(), "game.settings");
    assert_eq!(error.message(), "固定设置错误");
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<PortableProbeError>())
        .expect("应用错误必须保留设置与清理双重失败");
    match source {
        PortableProbeError::OperationAndSessionCleanup { operation, cleanup } => {
            assert!(operation.downcast_ref::<AppError>().is_some());
            assert!(matches!(
                cleanup.as_ref(),
                PortableProbeError::Adb {
                    stage: "adb.cleanup",
                    ..
                }
            ));
        }
        other => panic!("预期持久会话双重失败，实际为 {other:?}"),
    }
    assert!(port.has_session());
    port.shutdown_active_session().unwrap();
    assert!(!port.has_session());
    let stats = stats.borrow();
    assert_eq!(stats.shutdowns, 2);
    assert_eq!(stats.fallback_drops, 0);
    assert_eq!(stats.events, ["open", "read", "shutdown", "shutdown"]);
}

/// 在 Windows 发布目录上验证驻留与卸载各三轮会话始终保留同一游戏进程。
#[cfg(target_os = "windows")]
#[test]
#[ignore = "需要 MuMu12、已登录游戏和完整 Windows 发布目录"]
fn live_repeated_sessions_preserve_game_process() {
    let root_path: PathBuf = env::var_os("AZLW_LIVE_TOOL_ROOT")
        .map(PathBuf::from)
        .expect("必须通过 AZLW_LIVE_TOOL_ROOT 指定完整发布目录");
    let root = ToolRoot::open(&root_path).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(
        &fs::read(root.existing_file(Path::new("settings.json")).unwrap()).unwrap(),
    )
    .unwrap();
    let mut process_instances = Vec::new();
    let mut schema_version = None;
    let mut module_sha256: Option<String> = None;
    // 固定内存设置分别验证驻留复用和逐轮卸载，不修改用户配置。
    for unload_after_sync in [false, true] {
        document["runtime"]["unload_after_sync"] = unload_after_sync.into();
        let settings =
            Settings::from_slice(&root, &serde_json::to_vec(&document).unwrap()).unwrap();
        let before = runtime_journals(&root_path);
        for cycle in 0..3 {
            eprintln!("live session: unload_after_sync={unload_after_sync}, cycle={cycle}");
            let mut port =
                PortableGamePort::new(root.clone()).with_operation_settings(settings.clone());
            port.prepare_synchronization();
            let state = port.read_full_state().unwrap();
            match schema_version {
                Some(expected) => assert_eq!(state.schema_version(), expected),
                None => schema_version = Some(state.schema_version()),
            }
            match module_sha256.as_deref() {
                Some(expected) => assert_eq!(state.source().module_sha256(), expected),
                None => module_sha256 = Some(state.source().module_sha256().to_owned()),
            }
            if cycle < 2 {
                port.shutdown_session().unwrap();
            } else {
                drop(port);
            }
        }
        let after = runtime_journals(&root_path);
        let created: Vec<_> = after.difference(&before).collect();
        assert_eq!(created.len(), 3, "三次独立会话应分别创建运行日志");
        for path in created {
            let events = runtime_journal_events(path);
            assert_eq!(stage_count(&events, "session.start"), 1);
            assert_eq!(stage_count(&events, "session.read.complete"), 1);
            let close_stage = if unload_after_sync {
                "session.shutdown.unloaded"
            } else {
                "session.detached"
            };
            assert_eq!(stage_count(&events, close_stage), 1);
            assert_eq!(
                stage_count(&events, "cleanup.complete"),
                usize::from(unload_after_sync)
            );
            let closed = events
                .iter()
                .find(|event| event["stage"] == close_stage)
                .unwrap();
            let pid = closed["details"]["process_id"].as_u64().unwrap();
            let start = closed["details"]["process_start_time"].as_u64().unwrap();
            if unload_after_sync {
                let cleanup = events
                    .iter()
                    .find(|event| event["stage"] == "cleanup.complete")
                    .unwrap();
                assert_eq!(cleanup["details"]["old_process_stopped"], false);
                assert_eq!(cleanup["details"]["game_restarted"], false);
                assert_eq!(cleanup["details"]["process"]["process_id"], pid);
                assert_eq!(cleanup["details"]["process"]["process_start_time"], start);
            } else {
                assert_eq!(closed["details"]["resident_record_verified"], true);
                assert_eq!(closed["details"]["forward_list_restored"], true);
                assert_eq!(stage_count(&events, "session.shutdown.unloaded"), 0);
            }
            process_instances.push((pid, start));
        }
    }
    assert!(
        process_instances
            .iter()
            .all(|identity| *identity == process_instances[0])
    );
}
#[cfg(target_os = "windows")]
fn runtime_journal_events(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line: &str| serde_json::from_str(line).unwrap())
        .collect()
}

#[cfg(target_os = "windows")]
fn stage_count(events: &[serde_json::Value], stage: &str) -> usize {
    events
        .iter()
        .filter(|event| event["stage"] == stage)
        .count()
}

#[cfg(target_os = "windows")]
fn runtime_journals(root: &Path) -> BTreeSet<PathBuf> {
    let directory: PathBuf = root.join("data/logs");
    match fs::read_dir(&directory) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().path())
            .filter(|path: &PathBuf| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        matches!(
                            crate::adapters::log_catalog::classify_file_name(name),
                            Some((_, crate::application::LogRecordKind::RuntimeProbe))
                        )
                    })
            })
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => panic!("读取运行态日志目录 {} 失败: {error}", directory.display()),
    }
}

/// 调用方未显式关闭时，会话对象的析构路径仍接管唯一一次兜底释放。
#[test]
fn dropping_port_releases_active_session_through_fallback() {
    let root: ToolRoot = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options: PortableProbeOptions = fixture_options(&root);
    let stats: Rc<RefCell<SessionStats>> = Rc::new(RefCell::new(SessionStats::default()));
    let mut port: PortableGamePort = fake_port(root, Rc::clone(&stats), false, false);

    port.read_with_options(options).unwrap();
    drop(port);

    let stats = stats.borrow();
    assert_eq!(stats.shutdowns, 0);
    assert_eq!(stats.fallback_drops, 1);
    assert_eq!(stats.events, ["open", "read", "drop"]);
}

/// 发现、歧义、ADB 和兼容性错误保持彼此独立的稳定应用错误码。
#[test]
fn portable_failures_keep_stable_application_categories() {
    let cases = [
        (
            PortableProbeError::Discovery {
                message: "没有实例".to_owned(),
            },
            AppErrorCode::EmulatorNotFound,
        ),
        (
            PortableProbeError::AmbiguousManager {
                message: "存在两个管理器".to_owned(),
            },
            AppErrorCode::EmulatorManagerAmbiguous,
        ),
        (
            PortableProbeError::AmbiguousTarget {
                message: "存在两个实例".to_owned(),
            },
            AppErrorCode::EmulatorInstanceAmbiguous,
        ),
        (
            PortableProbeError::Adb {
                stage: "adb.target",
                source: Box::new(io::Error::other("目标离线")),
            },
            AppErrorCode::AdbTargetUnavailable,
        ),
        (
            PortableProbeError::Adb {
                stage: "adb.start",
                source: Box::new(io::Error::other("服务启动失败")),
            },
            AppErrorCode::RuntimeBootstrapFailed,
        ),
        (
            PortableProbeError::ProfilePackageMismatch {
                configured: "com.example.wrong".to_owned(),
                profile: "com.bilibili.azurlane".to_owned(),
            },
            AppErrorCode::SettingsInvalid,
        ),
        (
            PortableProbeError::IncompatibleTarget {
                message: "ABI 不匹配".to_owned(),
            },
            AppErrorCode::RuntimeIncompatible,
        ),
    ];

    for (source, expected) in cases {
        assert_eq!(classify_portable_error(&source), expected);
    }
}

/// 探针收尾包装不会丢失完整读取阶段、用户说明、对象上下文或日志位置。
#[test]
fn nested_application_error_survives_probe_failure_wrapping() {
    let application: AppError = AppError::from_source(
        "game.read.capabilities",
        AppErrorCode::CapabilityMissing,
        "运行态缺少完整状态读取能力",
        io::Error::other("fixture capability failure"),
    )
    .with_context("capability", "read.ship_details");
    let runtime = RuntimeProbeError::ProbeFailed {
        source: Box::new(RuntimeProbeError::GameStateRead(application)),
        journal_path: PathBuf::from("data/logs/runtime-fixture.jsonl"),
        cleanup_error: None,
    };

    let error: AppError = map_portable_error(PortableProbeError::Runtime(runtime));

    assert_eq!(error.code(), AppErrorCode::CapabilityMissing);
    assert_eq!(error.stage(), "game.read.capabilities");
    assert_eq!(error.message(), "运行态缺少完整状态读取能力");
    assert_eq!(
        error.context().get("capability").map(String::as_str),
        Some("read.ship_details")
    );
    assert_eq!(
        error.context().get("runtime_journal").map(String::as_str),
        Some("data/logs/runtime-fixture.jsonl")
    );
}

/// 未就绪提示不能覆盖探针内部清理失败，且没有会话所有权时不得伪造可重试标记。
#[test]
fn game_not_ready_preserves_unowned_probe_cleanup_failure() {
    let runtime = RuntimeProbeError::ProbeFailed {
        source: Box::new(RuntimeProbeError::InvalidOutput {
            stage: "rpc.wait_bag_proxy",
            message: "固定 BagProxy 未就绪".to_owned(),
        }),
        journal_path: PathBuf::from("data/logs/not-ready-fixture.jsonl"),
        cleanup_error: Some("固定探针清理失败".to_owned()),
    };
    let source = PortableProbeError::GameNotReady {
        detail: "运行态日志：data/logs/not-ready-fixture.jsonl".to_owned(),
        source: Box::new(runtime),
    };

    let error = map_portable_error(source);

    assert_eq!(error.code(), AppErrorCode::GameNotReady);
    assert_eq!(error.stage(), "game.runtime");
    assert_eq!(error.message(), "请启动碧蓝航线、完成登录并进入港区后重试");
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        error.context().get("runtime_journal").map(String::as_str),
        Some("data/logs/not-ready-fixture.jsonl")
    );
    assert_eq!(
        error.context().get("adapter_stage").map(String::as_str),
        Some("rpc.wait_bag_proxy")
    );
    assert!(!error.context().contains_key("cleanup_owner_retained"));
}

/// 显式关闭映射既保留日志路径，也必须使用稳定清理阶段和用户说明。
#[test]
fn shutdown_cleanup_failure_exposes_cleanup_context() {
    let runtime = RuntimeProbeError::ProbeFailed {
        source: Box::new(RuntimeProbeError::Cleanup {
            messages: "固定清理失败".to_owned(),
        }),
        journal_path: PathBuf::from("data/logs/shutdown-fixture.jsonl"),
        cleanup_error: None,
    };

    let error: AppError = map_session_cleanup_error(PortableProbeError::Runtime(runtime));

    assert_eq!(error.code(), AppErrorCode::RuntimeBootstrapFailed);
    assert_eq!(error.stage(), "game.cleanup");
    assert_eq!(error.message(), "游戏运行态会话清理未完整确认");
    assert_eq!(
        error.context().get("runtime_journal").map(String::as_str),
        Some("data/logs/shutdown-fixture.jsonl")
    );
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("failed")
    );
}

/// 卸载器结构化失败在应用层保留阶段、稳定码、退出码和目标 PID。
#[test]
fn unloader_rejection_exposes_structured_context() {
    let source = RuntimeProbeError::UnloaderRejected {
        exit_code: 11,
        code: "unload_identity_changed".to_owned(),
        message: "读取目标身份失败".to_owned(),
        process_id: 42,
    };

    assert_eq!(
        runtime_context(&source),
        vec![
            ("adapter_stage".to_owned(), "unloader.execute".to_owned()),
            (
                "runtime_code".to_owned(),
                "unload_identity_changed".to_owned(),
            ),
            ("exit_code".to_owned(), "11".to_owned()),
            ("process_id".to_owned(), "42".to_owned()),
        ]
    );
}

/// 加载器结构化失败在应用层保留恢复决策所需的全部稳定字段。
#[test]
fn loader_rejection_exposes_structured_context() {
    let source = RuntimeProbeError::LoaderRejected {
        exit_code: 15,
        code: "ptrace_detach_failed".to_owned(),
        message: "最后分离主线程失败".to_owned(),
        process_id: 42,
        target_state: "unknown".to_owned(),
    };

    assert_eq!(
        classify_runtime_probe_error(&source),
        AppErrorCode::RuntimeBootstrapFailed
    );
    assert_eq!(
        runtime_context(&source),
        vec![
            ("adapter_stage".to_owned(), "loader.execute".to_owned()),
            ("runtime_code".to_owned(), "ptrace_detach_failed".to_owned(),),
            ("exit_code".to_owned(), "15".to_owned()),
            ("process_id".to_owned(), "42".to_owned()),
            ("target_state".to_owned(), "unknown".to_owned()),
        ]
    );
}

/// 恢复资源后仍保留卸载失败，但端口不得重复清理已经释放的会话。
#[test]
fn recovered_shutdown_reports_failure_and_releases_session() {
    let root = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let options = fixture_options(&root);
    let stats = Rc::new(RefCell::new(SessionStats::default()));
    let mut port = fake_port(root, Rc::clone(&stats), false, false);
    port.session_factory = Box::new(FakeSessionFactory {
        stats: Rc::clone(&stats),
        read_successes_before_failure: None,
        shutdown_failure: Some(FakeShutdownFailure::Recovered),
    });
    port.read_with_options(options).unwrap();
    let error = GamePort::shutdown_session(&mut port).unwrap_err();
    assert_eq!(error.stage(), "game.cleanup");
    assert_eq!(
        error.context().get("cleanup").map(String::as_str),
        Some("recovered")
    );
    assert_eq!(
        error
            .context()
            .get("cleanup_owner_retained")
            .map(String::as_str),
        Some("false")
    );
    assert_eq!(
        error.context().get("game_restarted").map(String::as_str),
        Some("true")
    );
    assert!(!port.has_session());
    assert!(!port.cleanup_pending());
    GamePort::shutdown_session(&mut port).unwrap();
    drop(port);
    assert_eq!(stats.borrow().shutdowns, 1);
    assert_eq!(stats.borrow().fallback_drops, 0);
}

#[test]
fn selected_instance_reaches_the_session_without_changing_settings() {
    let root = ToolRoot::open(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let settings_bytes = std::fs::read(root.as_path().join("settings.json")).unwrap();
    let settings = Settings::load(root.as_path()).unwrap();
    for instance in ["7", "12"] {
        let expected = PortableProbeOptions::from_settings(root.as_path().to_path_buf(), &settings)
            .unwrap()
            .with_retain_agent(true)
            .with_selected_instance(instance);
        let stats = Rc::new(RefCell::new(SessionStats::default()));
        let mut port = fake_port(root.clone(), Rc::clone(&stats), false, false)
            .with_selected_instance(instance.to_owned());
        port.read_full_state().unwrap();
        assert_eq!(port.active_options(), Some(&expected));
        port.read_full_state().unwrap();
        assert_eq!(stats.borrow().opens, 1);
        port.shutdown_active_session().unwrap();
    }
    assert_eq!(
        std::fs::read(root.as_path().join("settings.json")).unwrap(),
        settings_bytes
    );
}

#[test]
fn unloading_preference_applies_only_to_synchronization() {
    let path = PathBuf::from(
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .unwrap(),
    )
    .join("suzushiro/scratch/azlw-sync-retention")
    .join(
        super::super::session::SessionId::generate()
            .unwrap()
            .to_string(),
    );
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("settings.json"),
        include_str!("../../../../settings.json"),
    )
    .unwrap();
    let root = ToolRoot::open(&path).unwrap();
    let stats = Rc::new(RefCell::new(SessionStats::default()));
    let mut port = fake_port(root.clone(), stats, false, false);
    for unload_after_sync in [false, true] {
        Settings::save_preferences(
            &path,
            Settings::load(&path).unwrap().preferences(),
            crate::application::UserPreferences {
                unload_after_sync,
                ..Default::default()
            },
        )
        .unwrap();
        let settings = Settings::load(&path).unwrap();
        let expected =
            PortableProbeOptions::from_settings(root.as_path().to_path_buf(), &settings).unwrap();
        port.prepare_synchronization();
        port.read_full_state().unwrap();
        assert_eq!(
            port.active_options(),
            Some(&expected.clone().with_retain_agent(!unload_after_sync))
        );
        port.shutdown_session().unwrap();
        port.read_full_state().unwrap();
        assert_eq!(
            port.active_options(),
            Some(&expected.with_retain_agent(true))
        );
        port.shutdown_session().unwrap();
    }
    drop(port);
    std::fs::remove_dir_all(path).unwrap();
}

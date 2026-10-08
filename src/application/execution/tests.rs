//! 覆盖执行状态机、预检、收据和读回证据的单元测试。

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use super::preflight::expected_execution;
use super::state_machine::{execute_plan, resolve_unknown_receipt};
use super::{
    ExecutionAction, ExecutionCancellation, ExecutionCommand, ExecutionCommandReceipt,
    ExecutionEquipmentState, ExecutionFinalVerificationStatus, ExecutionPort, ExecutionPreflight,
    ExecutionReportStatus, ExecutionSendResult, ExecutionStatus, ExecutionStopReason,
    ExecutionTargetIdentity, ExecutionWriteEffect,
};
use crate::application::test_support::{
    plan_game_state, plan_game_state_with_compose, plan_game_state_with_compose_and_enhance,
    plan_game_state_with_enhance, plan_game_state_with_ship_count,
};
use crate::application::{
    AppError, AppErrorCode, GamePort, PlanStep, compile_plan, compile_plan_with_inventory,
};
use crate::domain::{
    AccountResources, BagInventory, BagItem, DesiredEquipment, DesiredSlotState, DesiredState,
    EnhanceLevel, EquipmentConfigId, EquipmentInventory, EquipmentInventoryAction,
    EquipmentInventoryActionKind, EquipmentInventoryPlan, EquipmentSourceRef, GameState,
    GameStateSource, ShipEquipment, ShipIdentity, ShipInstanceId, ShipProfile, ShipRoster,
    ShipSlotRef, SlotIndex, SlotTarget, SourcePolicy, WarehouseEquipmentStack,
};

enum PortOutcome {
    Receipt {
        status: ExecutionStatus,
        wrong_command_id: bool,
        diagnostics: BTreeMap<String, String>,
    },
    Error(AppError),
}

impl PortOutcome {
    fn status(status: ExecutionStatus) -> Self {
        Self::Receipt {
            status,
            wrong_command_id: false,
            diagnostics: BTreeMap::new(),
        }
    }

    fn phase(status: ExecutionStatus, phase: &str) -> Self {
        Self::Receipt {
            status,
            wrong_command_id: false,
            diagnostics: BTreeMap::from([("phase".to_owned(), phase.to_owned())]),
        }
    }

    fn wrong_id(status: ExecutionStatus) -> Self {
        Self::Receipt {
            status,
            wrong_command_id: true,
            diagnostics: BTreeMap::new(),
        }
    }
}

struct FakeExecutionPort {
    reads: VecDeque<Result<GameState, AppError>>,
    last_state: Option<GameState>,
    read_count: usize,
    read_scopes: Vec<crate::domain::GameReadScope>,
    sends: VecDeque<PortOutcome>,
    queries: VecDeque<PortOutcome>,
    cancels: VecDeque<PortOutcome>,
    commands: Vec<ExecutionCommand>,
    queried_command_ids: Vec<String>,
    query_budgets: Vec<Duration>,
    block_for_query_budget: bool,
    cancelled_command_ids: Vec<String>,
    preflight_count: usize,
    preflight_error: Option<AppError>,
    target_identity: ExecutionTargetIdentity,
}

impl FakeExecutionPort {
    fn new(states: Vec<GameState>, sends: Vec<PortOutcome>) -> Self {
        Self {
            reads: states.into_iter().map(Ok).collect(),
            last_state: None,
            read_count: 0,
            read_scopes: Vec::new(),
            sends: sends.into(),
            queries: VecDeque::new(),
            cancels: VecDeque::new(),
            commands: Vec::new(),
            queried_command_ids: Vec::new(),
            query_budgets: Vec::new(),
            block_for_query_budget: false,
            cancelled_command_ids: Vec::new(),
            preflight_count: 0,
            preflight_error: None,
            target_identity: ExecutionTargetIdentity::new("a".repeat(64)).unwrap(),
        }
    }

    fn respond(
        outcome: PortOutcome,
        command_id: &str,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        match outcome {
            PortOutcome::Receipt {
                status,
                wrong_command_id,
                diagnostics,
            } => Ok(ExecutionCommandReceipt::new(
                if wrong_command_id {
                    "f".repeat(64)
                } else {
                    command_id.to_owned()
                },
                status,
                Some(format!("fixture {}", status.as_str())),
                None,
                None,
                diagnostics,
            )),
            PortOutcome::Error(error) => Err(error),
        }
    }
}

impl GamePort for FakeExecutionPort {
    fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        _progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<crate::application::GameObservation, AppError> {
        self.read_scopes.push(scope);
        self.read_full_state()
    }

    fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
        self.read_count += 1;
        match self.reads.pop_front() {
            Some(Ok(state)) => {
                self.last_state = Some(state.clone());
                Ok(crate::application::GameObservation::from_state(state))
            }
            Some(Err(error)) => Err(error),
            None => Ok(crate::application::GameObservation::from_state(
                self.last_state
                    .clone()
                    .expect("假执行端口缺少可复用的最终状态"),
            )),
        }
    }
}

impl ExecutionPort for FakeExecutionPort {
    fn bind_current_session(&mut self) -> Result<(), AppError> {
        Ok(())
    }

    fn target_identity(&mut self, _state: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
        Ok(self.target_identity.clone())
    }

    fn preflight_plan(&mut self, preflight: &ExecutionPreflight) -> Result<(), AppError> {
        self.preflight_count += 1;
        assert_eq!(preflight.target_identity(), &self.target_identity);
        assert!(preflight.initial_state_content_sha256().len() == 64);
        if let Some(error) = self.preflight_error.take() {
            return Err(error);
        }
        Ok(())
    }

    fn send_command(&mut self, command: &ExecutionCommand) -> ExecutionSendResult {
        self.commands.push(command.clone());
        match Self::respond(
            self.sends
                .pop_front()
                .expect("假执行端口缺少预期的发送结果"),
            command.command_id(),
        ) {
            Ok(receipt) => ExecutionSendResult::Receipt(receipt),
            Err(error) => ExecutionSendResult::NotSent(error),
        }
    }

    fn query_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        self.queried_command_ids.push(command_id.to_owned());
        self.query_budgets.push(budget);
        if self.block_for_query_budget {
            std::thread::sleep(budget);
        }
        Self::respond(
            self.queries
                .pop_front()
                .expect("假执行端口缺少预期的查询结果"),
            command_id,
        )
    }

    fn cancel_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        let _ = budget;
        self.cancelled_command_ids.push(command_id.to_owned());
        Self::respond(
            self.cancels
                .pop_front()
                .expect("假执行端口缺少预期的取消结果"),
            command_id,
        )
    }
}

struct CancelAfterFirstBoundary {
    checks: Cell<usize>,
}

impl ExecutionCancellation for CancelAfterFirstBoundary {
    fn is_cancelled(&self) -> bool {
        let checks = self.checks.get();
        self.checks.set(checks + 1);
        checks >= 1
    }
}

struct CancelOnlyDuringResolution {
    checks: Cell<usize>,
}

impl ExecutionCancellation for CancelOnlyDuringResolution {
    fn is_cancelled(&self) -> bool {
        let checks = self.checks.get();
        self.checks.set(checks + 1);
        checks == 1
    }
}

struct CancelAfterObservationQuery {
    checks: Cell<usize>,
}

impl ExecutionCancellation for CancelAfterObservationQuery {
    fn is_cancelled(&self) -> bool {
        let checks = self.checks.get();
        self.checks.set(checks + 1);
        checks >= 2
    }
}

struct NeverCancel;

impl ExecutionCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn slot(ship_instance_id: u64, slot_index: u8) -> ShipSlotRef {
    ShipSlotRef::new(
        ShipInstanceId::new(ship_instance_id).unwrap(),
        SlotIndex::new(slot_index).unwrap(),
    )
}

fn warehouse_equip_plan(state: &GameState) -> crate::application::CompiledPlan {
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::Warehouse(
            EquipmentConfigId::new(1001).unwrap(),
        )),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    compile_plan(state, &desired).unwrap().plan().clone()
}

fn ship_move_plan(state: &GameState) -> crate::application::CompiledPlan {
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ExactSource,
        Some(EquipmentSourceRef::ShipSlot(slot(9001, 1))),
        None,
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    compile_plan(state, &desired).unwrap().plan().clone()
}

fn keep_plan(state: &GameState) -> crate::application::CompiledPlan {
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 1),
        SlotTarget::Keep,
        0,
    )])
    .unwrap();
    compile_plan(state, &desired).unwrap().plan().clone()
}

fn warehouse_dismantle_plan(state: &GameState) -> crate::application::CompiledPlan {
    let desired = DesiredState::new(Vec::new()).unwrap();
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Dismantle,
        Some(1),
        None,
        None,
    )
    .unwrap();
    let inventory = EquipmentInventoryPlan::new(vec![action]).unwrap();
    compile_plan_with_inventory(state, &desired, &inventory)
        .unwrap()
        .plan()
        .clone()
}

fn compose_plan(state: &GameState) -> crate::application::CompiledPlan {
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    compile_plan(state, &desired).unwrap().plan().clone()
}

fn compose_enhance_plan(state: &GameState) -> crate::application::CompiledPlan {
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 2),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    compile_plan(state, &desired).unwrap().plan().clone()
}

fn warehouse_enhance_plan(state: &GameState) -> crate::application::CompiledPlan {
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(1),
    )
    .unwrap();
    let inventory = EquipmentInventoryPlan::new(vec![action]).unwrap();
    compile_plan_with_inventory(state, &DesiredState::new(Vec::new()).unwrap(), &inventory)
        .unwrap()
        .plan()
        .clone()
}

fn current_ship_enhance_plan(
    state: &GameState,
    target_level: u8,
) -> crate::application::CompiledPlan {
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip,
        None,
        Some(EnhanceLevel::new(target_level)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![DesiredSlotState::new(
        slot(9001, 1),
        SlotTarget::Equipment(equipment),
        0,
    )])
    .unwrap();
    compile_plan(state, &desired).unwrap().plan().clone()
}

fn state_after(
    original: &GameState,
    digest_byte: char,
    slot_updates: &[(u8, Option<(u64, u8)>)],
    warehouse_updates: &[(u64, u64)],
) -> GameState {
    let original_ship = &original.ships().ships()[0];
    let mut slots = (*original_ship.slots()).clone();
    for (slot_index, equipment) in slot_updates {
        let offset = usize::from(*slot_index - 1);
        slots[offset] =
            slots[offset].with_equipment(equipment.map(|(config_id, enhance_level)| {
                ShipEquipment::new(
                    900_000 + u64::from(*slot_index),
                    EquipmentConfigId::new(config_id).unwrap(),
                    EnhanceLevel::new(enhance_level),
                )
            }));
    }
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
            let quantity = warehouse_updates
                .iter()
                .find(|(config_id, _)| *config_id == stack.config_id().get())
                .map(|(_, quantity)| *quantity)
                .unwrap_or_else(|| stack.quantity());
            WarehouseEquipmentStack::new(
                stack.runtime_group_id(),
                stack.config_id(),
                stack.family_id(),
                stack.enhance_level(),
                quantity,
            )
        })
        .collect();
    let equipment_capacity = warehouse
        .iter()
        .try_fold(0_u64, |total, stack| total.checked_add(stack.quantity()));
    let equipment_capacity = equipment_capacity.expect("测试仓库数量不应溢出");
    let original_resources = original.resources();
    let source = original.source();
    let digest = digest_byte.to_string().repeat(64);
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
            digest.clone(),
        ),
        ShipRoster::new(original.ships().source().clone(), vec![ship]),
        original.ship_catalog().clone(),
        original.equipment_catalog().clone(),
        original.equipment_details().clone(),
        EquipmentInventory::new(warehouse),
        original.bag().clone(),
        AccountResources::new(
            original_resources.gold(),
            equipment_capacity,
            original_resources.equipment_limit(),
        ),
        original.raw_records().clone(),
    )
}

fn state_with_resource_updates(
    original: &GameState,
    gold: u64,
    material_updates: &[(u64, u64)],
) -> GameState {
    let mut items: BTreeMap<u64, BagItem> = original
        .bag()
        .items()
        .iter()
        .cloned()
        .map(|item| (item.item_id(), item))
        .collect();
    for &(item_id, quantity) in material_updates {
        let (name, compose) = items
            .get(&item_id)
            .map(|item| (item.name().to_owned(), item.compose()))
            .unwrap_or_else(|| (format!("测试材料 {item_id}"), None));
        items.insert(item_id, BagItem::new(item_id, quantity, name, compose));
    }
    let resources = original.resources();
    GameState::new(
        original.source().clone(),
        original.ships().clone(),
        original.ship_catalog().clone(),
        original.equipment_catalog().clone(),
        original.equipment_details().clone(),
        original.equipment_inventory().clone(),
        BagInventory::new(items.into_values().collect()),
        AccountResources::new(
            gold,
            resources.equipment_capacity(),
            resources.equipment_limit(),
        ),
        original.raw_records().clone(),
    )
}

fn state_with_ship_config(original: &GameState, config_id: u64) -> GameState {
    let original_ship = &original.ships().ships()[0];
    let ship = ShipProfile::new(
        ShipIdentity::new(
            original_ship.identity().instance_id(),
            config_id,
            original_ship.identity().name().to_owned(),
            original_ship.identity().create_time(),
        ),
        original_ship.growth(),
        original_ship.intimacy().clone(),
        original_ship.fleet_memberships().to_vec(),
        original_ship.classification().clone(),
        original_ship.performance().clone(),
        original_ship.skills().to_vec(),
        (*original_ship.slots()).clone(),
    );
    GameState::new(
        original.source().clone(),
        ShipRoster::new(original.ships().source().clone(), vec![ship]),
        original.ship_catalog().clone(),
        original.equipment_catalog().clone(),
        original.equipment_details().clone(),
        original.equipment_inventory().clone(),
        original.bag().clone(),
        original.resources(),
        original.raw_records().clone(),
    )
}

fn fixture_error(stage: &'static str, code: AppErrorCode, message: &'static str) -> AppError {
    AppError::from_source(
        stage,
        code,
        message,
        std::io::Error::other("fixture failure"),
    )
}

#[test]
fn status_values_match_the_workbook_contract() {
    assert_eq!(ExecutionStatus::Success.as_str(), "success");
    assert_eq!(ExecutionStatus::Failed.as_str(), "failed");
    assert_eq!(ExecutionStatus::Unknown.as_str(), "unknown");
    assert_eq!(ExecutionStatus::NotExecuted.as_str(), "not_executed");
    assert_eq!(ExecutionReportStatus::Success.as_str(), "success");
    assert_eq!(ExecutionReportStatus::Failed.as_str(), "failed");
    assert_eq!(ExecutionReportStatus::Unknown.as_str(), "unknown");
    assert_eq!(ExecutionReportStatus::Cancelled.as_str(), "cancelled");
}

#[test]
fn target_identity_rejects_a_noncanonical_fingerprint() {
    let error = ExecutionTargetIdentity::new("ABC").unwrap_err();

    assert_eq!(error.code(), AppErrorCode::RuntimeIncompatible);
    assert_eq!(error.stage(), "execution.target_identity");
    assert_eq!(
        error
            .context()
            .get("fingerprint_length")
            .map(String::as_str),
        Some("3")
    );
}

#[test]
fn target_identity_wire_shape_remains_stable() {
    let identity = ExecutionTargetIdentity::new("a".repeat(64)).unwrap();

    assert_eq!(
        serde_json::to_value(identity).unwrap(),
        serde_json::json!({ "fingerprint_sha256": "a".repeat(64) })
    );
}

#[test]
fn target_identity_is_stable_across_equipment_locations_and_runtime_ids() {
    let initial = plan_game_state();
    let moved_to_warehouse = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let runtime_id_changed = state_after(&initial, '3', &[(1, Some((1000, 0)))], &[]);

    let expected =
        ExecutionTargetIdentity::from_runtime_scope("device-1", "game.package", &initial).unwrap();
    let moved = ExecutionTargetIdentity::from_runtime_scope(
        "device-1",
        "game.package",
        &moved_to_warehouse,
    )
    .unwrap();
    let runtime_changed = ExecutionTargetIdentity::from_runtime_scope(
        "device-1",
        "game.package",
        &runtime_id_changed,
    )
    .unwrap();

    assert_eq!(moved, expected);
    assert_eq!(runtime_changed, expected);
}

#[test]
fn target_identity_is_stable_across_planned_equipment_changes() {
    let initial = plan_game_state();
    let extra_equipment = state_after(&initial, '2', &[], &[(1001, 3)]);
    let dismantled_equipment = state_after(&initial, '3', &[], &[(1000, 0)]);
    let expected =
        ExecutionTargetIdentity::from_runtime_scope("device-1", "game.package", &initial).unwrap();

    assert_eq!(
        ExecutionTargetIdentity::from_runtime_scope("device-1", "game.package", &extra_equipment,)
            .unwrap(),
        expected
    );
    assert_eq!(
        ExecutionTargetIdentity::from_runtime_scope(
            "device-1",
            "game.package",
            &dismantled_equipment,
        )
        .unwrap(),
        expected
    );
}

#[test]
fn target_identity_changes_with_device_package_or_ship_roster() {
    let initial = plan_game_state();
    let replaced_ship = state_with_ship_config(&initial, 2);
    let expected =
        ExecutionTargetIdentity::from_runtime_scope("device-1", "game.package", &initial).unwrap();

    assert_ne!(
        ExecutionTargetIdentity::from_runtime_scope("device-2", "game.package", &initial,).unwrap(),
        expected
    );
    assert_ne!(
        ExecutionTargetIdentity::from_runtime_scope("device-1", "other.package", &initial,)
            .unwrap(),
        expected
    );
    assert_ne!(
        ExecutionTargetIdentity::from_runtime_scope("device-1", "game.package", &replaced_ship,)
            .unwrap(),
        expected
    );
}

#[test]
fn command_id_is_deterministic_and_covers_sequence_state_and_action() {
    let state = plan_game_state();
    let plan = warehouse_equip_plan(&state);
    let step = plan.steps()[0].clone();
    let action = ExecutionAction::from_step(&step).unwrap();
    let target = ExecutionTargetIdentity::new("a".repeat(64)).unwrap();
    let other_target = ExecutionTargetIdentity::new("b".repeat(64)).unwrap();
    let first = ExecutionCommand::new(&target, "a", 1, "b", action.clone()).unwrap();
    let same = ExecutionCommand::new(&target, "a", 1, "b", action.clone()).unwrap();
    let other_sequence = ExecutionCommand::new(&target, "a", 2, "b", action.clone()).unwrap();
    let other_state = ExecutionCommand::new(&target, "a", 1, "c", action.clone()).unwrap();
    let other_target_command = ExecutionCommand::new(&other_target, "a", 1, "b", action).unwrap();
    let other_action = ExecutionCommand::new(
        &target,
        "a",
        1,
        "b",
        match step {
            PlanStep::Equip { slot, .. } => ExecutionAction::Unequip { slot },
            _ => unreachable!(),
        },
    )
    .unwrap();

    assert_eq!(first.command_id(), same.command_id());
    assert_ne!(first.command_id(), other_sequence.command_id());
    assert_ne!(first.command_id(), other_state.command_id());
    assert_ne!(first.command_id(), other_target_command.command_id());
    assert_ne!(first.command_id(), other_action.command_id());
    assert_eq!(
        first.command_id(),
        "1764e9847944c091686ac480c33e556e4fcea20453af5aae95c698996485b3d2"
    );
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        concat!(
            "{\"schema_version\":4,\"target_fingerprint_sha256\":\"",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",",
            "\"plan_hash\":\"a\",\"sequence\":1,",
            "\"pre_state_content_sha256\":\"b\",\"action\":{\"equip\":{",
            "\"slot\":{\"ship_instance_id\":9001,\"slot_index\":2},",
            "\"source\":{\"Warehouse\":{\"config_id\":1001}},",
            "\"equipment\":{\"family_id\":1000,\"config_id\":1001,",
            "\"enhance_level\":1}}},\"command_id\":",
            "\"1764e9847944c091686ac480c33e556e4fcea20453af5aae95c698996485b3d2\"}"
        )
    );
}

#[test]
fn changed_initial_state_stops_before_any_command() {
    let original = plan_game_state();
    let plan = warehouse_equip_plan(&original);
    let changed = state_after(&original, '9', &[], &[]);
    let mut port = FakeExecutionPort::new(vec![changed], Vec::new());

    let error = execute_plan(&mut port, &plan, &NeverCancel).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
    assert_eq!(error.stage(), "plan.execute.precondition");
    assert!(port.commands.is_empty());
}

#[test]
fn batch_preflight_failure_stops_before_any_command() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mut port = FakeExecutionPort::new(vec![initial], Vec::new());
    port.preflight_error = Some(fixture_error(
        "execution.preflight.fixture",
        AppErrorCode::CapabilityMissing,
        "测试运行态不支持整批写步骤",
    ));

    let error = execute_plan(&mut port, &plan, &NeverCancel).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::CapabilityMissing);
    assert_eq!(error.stage(), "execution.preflight.fixture");
    assert_eq!(port.preflight_count, 1);
    assert!(port.commands.is_empty());
}

#[test]
fn keep_step_sends_nothing_and_still_reads_an_independent_final_state() {
    let state = plan_game_state();
    let plan = keep_plan(&state);
    let mut port = FakeExecutionPort::new(vec![state], Vec::new());

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert!(report.steps()[0].command_id().is_none());
    assert!(port.commands.is_empty());
    assert!(port.reads.is_empty());
    assert_eq!(port.read_count, 2);
    assert_eq!(port.preflight_count, 1);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Verified
    );
}

#[test]
fn completed_steps_become_unknown_when_the_independent_final_read_fails() {
    let state = plan_game_state();
    let plan = keep_plan(&state);
    let mut port = FakeExecutionPort::new(vec![state], Vec::new());
    port.reads.push_back(Err(fixture_error(
        "game.final_read.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试独立终态读取失败",
    )));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(
        report.stop_reason(),
        ExecutionStopReason::FinalReadbackFailed
    );
    assert_eq!(report.final_state_content_sha256(), None);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Unavailable
    );
}

#[test]
fn warehouse_equip_requires_target_and_quantity_readback() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(
        report.final_state_content_sha256(),
        Some(&"2".repeat(64)[..])
    );
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("已装备配置 1001")
    );
    assert_eq!(port.commands.len(), 1);
}

#[test]
fn warehouse_dismantle_verifies_equipment_gold_and_materials() {
    let initial = plan_game_state();
    let plan = warehouse_dismantle_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 0)]);
    let post = state_with_resource_updates(&equipment_post, 10, &[(2001, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    let step = &report.steps()[0];
    assert_eq!(step.status(), ExecutionStatus::Success);
    let evidence = step.readback_evidence().unwrap();
    assert!(evidence.matches_expected());
    assert!(evidence.target_slot().is_none());
    assert_eq!(evidence.dismantled_source_quantity(), Some(1));
    assert_eq!(evidence.warehouse()[0].before_quantity(), 1);
    assert_eq!(evidence.warehouse()[0].actual_after_quantity(), 0);
    let gold = evidence.gold().unwrap();
    assert_eq!(
        (gold.before(), gold.expected_after(), gold.actual_after()),
        (0, 10, 10)
    );
    assert_eq!(evidence.materials().len(), 1);
    let material = evidence.materials()[0];
    assert_eq!(material.item_id(), 2001);
    assert_eq!(
        (
            material.before_quantity(),
            material.expected_after_quantity(),
            material.actual_after_quantity(),
        ),
        (0, 2, 2)
    );
    assert!(
        step.readback_summary()
            .unwrap()
            .contains("物资增加 10，材料 [2001:+2]")
    );
}

#[test]
fn observing_dismantle_receipt_is_polled_until_success_without_resending() {
    let initial = plan_game_state();
    let plan = warehouse_dismantle_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 0)]);
    let post = state_with_resource_updates(&equipment_post, 10, &[(2001, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::phase(ExecutionStatus::Unknown, "observing")],
    );
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Unknown, "observing"));
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Success, "succeeded"));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 2);
    let step = &report.steps()[0];
    assert_eq!(step.status(), ExecutionStatus::Success);
    assert!(step.readback_evidence().unwrap().matches_expected());
    assert_eq!(
        step.diagnostics().get("resolution.query_count"),
        Some(&"2".to_owned())
    );
    assert_eq!(
        step.diagnostics().get("resolution.observing_query_count"),
        Some(&"1".to_owned())
    );
    assert_eq!(
        step.diagnostics().get("initial.phase"),
        Some(&"observing".to_owned())
    );
    assert_eq!(
        step.diagnostics().get("phase"),
        Some(&"succeeded".to_owned())
    );
}

#[test]
fn compose_verifies_output_gold_and_materials_before_equipping() {
    let initial = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let plan = compose_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 2)]);
    let compose_post = state_with_resource_updates(&equipment_post, 900, &[(2001, 15)]);
    let equip_post = state_after(&compose_post, '3', &[(2, Some((1000, 0)))], &[(1000, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, compose_post, equip_post],
        vec![
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(port.commands.len(), 2);
    assert!(matches!(
        port.commands[0].action(),
        ExecutionAction::Compose {
            recipe_id: 5001,
            quantity: 1,
            material_id: 2001,
            material_quantity_per_unit: 5,
            gold_per_unit: 100,
            ..
        }
    ));
    assert!(matches!(
        port.commands[1].action(),
        ExecutionAction::Equip {
            source: super::PlanSource::Compose { recipe_id: 5001 },
            ..
        }
    ));
    let evidence = report.steps()[0].readback_evidence().unwrap();
    assert!(evidence.matches_expected());
    assert_eq!(evidence.composed_output_quantity(), Some(1));
    assert_eq!(evidence.dismantled_source_quantity(), None);
    assert_eq!(evidence.warehouse()[0].config_id(), 1000);
    assert_eq!(evidence.warehouse()[0].before_quantity(), 1);
    assert_eq!(evidence.warehouse()[0].actual_after_quantity(), 2);
    let gold = evidence.gold().unwrap();
    assert_eq!(
        (gold.before(), gold.expected_after(), gold.actual_after()),
        (1_000, 900, 900)
    );
    let material = evidence.materials()[0];
    assert_eq!(material.item_id(), 2001);
    assert_eq!(
        (
            material.before_quantity(),
            material.expected_after_quantity(),
            material.actual_after_quantity(),
        ),
        (20, 15, 15)
    );
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("物资消耗 100，材料 [2001:-5]")
    );
}

#[test]
fn composed_equipment_is_enhanced_and_equipped_with_each_state_verified() {
    let initial = plan_game_state_with_compose_and_enhance(1_000, 20, 10, 3, 300, Some(4));
    let plan = compose_enhance_plan(&initial);
    let composed_equipment = state_after(&initial, '2', &[], &[(1000, 2)]);
    let compose_post = state_with_resource_updates(&composed_equipment, 900, &[(2001, 15)]);
    let enhanced_equipment = state_after(&compose_post, '3', &[], &[(1000, 1), (1001, 3)]);
    let enhance_post = state_with_resource_updates(&enhanced_equipment, 890, &[(3001, 8)]);
    let equip_post = state_after(&enhance_post, '4', &[(2, Some((1001, 1)))], &[(1001, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![
            initial,
            compose_post,
            enhance_post,
            equip_post.clone(),
            equip_post,
        ],
        vec![
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(report.verified_write_count(), 3);
    assert_eq!(port.commands.len(), 3);
    assert!(matches!(
        port.commands[0].action(),
        ExecutionAction::Compose {
            recipe_id: 5001,
            quantity: 1,
            ..
        }
    ));
    assert!(matches!(
        port.commands[1].action(),
        ExecutionAction::Enhance {
            source: super::PlanSource::Warehouse { config_id: 1000 },
            source_equipment,
            target_equipment,
            ..
        } if source_equipment.config_id() == 1000
            && target_equipment.config_id() == 1001
    ));
    assert!(matches!(
        port.commands[2].action(),
        ExecutionAction::Equip {
            source: super::PlanSource::Warehouse { config_id: 1001 },
            ..
        }
    ));
    assert_eq!(
        report.steps()[0]
            .readback_evidence()
            .unwrap()
            .composed_output_quantity(),
        Some(1)
    );
    assert_eq!(
        report.steps()[1]
            .readback_evidence()
            .unwrap()
            .enhanced_target_quantity(),
        Some(1)
    );
    assert!(
        report
            .steps()
            .iter()
            .all(|step| step.readback_evidence().unwrap().matches_expected())
    );
}

#[test]
fn warehouse_enhance_verifies_source_target_gold_and_materials() {
    let initial = plan_game_state_with_enhance(100, 10, 3, 300);
    let plan = warehouse_enhance_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 0), (1001, 3)]);
    let post = state_with_resource_updates(&equipment_post, 90, &[(3001, 8)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post.clone(), post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(port.commands.len(), 1);
    assert!(matches!(
        port.commands[0].action(),
        ExecutionAction::Enhance {
            source: super::PlanSource::Warehouse { config_id: 1000 },
            source_equipment,
            target_equipment,
            ..
        } if source_equipment.config_id() == 1000
            && source_equipment.enhance_level() == 0
            && target_equipment.config_id() == 1001
            && target_equipment.enhance_level() == 1
    ));
    let evidence = report.steps()[0].readback_evidence().unwrap();
    assert!(evidence.matches_expected());
    assert_eq!(evidence.enhanced_target_quantity(), Some(1));
    assert_eq!(evidence.composed_output_quantity(), None);
    assert_eq!(evidence.dismantled_source_quantity(), None);
    assert_eq!(
        evidence
            .warehouse()
            .iter()
            .map(|row| {
                (
                    row.config_id(),
                    row.before_quantity(),
                    row.expected_after_quantity(),
                    row.actual_after_quantity(),
                )
            })
            .collect::<Vec<_>>(),
        vec![(1000, 1, 0, 0), (1001, 2, 3, 3)]
    );
    let gold = evidence.gold().unwrap();
    assert_eq!(
        (gold.before(), gold.expected_after(), gold.actual_after()),
        (100, 90, 90)
    );
    let material = evidence.materials()[0];
    assert_eq!(material.item_id(), 3001);
    assert_eq!(
        (
            material.before_quantity(),
            material.expected_after_quantity(),
            material.actual_after_quantity(),
        ),
        (10, 8, 8)
    );
}

#[test]
fn multilevel_ship_enhance_executes_each_adjacent_transition_with_readback() {
    let initial = plan_game_state_with_enhance(100, 10, 3, 300);
    let plan = current_ship_enhance_plan(&initial, 2);
    let first_equipment_post = state_after(&initial, '2', &[(1, Some((1001, 1)))], &[]);
    let first_post = state_with_resource_updates(&first_equipment_post, 90, &[(3001, 8)]);
    let second_equipment_post = state_after(&first_post, '3', &[(1, Some((1002, 2)))], &[]);
    let second_post = state_with_resource_updates(&second_equipment_post, 70, &[(3001, 5)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, first_post, second_post.clone(), second_post],
        vec![
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(report.verified_write_count(), 2);
    assert_eq!(port.commands.len(), 2);
    assert!(matches!(
        port.commands[0].action(),
        ExecutionAction::Enhance {
            source: super::PlanSource::ShipSlot {
                ship_instance_id: 9001,
                slot_index: 1,
            },
            source_equipment,
            target_equipment,
            ..
        } if source_equipment.config_id() == 1000
            && source_equipment.enhance_level() == 0
            && target_equipment.config_id() == 1001
            && target_equipment.enhance_level() == 1
    ));
    assert!(matches!(
        port.commands[1].action(),
        ExecutionAction::Enhance {
            source: super::PlanSource::ShipSlot {
                ship_instance_id: 9001,
                slot_index: 1,
            },
            source_equipment,
            target_equipment,
            ..
        } if source_equipment.config_id() == 1001
            && source_equipment.enhance_level() == 1
            && target_equipment.config_id() == 1002
            && target_equipment.enhance_level() == 2
    ));
    for step in report.steps() {
        let evidence = step.readback_evidence().unwrap();
        assert!(evidence.matches_expected());
        assert_eq!(evidence.enhanced_target_quantity(), Some(1));
        assert!(evidence.warehouse().is_empty());
        assert!(evidence.target_slot().is_some());
        assert!(evidence.source_slot().is_some());
    }
    let final_gold = report.steps()[1]
        .readback_evidence()
        .unwrap()
        .gold()
        .unwrap();
    assert_eq!(
        (
            final_gold.before(),
            final_gold.expected_after(),
            final_gold.actual_after(),
        ),
        (90, 70, 70)
    );
}

#[test]
fn enhance_rejects_readback_that_does_not_consume_materials() {
    let initial = plan_game_state_with_enhance(100, 10, 3, 300);
    let plan = current_ship_enhance_plan(&initial, 1);
    let equipment_post = state_after(&initial, '2', &[(1, Some((1001, 1)))], &[]);
    let mismatched = state_with_resource_updates(&equipment_post, 90, &[(3001, 10)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, mismatched],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert!(matches!(
        report.steps()[0]
            .readback_evidence()
            .unwrap()
            .first_mismatch(),
        Some(super::ExecutionStateMismatch::Material {
            item_id: 3001,
            expected_quantity: 8,
            actual_quantity: 10,
        })
    ));
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("背包材料 3001 数量为 10，期望 8")
    );
}

#[test]
fn split_compose_batches_have_valid_equipment_and_resource_states() {
    let initial = plan_game_state_with_compose(1_000, 20, 3, 4, Some(4));
    let equipment = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(0)),
    )
    .unwrap();
    let desired = DesiredState::new(vec![
        DesiredSlotState::new(
            ShipSlotRef::new(
                ShipInstanceId::new(9001).unwrap(),
                SlotIndex::new(2).unwrap(),
            ),
            SlotTarget::Equipment(equipment),
            0,
        ),
        DesiredSlotState::new(
            ShipSlotRef::new(
                ShipInstanceId::new(9001).unwrap(),
                SlotIndex::new(3).unwrap(),
            ),
            SlotTarget::Equipment(equipment),
            1,
        ),
    ])
    .unwrap();
    let report = compile_plan(&initial, &desired).unwrap();

    let expected = expected_execution(&initial, report.plan()).unwrap();

    assert_eq!(expected.checkpoint_count(), 5);
    let final_equipment = expected.equipment_at(expected.checkpoint_count() - 1);
    assert_eq!(final_equipment.warehouse.get(&1000), Some(&1));
    assert_eq!(
        final_equipment.slots.get(&(9001, 2)),
        Some(&Some(super::ExecutionEquipmentState {
            config_id: 1000,
            enhance_level: 0,
        }))
    );
    assert_eq!(
        final_equipment.slots.get(&(9001, 3)),
        Some(&Some(super::ExecutionEquipmentState {
            config_id: 1000,
            enhance_level: 0,
        }))
    );
    assert_eq!(expected.checkpoint_count(), 5);
    let final_resources = expected.resources_at(expected.checkpoint_count() - 1);
    assert_eq!(final_resources.gold, 800);
    assert_eq!(final_resources.materials.get(&2001), Some(&10));
    assert_eq!(
        (0..expected.checkpoint_count())
            .map(|index| expected.verify_resources_at(index))
            .collect::<Vec<_>>(),
        vec![false, true, true, true, true]
    );
    let steps = expected.steps();
    for window in steps.windows(2) {
        assert!(std::sync::Arc::ptr_eq(
            &window[0].equipment_after,
            &window[1].equipment_before
        ));
        assert!(std::sync::Arc::ptr_eq(
            &window[0].resources_after,
            &window[1].resources_before
        ));
    }
    for step in steps {
        if step.equipment_before == step.equipment_after {
            assert!(std::sync::Arc::ptr_eq(
                &step.equipment_before,
                &step.equipment_after
            ));
        }
        if step.resources_before == step.resources_after {
            assert!(std::sync::Arc::ptr_eq(
                &step.resources_before,
                &step.resources_after
            ));
        }
    }
}

#[test]
fn observing_compose_receipt_is_polled_without_resending_the_recipe() {
    let initial = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let plan = compose_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 2)]);
    let compose_post = state_with_resource_updates(&equipment_post, 900, &[(2001, 15)]);
    let equip_post = state_after(&compose_post, '3', &[(2, Some((1000, 0)))], &[(1000, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, compose_post, equip_post],
        vec![
            PortOutcome::phase(ExecutionStatus::Unknown, "observing"),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Success, "succeeded"));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(port.commands.len(), 2);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert!(matches!(
        port.commands[0].action(),
        ExecutionAction::Compose {
            recipe_id: 5001,
            ..
        }
    ));
    assert!(matches!(
        port.commands[1].action(),
        ExecutionAction::Equip { .. }
    ));
}

#[test]
fn compose_rejects_a_readback_that_does_not_consume_materials() {
    let initial = plan_game_state_with_compose(1_000, 20, 3, 300, Some(4));
    let plan = compose_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 2)]);
    let mismatched = state_with_resource_updates(&equipment_post, 900, &[(2001, 20)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, mismatched],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("背包材料 2001 数量为 20，期望 15")
    );
}

#[test]
fn warehouse_dismantle_rejects_missing_material_yield() {
    let initial = plan_game_state();
    let plan = warehouse_dismantle_plan(&initial);
    let equipment_post = state_after(&initial, '2', &[], &[(1000, 0)]);
    let post = state_with_resource_updates(&equipment_post, 10, &[(2001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("背包材料 2001 数量为 1，期望 2")
    );
}

#[test]
fn acknowledged_dismantle_with_unchanged_readback_remains_only_possible() {
    let initial = plan_game_state();
    let plan = warehouse_dismantle_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial.clone(), initial],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert!(report.steps()[0].write_acknowledged());
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::Possible
    );
    assert_eq!(report.observed_state_change_count(), 0);
    assert_eq!(report.verified_write_count(), 0);
}

#[test]
fn unequip_rejects_equipment_that_does_not_reach_the_warehouse() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post = state_after(&initial, '2', &[(1, None)], &[]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 0);
    assert!(report.may_have_writes());
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::StateChangedMismatch
    );
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("仓库配置 1000")
    );
}

#[test]
fn ship_move_preserves_equipment_and_warehouse_quantity() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post_unequip = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let post_equip = state_after(&post_unequip, '3', &[(2, Some((1000, 0)))], &[(1000, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post_unequip, post_equip],
        vec![
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(report.acknowledged_write_count(), 2);
    assert_eq!(report.observed_state_change_count(), 2);
    assert_eq!(report.verified_write_count(), 2);
    assert!(report.may_have_writes());
    assert_eq!(port.commands.len(), 2);
    assert!(
        report
            .final_verification_summary()
            .unwrap()
            .contains("独立终态已核对")
    );
}

#[test]
fn ship_source_equip_requires_the_warehouse_quantity_to_decrease() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post_unequip = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let post_equip = state_after(&post_unequip, '3', &[(2, Some((1000, 0)))], &[]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post_unequip, post_equip],
        vec![
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::Failed);
    assert_eq!(report.acknowledged_write_count(), 2);
    assert_eq!(report.observed_state_change_count(), 2);
    assert_eq!(report.verified_write_count(), 1);
    assert!(
        report.steps()[1]
            .readback_summary()
            .unwrap()
            .contains("仓库配置 1000")
    );
}

#[test]
fn unrelated_drift_after_a_step_stops_before_the_next_command() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post = state_after(&initial, '2', &[(1, None), (4, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![
            PortOutcome::status(ExecutionStatus::Success),
            PortOutcome::status(ExecutionStatus::Success),
        ],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 0);
    assert!(
        report.steps()[0]
            .readback_evidence()
            .unwrap()
            .first_mismatch()
            .is_some()
    );
}

#[test]
fn independent_final_read_detects_drift_after_the_last_step_readback() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let final_drift = state_after(&post, '3', &[(4, None)], &[]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post, final_drift],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(
        report.stop_reason(),
        ExecutionStopReason::FinalStateMismatch
    );
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert_eq!(port.read_count, 3);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Mismatch
    );
    assert!(
        report
            .final_verification_summary()
            .unwrap()
            .contains("槽位 4")
    );
}

#[test]
fn explicit_command_failure_stops_and_marks_remaining_steps() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Failed)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::CommandFailed);
    assert_eq!(report.steps().len(), 2);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(port.commands.len(), 1);
}

#[test]
fn unknown_command_is_queried_once_without_resending() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Unknown)],
    );
    port.queries
        .push_back(PortOutcome::status(ExecutionStatus::Unknown));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.stop_reason(), ExecutionStopReason::CommandUnknown);
    assert_eq!(
        report.final_state_content_sha256(),
        Some(&"1".repeat(64)[..])
    );
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Unknown);
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::Possible
    );
    assert!(report.steps()[0].readback_evidence().is_some());
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(report.steps()[1].pre_state_content_sha256(), None);
    assert_eq!(report.steps()[1].post_state_content_sha256(), None);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert!(port.cancelled_command_ids.is_empty());
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::UnconfirmedStepNotObserved
    );
}

#[test]
fn unknown_command_records_when_the_independent_state_reaches_the_expected_post_state() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Unknown)],
    );
    port.queries
        .push_back(PortOutcome::status(ExecutionStatus::Unknown));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.stop_reason(), ExecutionStopReason::CommandUnknown);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Unknown);
    assert!(!report.steps()[0].write_acknowledged());
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::ExpectedPostStateObserved
    );
    assert!(
        report.steps()[0]
            .readback_evidence()
            .unwrap()
            .matches_expected()
    );
    assert_eq!(report.acknowledged_write_count(), 0);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 0);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::UnconfirmedStepReached
    );
}

#[test]
fn unknown_command_records_when_the_independent_state_diverges_from_both_boundaries() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let diverged = state_after(&initial, '3', &[(1, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, diverged],
        vec![PortOutcome::status(ExecutionStatus::Unknown)],
    );
    port.queries
        .push_back(PortOutcome::status(ExecutionStatus::Unknown));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Unknown);
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::StateChangedMismatch
    );
    assert!(report.steps()[0].readback_evidence().is_some());
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Mismatch
    );
}

#[test]
fn unknown_command_can_resolve_success_without_resending() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Unknown)],
    );
    port.queries
        .push_back(PortOutcome::status(ExecutionStatus::Success));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert!(
        report.steps()[0]
            .diagnostics()
            .contains_key("initial.response_summary")
    );
}

#[test]
fn uncertain_receipt_stops_observation_without_another_query() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::phase(ExecutionStatus::Unknown, "observing")],
    );
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Unknown, "uncertain"));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.stop_reason(), ExecutionStopReason::CommandUnknown);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert!(port.cancelled_command_ids.is_empty());
    assert_eq!(
        report.steps()[0].diagnostics().get("phase"),
        Some(&"uncertain".to_owned())
    );
}

#[test]
fn observing_query_error_stops_without_retrying_the_error() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::phase(ExecutionStatus::Unknown, "observing")],
    );
    port.queries.push_back(PortOutcome::Error(fixture_error(
        "execution.query.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试查询失败",
    )));
    port.queries
        .push_back(PortOutcome::status(ExecutionStatus::Success));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.stop_reason(), ExecutionStopReason::CommandUnknown);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert_eq!(
        report.steps()[0].diagnostics().get("follow_up_stage"),
        Some(&"execution.query.fixture".to_owned())
    );
}

#[test]
fn observing_receipt_respects_the_host_poll_deadline() {
    let state = plan_game_state();
    let plan = warehouse_equip_plan(&state);
    let target_identity = ExecutionTargetIdentity::new("a".repeat(64)).unwrap();
    let step = plan.steps()[0].clone();
    let command = ExecutionCommand::new(
        &target_identity,
        plan.content_sha256(),
        step.sequence(),
        state.source().content_sha256(),
        ExecutionAction::from_step(&step).unwrap(),
    )
    .unwrap();
    let initial = FakeExecutionPort::respond(
        PortOutcome::phase(ExecutionStatus::Unknown, "observing"),
        command.command_id(),
    )
    .unwrap();
    let mut port = FakeExecutionPort::new(Vec::new(), Vec::new());
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Unknown, "observing"));

    let (receipt, cancellation_observed) = resolve_unknown_receipt(
        &mut port,
        &NeverCancel,
        &command,
        initial,
        Duration::ZERO,
        &|_| panic!("零时限不应进入等待"),
    );

    assert!(!cancellation_observed);
    assert_eq!(receipt.status, ExecutionStatus::Unknown);
    assert!(port.queried_command_ids.is_empty());
    assert_eq!(
        receipt.diagnostics.get("resolution.timeout_reached"),
        Some(&"true".to_owned())
    );
    assert_eq!(
        receipt.diagnostics.get("resolution.budget_exhausted"),
        Some(&"true".to_owned())
    );
    assert_eq!(
        receipt.diagnostics.get("resolution.timeout_ms"),
        Some(&"0".to_owned())
    );
}

#[test]
fn slow_observing_query_stops_when_its_budget_is_spent() {
    use std::time::Instant;

    let state = plan_game_state();
    let plan = warehouse_equip_plan(&state);
    let target_identity = ExecutionTargetIdentity::new("a".repeat(64)).unwrap();
    let step = plan.steps()[0].clone();
    let command = ExecutionCommand::new(
        &target_identity,
        plan.content_sha256(),
        step.sequence(),
        state.source().content_sha256(),
        ExecutionAction::from_step(&step).unwrap(),
    )
    .unwrap();
    let initial = FakeExecutionPort::respond(
        PortOutcome::phase(ExecutionStatus::Unknown, "observing"),
        command.command_id(),
    )
    .unwrap();
    let mut port = FakeExecutionPort::new(Vec::new(), Vec::new());
    port.block_for_query_budget = true;
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Unknown, "observing"));
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Unknown, "observing"));
    let budget = Duration::from_millis(250);
    let started = Instant::now();

    let (receipt, cancellation_observed) =
        resolve_unknown_receipt(&mut port, &NeverCancel, &command, initial, budget, &|_| {
            panic!("预算已被慢查询用尽，不应再等待")
        });

    let elapsed = started.elapsed();
    assert!(!cancellation_observed);
    assert!(elapsed < Duration::from_millis(800), "{elapsed:?}");
    assert_eq!(port.queried_command_ids.len(), 1);
    assert_eq!(port.commands.len(), 0);
    assert_eq!(port.query_budgets.len(), 1);
    assert!(port.query_budgets[0] <= budget);
    assert!(
        port.query_budgets[0] > Duration::from_millis(150),
        "{:?}",
        port.query_budgets[0]
    );
    assert_eq!(receipt.command_id, command.command_id());
    assert_eq!(receipt.status, ExecutionStatus::Unknown);
    assert_eq!(
        receipt.diagnostics.get("resolution.timeout_reached"),
        Some(&"true".to_owned())
    );
    assert_eq!(
        receipt.diagnostics.get("resolution.query_count"),
        Some(&"1".to_owned())
    );
}

#[test]
fn cancellation_during_observation_cancels_original_command() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post_unequip = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post_unequip],
        vec![PortOutcome::phase(ExecutionStatus::Unknown, "observing")],
    );
    port.queries
        .push_back(PortOutcome::phase(ExecutionStatus::Unknown, "observing"));
    port.cancels
        .push_back(PortOutcome::phase(ExecutionStatus::Success, "succeeded"));
    let cancellation = CancelAfterObservationQuery {
        checks: Cell::new(0),
    };

    let report = execute_plan(&mut port, &plan, &cancellation).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Cancelled);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Cancelled);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert_eq!(port.cancelled_command_ids.len(), 1);
    assert_eq!(cancellation.checks.get(), 3);
}

#[test]
fn cancellation_after_unknown_requests_remote_cancel_instead_of_query() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Unknown)],
    );
    port.cancels
        .push_back(PortOutcome::status(ExecutionStatus::Unknown));
    let cancellation = CancelAfterFirstBoundary {
        checks: Cell::new(0),
    };

    let report = execute_plan(&mut port, &plan, &cancellation).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.stop_reason(), ExecutionStopReason::CommandUnknown);
    assert_eq!(port.commands.len(), 1);
    assert!(port.queried_command_ids.is_empty());
    assert_eq!(port.cancelled_command_ids.len(), 1);
}

#[test]
fn observed_cancellation_is_latched_after_the_unknown_command_resolves_successfully() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post_unequip = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post_unequip],
        vec![PortOutcome::status(ExecutionStatus::Unknown)],
    );
    port.cancels
        .push_back(PortOutcome::status(ExecutionStatus::Success));
    let cancellation = CancelOnlyDuringResolution {
        checks: Cell::new(0),
    };

    let report = execute_plan(&mut port, &plan, &cancellation).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Cancelled);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Cancelled);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(port.commands.len(), 1);
    assert!(port.queried_command_ids.is_empty());
    assert_eq!(port.cancelled_command_ids.len(), 1);
    assert_eq!(cancellation.checks.get(), 2);
}

#[test]
fn cancellation_at_the_next_boundary_preserves_completed_readback() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post_unequip = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post_unequip],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    let cancellation = CancelAfterFirstBoundary {
        checks: Cell::new(0),
    };

    let report = execute_plan(&mut port, &plan, &cancellation).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Cancelled);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Cancelled);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(
        report.final_state_content_sha256(),
        Some(&"2".repeat(64)[..])
    );
    assert_eq!(port.commands.len(), 1);
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 1);
    assert!(report.may_have_writes());
}

#[test]
fn acknowledged_write_with_failed_readback_detects_an_unchanged_final_state() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    port.reads.push_back(Err(fixture_error(
        "game.read.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试回读失败",
    )));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(
        report.final_state_content_sha256(),
        Some(&"1".repeat(64)[..])
    );
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert!(report.steps()[0].write_acknowledged());
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::Possible
    );
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 0);
    assert_eq!(report.verified_write_count(), 0);
    assert!(report.may_have_writes());
    assert_eq!(
        report.steps()[0].error_code(),
        Some(AppErrorCode::EquipmentStateChanged.as_str())
    );
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Mismatch
    );
    assert!(
        report.steps()[0]
            .diagnostics()
            .contains_key("readback.stage")
    );
}

#[test]
fn acknowledged_write_with_failed_readback_converges_on_the_expected_final_state() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    port.reads.push_back(Err(fixture_error(
        "game.read.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试回读失败",
    )));
    port.reads.push_back(Ok(post));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(report.stop_reason(), ExecutionStopReason::Completed);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert!(report.steps()[0].write_acknowledged());
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::Verified
    );
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 1);
    assert_eq!(report.steps()[0].error_code(), None);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Verified
    );
    assert!(
        report.steps()[0]
            .diagnostics()
            .contains_key("readback.stage")
    );
}

#[test]
fn acknowledged_write_with_failed_readback_detects_a_diverged_final_state() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let diverged = state_after(&initial, '3', &[(1, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    port.reads.push_back(Err(fixture_error(
        "game.read.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试回读失败",
    )));
    port.reads.push_back(Ok(diverged));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert!(report.steps()[0].write_acknowledged());
    assert_eq!(
        report.steps()[0].write_effect(),
        ExecutionWriteEffect::StateChangedMismatch
    );
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 0);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Mismatch
    );
}

#[test]
fn acknowledged_step_can_converge_without_resuming_the_remaining_plan() {
    let initial = plan_game_state();
    let plan = ship_move_plan(&initial);
    let post_unequip = state_after(&initial, '2', &[(1, None)], &[(1000, 2)]);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    port.reads.push_back(Err(fixture_error(
        "game.read.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试回读失败",
    )));
    port.reads.push_back(Ok(post_unequip));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackFailed);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Success);
    assert_eq!(report.steps()[1].status(), ExecutionStatus::NotExecuted);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(report.acknowledged_write_count(), 1);
    assert_eq!(report.observed_state_change_count(), 1);
    assert_eq!(report.verified_write_count(), 1);
    assert_eq!(
        report.final_verification_status(),
        ExecutionFinalVerificationStatus::Incomplete
    );
}

#[test]
fn successful_receipt_with_mismatched_readback_is_failed() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mismatched = state_after(&initial, '2', &[], &[]);
    let mut port = FakeExecutionPort::new(
        vec![initial, mismatched],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Failed);
    assert_eq!(report.stop_reason(), ExecutionStopReason::ReadbackMismatch);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Failed);
    assert_eq!(report.steps()[0].message(), "回读状态不符合完整步骤预期");
    assert!(
        report.steps()[0]
            .readback_summary()
            .unwrap()
            .contains("空槽")
    );
}

#[test]
fn invalid_receipt_identity_remains_unknown_after_query_failure() {
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let mut port = FakeExecutionPort::new(
        vec![initial],
        vec![PortOutcome::wrong_id(ExecutionStatus::Success)],
    );
    port.queries.push_back(PortOutcome::Error(fixture_error(
        "execution.query.fixture",
        AppErrorCode::RuntimeBootstrapFailed,
        "测试查询失败",
    )));

    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    assert_eq!(report.status(), ExecutionReportStatus::Unknown);
    assert_eq!(report.steps()[0].status(), ExecutionStatus::Unknown);
    assert_eq!(port.commands.len(), 1);
    assert_eq!(port.queried_command_ids.len(), 1);
    assert!(
        report.steps()[0]
            .diagnostics()
            .contains_key("actual_command_id")
    );
}

#[test]
fn report_serialization_contains_stable_identity_and_status() {
    let state = plan_game_state();
    let plan = keep_plan(&state);
    let mut port = FakeExecutionPort::new(vec![state], Vec::new());
    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();

    let value = serde_json::to_value(&report).unwrap();

    assert_eq!(value["schema_version"], 4);
    assert_eq!(value["plan_hash"], plan.content_sha256());
    assert_eq!(value["status"], "success");
    assert_eq!(value["stop_reason"], "completed");
    assert_eq!(
        value["target_identity"]["fingerprint_sha256"],
        "a".repeat(64)
    );
    assert_eq!(value["acknowledged_write_count"], 0);
    assert_eq!(value["observed_state_change_count"], 0);
    assert_eq!(value["verified_write_count"], 0);
    assert_eq!(value["may_have_writes"], false);
    assert_eq!(value["final_verification_status"], "verified");
    assert_eq!(value["steps"][0]["step_kind"], "keep");
    assert_eq!(
        report.content_sha256(),
        "455ec6eba632c7624a5742e676b80b1142ca2c9993012f685f5fd2ee8fad868e"
    );
}

#[test]
fn execution_outcome_carries_actual_equipped_state_and_never_substitutes_failed_read() {
    use super::state_machine::execute_plan_from_state_with_outcome;
    let initial = plan_game_state();
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![post.clone(), post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    let target = port.target_identity.clone();
    let outcome = execute_plan_from_state_with_outcome(
        &mut port,
        &plan,
        crate::application::GameObservation::from_state(initial),
        &target,
        &NeverCancel,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(outcome.report.status(), ExecutionReportStatus::Success);
    let state = outcome.final_state.unwrap();
    let projection = crate::application::project_game_state_to_workbook(&state).unwrap();
    assert!(projection.sheet("equipment_inventory").unwrap().rows().iter().any(|row| {
        matches!(row.value("source_type"), Some(crate::application::WorkbookProjectionValue::Text(value)) if value == "ship")
            && matches!(row.value("config_id"), Some(crate::application::WorkbookProjectionValue::Text(value)) if value == "1001")
    }));

    let initial = plan_game_state();
    let plan = keep_plan(&initial);
    let mut port = FakeExecutionPort::new(Vec::new(), Vec::new());
    port.reads.push_back(Err(fixture_error(
        "fixture.final",
        AppErrorCode::RuntimeBootstrapFailed,
        "终态读取失败",
    )));
    let target = port.target_identity.clone();
    let outcome = execute_plan_from_state_with_outcome(
        &mut port,
        &plan,
        crate::application::GameObservation::from_state(initial),
        &target,
        &NeverCancel,
        &mut |_| {},
    )
    .unwrap();
    assert!(outcome.final_state.is_none());
    assert_eq!(
        outcome.report.stop_reason(),
        ExecutionStopReason::FinalReadbackFailed
    );
}

#[test]
fn equipment_writeback_reads_preserve_initial_optional_scope() {
    let state = plan_game_state();
    let scope = crate::domain::GameReadScope::with_ship_skill_effects(false)
        .with_equipment_details(false, false);
    let initial = GameState::new(
        state.source().clone().with_read_scope(scope),
        state.ships().clone(),
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );
    let plan = warehouse_equip_plan(&initial);
    let post = state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)]);
    let mut port = FakeExecutionPort::new(
        vec![initial, post],
        vec![PortOutcome::status(ExecutionStatus::Success)],
    );
    let report = execute_plan(&mut port, &plan, &NeverCancel).unwrap();
    assert_eq!(report.status(), ExecutionReportStatus::Success);
    assert_eq!(port.read_scopes, vec![scope, scope]);
}

#[test]
fn execution_progress_counts_only_verified_steps_and_reports_receipt_waits() {
    use super::state_machine::execute_plan_from_state_with_outcome;
    for matches in [false, true] {
        let initial = plan_game_state();
        let plan = warehouse_equip_plan(&initial);
        let post = if matches {
            state_after(&initial, '2', &[(2, Some((1001, 1)))], &[(1001, 1)])
        } else {
            initial.clone()
        };
        let mut port = FakeExecutionPort::new(
            vec![post.clone(), post],
            vec![PortOutcome::status(ExecutionStatus::Unknown)],
        );
        port.queries
            .push_back(PortOutcome::status(ExecutionStatus::Success));
        let target = port.target_identity.clone();
        let mut progress = Vec::new();
        let outcome = execute_plan_from_state_with_outcome(
            &mut port,
            &plan,
            crate::application::GameObservation::from_state(initial),
            &target,
            &NeverCancel,
            &mut |event| progress.push(event),
        )
        .unwrap();
        assert_eq!(
            outcome.report.status() == ExecutionReportStatus::Success,
            matches
        );
        let waiting = progress
            .iter()
            .position(|event| event.message.contains("等待第1步命令回执"))
            .unwrap();
        let reading = progress
            .iter()
            .position(|event| event.message.contains("回读并核验第1步"))
            .unwrap();
        assert!(waiting < reading);
        assert_eq!(progress[waiting].units, Some((0, 1)));
        assert_eq!(progress[reading].units, Some((0, 1)));
        assert_eq!(
            progress.iter().any(|event| event.units == Some((1, 1))),
            matches
        );
        assert!(progress.last().unwrap().message.contains("独立最终状态"));
        assert!(progress.last().unwrap().units.is_none());
        assert_eq!(port.commands.len(), 1);
    }
}

fn fleet_plan(state: &GameState, ship_count: usize) -> crate::application::CompiledPlan {
    let mut desired_slots = Vec::with_capacity(ship_count + 1);
    for index in 0..ship_count {
        let instance_id = 9001 + index as u64;
        let equipment = DesiredEquipment::new(
            crate::domain::EquipmentFamilyId::new(1000).unwrap(),
            SourcePolicy::ExactSource,
            Some(EquipmentSourceRef::Warehouse(
                EquipmentConfigId::new(1001).unwrap(),
            )),
            None,
        )
        .unwrap();
        desired_slots.push(DesiredSlotState::new(
            slot(instance_id, 2),
            SlotTarget::Equipment(equipment),
            index as i32,
        ));
    }
    let compose = DesiredEquipment::new(
        crate::domain::EquipmentFamilyId::new(1000).unwrap(),
        SourcePolicy::ComposeOnly,
        None,
        Some(EnhanceLevel::new(1)),
    )
    .unwrap();
    desired_slots.push(DesiredSlotState::new(
        slot(9001, 3),
        SlotTarget::Equipment(compose),
        ship_count as i32,
    ));
    compile_plan(state, &DesiredState::new(desired_slots).unwrap())
        .unwrap()
        .plan()
        .clone()
}

#[test]
fn many_ship_plan_rehearses_equipment_and_resource_steps() {
    let state = plan_game_state_with_ship_count(8);
    let plan = fleet_plan(&state, 8);
    let expected = expected_execution(&state, &plan).unwrap();
    assert!(plan.steps().len() >= 8, "步数 {}", plan.steps().len());
    assert!(
        plan.steps()
            .iter()
            .any(|step| matches!(step, PlanStep::Equip { .. })),
        "计划应包含装备状态变化"
    );
    assert!(
        plan.steps()
            .iter()
            .any(|step| { matches!(step, PlanStep::Compose { .. } | PlanStep::Enhance { .. }) }),
        "计划应包含合成或强化"
    );
    let last = expected.checkpoint_count() - 1;
    assert_ne!(
        expected.equipment_at(0).slots.len(),
        0,
        "预演应保留装备槽位"
    );
    assert_eq!(
        expected.equipment_at(last).slots.len(),
        expected.equipment_at(0).slots.len()
    );
}

fn estimate_snapshot_bytes(header: usize, entries: usize, entry_size: usize) -> usize {
    let node = 3 * std::mem::size_of::<usize>();
    header + entries.saturating_mul(entry_size.saturating_add(node))
}

fn equip_only_plan(state: &GameState, ship_count: usize) -> crate::application::CompiledPlan {
    let mut desired_slots = Vec::with_capacity(ship_count);
    for index in 0..ship_count {
        let instance_id = 9001 + index as u64;
        let equipment = DesiredEquipment::new(
            crate::domain::EquipmentFamilyId::new(1000).unwrap(),
            SourcePolicy::ExactSource,
            Some(EquipmentSourceRef::Warehouse(
                EquipmentConfigId::new(1001).unwrap(),
            )),
            None,
        )
        .unwrap();
        desired_slots.push(DesiredSlotState::new(
            slot(instance_id, 2),
            SlotTarget::Equipment(equipment),
            index as i32,
        ));
    }
    compile_plan(state, &DesiredState::new(desired_slots).unwrap())
        .unwrap()
        .plan()
        .clone()
}

#[test]
fn equip_rehearsal_shares_resources_and_preserves_earlier_checkpoints() {
    for count in [1_usize, 100] {
        let state = plan_game_state_with_ship_count(count);
        let plan = equip_only_plan(&state, count);
        assert_eq!(plan.steps().len(), count);
        let expected = expected_execution(&state, &plan).unwrap();
        assert_eq!(expected.checkpoint_count(), count + 1);
        let initial = expected.equipment_at(0);
        for index in 0..=count {
            let checkpoint = expected.equipment_at(index);
            assert!(std::ptr::eq(
                expected.resources_at(0),
                expected.resources_at(index)
            ));
            let remaining = initial.warehouse[&1001] - index as u64;
            assert_eq!(
                checkpoint.warehouse.get(&1001).copied(),
                (remaining > 0).then_some(remaining)
            );
            let equipped = (0..count)
                .filter(|ship| checkpoint.slots[&(9001 + *ship as u64, 2)].is_some())
                .count();
            assert_eq!(equipped, index);
        }
    }
}

#[test]
fn rehearsal_checkpoints_come_from_the_initial_state_and_plan() {
    let state = plan_game_state();
    let empty = compile_plan(&state, &DesiredState::new(Vec::new()).unwrap()).unwrap();
    let initial = super::preflight::equipment_state_snapshot(&state);
    let expected = expected_execution(&state, empty.plan()).unwrap();
    assert_eq!(expected.checkpoint_count(), 1);
    assert_eq!(expected.equipment_at(0), &initial);

    let plan = warehouse_equip_plan(&state);
    let expected = expected_execution(&state, &plan).unwrap();
    assert_eq!(expected.checkpoint_count(), 2);
    assert_eq!(expected.equipment_at(0), &initial);
    let mut equipped = initial;
    equipped.slots.insert(
        (9001, 2),
        Some(ExecutionEquipmentState {
            config_id: 1001,
            enhance_level: 1,
        }),
    );
    let remaining = equipped.warehouse[&1001] - 1;
    if remaining == 0 {
        equipped.warehouse.remove(&1001);
    } else {
        equipped.warehouse.insert(1001, remaining);
    }
    assert_eq!(expected.equipment_at(1), &equipped);
    assert_eq!(expected.resources_at(0), expected.resources_at(1));
}

#[test]
fn repeated_enhances_keep_distinct_resource_checkpoints() {
    for count in [1_usize, 100] {
        let state = plan_game_state_with_ship_count(count.max(2));
        let plan = repeated_warehouse_enhances(&state, count);
        assert_eq!(plan.steps().len(), count);
        assert!(
            plan.steps()
                .iter()
                .all(|step| matches!(step, PlanStep::Enhance { .. }))
        );
        let expected = expected_execution(&state, &plan).unwrap();
        assert_eq!(expected.checkpoint_count(), count + 1);
        let initial = expected.resources_at(0);
        let mut identities = std::collections::BTreeSet::new();
        for index in 0..=count {
            let resources = expected.resources_at(index);
            assert_eq!(resources.gold, initial.gold - index as u64 * 10);
            assert_eq!(
                resources.materials[&3001],
                initial.materials[&3001] - index as u64 * 2
            );
            assert!(identities.insert(std::ptr::from_ref(resources)));
        }
    }
}

fn repeated_warehouse_enhances(
    state: &GameState,
    count: usize,
) -> crate::application::CompiledPlan {
    let action = EquipmentInventoryAction::new(
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(1000).unwrap()),
        EquipmentInventoryActionKind::Keep,
        None,
        Some(EnhanceLevel::new(1)),
        Some(count as u64),
    )
    .unwrap();
    let inventory = EquipmentInventoryPlan::new(vec![action]).unwrap();
    compile_plan_with_inventory(state, &DesiredState::new(Vec::new()).unwrap(), &inventory)
        .unwrap()
        .plan()
        .clone()
}

#[cfg(windows)]
fn process_working_set_bytes() -> (usize, usize) {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged_pool: usize,
        quota_paged_pool: usize,
        quota_peak_non_paged_pool: usize,
        quota_non_paged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
        fn GetCurrentProcess() -> isize;
    }
    let mut counters = Counters {
        cb: 0,
        page_fault_count: 0,
        peak_working_set: 0,
        working_set: 0,
        quota_peak_paged_pool: 0,
        quota_paged_pool: 0,
        quota_peak_non_paged_pool: 0,
        quota_non_paged_pool: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    counters.cb = std::mem::size_of::<Counters>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if ok == 0 {
        panic!("读取进程内存失败: {}", std::io::Error::last_os_error())
    } else {
        (counters.working_set, counters.peak_working_set)
    }
}

#[cfg(not(windows))]
fn process_working_set_bytes() -> (usize, usize) {
    (0, 0)
}

/// 用真实编译和预演测量 100 与 1000 步规模。不改变状态机。
#[test]
#[ignore = "AZLW_MEASURE_MODE=plan"]
fn measure_preflight_checkpoint_copies_when_requested() {
    if std::env::var("AZLW_MEASURE_MODE").ok().as_deref() != Some("plan") {
        return;
    }
    let samples: usize = std::env::var("AZLW_MEASURE_PLAN_SAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1);
    for sample in 1..=samples {
        for ship_count in [0_usize, 1, 100, 1000] {
            let state = plan_game_state_with_ship_count(ship_count);
            let compile_started = std::time::Instant::now();
            let plan = fleet_plan(&state, ship_count);
            let compile_us = compile_started.elapsed().as_micros();
            let rehearsal_started = std::time::Instant::now();
            let expected = expected_execution(&state, &plan).unwrap();
            let rehearsal_us = rehearsal_started.elapsed().as_micros();
            let slots = expected.equipment_at(0).slots.len();
            let warehouse = expected.equipment_at(0).warehouse.len();
            let materials = expected.resources_at(0).materials.len();
            let mut seen_equipment = std::collections::BTreeSet::new();
            let mut seen_resources = std::collections::BTreeSet::new();
            let mut equipment_retained = 0_usize;
            let mut resource_retained = 0_usize;
            for index in 0..expected.checkpoint_count() {
                let equipment = expected.equipment_at(index);
                let equipment_address = std::ptr::from_ref(equipment) as usize;
                if seen_equipment.insert(equipment_address) {
                    equipment_retained += estimate_snapshot_bytes(
                        std::mem::size_of_val(equipment),
                        equipment.slots.len(),
                        std::mem::size_of::<((u64, u8), Option<ExecutionEquipmentState>)>(),
                    ) + estimate_snapshot_bytes(
                        0,
                        equipment.warehouse.len(),
                        std::mem::size_of::<(u64, u64)>(),
                    );
                }
                let resources = expected.resources_at(index);
                let resource_address = std::ptr::from_ref(resources) as usize;
                if seen_resources.insert(resource_address) {
                    resource_retained += estimate_snapshot_bytes(
                        std::mem::size_of_val(resources),
                        resources.materials.len(),
                        std::mem::size_of::<(u64, u64)>(),
                    );
                }
            }
            let target = ExecutionTargetIdentity::new("a".repeat(64)).unwrap();
            let preflight_started = std::time::Instant::now();
            let preflight = super::preflight::build_execution_preflight(
                &target,
                &plan,
                state.source().content_sha256(),
                &expected,
            )
            .unwrap();
            let preflight_us = preflight_started.elapsed().as_micros();
            std::hint::black_box(&preflight);
            let access_started = std::time::Instant::now();
            for step in expected.steps() {
                std::hint::black_box((&step.equipment_before, &step.equipment_after));
            }
            let checkpoint_access_us = access_started.elapsed().as_micros();
            let steps = plan.steps().len();
            let checkpoints = expected.checkpoint_count();
            let unique_equipment = seen_equipment.len();
            let unique_resources = seen_resources.len();
            drop(expected);
            let (working_set, peak_working_set) = process_working_set_bytes();
            println!(
                "\nMEASURE stage=long_plan_rehearsal sample={sample} samples={samples} ships={ship_count} steps={steps} checkpoints={checkpoints} slots={slots} warehouse={warehouse} materials={materials} compile_us={compile_us} rehearsal_us={rehearsal_us} preflight_us={preflight_us} checkpoint_access_us={checkpoint_access_us} unique_equipment_snapshots={unique_equipment} unique_resource_snapshots={unique_resources} estimated_equipment_retained_bytes={equipment_retained} estimated_resource_retained_bytes={resource_retained} process_working_set_after_drop={working_set} process_peak_working_set={peak_working_set} note=peak_is_process_high_water"
            );
        }
    }
}

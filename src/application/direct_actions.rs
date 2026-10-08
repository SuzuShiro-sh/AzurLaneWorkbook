//! 将直接装备操作转换为统一计划，并复用游戏会话与执行状态机。

use serde::{Deserialize, Serialize};

use super::{GameOperationAccess, GameOperationFinish, GameSession};
use crate::application::execution::{ExecutionPort, execute_plan_from_state_with_outcome};
use crate::application::{
    AppError, AppErrorCode, CheckReport, ExecutionCancellation, ExecutionReport,
    NoExecutionCancellation, OperationTerminal, PlanCheckError, SessionCleanup,
    map_plan_check_error,
};
use crate::domain::{
    DesiredEquipment, DesiredSlotState, DesiredState, EnhanceLevel, EquipmentConfigId,
    EquipmentFamilyId, EquipmentInventoryAction, EquipmentInventoryActionKind,
    EquipmentInventoryPlan, EquipmentSourceRef, GameState, ShipInstanceId, ShipSlotRef, SlotIndex,
    SlotTarget, SourcePolicy,
};

/// 账号内舰船实例与 1 至 5 的装备槽编号。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DirectShipSlot {
    pub ship_id: u64,
    pub slot_index: u8,
}

impl DirectShipSlot {
    fn domain(self) -> Result<ShipSlotRef, AppError> {
        Ok(ShipSlotRef::new(
            ShipInstanceId::new(self.ship_id).map_err(input_error)?,
            SlotIndex::new(self.slot_index).map_err(input_error)?,
        ))
    }
}

/// 精确来源：仓库 config_id 包含强化配置，舰上来源指向实际槽位。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectEquipmentSource {
    Warehouse { config_id: u64 },
    ShipSlot { ship_id: u64, slot_index: u8 },
}

impl DirectEquipmentSource {
    fn domain(self) -> Result<EquipmentSourceRef, AppError> {
        match self {
            Self::Warehouse { config_id } => Ok(EquipmentSourceRef::Warehouse(
                EquipmentConfigId::new(config_id).map_err(input_error)?,
            )),
            Self::ShipSlot {
                ship_id,
                slot_index,
            } => Ok(EquipmentSourceRef::ShipSlot(
                DirectShipSlot {
                    ship_id,
                    slot_index,
                }
                .domain()?,
            )),
        }
    }
}

/// 一项最终状态要求。数量用于仓库聚合来源，舰上来源只能消费一件。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectAction {
    Compose {
        recipe_id: u64,
        count: u64,
    },
    EquipFamily {
        target: DirectShipSlot,
        family_id: u64,
        #[serde(with = "direct_source_policy")]
        policy: SourcePolicy,
        target_level: u8,
    },
    Equip {
        target: DirectShipSlot,
        source: DirectEquipmentSource,
    },
    Unequip {
        target: DirectShipSlot,
    },
    Enhance {
        source: DirectEquipmentSource,
        target_level: u8,
        quantity: u64,
    },
    Dismantle {
        source: DirectEquipmentSource,
        quantity: u64,
    },
}

/// 单项与批量共用的输入；数组表达同一计划，步骤顺序由依赖关系确定。
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DirectActionBatch {
    pub actions: Vec<DirectAction>,
}

impl DirectActionBatch {
    fn validate(&self) -> Result<(), AppError> {
        if self.actions.is_empty() {
            return Err(input_error(std::io::Error::other("actions 不能为空")));
        }
        for action in &self.actions {
            match *action {
                DirectAction::Compose { recipe_id, count } => {
                    if recipe_id == 0 || count == 0 {
                        return Err(input_error(std::io::Error::other(
                            "配方 ID 和合成数量必须大于 0",
                        )));
                    }
                }
                DirectAction::EquipFamily {
                    target,
                    family_id,
                    policy,
                    ..
                } => {
                    target.domain()?;
                    EquipmentFamilyId::new(family_id).map_err(input_error)?;
                    if !matches!(
                        policy,
                        SourcePolicy::WarehouseOnly
                            | SourcePolicy::WarehouseThenCompose
                            | SourcePolicy::ComposeOnly
                    ) {
                        return Err(input_error(std::io::Error::other("装备族来源策略无效")));
                    }
                }
                DirectAction::Equip { target, source } => {
                    target.domain()?;
                    source.domain()?;
                }
                DirectAction::Unequip { target } => {
                    target.domain()?;
                }
                DirectAction::Enhance {
                    source, quantity, ..
                }
                | DirectAction::Dismantle { source, quantity } => {
                    source.domain()?;
                    if quantity == 0
                        || (matches!(source, DirectEquipmentSource::ShipSlot { .. })
                            && quantity != 1)
                    {
                        return Err(input_error(std::io::Error::other(
                            "数量必须大于 0，舰船槽位来源数量必须为 1",
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn compile(&self, state: &GameState) -> Result<CheckReport, AppError> {
        self.validate()?;
        let mut compositions = Vec::new();
        let mut slots = Vec::new();
        let mut inventory = Vec::new();
        for action in &self.actions {
            match *action {
                DirectAction::Compose { recipe_id, count } => compositions.push((recipe_id, count)),
                DirectAction::EquipFamily {
                    target,
                    family_id,
                    policy,
                    target_level,
                } => {
                    let equipment = DesiredEquipment::new(
                        EquipmentFamilyId::new(family_id).map_err(input_error)?,
                        policy,
                        None,
                        Some(EnhanceLevel::new(target_level)),
                    )
                    .map_err(input_error)?;
                    slots.push(DesiredSlotState::new(
                        target.domain()?,
                        SlotTarget::Equipment(equipment),
                        0,
                    ));
                }
                DirectAction::Equip { target, source } => {
                    let source = source.domain()?;
                    let family = source_family(state, source).map_err(map_plan_check_error)?;
                    let equipment = DesiredEquipment::new(
                        family,
                        SourcePolicy::ExactSource,
                        Some(source),
                        None,
                    )
                    .map_err(input_error)?;
                    slots.push(DesiredSlotState::new(
                        target.domain()?,
                        SlotTarget::Equipment(equipment),
                        0,
                    ));
                }
                DirectAction::Unequip { target } => slots.push(DesiredSlotState::new(
                    target.domain()?,
                    SlotTarget::Empty,
                    0,
                )),
                DirectAction::Enhance {
                    source,
                    target_level,
                    quantity,
                } => inventory.push(
                    EquipmentInventoryAction::new(
                        source.domain()?,
                        EquipmentInventoryActionKind::Keep,
                        None,
                        Some(EnhanceLevel::new(target_level)),
                        Some(quantity),
                    )
                    .map_err(input_error)?,
                ),
                DirectAction::Dismantle { source, quantity } => inventory.push(
                    EquipmentInventoryAction::new(
                        source.domain()?,
                        EquipmentInventoryActionKind::Dismantle,
                        Some(quantity),
                        None,
                        None,
                    )
                    .map_err(input_error)?,
                ),
            }
        }
        let desired = DesiredState::new(slots).map_err(input_error)?;
        let inventory = EquipmentInventoryPlan::new(inventory).map_err(input_error)?;
        crate::application::plan::compile_direct_plan(state, &desired, &inventory, &compositions)
            .map_err(map_plan_check_error)
    }
}

mod direct_source_policy {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        policy: &SourcePolicy,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match policy {
            SourcePolicy::WarehouseOnly => "warehouse-only",
            SourcePolicy::WarehouseThenCompose => "warehouse-compose",
            SourcePolicy::ComposeOnly => "compose-only",
            _ => return Err(serde::ser::Error::custom("装备族来源策略无效")),
        })
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SourcePolicy, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "warehouse-only" => Ok(SourcePolicy::WarehouseOnly),
            "warehouse-compose" => Ok(SourcePolicy::WarehouseThenCompose),
            "compose-only" => Ok(SourcePolicy::ComposeOnly),
            _ => Err(serde::de::Error::custom("装备族来源策略无效")),
        }
    }
}

fn input_error(source: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::from_source(
        "equipment.actions.input",
        AppErrorCode::InputInvalid,
        "装备操作输入无效",
        source,
    )
}

fn source_family(
    state: &GameState,
    source: EquipmentSourceRef,
) -> Result<EquipmentFamilyId, PlanCheckError> {
    match source {
        EquipmentSourceRef::Warehouse(config_id) => state
            .equipment_inventory()
            .warehouse_stack(config_id)
            .map(|stack| stack.family_id()),
        EquipmentSourceRef::ShipSlot(slot) => state
            .ships()
            .ships()
            .iter()
            .find(|ship| ship.identity().instance_id() == slot.ship_instance_id())
            .and_then(|ship| {
                ship.slots()
                    .iter()
                    .find(|item| item.index() == slot.slot_index())
            })
            .and_then(|item| item.equipment())
            .and_then(|equipment| {
                state
                    .equipment_catalog()
                    .families()
                    .iter()
                    .find(|family| {
                        family
                            .configs()
                            .iter()
                            .any(|config| config.identity().config_id() == equipment.config_id())
                    })
                    .map(|family| family.family_id())
            }),
    }
    .ok_or(PlanCheckError::SourceNotFound { source_ref: source })
}

/// 计划及可选执行报告；执行已发生时始终保留原报告和清理事实。
#[derive(Debug)]
pub struct DirectActionOutcome {
    pub check: CheckReport,
    pub execution: Option<ExecutionReport>,
    pub cleanup: SessionCleanup,
}

impl DirectActionOutcome {
    pub fn log_terminal(&self) -> OperationTerminal {
        let terminal = self
            .execution
            .as_ref()
            .map_or(OperationTerminal::Succeeded, |report| {
                OperationTerminal::from_execution(report.status())
            });
        if matches!(self.cleanup, SessionCleanup::Completed) {
            terminal
        } else {
            terminal.with_incomplete_tail()
        }
    }
}

/// 不依赖工作簿的装备计划检查和明确执行入口。
pub struct DirectActionService {
    session: GameSession,
}

impl DirectActionService {
    pub(crate) fn new(execution: Option<Box<dyn ExecutionPort>>) -> Self {
        Self {
            session: GameSession::execution(execution),
        }
    }

    /// 检查计划并关闭读取会话，不发送游戏写命令。
    pub fn check(&mut self, batch: &DirectActionBatch) -> Result<DirectActionOutcome, AppError> {
        self.check_with_progress(batch, &mut |_| {})
    }

    pub(crate) fn check_with_progress(
        &mut self,
        batch: &DirectActionBatch,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<DirectActionOutcome, AppError> {
        self.run(batch, false, None, &NoExecutionCancellation, progress)
    }

    /// 明确执行整批计划；失败或未知步骤均按统一执行器停止后续写入。
    pub fn apply(&mut self, batch: &DirectActionBatch) -> Result<DirectActionOutcome, AppError> {
        self.apply_with_cancellation(batch, &NoExecutionCancellation)
    }

    pub fn apply_with_cancellation(
        &mut self,
        batch: &DirectActionBatch,
        cancellation: &dyn ExecutionCancellation,
    ) -> Result<DirectActionOutcome, AppError> {
        self.run(batch, true, None, cancellation, &mut |_| {})
    }

    /// 执行调用方已经检查过的计划；在绑定写会话前拒绝摘要不一致。
    pub(crate) fn apply_checked(
        &mut self,
        batch: &DirectActionBatch,
        expected_plan_hash: &str,
        cancellation: &dyn ExecutionCancellation,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<DirectActionOutcome, AppError> {
        self.run(
            batch,
            true,
            Some(expected_plan_hash),
            cancellation,
            progress,
        )
    }

    fn run(
        &mut self,
        batch: &DirectActionBatch,
        apply: bool,
        expected_plan_hash: Option<&str>,
        cancellation: &dyn ExecutionCancellation,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
    ) -> Result<DirectActionOutcome, AppError> {
        batch.validate()?;
        let operation = (|| {
            let port = self.session.execution_mut().ok_or_else(|| {
                super::missing_game_port("equipment.actions", "当前连接未提供装备执行端口")
            })?;
            let scope = crate::domain::GameReadScope::with_ship_skill_effects(false)
                .with_ship_technology(false)
                .with_equipment_details(false, false);
            let state = port.read_state_with_scope(scope, progress)?;
            let check = batch.compile(&state)?;
            if let Some(expected) = expected_plan_hash
                && expected != check.plan().content_sha256()
            {
                return Err(AppError::from_source(
                    "equipment.actions.confirmation",
                    AppErrorCode::EquipmentStateChanged,
                    "装备计划与检查结果不一致，请重新检查",
                    std::io::Error::other("checked plan hash differs"),
                )
                .with_context("expected_plan_hash", expected)
                .with_context("actual_plan_hash", check.plan().content_sha256()));
            }
            let execution = if apply {
                let identity = port.target_identity(&state)?;
                port.bind_current_session()?;
                // 会话绑定后重新读取并编译，状态或精确来源变化时拒绝发送。
                let current = port.read_state_with_scope(scope, progress)?;
                let confirmed = batch.compile(&current)?;
                if confirmed.plan().content_sha256() != check.plan().content_sha256() {
                    return Err(AppError::from_source(
                        "equipment.actions.confirmation",
                        AppErrorCode::EquipmentStateChanged,
                        "装备计划在执行前发生变化，请重新检查",
                        std::io::Error::other("recompiled plan differs"),
                    )
                    .with_context("expected_plan_hash", check.plan().content_sha256())
                    .with_context("actual_plan_hash", confirmed.plan().content_sha256()));
                }
                Some(
                    execute_plan_from_state_with_outcome(
                        port,
                        confirmed.plan(),
                        current,
                        &identity,
                        cancellation,
                        progress,
                    )?
                    .report,
                )
            } else {
                None
            };
            Ok((check, execution))
        })();
        let access = if apply {
            GameOperationAccess::Write
        } else {
            GameOperationAccess::ReadOnly
        };
        match self.session.finish_operation(access, operation) {
            GameOperationFinish::Ready((check, execution)) => Ok(DirectActionOutcome {
                check,
                execution,
                cleanup: SessionCleanup::Completed,
            }),
            GameOperationFinish::ValueWithCleanup {
                value: (check, execution),
                cleanup,
            } => Ok(DirectActionOutcome {
                check,
                execution,
                cleanup: SessionCleanup::from_error(cleanup),
            }),
            GameOperationFinish::Failed(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::application::test_support::{plan_game_state, plan_game_state_with_enhance};
    use crate::application::{
        ExecutionCommand, ExecutionCommandReceipt, ExecutionPreflight, ExecutionSendResult,
        ExecutionTargetIdentity, GameObservation, GamePort, PlanStep,
    };

    struct Port {
        state: GameState,
        events: Arc<Mutex<Vec<&'static str>>>,
        cleanup_fails: bool,
        target_changes: bool,
        identities: usize,
    }

    impl GamePort for Port {
        fn read_full_state(&mut self) -> Result<GameObservation, AppError> {
            self.events.lock().unwrap().push("read");
            Ok(GameObservation::from_state(self.state.clone()))
        }

        fn shutdown_session(&mut self) -> Result<(), AppError> {
            self.events.lock().unwrap().push("shutdown");
            if self.cleanup_fails {
                Err(input_error(std::io::Error::other("cleanup failure")))
            } else {
                Ok(())
            }
        }
    }

    impl ExecutionPort for Port {
        fn bind_current_session(&mut self) -> Result<(), AppError> {
            self.events.lock().unwrap().push("bind");
            Ok(())
        }

        fn target_identity(&mut self, _: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
            self.identities += 1;
            ExecutionTargetIdentity::new(
                if self.target_changes && self.identities > 1 {
                    "b"
                } else {
                    "a"
                }
                .repeat(64),
            )
        }

        fn preflight_plan(&mut self, _: &ExecutionPreflight) -> Result<(), AppError> {
            self.events.lock().unwrap().push("preflight");
            Ok(())
        }

        fn send_command(&mut self, _: &ExecutionCommand) -> ExecutionSendResult {
            self.events.lock().unwrap().push("send");
            ExecutionSendResult::NotSent(input_error(std::io::Error::other("command rejected")))
        }

        fn query_command(
            &mut self,
            _: &str,
            _: Duration,
        ) -> Result<ExecutionCommandReceipt, AppError> {
            panic!("未发送命令不应查询")
        }
        fn cancel_command(
            &mut self,
            _: &str,
            _: Duration,
        ) -> Result<ExecutionCommandReceipt, AppError> {
            panic!("未发送命令不应取消")
        }
    }

    fn service(
        cleanup_fails: bool,
        target_changes: bool,
    ) -> (DirectActionService, Arc<Mutex<Vec<&'static str>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let port = Port {
            state: plan_game_state(),
            events: events.clone(),
            cleanup_fails,
            target_changes,
            identities: 0,
        };
        (DirectActionService::new(Some(Box::new(port))), events)
    }

    fn batch(json: &str) -> DirectActionBatch {
        serde_json::from_str(json).unwrap()
    }
    fn unequip(slot_index: u8) -> DirectActionBatch {
        DirectActionBatch {
            actions: vec![DirectAction::Unequip {
                target: DirectShipSlot {
                    ship_id: 9001,
                    slot_index,
                },
            }],
        }
    }

    #[test]
    fn standalone_compose_keeps_output_in_warehouse_and_checks_aggregate_resources() {
        use crate::application::ResourceKey;
        use crate::application::test_support::plan_game_state_with_compose;
        let request = batch(r#"{"actions":[{"action":"compose","recipe_id":5001,"count":2}]}"#);
        let state = plan_game_state_with_compose(1000, 20, 3, 5, Some(4));
        let report = request.compile(&state).unwrap();
        assert!(matches!(
            report.plan().steps(),
            [PlanStep::Compose { quantity: 2, .. }]
        ));
        assert!(
            report
                .plan()
                .resource_delta()
                .changes()
                .iter()
                .any(
                    |change| change.key() == ResourceKey::WarehouseEquipment { config_id: 1000 }
                        && change.delta() == 2
                )
        );
        for state in [
            plan_game_state_with_compose(199, 20, 3, 5, Some(4)),
            plan_game_state_with_compose(1000, 9, 3, 5, Some(4)),
            plan_game_state_with_compose(1000, 20, 3, 4, Some(4)),
            plan_game_state_with_compose(1000, 20, 3, 5, Some(1)),
        ] {
            assert!(request.compile(&state).is_err());
        }
        assert!(
            batch(r#"{"actions":[{"action":"compose","recipe_id":5001,"count":0}]}"#)
                .compile(&state)
                .is_err()
        );
    }

    #[test]
    fn standalone_composition_reaches_the_shared_executor() {
        use crate::application::test_support::plan_game_state_with_compose;
        let events = Arc::new(Mutex::new(Vec::new()));
        let port = Port {
            state: plan_game_state_with_compose(1000, 20, 3, 5, Some(4)),
            events: events.clone(),
            cleanup_fails: false,
            target_changes: false,
            identities: 0,
        };
        let mut service = DirectActionService::new(Some(Box::new(port)));
        let request = batch(r#"{"actions":[{"action":"compose","recipe_id":5001,"count":2}]}"#);
        let outcome = service.apply(&request).unwrap();
        assert_eq!(outcome.log_terminal(), OperationTerminal::Failed);
        let events = events.lock().unwrap();
        assert!(events.contains(&"preflight"));
        assert_eq!(events.iter().filter(|event| **event == "send").count(), 1);
        assert_eq!(events.last(), Some(&"shutdown"));
    }

    #[test]
    fn standalone_and_equipped_composition_share_reservations() {
        use crate::application::test_support::plan_game_state_with_compose;
        let request = batch(
            r#"{"actions":[{"action":"compose","recipe_id":5001,"count":2},{"action":"equip_family","target":{"ship_id":9001,"slot_index":2},"family_id":1000,"policy":"compose-only","target_level":0}]}"#,
        );
        let report = request
            .compile(&plan_game_state_with_compose(1000, 20, 3, 5, Some(4)))
            .unwrap();
        assert!(matches!(
            report.plan().steps(),
            [
                PlanStep::Compose { quantity: 1, .. },
                PlanStep::Equip { .. },
                PlanStep::Compose { quantity: 2, .. }
            ]
        ));
        assert!(
            request
                .compile(&plan_game_state_with_compose(299, 20, 3, 5, Some(4)))
                .is_err()
        );
        assert!(
            request
                .compile(&plan_game_state_with_compose(1000, 20, 3, 5, Some(2)))
                .is_err()
        );
    }

    #[test]
    fn strict_json_rejects_unknown_fields_at_every_boundary() {
        for json in [
            r#"{"actions":[],"apply":true}"#,
            r#"{"actions":[{"action":"unequip","target":{"ship_id":9001,"slot_index":1},"quantity":1}]}"#,
            r#"{"actions":[{"action":"unequip","target":{"ship_id":9001,"slot_index":1,"id":2}}]}"#,
            r#"{"actions":[{"action":"dismantle","source":{"kind":"warehouse","config_id":1000,"family_id":1000},"quantity":1}]}"#,
        ] {
            assert!(
                serde_json::from_str::<DirectActionBatch>(json).is_err(),
                "{json}"
            );
        }
    }

    #[test]
    fn invalid_inputs_never_open_a_game_connection() {
        for batch in [
            DirectActionBatch { actions: vec![] },
            unequip(0),
            batch(
                r#"{"actions":[{"action":"enhance","source":{"kind":"ship_slot","ship_id":9001,"slot_index":1},"target_level":1,"quantity":2}]}"#,
            ),
        ] {
            let (mut service, events) = service(false, false);
            assert_eq!(
                service.check(&batch).unwrap_err().code(),
                AppErrorCode::InputInvalid
            );
            assert!(events.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn checking_never_binds_or_sends_and_always_closes_session() {
        let (mut service, events) = service(false, false);
        let outcome = service.check(&unequip(1)).unwrap();
        assert!(outcome.execution.is_none());
        assert!(matches!(
            outcome.check.plan().steps(),
            [PlanStep::Unequip { .. }]
        ));
        assert_eq!(*events.lock().unwrap(), ["read", "shutdown"]);
    }

    #[test]
    fn checked_execution_rejects_changed_plan_before_binding() {
        let (mut checked, _) = service(false, false);
        let hash = checked
            .check(&unequip(1))
            .unwrap()
            .check
            .plan()
            .content_sha256()
            .to_owned();
        let (mut changed, events) = service(false, false);
        let error = changed
            .apply_checked(&unequip(2), &hash, &NoExecutionCancellation, &mut |_| {})
            .unwrap_err();
        assert_eq!(error.code(), AppErrorCode::EquipmentStateChanged);
        assert_eq!(*events.lock().unwrap(), ["read", "shutdown"]);
        let (mut same, events) = service(false, false);
        same.apply_checked(&unequip(1), &hash, &NoExecutionCancellation, &mut |_| {})
            .unwrap();
        assert!(events.lock().unwrap().contains(&"send"));
    }

    #[test]
    fn exact_sources_preserve_configuration_and_ship_origin() {
        for source in [
            r#"{"kind":"warehouse","config_id":1001}"#,
            r#"{"kind":"ship_slot","ship_id":9001,"slot_index":4}"#,
        ] {
            let batch = batch(&format!(
                r#"{{"actions":[{{"action":"equip","target":{{"ship_id":9001,"slot_index":2}},"source":{source}}}]}}"#
            ));
            let report = batch.compile(&plan_game_state()).unwrap();
            assert!(report.plan().steps().iter().any(|step| matches!(step, PlanStep::Equip { equipment, .. } if equipment.config_id() == 1001)));
        }
    }

    #[test]
    fn enhancement_resources_and_protected_dismantle_use_existing_validation() {
        let enhance = batch(
            r#"{"actions":[{"action":"enhance","source":{"kind":"warehouse","config_id":1000},"target_level":2,"quantity":1}]}"#,
        );
        let report = enhance
            .compile(&plan_game_state_with_enhance(100, 10, 3, 300))
            .unwrap();
        assert_eq!(
            report
                .plan()
                .steps()
                .iter()
                .filter(|step| matches!(step, PlanStep::Enhance { .. }))
                .count(),
            2
        );
        assert!(
            enhance
                .compile(&plan_game_state_with_enhance(0, 0, 3, 300))
                .is_err()
        );
        let protected = batch(
            r#"{"actions":[{"action":"dismantle","source":{"kind":"warehouse","config_id":1001},"quantity":1}]}"#,
        );
        assert_eq!(
            protected.compile(&plan_game_state()).unwrap_err().code(),
            AppErrorCode::InputInvalid
        );
        let dismantle = batch(
            r#"{"actions":[{"action":"dismantle","source":{"kind":"warehouse","config_id":1000},"quantity":1}]}"#,
        );
        assert!(matches!(
            dismantle
                .compile(&plan_game_state())
                .unwrap()
                .plan()
                .steps(),
            [PlanStep::Dismantle { .. }]
        ));
    }

    #[test]
    fn conflicting_batch_is_rejected_before_any_write() {
        let mut batch = unequip(1);
        batch.actions.push(batch.actions[0].clone());
        let (mut service, events) = service(false, false);
        assert!(service.apply(&batch).is_err());
        assert_eq!(*events.lock().unwrap(), ["read", "shutdown"]);
    }

    #[test]
    fn apply_preserves_execution_failure_and_cleanup_failure() {
        let (mut service, events) = service(true, false);
        let mut batch = unequip(1);
        batch.actions.push(DirectAction::Unequip {
            target: DirectShipSlot {
                ship_id: 9001,
                slot_index: 4,
            },
        });
        let outcome = service.apply(&batch).unwrap();
        assert_eq!(outcome.log_terminal(), OperationTerminal::Failed);
        assert!(matches!(outcome.cleanup, SessionCleanup::Failed(_)));
        let execution = outcome.execution.unwrap();
        assert_eq!(
            execution.status(),
            crate::application::ExecutionReportStatus::Failed
        );
        assert_eq!(execution.steps().len(), 2);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| **event == "send")
                .count(),
            1
        );
        assert_eq!(events.lock().unwrap().last(), Some(&"shutdown"));
    }

    #[test]
    fn changed_target_is_rejected_before_preflight_or_send() {
        let (mut service, events) = service(false, true);
        assert_eq!(
            service.apply(&unequip(1)).unwrap_err().code(),
            AppErrorCode::EquipmentStateChanged
        );
        assert_eq!(
            *events.lock().unwrap(),
            ["read", "bind", "read", "shutdown"]
        );
    }

    #[test]
    fn apply_noop_still_uses_execution_preflight_and_final_readback() {
        let (mut service, events) = service(false, false);
        let outcome = service.apply(&unequip(2)).unwrap();
        assert_eq!(outcome.log_terminal(), OperationTerminal::Succeeded);
        assert!(outcome.execution.is_some());
        assert!(events.lock().unwrap().contains(&"preflight"));
        assert!(!events.lock().unwrap().contains(&"send"));
    }
}

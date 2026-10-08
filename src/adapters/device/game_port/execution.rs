//! 将应用层装备动作映射为运行态命令，并复核写入前置状态与收据。

use super::super::portable::PortableProbeError;
use super::super::probe::RuntimeProbeError;
use super::super::runtime::{
    CapabilitiesResult, ClientStage, EquipmentCommandAction, EquipmentCommandEquipment,
    EquipmentCommandMaterialCost, EquipmentCommandReceipt, EquipmentCommandStatus,
    RuntimeClientError, RuntimeEquipmentCommand, SessionEffect,
};
use super::errors::{execution_session_error, map_portable_error};
use super::{PortableGamePort, SessionConnection};
use crate::application::{
    AppError, AppErrorCode, ExecutionAction, ExecutionCommand, ExecutionCommandReceipt,
    ExecutionPort, ExecutionPreflight, ExecutionSendResult, ExecutionStatus,
    ExecutionTargetIdentity, PlanEnhanceCost, PlanEquipment, PlanSlot, PlanSource,
};
use crate::domain::{GameState, ShipEquipment};

impl ExecutionPort for PortableGamePort {
    fn bind_current_session(&mut self) -> Result<(), AppError> {
        if self.cleanup_pending() {
            return Err(execution_session_error(
                "执行会话绑定前的运行态连接已经失效",
            ));
        }
        let current = std::mem::replace(&mut self.connection, SessionConnection::Disconnected);
        let SessionConnection::Active(live) = current else {
            self.connection = current;
            return Err(execution_session_error("执行会话绑定需要仍处于活动状态"));
        };
        if !live.session.matches_options(&live.options) {
            self.connection = SessionConnection::Active(live);
            return Err(execution_session_error(
                "执行会话绑定发现活动连接与其配置身份不一致",
            ));
        }
        self.connection = SessionConnection::Bound(live);
        Ok(())
    }

    fn target_identity(&mut self, state: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
        self.ensure_current_state(state)?;
        self.execution_target_identity(state)
    }

    fn preflight_plan(&mut self, preflight: &ExecutionPreflight) -> Result<(), AppError> {
        let state = self
            .observed_state()
            .ok_or_else(|| execution_session_error("执行预检缺少同一会话的完整状态"))?;
        if state.source().content_sha256() != preflight.initial_state_content_sha256() {
            return Err(execution_state_changed_error(
                preflight.initial_state_content_sha256(),
                state.source().content_sha256(),
            ));
        }
        let actual_target = self.execution_target_identity(state)?;
        if actual_target != *preflight.target_identity() {
            return Err(execution_target_changed_error(
                preflight.target_identity(),
                &actual_target,
            ));
        }
        let capabilities = self
            .live()
            .ok_or_else(|| execution_session_error("执行预检需要仍处于活动状态"))?
            .session
            .last_capabilities()
            .map_err(map_execution_portable_error)?;
        for step in preflight.steps() {
            let capability = match step.action() {
                ExecutionAction::Unequip { .. } => "write.unequip",
                ExecutionAction::Equip { .. } => "write.equip",
                ExecutionAction::Dismantle { .. } => "write.destroy",
                ExecutionAction::Compose { .. } => "write.compose",
                ExecutionAction::Enhance { .. } => "write.enhance",
            };
            require_write_capability(&capabilities, capability)?;
            validate_preflight_action(state, step.action())?;
        }
        Ok(())
    }

    fn send_command(&mut self, command: &ExecutionCommand) -> ExecutionSendResult {
        let runtime_command = match self.build_runtime_command(command) {
            Ok(command) => command,
            Err(error) => return ExecutionSendResult::NotSent(error),
        };
        let result = self
            .live_mut()
            .ok_or_else(|| execution_session_error("发送装备命令需要仍处于活动状态"))
            .and_then(|live| {
                let session = &mut live.session;
                session
                    .execute_equipment_command(&runtime_command)
                    .map_err(map_execution_portable_error)
            });
        match result {
            Ok(receipt) => ExecutionSendResult::Receipt(map_runtime_receipt(command, receipt)),
            Err(error) if execution_error_is_not_sent(&error) => {
                ExecutionSendResult::NotSent(error)
            }
            Err(error) => ExecutionSendResult::Receipt(unknown_receipt(command, error)),
        }
    }

    fn query_command(
        &mut self,
        command_id: &str,
        budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        let receipt = self
            .live_mut()
            .ok_or_else(|| execution_session_error("查询装备命令需要仍处于活动状态"))?
            .session
            .query_equipment_command(command_id, budget)
            .map_err(map_execution_portable_error)?;
        Ok(map_runtime_receipt_fields(command_id, receipt))
    }

    fn cancel_command(
        &mut self,
        command_id: &str,
        budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        let receipt = self
            .live_mut()
            .ok_or_else(|| execution_session_error("取消装备命令需要仍处于活动状态"))?
            .session
            .cancel_equipment_command(command_id, budget)
            .map_err(map_execution_portable_error)?;
        Ok(map_runtime_receipt_fields(command_id, receipt))
    }
}

impl PortableGamePort {
    fn execution_target_identity(
        &self,
        state: &GameState,
    ) -> Result<ExecutionTargetIdentity, AppError> {
        let (serial, package_name) = self
            .live()
            .ok_or_else(|| execution_session_error("目标身份读取需要仍处于活动状态"))?
            .session
            .target_scope()
            .map_err(map_execution_portable_error)?;
        ExecutionTargetIdentity::from_runtime_scope(&serial, &package_name, state)
    }
    fn ensure_current_state(&self, state: &GameState) -> Result<(), AppError> {
        let current = self
            .observed_state()
            .ok_or_else(|| execution_session_error("执行端口没有同一会话的完整状态"))?;
        if current.source().content_sha256() != state.source().content_sha256() {
            return Err(execution_state_changed_error(
                current.source().content_sha256(),
                state.source().content_sha256(),
            ));
        }
        Ok(())
    }

    pub(super) fn build_runtime_command(
        &self,
        command: &ExecutionCommand,
    ) -> Result<RuntimeEquipmentCommand, AppError> {
        let state = self
            .observed_state()
            .ok_or_else(|| execution_session_error("发送装备命令没有同一会话的完整状态"))?;
        if state.source().content_sha256() != command.pre_state_content_sha256() {
            return Err(execution_state_changed_error(
                command.pre_state_content_sha256(),
                state.source().content_sha256(),
            ));
        }
        let actual_target = self.execution_target_identity(state)?;
        if actual_target.fingerprint_sha256() != command.target_fingerprint_sha256() {
            return Err(execution_target_changed_error(
                &ExecutionTargetIdentity::new(command.target_fingerprint_sha256().to_owned())?,
                &actual_target,
            ));
        }
        let action = runtime_action(state, command.action())?;
        RuntimeEquipmentCommand::new(
            command.command_id(),
            command.target_fingerprint_sha256(),
            command.plan_hash(),
            command.sequence(),
            command.pre_state_content_sha256(),
            action,
        )
        .map_err(|source| execution_protocol_error("运行态装备命令编码失败", source))
    }
}

/// 整批预检只核对不随前序步骤变化的静态契约；实际来源和局部前态在逐条发送前复核。
pub(super) fn validate_preflight_action(
    state: &GameState,
    action: ExecutionAction,
) -> Result<(), AppError> {
    match action {
        ExecutionAction::Unequip { slot } => {
            let _ = find_ship_slot(state, slot)?;
        }
        ExecutionAction::Equip {
            slot,
            source,
            equipment,
        } => {
            let _target = find_ship_slot(state, slot)?;
            match source {
                PlanSource::Warehouse { config_id } => {
                    if config_id != equipment.config_id() {
                        return Err(execution_precondition_error(
                            "仓库来源配置与计划装备不一致",
                            "warehouse source does not match planned equipment",
                        ));
                    }
                }
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => {
                    let _ =
                        find_ship_slot(state, PlanSlot::from_raw(ship_instance_id, slot_index))?;
                }
                PlanSource::Compose { recipe_id } => {
                    validate_compose_equipment(state, recipe_id, equipment)?;
                }
            }
        }
        ExecutionAction::Dismantle {
            source,
            equipment,
            quantity,
        } => {
            if quantity == 0 {
                return Err(execution_precondition_error(
                    "拆解数量必须大于零",
                    "dismantle quantity must be positive",
                ));
            }
            match source {
                PlanSource::Warehouse { config_id } => {
                    if config_id != equipment.config_id() {
                        return Err(execution_precondition_error(
                            "仓库拆解来源配置与计划装备不一致",
                            "warehouse dismantle source does not match planned equipment",
                        ));
                    }
                    validate_dismantle_definition(state, equipment)?;
                }
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => {
                    let _ =
                        find_ship_slot(state, PlanSlot::from_raw(ship_instance_id, slot_index))?;
                    if quantity != 1 {
                        return Err(execution_precondition_error(
                            "舰船拆解数量与计划位置不一致",
                            "ship dismantle source does not match planned equipment",
                        ));
                    }
                    validate_dismantle_definition(state, equipment)?;
                }
                PlanSource::Compose { recipe_id } => {
                    return Err(execution_precondition_error(
                        "拆解步骤不能直接使用合成配方来源",
                        "dismantle action cannot consume a compose source",
                    )
                    .with_context("recipe_id", recipe_id.to_string()));
                }
            }
        }
        ExecutionAction::Compose {
            recipe_id,
            equipment,
            quantity,
            material_id,
            material_quantity_per_unit,
            gold_per_unit,
        } => validate_compose_action(
            state,
            recipe_id,
            equipment,
            quantity,
            material_id,
            material_quantity_per_unit,
            gold_per_unit,
            false,
        )?,
        ExecutionAction::Enhance {
            source,
            source_equipment,
            target_equipment,
            cost,
        } => {
            validate_enhance_action(state, source_equipment, target_equipment, &cost)?;
            match source {
                PlanSource::Warehouse { config_id } => {
                    if config_id != source_equipment.config_id() {
                        return Err(execution_precondition_error(
                            "仓库强化来源配置与计划装备不一致",
                            "warehouse enhance source does not match planned equipment",
                        ));
                    }
                }
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => {
                    let _ =
                        find_ship_slot(state, PlanSlot::from_raw(ship_instance_id, slot_index))?;
                }
                PlanSource::Compose { recipe_id } => {
                    return Err(execution_precondition_error(
                        "强化步骤必须引用实际仓库或舰船槽位",
                        "enhance action cannot use a compose source",
                    )
                    .with_context("recipe_id", recipe_id.to_string()));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn runtime_action(
    state: &GameState,
    action: ExecutionAction,
) -> Result<EquipmentCommandAction, AppError> {
    match action {
        ExecutionAction::Unequip { slot } => {
            let target = find_ship_slot(state, slot)?
                .ok_or_else(|| execution_state_changed_error("occupied", "empty"))?;
            let target_before = equipment_command_equipment(target)?;
            let resources = state.resources();
            EquipmentCommandAction::unequip(
                slot.ship_instance_id(),
                u32::from(slot.slot_index()),
                target_before,
                find_warehouse_quantity_by_runtime_id(state, target.runtime_id()),
                resources.equipment_capacity(),
                resources.equipment_limit(),
            )
            .map_err(|source| execution_protocol_error("卸下装备命令前置条件编码失败", source))
        }
        ExecutionAction::Equip {
            slot,
            source,
            equipment,
        } => {
            let target = find_ship_slot(state, slot)?;
            let target_before = target.map(equipment_command_equipment).transpose()?;
            let source_stack = match source {
                PlanSource::Warehouse { config_id } => find_warehouse_stack(state, config_id)
                    .ok_or_else(|| execution_state_changed_error("warehouse_source", "missing"))?,
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => {
                    let source_slot =
                        find_ship_slot(state, PlanSlot::from_raw(ship_instance_id, slot_index))?;
                    if source_slot.is_some() {
                        return Err(execution_precondition_error(
                            "舰船来源必须在前序卸下后从同一会话仓库解析",
                            "ship source slot is not empty after its preceding unequip",
                        ));
                    }
                    find_warehouse_stack(state, equipment.config_id()).ok_or_else(|| {
                        execution_state_changed_error("ship_source_warehouse", "missing")
                    })?
                }
                PlanSource::Compose { recipe_id } => {
                    validate_compose_equipment(state, recipe_id, equipment)?;
                    find_warehouse_stack(state, equipment.config_id()).ok_or_else(|| {
                        execution_state_changed_error("compose_source_warehouse", "missing")
                    })?
                }
            };
            if source_stack.config_id().get() != equipment.config_id()
                || source_stack.enhance_level().get() != equipment.enhance_level()
            {
                return Err(execution_precondition_error(
                    "来源装备的配置或强化等级与计划不一致",
                    "runtime source does not match planned equipment",
                ));
            }
            let source_before = equipment_command_equipment_from_stack(source_stack)?;
            let resources = state.resources();
            EquipmentCommandAction::equip(
                slot.ship_instance_id(),
                u32::from(slot.slot_index()),
                target_before,
                source_before,
                source_stack.quantity(),
                target.map_or(0, |target| {
                    find_warehouse_quantity_by_runtime_id(state, target.runtime_id())
                }),
                resources.equipment_capacity(),
                resources.equipment_limit(),
            )
            .map_err(|source| execution_protocol_error("装上装备命令前置条件编码失败", source))
        }
        ExecutionAction::Dismantle {
            source,
            equipment,
            quantity,
        } => {
            match source {
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => {
                    let source_slot =
                        find_ship_slot(state, PlanSlot::from_raw(ship_instance_id, slot_index))?;
                    if source_slot.is_some() {
                        return Err(execution_precondition_error(
                            "舰船拆解来源必须先卸下到同一会话仓库",
                            "ship dismantle source slot is not empty after its preceding unequip",
                        ));
                    }
                }
                PlanSource::Warehouse { .. } => {}
                PlanSource::Compose { recipe_id } => {
                    return Err(execution_precondition_error(
                        "拆解步骤不能直接使用合成配方来源",
                        "dismantle action cannot consume a compose source",
                    )
                    .with_context("recipe_id", recipe_id.to_string()));
                }
            }
            let source_stack = find_warehouse_stack(state, equipment.config_id())
                .ok_or_else(|| execution_state_changed_error("dismantle_source", "missing"))?;
            validate_dismantle_stack(state, source_stack, equipment, quantity)?;
            let source_before = equipment_command_equipment_from_stack(source_stack)?;
            let resources = state.resources();
            EquipmentCommandAction::dismantle(
                source_before,
                source_stack.quantity(),
                quantity,
                resources.equipment_capacity(),
                resources.equipment_limit(),
            )
            .map_err(|source| execution_protocol_error("拆解装备命令前置条件编码失败", source))
        }
        ExecutionAction::Compose {
            recipe_id,
            equipment,
            quantity,
            material_id,
            material_quantity_per_unit,
            gold_per_unit,
        } => {
            validate_compose_action(
                state,
                recipe_id,
                equipment,
                quantity,
                material_id,
                material_quantity_per_unit,
                gold_per_unit,
                true,
            )?;
            let output_stack = find_warehouse_stack(state, equipment.config_id());
            let output_before = output_stack
                .map(equipment_command_equipment_from_stack)
                .transpose()?;
            let material_quantity_before = state
                .bag()
                .item(material_id)
                .map_or(0, |item| item.quantity());
            let resources = state.resources();
            EquipmentCommandAction::compose(
                recipe_id,
                quantity,
                equipment.config_id(),
                output_before,
                output_stack.map_or(0, |stack| stack.quantity()),
                material_id,
                material_quantity_before,
                material_quantity_per_unit,
                resources.gold(),
                gold_per_unit,
                resources.equipment_capacity(),
                resources.equipment_limit(),
            )
            .map_err(|source| execution_protocol_error("合成装备命令前置条件编码失败", source))
        }
        ExecutionAction::Enhance {
            source,
            source_equipment,
            target_equipment,
            cost,
        } => {
            validate_enhance_action(state, source_equipment, target_equipment, &cost)?;
            let materials = equipment_command_material_costs(state, &cost)?;
            let resources = state.resources();
            match source {
                PlanSource::Warehouse { config_id } => {
                    if config_id != source_equipment.config_id() {
                        return Err(execution_precondition_error(
                            "仓库强化来源配置与计划装备不一致",
                            "warehouse enhance source does not match planned equipment",
                        ));
                    }
                    let source_stack = find_warehouse_stack(state, config_id).ok_or_else(|| {
                        execution_state_changed_error("warehouse_enhance_source", "missing")
                    })?;
                    if source_stack.enhance_level().get() != source_equipment.enhance_level() {
                        return Err(execution_precondition_error(
                            "仓库强化来源等级与计划不一致",
                            "warehouse enhance level does not match planned equipment",
                        ));
                    }
                    let source_before = equipment_command_equipment_from_stack(source_stack)?;
                    let target_stack = find_warehouse_stack(state, target_equipment.config_id());
                    let target_before = target_stack
                        .map(equipment_command_equipment_from_stack)
                        .transpose()?;
                    EquipmentCommandAction::enhance_warehouse(
                        source_before,
                        source_stack.quantity(),
                        target_equipment.config_id(),
                        u32::from(target_equipment.enhance_level()),
                        target_before,
                        target_stack.map_or(0, |stack| stack.quantity()),
                        materials,
                        resources.gold(),
                        cost.gold(),
                        resources.equipment_capacity(),
                        resources.equipment_limit(),
                    )
                    .map_err(|source| {
                        execution_protocol_error("仓库强化命令前置条件编码失败", source)
                    })
                }
                PlanSource::ShipSlot {
                    ship_instance_id,
                    slot_index,
                } => {
                    let slot = PlanSlot::from_raw(ship_instance_id, slot_index);
                    let source = find_ship_slot(state, slot)?.ok_or_else(|| {
                        execution_state_changed_error("ship_enhance_source", "missing")
                    })?;
                    if source.config_id().get() != source_equipment.config_id()
                        || source.enhance_level().get() != source_equipment.enhance_level()
                    {
                        return Err(execution_precondition_error(
                            "舰上强化来源与计划装备不一致",
                            "ship enhance source does not match planned equipment",
                        ));
                    }
                    EquipmentCommandAction::enhance_ship(
                        ship_instance_id,
                        u32::from(slot_index),
                        equipment_command_equipment(source)?,
                        target_equipment.config_id(),
                        u32::from(target_equipment.enhance_level()),
                        materials,
                        resources.gold(),
                        cost.gold(),
                        resources.equipment_capacity(),
                        resources.equipment_limit(),
                    )
                    .map_err(|source| {
                        execution_protocol_error("舰上强化命令前置条件编码失败", source)
                    })
                }
                PlanSource::Compose { recipe_id } => Err(execution_precondition_error(
                    "强化步骤必须引用实际仓库或舰船槽位",
                    "enhance action cannot use a compose source",
                )
                .with_context("recipe_id", recipe_id.to_string())),
            }
        }
    }
}

fn find_ship_slot(state: &GameState, slot: PlanSlot) -> Result<Option<ShipEquipment>, AppError> {
    let ship = state
        .ships()
        .ships()
        .iter()
        .find(|ship| ship.identity().instance_id().get() == slot.ship_instance_id())
        .ok_or_else(|| {
            execution_precondition_error(
                "计划引用的舰船不存在",
                "execution plan references a missing ship",
            )
            .with_context("ship_instance_id", slot.ship_instance_id().to_string())
        })?;
    let equipment = ship
        .slots()
        .iter()
        .find(|candidate| u64::from(candidate.index().get()) == u64::from(slot.slot_index()))
        .ok_or_else(|| {
            execution_precondition_error(
                "计划引用的舰船槽位不存在",
                "execution plan references a missing ship slot",
            )
            .with_context("slot_index", slot.slot_index().to_string())
        })?
        .equipment();
    Ok(equipment)
}

fn find_warehouse_stack(
    state: &GameState,
    config_id: u64,
) -> Option<crate::domain::WarehouseEquipmentStack> {
    state
        .equipment_inventory()
        .warehouse()
        .iter()
        .copied()
        .find(|stack| stack.config_id().get() == config_id)
}

pub(super) fn find_warehouse_quantity_by_runtime_id(state: &GameState, runtime_id: u64) -> u64 {
    state
        .equipment_inventory()
        .warehouse()
        .iter()
        .find(|stack| stack.runtime_group_id() == runtime_id)
        .map_or(0, |stack| stack.quantity())
}

fn validate_dismantle_stack(
    state: &GameState,
    stack: crate::domain::WarehouseEquipmentStack,
    equipment: crate::application::PlanEquipment,
    quantity: u64,
) -> Result<(), AppError> {
    if stack.config_id().get() != equipment.config_id()
        || stack.enhance_level().get() != equipment.enhance_level()
        || quantity == 0
        || quantity > stack.quantity()
    {
        return Err(execution_precondition_error(
            "仓库拆解来源的配置、强化等级或数量与计划不一致",
            "warehouse dismantle source does not match planned equipment",
        ));
    }
    validate_dismantle_definition(state, equipment)
}

fn validate_dismantle_definition(
    state: &GameState,
    equipment: crate::application::PlanEquipment,
) -> Result<(), AppError> {
    let definition = state
        .equipment_catalog()
        .families()
        .iter()
        .flat_map(|family| family.configs())
        .find(|definition| definition.identity().config_id().get() == equipment.config_id())
        .ok_or_else(|| execution_state_changed_error("equipment_definition", "missing"))?;
    let safety = definition.dismantle_safety();
    if definition.identity().family_id().get() != equipment.family_id()
        || definition.enhancement().level().get() != equipment.enhance_level()
        || !safety.allows_automatic_dismantle()
    {
        return Err(execution_precondition_error(
            "拆解来源不再满足低品质、未强化且非重要的安全边界",
            "dismantle source no longer satisfies the automatic safety boundary",
        ));
    }
    Ok(())
}

fn validate_compose_equipment(
    state: &GameState,
    recipe_id: u64,
    equipment: crate::application::PlanEquipment,
) -> Result<&crate::domain::EquipmentComposeRecipe, AppError> {
    let recipe = state
        .equipment_catalog()
        .recipes()
        .iter()
        .find(|recipe| recipe.recipe_id() == recipe_id)
        .ok_or_else(|| execution_state_changed_error("compose_recipe", "missing"))?;
    let definition = state
        .equipment_catalog()
        .families()
        .iter()
        .flat_map(|family| family.configs())
        .find(|definition| definition.identity().config_id() == recipe.equipment_config_id())
        .ok_or_else(|| execution_state_changed_error("compose_output_definition", "missing"))?;
    if recipe.equipment_config_id().get() != equipment.config_id()
        || definition.identity().family_id().get() != equipment.family_id()
        || definition.enhancement().level().get() != equipment.enhance_level()
        || equipment.enhance_level() != 0
    {
        return Err(execution_precondition_error(
            "合成配方产物与计划装备不一致",
            "compose recipe output does not match planned equipment",
        )
        .with_context("recipe_id", recipe_id.to_string()));
    }
    Ok(recipe)
}

#[allow(clippy::too_many_arguments)]
fn validate_compose_action(
    state: &GameState,
    recipe_id: u64,
    equipment: crate::application::PlanEquipment,
    quantity: u64,
    material_id: u64,
    material_quantity_per_unit: u64,
    gold_per_unit: u64,
    enforce_current_capacity: bool,
) -> Result<(), AppError> {
    if quantity == 0 {
        return Err(execution_precondition_error(
            "合成数量必须大于零",
            "compose quantity must be positive",
        ));
    }
    let recipe = validate_compose_equipment(state, recipe_id, equipment)?;
    let material = recipe.material();
    let live = state
        .bag()
        .item(recipe_id)
        .and_then(|item| item.compose())
        .ok_or_else(|| execution_state_changed_error("compose_recipe_available", "missing"))?;
    if material.item_id() != material_id
        || material.quantity() != material_quantity_per_unit
        || recipe.gold() != gold_per_unit
        || live.recipe_id() != recipe_id
        || live.material_id() != material_id
        || live.material_count() != material_quantity_per_unit
        || live.gold() != gold_per_unit
        || live.equipment_config_id() != Some(recipe.equipment_config_id())
        || live
            .max_count()
            .is_some_and(|max_count| quantity > max_count)
    {
        return Err(execution_precondition_error(
            "合成配方、当前可用次数或资源成本与计划不一致",
            "compose availability does not match the planned action",
        )
        .with_context("recipe_id", recipe_id.to_string()));
    }
    let material_required = material_quantity_per_unit
        .checked_mul(quantity)
        .ok_or_else(|| {
            execution_precondition_error(
                "合成材料成本超出稳定数量范围",
                "compose material cost overflowed",
            )
        })?;
    let material_available = state
        .bag()
        .item(material_id)
        .map_or(0, |item| item.quantity());
    let gold_required = gold_per_unit.checked_mul(quantity).ok_or_else(|| {
        execution_precondition_error(
            "合成物资成本超出稳定数量范围",
            "compose gold cost overflowed",
        )
    })?;
    if material_required > material_available || gold_required > state.resources().gold() {
        return Err(execution_precondition_error(
            "当前材料或物资不足以执行合成计划",
            "compose resources are below the planned cost",
        )
        .with_context("recipe_id", recipe_id.to_string())
        .with_context("material_available", material_available.to_string())
        .with_context("material_required", material_required.to_string())
        .with_context("gold_available", state.resources().gold().to_string())
        .with_context("gold_required", gold_required.to_string()));
    }
    if enforce_current_capacity {
        let resources = state.resources();
        let capacity_after = resources
            .equipment_capacity()
            .checked_add(quantity)
            .ok_or_else(|| {
                execution_precondition_error(
                    "合成后的装备仓库容量超出稳定数量范围",
                    "compose equipment capacity overflowed",
                )
            })?;
        if capacity_after > resources.equipment_limit() {
            return Err(execution_precondition_error(
                "装备仓库没有合成操作所需的空位",
                "equipment warehouse has no free capacity for compose",
            )
            .with_context(
                "capacity_before",
                resources.equipment_capacity().to_string(),
            )
            .with_context("capacity_limit", resources.equipment_limit().to_string())
            .with_context("compose_quantity", quantity.to_string()));
        }
    }
    Ok(())
}

fn validate_enhance_action(
    state: &GameState,
    source_equipment: PlanEquipment,
    target_equipment: PlanEquipment,
    cost: &PlanEnhanceCost,
) -> Result<(), AppError> {
    if source_equipment.family_id() != target_equipment.family_id()
        || source_equipment.enhance_level().checked_add(1) != Some(target_equipment.enhance_level())
    {
        return Err(execution_precondition_error(
            "强化步骤的装备族或相邻等级关系无效",
            "enhance transition is not an adjacent configuration in one family",
        ));
    }
    let source_definition = state
        .equipment_catalog()
        .families()
        .iter()
        .flat_map(|family| family.configs())
        .find(|definition| definition.identity().config_id().get() == source_equipment.config_id())
        .ok_or_else(|| execution_state_changed_error("enhance_source_definition", "missing"))?;
    let target_definition = state
        .equipment_catalog()
        .families()
        .iter()
        .flat_map(|family| family.configs())
        .find(|definition| definition.identity().config_id().get() == target_equipment.config_id())
        .ok_or_else(|| execution_state_changed_error("enhance_target_definition", "missing"))?;
    if source_definition.identity().family_id().get() != source_equipment.family_id()
        || target_definition.identity().family_id().get() != target_equipment.family_id()
        || source_definition.enhancement().level().get() != source_equipment.enhance_level()
        || target_definition.enhancement().level().get() != target_equipment.enhance_level()
        || source_definition.enhancement().next_config_id()
            != Some(target_definition.identity().config_id())
        || target_definition.enhancement().previous_config_id()
            != Some(source_definition.identity().config_id())
    {
        return Err(execution_precondition_error(
            "强化来源、目标或静态配置链与计划不一致",
            "enhance definitions do not match the planned transition",
        )
        .with_context("source_config_id", source_equipment.config_id().to_string())
        .with_context("target_config_id", target_equipment.config_id().to_string()));
    }
    let expected_cost = source_definition.enhancement().next_cost();
    let materials_match = expected_cost.items().len() == cost.materials().len()
        && expected_cost
            .items()
            .iter()
            .zip(cost.materials())
            .all(|(expected, planned)| {
                expected.item_id() == planned.item_id() && expected.quantity() == planned.quantity()
            });
    if expected_cost.gold() != cost.gold() || !materials_match {
        return Err(execution_precondition_error(
            "强化成本与当前装备目录不一致",
            "enhance cost does not match the current equipment catalog",
        )
        .with_context("source_config_id", source_equipment.config_id().to_string()));
    }
    if cost.gold() > state.resources().gold() {
        return Err(execution_precondition_error(
            "当前物资不足以执行强化计划",
            "gold is below the planned enhance cost",
        )
        .with_context("gold_available", state.resources().gold().to_string())
        .with_context("gold_required", cost.gold().to_string()));
    }
    for material in cost.materials() {
        let available = state
            .bag()
            .item(material.item_id())
            .map_or(0, |item| item.quantity());
        if material.quantity() > available {
            return Err(execution_precondition_error(
                "当前背包材料不足以执行强化计划",
                "bag material is below the planned enhance cost",
            )
            .with_context("item_id", material.item_id().to_string())
            .with_context("material_available", available.to_string())
            .with_context("material_required", material.quantity().to_string()));
        }
    }
    Ok(())
}

fn equipment_command_material_costs(
    state: &GameState,
    cost: &PlanEnhanceCost,
) -> Result<Vec<EquipmentCommandMaterialCost>, AppError> {
    cost.materials()
        .iter()
        .map(|material| {
            let quantity_before = state
                .bag()
                .item(material.item_id())
                .map_or(0, |item| item.quantity());
            EquipmentCommandMaterialCost::new(
                material.item_id(),
                quantity_before,
                material.quantity(),
            )
            .map_err(|source| execution_protocol_error("强化材料前置条件编码失败", source))
        })
        .collect()
}

fn equipment_command_equipment(
    equipment: ShipEquipment,
) -> Result<EquipmentCommandEquipment, AppError> {
    EquipmentCommandEquipment::new(
        equipment.runtime_id(),
        equipment.config_id().get(),
        u32::from(equipment.enhance_level().get()),
    )
    .map_err(|source| execution_protocol_error("舰船装备前态编码失败", source))
}

fn equipment_command_equipment_from_stack(
    stack: crate::domain::WarehouseEquipmentStack,
) -> Result<EquipmentCommandEquipment, AppError> {
    EquipmentCommandEquipment::new(
        stack.runtime_group_id(),
        stack.config_id().get(),
        u32::from(stack.enhance_level().get()),
    )
    .map_err(|source| execution_protocol_error("仓库装备前态编码失败", source))
}

pub(super) fn require_write_capability(
    capabilities: &CapabilitiesResult,
    capability: &'static str,
) -> Result<(), AppError> {
    let status = capabilities
        .capabilities
        .get(capability)
        .ok_or_else(|| execution_capability_error(capability, "能力报告缺少装备写入能力"))?;
    if status.available && status.reason_code == "ready" {
        return Ok(());
    }
    Err(
        execution_capability_error(capability, "当前运行态未声明装备写入能力已就绪")
            .with_context("available", status.available.to_string())
            .with_context("reason_code", status.reason_code.clone()),
    )
}

fn map_runtime_receipt(
    command: &ExecutionCommand,
    receipt: EquipmentCommandReceipt,
) -> ExecutionCommandReceipt {
    map_runtime_receipt_fields(command.command_id(), receipt)
}

fn map_runtime_receipt_fields(
    command_id: &str,
    receipt: EquipmentCommandReceipt,
) -> ExecutionCommandReceipt {
    let status = match receipt.status {
        EquipmentCommandStatus::Success => ExecutionStatus::Success,
        EquipmentCommandStatus::Failed => ExecutionStatus::Failed,
        EquipmentCommandStatus::Unknown => ExecutionStatus::Unknown,
    };
    let mut diagnostics = std::collections::BTreeMap::new();
    diagnostics.insert(
        "phase".to_owned(),
        format!("{:?}", receipt.phase).to_lowercase(),
    );
    diagnostics.insert(
        "write_dispatched".to_owned(),
        receipt.write_dispatched.to_string(),
    );
    diagnostics.insert(
        "cancel_requested".to_owned(),
        receipt.cancel_requested.to_string(),
    );
    diagnostics.insert(
        "observation_count".to_owned(),
        receipt.observation_count.to_string(),
    );
    ExecutionCommandReceipt::new(
        command_id,
        status,
        Some("设备端已返回装备命令生命周期收据".to_owned()),
        receipt.error_code,
        receipt.message,
        diagnostics,
    )
}

fn unknown_receipt(command: &ExecutionCommand, error: AppError) -> ExecutionCommandReceipt {
    let mut diagnostics = error.context().clone();
    diagnostics.insert("error_stage".to_owned(), error.stage().to_owned());
    ExecutionCommandReceipt::new(
        command.command_id(),
        ExecutionStatus::Unknown,
        None,
        Some(error.code().as_str().to_owned()),
        Some(error.message().to_owned()),
        diagnostics,
    )
}

fn execution_error_is_not_sent(error: &AppError) -> bool {
    let mut source = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(portable) = current.downcast_ref::<PortableProbeError>() {
            return portable_error_is_not_sent(portable);
        }
        if let Some(runtime) = current.downcast_ref::<RuntimeProbeError>() {
            return runtime_error_is_not_sent(runtime);
        }
        if let Some(client) = current.downcast_ref::<RuntimeClientError>() {
            return runtime_client_error_is_not_sent(client);
        }
        source = current.source();
    }
    false
}

fn portable_error_is_not_sent(error: &PortableProbeError) -> bool {
    match error {
        PortableProbeError::Runtime(source) => runtime_error_is_not_sent(source),
        PortableProbeError::GameNotReady { source, .. } => runtime_error_is_not_sent(source),
        PortableProbeError::OperationAndSessionCleanup { operation, .. } => operation
            .downcast_ref::<PortableProbeError>()
            .is_some_and(portable_error_is_not_sent),
        _ => false,
    }
}

fn runtime_error_is_not_sent(error: &RuntimeProbeError) -> bool {
    match error {
        RuntimeProbeError::ProbeFailed { source, .. } => runtime_error_is_not_sent(source),
        RuntimeProbeError::RuntimeClient(source) => runtime_client_error_is_not_sent(source),
        RuntimeProbeError::InvalidOutput { stage, .. } => *stage == "session.equipment_command",
        _ => false,
    }
}

pub(super) fn runtime_client_error_is_not_sent(error: &RuntimeClientError) -> bool {
    match error {
        RuntimeClientError::Agent { error, .. } => error.session_effect == SessionEffect::Unchanged,
        RuntimeClientError::SessionUnusable => true,
        RuntimeClientError::Io {
            stage: ClientStage::ConfigureSocket,
            ..
        } => true,
        RuntimeClientError::HandshakeAttemptsExhausted { .. }
        | RuntimeClientError::Io { .. }
        | RuntimeClientError::Json { .. }
        | RuntimeClientError::FrameTooLarge { .. }
        | RuntimeClientError::SecureChannel { .. }
        | RuntimeClientError::RandomSource { .. }
        | RuntimeClientError::Protocol(_) => false,
    }
}

fn map_execution_portable_error(source: PortableProbeError) -> AppError {
    map_portable_error(source).with_context("access", "write")
}

fn execution_state_changed_error(expected: &str, actual: &str) -> AppError {
    AppError::from_source(
        "plan.execute.precondition",
        AppErrorCode::EquipmentStateChanged,
        "执行命令前的完整游戏状态已经改变",
        std::io::Error::other("execution state digest changed before dispatch"),
    )
    .with_context("expected_state_content_sha256", expected)
    .with_context("actual_state_content_sha256", actual)
}

fn execution_target_changed_error(
    expected: &ExecutionTargetIdentity,
    actual: &ExecutionTargetIdentity,
) -> AppError {
    AppError::from_source(
        "plan.execute.target",
        AppErrorCode::EquipmentStateChanged,
        "当前执行目标与用户确认的设备范围或持有资产不一致",
        std::io::Error::other("execution target identity changed before dispatch"),
    )
    .with_context(
        "expected_target_fingerprint_sha256",
        expected.fingerprint_sha256(),
    )
    .with_context(
        "actual_target_fingerprint_sha256",
        actual.fingerprint_sha256(),
    )
}

fn execution_precondition_error(message: &'static str, detail: &'static str) -> AppError {
    AppError::from_source(
        "plan.execute.precondition",
        AppErrorCode::EquipmentStateChanged,
        message,
        std::io::Error::other(detail),
    )
}

fn execution_protocol_error<E>(message: &'static str, source: E) -> AppError
where
    E: std::error::Error + Send + Sync + 'static,
{
    AppError::from_source(
        "plan.execute.encode",
        AppErrorCode::RuntimeIncompatible,
        message,
        source,
    )
}

fn execution_capability_error(capability: &'static str, message: &'static str) -> AppError {
    AppError::from_source(
        "plan.execute.capability",
        AppErrorCode::CapabilityMissing,
        message,
        std::io::Error::other("execution write capability is not ready"),
    )
    .with_context("capability", capability)
}

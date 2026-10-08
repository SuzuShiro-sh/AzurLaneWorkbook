//! 定义装备写命令、局部前态和幂等查询载荷。

use serde::{Deserialize, Serialize};

use super::{
    MAX_ENHANCE_MATERIALS, RuntimeProtocolError, deserialize_required_nullable,
    validate_nonnegative_lua_integer, validate_positive_lua_integer, validate_positive_u32,
    validate_sha256,
};

/// 装备命令中用于核对槽位或仓库来源的单件运行态装备。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentCommandEquipment {
    equipment_id: u64,
    config_id: u64,
    enhance_level: u32,
}

impl EquipmentCommandEquipment {
    /// 创建可由当前 Lua 运行时精确表示的装备前态。
    pub fn new(
        equipment_id: u64,
        config_id: u64,
        enhance_level: u32,
    ) -> Result<Self, RuntimeProtocolError> {
        validate_positive_lua_integer("equipment_command.equipment_id", equipment_id)?;
        validate_positive_lua_integer("equipment_command.config_id", config_id)?;
        Ok(Self {
            equipment_id,
            config_id,
            enhance_level,
        })
    }

    /// 返回客户端装备对象的运行态标识。
    pub const fn equipment_id(self) -> u64 {
        self.equipment_id
    }

    /// 返回装备配置标识。
    pub const fn config_id(self) -> u64 {
        self.config_id
    }

    /// 返回用户可见强化等级。
    pub const fn enhance_level(self) -> u32 {
        self.enhance_level
    }

    fn validate(self) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("equipment_command.equipment_id", self.equipment_id)?;
        validate_positive_lua_integer("equipment_command.config_id", self.config_id)
    }
}

/// 单级强化消耗的一种背包材料及其发送前数量。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentCommandMaterialCost {
    item_id: u64,
    quantity_before: u64,
    cost: u64,
}

impl EquipmentCommandMaterialCost {
    /// 创建一条可由 Lua 精确表示且发送前数量充足的材料成本。
    pub fn new(
        item_id: u64,
        quantity_before: u64,
        cost: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let material = Self {
            item_id,
            quantity_before,
            cost,
        };
        material.validate()?;
        Ok(material)
    }

    /// 返回背包物品 ID。
    pub const fn item_id(self) -> u64 {
        self.item_id
    }

    /// 返回命令发送前的背包数量。
    pub const fn quantity_before(self) -> u64 {
        self.quantity_before
    }

    /// 返回该单级强化消耗的数量。
    pub const fn cost(self) -> u64 {
        self.cost
    }

    fn validate(self) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("enhance.material.item_id", self.item_id)?;
        validate_nonnegative_lua_integer("enhance.material.quantity_before", self.quantity_before)?;
        validate_nonnegative_lua_integer("enhance.material.cost", self.cost)?;
        if self.cost == 0 || self.cost > self.quantity_before {
            return Err(RuntimeProtocolError::new(
                "equipment_command_precondition_invalid",
                "enhance 的材料成本必须为正数且不能超过发送前数量",
            ));
        }
        Ok(())
    }
}

/// 装备命令当前允许触发的官方客户端动作。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentCommandActionKind {
    /// 从普通舰船槽位卸下装备。
    Unequip,
    /// 从装备仓库放入普通舰船槽位。
    Equip,
    /// 从装备仓库销毁指定数量。
    Dismantle,
    /// 按客户端静态配方合成指定数量的装备。
    Compose,
    /// 强化仓库中的一件聚合装备。
    EnhanceWarehouse,
    /// 强化舰船普通槽位中的一件装备。
    EnhanceShip,
}

/// 设备端在同一个游戏主线程帧内重新核对的强类型局部前置状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EquipmentCommandAction {
    /// 卸下舰船槽位中的一件装备。
    Unequip {
        ship_id: u64,
        slot_index: u32,
        target_before: EquipmentCommandEquipment,
        target_warehouse_quantity_before: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    },
    /// 把仓库中的一件装备放入舰船槽位。
    Equip {
        ship_id: u64,
        slot_index: u32,
        #[serde(deserialize_with = "deserialize_required_nullable")]
        target_before: Option<EquipmentCommandEquipment>,
        source_before: EquipmentCommandEquipment,
        source_quantity_before: u64,
        target_warehouse_quantity_before: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    },
    /// 销毁仓库中的同配置装备。
    Dismantle {
        source_before: EquipmentCommandEquipment,
        source_quantity_before: u64,
        dismantle_quantity: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    },
    /// 消耗背包材料和物资，向装备仓库增加配方产物。
    Compose {
        recipe_id: u64,
        compose_quantity: u64,
        output_config_id: u64,
        #[serde(deserialize_with = "deserialize_required_nullable")]
        output_before: Option<EquipmentCommandEquipment>,
        output_quantity_before: u64,
        material_id: u64,
        material_quantity_before: u64,
        material_quantity_per_unit: u64,
        gold_before: u64,
        gold_per_unit: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    },
    /// 把仓库中的一件装备从当前配置强化到相邻的下一配置。
    EnhanceWarehouse {
        source_before: EquipmentCommandEquipment,
        source_quantity_before: u64,
        target_config_id: u64,
        target_enhance_level: u32,
        #[serde(deserialize_with = "deserialize_required_nullable")]
        target_before: Option<EquipmentCommandEquipment>,
        target_warehouse_quantity_before: u64,
        materials: Vec<EquipmentCommandMaterialCost>,
        gold_before: u64,
        gold_cost: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    },
    /// 把舰船普通槽位中的装备从当前配置强化到相邻的下一配置。
    EnhanceShip {
        ship_id: u64,
        slot_index: u32,
        source_before: EquipmentCommandEquipment,
        target_config_id: u64,
        target_enhance_level: u32,
        materials: Vec<EquipmentCommandMaterialCost>,
        gold_before: u64,
        gold_cost: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    },
}

impl EquipmentCommandAction {
    /// 创建卸下命令；目标装备和其仓库聚合数量必须来自发送前最后一次完整读取。
    pub fn unequip(
        ship_id: u64,
        slot_index: u32,
        target_before: EquipmentCommandEquipment,
        target_warehouse_quantity_before: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let action = Self::Unequip {
            ship_id,
            slot_index,
            target_before,
            target_warehouse_quantity_before,
            equipment_capacity_before,
            equipment_limit_before,
        };
        action.validate()?;
        Ok(action)
    }

    /// 创建装上命令；来源和被替换装备的仓库数量共同冻结可观察后态。
    #[allow(clippy::too_many_arguments)]
    pub fn equip(
        ship_id: u64,
        slot_index: u32,
        target_before: Option<EquipmentCommandEquipment>,
        source_before: EquipmentCommandEquipment,
        source_quantity_before: u64,
        target_warehouse_quantity_before: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let action = Self::Equip {
            ship_id,
            slot_index,
            target_before,
            source_before,
            source_quantity_before,
            target_warehouse_quantity_before,
            equipment_capacity_before,
            equipment_limit_before,
        };
        action.validate()?;
        Ok(action)
    }

    /// 创建仓库拆解命令；来源身份、数量和仓库容量必须来自发送前最后一次完整读取。
    pub fn dismantle(
        source_before: EquipmentCommandEquipment,
        source_quantity_before: u64,
        dismantle_quantity: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let action = Self::Dismantle {
            source_before,
            source_quantity_before,
            dismantle_quantity,
            equipment_capacity_before,
            equipment_limit_before,
        };
        action.validate()?;
        Ok(action)
    }

    /// 创建装备合成命令；配方、资源和产物前态必须来自同一份完整状态。
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        recipe_id: u64,
        compose_quantity: u64,
        output_config_id: u64,
        output_before: Option<EquipmentCommandEquipment>,
        output_quantity_before: u64,
        material_id: u64,
        material_quantity_before: u64,
        material_quantity_per_unit: u64,
        gold_before: u64,
        gold_per_unit: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let action = Self::Compose {
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
        };
        action.validate()?;
        Ok(action)
    }

    /// 创建仓库单级强化命令；来源、目标聚合、资源和容量必须来自同一份完整状态。
    #[allow(clippy::too_many_arguments)]
    pub fn enhance_warehouse(
        source_before: EquipmentCommandEquipment,
        source_quantity_before: u64,
        target_config_id: u64,
        target_enhance_level: u32,
        target_before: Option<EquipmentCommandEquipment>,
        target_warehouse_quantity_before: u64,
        materials: Vec<EquipmentCommandMaterialCost>,
        gold_before: u64,
        gold_cost: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let action = Self::EnhanceWarehouse {
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
        };
        action.validate()?;
        Ok(action)
    }

    /// 创建舰上单级强化命令；槽位装备、资源和容量必须来自同一份完整状态。
    #[allow(clippy::too_many_arguments)]
    pub fn enhance_ship(
        ship_id: u64,
        slot_index: u32,
        source_before: EquipmentCommandEquipment,
        target_config_id: u64,
        target_enhance_level: u32,
        materials: Vec<EquipmentCommandMaterialCost>,
        gold_before: u64,
        gold_cost: u64,
        equipment_capacity_before: u64,
        equipment_limit_before: u64,
    ) -> Result<Self, RuntimeProtocolError> {
        let action = Self::EnhanceShip {
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
        };
        action.validate()?;
        Ok(action)
    }

    /// 返回官方命令类型。
    pub const fn kind(&self) -> EquipmentCommandActionKind {
        match self {
            Self::Unequip { .. } => EquipmentCommandActionKind::Unequip,
            Self::Equip { .. } => EquipmentCommandActionKind::Equip,
            Self::Dismantle { .. } => EquipmentCommandActionKind::Dismantle,
            Self::Compose { .. } => EquipmentCommandActionKind::Compose,
            Self::EnhanceWarehouse { .. } => EquipmentCommandActionKind::EnhanceWarehouse,
            Self::EnhanceShip { .. } => EquipmentCommandActionKind::EnhanceShip,
        }
    }

    /// 返回目标舰船实例标识；纯仓库动作为空。
    pub const fn ship_id(&self) -> Option<u64> {
        match self {
            Self::Unequip { ship_id, .. }
            | Self::Equip { ship_id, .. }
            | Self::EnhanceShip { ship_id, .. } => Some(*ship_id),
            Self::Dismantle { .. } | Self::Compose { .. } | Self::EnhanceWarehouse { .. } => None,
        }
    }

    /// 返回一至五号普通装备槽；纯仓库动作为空。
    pub const fn slot_index(&self) -> Option<u32> {
        match self {
            Self::Unequip { slot_index, .. }
            | Self::Equip { slot_index, .. }
            | Self::EnhanceShip { slot_index, .. } => Some(*slot_index),
            Self::Dismantle { .. } | Self::Compose { .. } | Self::EnhanceWarehouse { .. } => None,
        }
    }

    /// 返回发送前的目标槽装备。
    pub const fn target_before(&self) -> Option<EquipmentCommandEquipment> {
        match self {
            Self::Unequip { target_before, .. } => Some(*target_before),
            Self::Equip { target_before, .. } => *target_before,
            Self::Dismantle { .. }
            | Self::Compose { .. }
            | Self::EnhanceWarehouse { .. }
            | Self::EnhanceShip { .. } => None,
        }
    }

    /// 返回装上或拆解命令使用的仓库来源装备。
    pub const fn source_before(&self) -> Option<EquipmentCommandEquipment> {
        match self {
            Self::Unequip { .. } | Self::Compose { .. } => None,
            Self::Equip { source_before, .. }
            | Self::Dismantle { source_before, .. }
            | Self::EnhanceWarehouse { source_before, .. }
            | Self::EnhanceShip { source_before, .. } => Some(*source_before),
        }
    }

    /// 返回发送前来源装备的仓库聚合数量。
    pub const fn source_quantity_before(&self) -> u64 {
        match self {
            Self::Unequip { .. } | Self::Compose { .. } | Self::EnhanceShip { .. } => 0,
            Self::Equip {
                source_quantity_before,
                ..
            }
            | Self::Dismantle {
                source_quantity_before,
                ..
            }
            | Self::EnhanceWarehouse {
                source_quantity_before,
                ..
            } => *source_quantity_before,
        }
    }

    /// 返回发送前被替换装备的仓库聚合数量。
    pub const fn target_warehouse_quantity_before(&self) -> u64 {
        match self {
            Self::Unequip {
                target_warehouse_quantity_before,
                ..
            }
            | Self::Equip {
                target_warehouse_quantity_before,
                ..
            } => *target_warehouse_quantity_before,
            Self::Dismantle { .. } | Self::Compose { .. } | Self::EnhanceShip { .. } => 0,
            Self::EnhanceWarehouse {
                target_warehouse_quantity_before,
                ..
            } => *target_warehouse_quantity_before,
        }
    }

    /// 返回拆解数量；装上、卸下和合成动作为空。
    pub const fn dismantle_quantity(&self) -> Option<u64> {
        match self {
            Self::Dismantle {
                dismantle_quantity, ..
            } => Some(*dismantle_quantity),
            Self::Unequip { .. }
            | Self::Equip { .. }
            | Self::Compose { .. }
            | Self::EnhanceWarehouse { .. }
            | Self::EnhanceShip { .. } => None,
        }
    }

    /// 返回发送前装备仓库已用容量。
    pub const fn equipment_capacity_before(&self) -> u64 {
        match self {
            Self::Unequip {
                equipment_capacity_before,
                ..
            }
            | Self::Equip {
                equipment_capacity_before,
                ..
            }
            | Self::Dismantle {
                equipment_capacity_before,
                ..
            }
            | Self::Compose {
                equipment_capacity_before,
                ..
            }
            | Self::EnhanceWarehouse {
                equipment_capacity_before,
                ..
            }
            | Self::EnhanceShip {
                equipment_capacity_before,
                ..
            } => *equipment_capacity_before,
        }
    }

    /// 返回发送前装备仓库容量上限。
    pub const fn equipment_limit_before(&self) -> u64 {
        match self {
            Self::Unequip {
                equipment_limit_before,
                ..
            }
            | Self::Equip {
                equipment_limit_before,
                ..
            }
            | Self::Dismantle {
                equipment_limit_before,
                ..
            }
            | Self::Compose {
                equipment_limit_before,
                ..
            }
            | Self::EnhanceWarehouse {
                equipment_limit_before,
                ..
            }
            | Self::EnhanceShip {
                equipment_limit_before,
                ..
            } => *equipment_limit_before,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        match self {
            Self::Unequip {
                ship_id,
                slot_index,
                target_before,
                target_warehouse_quantity_before,
                equipment_capacity_before,
                equipment_limit_before,
            } => {
                validate_ship_slot(*ship_id, *slot_index)?;
                target_before.validate()?;
                validate_warehouse_state(
                    *target_warehouse_quantity_before,
                    *equipment_capacity_before,
                    *equipment_limit_before,
                )?;
                if equipment_capacity_before >= equipment_limit_before {
                    return Err(RuntimeProtocolError::new(
                        "equipment_command_precondition_invalid",
                        "unequip 要求仓库至少保留一个空位",
                    ));
                }
            }
            Self::Equip {
                ship_id,
                slot_index,
                target_before,
                source_before,
                source_quantity_before,
                target_warehouse_quantity_before,
                equipment_capacity_before,
                equipment_limit_before,
            } => {
                validate_ship_slot(*ship_id, *slot_index)?;
                source_before.validate()?;
                if let Some(target) = target_before {
                    target.validate()?;
                }
                validate_warehouse_state(
                    *target_warehouse_quantity_before,
                    *equipment_capacity_before,
                    *equipment_limit_before,
                )?;
                validate_nonnegative_lua_integer(
                    "source_quantity_before",
                    *source_quantity_before,
                )?;
                if *source_quantity_before == 0
                    || source_quantity_before > equipment_capacity_before
                    || (target_before.is_none() && *target_warehouse_quantity_before != 0)
                {
                    return Err(RuntimeProtocolError::new(
                        "equipment_command_precondition_invalid",
                        "equip 的来源数量或空目标槽仓库前置条件无效",
                    ));
                }
                if let Some(target) = target_before {
                    if target.equipment_id == source_before.equipment_id {
                        return Err(RuntimeProtocolError::new(
                            "equipment_command_precondition_invalid",
                            "目标与来源使用同一装备 ID 时命令前后状态不可区分",
                        ));
                    } else if *source_quantity_before
                        > equipment_capacity_before
                            .saturating_sub(*target_warehouse_quantity_before)
                    {
                        return Err(RuntimeProtocolError::new(
                            "equipment_command_precondition_invalid",
                            "不同装备 ID 的局部仓库数量之和超过总容量",
                        ));
                    }
                }
            }
            Self::Dismantle {
                source_before,
                source_quantity_before,
                dismantle_quantity,
                equipment_capacity_before,
                equipment_limit_before,
            } => {
                source_before.validate()?;
                validate_warehouse_state(0, *equipment_capacity_before, *equipment_limit_before)?;
                validate_nonnegative_lua_integer(
                    "source_quantity_before",
                    *source_quantity_before,
                )?;
                validate_nonnegative_lua_integer("dismantle_quantity", *dismantle_quantity)?;
                if source_before.enhance_level != 0
                    || *dismantle_quantity == 0
                    || dismantle_quantity > source_quantity_before
                    || source_quantity_before > equipment_capacity_before
                {
                    return Err(RuntimeProtocolError::new(
                        "equipment_command_precondition_invalid",
                        "dismantle 只允许未强化来源，且正数拆解数量不能超过来源数量或仓库已用容量",
                    ));
                }
            }
            Self::Compose {
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
                validate_positive_lua_integer("compose.recipe_id", *recipe_id)?;
                validate_positive_lua_integer("compose.output_config_id", *output_config_id)?;
                validate_positive_lua_integer("compose.material_id", *material_id)?;
                validate_nonnegative_lua_integer("compose.compose_quantity", *compose_quantity)?;
                validate_nonnegative_lua_integer(
                    "compose.output_quantity_before",
                    *output_quantity_before,
                )?;
                validate_nonnegative_lua_integer(
                    "compose.material_quantity_before",
                    *material_quantity_before,
                )?;
                validate_nonnegative_lua_integer(
                    "compose.material_quantity_per_unit",
                    *material_quantity_per_unit,
                )?;
                validate_nonnegative_lua_integer("compose.gold_before", *gold_before)?;
                validate_nonnegative_lua_integer("compose.gold_per_unit", *gold_per_unit)?;
                validate_warehouse_state(
                    *output_quantity_before,
                    *equipment_capacity_before,
                    *equipment_limit_before,
                )?;
                if let Some(output) = output_before {
                    output.validate()?;
                    if output.config_id != *output_config_id || output.enhance_level != 0 {
                        return Err(RuntimeProtocolError::new(
                            "equipment_command_precondition_invalid",
                            "compose 的既有产物对象与配方配置或初始强化等级不一致",
                        ));
                    }
                }
                let material_required = material_quantity_per_unit
                    .checked_mul(*compose_quantity)
                    .ok_or_else(|| {
                    RuntimeProtocolError::new(
                        "equipment_command_precondition_invalid",
                        "compose 的材料总成本超出稳定整数范围",
                    )
                })?;
                let gold_required =
                    gold_per_unit
                        .checked_mul(*compose_quantity)
                        .ok_or_else(|| {
                            RuntimeProtocolError::new(
                                "equipment_command_precondition_invalid",
                                "compose 的物资总成本超出稳定整数范围",
                            )
                        })?;
                let capacity_after = equipment_capacity_before
                    .checked_add(*compose_quantity)
                    .ok_or_else(|| {
                        RuntimeProtocolError::new(
                            "equipment_command_precondition_invalid",
                            "compose 的预期仓库容量超出稳定整数范围",
                        )
                    })?;
                if *compose_quantity == 0
                    || *material_quantity_per_unit == 0
                    || output_before.is_some() != (*output_quantity_before > 0)
                    || *output_quantity_before > *equipment_capacity_before
                    || material_required > *material_quantity_before
                    || gold_required > *gold_before
                    || capacity_after > *equipment_limit_before
                {
                    return Err(RuntimeProtocolError::new(
                        "equipment_command_precondition_invalid",
                        "compose 的数量、产物、材料、物资或仓库容量前置条件无效",
                    ));
                }
            }
            Self::EnhanceWarehouse {
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
            } => {
                validate_enhance_transition(
                    *source_before,
                    *target_config_id,
                    *target_enhance_level,
                    materials,
                    *gold_before,
                    *gold_cost,
                    *equipment_capacity_before,
                    *equipment_limit_before,
                )?;
                validate_nonnegative_lua_integer(
                    "enhance.source_quantity_before",
                    *source_quantity_before,
                )?;
                validate_nonnegative_lua_integer(
                    "enhance.target_warehouse_quantity_before",
                    *target_warehouse_quantity_before,
                )?;
                if let Some(target) = target_before {
                    target.validate()?;
                    if target.config_id != *target_config_id
                        || target.enhance_level != *target_enhance_level
                        || target.equipment_id == source_before.equipment_id
                    {
                        return Err(RuntimeProtocolError::new(
                            "equipment_command_precondition_invalid",
                            "enhance_warehouse 的既有目标聚合与下一配置不一致",
                        ));
                    }
                }
                let observed_quantity = source_quantity_before
                    .checked_add(*target_warehouse_quantity_before)
                    .ok_or_else(|| {
                        RuntimeProtocolError::new(
                            "equipment_command_precondition_invalid",
                            "enhance_warehouse 的局部仓库数量溢出",
                        )
                    })?;
                if *source_quantity_before == 0
                    || target_before.is_some() != (*target_warehouse_quantity_before > 0)
                    || observed_quantity > *equipment_capacity_before
                {
                    return Err(RuntimeProtocolError::new(
                        "equipment_command_precondition_invalid",
                        "enhance_warehouse 的来源、目标聚合或仓库容量前置条件无效",
                    ));
                }
            }
            Self::EnhanceShip {
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
            } => {
                validate_ship_slot(*ship_id, *slot_index)?;
                validate_enhance_transition(
                    *source_before,
                    *target_config_id,
                    *target_enhance_level,
                    materials,
                    *gold_before,
                    *gold_cost,
                    *equipment_capacity_before,
                    *equipment_limit_before,
                )?;
            }
        }
        Ok(())
    }
}

/// 校验两种强化位置共用的单级配置跃迁、资源和仓库容量前态。
#[allow(clippy::too_many_arguments)]
fn validate_enhance_transition(
    source_before: EquipmentCommandEquipment,
    target_config_id: u64,
    target_enhance_level: u32,
    materials: &[EquipmentCommandMaterialCost],
    gold_before: u64,
    gold_cost: u64,
    equipment_capacity_before: u64,
    equipment_limit_before: u64,
) -> Result<(), RuntimeProtocolError> {
    source_before.validate()?;
    validate_positive_lua_integer("enhance.target_config_id", target_config_id)?;
    validate_nonnegative_lua_integer("enhance.gold_before", gold_before)?;
    validate_nonnegative_lua_integer("enhance.gold_cost", gold_cost)?;
    validate_warehouse_state(0, equipment_capacity_before, equipment_limit_before)?;
    if source_before.config_id == target_config_id
        || source_before.enhance_level.checked_add(1) != Some(target_enhance_level)
        || gold_cost > gold_before
    {
        return Err(RuntimeProtocolError::new(
            "equipment_command_precondition_invalid",
            "enhance 只允许相邻的下一配置，且发送前物资必须足够",
        ));
    }
    let mut previous_item_id = 0;
    if materials.len() > MAX_ENHANCE_MATERIALS {
        return Err(RuntimeProtocolError::new(
            "equipment_command_precondition_invalid",
            "enhance 的单级材料种类不能超过 64 种",
        ));
    }
    for material in materials {
        material.validate()?;
        if material.item_id <= previous_item_id {
            return Err(RuntimeProtocolError::new(
                "equipment_command_precondition_invalid",
                "enhance 的材料必须按物品 ID 严格升序且不得重复",
            ));
        }
        previous_item_id = material.item_id;
    }
    Ok(())
}

fn validate_ship_slot(ship_id: u64, slot_index: u32) -> Result<(), RuntimeProtocolError> {
    validate_positive_lua_integer("equipment_command.ship_id", ship_id)?;
    if !(1..=5).contains(&slot_index) {
        return Err(RuntimeProtocolError::new(
            "equipment_command_slot_invalid",
            format!("equipment_command.slot_index 只允许 1 至 5，实际为 {slot_index}"),
        ));
    }
    Ok(())
}

fn validate_warehouse_state(
    target_warehouse_quantity_before: u64,
    equipment_capacity_before: u64,
    equipment_limit_before: u64,
) -> Result<(), RuntimeProtocolError> {
    for (field, value) in [
        (
            "target_warehouse_quantity_before",
            target_warehouse_quantity_before,
        ),
        ("equipment_capacity_before", equipment_capacity_before),
        ("equipment_limit_before", equipment_limit_before),
    ] {
        validate_nonnegative_lua_integer(field, value)?;
    }
    if equipment_limit_before == 0
        || equipment_capacity_before > equipment_limit_before
        || target_warehouse_quantity_before > equipment_capacity_before
    {
        return Err(RuntimeProtocolError::new(
            "equipment_command_precondition_invalid",
            "装备仓库数量与容量前置条件不一致",
        ));
    }
    Ok(())
}

/// 携带应用层确定性幂等标识和设备端局部前置条件的运行态写请求。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipmentCommand {
    schema_version: u32,
    command_id: String,
    target_fingerprint_sha256: String,
    plan_hash: String,
    sequence: u32,
    /// 主机侧完整状态审计摘要；设备端原子门禁由 `action` 携带的局部前置条件提供。
    pre_state_content_sha256: String,
    action: EquipmentCommandAction,
}

impl RuntimeEquipmentCommand {
    /// 创建版本固定为 2 的命令，并在任何 I/O 前校验审计摘要格式与局部前置条件。
    pub fn new(
        command_id: impl Into<String>,
        target_fingerprint_sha256: impl Into<String>,
        plan_hash: impl Into<String>,
        sequence: u32,
        pre_state_content_sha256: impl Into<String>,
        action: EquipmentCommandAction,
    ) -> Result<Self, RuntimeProtocolError> {
        let command = Self {
            schema_version: 2,
            command_id: command_id.into(),
            target_fingerprint_sha256: target_fingerprint_sha256.into(),
            plan_hash: plan_hash.into(),
            sequence,
            pre_state_content_sha256: pre_state_content_sha256.into(),
            action,
        };
        command.validate()?;
        Ok(command)
    }

    /// 返回应用层生成的确定性幂等命令标识。
    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    /// 返回设备端需要原子核对的局部动作前态。
    pub const fn action(&self) -> &EquipmentCommandAction {
        &self.action
    }

    pub(crate) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 2 {
            return Err(RuntimeProtocolError::new(
                "equipment_command_schema_unsupported",
                format!(
                    "equipment_command.schema_version 必须为 2，实际为 {}",
                    self.schema_version
                ),
            ));
        }
        validate_sha256("equipment_command.command_id", &self.command_id)?;
        validate_sha256(
            "equipment_command.target_fingerprint_sha256",
            &self.target_fingerprint_sha256,
        )?;
        validate_sha256("equipment_command.plan_hash", &self.plan_hash)?;
        validate_sha256(
            "equipment_command.pre_state_content_sha256",
            &self.pre_state_content_sha256,
        )?;
        validate_positive_u32("equipment_command.sequence", self.sequence)?;
        self.action.validate()
    }
}

/// query 和 cancel 只携带原命令的确定性标识。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EquipmentCommandLookupPayload {
    pub command_id: String,
}

impl EquipmentCommandLookupPayload {
    pub fn new(command_id: &str) -> Result<Self, RuntimeProtocolError> {
        validate_sha256("equipment_command.command_id", command_id)?;
        Ok(Self {
            command_id: command_id.to_owned(),
        })
    }
}

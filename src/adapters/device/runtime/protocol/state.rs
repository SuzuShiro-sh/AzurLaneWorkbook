//! 定义玩家持有状态与舰船详情的请求、响应和边界校验。

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    MAX_FLEET_TEAM_SHIPS, MAX_SHIP_FLEET_MEMBERSHIPS, MAX_SHIP_SKILLS,
    MAX_SHIP_SLOT_EQUIPMENT_TYPES, MAX_SNAPSHOT_ITEMS, RuntimeProtocolError,
    SHIP_EQUIPMENT_SLOT_COUNT, deserialize_required_nullable, validate_non_empty,
    validate_positive_lua_integer, validate_positive_u32, validate_positive_u64, validate_sha256,
    validate_stable_token, validate_text, validate_timeout,
};

/// 背包快照请求的显式条目上限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotBagPayload {
    pub max_items: u32,
}

impl SnapshotBagPayload {
    /// 创建位于冻结快照容量范围内的请求载荷。
    pub fn new(max_items: u32) -> Result<Self, RuntimeProtocolError> {
        if !(1..=MAX_SNAPSHOT_ITEMS).contains(&max_items) {
            return Err(RuntimeProtocolError::new(
                "snapshot_limit_out_of_range",
                format!(
                    "snapshot_bag.max_items 只允许 1 至 {MAX_SNAPSHOT_ITEMS}，实际为 {max_items}"
                ),
            ));
        }
        Ok(Self { max_items })
    }
}

/// 完整运行态快照对船坞、装备仓库和背包分别设置显式上限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotOwnedStatePayload {
    /// 单次返回的最大舰船数。
    pub max_ships: u32,
    /// 单次返回的最大仓库装备配置数。
    pub max_equipments: u32,
    /// 单次返回的最大背包物品数。
    pub max_items: u32,
}

impl SnapshotOwnedStatePayload {
    /// 创建三个容量都位于冻结范围内的请求载荷。
    pub fn new(
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
    ) -> Result<Self, RuntimeProtocolError> {
        for (name, value) in [
            ("max_ships", max_ships),
            ("max_equipments", max_equipments),
            ("max_items", max_items),
        ] {
            if !(1..=MAX_SNAPSHOT_ITEMS).contains(&value) {
                return Err(RuntimeProtocolError::new(
                    "snapshot_limit_out_of_range",
                    format!(
                        "snapshot_owned_state.{name} 只允许 1 至 {MAX_SNAPSHOT_ITEMS}，实际为 {value}"
                    ),
                ));
            }
        }
        Ok(Self {
            max_ships,
            max_equipments,
            max_items,
        })
    }
}

/// 舰船详情快照的显式船坞条目上限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotShipDetailsPayload {
    /// 单次返回的最大舰船数。
    pub max_ships: u32,
}

impl SnapshotShipDetailsPayload {
    /// 创建位于冻结快照容量范围内的舰船详情请求。
    pub fn new(max_ships: u32) -> Result<Self, RuntimeProtocolError> {
        if !(1..=MAX_SNAPSHOT_ITEMS).contains(&max_ships) {
            return Err(RuntimeProtocolError::new(
                "snapshot_limit_out_of_range",
                format!(
                    "snapshot_ship_details.max_ships 只允许 1 至 {MAX_SNAPSHOT_ITEMS}，实际为 {max_ships}"
                ),
            ));
        }
        Ok(Self { max_ships })
    }
}

/// 在发出任一 RPC 前复用各请求载荷的冻结范围校验。
pub(crate) fn validate_full_state_read_options(
    timeout_ms: u32,
    max_ships: u32,
    max_equipments: u32,
    max_items: u32,
    expected_module_sha256: &str,
) -> Result<(), RuntimeProtocolError> {
    validate_timeout(timeout_ms)?;
    SnapshotOwnedStatePayload::new(max_ships, max_equipments, max_items)?;
    validate_sha256("expected_module_sha256", expected_module_sha256)
}

/// `snapshot_bag` 成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotBagResult {
    /// 背包快照 schema 固定为 1。
    pub schema_version: u32,
    /// 快照是否完整。
    pub complete: bool,
    /// 返回条目数，必须等于 `items.len()`。
    pub count: u32,
    /// 是否因为条目上限而截断。
    pub truncated: bool,
    /// 已成功读取并按物品 ID 严格升序排列的背包条目。
    pub items: Vec<BagItem>,
    /// 逐条读取错误。
    pub read_errors: Vec<ReadError>,
}

impl SnapshotBagResult {
    /// 校验快照版本、计数、完整性、条目唯一性和逐条读取错误。
    pub(crate) fn validate(&self, max_items: u32) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "snapshot_schema_unsupported",
                format!("只支持背包 schema 1，实际为 {}", self.schema_version),
            ));
        }
        if self.items.len() != self.count as usize {
            return Err(RuntimeProtocolError::new(
                "snapshot_count_mismatch",
                format!(
                    "snapshot_bag.count={}，但 items 实际有 {} 项",
                    self.count,
                    self.items.len()
                ),
            ));
        }
        if self.count > max_items {
            return Err(RuntimeProtocolError::new(
                "snapshot_limit_exceeded",
                format!("快照返回 {} 项，超过请求上限 {max_items}", self.count),
            ));
        }
        if self.complete != (!self.truncated && self.read_errors.is_empty()) {
            return Err(RuntimeProtocolError::new(
                "snapshot_completeness_mismatch",
                "complete 必须与 truncated 和 read_errors 保持一致",
            ));
        }

        let mut item_ids: HashSet<u64> = HashSet::with_capacity(self.items.len());
        let mut previous_item_id: Option<u64> = None;
        for item in &self.items {
            item.validate()?;
            if !item_ids.insert(item.item_id) {
                return Err(RuntimeProtocolError::new(
                    "snapshot_duplicate_item",
                    format!("背包快照重复返回 item_id={}", item.item_id),
                ));
            }
            if previous_item_id.is_some_and(|previous| previous >= item.item_id) {
                return Err(RuntimeProtocolError::new(
                    "snapshot_order_invalid",
                    "背包快照必须按 item_id 严格升序返回",
                ));
            }
            previous_item_id = Some(item.item_id);
        }
        for error in &self.read_errors {
            error.validate()?;
        }
        Ok(())
    }
}

/// 单个背包条目。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BagItem {
    /// 游戏物品 ID。
    pub item_id: u64,
    /// 当前数量。
    pub quantity: u64,
    /// 条目来源固定为 bag。
    pub kind: BagItemKind,
    /// 游戏内解析后的名称。
    pub resolved_name: String,
    /// 可合成条目的配方信息；不可合成时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub compose_recipe: Option<ComposeRecipe>,
}

impl BagItem {
    /// 校验物品身份、名称以及可选合成配方的字段约束。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("item.item_id", self.item_id)?;
        validate_non_empty("item.resolved_name", &self.resolved_name, 512)?;
        if let Some(recipe) = &self.compose_recipe {
            recipe.validate()?;
        }
        Ok(())
    }
}

/// 背包条目种类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BagItemKind {
    /// 普通背包条目。
    Bag,
}

/// 背包物品的合成配方。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeRecipe {
    /// 配方 ID。
    pub recipe_id: u64,
    /// 消耗材料 ID。
    pub material_id: u64,
    /// 单次合成材料数量。
    pub material_count: u64,
    /// 单次合成金币数量。
    pub gold: u64,
    /// 产出装备 ID；非装备产物时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub equipment_id: Option<u64>,
    /// 当前资源最多可合成数量；未知时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub max_count: Option<u64>,
}

impl ComposeRecipe {
    /// 校验配方及其必需材料标识和数量均为有效正整数。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("recipe.recipe_id", self.recipe_id)?;
        validate_positive_u64("recipe.material_id", self.material_id)?;
        validate_positive_u64("recipe.material_count", self.material_count)?;
        if let Some(equipment_id) = self.equipment_id {
            validate_positive_u64("recipe.equipment_id", equipment_id)?;
        }
        Ok(())
    }
}

/// 背包单条数据无法完整读取时的定位信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadError {
    /// 已知物品 ID；读取 ID 本身失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub item_id: Option<u64>,
    /// 稳定字段错误码。
    pub code: String,
    /// 面向用户的中文说明。
    pub message: String,
}

impl ReadError {
    /// 校验可选物品标识、稳定错误码和用户可读消息。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        if let Some(item_id) = self.item_id {
            validate_positive_u64("read_error.item_id", item_id)?;
        }
        validate_stable_token("read_error.code", &self.code)?;
        validate_non_empty("read_error.message", &self.message, 1024)
    }
}

/// `snapshot_owned_state` 在同一次游戏主线程停顿内取得的完整运行态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotOwnedStateResult {
    /// 完整运行态 schema 固定为 3。
    pub schema_version: u32,
    /// 船坞、仓库和背包是否都完整。
    pub complete: bool,
    /// 船坞舰船养成、自身技能及五个固定装备槽位。
    pub dock: DockSnapshot,
    /// 按装备配置聚合计数的仓库。
    pub warehouse: WarehouseSnapshot,
    /// 与独立 `snapshot_bag` 相同的背包子契约。
    pub bag: SnapshotBagResult,
    /// 当前物资和装备仓库容量。
    pub player: PlayerResources,
}

/// 同一次船坞遍历得到的账号前窗口。`dock_frames` 是船坞分页帧数。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountBeforeResult {
    pub owned_state: SnapshotOwnedStateResult,
    pub ship_details: SnapshotShipDetailsResult,
    pub dock_frames: u32,
}

impl AccountBeforeResult {
    pub(crate) fn validate(
        &self,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        self.owned_state
            .validate(max_ships, max_equipments, max_items)?;
        self.ship_details
            .validate(max_ships, expected_module_sha256)?;
        let max_frames = max_ships.div_ceil(super::MAX_DOCK_PAGE_SIZE).max(1);
        if self.dock_frames == 0 || self.dock_frames > max_frames {
            return Err(RuntimeProtocolError::new(
                "dock_frames_invalid",
                format!(
                    "船坞分页帧数必须在 1 至 {max_frames}，实际为 {}",
                    self.dock_frames
                ),
            ));
        }
        Ok(())
    }
}

impl SnapshotOwnedStateResult {
    /// 独立复核所有子快照上限、完整性、唯一性和跨子快照容量关系。
    pub(crate) fn validate(
        &self,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 3 {
            return Err(RuntimeProtocolError::new(
                "owned_state_schema_unsupported",
                format!("只支持完整运行态 schema 3，实际为 {}", self.schema_version),
            ));
        }
        self.dock.validate(max_ships)?;
        self.warehouse.validate(max_equipments)?;
        self.bag.validate(max_items)?;
        self.player.validate()?;
        if self.complete != (self.dock.complete && self.warehouse.complete && self.bag.complete) {
            return Err(RuntimeProtocolError::new(
                "owned_state_completeness_mismatch",
                "顶层 complete 必须与 dock、warehouse 和 bag 的完整性保持一致",
            ));
        }

        // 截断或逐项读取失败时只拿到了仓库子集，不能拿子集数量反证全仓库容量。
        if self.warehouse.complete {
            let computed_capacity: u64 = self
                .warehouse
                .items
                .iter()
                .try_fold(0_u64, |total, item| total.checked_add(item.quantity))
                .ok_or_else(|| {
                    RuntimeProtocolError::new(
                        "warehouse_capacity_overflow",
                        "装备仓库数量求和溢出 u64",
                    )
                })?;
            if computed_capacity != self.player.equipment_capacity {
                return Err(RuntimeProtocolError::new(
                    "warehouse_capacity_mismatch",
                    format!(
                        "仓库条目数量之和为 {computed_capacity}，玩家容量为 {}",
                        self.player.equipment_capacity
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// 船坞快照及逐舰船、逐槽位读取错误。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DockSnapshot {
    /// 未截断且不存在逐舰船读取错误时为 true。
    pub complete: bool,
    /// 成功返回的舰船数量。
    pub count: u32,
    /// 船坞条目超过请求上限时为 true。
    pub truncated: bool,
    /// 按舰船运行态 ID 严格升序排列的船坞条目。
    pub ships: Vec<RuntimeShip>,
    /// 无法完整读取的舰船或槽位诊断。
    pub read_errors: Vec<ShipReadError>,
}

impl DockSnapshot {
    /// 校验船坞计数、上限、确定性顺序、实例唯一性和固定五槽结构。
    fn validate(&self, max_ships: u32) -> Result<(), RuntimeProtocolError> {
        if self.ships.len() != self.count as usize {
            return Err(RuntimeProtocolError::new(
                "dock_count_mismatch",
                format!(
                    "dock.count={}，但 ships 实际有 {} 项",
                    self.count,
                    self.ships.len()
                ),
            ));
        }
        if self.count > max_ships {
            return Err(RuntimeProtocolError::new(
                "dock_limit_exceeded",
                format!("船坞返回 {} 艘，超过请求上限 {max_ships}", self.count),
            ));
        }
        if self.complete != (!self.truncated && self.read_errors.is_empty()) {
            return Err(RuntimeProtocolError::new(
                "dock_completeness_mismatch",
                "dock.complete 必须与 truncated 和 read_errors 保持一致",
            ));
        }

        let mut ship_ids: HashSet<u64> = HashSet::with_capacity(self.ships.len());
        let mut fleet_definitions: BTreeMap<u32, (RuntimeFleetKind, Option<String>)> =
            BTreeMap::new();
        let mut occupied_fleet_positions: HashSet<(u32, RuntimeFleetTeam, u32)> = HashSet::new();
        let mut previous_ship_id: Option<u64> = None;
        for ship in &self.ships {
            ship.validate()?;
            if !ship_ids.insert(ship.ship_id) {
                return Err(RuntimeProtocolError::new(
                    "dock_duplicate_ship",
                    format!("船坞重复返回 ship_id={}", ship.ship_id),
                ));
            }
            if previous_ship_id.is_some_and(|previous| previous >= ship.ship_id) {
                return Err(RuntimeProtocolError::new(
                    "dock_order_invalid",
                    "船坞必须按 ship_id 严格升序返回",
                ));
            }
            previous_ship_id = Some(ship.ship_id);
            for membership in &ship.fleet_memberships {
                match fleet_definitions.get(&membership.fleet_id) {
                    Some((kind, display_name))
                        if *kind != membership.kind || *display_name != membership.display_name =>
                    {
                        return Err(RuntimeProtocolError::new(
                            "fleet_definition_mismatch",
                            format!(
                                "fleet_id={} 在不同舰船上的类型或名称不一致",
                                membership.fleet_id
                            ),
                        ));
                    }
                    Some(_) => {}
                    None => {
                        fleet_definitions.insert(
                            membership.fleet_id,
                            (membership.kind, membership.display_name.clone()),
                        );
                    }
                }
                if !occupied_fleet_positions.insert((
                    membership.fleet_id,
                    membership.team,
                    membership.position,
                )) {
                    return Err(RuntimeProtocolError::new(
                        "fleet_position_duplicate",
                        format!(
                            "fleet_id={} 的 {:?} 位置 {} 被多艘舰船占用",
                            membership.fleet_id, membership.team, membership.position
                        ),
                    ));
                }
            }
        }
        for error in &self.read_errors {
            error.validate()?;
        }
        Ok(())
    }
}

/// 舰船实例的稳定养成字段、持久编队、已学习技能和固定五个装备槽位。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShip {
    /// 游戏运行态中的舰船唯一 ID。
    pub ship_id: u64,
    /// 用于关联静态舰船配置的 ID。
    pub config_id: u64,
    /// 舰船当前等级。
    pub level: u32,
    /// 当前等级内的原始经验值。
    pub experience_in_level: u64,
    /// 客户端百分之一单位的原始好感值。
    pub intimacy_raw: u64,
    /// 舰船实例当前保存的心情值。
    pub energy: u64,
    /// 舰船实例当前保存的熟练度原始值。
    pub proficiency: u64,
    /// 按编队 ID、队伍和位置确定性排序的持久编队成员关系。
    pub fleet_memberships: Vec<RuntimeFleetMembership>,
    /// 只来自 `Ship.skills` 且按技能 ID 严格升序排列的学习进度。
    pub skills: Vec<RuntimeShipSkill>,
    /// 按 1 至 5 完整排列的固定装备槽位。
    pub slots: Vec<RuntimeShipSlot>,
}

impl RuntimeShip {
    /// 校验舰船标识、等级、技能顺序，并要求槽位按 1 至 5 完整排列。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("ship.ship_id", self.ship_id)?;
        validate_positive_u64("ship.config_id", self.config_id)?;
        validate_positive_u32("ship.level", self.level)?;
        if self.fleet_memberships.len() > MAX_SHIP_FLEET_MEMBERSHIPS {
            return Err(RuntimeProtocolError::new(
                "ship_fleet_membership_limit_exceeded",
                format!(
                    "ship_id={} 返回 {} 条编队关系，超过上限 {MAX_SHIP_FLEET_MEMBERSHIPS}",
                    self.ship_id,
                    self.fleet_memberships.len()
                ),
            ));
        }
        let mut previous_membership: Option<(u32, RuntimeFleetTeam, u32)> = None;
        let mut fleet_ids: HashSet<u32> = HashSet::with_capacity(self.fleet_memberships.len());
        for membership in &self.fleet_memberships {
            membership.validate()?;
            let key = (membership.fleet_id, membership.team, membership.position);
            if previous_membership.is_some_and(|previous| previous >= key) {
                return Err(RuntimeProtocolError::new(
                    "ship_fleet_membership_order_invalid",
                    format!(
                        "ship_id={} 的编队关系必须按 fleet_id、team、position 严格升序返回",
                        self.ship_id
                    ),
                ));
            }
            if !fleet_ids.insert(membership.fleet_id) {
                return Err(RuntimeProtocolError::new(
                    "ship_fleet_membership_duplicate",
                    format!(
                        "ship_id={} 在 fleet_id={} 中出现多次",
                        self.ship_id, membership.fleet_id
                    ),
                ));
            }
            previous_membership = Some(key);
        }
        if self.skills.len() > MAX_SHIP_SKILLS {
            return Err(RuntimeProtocolError::new(
                "ship_skill_limit_exceeded",
                format!(
                    "ship_id={} 返回 {} 个自身技能，超过上限 {MAX_SHIP_SKILLS}",
                    self.ship_id,
                    self.skills.len()
                ),
            ));
        }
        let mut previous_skill_id: Option<u64> = None;
        for skill in &self.skills {
            skill.validate()?;
            if previous_skill_id.is_some_and(|previous| previous >= skill.skill_id) {
                return Err(RuntimeProtocolError::new(
                    "ship_skill_order_invalid",
                    format!(
                        "ship_id={} 的技能必须按 skill_id 严格升序返回",
                        self.ship_id
                    ),
                ));
            }
            previous_skill_id = Some(skill.skill_id);
        }
        if self.slots.len() != 5 {
            return Err(RuntimeProtocolError::new(
                "ship_slot_count_invalid",
                format!("ship_id={} 必须返回五个槽位", self.ship_id),
            ));
        }
        for (expected, slot) in (1_u32..=5).zip(&self.slots) {
            if slot.slot_index != expected {
                return Err(RuntimeProtocolError::new(
                    "ship_slot_order_invalid",
                    format!(
                        "ship_id={} 的槽位应按 1 至 5 排列，当前位置为 {}",
                        self.ship_id, slot.slot_index
                    ),
                ));
            }
            if let Some(equipment) = &slot.equipment {
                equipment.validate()?;
            }
        }
        Ok(())
    }
}

/// 持久编队的业务种类；活动和挑战临时编队不会进入该契约。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFleetKind {
    /// 普通水面编队。
    Regular,
    /// 潜艇编队。
    Submarine,
    /// 演习防守编队。
    Exercise,
}

/// 舰船在编队内所属的客户端队伍。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFleetTeam {
    /// 主力队伍。
    Main,
    /// 先锋队伍。
    Vanguard,
    /// 潜艇队伍。
    Submarine,
}

/// 一艘舰船在一个持久编队中的确定位置。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFleetMembership {
    /// FleetProxy.data 使用的正整数编队 ID。
    pub fleet_id: u32,
    /// 客户端自定义名称或默认本地化名称；客户端没有名称时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub display_name: Option<String>,
    /// 普通、潜艇或演习编队。
    pub kind: RuntimeFleetKind,
    /// 主力、先锋或潜艇队伍。
    pub team: RuntimeFleetTeam,
    /// 队伍内一至三号位置。
    pub position: u32,
}

impl RuntimeFleetMembership {
    /// 校验编队标识、名称、队伍位置以及编队种类与队伍的一致性。
    pub(super) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u32("fleet_membership.fleet_id", self.fleet_id)?;
        if let Some(display_name) = &self.display_name {
            validate_non_empty("fleet_membership.display_name", display_name, 512)?;
        }
        if !(1..=MAX_FLEET_TEAM_SHIPS).contains(&self.position) {
            return Err(RuntimeProtocolError::new(
                "fleet_membership_position_invalid",
                format!(
                    "fleet_id={} 的 position={} 超出 1 至 {MAX_FLEET_TEAM_SHIPS}",
                    self.fleet_id, self.position
                ),
            ));
        }
        let compatible = matches!(
            (self.kind, self.team),
            (RuntimeFleetKind::Submarine, RuntimeFleetTeam::Submarine)
                | (
                    RuntimeFleetKind::Regular | RuntimeFleetKind::Exercise,
                    RuntimeFleetTeam::Main | RuntimeFleetTeam::Vanguard
                )
        );
        if !compatible {
            return Err(RuntimeProtocolError::new(
                "fleet_membership_team_invalid",
                format!(
                    "fleet_id={} 的 {:?} 编队不能包含 {:?} 队伍",
                    self.fleet_id, self.kind, self.team
                ),
            ));
        }
        Ok(())
    }
}

/// 舰船自身技能的原始学习进度，不包含装备或触发技能。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipSkill {
    /// `Ship.skills` 的稳定正整数键和值内 ID。
    pub skill_id: u64,
    /// 当前学习等级。
    pub level: u32,
    /// 当前等级内的原始技能经验。
    pub experience: u64,
}

impl RuntimeShipSkill {
    /// 技能 ID 和等级必须为正数；当前等级经验允许为零。
    pub(super) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("ship_skill.skill_id", self.skill_id)?;
        validate_positive_u32("ship_skill.level", self.level)
    }
}

/// 舰船槽位及其可空装备。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipSlot {
    /// 游戏使用的 1 至 5 槽位编号。
    pub slot_index: u32,
    /// 当前槽位装备；空槽明确表示为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub equipment: Option<RuntimeEquipment>,
}

/// 运行态装备对象保留运行态 ID、配置 ID 和用户可见强化等级。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEquipment {
    /// Lua 装备对象的运行态 ID；仓库聚合条目中不代表持久实例。
    pub equipment_id: u64,
    /// 用于业务匹配和配置查询的装备配置 ID。
    pub config_id: u64,
    /// 用户界面显示的 `+N` 强化等级。
    pub enhance_level: u32,
}

impl RuntimeEquipment {
    /// ID 必须为正数；强化等级允许从零开始。
    pub(super) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("equipment.equipment_id", self.equipment_id)?;
        validate_positive_u64("equipment.config_id", self.config_id)
    }
}

/// 船坞单艘舰船或单个槽位无法完整读取时的定位信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShipReadError {
    /// 已知舰船 ID；读取舰船 ID 本身失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub ship_id: Option<u64>,
    /// 已知自身技能 ID；错误不属于单个技能时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub skill_id: Option<u64>,
    /// 已知槽位编号；错误不属于单个槽位时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub slot_index: Option<u32>,
    /// 稳定字段错误码。
    pub code: String,
    /// 面向用户的中文说明。
    pub message: String,
}

impl ShipReadError {
    /// 校验可选定位标识、稳定错误码和用户可读消息。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        if let Some(ship_id) = self.ship_id {
            validate_positive_u64("ship_read_error.ship_id", ship_id)?;
        }
        if let Some(skill_id) = self.skill_id {
            validate_positive_u64("ship_read_error.skill_id", skill_id)?;
        }
        if let Some(slot_index) = self.slot_index
            && !(1..=5).contains(&slot_index)
        {
            return Err(RuntimeProtocolError::new(
                "ship_read_error_slot_invalid",
                format!("舰船读取错误的 slot_index={slot_index} 超出 1 至 5"),
            ));
        }
        validate_stable_token("ship_read_error.code", &self.code)?;
        validate_non_empty("ship_read_error.message", &self.message, 1024)
    }
}

/// 按装备配置聚合计数的仓库快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WarehouseSnapshot {
    /// 未截断且不存在逐条读取错误时为 true。
    pub complete: bool,
    /// 成功返回的装备配置条目数，不是装备总件数。
    pub count: u32,
    /// 装备配置条目超过请求上限时为 true。
    pub truncated: bool,
    /// 按运行态装备 ID 严格升序排列的聚合条目。
    pub items: Vec<WarehouseEquipment>,
    /// 无法完整读取的仓库条目诊断。
    pub read_errors: Vec<EquipmentReadError>,
}

impl WarehouseSnapshot {
    /// 校验仓库计数、上限、完整性、装备 ID 唯一性和确定性顺序。
    fn validate(&self, max_equipments: u32) -> Result<(), RuntimeProtocolError> {
        if self.items.len() != self.count as usize {
            return Err(RuntimeProtocolError::new(
                "warehouse_count_mismatch",
                format!(
                    "warehouse.count={}，但 items 实际有 {} 项",
                    self.count,
                    self.items.len()
                ),
            ));
        }
        if self.count > max_equipments {
            return Err(RuntimeProtocolError::new(
                "warehouse_limit_exceeded",
                format!("仓库返回 {} 项，超过请求上限 {max_equipments}", self.count),
            ));
        }
        if self.complete != (!self.truncated && self.read_errors.is_empty()) {
            return Err(RuntimeProtocolError::new(
                "warehouse_completeness_mismatch",
                "warehouse.complete 必须与 truncated 和 read_errors 保持一致",
            ));
        }

        let mut equipment_ids: HashSet<u64> = HashSet::with_capacity(self.items.len());
        let mut previous_equipment_id: Option<u64> = None;
        for item in &self.items {
            item.validate()?;
            if !equipment_ids.insert(item.equipment_id) {
                return Err(RuntimeProtocolError::new(
                    "warehouse_duplicate_equipment",
                    format!("仓库重复返回 equipment_id={}", item.equipment_id),
                ));
            }
            if previous_equipment_id.is_some_and(|previous| previous >= item.equipment_id) {
                return Err(RuntimeProtocolError::new(
                    "warehouse_order_invalid",
                    "仓库必须按 equipment_id 严格升序返回",
                ));
            }
            previous_equipment_id = Some(item.equipment_id);
        }
        for error in &self.read_errors {
            error.validate()?;
        }
        Ok(())
    }
}

/// 仓库中按配置聚合计数的一种装备。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WarehouseEquipment {
    /// Lua 聚合装备对象的运行态 ID，不表示一件持久装备实例。
    pub equipment_id: u64,
    /// 用于业务匹配和配置查询的装备配置 ID。
    pub config_id: u64,
    /// 当前配置和强化等级的仓库数量。
    pub quantity: u64,
    /// 用户界面显示的 `+N` 强化等级。
    pub enhance_level: u32,
}

impl WarehouseEquipment {
    /// 校验装备标识和聚合数量均为正数。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("warehouse_item.equipment_id", self.equipment_id)?;
        validate_positive_u64("warehouse_item.config_id", self.config_id)?;
        validate_positive_u64("warehouse_item.quantity", self.quantity)
    }
}

/// 单个仓库条目无法完整读取时的定位信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentReadError {
    /// 已知装备运行态 ID；读取 ID 本身失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub equipment_id: Option<u64>,
    /// 稳定字段错误码。
    pub code: String,
    /// 面向用户的中文说明。
    pub message: String,
}

impl EquipmentReadError {
    /// 校验可选装备标识、稳定错误码和用户可读消息。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        if let Some(equipment_id) = self.equipment_id {
            validate_positive_u64("equipment_read_error.equipment_id", equipment_id)?;
        }
        validate_stable_token("equipment_read_error.code", &self.code)?;
        validate_non_empty("equipment_read_error.message", &self.message, 1024)
    }
}

/// 当前物资和装备仓库的已用容量与上限。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerResources {
    /// 当前可用物资。
    pub gold: u64,
    /// `EquipmentProxy` 报告的装备仓库已用容量。
    pub equipment_capacity: u64,
    /// 玩家当前装备仓库容量上限。
    pub equipment_limit: u64,
}

impl PlayerResources {
    /// 容量上限必须为正数；物资和已用容量允许为零。
    pub(crate) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("player.equipment_limit", self.equipment_limit)
    }
}

/// `snapshot_ship_details` 返回的当前客户端展示详情快照。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotShipDetailsResult {
    /// 舰船详情 schema 固定为 4。
    pub schema_version: u32,
    /// 未截断且没有逐项读取错误时为 true。
    pub complete: bool,
    /// 成功返回的舰船数量，必须等于 `ships.len()`。
    pub count: u32,
    /// 船坞条目超过请求上限时为 true。
    pub truncated: bool,
    /// loader 已在当前目标上验证的模块身份。
    pub source: ShipDetailSource,
    /// 按舰船实例 ID 严格升序排列的详情。
    pub ships: Vec<RuntimeShipDetail>,
    /// 无法完整解析的舰船或技能诊断。
    pub read_errors: Vec<ShipDetailReadError>,
}

impl SnapshotShipDetailsResult {
    /// 复核版本、来源模块、完整性、上限、排序和全部舰船字段不变量。
    pub(crate) fn validate(
        &self,
        max_ships: u32,
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 4 {
            return Err(RuntimeProtocolError::new(
                "ship_details_schema_unsupported",
                format!("只支持舰船详情 schema 4，实际为 {}", self.schema_version),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        if self.ships.len() != self.count as usize {
            return Err(RuntimeProtocolError::new(
                "ship_details_count_mismatch",
                format!(
                    "snapshot_ship_details.count={}，但 ships 实际有 {} 项",
                    self.count,
                    self.ships.len()
                ),
            ));
        }
        if self.count > max_ships {
            return Err(RuntimeProtocolError::new(
                "ship_details_limit_exceeded",
                format!("舰船详情返回 {} 艘，超过请求上限 {max_ships}", self.count),
            ));
        }
        if self.complete != (!self.truncated && self.read_errors.is_empty()) {
            return Err(RuntimeProtocolError::new(
                "ship_details_completeness_mismatch",
                "舰船详情 complete 必须与 truncated 和 read_errors 保持一致",
            ));
        }

        let mut previous_ship_id: Option<u64> = None;
        for ship in &self.ships {
            ship.validate()?;
            if previous_ship_id.is_some_and(|previous| previous >= ship.ship_id) {
                return Err(RuntimeProtocolError::new(
                    "ship_details_order_invalid",
                    "舰船详情必须按 ship_id 严格升序返回",
                ));
            }
            previous_ship_id = Some(ship.ship_id);
        }
        for error in &self.read_errors {
            error.validate()?;
        }
        Ok(())
    }
}

/// 当前详情快照所依赖的目标模块身份。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShipDetailSource {
    /// loader 已验证并写入 bootstrap 的 64 位小写 SHA-256。
    pub module_sha256: String,
}

impl ShipDetailSource {
    /// 要求摘要格式规范且与握手固定的目标模块身份完全一致。
    fn validate(&self, expected: &str) -> Result<(), RuntimeProtocolError> {
        validate_sha256("ship_details.source.module_sha256", &self.module_sha256)?;
        validate_sha256("expected_module_sha256", expected)?;
        if self.module_sha256 != expected {
            return Err(RuntimeProtocolError::new(
                "ship_details_source_mismatch",
                format!(
                    "舰船详情模块摘要应为 {expected}，实际为 {}",
                    self.module_sha256
                ),
            ));
        }
        Ok(())
    }
}

/// 当前客户端方法解析出的单艘舰船展示详情。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipDetail {
    /// 游戏运行态中的舰船唯一 ID。
    pub ship_id: u64,
    /// 用于关联当前舰船静态配置的 ID。
    pub config_id: u64,
    /// 当前客户端解析出的舰船显示名称。
    pub name: String,
    /// 舰船当前等级。
    pub level: u32,
    /// 当前舰船允许达到的等级上限。
    pub max_level: u32,
    /// 当前等级内已经积累的经验。
    pub experience_in_level: u64,
    /// 客户端计算的累计经验。
    pub total_experience: u64,
    /// 升到下一级所需经验；满级时为零。
    pub next_level_experience: u64,
    /// 客户端以百分之一为单位保存的原始好感值。
    pub intimacy_raw: u64,
    /// 客户端当前规则计算的好感显示上限，不使用百分之一原始单位。
    pub intimacy_maximum: u64,
    /// 客户端 `getIntimacyLevel` 返回的阶段配置 ID。
    pub intimacy_stage_id: u64,
    /// 当前客户端 `intimacy_template` 返回的本地化阶段说明。
    pub intimacy_stage_description: String,
    /// 舰船是否已经完成誓约。
    pub proposed: bool,
    /// 舰船对象保存的誓约时间原始值。
    pub propose_time: u64,
    /// 舰船对象保存的获得时间原始值。
    pub create_time: u64,
    /// 客户端计算的综合性能。
    pub combat_power: u64,
    /// 舰船是否处于锁定状态。
    pub locked: bool,
    /// 开始战斗、结束战斗和总油耗的计算结果。
    pub oil_cost: RuntimeShipOilCost,
    /// 舰种、装甲、阵营、稀有度和星级等当前分类。
    pub classification: RuntimeShipClassification,
    /// 不计装备的舰船基础属性。
    pub base_attributes: RuntimeShipAttributes,
    /// 已计入装备、尚未计入其他全局修正的属性。
    pub equipment_applied_attributes: RuntimeShipAttributes,
    /// 客户端最终用于展示和计算的有效属性。
    pub effective_attributes: RuntimeShipAttributes,
    /// 按一至五号槽排列的允许装备类型集合。
    pub slot_rules: Vec<RuntimeShipSlotRule>,
    /// 按原始技能 ID 严格升序排列的自身技能详情。
    pub skills: Vec<RuntimeShipSkillDetail>,
}

impl RuntimeShipDetail {
    /// 校验标识、养成边界、分类、属性阶段和技能严格顺序。
    pub(super) fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("ship_detail.ship_id", self.ship_id)?;
        validate_positive_u64("ship_detail.config_id", self.config_id)?;
        validate_non_empty("ship_detail.name", &self.name, 512)?;
        validate_positive_u32("ship_detail.level", self.level)?;
        validate_positive_u32("ship_detail.max_level", self.max_level)?;
        validate_positive_u64("ship_detail.intimacy_stage_id", self.intimacy_stage_id)?;
        validate_non_empty(
            "ship_detail.intimacy_stage_description",
            &self.intimacy_stage_description,
            512,
        )?;
        if self.level > self.max_level {
            return Err(RuntimeProtocolError::new(
                "ship_detail_level_invalid",
                format!(
                    "ship_id={} 当前等级 {} 高于上限 {}",
                    self.ship_id, self.level, self.max_level
                ),
            ));
        }
        if self.level >= self.max_level && self.next_level_experience != 0 {
            return Err(RuntimeProtocolError::new(
                "ship_detail_next_experience_invalid",
                format!("ship_id={} 已满级但下一级经验不为零", self.ship_id),
            ));
        }
        if self.level < self.max_level && self.next_level_experience == 0 {
            return Err(RuntimeProtocolError::new(
                "ship_detail_next_experience_invalid",
                format!("ship_id={} 未满级但下一级经验为零", self.ship_id),
            ));
        }
        self.oil_cost.validate()?;
        self.classification.validate()?;
        self.base_attributes.validate("base_attributes")?;
        self.equipment_applied_attributes
            .validate("equipment_applied_attributes")?;
        self.effective_attributes.validate("effective_attributes")?;
        if self.slot_rules.len() != SHIP_EQUIPMENT_SLOT_COUNT {
            return Err(RuntimeProtocolError::new(
                "ship_slot_rule_count_invalid",
                format!(
                    "ship_id={} 必须返回 {SHIP_EQUIPMENT_SLOT_COUNT} 个槽位规则，实际为 {}",
                    self.ship_id,
                    self.slot_rules.len()
                ),
            ));
        }
        for (index, rule) in self.slot_rules.iter().enumerate() {
            rule.validate((index + 1) as u32, self.ship_id)?;
        }
        if self.skills.len() > MAX_SHIP_SKILLS {
            return Err(RuntimeProtocolError::new(
                "ship_skill_limit_exceeded",
                format!(
                    "ship_id={} 返回 {} 个技能详情，超过上限 {MAX_SHIP_SKILLS}",
                    self.ship_id,
                    self.skills.len()
                ),
            ));
        }
        let mut previous_skill_id: Option<u64> = None;
        for skill in &self.skills {
            skill.validate()?;
            if previous_skill_id.is_some_and(|previous| previous >= skill.skill_id) {
                return Err(RuntimeProtocolError::new(
                    "ship_skill_detail_order_invalid",
                    format!(
                        "ship_id={} 的技能详情必须按 skill_id 严格升序返回",
                        self.ship_id
                    ),
                ));
            }
            previous_skill_id = Some(skill.skill_id);
        }
        Ok(())
    }
}

/// 当前突破阶段下单个装备槽位允许的装备类型集合。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipSlotRule {
    /// 固定为一至五号槽，并与数组位置一致。
    pub slot_index: u32,
    /// 正整数、严格升序且不重复的装备类型 ID。
    pub allowed_equipment_type_ids: Vec<u64>,
}

impl RuntimeShipSlotRule {
    /// 校验槽位顺序、集合容量和类型 ID 的规范顺序。
    fn validate(&self, expected_slot_index: u32, ship_id: u64) -> Result<(), RuntimeProtocolError> {
        if self.slot_index != expected_slot_index {
            return Err(RuntimeProtocolError::new(
                "ship_slot_rule_order_invalid",
                format!(
                    "ship_id={ship_id} 的第 {expected_slot_index} 个槽位规则声明为 {} 号槽",
                    self.slot_index
                ),
            ));
        }
        if self.allowed_equipment_type_ids.is_empty()
            || self.allowed_equipment_type_ids.len() > MAX_SHIP_SLOT_EQUIPMENT_TYPES
        {
            return Err(RuntimeProtocolError::new(
                "ship_slot_equipment_type_count_invalid",
                format!(
                    "ship_id={ship_id} 的 {} 号槽必须包含 1 至 {MAX_SHIP_SLOT_EQUIPMENT_TYPES} 个允许装备类型",
                    self.slot_index
                ),
            ));
        }

        let mut previous_type_id: Option<u64> = None;
        for &equipment_type_id in &self.allowed_equipment_type_ids {
            validate_positive_lua_integer(
                "ship_slot_rule.allowed_equipment_type_id",
                equipment_type_id,
            )?;
            if previous_type_id.is_some_and(|previous| previous >= equipment_type_id) {
                return Err(RuntimeProtocolError::new(
                    "ship_slot_equipment_type_order_invalid",
                    format!(
                        "ship_id={ship_id} 的 {} 号槽装备类型必须严格升序",
                        self.slot_index
                    ),
                ));
            }
            previous_type_id = Some(equipment_type_id);
        }
        Ok(())
    }
}

/// 三种客户端油耗方法的显式结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipOilCost {
    /// 开始战斗时消耗的油量。
    pub start: u64,
    /// 结束战斗时消耗的油量。
    pub end: u64,
    /// 客户端计算的总油耗，必须等于开始与结束油耗之和。
    pub total: u64,
}

impl RuntimeShipOilCost {
    /// 总油耗必须无溢出地等于开始与结束油耗之和。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        if self.start.checked_add(self.end) != Some(self.total) {
            return Err(RuntimeProtocolError::new(
                "ship_oil_cost_mismatch",
                format!(
                    "油耗 total={}，但 start={} 与 end={} 之和不一致",
                    self.total, self.start, self.end
                ),
            ));
        }
        Ok(())
    }
}

/// 舰种、装甲、阵营、稀有度与星级等客户端分类。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipClassification {
    /// 同一舰船系列共享的分组 ID。
    pub group_id: u64,
    /// 当前舰种分类 ID。
    pub ship_type_id: u64,
    /// 当前客户端解析出的舰种名称。
    pub ship_type_name: String,
    /// 当前装甲分类 ID。
    pub armor_type_id: u64,
    /// 当前客户端解析出的装甲名称。
    pub armor_type_name: String,
    /// 当前阵营分类 ID。
    pub nation_id: u64,
    /// 当前客户端解析出的阵营名称。
    pub nation_name: String,
    /// 当前稀有度数值。
    pub rarity: u32,
    /// 当前突破状态对应的星级。
    pub star: u32,
    /// 当前舰船允许达到的最大星级。
    pub max_star: u32,
    /// 当前舰船使用的皮肤配置 ID。
    pub skin_id: u64,
}

impl RuntimeShipClassification {
    /// 分类标识和星级为正数，展示名称必须为有界非空 UTF-8。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("ship_classification.group_id", self.group_id)?;
        validate_positive_u64("ship_classification.ship_type_id", self.ship_type_id)?;
        validate_non_empty(
            "ship_classification.ship_type_name",
            &self.ship_type_name,
            512,
        )?;
        validate_positive_u64("ship_classification.armor_type_id", self.armor_type_id)?;
        validate_non_empty(
            "ship_classification.armor_type_name",
            &self.armor_type_name,
            512,
        )?;
        validate_positive_u64("ship_classification.nation_id", self.nation_id)?;
        validate_non_empty("ship_classification.nation_name", &self.nation_name, 512)?;
        validate_positive_u32("ship_classification.rarity", self.rarity)?;
        validate_positive_u32("ship_classification.star", self.star)?;
        validate_positive_u32("ship_classification.max_star", self.max_star)?;
        validate_positive_u64("ship_classification.skin_id", self.skin_id)?;
        if self.star > self.max_star {
            return Err(RuntimeProtocolError::new(
                "ship_star_invalid",
                format!("当前星级 {} 高于最大星级 {}", self.star, self.max_star),
            ));
        }
        Ok(())
    }
}

/// 客户端属性阶段中当前已冻结的十一项面板属性。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipAttributes {
    /// 耐久。
    pub durability: f64,
    /// 炮击。
    pub cannon: f64,
    /// 雷击。
    pub torpedo: f64,
    /// 防空。
    pub anti_aircraft: f64,
    /// 航空。
    pub air: f64,
    /// 装填。
    pub reload: f64,
    /// 命中。
    pub hit: f64,
    /// 机动。
    pub dodge: f64,
    /// 反潜。
    pub anti_sub: f64,
    /// 幸运。
    pub luck: f64,
    /// 航速；客户端可能返回合法小数。
    pub speed: f64,
}

impl RuntimeShipAttributes {
    /// 所有属性必须是有限非负数，航速等字段允许合法小数。
    fn validate(&self, stage: &'static str) -> Result<(), RuntimeProtocolError> {
        for (name, value) in [
            ("durability", self.durability),
            ("cannon", self.cannon),
            ("torpedo", self.torpedo),
            ("anti_aircraft", self.anti_aircraft),
            ("air", self.air),
            ("reload", self.reload),
            ("hit", self.hit),
            ("dodge", self.dodge),
            ("anti_sub", self.anti_sub),
            ("luck", self.luck),
            ("speed", self.speed),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(RuntimeProtocolError::new(
                    "ship_attribute_invalid",
                    format!("{stage}.{name} 必须是有限非负数，实际为 {value}"),
                ));
            }
        }
        Ok(())
    }
}

/// 自身技能的原始身份、当前生效展示身份和等级解析结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeShipSkillDetail {
    /// `Ship.skills` 使用的原始技能 ID。
    pub skill_id: u64,
    /// 当前等级实际展示的生效技能 ID。
    pub effective_skill_id: u64,
    /// 当前客户端解析出的技能显示名称。
    pub name: String,
    /// 当前学习等级。
    pub level: u32,
    /// 技能等级上限。
    pub max_level: u32,
    /// 当前等级内已经积累的技能经验。
    pub experience: u64,
    /// 升到下一级所需经验；满级时为零。
    pub next_level_experience: u64,
    /// 静态技能配置保存的未插值描述模板。
    pub description_template: String,
    /// 当前等级参数已经代入后的效果文本。
    pub current_effect: String,
}

impl RuntimeShipSkillDetail {
    /// 技能身份、等级和文本必须满足当前客户端详情契约。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_u64("ship_skill_detail.skill_id", self.skill_id)?;
        validate_positive_u64(
            "ship_skill_detail.effective_skill_id",
            self.effective_skill_id,
        )?;
        validate_non_empty("ship_skill_detail.name", &self.name, 512)?;
        validate_positive_u32("ship_skill_detail.level", self.level)?;
        validate_positive_u32("ship_skill_detail.max_level", self.max_level)?;
        if self.level > self.max_level {
            return Err(RuntimeProtocolError::new(
                "ship_skill_level_invalid",
                format!(
                    "skill_id={} 当前等级 {} 高于上限 {}",
                    self.skill_id, self.level, self.max_level
                ),
            ));
        }
        if self.level >= self.max_level && self.next_level_experience != 0 {
            return Err(RuntimeProtocolError::new(
                "ship_skill_next_experience_invalid",
                format!("skill_id={} 已满级但下一级经验不为零", self.skill_id),
            ));
        }
        if self.level < self.max_level && self.next_level_experience == 0 {
            return Err(RuntimeProtocolError::new(
                "ship_skill_next_experience_invalid",
                format!("skill_id={} 未满级但下一级经验为零", self.skill_id),
            ));
        }
        validate_text(
            "ship_skill_detail.description_template",
            &self.description_template,
            4096,
        )?;
        validate_text(
            "ship_skill_detail.current_effect",
            &self.current_effect,
            4096,
        )
    }
}

/// 舰船详情中单艘舰船或技能读取失败的定位信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShipDetailReadError {
    /// 已知舰船 ID；读取舰船身份失败时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub ship_id: Option<u64>,
    /// 已知技能 ID；错误不属于单个技能时为 null。
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub skill_id: Option<u64>,
    /// 稳定字段错误码。
    pub code: String,
    /// 面向用户的中文诊断说明。
    pub message: String,
}

impl ShipDetailReadError {
    /// 校验可空定位标识、稳定错误码和面向用户的诊断文本。
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        if let Some(ship_id) = self.ship_id {
            validate_positive_u64("ship_detail_error.ship_id", ship_id)?;
        }
        if let Some(skill_id) = self.skill_id {
            validate_positive_u64("ship_detail_error.skill_id", skill_id)?;
        }
        validate_stable_token("ship_detail_error.code", &self.code)?;
        validate_non_empty("ship_detail_error.message", &self.message, 1024)
    }
}

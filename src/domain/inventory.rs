//! 表示账号仓库、背包和当前玩家资源的不可变运行态。

use super::{EnhanceLevel, EquipmentConfigId, EquipmentFamilyId, EquipmentSourceRef};

/// 仓库中同一装备配置和强化等级的聚合数量。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WarehouseEquipmentStack {
    runtime_group_id: u64,
    config_id: EquipmentConfigId,
    family_id: EquipmentFamilyId,
    enhance_level: EnhanceLevel,
    quantity: u64,
}

impl WarehouseEquipmentStack {
    pub(crate) const fn new(
        runtime_group_id: u64,
        config_id: EquipmentConfigId,
        family_id: EquipmentFamilyId,
        enhance_level: EnhanceLevel,
        quantity: u64,
    ) -> Self {
        Self {
            runtime_group_id,
            config_id,
            family_id,
            enhance_level,
            quantity,
        }
    }

    /// 返回当次会话中 Lua 聚合对象的诊断标识，不作为跨读取稳定身份。
    pub const fn runtime_group_id(self) -> u64 {
        self.runtime_group_id
    }

    /// 返回仓库聚合使用的具体装备配置 ID。
    pub const fn config_id(self) -> EquipmentConfigId {
        self.config_id
    }

    /// 返回配置所属的装备族 ID。
    pub const fn family_id(self) -> EquipmentFamilyId {
        self.family_id
    }

    /// 返回仓库条目的实际强化等级。
    pub const fn enhance_level(self) -> EnhanceLevel {
        self.enhance_level
    }

    /// 返回该配置和强化等级在仓库中的聚合数量。
    pub const fn quantity(self) -> u64 {
        self.quantity
    }

    /// 返回计划器使用的稳定仓库来源引用。
    pub const fn source(self) -> EquipmentSourceRef {
        EquipmentSourceRef::Warehouse(self.config_id)
    }
}

/// 按装备配置 ID 严格升序排列的仓库聚合状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentInventory {
    warehouse: Vec<WarehouseEquipmentStack>,
}

impl EquipmentInventory {
    pub(crate) fn new(warehouse: Vec<WarehouseEquipmentStack>) -> Self {
        Self { warehouse }
    }

    /// 返回仓库中的全部配置聚合条目。
    pub fn warehouse(&self) -> &[WarehouseEquipmentStack] {
        &self.warehouse
    }

    /// 按配置 ID 查找仓库聚合条目。
    pub fn warehouse_stack(&self, config_id: EquipmentConfigId) -> Option<WarehouseEquipmentStack> {
        self.warehouse
            .binary_search_by_key(&config_id, |item| item.config_id())
            .ok()
            .map(|index| self.warehouse[index])
    }
}

/// 当前背包条目提供的即时合成信息。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BagComposeAvailability {
    recipe_id: u64,
    material_id: u64,
    material_count: u64,
    gold: u64,
    equipment_config_id: Option<EquipmentConfigId>,
    max_count: Option<u64>,
}

impl BagComposeAvailability {
    pub(crate) const fn new(
        recipe_id: u64,
        material_id: u64,
        material_count: u64,
        gold: u64,
        equipment_config_id: Option<EquipmentConfigId>,
        max_count: Option<u64>,
    ) -> Self {
        Self {
            recipe_id,
            material_id,
            material_count,
            gold,
            equipment_config_id,
            max_count,
        }
    }

    /// 返回客户端配方 ID。
    pub const fn recipe_id(self) -> u64 {
        self.recipe_id
    }

    /// 返回单次合成消耗的材料 ID。
    pub const fn material_id(self) -> u64 {
        self.material_id
    }

    /// 返回单次合成消耗的材料数量。
    pub const fn material_count(self) -> u64 {
        self.material_count
    }

    /// 返回单次合成消耗的物资。
    pub const fn gold(self) -> u64 {
        self.gold
    }

    /// 返回装备产物的配置 ID；非装备产物保持为空。
    pub const fn equipment_config_id(self) -> Option<EquipmentConfigId> {
        self.equipment_config_id
    }

    /// 返回当前资源条件下客户端报告的最大合成次数。
    pub const fn max_count(self) -> Option<u64> {
        self.max_count
    }
}

/// 一种账号持有的背包物品。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BagItem {
    item_id: u64,
    quantity: u64,
    name: String,
    compose: Option<BagComposeAvailability>,
}

impl BagItem {
    pub(crate) fn new(
        item_id: u64,
        quantity: u64,
        name: String,
        compose: Option<BagComposeAvailability>,
    ) -> Self {
        Self {
            item_id,
            quantity,
            name,
            compose,
        }
    }

    /// 返回游戏背包物品 ID。
    pub const fn item_id(&self) -> u64 {
        self.item_id
    }

    /// 返回当前持有数量；协议允许零值。
    pub const fn quantity(&self) -> u64 {
        self.quantity
    }

    /// 返回当前客户端解析出的物品名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回该背包物品的即时合成信息。
    pub const fn compose(&self) -> Option<BagComposeAvailability> {
        self.compose
    }
}

/// 按物品 ID 严格升序排列的完整背包。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BagInventory {
    items: Vec<BagItem>,
}

impl BagInventory {
    pub(crate) fn new(items: Vec<BagItem>) -> Self {
        Self { items }
    }

    /// 返回完整背包条目。
    pub fn items(&self) -> &[BagItem] {
        &self.items
    }

    /// 按物品 ID 查找背包条目。
    pub fn item(&self, item_id: u64) -> Option<&BagItem> {
        self.items
            .binary_search_by_key(&item_id, BagItem::item_id)
            .ok()
            .map(|index| &self.items[index])
    }
}

/// 当前物资和装备仓库容量状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountResources {
    gold: u64,
    equipment_capacity: u64,
    equipment_limit: u64,
}

impl AccountResources {
    pub(crate) const fn new(gold: u64, equipment_capacity: u64, equipment_limit: u64) -> Self {
        Self {
            gold,
            equipment_capacity,
            equipment_limit,
        }
    }

    /// 返回当前可用物资。
    pub const fn gold(self) -> u64 {
        self.gold
    }

    /// 返回装备仓库当前已用容量。
    pub const fn equipment_capacity(self) -> u64 {
        self.equipment_capacity
    }

    /// 返回装备仓库容量上限。
    pub const fn equipment_limit(self) -> u64 {
        self.equipment_limit
    }
}

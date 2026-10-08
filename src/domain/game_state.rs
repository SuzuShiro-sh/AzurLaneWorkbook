//! 聚合一次一致性读取产生的完整不可变游戏状态。

use super::{
    AccountResources, BagInventory, EquipmentCatalog, EquipmentDetailCatalog, EquipmentInventory,
    RawRecordSet, ShipCatalog, ShipRoster,
};

/// 完整游戏状态及其稳定摘要编码的冻结版本。
pub const GAME_STATE_SCHEMA_VERSION: u32 = 7;

/// 核心操作状态始终完整读取，展示证据按本次请求标记覆盖范围。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameReadScope {
    equipment_skill_effects: bool,
    equipment_weapons: bool,
    ship_skill_effects: bool,
    #[serde(default)]
    ship_technology: bool,
}

impl GameReadScope {
    pub const fn full() -> Self {
        Self {
            ship_skill_effects: true,
            ship_technology: true,
            equipment_weapons: true,
            equipment_skill_effects: true,
        }
    }
    pub const fn with_ship_skill_effects(ship_skill_effects: bool) -> Self {
        Self {
            ship_skill_effects,
            ..Self::full()
        }
    }
    pub const fn with_equipment_details(mut self, weapons: bool, skills: bool) -> Self {
        self.equipment_weapons = weapons;
        self.equipment_skill_effects = skills;
        self
    }
    pub const fn with_ship_technology(mut self, requested: bool) -> Self {
        self.ship_technology = requested;
        self
    }
    pub const fn ship_technology(self) -> bool {
        self.ship_technology
    }
    pub const fn equipment_weapons(self) -> bool {
        self.equipment_weapons
    }
    pub const fn equipment_skill_effects(self) -> bool {
        self.equipment_skill_effects
    }
    pub const fn ship_skill_effects(self) -> bool {
        self.ship_skill_effects
    }
}

/// 生成完整游戏状态时使用的协议版本和各组件内容身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameStateSource {
    read_scope: GameReadScope,
    module_sha256: String,
    owned_state_schema_version: u32,
    ship_details_schema_version: u32,
    ship_catalog_schema_version: u32,
    equipment_catalog_schema_version: u32,
    raw_records_schema_version: u32,
    owned_state_content_sha256: String,
    ship_roster_content_sha256: String,
    ship_catalog_content_sha256: String,
    equipment_catalog_content_sha256: String,
    raw_records_content_sha256: String,
    content_sha256: String,
}

impl GameStateSource {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        module_sha256: String,
        owned_state_schema_version: u32,
        ship_details_schema_version: u32,
        ship_catalog_schema_version: u32,
        equipment_catalog_schema_version: u32,
        raw_records_schema_version: u32,
        owned_state_content_sha256: String,
        ship_roster_content_sha256: String,
        ship_catalog_content_sha256: String,
        equipment_catalog_content_sha256: String,
        raw_records_content_sha256: String,
        content_sha256: String,
    ) -> Self {
        Self {
            read_scope: GameReadScope::full(),
            module_sha256,
            owned_state_schema_version,
            ship_details_schema_version,
            ship_catalog_schema_version,
            equipment_catalog_schema_version,
            raw_records_schema_version,
            owned_state_content_sha256,
            ship_roster_content_sha256,
            ship_catalog_content_sha256,
            equipment_catalog_content_sha256,
            raw_records_content_sha256,
            content_sha256,
        }
    }

    pub const fn read_scope(&self) -> GameReadScope {
        self.read_scope
    }

    pub(crate) fn with_read_scope(mut self, scope: GameReadScope) -> Self {
        self.read_scope = scope;
        self
    }

    /// 返回 loader 在当前目标上验证的模块 SHA-256。
    pub fn module_sha256(&self) -> &str {
        &self.module_sha256
    }

    /// 返回完整持有状态的运行时协议版本。
    pub const fn owned_state_schema_version(&self) -> u32 {
        self.owned_state_schema_version
    }

    /// 返回舰船详情的运行时协议版本。
    pub const fn ship_details_schema_version(&self) -> u32 {
        self.ship_details_schema_version
    }

    /// 返回规范化舰船静态目录版本。
    pub const fn ship_catalog_schema_version(&self) -> u32 {
        self.ship_catalog_schema_version
    }

    /// 返回规范化装备目录版本。
    pub const fn equipment_catalog_schema_version(&self) -> u32 {
        self.equipment_catalog_schema_version
    }

    /// 返回完整装备原始记录版本。
    pub const fn raw_records_schema_version(&self) -> u32 {
        self.raw_records_schema_version
    }

    /// 返回前后完全一致的持有状态摘要。
    pub fn owned_state_content_sha256(&self) -> &str {
        &self.owned_state_content_sha256
    }

    /// 返回舰船名册映射输入摘要。
    pub fn ship_roster_content_sha256(&self) -> &str {
        &self.ship_roster_content_sha256
    }

    /// 返回规范化舰船静态目录摘要。
    pub fn ship_catalog_content_sha256(&self) -> &str {
        &self.ship_catalog_content_sha256
    }

    /// 返回规范化装备目录摘要。
    pub fn equipment_catalog_content_sha256(&self) -> &str {
        &self.equipment_catalog_content_sha256
    }

    /// 返回装备原始详情摘要。
    pub fn raw_records_content_sha256(&self) -> &str {
        &self.raw_records_content_sha256
    }

    /// 返回完整领域状态的稳定语义摘要。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 一次完整读取产生的舰船、装备、库存、背包和资源状态。
#[derive(Clone, Debug, PartialEq)]
pub struct GameState {
    source: GameStateSource,
    ships: ShipRoster,
    ship_catalog: ShipCatalog,
    equipment_catalog: EquipmentCatalog,
    equipment_details: EquipmentDetailCatalog,
    equipment_inventory: EquipmentInventory,
    bag: BagInventory,
    resources: AccountResources,
    raw_records: RawRecordSet,
}

impl GameState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: GameStateSource,
        ships: ShipRoster,
        ship_catalog: ShipCatalog,
        equipment_catalog: EquipmentCatalog,
        equipment_details: EquipmentDetailCatalog,
        equipment_inventory: EquipmentInventory,
        bag: BagInventory,
        resources: AccountResources,
        raw_records: RawRecordSet,
    ) -> Self {
        Self {
            source,
            ships,
            ship_catalog,
            equipment_catalog,
            equipment_details,
            equipment_inventory,
            bag,
            resources,
            raw_records,
        }
    }

    /// 返回完整游戏状态领域契约版本。
    pub const fn schema_version(&self) -> u32 {
        GAME_STATE_SCHEMA_VERSION
    }

    /// 返回组件版本、模块身份和内容摘要。
    pub const fn source(&self) -> &GameStateSource {
        &self.source
    }

    /// 返回含固定五槽的完整舰船名册。
    pub const fn ships(&self) -> &ShipRoster {
        &self.ships
    }

    /// 返回当前客户端完整舰船静态目录。
    pub const fn ship_catalog(&self) -> &ShipCatalog {
        &self.ship_catalog
    }

    /// 返回全部装备族、强化配置和静态合成配方。
    pub const fn equipment_catalog(&self) -> &EquipmentCatalog {
        &self.equipment_catalog
    }

    /// 返回完整武器参数和装备技能效果。
    pub const fn equipment_details(&self) -> &EquipmentDetailCatalog {
        &self.equipment_details
    }

    /// 返回按具体配置聚合的仓库装备。
    pub const fn equipment_inventory(&self) -> &EquipmentInventory {
        &self.equipment_inventory
    }

    /// 返回当前完整背包。
    pub const fn bag(&self) -> &BagInventory {
        &self.bag
    }

    /// 返回物资和装备仓库容量。
    pub const fn resources(&self) -> AccountResources {
        self.resources
    }

    /// 返回规范化模型未展开的完整原始记录。
    pub const fn raw_records(&self) -> &RawRecordSet {
        &self.raw_records
    }
}

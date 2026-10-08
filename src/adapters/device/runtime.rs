//! 专用设备 agent 的严格 RPC 协议和宿主客户端。

mod client;
mod protocol;

pub(crate) use client::LiveProtocolProbeResult;
pub use client::{AgentClient, ClientStage, RuntimeClientError};
#[cfg(test)]
pub(crate) use protocol::validate_capabilities_result;
pub use protocol::{
    AccountBeforeResult, AgentError, AgentIdentity, BagItem, CapabilitiesResult, CapabilityStatus,
    ComposeRecipe, ComposeRecipePageReadError, ComposeRecipePageResult, DockSnapshot,
    EquipmentCommandAction, EquipmentCommandActionKind, EquipmentCommandEquipment,
    EquipmentCommandMaterialCost, EquipmentCommandPhase, EquipmentCommandReceipt,
    EquipmentCommandStatus, EquipmentComposeRecipe, EquipmentConfigBatchResult,
    EquipmentConfigPageReadError, EquipmentConfigPageResult, EquipmentConfigSource,
    EquipmentReadError, EquipmentReferenceNameBatchResult, EquipmentWeaponBatchResult,
    ExpectedAgent, HealthResult, OwnedQuery, OwnedQueryKind, OwnedQueryResult, PlayerResources,
    ReadError, RequestId, RetryDirective, RuntimeAbi, RuntimeAddress, RuntimeEquipment,
    RuntimeEquipmentAttributeName, RuntimeEquipmentCommand, RuntimeEquipmentConfig,
    RuntimeEquipmentNationName, RuntimeEquipmentShipTypeName, RuntimeEquipmentTypeName,
    RuntimeEquipmentWeaponDetail, RuntimeFleetKind, RuntimeFleetMembership, RuntimeFleetTeam,
    RuntimeProtocolError, RuntimeShip, RuntimeShipAttributes, RuntimeShipClassification,
    RuntimeShipDetail, RuntimeShipOilCost, RuntimeShipSkill, RuntimeShipSkillDetail,
    RuntimeShipSlot, RuntimeShipSlotRule, RuntimeSkillEffectDetail, RuntimeSkillEffectSource,
    SessionEffect, ShipCatalogPageReadError, ShipCatalogPageResult, ShipCatalogRecord,
    ShipCatalogTableKey, ShipDetailReadError, ShipDetailSource, ShipReadError,
    ShutdownPreparedResult, ShutdownState, SkillEffectBatchResult, SkillEffectQuery,
    SnapshotBagResult, SnapshotOwnedStateResult, SnapshotShipDetailsResult, WarehouseEquipment,
    WarehouseSnapshot,
};
pub(crate) use protocol::{
    EXPECTED_AGENT_VERSION, EquipmentReferenceNameBatchPayload, MAX_EQUIPMENT_PAGE_SIZE,
    MAX_EQUIPMENT_WEAPON_BATCH_SIZE, MAX_SHIP_CATALOG_PAGE_SIZE, MAX_SKILL_EFFECT_BATCH_SIZE,
    MAX_SNAPSHOT_ITEMS, validate_full_state_read_options,
};

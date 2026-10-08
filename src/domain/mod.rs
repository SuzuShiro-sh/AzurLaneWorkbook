//! 与工作簿、设备运行时和界面无关的业务模型与规则。

mod equipment;
mod equipment_detail;
mod game_state;
mod inventory;
mod inventory_action;
mod loadout;
mod raw_record;
mod ship;
mod ship_catalog;
mod skill_effect;

pub use equipment::{
    EQUIPMENT_CATALOG_SCHEMA_VERSION, EquipmentAttribute, EquipmentCatalog, EquipmentCatalogSource,
    EquipmentClassification, EquipmentCompatibility, EquipmentComposeRecipe, EquipmentDefinition,
    EquipmentDismantleSafety, EquipmentEnhancement, EquipmentFamily, EquipmentIdentity,
    EquipmentItemQuantity, EquipmentResources, EquipmentSkillReference, EquipmentSkillVisibility,
    NamedEquipmentNation, NamedEquipmentShipType, NamedEquipmentType,
};
pub use equipment_detail::{
    EquipmentDetailCatalog, EquipmentSkillDetail, EquipmentSkillDisplay, EquipmentSkillEffect,
    EquipmentSkillSource, EquipmentWeapon, SkillEffectArgument, SkillMixedTable, SkillTableEntry,
    SkillTableKey, SkillValue, SkillValueField, WeaponChargeParameter, WeaponPrecastParameter,
};
pub use game_state::{GAME_STATE_SCHEMA_VERSION, GameReadScope, GameState, GameStateSource};
pub use inventory::{
    AccountResources, BagComposeAvailability, BagInventory, BagItem, EquipmentInventory,
    WarehouseEquipmentStack,
};
pub use inventory_action::{
    EquipmentInventoryAction, EquipmentInventoryActionError, EquipmentInventoryActionKind,
    EquipmentInventoryPlan,
};
pub use loadout::{
    DesiredEquipment, DesiredSlotState, DesiredState, EnhanceLevel, EquipmentConfigId,
    EquipmentFamilyId, EquipmentSourceRef, LoadoutModelError, ShipInstanceId, ShipSlotRef,
    SlotIndex, SlotTarget, SourcePolicy,
};
pub use raw_record::{RawRecord, RawRecordKey, RawRecordSet};
pub use ship::{
    NamedShipClass, ShipAttributeBreakdown, ShipAttributeValues, ShipClassification, ShipEquipment,
    ShipEquipmentSlot, ShipFleetKind, ShipFleetMembership, ShipFleetTeam, ShipGrowth, ShipIdentity,
    ShipIntimacy, ShipOilCost, ShipPerformance, ShipProfile, ShipRoster, ShipRosterSource,
    ShipSkill, ShipSkillIdentity, ShipSkillProgress, ShipStars,
};
pub use ship_catalog::{
    SHIP_CATALOG_SCHEMA_VERSION, ShipCatalog, ShipCatalogGroup, ShipCatalogSource, ShipStaticSkill,
};
pub use skill_effect::{
    SkillEffectEvidence, SkillEffectEvidenceCatalog, SkillEffectEvidenceKey,
    SkillEffectParameterSource, SkillEffectParameters, SkillEffectSourceKind,
};

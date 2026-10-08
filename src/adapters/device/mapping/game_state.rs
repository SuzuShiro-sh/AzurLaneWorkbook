//! 将稳定持有状态、舰船详情和装备目录严格关联为完整游戏状态。

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use super::super::reading::equipment::EquipmentReadResult;
use super::super::reading::ship_catalog::ShipCatalogReadResult;
use super::super::runtime::{
    BagItem as RuntimeBagItem, MAX_SNAPSHOT_ITEMS, RuntimeProtocolError, RuntimeSkillEffectDetail,
    SnapshotOwnedStateResult, SnapshotShipDetailsResult, WarehouseEquipment,
};
use super::equipment_detail::{
    EquipmentDetailMappingError, EquipmentDetailProjection, map_equipment_details,
};
use super::ship::{ShipMappingError, map_ship_roster_with_skill_effects};
use super::ship_catalog::{ShipCatalogMappingError, combined_raw_content_sha256};
use crate::domain::{
    AccountResources, BagComposeAvailability, BagInventory, BagItem, EnhanceLevel,
    EquipmentCatalog, EquipmentConfigId, EquipmentDefinition, EquipmentInventory,
    GAME_STATE_SCHEMA_VERSION, GameState, GameStateSource, LoadoutModelError, RawRecordSet,
    ShipCatalog, ShipRoster, WarehouseEquipmentStack,
};
use suzushiro_content_digest::sha256_sorted_json;

#[allow(clippy::too_many_arguments)]
pub(crate) fn map_game_state_borrowed_with_scope(
    owned_before: &SnapshotOwnedStateResult,
    ship_details: &SnapshotShipDetailsResult,
    owned_after: &SnapshotOwnedStateResult,
    equipment: &EquipmentReadResult,
    ship_catalog: &ShipCatalogReadResult,
    ship_skill_effects: &[RuntimeSkillEffectDetail],
    expected_module_sha256: &str,
    scope: crate::domain::GameReadScope,
) -> Result<GameState, GameStateMappingError> {
    let prepared = prepare_game_state(
        owned_before,
        ship_details,
        owned_after,
        equipment,
        ship_catalog,
        ship_skill_effects,
        expected_module_sha256,
        scope,
    )?;
    assemble_game_state(
        owned_before,
        ship_details,
        equipment.catalog().clone(),
        prepared,
        expected_module_sha256,
    )
}

struct PreparedGameState {
    scope: crate::domain::GameReadScope,
    owned_state_content_sha256: String,
    ships: ShipRoster,
    equipment_details: crate::domain::EquipmentDetailCatalog,
    ship_catalog: ShipCatalog,
    raw_records: crate::domain::RawRecordSet,
}

#[allow(clippy::too_many_arguments)]
fn prepare_game_state(
    owned_before: &SnapshotOwnedStateResult,
    ship_details: &SnapshotShipDetailsResult,
    owned_after: &SnapshotOwnedStateResult,
    equipment: &EquipmentReadResult,
    ship_catalog: &ShipCatalogReadResult,
    ship_skill_effects: &[RuntimeSkillEffectDetail],
    expected_module_sha256: &str,
    scope: crate::domain::GameReadScope,
) -> Result<PreparedGameState, GameStateMappingError> {
    validate_owned_state(owned_before)?;
    ship_details.validate(MAX_SNAPSHOT_ITEMS, expected_module_sha256)?;

    let owned_before_sha256 = sha256_sorted_json(owned_before)?;
    let owned_after_sha256 = sha256_sorted_json(owned_after)?;
    if owned_before != owned_after {
        return Err(GameStateMappingError::OwnedStateChanged {
            before_sha256: owned_before_sha256,
            after_sha256: owned_after_sha256,
        });
    }

    let catalog_module = equipment.catalog().source().module_sha256();
    if catalog_module != expected_module_sha256 {
        return Err(GameStateMappingError::ModuleMismatch {
            component: "equipment_catalog",
            expected: expected_module_sha256.to_owned(),
            actual: catalog_module.to_owned(),
        });
    }

    let ships = map_ship_roster_with_skill_effects(owned_before, ship_details, ship_skill_effects)?;
    let projection: EquipmentDetailProjection = map_equipment_details(equipment)?;
    let (equipment_details, equipment_raw_records) = projection.into_parts();
    let ship_projection = super::ship_catalog::map_ship_catalog_with_scope(
        ship_catalog,
        ship_skill_effects,
        expected_module_sha256,
        scope,
    )?;
    let (ship_catalog, ship_raw_records) = ship_projection.into_parts();
    let source_content_sha256 = combined_raw_content_sha256(
        equipment_raw_records.source_content_sha256(),
        ship_catalog.source().content_sha256(),
        ship_skill_effects,
    )?;
    let mut records = equipment_raw_records.records().to_vec();
    records.extend(ship_raw_records);
    let raw_records = RawRecordSet::new(3, source_content_sha256, records);
    Ok(PreparedGameState {
        scope,
        owned_state_content_sha256: owned_before_sha256,
        ships,
        equipment_details,
        ship_catalog,
        raw_records,
    })
}

fn assemble_game_state(
    owned_before: &SnapshotOwnedStateResult,
    ship_details: &SnapshotShipDetailsResult,
    equipment_catalog: EquipmentCatalog,
    prepared: PreparedGameState,
    expected_module_sha256: &str,
) -> Result<GameState, GameStateMappingError> {
    let PreparedGameState {
        scope,
        owned_state_content_sha256,
        ships,
        equipment_details,
        ship_catalog,
        raw_records,
    } = prepared;
    let config_index = build_config_index(&equipment_catalog)?;
    validate_ship_slots(&ships, &config_index)?;
    validate_detail_references(&equipment_catalog, &equipment_details, scope)?;
    let equipment_inventory = map_warehouse(&owned_before.warehouse.items, &config_index)?;
    let bag = map_bag(&owned_before.bag.items, &equipment_catalog, &config_index)?;
    let resources = AccountResources::new(
        owned_before.player.gold,
        owned_before.player.equipment_capacity,
        owned_before.player.equipment_limit,
    );

    let source_digest = GameStateDigestInput {
        read_scope: scope,
        schema_version: GAME_STATE_SCHEMA_VERSION,
        module_sha256: expected_module_sha256,
        owned_state_schema_version: owned_before.schema_version,
        ship_details_schema_version: ship_details.schema_version,
        equipment_catalog_schema_version: equipment_catalog.schema_version(),
        owned_state_content_sha256: &owned_state_content_sha256,
        ship_roster_content_sha256: ships.source().content_sha256(),
        ship_catalog_schema_version: ship_catalog.source().schema_version(),
        ship_catalog_content_sha256: ship_catalog.source().content_sha256(),
        equipment_catalog_content_sha256: equipment_catalog.source().content_sha256(),
        raw_records_schema_version: raw_records.schema_version(),
        raw_records_content_sha256: raw_records.source_content_sha256(),
    };
    let content_sha256 = sha256_sorted_json(&source_digest)?;
    let source = GameStateSource::new(
        expected_module_sha256.to_owned(),
        owned_before.schema_version,
        ship_details.schema_version,
        ship_catalog.source().schema_version(),
        equipment_catalog.schema_version(),
        raw_records.schema_version(),
        owned_state_content_sha256,
        ships.source().content_sha256().to_owned(),
        ship_catalog.source().content_sha256().to_owned(),
        equipment_catalog.source().content_sha256().to_owned(),
        raw_records.source_content_sha256().to_owned(),
        content_sha256,
    )
    .with_read_scope(scope);

    Ok(GameState::new(
        source,
        ships,
        ship_catalog,
        equipment_catalog,
        equipment_details,
        equipment_inventory,
        bag,
        resources,
        raw_records,
    ))
}

fn validate_owned_state(owned: &SnapshotOwnedStateResult) -> Result<(), RuntimeProtocolError> {
    owned.validate(MAX_SNAPSHOT_ITEMS, MAX_SNAPSHOT_ITEMS, MAX_SNAPSHOT_ITEMS)
}

fn build_config_index(
    catalog: &EquipmentCatalog,
) -> Result<BTreeMap<EquipmentConfigId, &EquipmentDefinition>, GameStateMappingError> {
    let mut index = BTreeMap::new();
    for config in catalog
        .families()
        .iter()
        .flat_map(|family| family.configs())
    {
        let config_id = config.identity().config_id();
        if index.insert(config_id, config).is_some() {
            return Err(GameStateMappingError::DuplicateCatalogConfig {
                config_id: config_id.get(),
            });
        }
    }
    Ok(index)
}

fn validate_ship_slots(
    ships: &ShipRoster,
    configs: &BTreeMap<EquipmentConfigId, &EquipmentDefinition>,
) -> Result<(), GameStateMappingError> {
    for ship in ships.ships() {
        for slot in ship.slots() {
            let Some(equipment) = slot.equipment() else {
                continue;
            };
            let context = format!(
                "ship:{}:{}",
                ship.identity().instance_id().get(),
                slot.index().get()
            );
            let config = lookup_config(configs, equipment.config_id(), context.as_str())?;
            validate_enhance_level(
                context.as_str(),
                equipment.config_id(),
                equipment.enhance_level(),
                config,
            )?;
        }
    }
    Ok(())
}

fn validate_detail_references(
    catalog: &EquipmentCatalog,
    details: &crate::domain::EquipmentDetailCatalog,
    scope: crate::domain::GameReadScope,
) -> Result<(), GameStateMappingError> {
    for config in catalog
        .families()
        .iter()
        .flat_map(|family| family.configs())
    {
        for &weapon_id in config
            .weapon_ids()
            .iter()
            .filter(|_| scope.equipment_weapons())
        {
            if details.weapon(weapon_id).is_none() {
                return Err(GameStateMappingError::EquipmentDetailMissing {
                    config_id: config.identity().config_id().get(),
                    detail: format!("weapon:{weapon_id}"),
                });
            }
        }
        for skill in config
            .skill_references()
            .iter()
            .filter(|_| scope.equipment_skill_effects())
        {
            if details.skill(skill.skill_id(), skill.level()).is_none() {
                return Err(GameStateMappingError::EquipmentDetailMissing {
                    config_id: config.identity().config_id().get(),
                    detail: format!("skill:{}:{}", skill.skill_id(), skill.level()),
                });
            }
        }
    }
    Ok(())
}

fn map_warehouse(
    items: &[WarehouseEquipment],
    configs: &BTreeMap<EquipmentConfigId, &EquipmentDefinition>,
) -> Result<EquipmentInventory, GameStateMappingError> {
    let mut warehouse = Vec::with_capacity(items.len());
    for item in items {
        let config_id = EquipmentConfigId::new(item.config_id)?;
        let context = format!("warehouse:{}", config_id.get());
        let config = lookup_config(configs, config_id, context.as_str())?;
        let enhance_level = runtime_enhance_level(context.as_str(), item.enhance_level)?;
        validate_enhance_level(context.as_str(), config_id, enhance_level, config)?;
        warehouse.push(WarehouseEquipmentStack::new(
            item.equipment_id,
            config_id,
            config.identity().family_id(),
            enhance_level,
            item.quantity,
        ));
    }
    warehouse.sort_by_key(|item| item.config_id());
    if let Some(pair) = warehouse
        .windows(2)
        .find(|pair| pair[0].config_id() == pair[1].config_id())
    {
        return Err(GameStateMappingError::DuplicateWarehouseConfig {
            config_id: pair[0].config_id().get(),
        });
    }
    Ok(EquipmentInventory::new(warehouse))
}

fn map_bag(
    items: &[RuntimeBagItem],
    catalog: &EquipmentCatalog,
    configs: &BTreeMap<EquipmentConfigId, &EquipmentDefinition>,
) -> Result<BagInventory, GameStateMappingError> {
    let recipes: BTreeMap<u64, _> = catalog
        .recipes()
        .iter()
        .map(|recipe| (recipe.recipe_id(), recipe))
        .collect();
    let items = items
        .iter()
        .map(|item| {
            let compose = item
                .compose_recipe
                .as_ref()
                .map(
                    |recipe| -> Result<BagComposeAvailability, GameStateMappingError> {
                        if recipe.recipe_id != item.item_id {
                            return Err(GameStateMappingError::BagRecipeMismatch {
                                recipe_id: recipe.recipe_id,
                            });
                        }
                        let equipment_config_id = recipe
                            .equipment_id
                            .map(EquipmentConfigId::new)
                            .transpose()?;
                        if let Some(config_id) = equipment_config_id {
                            lookup_config(configs, config_id, "bag.compose_recipe")?;
                        }
                        match recipes.get(&recipe.recipe_id) {
                            Some(static_recipe) => validate_bag_recipe(recipe, static_recipe)?,
                            None if equipment_config_id.is_some() => {
                                return Err(GameStateMappingError::BagRecipeMismatch {
                                    recipe_id: recipe.recipe_id,
                                });
                            }
                            None => {}
                        }
                        Ok(BagComposeAvailability::new(
                            recipe.recipe_id,
                            recipe.material_id,
                            recipe.material_count,
                            recipe.gold,
                            equipment_config_id,
                            recipe.max_count,
                        ))
                    },
                )
                .transpose()?;
            Ok(BagItem::new(
                item.item_id,
                item.quantity,
                item.resolved_name.clone(),
                compose,
            ))
        })
        .collect::<Result<_, GameStateMappingError>>()?;
    Ok(BagInventory::new(items))
}

fn validate_bag_recipe(
    runtime: &super::super::runtime::ComposeRecipe,
    static_recipe: &crate::domain::EquipmentComposeRecipe,
) -> Result<(), GameStateMappingError> {
    let static_material = static_recipe.material();
    let matches = runtime.material_id == static_material.item_id()
        && runtime.material_count == static_material.quantity()
        && runtime.gold == static_recipe.gold()
        && runtime.equipment_id == Some(static_recipe.equipment_config_id().get());
    if !matches {
        return Err(GameStateMappingError::BagRecipeMismatch {
            recipe_id: runtime.recipe_id,
        });
    }
    Ok(())
}

fn lookup_config<'a>(
    configs: &BTreeMap<EquipmentConfigId, &'a EquipmentDefinition>,
    config_id: EquipmentConfigId,
    context: &str,
) -> Result<&'a EquipmentDefinition, GameStateMappingError> {
    configs
        .get(&config_id)
        .copied()
        .ok_or_else(|| GameStateMappingError::EquipmentConfigMissing {
            context: context.to_owned(),
            config_id: config_id.get(),
        })
}

fn runtime_enhance_level(context: &str, value: u32) -> Result<EnhanceLevel, GameStateMappingError> {
    u8::try_from(value).map(EnhanceLevel::new).map_err(|_| {
        GameStateMappingError::EnhanceLevelOutOfRange {
            context: context.to_owned(),
            value,
        }
    })
}

fn validate_enhance_level(
    context: &str,
    config_id: EquipmentConfigId,
    actual: EnhanceLevel,
    config: &EquipmentDefinition,
) -> Result<(), GameStateMappingError> {
    let expected = config.enhancement().level();
    if actual != expected {
        return Err(GameStateMappingError::EnhanceLevelMismatch {
            context: context.to_owned(),
            config_id: config_id.get(),
            actual: actual.get(),
            expected: expected.get(),
        });
    }
    Ok(())
}

#[derive(Serialize)]
struct GameStateDigestInput<'a> {
    read_scope: crate::domain::GameReadScope,
    schema_version: u32,
    module_sha256: &'a str,
    owned_state_schema_version: u32,
    ship_details_schema_version: u32,
    ship_catalog_schema_version: u32,
    equipment_catalog_schema_version: u32,
    raw_records_schema_version: u32,
    owned_state_content_sha256: &'a str,
    ship_roster_content_sha256: &'a str,
    ship_catalog_content_sha256: &'a str,
    equipment_catalog_content_sha256: &'a str,
    raw_records_content_sha256: &'a str,
}

/// 多次快照、装备目录和领域关联无法形成一致完整状态。
#[derive(Debug, Error)]
pub enum GameStateMappingError {
    /// 输入运行态违反已经冻结的协议契约。
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
    /// 舰船运行态和详情无法严格关联。
    #[error(transparent)]
    Ship(#[from] ShipMappingError),
    /// 舰船静态目录无法形成严格关系和完整原始记录。
    #[error(transparent)]
    ShipCatalog(#[from] ShipCatalogMappingError),
    /// 武器、技能或原始记录无法投影。
    #[error(transparent)]
    EquipmentDetails(#[from] EquipmentDetailMappingError),
    /// 正整数领域标识无法建立。
    #[error(transparent)]
    Model(#[from] LoadoutModelError),
    /// 稳定摘要输入无法编码。
    #[error("完整游戏状态摘要编码失败: {0}")]
    Encode(#[from] serde_json::Error),
    /// 舰船详情或装备目录来自其他模块版本。
    #[error("{component} 模块摘要不一致: expected={expected}, actual={actual}")]
    ModuleMismatch {
        component: &'static str,
        expected: String,
        actual: String,
    },
    /// 详情读取期间账号持有状态发生变化。
    #[error("完整状态读取期间持有状态发生变化: before={before_sha256}, after={after_sha256}")]
    OwnedStateChanged {
        before_sha256: String,
        after_sha256: String,
    },
    /// 规范化装备目录重复声明同一个配置 ID。
    #[error("装备目录重复包含 config_id={config_id}")]
    DuplicateCatalogConfig { config_id: u64 },
    /// 舰船槽位、仓库或背包引用了目录中不存在的配置。
    #[error("{context} 引用了不存在的 config_id={config_id}")]
    EquipmentConfigMissing { context: String, config_id: u64 },
    /// 运行态强化等级无法装入领域类型。
    #[error("{context} 的强化等级 {value} 超出领域模型允许范围")]
    EnhanceLevelOutOfRange { context: String, value: u32 },
    /// 运行态强化等级与具体装备配置不一致。
    #[error(
        "{context} 的 config_id={config_id} 强化等级不一致: actual={actual}, expected={expected}"
    )]
    EnhanceLevelMismatch {
        context: String,
        config_id: u64,
        actual: u8,
        expected: u8,
    },
    /// 仓库没有按具体配置聚合为唯一条目。
    #[error("仓库重复包含 config_id={config_id} 的聚合条目")]
    DuplicateWarehouseConfig { config_id: u64 },
    /// 装备配置引用的武器或技能没有对应详情。
    #[error("config_id={config_id} 缺少装备详情 {detail}")]
    EquipmentDetailMissing { config_id: u64, detail: String },
    /// 背包即时合成信息无法与所属物品或同 ID 静态装备配方关联。
    #[error("背包合成信息无法与物品和静态目录的 recipe_id={recipe_id} 关联")]
    BagRecipeMismatch { recipe_id: u64 },
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{GameStateMappingError, map_game_state_borrowed_with_scope};
    use crate::adapters::device::reading::equipment::{EquipmentRawRecords, EquipmentReadResult};
    use crate::adapters::device::reading::ship_catalog::{ShipCatalogReadResult, ShipCatalogTable};
    use crate::adapters::device::runtime::{
        EquipmentReferenceNameBatchResult, ShipCatalogRecord, ShipCatalogTableKey,
        SnapshotOwnedStateResult, SnapshotShipDetailsResult,
    };
    use crate::domain::{
        EnhanceLevel, EquipmentCatalog, EquipmentCatalogSource, EquipmentClassification,
        EquipmentCompatibility, EquipmentDefinition, EquipmentEnhancement, EquipmentFamily,
        EquipmentFamilyId, EquipmentIdentity, EquipmentItemQuantity, EquipmentResources,
        NamedEquipmentNation, NamedEquipmentType,
    };

    const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const CATALOG_SHA256: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn map_game_state(
        owned_before: &SnapshotOwnedStateResult,
        ship_details: &SnapshotShipDetailsResult,
        owned_after: &SnapshotOwnedStateResult,
        equipment: EquipmentReadResult,
        expected_module_sha256: &str,
    ) -> Result<crate::domain::GameState, GameStateMappingError> {
        let skill_effects = equipment.raw_records().skills().to_vec();
        let ship_catalog = test_ship_catalog(expected_module_sha256);
        map_game_state_borrowed_with_scope(
            owned_before,
            ship_details,
            owned_after,
            &equipment,
            &ship_catalog,
            &skill_effects,
            expected_module_sha256,
            crate::domain::GameReadScope::full(),
        )
    }

    fn test_ship_catalog(module_sha256: &str) -> ShipCatalogReadResult {
        let records_for = |table_key| match table_key {
            ShipCatalogTableKey::ShipDataGroup => vec![ShipCatalogRecord {
                id: 1,
                raw: json!({"group_type": 10117, "trans_skill": [], "trans_type": 0}),
            }],
            ShipCatalogTableKey::ShipDataTemplate => vec![ShipCatalogRecord {
                id: 101171,
                raw: json!({
                    "id": 101171,
                    "group_type": 10117,
                    "type": 1,
                    "max_level": 125,
                    "star_max": 6,
                    "equip_1": [10],
                    "equip_2": [5, 10],
                    "equip_3": [6, 21],
                    "equip_4": [10],
                    "equip_5": [10],
                    "strengthen_id": 0,
                    "buff_list": [],
                    "buff_list_display": [],
                    "hide_buff_list": []
                }),
            }],
            ShipCatalogTableKey::ShipDataStatistics => vec![ShipCatalogRecord {
                id: 101171,
                raw: json!({
                    "id": 101171,
                    "name": "拉菲",
                    "english_name": "USS Laffey",
                    "nationality": 1,
                    "armor_type": 1,
                    "rarity": 4
                }),
            }],
            _ => Vec::new(),
        };
        ShipCatalogReadResult::from_capture(
            module_sha256.to_owned(),
            "3".repeat(64),
            ShipCatalogTableKey::ALL
                .into_iter()
                .map(|table_key| ShipCatalogTable::from_capture(table_key, records_for(table_key)))
                .collect(),
        )
    }

    #[test]
    fn maps_complete_inputs_to_stable_game_state() {
        let owned: SnapshotOwnedStateResult = deserialize(owned_value());
        let details: SnapshotShipDetailsResult = deserialize(details_value());
        let first =
            map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256).unwrap();
        let second =
            map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.schema_version(), 7);
        assert_eq!(first.source().module_sha256(), MODULE_SHA256);
        assert_eq!(first.source().owned_state_schema_version(), 3);
        assert_eq!(first.source().ship_details_schema_version(), 4);
        assert_eq!(first.source().ship_catalog_schema_version(), 1);
        assert_eq!(first.source().equipment_catalog_schema_version(), 2);
        assert_eq!(first.source().raw_records_schema_version(), 3);
        assert_eq!(first.source().content_sha256().len(), 64);
        assert_eq!(first.ships().len(), 1);
        let ship = &first.ships().ships()[0];
        assert_eq!(ship.slots()[0].index().get(), 1);
        assert_eq!(ship.intimacy().stage_id(), 5);
        assert_eq!(ship.intimacy().stage_description(), "爱");
        assert_eq!(ship.fleet_memberships().len(), 1);
        assert_eq!(ship.fleet_memberships()[0].fleet_id(), 1);
        assert_eq!(ship.fleet_memberships()[0].display_name(), Some("第一舰队"));
        let equipped = ship.slots()[0].equipment().unwrap();
        assert_eq!(equipped.runtime_id(), 8_001);
        assert_eq!(equipped.config_id().get(), 1_000);
        assert_eq!(equipped.enhance_level().get(), 0);
        assert!(ship.slots()[1].equipment().is_none());
        let warehouse = first.equipment_inventory().warehouse();
        assert_eq!(warehouse.len(), 1);
        assert_eq!(warehouse[0].runtime_group_id(), 7_001);
        assert_eq!(warehouse[0].config_id().get(), 1_000);
        assert_eq!(warehouse[0].quantity(), 2);
        assert_eq!(first.bag().items()[0].item_id(), 17_001);
        assert_eq!(
            first.bag().items()[0]
                .compose()
                .unwrap()
                .equipment_config_id()
                .unwrap()
                .get(),
            1_000
        );
        assert_eq!(first.resources().gold(), 12_345);
        assert_eq!(first.resources().equipment_capacity(), 2);
        assert_eq!(first.raw_records().records().len(), 4);
    }

    #[test]
    fn rejects_owned_state_changed_around_ship_details() {
        let owned: SnapshotOwnedStateResult = deserialize(owned_value());
        let mut changed = owned_value();
        changed["player"]["gold"] = json!(12_344);
        let changed: SnapshotOwnedStateResult = deserialize(changed);
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(
            &owned,
            &details,
            &changed,
            equipment_result(),
            MODULE_SHA256,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::OwnedStateChanged { .. }
        ));
    }

    #[test]
    fn rejects_snapshot_above_protocol_item_limit() {
        let mut owned = owned_value();
        owned["warehouse"]["items"] = Value::Array(
            (1_u64..=2_001)
                .map(|equipment_id| {
                    json!({
                        "equipment_id": equipment_id,
                        "config_id": 1000,
                        "quantity": 1,
                        "enhance_level": 0
                    })
                })
                .collect(),
        );
        owned["warehouse"]["count"] = json!(2_001);
        owned["player"]["equipment_capacity"] = json!(2_001);
        let owned: SnapshotOwnedStateResult = deserialize(owned);
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256)
            .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::Protocol(ref source)
                if source.code == "warehouse_limit_exceeded"
        ));
    }

    #[test]
    fn rejects_ship_slot_config_missing_from_catalog() {
        let mut owned = owned_value();
        owned["dock"]["ships"][0]["slots"][0]["equipment"]["config_id"] = json!(1_001);
        let owned: SnapshotOwnedStateResult = deserialize(owned);
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256)
            .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::EquipmentConfigMissing {
                config_id: 1_001,
                ..
            }
        ));
    }

    #[test]
    fn rejects_runtime_enhance_level_that_disagrees_with_config() {
        let mut owned = owned_value();
        owned["warehouse"]["items"][0]["enhance_level"] = json!(1);
        let owned: SnapshotOwnedStateResult = deserialize(owned);
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256)
            .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::EnhanceLevelMismatch {
                config_id: 1_000,
                actual: 1,
                expected: 0,
                ..
            }
        ));
    }

    #[test]
    fn rejects_duplicate_warehouse_config_rows() {
        let mut owned = owned_value();
        owned["warehouse"]["items"] = json!([
            {"equipment_id": 7001, "config_id": 1000, "quantity": 1, "enhance_level": 0},
            {"equipment_id": 7002, "config_id": 1000, "quantity": 1, "enhance_level": 0}
        ]);
        owned["warehouse"]["count"] = json!(2);
        let owned: SnapshotOwnedStateResult = deserialize(owned);
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256)
            .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::DuplicateWarehouseConfig { config_id: 1_000 }
        ));
    }

    #[test]
    fn rejects_equipment_bag_recipe_missing_from_static_catalog() {
        let owned: SnapshotOwnedStateResult = deserialize(owned_value());
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(
            &owned,
            &details,
            &owned,
            equipment_result_with_recipe(false),
            MODULE_SHA256,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::BagRecipeMismatch { recipe_id: 17_001 }
        ));
    }

    #[test]
    fn rejects_bag_recipe_id_that_differs_from_item_id() {
        let mut owned = owned_value();
        owned["bag"]["items"][0]["compose_recipe"]["recipe_id"] = json!(17_002);
        let owned: SnapshotOwnedStateResult = deserialize(owned);
        let details: SnapshotShipDetailsResult = deserialize(details_value());

        let error = map_game_state(&owned, &details, &owned, equipment_result(), MODULE_SHA256)
            .unwrap_err();

        assert!(matches!(
            error,
            GameStateMappingError::BagRecipeMismatch { recipe_id: 17_002 }
        ));
    }

    fn equipment_result() -> EquipmentReadResult {
        equipment_result_with_recipe(true)
    }

    fn equipment_result_with_recipe(include_recipe: bool) -> EquipmentReadResult {
        let config_id = crate::domain::EquipmentConfigId::new(1_000).unwrap();
        let family_id = EquipmentFamilyId::new(1_000).unwrap();
        let empty_resources = || EquipmentResources::new(0, Vec::new());
        let config = EquipmentDefinition::new(
            EquipmentIdentity::new(
                config_id,
                family_id,
                "测试设备".to_owned(),
                "test-equipment".to_owned(),
            ),
            EquipmentClassification::new(
                NamedEquipmentType::new(10, "设备".to_owned()),
                NamedEquipmentNation::new(0, "其他".to_owned()),
                3,
                1,
                String::new(),
                0,
                0,
                true,
                false,
            ),
            EquipmentEnhancement::new(
                EnhanceLevel::new(0),
                None,
                None,
                None,
                Vec::new(),
                empty_resources(),
                empty_resources(),
                empty_resources(),
            ),
            Vec::new(),
            EquipmentCompatibility::new(Vec::new(), Vec::new(), Vec::new()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            String::new(),
            0,
            None,
            0,
            0,
        );
        let recipes = include_recipe
            .then(|| {
                crate::domain::EquipmentComposeRecipe::new(
                    17_001,
                    EquipmentItemQuantity::new(17_001, 5),
                    100,
                    config_id,
                )
            })
            .into_iter()
            .collect();
        let catalog = EquipmentCatalog::new(
            EquipmentCatalogSource::new(MODULE_SHA256.to_owned(), CATALOG_SHA256.to_owned()),
            vec![EquipmentFamily::new(family_id, vec![config])],
            recipes,
            1,
        );
        let references: EquipmentReferenceNameBatchResult = deserialize(json!({
            "schema_version": 1,
            "complete": true,
            "count": 0,
            "source": {"module_sha256": MODULE_SHA256},
            "equipment_types": [],
            "nations": [],
            "ship_types": [],
            "attributes": []
        }));
        let raw =
            EquipmentRawRecords::new(Vec::new(), Vec::new(), references, Vec::new(), Vec::new())
                .unwrap();
        EquipmentReadResult::new(catalog, raw)
    }

    fn deserialize<T: serde::de::DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).expect("测试数据应满足运行时 DTO 结构")
    }

    fn owned_value() -> Value {
        json!({
            "schema_version": 3,
            "complete": true,
            "dock": {
                "complete": true,
                "count": 1,
                "truncated": false,
                "ships": [{
                    "ship_id": 9001,
                    "config_id": 101174,
                    "level": 100,
                    "experience_in_level": 3000000,
                    "intimacy_raw": 10000,
                    "energy": 150,
                    "proficiency": 7,
                    "fleet_memberships": [{
                        "fleet_id": 1,
                        "display_name": "第一舰队",
                        "kind": "regular",
                        "team": "vanguard",
                        "position": 1
                    }],
                    "skills": [],
                    "slots": [
                        {"slot_index": 1, "equipment": {"equipment_id": 8001, "config_id": 1000, "enhance_level": 0}},
                        {"slot_index": 2, "equipment": null},
                        {"slot_index": 3, "equipment": null},
                        {"slot_index": 4, "equipment": null},
                        {"slot_index": 5, "equipment": null}
                    ]
                }],
                "read_errors": []
            },
            "warehouse": {
                "complete": true,
                "count": 1,
                "truncated": false,
                "items": [{
                    "equipment_id": 7001,
                    "config_id": 1000,
                    "quantity": 2,
                    "enhance_level": 0
                }],
                "read_errors": []
            },
            "bag": {
                "schema_version": 1,
                "complete": true,
                "count": 1,
                "truncated": false,
                "items": [{
                    "item_id": 17001,
                    "quantity": 30,
                    "kind": "bag",
                    "resolved_name": "测试设计图",
                    "compose_recipe": {
                        "recipe_id": 17001,
                        "material_id": 17001,
                        "material_count": 5,
                        "gold": 100,
                        "equipment_id": 1000,
                        "max_count": 6
                    }
                }],
                "read_errors": []
            },
            "player": {
                "gold": 12345,
                "equipment_capacity": 2,
                "equipment_limit": 300
            }
        })
    }

    fn details_value() -> Value {
        json!({
            "schema_version": 4,
            "complete": true,
            "count": 1,
            "truncated": false,
            "source": {"module_sha256": MODULE_SHA256},
            "ships": [{
                "ship_id": 9001,
                "config_id": 101174,
                "name": "测试舰船",
                "level": 100,
                "max_level": 125,
                "experience_in_level": 3000000,
                "total_experience": 4500000,
                "next_level_experience": 10000,
                "intimacy_raw": 10000,
                "intimacy_maximum": 200,
                "intimacy_stage_id": 5,
                "intimacy_stage_description": "爱",
                "proposed": false,
                "propose_time": 0,
                "create_time": 0,
                "combat_power": 4321,
                "locked": true,
                "oil_cost": {"start": 4, "end": 6, "total": 10},
                "classification": {
                    "group_id": 10117,
                    "ship_type_id": 1,
                    "ship_type_name": "驱逐舰",
                    "armor_type_id": 1,
                    "armor_type_name": "轻型装甲",
                    "nation_id": 1,
                    "nation_name": "白鹰",
                    "rarity": 4,
                    "star": 5,
                    "max_star": 6,
                    "skin_id": 101170
                },
                "base_attributes": attributes(100.0),
                "equipment_applied_attributes": attributes(125.0),
                "effective_attributes": attributes(132.0),
                "slot_rules": [
                    {"slot_index": 1, "allowed_equipment_type_ids": [10]},
                    {"slot_index": 2, "allowed_equipment_type_ids": [5, 10]},
                    {"slot_index": 3, "allowed_equipment_type_ids": [6, 21]},
                    {"slot_index": 4, "allowed_equipment_type_ids": [10]},
                    {"slot_index": 5, "allowed_equipment_type_ids": [10]}
                ],
                "skills": []
            }],
            "read_errors": []
        })
    }

    fn attributes(cannon: f64) -> Value {
        json!({
            "durability": 1000.0,
            "cannon": cannon,
            "torpedo": 80.0,
            "anti_aircraft": 70.0,
            "air": 0.0,
            "reload": 120.0,
            "hit": 90.0,
            "dodge": 60.0,
            "anti_sub": 50.0,
            "luck": 45.0,
            "speed": 16.0
        })
    }
}

#[cfg(test)]
pub(crate) mod golden_fixture;

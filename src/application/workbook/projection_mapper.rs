//! 将不可变完整游戏状态映射为与 XLSX 实现无关的稳定工作簿投影。

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::super::{
    WorkbookProjectionError, WorkbookProjectionSource, WorkbookProjectionV4,
    WorkbookProjectionValue,
};
use super::projection::WorkbookProjectionBuilder;
use crate::domain::{
    EquipmentComposeRecipe, EquipmentConfigId, EquipmentDefinition, EquipmentFamily,
    EquipmentFamilyId, EquipmentSkillVisibility, GameState, RawRecordKey, SkillTableKey,
    SkillValue,
};

// Excel 按 UTF-16 单元限制文本长度，保留余量后再由 `_原始数据` 重组正文。
const RAW_JSON_CHUNK_UTF16_UNITS: usize = 30_000;
const SHIP_SKILL_EVIDENCE_MISSING: &str = "未读取到当前等级的技能效果证据";
const SHIP_SKILL_EVIDENCE_INCOMPLETE: &str = "技能效果证据不完整，运行态未提供具体诊断";

type ProjectionValue = WorkbookProjectionValue;
type ProjectionValues = Vec<(String, ProjectionValue)>;

macro_rules! projection_values {
    ($($key:literal => $value:expr),* $(,)?) => {
        vec![$(($key.to_owned(), $value)),*]
    };
}

mod equipment;
mod equipment_choices;
mod raw;
mod recipes;
mod ships;

/// 把一次已经完整校验的游戏状态转换为注册表定义的稳定投影表。
///
/// 装备候选字典随业务状态投影；枚举字典和 schema 由布局写入器生成，
/// 检查、执行和计划行由对应应用用例提供。正式生成和执行终态按布局决定原始行。
#[cfg(test)]
pub(in crate::application) fn project_game_state_to_workbook(
    state: &GameState,
) -> Result<WorkbookProjectionV4, WorkbookProjectionError> {
    project_game_state(state, true)
}

/// 按布局决定是否物化原始数据行。省略的原始表不生成行；隐藏表仍然生成。
pub(in crate::application) fn project_game_state_for_layout(
    state: &GameState,
    layout: &crate::application::WorkbookLayout,
) -> Result<WorkbookProjectionV4, WorkbookProjectionError> {
    let include_raw_rows = layout.sheets().iter().any(|sheet| {
        sheet.stable_key() == "raw_data"
            && sheet.generation() != crate::application::LayoutGenerationMode::Omitted
    });
    project_game_state(state, include_raw_rows)
}

fn project_game_state(
    state: &GameState,
    include_raw_rows: bool,
) -> Result<WorkbookProjectionV4, WorkbookProjectionError> {
    let index = ProjectionIndex::new(state)?;
    let mut builder = WorkbookProjectionBuilder::new(projection_source(state))?;

    ships::project_loadout_plan(state, &index, &mut builder)?;
    equipment::project_equipment_inventory(state, &index, &mut builder)?;
    recipes::project_resource_recipes(state, &index, &mut builder)?;
    if include_raw_rows {
        raw::project_raw_data(state, &mut builder)?;
    }
    equipment_choices::project_equipment_choices(state, &index, &mut builder)?;

    builder.finish()
}

struct ProjectionIndex<'a> {
    families: BTreeMap<EquipmentFamilyId, &'a EquipmentFamily>,
    configs: BTreeMap<EquipmentConfigId, &'a EquipmentDefinition>,
    warehouse_counts: BTreeMap<EquipmentFamilyId, u64>,
    equipped_counts: BTreeMap<EquipmentFamilyId, u64>,
    owned_config_counts: BTreeMap<EquipmentConfigId, u64>,
    raw_config_hashes: BTreeMap<EquipmentConfigId, &'a str>,
    recipes_by_family: BTreeMap<EquipmentFamilyId, Vec<&'a EquipmentComposeRecipe>>,
    equipment_type_names: BTreeMap<u64, &'a str>,
    ship_type_names: BTreeMap<u64, &'a str>,
    nation_names: BTreeMap<u64, &'a str>,
    armor_type_names: BTreeMap<u64, &'a str>,
}

impl<'a> ProjectionIndex<'a> {
    fn new(state: &'a GameState) -> Result<Self, WorkbookProjectionError> {
        let mut families = BTreeMap::new();
        let mut configs = BTreeMap::new();
        let mut equipment_type_names = BTreeMap::new();
        let mut ship_type_names = BTreeMap::new();
        let mut nation_names = BTreeMap::new();
        for family in state.equipment_catalog().families() {
            families.insert(family.family_id(), family);
            for config in family.configs() {
                configs.insert(config.identity().config_id(), config);
                let equipment_type = config.classification().equipment_type();
                equipment_type_names
                    .entry(equipment_type.equipment_type_id())
                    .or_insert(equipment_type.name());
                let nation = config.classification().nation();
                nation_names
                    .entry(nation.nation_id())
                    .or_insert(nation.name());
                let compatibility = config.compatibility();
                for ship_type in compatibility
                    .main_ship_types()
                    .iter()
                    .chain(compatibility.sub_ship_types())
                    .chain(compatibility.forbidden_ship_types())
                {
                    ship_type_names
                        .entry(ship_type.ship_type_id())
                        .or_insert(ship_type.name());
                }
            }
        }

        let mut armor_type_names = BTreeMap::new();
        for ship in state.ships().ships() {
            let classification = ship.classification();
            ship_type_names
                .entry(classification.ship_type().id())
                .or_insert(classification.ship_type().name());
            nation_names
                .entry(classification.nation().id())
                .or_insert(classification.nation().name());
            armor_type_names
                .entry(classification.armor_type().id())
                .or_insert(classification.armor_type().name());
        }

        let mut warehouse_counts = BTreeMap::new();
        let mut owned_config_counts = BTreeMap::new();
        for stack in state.equipment_inventory().warehouse() {
            checked_add_count(
                &mut warehouse_counts,
                stack.family_id(),
                stack.quantity(),
                "equipment_catalog",
                format!("family:{}", stack.family_id().get()),
                "仓库装备数量",
            )?;
            checked_add_count(
                &mut owned_config_counts,
                stack.config_id(),
                stack.quantity(),
                "resource_recipes",
                format!("config:{}", stack.config_id().get()),
                "仓库配置数量",
            )?;
        }

        let mut equipped_counts = BTreeMap::new();
        for ship in state.ships().ships() {
            for slot in ship.slots() {
                let Some(equipment) = slot.equipment() else {
                    continue;
                };
                let config = configs.get(&equipment.config_id()).ok_or_else(|| {
                    missing_reference(
                        "equipment_catalog",
                        format!(
                            "ship:{}:{}",
                            ship.identity().instance_id().get(),
                            slot.index()
                        ),
                        "装备配置",
                        equipment.config_id().get().to_string(),
                    )
                })?;
                checked_add_count(
                    &mut equipped_counts,
                    config.identity().family_id(),
                    1,
                    "equipment_catalog",
                    format!("family:{}", config.identity().family_id().get()),
                    "舰上装备数量",
                )?;
                checked_add_count(
                    &mut owned_config_counts,
                    equipment.config_id(),
                    1,
                    "resource_recipes",
                    format!("config:{}", equipment.config_id().get()),
                    "舰上配置数量",
                )?;
            }
        }

        let raw_config_hashes = state
            .raw_records()
            .records()
            .iter()
            .filter_map(|record| match record.key() {
                RawRecordKey::EquipmentConfig(config_id) => {
                    Some((*config_id, record.content_sha256()))
                }
                _ => None,
            })
            .collect();

        let mut recipes_by_family: BTreeMap<EquipmentFamilyId, Vec<&EquipmentComposeRecipe>> =
            BTreeMap::new();
        for recipe in state.equipment_catalog().recipes() {
            let config = configs.get(&recipe.equipment_config_id()).ok_or_else(|| {
                missing_reference(
                    "resource_recipes",
                    format!("recipe:{}", recipe.recipe_id()),
                    "装备配置",
                    recipe.equipment_config_id().get().to_string(),
                )
            })?;
            recipes_by_family
                .entry(config.identity().family_id())
                .or_default()
                .push(recipe);
        }

        Ok(Self {
            families,
            configs,
            warehouse_counts,
            equipped_counts,
            owned_config_counts,
            raw_config_hashes,
            recipes_by_family,
            equipment_type_names,
            ship_type_names,
            nation_names,
            armor_type_names,
        })
    }

    fn config(
        &self,
        sheet_key: &str,
        object_ref: impl Into<String>,
        config_id: EquipmentConfigId,
    ) -> Result<&'a EquipmentDefinition, WorkbookProjectionError> {
        let object_ref = object_ref.into();
        self.configs.get(&config_id).copied().ok_or_else(|| {
            missing_reference(
                sheet_key,
                object_ref,
                "装备配置",
                config_id.get().to_string(),
            )
        })
    }

    fn family(
        &self,
        sheet_key: &str,
        object_ref: impl Into<String>,
        family_id: EquipmentFamilyId,
    ) -> Result<&'a EquipmentFamily, WorkbookProjectionError> {
        let object_ref = object_ref.into();
        self.families.get(&family_id).copied().ok_or_else(|| {
            missing_reference(sheet_key, object_ref, "装备族", family_id.get().to_string())
        })
    }
}

fn projection_source(state: &GameState) -> WorkbookProjectionSource {
    let source = state.source();
    WorkbookProjectionSource::new(
        state.schema_version(),
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
        source.content_sha256().to_owned(),
    )
    .with_read_scope(source.read_scope())
}

fn missing_reference(
    sheet_key: impl Into<String>,
    object_ref: impl Into<String>,
    target_type: &'static str,
    target_ref: impl Into<String>,
) -> WorkbookProjectionError {
    WorkbookProjectionError::MissingReference {
        sheet_key: sheet_key.into(),
        object_ref: object_ref.into(),
        target_type,
        target_ref: target_ref.into(),
    }
}

fn checked_add_count<K: Ord + Copy>(
    counts: &mut BTreeMap<K, u64>,
    key: K,
    value: u64,
    sheet_key: &'static str,
    object_ref: String,
    operation: &'static str,
) -> Result<(), WorkbookProjectionError> {
    let current = counts.entry(key).or_default();
    *current = current
        .checked_add(value)
        .ok_or(WorkbookProjectionError::ArithmeticOverflow {
            sheet_key,
            object_ref,
            operation,
        })?;
    Ok(())
}

fn blank() -> ProjectionValue {
    ProjectionValue::Blank
}

fn text(value: impl ToString) -> ProjectionValue {
    ProjectionValue::text(value.to_string())
}

fn optional_text(value: Option<impl ToString>) -> ProjectionValue {
    value.map(text).unwrap_or_else(blank)
}

fn integer(value: impl Into<i64>) -> ProjectionValue {
    ProjectionValue::Integer(value.into())
}

fn integer_u64(
    sheet_key: &str,
    object_ref: &str,
    field_key: &str,
    value: u64,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    i64::try_from(value)
        .map(ProjectionValue::Integer)
        .map_err(|_| WorkbookProjectionError::IntegerOverflow {
            sheet_key: sheet_key.to_owned(),
            object_ref: object_ref.to_owned(),
            field_key: field_key.to_owned(),
            value,
        })
}

fn integer_usize(
    sheet_key: &'static str,
    object_ref: &str,
    field_key: &str,
    value: usize,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    let value = u64::try_from(value).map_err(|_| WorkbookProjectionError::ArithmeticOverflow {
        sheet_key,
        object_ref: object_ref.to_owned(),
        operation: "usize 转换为工作簿整数",
    })?;
    integer_u64(sheet_key, object_ref, field_key, value)
}

fn optional_integer_u64(
    sheet_key: &str,
    object_ref: &str,
    field_key: &str,
    value: Option<u64>,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    value.map_or_else(
        || Ok(blank()),
        |value| integer_u64(sheet_key, object_ref, field_key, value),
    )
}

fn decimal(value: f64) -> ProjectionValue {
    ProjectionValue::Decimal(value)
}

fn boolean(value: bool) -> ProjectionValue {
    ProjectionValue::Boolean(value)
}

fn json_cell(
    sheet_key: &'static str,
    object_ref: &str,
    field_key: &'static str,
    value: &Value,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    serde_json::to_string(value)
        .map(ProjectionValue::Json)
        .map_err(|source| WorkbookProjectionError::JsonEncode {
            sheet_key,
            object_ref: object_ref.to_owned(),
            field_key,
            source,
        })
}

fn json_fragment(value: impl Into<String>) -> ProjectionValue {
    ProjectionValue::Json(value.into())
}

fn equipment_effect_summary(config: &EquipmentDefinition) -> String {
    let mut parts = Vec::new();
    if !config.attributes().is_empty() {
        parts.push(
            config
                .attributes()
                .iter()
                .map(|attribute| format!("{} {:+}", attribute.name(), attribute.value()))
                .collect::<Vec<_>>()
                .join("，"),
        );
    }
    if !config.weapon_ids().is_empty() {
        parts.push(format!("武器 {}", join_u64(config.weapon_ids())));
    }
    if !config.skill_references().is_empty() {
        parts.push(format!(
            "技能 {}",
            config
                .skill_references()
                .iter()
                .map(|skill| format!("{}:Lv{}", skill.skill_id(), skill.level()))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    parts.join("；")
}

struct ComposeAvailability {
    material_id: Option<u64>,
    material_required: Option<u64>,
    material_available: Option<u64>,
    gold_required: Option<u64>,
    by_material: Option<u64>,
    by_gold: Option<u64>,
    actual: Option<u64>,
    material_costs: ProjectionValue,
}

fn compose_availability(
    state: &GameState,
    object_ref: &str,
    recipe: Option<&EquipmentComposeRecipe>,
) -> Result<ComposeAvailability, WorkbookProjectionError> {
    let Some(recipe) = recipe else {
        return Ok(ComposeAvailability {
            material_id: None,
            material_required: None,
            material_available: None,
            gold_required: None,
            by_material: None,
            by_gold: None,
            actual: None,
            material_costs: json_cell(
                "equipment_catalog",
                object_ref,
                "compose_material_costs",
                &Value::Array(Vec::new()),
            )?,
        });
    };
    let material = recipe.material();
    let material_available = state
        .bag()
        .item(material.item_id())
        .map(|item| item.quantity())
        .unwrap_or(0);
    let by_material = (material.quantity() > 0).then(|| material_available / material.quantity());
    let by_gold = (recipe.gold() > 0).then(|| state.resources().gold() / recipe.gold());
    let client_max = state
        .bag()
        .item(recipe.recipe_id())
        .and_then(|item| item.compose())
        .filter(|compose| compose.recipe_id() == recipe.recipe_id())
        .and_then(|compose| compose.max_count());
    let actual = minimum_constraint(client_max, minimum_constraint(by_material, by_gold));
    let material_costs = json_cell(
        "equipment_catalog",
        object_ref,
        "compose_material_costs",
        &json!([{
            "item_id": material.item_id(),
            "quantity": material.quantity(),
        }]),
    )?;
    Ok(ComposeAvailability {
        material_id: Some(material.item_id()),
        material_required: Some(material.quantity()),
        material_available: Some(material_available),
        gold_required: Some(recipe.gold()),
        by_material,
        by_gold,
        actual,
        material_costs,
    })
}

fn minimum_constraint(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn family_compose_recipe<'a>(
    family: &EquipmentFamily,
    index: &'a ProjectionIndex<'_>,
) -> Result<Option<&'a EquipmentComposeRecipe>, WorkbookProjectionError> {
    let recipes = index
        .recipes_by_family
        .get(&family.family_id())
        .map(Vec::as_slice)
        .unwrap_or_default();
    match recipes {
        [] => Ok(None),
        [recipe] => Ok(Some(*recipe)),
        _ => Err(WorkbookProjectionError::AmbiguousComposeRecipe {
            family_id: family.family_id().get(),
            recipe_ids: recipes.iter().map(|recipe| recipe.recipe_id()).collect(),
        }),
    }
}

fn named_ship_types(types: &[crate::domain::NamedEquipmentShipType]) -> String {
    types
        .iter()
        .map(|ship_type| format!("[{}] {}", ship_type.ship_type_id(), ship_type.name()))
        .collect::<Vec<_>>()
        .join("，")
}

fn join_u64(values: &[u64]) -> String {
    values
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn skill_visibility_value(value: EquipmentSkillVisibility) -> &'static str {
    match value {
        EquipmentSkillVisibility::Visible => "visible",
        EquipmentSkillVisibility::Hidden => "hidden",
    }
}

fn skill_value_json(value: &SkillValue) -> Value {
    match value {
        SkillValue::Null => json!({"kind": "null"}),
        SkillValue::Bool(value) => json!({"kind": "bool", "value": value}),
        SkillValue::Number(value) => json!({"kind": "number", "value": value}),
        SkillValue::String(value) => json!({"kind": "string", "value": value}),
        SkillValue::List(values) => json!({
            "kind": "list",
            "items": values.iter().map(skill_value_json).collect::<Vec<_>>(),
        }),
        SkillValue::Object(fields) => json!({
            "kind": "object",
            "fields": fields.iter().map(|field| {
                json!({"name": field.name(), "value": skill_value_json(field.value())})
            }).collect::<Vec<_>>(),
        }),
        SkillValue::MixedTable(table) => json!({
            "kind": "mixed_table",
            "entries": table.entries().iter().map(|entry| {
                let key = match entry.key() {
                    SkillTableKey::Number(value) => json!({"kind": "number", "value": value}),
                    SkillTableKey::String(value) => json!({"kind": "string", "value": value}),
                };
                json!({"key": key, "value": skill_value_json(entry.value())})
            }).collect::<Vec<_>>(),
            "reason": table.reason(),
        }),
    }
}

#[cfg(test)]
mod tests;

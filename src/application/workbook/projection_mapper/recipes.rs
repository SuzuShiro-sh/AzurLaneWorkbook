//! 负责资源配方工作表的投影映射。

use super::{
    ProjectionIndex, WorkbookProjectionBuilder, WorkbookProjectionError, blank,
    compose_availability, missing_reference, optional_integer_u64, text,
};
use crate::domain::{
    EquipmentComposeRecipe, EquipmentConfigId, EquipmentDefinition, EquipmentFamily,
    EquipmentFamilyId, EquipmentResources, GameState,
};

pub(super) fn project_resource_recipes(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
) -> Result<(), WorkbookProjectionError> {
    for recipe in state.equipment_catalog().recipes() {
        project_compose_recipe(state, index, builder, recipe)?;
    }
    for family in state.equipment_catalog().families() {
        for config in family.configs() {
            project_enhance_recipe(state, index, builder, family, config)?;
            project_resource_yield(
                state,
                index,
                builder,
                family,
                config,
                "restore",
                config.enhancement().restore_yield(),
            )?;
            project_resource_yield(
                state,
                index,
                builder,
                family,
                config,
                "destroy",
                config.enhancement().destroy_yield(),
            )?;
        }
    }
    Ok(())
}

fn project_compose_recipe(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
    recipe: &EquipmentComposeRecipe,
) -> Result<(), WorkbookProjectionError> {
    let recipe_id = recipe.recipe_id().to_string();
    let source_ref = format!("equipment_recipe:{}", recipe.recipe_id());
    let config = index.config(
        "resource_recipes",
        &source_ref,
        recipe.equipment_config_id(),
    )?;
    let family_id = config.identity().family_id();
    let compose = compose_availability(state, &source_ref, Some(recipe))?;
    push_resource_recipe_row(
        builder,
        ResourceRecipeRow {
            object_ref: format!("compose:{}:output:equipment", recipe.recipe_id()),
            recipe_type: "compose",
            recipe_id: recipe_id.clone(),
            equipment_family_id: family_id,
            equipment_config_id: config.identity().config_id(),
            resource_type: "equipment",
            resource_id: config.identity().config_id().get().to_string(),
            resource_name: config.identity().name().to_owned(),
            source_ref: source_ref.clone(),
            result_quantity: Some(1),
            required_quantity: None,
            available_quantity: Some(owned_config_count(index, config.identity().config_id())),
            maximum_craftable: compose.actual,
        },
    )?;

    if recipe.gold() > 0 {
        push_resource_recipe_row(
            builder,
            ResourceRecipeRow {
                object_ref: format!("compose:{}:input:gold", recipe.recipe_id()),
                recipe_type: "compose",
                recipe_id: recipe_id.clone(),
                equipment_family_id: family_id,
                equipment_config_id: config.identity().config_id(),
                resource_type: "gold",
                resource_id: "gold".to_owned(),
                resource_name: "物资".to_owned(),
                source_ref: source_ref.clone(),
                result_quantity: None,
                required_quantity: Some(recipe.gold()),
                available_quantity: Some(state.resources().gold()),
                maximum_craftable: compose.by_gold,
            },
        )?;
    }

    let material = recipe.material();
    push_resource_recipe_row(
        builder,
        ResourceRecipeRow {
            object_ref: format!(
                "compose:{}:input:item:{}",
                recipe.recipe_id(),
                material.item_id()
            ),
            recipe_type: "compose",
            recipe_id,
            equipment_family_id: family_id,
            equipment_config_id: config.identity().config_id(),
            resource_type: "item",
            resource_id: material.item_id().to_string(),
            resource_name: item_name(state, material.item_id()),
            source_ref,
            result_quantity: None,
            required_quantity: Some(material.quantity()),
            available_quantity: compose.material_available,
            maximum_craftable: compose.by_material,
        },
    )
}

fn project_enhance_recipe(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
    family: &EquipmentFamily,
    config: &EquipmentDefinition,
) -> Result<(), WorkbookProjectionError> {
    let Some(next_config_id) = config.enhancement().next_config_id() else {
        return Ok(());
    };
    let source_ref = format!("equipment_config:{}", config.identity().config_id().get());
    let next = index.config("resource_recipes", &source_ref, next_config_id)?;
    if next.identity().family_id() != family.family_id() {
        return Err(missing_reference(
            "resource_recipes",
            source_ref,
            "同族后一强化配置",
            next_config_id.get().to_string(),
        ));
    }
    let recipe_id = format!(
        "enhance:{}:{}",
        config.identity().config_id().get(),
        next_config_id.get()
    );
    let owned = owned_config_count(index, config.identity().config_id());
    let maximum = enhance_maximum(state, config, owned);

    push_resource_recipe_row(
        builder,
        ResourceRecipeRow {
            object_ref: format!("{recipe_id}:input:equipment"),
            recipe_type: "enhance",
            recipe_id: recipe_id.clone(),
            equipment_family_id: family.family_id(),
            equipment_config_id: config.identity().config_id(),
            resource_type: "equipment",
            resource_id: config.identity().config_id().get().to_string(),
            resource_name: config.identity().name().to_owned(),
            source_ref: source_ref.clone(),
            result_quantity: None,
            required_quantity: Some(1),
            available_quantity: Some(owned),
            maximum_craftable: Some(owned),
        },
    )?;
    push_resource_recipe_row(
        builder,
        ResourceRecipeRow {
            object_ref: format!("{recipe_id}:output:equipment"),
            recipe_type: "enhance",
            recipe_id: recipe_id.clone(),
            equipment_family_id: family.family_id(),
            equipment_config_id: config.identity().config_id(),
            resource_type: "equipment",
            resource_id: next_config_id.get().to_string(),
            resource_name: next.identity().name().to_owned(),
            source_ref: source_ref.clone(),
            result_quantity: Some(1),
            required_quantity: None,
            available_quantity: Some(owned_config_count(index, next_config_id)),
            maximum_craftable: Some(maximum),
        },
    )?;

    let cost = config.enhancement().next_cost();
    if cost.gold() > 0 {
        push_resource_recipe_row(
            builder,
            ResourceRecipeRow {
                object_ref: format!("{recipe_id}:input:gold"),
                recipe_type: "enhance",
                recipe_id: recipe_id.clone(),
                equipment_family_id: family.family_id(),
                equipment_config_id: config.identity().config_id(),
                resource_type: "gold",
                resource_id: "gold".to_owned(),
                resource_name: "物资".to_owned(),
                source_ref: source_ref.clone(),
                result_quantity: None,
                required_quantity: Some(cost.gold()),
                available_quantity: Some(state.resources().gold()),
                maximum_craftable: Some(state.resources().gold() / cost.gold()),
            },
        )?;
    }
    for item in cost.items().iter().copied() {
        let available = item_quantity(state, item.item_id());
        push_resource_recipe_row(
            builder,
            ResourceRecipeRow {
                object_ref: format!("{recipe_id}:input:item:{}", item.item_id()),
                recipe_type: "enhance",
                recipe_id: recipe_id.clone(),
                equipment_family_id: family.family_id(),
                equipment_config_id: config.identity().config_id(),
                resource_type: "item",
                resource_id: item.item_id().to_string(),
                resource_name: item_name(state, item.item_id()),
                source_ref: source_ref.clone(),
                result_quantity: None,
                required_quantity: Some(item.quantity()),
                available_quantity: Some(available),
                maximum_craftable: Some(available / item.quantity()),
            },
        )?;
    }
    Ok(())
}

fn project_resource_yield(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
    family: &EquipmentFamily,
    config: &EquipmentDefinition,
    recipe_type: &'static str,
    yields: &EquipmentResources,
) -> Result<(), WorkbookProjectionError> {
    let recipe_id = format!("{recipe_type}:{}", config.identity().config_id().get());
    let source_ref = format!("equipment_config:{}", config.identity().config_id().get());
    let owned = owned_config_count(index, config.identity().config_id());
    push_resource_recipe_row(
        builder,
        ResourceRecipeRow {
            object_ref: format!("{recipe_id}:input:equipment"),
            recipe_type,
            recipe_id: recipe_id.clone(),
            equipment_family_id: family.family_id(),
            equipment_config_id: config.identity().config_id(),
            resource_type: "equipment",
            resource_id: config.identity().config_id().get().to_string(),
            resource_name: config.identity().name().to_owned(),
            source_ref: source_ref.clone(),
            result_quantity: None,
            required_quantity: Some(1),
            available_quantity: Some(owned),
            maximum_craftable: Some(owned),
        },
    )?;
    if yields.gold() > 0 {
        push_resource_recipe_row(
            builder,
            ResourceRecipeRow {
                object_ref: format!("{recipe_id}:output:gold"),
                recipe_type,
                recipe_id: recipe_id.clone(),
                equipment_family_id: family.family_id(),
                equipment_config_id: config.identity().config_id(),
                resource_type: "gold",
                resource_id: "gold".to_owned(),
                resource_name: "物资".to_owned(),
                source_ref: source_ref.clone(),
                result_quantity: Some(yields.gold()),
                required_quantity: None,
                available_quantity: Some(state.resources().gold()),
                maximum_craftable: Some(owned),
            },
        )?;
    }
    for item in yields.items().iter().copied() {
        push_resource_recipe_row(
            builder,
            ResourceRecipeRow {
                object_ref: format!("{recipe_id}:output:item:{}", item.item_id()),
                recipe_type,
                recipe_id: recipe_id.clone(),
                equipment_family_id: family.family_id(),
                equipment_config_id: config.identity().config_id(),
                resource_type: "item",
                resource_id: item.item_id().to_string(),
                resource_name: item_name(state, item.item_id()),
                source_ref: source_ref.clone(),
                result_quantity: Some(item.quantity()),
                required_quantity: None,
                available_quantity: Some(item_quantity(state, item.item_id())),
                maximum_craftable: Some(owned),
            },
        )?;
    }
    Ok(())
}

struct ResourceRecipeRow {
    object_ref: String,
    recipe_type: &'static str,
    recipe_id: String,
    equipment_family_id: EquipmentFamilyId,
    equipment_config_id: EquipmentConfigId,
    resource_type: &'static str,
    resource_id: String,
    resource_name: String,
    source_ref: String,
    result_quantity: Option<u64>,
    required_quantity: Option<u64>,
    available_quantity: Option<u64>,
    maximum_craftable: Option<u64>,
}

fn push_resource_recipe_row(
    builder: &mut WorkbookProjectionBuilder,
    row: ResourceRecipeRow,
) -> Result<(), WorkbookProjectionError> {
    let object_ref = row.object_ref;
    builder.push_row(
        "resource_recipes",
        &object_ref,
        projection_values![
            "recipe_type" => text(row.recipe_type),
            "recipe_id" => text(row.recipe_id),
            "equipment_family_id" => text(row.equipment_family_id),
            "equipment_config_id" => text(row.equipment_config_id),
            "resource_type" => text(row.resource_type),
            "resource_id" => text(row.resource_id),
            "resource_name" => text(row.resource_name),
            "source_ref" => text(row.source_ref),
            "result_quantity" => optional_integer_u64("resource_recipes", &object_ref, "result_quantity", row.result_quantity)?,
            "required_quantity" => optional_integer_u64("resource_recipes", &object_ref, "required_quantity", row.required_quantity)?,
            "available_quantity" => optional_integer_u64("resource_recipes", &object_ref, "available_quantity", row.available_quantity)?,
            "maximum_craftable" => optional_integer_u64("resource_recipes", &object_ref, "maximum_craftable", row.maximum_craftable)?,
            "planned_delta" => blank(),
        ],
    )
}

fn enhance_maximum(state: &GameState, config: &EquipmentDefinition, owned: u64) -> u64 {
    let cost = config.enhancement().next_cost();
    let mut maximum = owned;
    if cost.gold() > 0 {
        maximum = maximum.min(state.resources().gold() / cost.gold());
    }
    for item in cost.items() {
        maximum = maximum.min(item_quantity(state, item.item_id()) / item.quantity());
    }
    maximum
}

fn owned_config_count(index: &ProjectionIndex<'_>, config_id: EquipmentConfigId) -> u64 {
    index
        .owned_config_counts
        .get(&config_id)
        .copied()
        .unwrap_or(0)
}

fn item_quantity(state: &GameState, item_id: u64) -> u64 {
    state
        .bag()
        .item(item_id)
        .map(|item| item.quantity())
        .unwrap_or(0)
}

fn item_name(state: &GameState, item_id: u64) -> String {
    state
        .bag()
        .item(item_id)
        .map(|item| item.name().to_owned())
        .unwrap_or_else(|| format!("物品 {item_id}"))
}

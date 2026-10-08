//! 将装备配置、引用名称和静态配方分页映射为领域装备目录。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use thiserror::Error;

use super::super::runtime::{
    ComposeRecipePageResult, EquipmentComposeRecipe as RuntimeComposeRecipe,
    EquipmentConfigPageResult, EquipmentReferenceNameBatchPayload,
    EquipmentReferenceNameBatchResult, RuntimeEquipmentAttributeName, RuntimeEquipmentConfig,
    RuntimeEquipmentNationName, RuntimeEquipmentShipTypeName, RuntimeEquipmentTypeName,
    RuntimeProtocolError,
};
use crate::domain::{
    EnhanceLevel, EquipmentAttribute, EquipmentCatalog, EquipmentCatalogSource,
    EquipmentClassification, EquipmentCompatibility, EquipmentComposeRecipe, EquipmentConfigId,
    EquipmentDefinition, EquipmentEnhancement, EquipmentFamily, EquipmentFamilyId,
    EquipmentIdentity, EquipmentItemQuantity, EquipmentResources, EquipmentSkillReference,
    EquipmentSkillVisibility, NamedEquipmentNation, NamedEquipmentShipType, NamedEquipmentType,
};
use suzushiro_content_digest::sha256_sorted_json;

mod document;

pub(crate) use document::EquipmentCatalogDocument;

/// 把完整配置页、配方页和名称批次合成为不可变的核心装备目录。
///
/// 武器参数和技能效果保留为显式引用，由对应详情目录关联，不把原始 Lua JSON 混入领域模型。
pub fn map_equipment_catalog(
    config_pages: &[EquipmentConfigPageResult],
    config_page_size: u32,
    recipe_pages: &[ComposeRecipePageResult],
    recipe_page_size: u32,
    reference_names: &EquipmentReferenceNameBatchResult,
    expected_module_sha256: &str,
) -> Result<EquipmentCatalog, EquipmentMappingError> {
    let configs = collect_config_pages(config_pages, config_page_size, expected_module_sha256)?;
    let runtime_recipes =
        collect_recipe_pages(recipe_pages, recipe_page_size, expected_module_sha256)?;
    let requests = build_reference_request(&configs)?;
    reference_names.validate(&requests, expected_module_sha256)?;
    let names = ReferenceNames::from_result(reference_names)?;

    let mut families: BTreeMap<EquipmentFamilyId, Vec<EquipmentDefinition>> = BTreeMap::new();
    for config in &configs {
        if !config.complete || !config.read_errors.is_empty() {
            return Err(EquipmentMappingError::IncompleteConfig {
                config_id: config.config_id,
                errors: config.read_errors.clone(),
            });
        }
        let (family_id, definition) = map_config(config, &names)?;
        families.entry(family_id).or_default().push(definition);
    }

    let families: Vec<EquipmentFamily> = families
        .into_iter()
        .map(|(family_id, definitions)| build_family(family_id, definitions))
        .collect::<Result<_, _>>()?;
    let known_configs: BTreeSet<EquipmentConfigId> = families
        .iter()
        .flat_map(|family| {
            family
                .configs()
                .iter()
                .map(|config| config.identity().config_id())
        })
        .collect();
    let recipes: Vec<EquipmentComposeRecipe> = runtime_recipes
        .iter()
        .map(|recipe| map_recipe(recipe, &known_configs))
        .collect::<Result<_, _>>()?;

    let digest_input = EquipmentCatalogDocument::new(&families, &recipes);
    let source = EquipmentCatalogSource::new(
        expected_module_sha256.to_owned(),
        sha256_sorted_json(&digest_input).map_err(EquipmentMappingError::Encode)?,
    );
    Ok(EquipmentCatalog::new(
        source,
        families,
        recipes,
        configs.len(),
    ))
}

/// 从已经完整收集的配置页推导名称批次，供读取器与映射器共享同一字段口径。
pub(in crate::adapters::device) fn build_reference_request_from_pages(
    config_pages: &[EquipmentConfigPageResult],
    config_page_size: u32,
    expected_module_sha256: &str,
) -> Result<EquipmentReferenceNameBatchPayload, EquipmentMappingError> {
    let configs = collect_config_pages(config_pages, config_page_size, expected_module_sha256)?;
    build_reference_request(&configs)
}

fn collect_config_pages<'a>(
    pages: &'a [EquipmentConfigPageResult],
    page_size: u32,
    expected_module_sha256: &str,
) -> Result<Vec<&'a RuntimeEquipmentConfig>, EquipmentMappingError> {
    if pages.is_empty() {
        return Err(EquipmentMappingError::PagesMissing {
            kind: "装备配置"
        });
    }
    let mut expected_start = 0;
    let mut total_count = None;
    let mut configs = Vec::new();
    let mut previous_config_id = None;
    for page in pages {
        if total_count.is_some_and(|total| expected_start >= total) {
            return Err(EquipmentMappingError::UnexpectedPage {
                kind: "装备配置",
                start_index: page.start_index,
            });
        }
        page.validate(expected_start, page_size, expected_module_sha256)?;
        if page.start_index != expected_start {
            return Err(EquipmentMappingError::PageGap {
                kind: "装备配置",
                expected: expected_start,
                actual: page.start_index,
            });
        }
        if !page.complete || !page.read_errors.is_empty() {
            return Err(EquipmentMappingError::PageIncomplete {
                kind: "装备配置",
                start_index: page.start_index,
            });
        }
        match total_count {
            Some(expected) if expected != page.total_count => {
                return Err(EquipmentMappingError::TotalCountMismatch {
                    kind: "装备配置",
                    expected,
                    actual: page.total_count,
                });
            }
            None => total_count = Some(page.total_count),
            _ => {}
        }
        for config in &page.configs {
            if previous_config_id.is_some_and(|previous| previous >= config.config_id) {
                return Err(EquipmentMappingError::CatalogOrderInvalid {
                    kind: "装备配置",
                    previous: previous_config_id.unwrap_or_default(),
                    actual: config.config_id,
                });
            }
            previous_config_id = Some(config.config_id);
            configs.push(config);
        }
        expected_start = page.next_index.unwrap_or(page.total_count);
    }
    let total_count = total_count.ok_or(EquipmentMappingError::TotalCountMissing {
        kind: "装备配置",
    })?;
    if expected_start != total_count || configs.len() != total_count as usize {
        return Err(EquipmentMappingError::CatalogCoverage {
            kind: "装备配置",
            expected: total_count,
            actual: configs.len(),
        });
    }
    Ok(configs)
}

fn collect_recipe_pages<'a>(
    pages: &'a [ComposeRecipePageResult],
    page_size: u32,
    expected_module_sha256: &str,
) -> Result<Vec<&'a RuntimeComposeRecipe>, EquipmentMappingError> {
    if pages.is_empty() {
        return Err(EquipmentMappingError::PagesMissing {
            kind: "合成配方"
        });
    }
    let mut expected_start = 0;
    let mut total_count = None;
    let mut recipes = Vec::new();
    let mut previous_recipe_id = None;
    for page in pages {
        if total_count.is_some_and(|total| expected_start >= total) {
            return Err(EquipmentMappingError::UnexpectedPage {
                kind: "合成配方",
                start_index: page.start_index,
            });
        }
        page.validate(expected_start, page_size, expected_module_sha256)?;
        if page.start_index != expected_start {
            return Err(EquipmentMappingError::PageGap {
                kind: "合成配方",
                expected: expected_start,
                actual: page.start_index,
            });
        }
        if !page.complete || !page.read_errors.is_empty() {
            return Err(EquipmentMappingError::PageIncomplete {
                kind: "合成配方",
                start_index: page.start_index,
            });
        }
        match total_count {
            Some(expected) if expected != page.total_count => {
                return Err(EquipmentMappingError::TotalCountMismatch {
                    kind: "合成配方",
                    expected,
                    actual: page.total_count,
                });
            }
            None => total_count = Some(page.total_count),
            _ => {}
        }
        for recipe in &page.recipes {
            if previous_recipe_id.is_some_and(|previous| previous >= recipe.recipe_id) {
                return Err(EquipmentMappingError::CatalogOrderInvalid {
                    kind: "合成配方",
                    previous: previous_recipe_id.unwrap_or_default(),
                    actual: recipe.recipe_id,
                });
            }
            previous_recipe_id = Some(recipe.recipe_id);
            recipes.push(recipe);
        }
        expected_start = page.next_index.unwrap_or(page.total_count);
    }
    let total_count = total_count.ok_or(EquipmentMappingError::TotalCountMissing {
        kind: "合成配方",
    })?;
    if expected_start != total_count || recipes.len() != total_count as usize {
        return Err(EquipmentMappingError::CatalogCoverage {
            kind: "合成配方",
            expected: total_count,
            actual: recipes.len(),
        });
    }
    Ok(recipes)
}

fn build_reference_request(
    configs: &[&RuntimeEquipmentConfig],
) -> Result<EquipmentReferenceNameBatchPayload, EquipmentMappingError> {
    let mut equipment_type_ids = BTreeSet::new();
    let mut nation_ids = BTreeSet::new();
    let mut ship_type_ids = BTreeSet::new();
    let mut attribute_keys = BTreeSet::new();
    for config in configs {
        let raw = object(config, "raw_config")?;
        equipment_type_ids.insert(required_u64(raw, "type", config.config_id)?);
        nation_ids.insert(required_nonnegative_u64(
            raw,
            "nationality",
            config.config_id,
        )?);
        for field in ["part_main", "part_sub", "ship_type_forbidden"] {
            for id in array_u64(raw, field, config.config_id, false)? {
                ship_type_ids.insert(id);
            }
        }
        for attribute in attributes(config)? {
            attribute_keys.insert(attribute.0);
        }
    }
    EquipmentReferenceNameBatchPayload::new(
        &equipment_type_ids.into_iter().collect::<Vec<_>>(),
        &nation_ids.into_iter().collect::<Vec<_>>(),
        &ship_type_ids.into_iter().collect::<Vec<_>>(),
        &attribute_keys.into_iter().collect::<Vec<_>>(),
    )
    .map_err(EquipmentMappingError::Protocol)
}

struct ReferenceNames<'a> {
    equipment_types: BTreeMap<u64, &'a str>,
    nations: BTreeMap<u64, &'a str>,
    ship_types: BTreeMap<u64, &'a str>,
    attributes: BTreeMap<String, &'a str>,
}

fn equipment_type_names(
    records: &[RuntimeEquipmentTypeName],
) -> Result<BTreeMap<u64, &str>, EquipmentMappingError> {
    records
        .iter()
        .map(|record| match (&record.name, &record.error) {
            (Some(name), None) => Ok((record.equipment_type_id, name.as_str())),
            (None, Some(error)) => Err(EquipmentMappingError::MissingReferenceName {
                namespace: "装备类型",
                key: record.equipment_type_id.to_string(),
                error: error.clone(),
            }),
            _ => Err(EquipmentMappingError::ReferenceNameState {
                namespace: "装备类型",
                key: record.equipment_type_id.to_string(),
            }),
        })
        .collect()
}

fn nation_names(
    records: &[RuntimeEquipmentNationName],
) -> Result<BTreeMap<u64, &str>, EquipmentMappingError> {
    records
        .iter()
        .map(|record| match (&record.name, &record.error) {
            (Some(name), None) => Ok((record.nation_id, name.as_str())),
            (None, Some(error)) => Err(EquipmentMappingError::MissingReferenceName {
                namespace: "阵营",
                key: record.nation_id.to_string(),
                error: error.clone(),
            }),
            _ => Err(EquipmentMappingError::ReferenceNameState {
                namespace: "阵营",
                key: record.nation_id.to_string(),
            }),
        })
        .collect()
}

fn ship_type_names(
    records: &[RuntimeEquipmentShipTypeName],
) -> Result<BTreeMap<u64, &str>, EquipmentMappingError> {
    records
        .iter()
        .map(|record| match (&record.name, &record.error) {
            (Some(name), None) => Ok((record.ship_type_id, name.as_str())),
            (None, Some(error)) => Err(EquipmentMappingError::MissingReferenceName {
                namespace: "舰种",
                key: record.ship_type_id.to_string(),
                error: error.clone(),
            }),
            _ => Err(EquipmentMappingError::ReferenceNameState {
                namespace: "舰种",
                key: record.ship_type_id.to_string(),
            }),
        })
        .collect()
}

fn attribute_names(
    records: &[RuntimeEquipmentAttributeName],
) -> Result<BTreeMap<String, &str>, EquipmentMappingError> {
    records
        .iter()
        .map(|record| match (&record.name, &record.error) {
            (Some(name), None) => Ok((record.attribute_key.clone(), name.as_str())),
            (None, Some(error)) => Err(EquipmentMappingError::MissingReferenceName {
                namespace: "属性",
                key: record.attribute_key.clone(),
                error: error.clone(),
            }),
            _ => Err(EquipmentMappingError::ReferenceNameState {
                namespace: "属性",
                key: record.attribute_key.clone(),
            }),
        })
        .collect()
}

impl<'a> ReferenceNames<'a> {
    fn from_result(
        result: &'a EquipmentReferenceNameBatchResult,
    ) -> Result<Self, EquipmentMappingError> {
        Ok(Self {
            equipment_types: equipment_type_names(&result.equipment_types)?,
            nations: nation_names(&result.nations)?,
            ship_types: ship_type_names(&result.ship_types)?,
            attributes: attribute_names(&result.attributes)?,
        })
    }
}

/// 客户端配置等级从 1 起算，展示和操作使用从 0 起算的强化等级。
pub(in crate::adapters::device) fn equipment_enhance_level(
    config: &RuntimeEquipmentConfig,
) -> Result<u8, EquipmentMappingError> {
    let level = required_u32(object(config, "raw_config")?, "level", config.config_id)?;
    u8::try_from(
        level
            .checked_sub(1)
            .ok_or_else(|| invalid(config.config_id, "level", "强化等级必须从 1 开始"))?,
    )
    .map_err(|_| invalid(config.config_id, "level", "强化等级超出领域模型允许的范围"))
}

fn map_config(
    config: &RuntimeEquipmentConfig,
    names: &ReferenceNames<'_>,
) -> Result<(EquipmentFamilyId, EquipmentDefinition), EquipmentMappingError> {
    let raw = object(config, "raw_config")?;
    let config_id = EquipmentConfigId::new(config.config_id)?;
    let root_id = EquipmentConfigId::new(
        config
            .root_config_id
            .ok_or_else(|| invalid(config.config_id, "root_config_id", "缺少根配置 ID"))?,
    )?;
    // group 是可由不同装备共享的客户端元数据，强化族身份以 GetRootEquipment 为准。
    required_u64(raw, "group", config.config_id)?;
    let family_id = EquipmentFamilyId::new(root_id.get())?;
    let name = required_text(raw, "name", config.config_id)?;
    let icon_key = required_text(raw, "icon", config.config_id)?;
    let equipment_type_id = required_u64(raw, "type", config.config_id)?;
    let nation_id = required_nonnegative_u64(raw, "nationality", config.config_id)?;
    let equipment_type = NamedEquipmentType::new(
        equipment_type_id,
        lookup_numeric(
            &names.equipment_types,
            "装备类型",
            equipment_type_id,
            config.config_id,
        )?,
    );
    let nation = NamedEquipmentNation::new(
        nation_id,
        lookup_numeric(&names.nations, "阵营", nation_id, config.config_id)?,
    );
    let classification = EquipmentClassification::new(
        equipment_type,
        nation,
        required_u32(raw, "rarity", config.config_id)?,
        required_nonnegative_u32(raw, "tech", config.config_id)?,
        optional_text(raw, "speciality", config.config_id)?.unwrap_or_default(),
        required_u32(raw, "ammo", config.config_id)?,
        required_nonnegative_u32(raw, "torpedo_ammo", config.config_id)?,
        config
            .is_device
            .ok_or_else(|| invalid(config.config_id, "is_device", "缺少客户端设备分类结果"))?,
        config
            .is_aircraft
            .ok_or_else(|| invalid(config.config_id, "is_aircraft", "缺少客户端舰载机分类结果"))?,
    );
    let enhance_level = equipment_enhance_level(config)?;
    let base_config_id = optional_config_id(raw, "base", config.config_id)?;
    let valid_base = if config_id == root_id {
        base_config_id.is_none()
    } else {
        base_config_id == Some(root_id)
    };
    if !valid_base {
        return Err(EquipmentMappingError::RootConfigMismatch {
            config_id: config.config_id,
            field: "base",
            expected: root_id.get(),
            actual: base_config_id.map(EquipmentConfigId::get),
        });
    }
    let enhancement = EquipmentEnhancement::new(
        EnhanceLevel::new(enhance_level),
        base_config_id,
        optional_config_id(raw, "prev", config.config_id)?,
        optional_config_id(raw, "next", config.config_id)?,
        array_u64(raw, "upgrade_formula_id", config.config_id, false)?,
        resources(raw, "trans_use_gold", "trans_use_item", config.config_id)?,
        resources(raw, "restore_gold", "restore_item", config.config_id)?,
        resources(raw, "destory_gold", "destory_item", config.config_id)?,
    );
    let mut attributes = attributes(config)?
        .into_iter()
        .map(|(key, value, auxiliary_boost)| {
            let name = names.attributes.get(&key).ok_or_else(|| {
                EquipmentMappingError::MissingReferenceName {
                    namespace: "属性",
                    key: key.clone(),
                    error: "批次响应未返回该属性名称".to_owned(),
                }
            })?;
            Ok(EquipmentAttribute::new(
                key,
                (*name).to_owned(),
                value,
                auxiliary_boost,
            ))
        })
        .collect::<Result<Vec<_>, EquipmentMappingError>>()?;
    attributes.sort_by(|left, right| left.key().cmp(right.key()));

    let compatibility = EquipmentCompatibility::new(
        named_ship_types(raw, "part_main", names, config.config_id)?,
        named_ship_types(raw, "part_sub", names, config.config_id)?,
        named_ship_types(raw, "ship_type_forbidden", names, config.config_id)?,
    );
    let mut skill_references = parse_skill_references(
        raw,
        "skill_id",
        EquipmentSkillVisibility::Visible,
        config.config_id,
    )?;
    skill_references.extend(parse_skill_references(
        raw,
        "hidden_skill_id",
        EquipmentSkillVisibility::Hidden,
        config.config_id,
    )?);
    skill_references.sort_unstable();

    let labels = string_array(raw, "label", config.config_id, false)?;
    let definition = EquipmentDefinition::new(
        EquipmentIdentity::new(config_id, family_id, name, icon_key),
        classification,
        enhancement,
        attributes,
        compatibility,
        config.weapon_ids.clone(),
        skill_references,
        labels,
        optional_text(raw, "descrip", config.config_id)?.unwrap_or_default(),
        config
            .gear_score
            .ok_or_else(|| invalid(config.config_id, "gear_score", "缺少装备评分"))?,
        config.anti_siren_power,
        required_nonnegative_u32(raw, "important", config.config_id)?,
        required_nonnegative_u64(raw, "equip_limit", config.config_id)?,
    );
    Ok((family_id, definition))
}

fn build_family(
    family_id: EquipmentFamilyId,
    mut configs: Vec<EquipmentDefinition>,
) -> Result<EquipmentFamily, EquipmentMappingError> {
    configs.sort_by_key(|config| (config.enhancement().level(), config.identity().config_id()));
    if configs
        .first()
        .is_none_or(|config| config.identity().config_id().get() != family_id.get())
    {
        return Err(EquipmentMappingError::FamilyChainInvalid {
            family_id: family_id.get(),
            message: "强化链必须包含与装备族 ID 相同的 +0 根配置".to_owned(),
        });
    }
    for (index, config) in configs.iter().enumerate() {
        if config.identity().family_id() != family_id
            || usize::from(config.enhancement().level().get()) != index
        {
            return Err(EquipmentMappingError::FamilyChainInvalid {
                family_id: family_id.get(),
                message: "强化等级必须从 +0 开始连续排列".to_owned(),
            });
        }
        let expected_previous = index
            .checked_sub(1)
            .and_then(|previous| configs.get(previous))
            .map(|previous| previous.identity().config_id());
        if config.enhancement().previous_config_id() != expected_previous {
            return Err(EquipmentMappingError::FamilyChainInvalid {
                family_id: family_id.get(),
                message: format!(
                    "config_id={} 的 prev 与强化链顺序不一致",
                    config.identity().config_id().get()
                ),
            });
        }
        let expected_next = configs
            .get(index + 1)
            .map(|next| next.identity().config_id());
        if config.enhancement().next_config_id() != expected_next {
            return Err(EquipmentMappingError::FamilyChainInvalid {
                family_id: family_id.get(),
                message: format!(
                    "config_id={} 的 next 与强化链顺序不一致",
                    config.identity().config_id().get()
                ),
            });
        }
    }
    Ok(EquipmentFamily::new(family_id, configs))
}

fn map_recipe(
    recipe: &RuntimeComposeRecipe,
    known_configs: &BTreeSet<EquipmentConfigId>,
) -> Result<EquipmentComposeRecipe, EquipmentMappingError> {
    let equipment_config_id = EquipmentConfigId::new(recipe.equipment_id)?;
    if !known_configs.contains(&equipment_config_id) {
        return Err(EquipmentMappingError::RecipeTargetMissing {
            recipe_id: recipe.recipe_id,
            equipment_id: recipe.equipment_id,
        });
    }
    Ok(EquipmentComposeRecipe::new(
        recipe.recipe_id,
        EquipmentItemQuantity::new(recipe.material_id, recipe.material_count),
        recipe.gold,
        equipment_config_id,
    ))
}

fn object<'a>(
    config: &'a RuntimeEquipmentConfig,
    field: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, EquipmentMappingError> {
    config
        .raw_config
        .as_object()
        .ok_or_else(|| invalid(config.config_id, field, "必须是对象"))
}

fn required_u64(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<u64, EquipmentMappingError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(config_id, field, "必须是正整数"))
}

fn required_u32(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<u32, EquipmentMappingError> {
    u32::try_from(required_u64(object, field, config_id)?)
        .map_err(|_| invalid(config_id, field, "数值超出 32 位无符号整数范围"))
}

fn required_nonnegative_u64(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<u64, EquipmentMappingError> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(config_id, field, "必须是非负整数"))
}

fn required_nonnegative_u32(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<u32, EquipmentMappingError> {
    u32::try_from(required_nonnegative_u64(object, field, config_id)?)
        .map_err(|_| invalid(config_id, field, "数值超出 32 位无符号整数范围"))
}

fn optional_u64(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<Option<u64>, EquipmentMappingError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|number| (number != 0).then_some(number))
            .ok_or_else(|| invalid(config_id, field, "必须是 null 或非负整数")),
    }
}

fn optional_config_id(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<Option<EquipmentConfigId>, EquipmentMappingError> {
    optional_u64(object, field, config_id)?
        .map(EquipmentConfigId::new)
        .transpose()
        .map_err(EquipmentMappingError::from)
}

fn required_text(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<String, EquipmentMappingError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid(config_id, field, "必须是非空文本"))
}

fn optional_text(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
) -> Result<Option<String>, EquipmentMappingError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid(config_id, field, "必须是文本或 null"))
            .map(Some),
    }
}

fn string_array(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
    required: bool,
) -> Result<Vec<String>, EquipmentMappingError> {
    let Some(value) = object.get(field) else {
        if required {
            return Err(invalid(config_id, field, "必须是字符串数组"));
        }
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(invalid(config_id, field, "必须是字符串数组"));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| invalid(config_id, field, "数组项必须是非空文本"))
        })
        .collect()
}

fn array_u64(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    config_id: u64,
    required: bool,
) -> Result<Vec<u64>, EquipmentMappingError> {
    let Some(value) = object.get(field) else {
        if required {
            return Err(invalid(config_id, field, "必须是正整数数组"));
        }
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(invalid(config_id, field, "必须是正整数数组"));
    };
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let number = value
            .as_u64()
            .filter(|number| *number > 0)
            .ok_or_else(|| invalid(config_id, field, "数组项必须是正整数"))?;
        output.push(number);
    }
    ensure_strictly_increasing(&output, config_id, field)?;
    Ok(output)
}

fn resources(
    object: &serde_json::Map<String, Value>,
    gold_field: &'static str,
    item_field: &'static str,
    config_id: u64,
) -> Result<EquipmentResources, EquipmentMappingError> {
    let gold = object
        .get(gold_field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(config_id, gold_field, "必须是非负整数"))?;
    let Some(value) = object.get(item_field) else {
        return Err(invalid(config_id, item_field, "必须是资源二维数组"));
    };
    let values = value
        .as_array()
        .ok_or_else(|| invalid(config_id, item_field, "必须是资源二维数组"))?;
    let mut items = Vec::with_capacity(values.len());
    for row in values {
        let pair = row
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| invalid(config_id, item_field, "每项必须是 [item_id, quantity]"))?;
        let item_id = pair[0]
            .as_u64()
            .filter(|number| *number > 0)
            .ok_or_else(|| invalid(config_id, item_field, "item_id 必须是正整数"))?;
        let quantity = pair[1]
            .as_u64()
            .filter(|number| *number > 0)
            .ok_or_else(|| invalid(config_id, item_field, "quantity 必须是正整数"))?;
        items.push(EquipmentItemQuantity::new(item_id, quantity));
    }
    items.sort_unstable_by_key(|item| item.item_id());
    for pair in items.windows(2) {
        if pair[0].item_id() == pair[1].item_id() {
            return Err(invalid(config_id, item_field, "同一资源 ID 不得重复"));
        }
    }
    Ok(EquipmentResources::new(gold, items))
}

fn attributes(
    config: &RuntimeEquipmentConfig,
) -> Result<Vec<(String, f64, bool)>, EquipmentMappingError> {
    let values = config
        .attributes
        .as_array()
        .ok_or_else(|| invalid(config.config_id, "attributes", "必须是数组"))?;
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        // Equipment:GetAttributes() 固定返回三个槽位，未配置的槽位以 false 占位。
        if matches!(value, Value::Bool(false)) {
            continue;
        }
        let row = value
            .as_object()
            .ok_or_else(|| invalid(config.config_id, "attributes", "每项必须是对象或 false"))?;
        let key = row
            .get("key")
            .or_else(|| row.get("type"))
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| invalid(config.config_id, "attributes.key", "必须是非空文本"))?
            .to_owned();
        let number = row
            .get("value")
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite())
            .ok_or_else(|| invalid(config.config_id, "attributes.value", "必须是有限数值"))?;
        let auxiliary_boost = row
            .get("aux_boost")
            .or_else(|| row.get("auxBoost"))
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid(config.config_id, "attributes.aux_boost", "必须是布尔值"))?;
        output.push((key, number, auxiliary_boost));
    }
    output.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    for pair in output.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(invalid(config.config_id, "attributes", "属性键不得重复"));
        }
    }
    Ok(output)
}

pub(in crate::adapters::device) fn parse_skill_references(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    visibility: EquipmentSkillVisibility,
    config_id: u64,
) -> Result<Vec<EquipmentSkillReference>, EquipmentMappingError> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| invalid(config_id, field, "必须是技能引用数组"))?;
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let (skill_id, level) = if let Some(skill_id) = value.as_u64() {
            (skill_id, 1)
        } else {
            let pair = value
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| invalid(config_id, field, "技能引用必须是 ID 或 [ID, level]"))?;
            (
                pair[0]
                    .as_u64()
                    .ok_or_else(|| invalid(config_id, field, "技能 ID 必须是正整数"))?,
                u32::try_from(
                    pair[1]
                        .as_u64()
                        .ok_or_else(|| invalid(config_id, field, "技能等级必须是正整数"))?,
                )
                .map_err(|_| invalid(config_id, field, "技能等级超出 32 位范围"))?,
            )
        };
        if skill_id == 0 || level == 0 {
            return Err(invalid(config_id, field, "技能 ID 和等级必须为正整数"));
        }
        output.push(EquipmentSkillReference::new(skill_id, level, visibility));
    }
    Ok(output)
}

fn named_ship_types(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
    names: &ReferenceNames<'_>,
    config_id: u64,
) -> Result<Vec<NamedEquipmentShipType>, EquipmentMappingError> {
    array_u64(object, field, config_id, false)?
        .into_iter()
        .map(|id| {
            let name = lookup_numeric(&names.ship_types, "舰种", id, config_id)?;
            Ok(NamedEquipmentShipType::new(id, name))
        })
        .collect()
}

fn ensure_strictly_increasing(
    values: &[u64],
    config_id: u64,
    field: &'static str,
) -> Result<(), EquipmentMappingError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid(config_id, field, "数组必须严格升序且不重复"));
    }
    Ok(())
}

fn lookup_numeric(
    names: &BTreeMap<u64, &str>,
    namespace: &'static str,
    id: u64,
    config_id: u64,
) -> Result<String, EquipmentMappingError> {
    names
        .get(&id)
        .map(|name| (*name).to_owned())
        .ok_or_else(|| EquipmentMappingError::MissingReferenceName {
            namespace,
            key: format!("{id} (config_id={config_id})"),
            error: "批次响应未返回该名称".to_owned(),
        })
}

fn invalid(config_id: u64, field: &'static str, message: &str) -> EquipmentMappingError {
    EquipmentMappingError::InvalidConfigField {
        config_id,
        field,
        message: message.to_owned(),
    }
}

/// 装备静态配置到领域模型的稳定失败分类。
#[derive(Debug, Error)]
pub enum EquipmentMappingError {
    /// 任一运行态页违反了已经冻结的 RPC 契约。
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
    /// 领域值对象无法接收协议标识。
    #[error(transparent)]
    Model(#[from] crate::domain::LoadoutModelError),
    /// 配置或配方页完全缺失。
    #[error("{kind}页不能为空")]
    PagesMissing { kind: &'static str },
    /// 相邻页的零基游标不连续。
    #[error("{kind}页游标断开: 期望 {expected}，实际 {actual}")]
    PageGap {
        kind: &'static str,
        expected: u32,
        actual: u32,
    },
    /// 已经到达目录末尾后仍然返回了额外页面。
    #[error("{kind}目录已结束后仍有起点为 {start_index} 的额外页面")]
    UnexpectedPage {
        kind: &'static str,
        start_index: u32,
    },
    /// 页面集合没有提供可用的目录总数。
    #[error("{kind}页缺少目录总数")]
    TotalCountMissing { kind: &'static str },
    /// 同一目录的条目标识跨页重复或倒序。
    #[error("{kind}条目标识未保持严格升序: 前项 {previous}，当前 {actual}")]
    CatalogOrderInvalid {
        kind: &'static str,
        previous: u64,
        actual: u64,
    },
    /// 同一目录的 total_count 在页之间发生变化。
    #[error("{kind}页 total_count 不一致: 期望 {expected}，实际 {actual}")]
    TotalCountMismatch {
        kind: &'static str,
        expected: u32,
        actual: u32,
    },
    /// 目录没有覆盖所有位置或包含页级诊断。
    #[error("{kind}目录未完整覆盖: 期望 {expected} 项，实际 {actual} 项")]
    CatalogCoverage {
        kind: &'static str,
        expected: u32,
        actual: usize,
    },
    /// 页声明不完整时不允许生成领域目录。
    #[error("{kind}第 {start_index} 项开始的页面不完整")]
    PageIncomplete {
        kind: &'static str,
        start_index: u32,
    },
    /// 单个配置含有读取诊断。
    #[error("config_id={config_id} 不完整: {errors:?}")]
    IncompleteConfig { config_id: u64, errors: Vec<String> },
    /// 配置字段无法按领域类型解释。
    #[error("config_id={config_id} 的 {field} 无效: {message}")]
    InvalidConfigField {
        config_id: u64,
        field: &'static str,
        message: String,
    },
    /// 客户端声明的装备族或基础配置与根配置方法结果不一致。
    #[error("config_id={config_id} 的 {field} 与根配置不一致: 期望 {expected}，实际 {actual:?}")]
    RootConfigMismatch {
        config_id: u64,
        field: &'static str,
        expected: u64,
        actual: Option<u64>,
    },
    /// 名称批次中某个 ID 返回了读取错误。
    #[error("{namespace} {key} 没有可用名称: {error}")]
    MissingReferenceName {
        namespace: &'static str,
        key: String,
        error: String,
    },
    /// 名称记录同时缺少或同时包含 name/error。
    #[error("{namespace} {key} 的名称状态无效")]
    ReferenceNameState {
        namespace: &'static str,
        key: String,
    },
    /// 同一装备族的强化链无法连续排列。
    #[error("装备族 {family_id} 的强化链无效: {message}")]
    FamilyChainInvalid { family_id: u64, message: String },
    /// 配方产物不在完整装备目录中。
    #[error("配方 {recipe_id} 的产物装备 {equipment_id} 不在装备目录中")]
    RecipeTargetMissing { recipe_id: u64, equipment_id: u64 },
    /// 领域内容无法稳定序列化为摘要输入。
    #[error("装备目录生成内容摘要失败: {0}")]
    Encode(#[source] serde_json::Error),
}

impl EquipmentMappingError {
    /// 构造不包含 Lua 原文、配置正文和自由格式错误文本的稳定诊断摘要。
    pub(crate) fn diagnostic_summary(&self) -> String {
        match self {
            Self::Protocol(_) => "完整装备目录协议校验失败".to_owned(),
            Self::Model(_) => "完整装备目录领域值校验失败".to_owned(),
            Self::PagesMissing { kind } => format!("{kind}页为空"),
            Self::PageGap {
                kind,
                expected,
                actual,
            } => format!("{kind}页游标断开，期望 {expected}，实际 {actual}"),
            Self::UnexpectedPage { kind, start_index } => {
                format!("{kind}目录结束后出现额外页面，起点 {start_index}")
            }
            Self::TotalCountMissing { kind } => format!("{kind}页缺少目录总数"),
            Self::CatalogOrderInvalid {
                kind,
                previous,
                actual,
            } => format!("{kind}条目标识顺序无效，前项 {previous}，当前 {actual}"),
            Self::TotalCountMismatch {
                kind,
                expected,
                actual,
            } => format!("{kind}页总数不一致，期望 {expected}，实际 {actual}"),
            Self::CatalogCoverage {
                kind,
                expected,
                actual,
            } => format!("{kind}目录覆盖不完整，期望 {expected}，实际 {actual}"),
            Self::PageIncomplete { kind, start_index } => {
                format!("{kind}页面不完整，起点 {start_index}")
            }
            Self::IncompleteConfig { config_id, errors } => format!(
                "装备配置 {config_id} 不完整，共 {} 条读取诊断，来源 {}",
                errors.len(),
                incomplete_config_sources(errors)
            ),
            Self::InvalidConfigField {
                config_id,
                field,
                message,
            } => format!(
                "装备配置 {config_id} 的字段 {field} 无效，规则 {}",
                safe_invalid_config_rule(message)
            ),
            Self::RootConfigMismatch {
                config_id,
                field,
                expected,
                actual,
            } => format!(
                "装备配置 {config_id} 的字段 {field} 与根配置不一致，期望 {expected}，实际 {actual:?}"
            ),
            Self::MissingReferenceName { namespace, key, .. } => {
                format!("{namespace}引用 {key} 缺少可用名称")
            }
            Self::ReferenceNameState { namespace, key } => {
                format!("{namespace}引用 {key} 的名称状态无效")
            }
            Self::FamilyChainInvalid { family_id, .. } => {
                format!("装备族 {family_id} 的强化链无效")
            }
            Self::RecipeTargetMissing {
                recipe_id,
                equipment_id,
            } => format!("配方 {recipe_id} 的产物装备 {equipment_id} 不在装备目录中"),
            Self::Encode(_) => "完整装备目录内容摘要编码失败".to_owned(),
        }
    }
}

/// 只允许映射器自身定义的固定规则进入日志，拒绝公开外部构造的自由格式文本。
fn safe_invalid_config_rule(message: &str) -> &'static str {
    match message {
        "缺少根配置 ID" => "缺少根配置 ID",
        "缺少客户端设备分类结果" => "缺少客户端设备分类结果",
        "缺少客户端舰载机分类结果" => "缺少客户端舰载机分类结果",
        "强化等级必须从 1 开始" => "强化等级必须从 1 开始",
        "强化等级超出领域模型允许的范围" => "强化等级超出领域模型允许的范围",
        "缺少装备评分" => "缺少装备评分",
        "必须是对象" => "必须是对象",
        "必须是正整数" => "必须是正整数",
        "数值超出 32 位无符号整数范围" => "数值超出 32 位无符号整数范围",
        "必须是非负整数" => "必须是非负整数",
        "必须是 null 或非负整数" => "必须是 null 或非负整数",
        "必须是非空文本" => "必须是非空文本",
        "必须是文本或 null" => "必须是文本或 null",
        "必须是字符串数组" => "必须是字符串数组",
        "数组项必须是非空文本" => "数组项必须是非空文本",
        "必须是正整数数组" => "必须是正整数数组",
        "数组项必须是正整数" => "数组项必须是正整数",
        "必须是资源二维数组" => "必须是资源二维数组",
        "每项必须是 [item_id, quantity]" => "每项必须是 [item_id, quantity]",
        "item_id 必须是正整数" => "item_id 必须是正整数",
        "quantity 必须是正整数" => "quantity 必须是正整数",
        "同一资源 ID 不得重复" => "同一资源 ID 不得重复",
        "必须是数组" => "必须是数组",
        "每项必须是对象" => "每项必须是对象",
        "每项必须是对象或 false" => "每项必须是对象或 false",
        "数组项必须是有限数值" => "数组项必须是有限数值",
        "必须是有限数值" => "必须是有限数值",
        "必须是布尔值" => "必须是布尔值",
        "属性键不得重复" => "属性键不得重复",
        "必须是技能引用数组" => "必须是技能引用数组",
        "技能引用必须是 ID 或 [ID, level]" => "技能引用必须是 ID 或 [ID, level]",
        "技能 ID 必须是正整数" => "技能 ID 必须是正整数",
        "技能等级必须是正整数" => "技能等级必须是正整数",
        "技能等级超出 32 位范围" => "技能等级超出 32 位范围",
        "技能 ID 和等级必须为正整数" => "技能 ID 和等级必须为正整数",
        "数组必须严格升序且不重复" => "数组必须严格升序且不重复",
        _ => "未分类规则",
    }
}

/// 只从原生读取器固定前缀推导来源标签，冒号后的动态错误正文不会进入日志。
fn incomplete_config_sources(errors: &[String]) -> String {
    const KNOWN_SOURCES: &[(&str, &str)] = &[
        ("读取 pg.equip_data_statistics", "equip_data_statistics"),
        ("equip_data_statistics", "equip_data_statistics"),
        ("读取 pg.equip_data_template", "equip_data_template"),
        ("equip_data_template", "equip_data_template"),
        ("Equipment.New", "Equipment.New"),
        ("getConfigTable", "getConfigTable"),
        ("GetRootEquipment", "GetRootEquipment"),
        ("GetAttributes", "GetAttributes"),
        ("GetPropertiesInfo", "GetPropertiesInfo"),
        ("GetSkill", "GetSkill"),
        ("GetPropertyRate", "GetPropertyRate"),
        ("GetWeaponID", "GetWeaponID"),
        ("GetGearScore", "GetGearScore"),
        ("GetAntiSirenPower", "GetAntiSirenPower"),
        ("isDevice", "isDevice"),
        ("isAircraft", "isAircraft"),
    ];

    let mut sources = BTreeSet::new();
    let mut unclassified = 0usize;
    for error in errors {
        if let Some((_, label)) = KNOWN_SOURCES
            .iter()
            .find(|(prefix, _)| error.starts_with(prefix))
        {
            sources.insert(*label);
        } else {
            unclassified += 1;
        }
    }
    let mut labels: Vec<&str> = sources.into_iter().collect();
    if unclassified > 0 {
        labels.push("未分类");
    }
    if labels.is_empty() {
        "无".to_owned()
    } else {
        labels.join("、")
    }
}

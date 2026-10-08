//! 定义装备目录摘要与维护样本共用的稳定序列化投影。

use serde::Serialize;

use crate::domain::{
    EQUIPMENT_CATALOG_SCHEMA_VERSION, EquipmentAttribute, EquipmentCatalog, EquipmentComposeRecipe,
    EquipmentConfigId, EquipmentDefinition, EquipmentFamily, EquipmentItemQuantity,
    EquipmentResources, EquipmentSkillReference, EquipmentSkillVisibility, NamedEquipmentShipType,
};

/// 核心目录的稳定序列化投影，同时作为内容摘要和维护样本的规范化正文。
#[derive(Serialize)]
pub(crate) struct EquipmentCatalogDocument<'a> {
    schema_version: u32,
    families: Vec<EquipmentFamilyDigest<'a>>,
    recipes: Vec<EquipmentComposeRecipeDigest>,
}

impl<'a> EquipmentCatalogDocument<'a> {
    pub(super) fn new(families: &'a [EquipmentFamily], recipes: &[EquipmentComposeRecipe]) -> Self {
        Self {
            schema_version: EQUIPMENT_CATALOG_SCHEMA_VERSION,
            families: families.iter().map(EquipmentFamilyDigest::from).collect(),
            recipes: recipes
                .iter()
                .map(EquipmentComposeRecipeDigest::from)
                .collect(),
        }
    }

    /// 从已经映射完成的领域目录恢复与内容摘要完全相同的规范化投影。
    pub(crate) fn from_catalog(catalog: &'a EquipmentCatalog) -> Self {
        Self::new(catalog.families(), catalog.recipes())
    }
}

#[derive(Serialize)]
struct EquipmentFamilyDigest<'a> {
    family_id: u64,
    configs: Vec<EquipmentDefinitionDigest<'a>>,
}

impl<'a> From<&'a EquipmentFamily> for EquipmentFamilyDigest<'a> {
    fn from(family: &'a EquipmentFamily) -> Self {
        Self {
            family_id: family.family_id().get(),
            configs: family
                .configs()
                .iter()
                .map(EquipmentDefinitionDigest::from)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentDefinitionDigest<'a> {
    identity: EquipmentIdentityDigest<'a>,
    classification: EquipmentClassificationDigest<'a>,
    enhancement: EquipmentEnhancementDigest<'a>,
    attributes: Vec<EquipmentAttributeDigest<'a>>,
    compatibility: EquipmentCompatibilityDigest<'a>,
    weapon_ids: &'a [u64],
    skill_references: Vec<EquipmentSkillReferenceDigest>,
    labels: &'a [String],
    description: &'a str,
    gear_score: u64,
    anti_siren_power: Option<f64>,
    importance: u32,
    equipment_limit: u64,
}

impl<'a> From<&'a EquipmentDefinition> for EquipmentDefinitionDigest<'a> {
    fn from(config: &'a EquipmentDefinition) -> Self {
        Self {
            identity: EquipmentIdentityDigest::from(config),
            classification: EquipmentClassificationDigest::from(config),
            enhancement: EquipmentEnhancementDigest::from(config),
            attributes: config
                .attributes()
                .iter()
                .map(EquipmentAttributeDigest::from)
                .collect(),
            compatibility: EquipmentCompatibilityDigest::from(config),
            weapon_ids: config.weapon_ids(),
            skill_references: config
                .skill_references()
                .iter()
                .copied()
                .map(EquipmentSkillReferenceDigest::from)
                .collect(),
            labels: config.labels(),
            description: config.description(),
            gear_score: config.gear_score(),
            anti_siren_power: config.anti_siren_power(),
            importance: config.importance(),
            equipment_limit: config.equipment_limit(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentIdentityDigest<'a> {
    config_id: u64,
    family_id: u64,
    name: &'a str,
    icon_key: &'a str,
}

impl<'a> From<&'a EquipmentDefinition> for EquipmentIdentityDigest<'a> {
    fn from(config: &'a EquipmentDefinition) -> Self {
        let identity = config.identity();
        Self {
            config_id: identity.config_id().get(),
            family_id: identity.family_id().get(),
            name: identity.name(),
            icon_key: identity.icon_key(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentClassificationDigest<'a> {
    equipment_type_id: u64,
    equipment_type_name: &'a str,
    nation_id: u64,
    nation_name: &'a str,
    rarity: u32,
    tech_level: u32,
    speciality: &'a str,
    ammo_type: u32,
    torpedo_ammo: u32,
    is_device: bool,
    is_aircraft: bool,
}

impl<'a> From<&'a EquipmentDefinition> for EquipmentClassificationDigest<'a> {
    fn from(config: &'a EquipmentDefinition) -> Self {
        let classification = config.classification();
        Self {
            equipment_type_id: classification.equipment_type().equipment_type_id(),
            equipment_type_name: classification.equipment_type().name(),
            nation_id: classification.nation().nation_id(),
            nation_name: classification.nation().name(),
            rarity: classification.rarity(),
            tech_level: classification.tech_level(),
            speciality: classification.speciality(),
            ammo_type: classification.ammo_type(),
            torpedo_ammo: classification.torpedo_ammo(),
            is_device: classification.is_device(),
            is_aircraft: classification.is_aircraft(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentEnhancementDigest<'a> {
    level: u8,
    base_config_id: Option<u64>,
    previous_config_id: Option<u64>,
    next_config_id: Option<u64>,
    upgrade_formula_ids: &'a [u64],
    next_cost: EquipmentResourcesDigest,
    restore_yield: EquipmentResourcesDigest,
    destroy_yield: EquipmentResourcesDigest,
}

impl<'a> From<&'a EquipmentDefinition> for EquipmentEnhancementDigest<'a> {
    fn from(config: &'a EquipmentDefinition) -> Self {
        let enhancement = config.enhancement();
        Self {
            level: enhancement.level().get(),
            base_config_id: enhancement.base_config_id().map(EquipmentConfigId::get),
            previous_config_id: enhancement.previous_config_id().map(EquipmentConfigId::get),
            next_config_id: enhancement.next_config_id().map(EquipmentConfigId::get),
            upgrade_formula_ids: enhancement.upgrade_formula_ids(),
            next_cost: EquipmentResourcesDigest::from(enhancement.next_cost()),
            restore_yield: EquipmentResourcesDigest::from(enhancement.restore_yield()),
            destroy_yield: EquipmentResourcesDigest::from(enhancement.destroy_yield()),
        }
    }
}

#[derive(Serialize)]
struct EquipmentResourcesDigest {
    gold: u64,
    items: Vec<EquipmentItemQuantityDigest>,
}

impl From<&EquipmentResources> for EquipmentResourcesDigest {
    fn from(resources: &EquipmentResources) -> Self {
        Self {
            gold: resources.gold(),
            items: resources
                .items()
                .iter()
                .copied()
                .map(EquipmentItemQuantityDigest::from)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentItemQuantityDigest {
    item_id: u64,
    quantity: u64,
}

impl From<EquipmentItemQuantity> for EquipmentItemQuantityDigest {
    fn from(item: EquipmentItemQuantity) -> Self {
        Self {
            item_id: item.item_id(),
            quantity: item.quantity(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentAttributeDigest<'a> {
    key: &'a str,
    name: &'a str,
    value: f64,
    auxiliary_boost: bool,
}

impl<'a> From<&'a EquipmentAttribute> for EquipmentAttributeDigest<'a> {
    fn from(attribute: &'a EquipmentAttribute) -> Self {
        Self {
            key: attribute.key(),
            name: attribute.name(),
            value: attribute.value(),
            auxiliary_boost: attribute.auxiliary_boost(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentCompatibilityDigest<'a> {
    main_ship_types: Vec<NamedEquipmentShipTypeDigest<'a>>,
    sub_ship_types: Vec<NamedEquipmentShipTypeDigest<'a>>,
    forbidden_ship_types: Vec<NamedEquipmentShipTypeDigest<'a>>,
}

impl<'a> From<&'a EquipmentDefinition> for EquipmentCompatibilityDigest<'a> {
    fn from(config: &'a EquipmentDefinition) -> Self {
        let compatibility = config.compatibility();
        Self {
            main_ship_types: compatibility
                .main_ship_types()
                .iter()
                .map(NamedEquipmentShipTypeDigest::from)
                .collect(),
            sub_ship_types: compatibility
                .sub_ship_types()
                .iter()
                .map(NamedEquipmentShipTypeDigest::from)
                .collect(),
            forbidden_ship_types: compatibility
                .forbidden_ship_types()
                .iter()
                .map(NamedEquipmentShipTypeDigest::from)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct NamedEquipmentShipTypeDigest<'a> {
    ship_type_id: u64,
    name: &'a str,
}

impl<'a> From<&'a NamedEquipmentShipType> for NamedEquipmentShipTypeDigest<'a> {
    fn from(ship_type: &'a NamedEquipmentShipType) -> Self {
        Self {
            ship_type_id: ship_type.ship_type_id(),
            name: ship_type.name(),
        }
    }
}

#[derive(Serialize)]
struct EquipmentSkillReferenceDigest {
    skill_id: u64,
    level: u32,
    visibility: &'static str,
}

impl From<EquipmentSkillReference> for EquipmentSkillReferenceDigest {
    fn from(reference: EquipmentSkillReference) -> Self {
        let visibility = match reference.visibility() {
            EquipmentSkillVisibility::Visible => "visible",
            EquipmentSkillVisibility::Hidden => "hidden",
        };
        Self {
            skill_id: reference.skill_id(),
            level: reference.level(),
            visibility,
        }
    }
}

#[derive(Serialize)]
struct EquipmentComposeRecipeDigest {
    recipe_id: u64,
    material: EquipmentItemQuantityDigest,
    gold: u64,
    equipment_config_id: u64,
}

impl From<&EquipmentComposeRecipe> for EquipmentComposeRecipeDigest {
    fn from(recipe: &EquipmentComposeRecipe) -> Self {
        Self {
            recipe_id: recipe.recipe_id(),
            material: EquipmentItemQuantityDigest::from(recipe.material()),
            gold: recipe.gold(),
            equipment_config_id: recipe.equipment_config_id().get(),
        }
    }
}

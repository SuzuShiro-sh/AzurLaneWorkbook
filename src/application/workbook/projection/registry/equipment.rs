//! 定义装备总表与合成配方工作表字段。

use super::{LayoutEditor, LayoutValueFormat, RegisteredLayoutField, push_field, push_read_only};

pub(super) fn add_equipment_inventory_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "equipment_inventory";
    for key in [
        "source_ref",
        "source_type",
        "family_id",
        "runtime_id",
        "config_id",
        "family_base_config_id",
        "previous_config_id",
        "next_config_id",
        "name",
        "ship_instance_id",
        "ship_name",
        "equipment_type",
        "nation",
        "speciality",
        "ammo_type",
        "compatible_main_ship_types",
        "compatible_sub_ship_types",
        "forbidden_ship_types",
        "description",
        "labels",
        "effect_summary",
        "family_owned_enhance_distribution",
        "blueprint_item_id",
        "planned_usage",
        "raw_config_hash",
        "read_errors",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            matches!(key, "source_type" | "config_id"),
        );
    }
    for key in [
        "attributes_json",
        "family_initial_attributes_json",
        "weapons_json",
        "skill_references_json",
        "skill_effects_json",
        "compose_material_costs",
        "next_cost_json",
        "restore_yield_json",
        "dismantle_yield_json",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Json, false);
    }
    for key in [
        "current_enhance_level",
        "family_initial_enhance_level",
        "maximum_enhance_level",
        "rarity",
        "tech_level",
        "gear_score",
        "importance",
        "equipment_limit",
        "quantity",
        "slot_index",
        "family_warehouse_quantity",
        "family_equipped_quantity",
        "family_owned_quantity",
        "blueprint_count",
        "compose_blueprint_cost",
        "compose_gold_cost",
        "craftable_by_gold",
        "craftable_by_materials",
        "craftable_actual",
        "family_potential_quantity",
        "planned_required_count",
        "planned_compose_count",
        "planned_dismantle_count",
        "simulated_remaining_count",
        "dismantle_gold_yield",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Integer,
            key == "quantity",
        );
    }
    push_read_only(
        fields,
        sheet,
        "anti_siren_power",
        LayoutValueFormat::Decimal,
        false,
    );
    for key in [
        "locked",
        "protected",
        "dismantlable",
        "is_device",
        "is_aircraft",
        "data_complete",
        "config_data_complete",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, false);
    }
    push_field(
        fields,
        sheet,
        "operation",
        LayoutValueFormat::Text,
        LayoutEditor::Enumeration,
        false,
        Some("inventory_operation"),
    );
    push_field(
        fields,
        sheet,
        "processing_quantity",
        LayoutValueFormat::Integer,
        LayoutEditor::Integer,
        false,
        None,
    );
    push_field(
        fields,
        sheet,
        "target_enhance_level",
        LayoutValueFormat::Integer,
        LayoutEditor::Integer,
        false,
        None,
    );
    push_field(
        fields,
        sheet,
        "note",
        LayoutValueFormat::Text,
        LayoutEditor::Text,
        false,
        None,
    );
}

pub(super) fn add_resource_recipe_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "resource_recipes";
    for key in [
        "recipe_type",
        "recipe_id",
        "equipment_family_id",
        "equipment_config_id",
        "resource_type",
        "resource_id",
        "resource_name",
        "source_ref",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            matches!(
                key,
                "recipe_type" | "recipe_id" | "resource_type" | "resource_id"
            ),
        );
    }
    for key in [
        "result_quantity",
        "required_quantity",
        "available_quantity",
        "maximum_craftable",
        "planned_delta",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Integer,
            matches!(key, "result_quantity" | "required_quantity"),
        );
    }
}

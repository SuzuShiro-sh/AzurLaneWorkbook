//! 定义舰船配装总表和计划元数据工作表字段。

use super::{LayoutEditor, LayoutValueFormat, RegisteredLayoutField, push_field, push_read_only};

pub(super) fn add_loadout_plan_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "loadout_plan";
    push_field(
        fields,
        sheet,
        "technology_bonus",
        LayoutValueFormat::Text,
        LayoutEditor::Text,
        false,
        None,
    );
    for key in crate::application::TECHNOLOGY_FIELDS {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, false);
    }
    push_read_only(fields, sheet, "source_type", LayoutValueFormat::Text, false);
    push_read_only(fields, sheet, "source_ref", LayoutValueFormat::Text, false);
    push_read_only(
        fields,
        sheet,
        "static_summary",
        LayoutValueFormat::Text,
        false,
    );
    push_read_only(
        fields,
        sheet,
        "static_raw_ref",
        LayoutValueFormat::Text,
        false,
    );
    push_read_only(fields, sheet, "instance_id", LayoutValueFormat::Text, false);
    for key in [
        "config_id",
        "name",
        "acquisition",
        "group_id",
        "ship_type",
        "nation",
        "armor_type",
        "skin_id",
        "fleet_status",
        "intimacy_stage",
        "read_errors",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, false);
    }
    for key in [
        "rarity",
        "current_stars",
        "maximum_stars",
        "level",
        "maximum_level",
        "experience_in_level",
        "total_experience",
        "next_level_experience",
        "energy",
        "proficiency",
        "intimacy",
        "intimacy_maximum",
        "create_time",
        "propose_time",
        "combat_power",
        "oil_start",
        "oil_end",
        "oil_total",
        "learned_skill_count",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            if key == "intimacy" {
                LayoutValueFormat::Decimal
            } else if key == "propose_time" || key == "create_time" {
                LayoutValueFormat::DateTime
            } else {
                LayoutValueFormat::Integer
            },
            false,
        );
    }
    for key in ["locked", "proposed", "data_complete"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, false);
    }
    for attribute in [
        "durability",
        "cannon",
        "torpedo",
        "air",
        "reload",
        "anti_aircraft",
        "hit",
        "dodge",
        "anti_sub",
        "luck",
        "speed",
    ] {
        push_read_only(
            fields,
            sheet,
            &format!("stat_{attribute}_summary"),
            LayoutValueFormat::Text,
            false,
        );
        for stage in ["base", "equipment_delta", "global_delta", "final"] {
            push_read_only(
                fields,
                sheet,
                &format!("stat_{attribute}_{stage}"),
                LayoutValueFormat::Decimal,
                false,
            );
        }
    }
    for key in [
        "skills_progress_summary",
        "skills_description_summary",
        "skills_effective_skill_id",
        "skills_name",
        "skills_description",
        "skills_current_effect",
        "skills_level",
        "skills_maximum_level",
        "skills_experience",
        "skills_next_level_experience",
        "skills_data_complete",
        "skills_read_errors",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, false);
    }
    for key in ["skills_effect_parameters", "skills_raw_structure"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Json, false);
    }

    for slot in 1_u8..=5 {
        add_loadout_slot_fields(fields, slot);
    }
    push_read_only(
        fields,
        sheet,
        "original_name",
        LayoutValueFormat::Text,
        false,
    );
}

fn add_loadout_slot_fields(fields: &mut Vec<RegisteredLayoutField>, slot: u8) {
    let sheet = "loadout_plan";
    for (suffix, value_format) in [
        ("allowed_equipment_types", LayoutValueFormat::Text),
        ("equipment_name", LayoutValueFormat::Text),
        ("runtime_id", LayoutValueFormat::Text),
        ("config_id", LayoutValueFormat::Text),
        ("family_id", LayoutValueFormat::Text),
        ("enhance_level", LayoutValueFormat::Integer),
        ("effect_summary", LayoutValueFormat::Text),
    ] {
        push_read_only(
            fields,
            sheet,
            &format!("slot_{slot}_{suffix}"),
            value_format,
            false,
        );
    }
    for (suffix, value_format, editor, required, enum_category) in [
        (
            "target_equipment_family",
            LayoutValueFormat::Text,
            LayoutEditor::Text,
            false,
            None,
        ),
        (
            "source_policy",
            LayoutValueFormat::Text,
            LayoutEditor::Enumeration,
            false,
            Some("source_policy"),
        ),
        (
            "exact_source",
            LayoutValueFormat::Text,
            LayoutEditor::Text,
            false,
            None,
        ),
        (
            "target_enhance_level",
            LayoutValueFormat::Integer,
            LayoutEditor::Integer,
            false,
            None,
        ),
        (
            "allocation_priority",
            LayoutValueFormat::Integer,
            LayoutEditor::Integer,
            false,
            None,
        ),
        (
            "note",
            LayoutValueFormat::Text,
            LayoutEditor::Text,
            false,
            None,
        ),
    ] {
        push_field(
            fields,
            sheet,
            &format!("slot_{slot}_{suffix}"),
            value_format,
            editor,
            required,
            enum_category,
        );
    }
}

pub(super) fn add_plan_data_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "plan_data";
    for key in [
        "plan_hash",
        "step_type",
        "target_slot_ref",
        "target_equipment_family_id",
        "source_ref",
        "source_config_id",
        "source_slot_final_state",
        "expected_result",
        "precondition_hash",
        "resource_snapshot_hash",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            matches!(
                key,
                "plan_hash" | "step_type" | "target_slot_ref" | "precondition_hash"
            ),
        );
    }
    for key in [
        "step_sequence",
        "source_enhance_level",
        "target_enhance_level",
        "quantity",
        "gold_delta",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Integer,
            matches!(key, "step_sequence" | "quantity"),
        );
    }
    push_read_only(
        fields,
        sheet,
        "material_deltas",
        LayoutValueFormat::Json,
        true,
    );
}

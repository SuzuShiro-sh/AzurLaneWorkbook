//! 定义检查结果和执行结果工作表字段。

use super::{LayoutEditor, LayoutValueFormat, RegisteredLayoutField, push_field, push_read_only};

pub(super) fn add_check_result_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "check_results";
    push_read_only(
        fields,
        sheet,
        "checked_at",
        LayoutValueFormat::DateTime,
        true,
    );
    push_read_only(fields, sheet, "status", LayoutValueFormat::Text, true);
    push_read_only(
        fields,
        sheet,
        "issue_sequence",
        LayoutValueFormat::Integer,
        true,
    );
    push_read_only(fields, sheet, "severity", LayoutValueFormat::Text, true);
    for key in [
        "code",
        "message",
        "object_type",
        "object_ref",
        "sheet_key",
        "field_key",
        "current_value",
        "expected_value",
        "slot_current_state",
        "slot_target_state",
        "equipment_source",
        "source_slot_final_state",
        "dismantle_source",
        "plan_hash",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            matches!(key, "code" | "message" | "plan_hash"),
        );
    }
    for key in ["logical_row", "compose_quantity", "dismantle_quantity"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Integer, false);
    }
    for key in [
        "compose_costs",
        "enhance_steps",
        "enhance_costs",
        "dismantle_yields",
        "resource_delta",
        "execution_steps",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Json, false);
    }
}

pub(super) fn add_execution_result_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "execution_results";
    push_read_only(
        fields,
        sheet,
        "executed_at",
        LayoutValueFormat::DateTime,
        true,
    );
    push_read_only(
        fields,
        sheet,
        "step_sequence",
        LayoutValueFormat::Integer,
        true,
    );
    for key in [
        "step_type",
        "object_ref",
        "request_summary",
        "response_summary",
        "readback_summary",
        "error_code",
        "message",
        "verified_slot_state",
        "verified_equipment_source",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            matches!(key, "step_type" | "object_ref"),
        );
    }
    push_field(
        fields,
        sheet,
        "final_verification_status",
        LayoutValueFormat::Text,
        LayoutEditor::ReadOnly,
        true,
        Some("execution_final_verification_status"),
    );
    push_read_only(fields, sheet, "plan_hash", LayoutValueFormat::Text, true);
    push_field(
        fields,
        sheet,
        "status",
        LayoutValueFormat::Text,
        LayoutEditor::ReadOnly,
        true,
        Some("execution_status"),
    );
    for key in [
        "verified_enhance_level",
        "gold_before",
        "gold_after",
        "gold_delta",
        "composed_config_delta",
        "dismantled_source_quantity",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Integer, false);
    }
    for key in ["materials_before", "materials_after", "materials_delta"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Json, false);
    }

    for (key, enum_category) in [
        ("report_status", "execution_report_status"),
        ("stop_reason", "execution_stop_reason"),
    ] {
        push_field(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            LayoutEditor::ReadOnly,
            true,
            Some(enum_category),
        );
    }
    for key in ["target_fingerprint_sha256", "report_hash"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, true);
    }
    for key in ["may_have_writes", "write_acknowledged"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, true);
    }
    push_field(
        fields,
        sheet,
        "write_effect",
        LayoutValueFormat::Text,
        LayoutEditor::ReadOnly,
        true,
        Some("execution_write_effect"),
    );
    for key in [
        "acknowledged_write_count",
        "observed_state_change_count",
        "verified_write_count",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Integer, true);
    }
}

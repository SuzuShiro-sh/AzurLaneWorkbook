//! 定义字典、原始数据和 schema 支撑工作表字段。

use super::{LayoutValueFormat, RegisteredLayoutField, push_read_only};

pub(super) fn add_dictionary_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "dictionaries";
    for key in [
        "category_key",
        "stable_value",
        "display_label",
        "object_ref",
        "description",
        "layout_hash",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            LayoutValueFormat::Text,
            matches!(key, "category_key" | "stable_value" | "display_label"),
        );
    }
    push_read_only(fields, sheet, "order", LayoutValueFormat::Integer, true);
}

pub(super) fn add_raw_data_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "raw_data";
    for key in [
        "entity_type",
        "entity_id",
        "content_sha256",
        "source_content_sha256",
        "source_ref",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, false);
    }
    for key in ["chunk_index", "chunk_count", "schema_version"] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Integer, false);
    }
    push_read_only(
        fields,
        sheet,
        "canonical_json_chunk",
        LayoutValueFormat::Json,
        false,
    );
}

pub(super) fn add_schema_fields(fields: &mut Vec<RegisteredLayoutField>) {
    let sheet = "schema";
    for key in [
        "layout_hash",
        "workbook_hash",
        "plan_hash",
        "snapshot_hash",
        "sheet_key",
        "sheet_name",
        "sheet_generation",
        "sheet_freeze_cell",
        "sheet_description",
        "field_key",
        "column_name",
        "field_generation",
        "field_value_format",
        "field_description",
        "model_path",
        "editor",
        "field_enum_category",
        "read_only_range",
        "formula_hash",
        "formula_key",
        "omitted_items",
    ] {
        push_read_only(
            fields,
            sheet,
            key,
            if key == "omitted_items" {
                LayoutValueFormat::Json
            } else {
                LayoutValueFormat::Text
            },
            true,
        );
    }
    for key in [
        "workbook_schema_version",
        "layout_schema_version",
        "sheet_order",
        "field_order",
        "field_width_hundredths",
        "column_index",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Integer, true);
    }
    for key in [
        "sheet_default_filter",
        "sheet_required",
        "field_wrap",
        "field_required",
    ] {
        push_read_only(fields, sheet, key, LayoutValueFormat::Text, true);
    }
    push_read_only(
        fields,
        sheet,
        "created_at",
        LayoutValueFormat::DateTime,
        true,
    );
}

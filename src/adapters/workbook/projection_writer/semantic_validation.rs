//! 按严格布局逐格核验投影、字典与 schema 工作表的业务语义。

use std::collections::BTreeMap;

use calamine::Data;

use crate::application::{
    LayoutEditor, LayoutGenerationMode, LayoutValueFormat, WORKBOOK_PROJECTION_SCHEMA_VERSION,
    WorkbookFieldLayout, WorkbookLayout, WorkbookProjectionSheet, WorkbookProjectionV4,
    WorkbookProjectionValue, WorkbookSheetLayout,
};

use super::{EXCEL_UNIX_EPOCH_DAYS, MILLIS_PER_DAY, WorkbookProbeError, invalid};
use crate::adapters::workbook::rendering::{
    generated_column_index, integer_overflow, omitted_items_json,
};
use crate::adapters::workbook::worksheet_primitives::column_read_only_range;

pub(super) fn verify_headers_and_extent(
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    row_count: usize,
    range: &calamine::Range<Data>,
) -> Result<(), WorkbookProbeError> {
    if range.width() != fields.len() {
        return Err(invalid(format!(
            "工作表 {} 列数错误: expected={}, actual={}",
            sheet.display_name(),
            fields.len(),
            range.width()
        )));
    }
    let maximum_height = row_count.max(1) + 1;
    if range.height() > maximum_height || range.height() == 0 {
        return Err(invalid(format!(
            "工作表 {} 数据范围高度错误: maximum={maximum_height}, actual={}",
            sheet.display_name(),
            range.height()
        )));
    }
    for (column, field) in fields.iter().enumerate() {
        let actual = range.get_value((0, u32::try_from(column).map_err(integer_overflow)?));
        assert_text_cell(
            actual,
            field.display_name(),
            &format!("{}.header.{}", sheet.stable_key(), field.stable_key()),
        )?;
    }
    if row_count == 0 {
        for (column, field) in fields.iter().enumerate() {
            let column = u32::try_from(column).map_err(integer_overflow)?;
            assert_blank_cell(
                range.get_value((1, column)),
                &format!(
                    "{}.empty_placeholder.{}",
                    sheet.stable_key(),
                    field.stable_key()
                ),
            )?;
        }
    }
    Ok(())
}

/// 按稳定对象行和布局列逐格核对普通投影工作表的语义值。
pub(super) fn verify_projection_values(
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    projection_sheet: &WorkbookProjectionSheet,
    range: &calamine::Range<Data>,
    enum_labels: &super::EnumLabels,
    row_offset: usize,
) -> Result<(), WorkbookProbeError> {
    for (row_index, row) in projection_sheet.rows().iter().enumerate() {
        let excel_row = u32::try_from(row_index + row_offset + 1).map_err(integer_overflow)?;
        for (column_index, field) in fields.iter().enumerate() {
            let column = u32::try_from(column_index).map_err(integer_overflow)?;
            let value = row.value(field.stable_key()).ok_or_else(|| {
                invalid(format!(
                    "工作表 {} 的对象 {} 缺少字段 {}",
                    sheet.stable_key(),
                    row.object_ref(),
                    field.stable_key()
                ))
            })?;
            let display = crate::adapters::workbook::equipment_display::projected_text(field, row)
                .map_err(invalid)?
                .map(WorkbookProjectionValue::Text);
            let value = display.as_ref().unwrap_or(value);
            verify_projection_cell(
                range.get_value((excel_row, column)),
                value,
                enum_labels,
                &format!(
                    "{}.{}.{}",
                    sheet.stable_key(),
                    row.object_ref(),
                    field.stable_key()
                ),
            )?;
        }
    }
    Ok(())
}

/// 依照投影值类型执行文本、精确数值、日期和枚举标签比较。
pub(in crate::adapters::workbook) fn verify_projection_cell(
    actual: Option<&Data>,
    expected: &WorkbookProjectionValue,
    enum_labels: &super::EnumLabels,
    label: &str,
) -> Result<(), WorkbookProbeError> {
    match expected {
        WorkbookProjectionValue::Blank => assert_blank_cell(actual, label),
        WorkbookProjectionValue::Text(value) | WorkbookProjectionValue::Json(value) => {
            assert_text_cell(actual, value, label)
        }
        WorkbookProjectionValue::Integer(value) => assert_number_cell(actual, *value as f64, label),
        WorkbookProjectionValue::Decimal(value) => assert_number_cell(actual, *value, label),
        WorkbookProjectionValue::DateTimeUnixMillis(value) => {
            assert_datetime_cell(actual, *value, label)
        }
        WorkbookProjectionValue::Boolean(value) => {
            assert_text_cell(actual, if *value { "是" } else { "否" }, label)
        }
        WorkbookProjectionValue::Enumeration {
            category_key,
            stable_value,
        } => {
            let expected = enum_labels
                .get(category_key.as_str())
                .and_then(|values| values.get(stable_value.as_str()))
                .ok_or_else(|| invalid(format!("{label} 缺少枚举显示标签")))?;
            assert_text_cell(actual, expected, label)
        }
    }
}

/// 核对由布局枚举选项合成的字典行，不信任投影输入提供技术表内容。
pub(super) fn verify_dictionary_values(
    layout: &WorkbookLayout,
    fields: &[&WorkbookFieldLayout],
    range: &calamine::Range<Data>,
) -> Result<(), WorkbookProbeError> {
    for (row_index, option) in layout.enum_options().iter().enumerate() {
        let row = u32::try_from(row_index + 1).map_err(integer_overflow)?;
        for (column_index, field) in fields.iter().enumerate() {
            let column = u32::try_from(column_index).map_err(integer_overflow)?;
            let actual = range.get_value((row, column));
            let label = format!(
                "dictionaries.{}.{}.{}",
                option.category_key(),
                option.stable_value(),
                field.stable_key()
            );
            match field.stable_key() {
                "category_key" => assert_text_cell(actual, option.category_key(), &label)?,
                "stable_value" => assert_text_cell(actual, option.stable_value(), &label)?,
                "display_label" => assert_text_cell(actual, option.label(), &label)?,
                "object_ref" => assert_blank_cell(actual, &label)?,
                "description" => assert_text_cell(actual, option.description(), &label)?,
                "layout_hash" => assert_text_cell(actual, layout.content_sha256(), &label)?,
                "order" => assert_number_cell(actual, f64::from(option.order()), &label)?,
                key => return Err(invalid(format!("字典字段 {key} 缺少验证映射"))),
            }
        }
    }
    Ok(())
}

/// 核对每个布局字段对应的 schema 行，并返回一致的首次生成时间。
pub(super) fn verify_schema_values(
    layout: &WorkbookLayout,
    fields: &[&WorkbookFieldLayout],
    range: &calamine::Range<Data>,
    projection: &WorkbookProjectionV4,
    expected_semantic_sha256: &str,
) -> Result<i64, WorkbookProbeError> {
    let columns: BTreeMap<&str, u32> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            Ok((
                field.stable_key(),
                u32::try_from(index).map_err(integer_overflow)?,
            ))
        })
        .collect::<Result<_, WorkbookProbeError>>()?;
    let created_column = required_column(&columns, "created_at")?;
    let first_created = range
        .get_value((1, created_column))
        .and_then(cell_number)
        .ok_or_else(|| invalid("schema.created_at 不是日期数值"))?;
    let generated_at_unix_millis = unix_millis_from_excel_serial(first_created)?;
    let omitted_items = omitted_items_json(layout)?;

    let sheets_by_key: BTreeMap<&str, &WorkbookSheetLayout> = layout
        .sheets()
        .iter()
        .map(|sheet| (sheet.stable_key(), sheet))
        .collect();
    for (row_index, field) in layout.fields().iter().enumerate() {
        let row = u32::try_from(row_index + 1).map_err(integer_overflow)?;
        let sheet = sheets_by_key
            .get(field.sheet_key())
            .copied()
            .ok_or_else(|| invalid(format!("schema 字段 {} 缺少所属工作表", field.stable_key())))?;
        let generated_column = generated_column_index(layout, sheet, field);
        for schema_field in fields {
            let column = required_column(&columns, schema_field.stable_key())?;
            verify_schema_cell(
                range.get_value((row, column)),
                schema_field.stable_key(),
                row,
                layout,
                projection,
                sheet,
                field,
                generated_column,
                &omitted_items,
                expected_semantic_sha256,
                generated_at_unix_millis,
            )?;
        }
    }
    Ok(generated_at_unix_millis)
}

/// 将一个 schema 字段映射回布局、投影摘要或固定空白占位并逐值比较。
#[allow(clippy::too_many_arguments)]
fn verify_schema_cell(
    actual: Option<&Data>,
    key: &str,
    row: u32,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    sheet: &WorkbookSheetLayout,
    field: &WorkbookFieldLayout,
    generated_column: Option<u16>,
    omitted_items: &str,
    expected_semantic_sha256: &str,
    expected_created_unix_millis: i64,
) -> Result<(), WorkbookProbeError> {
    let label = format!("schema.{row}.{key}");
    if key == "read_only_range" {
        let expected = if field.editor() == LayoutEditor::ReadOnly {
            generated_column.map(column_read_only_range)
        } else {
            None
        };
        return match expected {
            Some(expected) => assert_text_cell(actual, &expected, &label),
            None => assert_blank_cell(actual, &label),
        };
    }
    let expected_text = match key {
        "layout_hash" => Some(layout.content_sha256()),
        "workbook_hash" => Some(expected_semantic_sha256),
        "plan_hash" => Some(""),
        "snapshot_hash" => Some(projection.source().game_state_content_sha256()),
        "sheet_key" => Some(sheet.stable_key()),
        "sheet_name" => Some(sheet.display_name()),
        "sheet_generation" => Some(LayoutGenerationMode::stable_value(sheet.generation())),
        "sheet_freeze_cell" => Some(sheet.freeze_cell().unwrap_or("")),
        "sheet_description" => Some(sheet.description()),
        "field_key" => Some(field.stable_key()),
        "column_name" => Some(field.display_name()),
        "field_generation" => Some(LayoutGenerationMode::stable_value(field.generation())),
        "field_value_format" => Some(LayoutValueFormat::stable_value(field.value_format())),
        "field_description" => Some(field.description()),
        "model_path" => Some(field.model_path()),
        "editor" => Some(LayoutEditor::stable_value(field.editor())),
        "field_enum_category" => Some(field.enum_category().unwrap_or("")),
        "formula_hash" | "formula_key" => Some(""),
        "omitted_items" => Some(omitted_items),
        "sheet_default_filter" => Some(boolean_label(sheet.default_filter())),
        "sheet_required" => Some(boolean_label(sheet.required())),
        "field_wrap" => Some(boolean_label(field.wrap())),
        "field_required" => Some(boolean_label(field.required())),
        _ => None,
    };
    if let Some(expected) = expected_text {
        return if expected.is_empty() {
            assert_blank_cell(actual, &label)
        } else {
            assert_text_cell(actual, expected, &label)
        };
    }
    if key == "created_at" {
        return assert_datetime_cell(actual, expected_created_unix_millis, &label);
    }
    let expected_number = match key {
        "workbook_schema_version" => f64::from(WORKBOOK_PROJECTION_SCHEMA_VERSION),
        "layout_schema_version" => f64::from(layout.schema_version()),
        "sheet_order" => f64::from(sheet.order()),
        "field_order" => f64::from(field.order()),
        "field_width_hundredths" => f64::from(field.width().hundredths()),
        "column_index" => generated_column.map_or(0.0, |column| f64::from(column) + 1.0),
        _ => return Err(invalid(format!("schema 字段 {key} 缺少验证映射"))),
    };
    assert_number_cell(actual, expected_number, &label)
}

const fn boolean_label(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}

fn required_column(columns: &BTreeMap<&str, u32>, key: &str) -> Result<u32, WorkbookProbeError> {
    columns
        .get(key)
        .copied()
        .ok_or_else(|| invalid(format!("schema 工作表缺少可验证字段 {key}")))
}

fn assert_blank_cell(actual: Option<&Data>, label: &str) -> Result<(), WorkbookProbeError> {
    if actual.is_none() || matches!(actual, Some(Data::Empty)) {
        Ok(())
    } else {
        Err(invalid(format!("{label} 应为空白，实际为 {actual:?}")))
    }
}

fn assert_text_cell(
    actual: Option<&Data>,
    expected: &str,
    label: &str,
) -> Result<(), WorkbookProbeError> {
    if expected.is_empty() && (actual.is_none() || matches!(actual, Some(Data::Empty))) {
        return Ok(());
    }
    match actual {
        Some(Data::String(actual)) if actual == expected => Ok(()),
        _ => Err(invalid(format!(
            "{label} 文本错误: expected={expected:?}, actual={actual:?}"
        ))),
    }
}

pub(super) fn assert_number_cell(
    actual: Option<&Data>,
    expected: f64,
    label: &str,
) -> Result<(), WorkbookProbeError> {
    let actual_number = actual.and_then(cell_number).ok_or_else(|| {
        invalid(format!(
            "{label} 应为数值: expected={expected}, actual={actual:?}"
        ))
    })?;
    if actual_number == expected {
        Ok(())
    } else {
        Err(invalid(format!(
            "{label} 数值错误: expected={expected}, actual={actual_number}"
        )))
    }
}

pub(super) fn assert_datetime_cell(
    actual: Option<&Data>,
    expected_unix_millis: i64,
    label: &str,
) -> Result<(), WorkbookProbeError> {
    let actual_serial = actual.and_then(cell_number).ok_or_else(|| {
        invalid(format!(
            "{label} 应为日期数值: expected_unix_millis={expected_unix_millis}, actual={actual:?}"
        ))
    })?;
    let actual_unix_millis = unix_millis_from_excel_serial(actual_serial)?;
    if actual_unix_millis == expected_unix_millis {
        Ok(())
    } else {
        Err(invalid(format!(
            "{label} 日期错误: expected_unix_millis={expected_unix_millis}, actual_unix_millis={actual_unix_millis}"
        )))
    }
}

fn cell_number(value: &Data) -> Option<f64> {
    match value {
        Data::Int(value) => Some(*value as f64),
        Data::Float(value) => Some(*value),
        Data::DateTime(value) => Some(value.as_f64()),
        _ => None,
    }
}

fn unix_millis_from_excel_serial(value: f64) -> Result<i64, WorkbookProbeError> {
    if !value.is_finite() {
        return Err(invalid("schema.created_at 不是有限日期数值"));
    }
    let millis = ((value - EXCEL_UNIX_EPOCH_DAYS) * MILLIS_PER_DAY).round();
    if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return Err(invalid("schema.created_at 超出 Unix 毫秒范围"));
    }
    Ok(millis as i64)
}

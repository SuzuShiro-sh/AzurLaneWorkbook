//! 构建并重开验证只含示例行的布局预览工作簿。

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook_from_rs};
use rust_xlsxwriter::{DocProperties, ExcelDateTime, Format, Workbook, Worksheet};

use crate::application::{
    LayoutEditor, LayoutGenerationMode, LayoutValueFormat, WorkbookFieldLayout, WorkbookLayout,
    WorkbookLayoutEnumOption, WorkbookSheetLayout,
};

use super::super::super::WorkbookProbeError;
use super::super::super::editor::reject_external_content;
use super::super::super::package::PackageSnapshot;
use super::super::super::rendering::{
    DictionaryRange, SchemaValues, WorkbookFormats, add_layout_worksheet, build_error,
    define_dictionary_names, dictionary_ranges, finalize_generated_workbook,
    finish_layout_worksheet, integer_overflow, invalid_generated_workbook, omitted_items_json,
    workbook_sheet_states, write_dictionary_rows, write_schema_rows,
};
/// 生成结果及原子发布报告所需的布局计数。
pub(in crate::adapters::workbook) struct LayoutPreviewBuild {
    pub(in crate::adapters::workbook) bytes: Vec<u8>,
    pub(in crate::adapters::workbook) generated_sheets: usize,
    pub(in crate::adapters::workbook) hidden_sheets: usize,
    pub(in crate::adapters::workbook) omitted_sheets: usize,
    pub(in crate::adapters::workbook) generated_fields: usize,
    pub(in crate::adapters::workbook) hidden_fields: usize,
    pub(in crate::adapters::workbook) omitted_fields: usize,
    pub(in crate::adapters::workbook) example_rows: usize,
}

/// 根据严格布局建立只有表头、样式、隐藏状态和示例行的数据工作簿。
pub(in crate::adapters::workbook) fn build_layout_preview_workbook_bytes(
    path: &Path,
    layout: &WorkbookLayout,
) -> Result<LayoutPreviewBuild, WorkbookProbeError> {
    let generated_sheets: Vec<&WorkbookSheetLayout> = layout
        .sheets()
        .iter()
        .filter(|sheet| sheet.generation() != LayoutGenerationMode::Omitted)
        .collect();
    if !generated_sheets
        .iter()
        .any(|sheet| sheet.generation() == LayoutGenerationMode::Visible)
    {
        return Err(build_error("布局至少需要一张可见工作表"));
    }

    let dictionary_ranges = dictionary_ranges(layout)?;
    let omitted_items = omitted_items_json(layout)?;
    let formats = WorkbookFormats::new(layout)?;
    let example_time = fixed_datetime()?;
    let schema_values = SchemaValues {
        workbook_hash: "",
        plan_hash: "",
        snapshot_hash: "",
        formula_hash: "",
        formula_key: "",
        created_at: &example_time,
    };
    let mut workbook = Workbook::new();
    let properties = DocProperties::new()
        .set_title("碧蓝航线工作簿布局预览")
        .set_subject("严格布局生成结果")
        .set_author("AzurLaneWorkbook")
        .set_company("AzurLaneWorkbook")
        .set_creation_datetime(&example_time);
    workbook.set_properties(&properties);

    let active_key = generated_sheets
        .iter()
        .find(|sheet| sheet.generation() == LayoutGenerationMode::Visible)
        .map(|sheet| sheet.stable_key())
        .expect("上方已经确认存在可见工作表");
    let mut example_rows = 0_usize;
    for sheet in &generated_sheets {
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        if fields.is_empty() {
            return Err(build_error(format!(
                "生成工作表 {} 没有可写字段",
                sheet.stable_key()
            )));
        }

        let rows = write_preview_sheet(
            &mut workbook,
            layout,
            sheet,
            &fields,
            &formats,
            &dictionary_ranges,
            &omitted_items,
            &schema_values,
        )?;
        example_rows = example_rows
            .checked_add(rows)
            .ok_or_else(|| build_error("示例行计数溢出"))?;
        let worksheet = workbook
            .worksheet_from_name(sheet.display_name())
            .map_err(WorkbookProbeError::from)?;
        if sheet.stable_key() == active_key {
            worksheet.set_active(true);
        }
        if sheet.generation() == LayoutGenerationMode::Hidden {
            worksheet.set_hidden(true);
        }
    }
    define_dictionary_names(&mut workbook, layout, &dictionary_ranges)?;

    let bytes = workbook
        .save_to_buffer()
        .map_err(WorkbookProbeError::from)?;
    let bytes = finalize_generated_workbook(&bytes, path)?;
    verify_layout_preview_workbook(path, &bytes, layout)?;

    let generated_fields = layout
        .fields()
        .iter()
        .filter(|field| {
            field.generation() != LayoutGenerationMode::Omitted
                && generated_sheets
                    .iter()
                    .any(|sheet| sheet.stable_key() == field.sheet_key())
        })
        .count();
    let hidden_fields = layout
        .fields()
        .iter()
        .filter(|field| {
            field.generation() == LayoutGenerationMode::Hidden
                && generated_sheets
                    .iter()
                    .any(|sheet| sheet.stable_key() == field.sheet_key())
        })
        .count();
    Ok(LayoutPreviewBuild {
        bytes,
        generated_sheets: generated_sheets.len(),
        hidden_sheets: generated_sheets
            .iter()
            .filter(|sheet| sheet.generation() == LayoutGenerationMode::Hidden)
            .count(),
        omitted_sheets: layout.sheets().len() - generated_sheets.len(),
        generated_fields,
        hidden_fields,
        omitted_fields: layout.fields().len() - generated_fields,
        example_rows,
    })
}

/// 用受限包解析器和独立语义解析器重开生成字节，核对表名、表头和隐藏状态。
pub(in crate::adapters::workbook) fn verify_layout_preview_workbook(
    path: &Path,
    bytes: &[u8],
    layout: &WorkbookLayout,
) -> Result<(), WorkbookProbeError> {
    let package = PackageSnapshot::from_bytes(bytes, path)?;
    reject_external_content(&package)?;
    let expected_sheets: Vec<&WorkbookSheetLayout> = layout
        .sheets()
        .iter()
        .filter(|sheet| sheet.generation() != LayoutGenerationMode::Omitted)
        .collect();
    let actual_states =
        workbook_sheet_states(package.part("xl/workbook.xml")?, "layout-preview.xlsx")?;
    if actual_states.len() != expected_sheets.len() {
        return Err(invalid_preview(format!(
            "工作表数量错误: expected={}, actual={}",
            expected_sheets.len(),
            actual_states.len()
        )));
    }
    for (actual, expected) in actual_states.iter().zip(&expected_sheets) {
        let expected_hidden = expected.generation() == LayoutGenerationMode::Hidden;
        if actual.name != expected.display_name() || actual.hidden != expected_hidden {
            return Err(invalid_preview(format!(
                "工作表状态错误: expected={} hidden={}, actual={} hidden={}",
                expected.display_name(),
                expected_hidden,
                actual.name,
                actual.hidden
            )));
        }
    }

    let mut workbook: Xlsx<Cursor<&[u8]>> = open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    if workbook.sheet_names()
        != expected_sheets
            .iter()
            .map(|sheet| sheet.display_name().to_owned())
            .collect::<Vec<_>>()
    {
        return Err(invalid_preview("语义读取的工作表顺序与布局不一致"));
    }
    for sheet in expected_sheets {
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let range = workbook
            .worksheet_range(sheet.display_name())
            .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
        if range.width() != fields.len() || range.height() < 2 {
            return Err(invalid_preview(format!(
                "工作表 {} 的列数或示例行不完整",
                sheet.display_name()
            )));
        }
        for (column, field) in fields.iter().enumerate() {
            match range.get_value((0, u32::try_from(column).map_err(integer_overflow)?)) {
                Some(Data::String(value)) if value == field.display_name() => {}
                actual => {
                    return Err(invalid_preview(format!(
                        "工作表 {} 第 {} 列表头错误: {actual:?}",
                        sheet.display_name(),
                        column + 1
                    )));
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_preview_sheet(
    workbook: &mut Workbook,
    layout: &WorkbookLayout,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    formats: &WorkbookFormats,
    dictionary_ranges: &BTreeMap<String, DictionaryRange>,
    omitted_items: &str,
    schema_values: &SchemaValues<'_>,
) -> Result<usize, WorkbookProbeError> {
    let worksheet = add_layout_worksheet(workbook, sheet, fields)?;

    let row_count = match sheet.stable_key() {
        "dictionaries" => write_dictionary_rows(worksheet, layout, fields, formats)?,
        "schema" => write_schema_rows(
            worksheet,
            layout,
            fields,
            formats,
            omitted_items,
            schema_values,
        )?,
        "check_results" => write_regular_rows(
            worksheet,
            layout,
            fields,
            formats,
            schema_values.created_at,
            2,
        )?,
        _ => write_regular_rows(
            worksheet,
            layout,
            fields,
            formats,
            schema_values.created_at,
            1,
        )?,
    };
    finish_layout_worksheet(
        worksheet,
        sheet,
        fields,
        formats,
        dictionary_ranges,
        row_count,
    )?;
    Ok(row_count)
}

fn write_regular_rows(
    worksheet: &mut Worksheet,
    layout: &WorkbookLayout,
    fields: &[&WorkbookFieldLayout],
    formats: &WorkbookFormats,
    example_time: &ExcelDateTime,
    row_count: usize,
) -> Result<usize, WorkbookProbeError> {
    for row_index in 0..row_count {
        let row = u32::try_from(row_index + 1).map_err(integer_overflow)?;
        let row_style = if row_count == 2 {
            Some(if row_index == 0 { "warning" } else { "error" })
        } else {
            None
        };
        for (column_index, field) in fields.iter().enumerate() {
            let column = u16::try_from(column_index).map_err(integer_overflow)?;
            let format = formats.for_field(field, row_style)?;
            write_regular_sample(
                worksheet,
                row,
                column,
                field,
                layout.enum_options(),
                row_index,
                example_time,
                &format,
            )?;
        }
    }
    Ok(row_count)
}

#[allow(clippy::too_many_arguments)]
fn write_regular_sample(
    worksheet: &mut Worksheet,
    row: u32,
    column: u16,
    field: &WorkbookFieldLayout,
    enum_options: &[WorkbookLayoutEnumOption],
    row_index: usize,
    example_time: &ExcelDateTime,
    format: &Format,
) -> Result<(), WorkbookProbeError> {
    if field.sheet_key() == "check_results" && field.stable_key() == "message" {
        let message = if row_index == 0 {
            "示例警告"
        } else {
            "示例错误"
        };
        worksheet.write_string_with_format(row, column, message, format)?;
        return Ok(());
    }
    if let Some(category) = field.enum_category() {
        let stable_value =
            if field.sheet_key() == "check_results" && field.stable_key() == "severity" {
                Some(if row_index == 0 { "warning" } else { "error" })
            } else {
                None
            };
        let option = stable_value
            .and_then(|value| {
                enum_options.iter().find(|option| {
                    option.category_key() == category && option.stable_value() == value
                })
            })
            .or_else(|| {
                enum_options
                    .iter()
                    .find(|option| option.category_key() == category)
            })
            .ok_or_else(|| build_error(format!("枚举分类 {category} 没有示例标签")))?;
        worksheet.write_string_with_format(row, column, option.label(), format)?;
        return Ok(());
    }
    if field.editor() == LayoutEditor::Boolean {
        worksheet.write_string_with_format(row, column, "是", format)?;
        return Ok(());
    }
    match field.value_format() {
        LayoutValueFormat::Text => {
            let value = if field.editor() == LayoutEditor::Text {
                "示例输入"
            } else {
                "示例"
            };
            worksheet.write_string_with_format(row, column, value, format)?;
        }
        LayoutValueFormat::Integer => {
            worksheet.write_number_with_format(row, column, 1.0, format)?;
        }
        LayoutValueFormat::Decimal => {
            worksheet.write_number_with_format(row, column, 1.25, format)?;
        }
        LayoutValueFormat::Percentage => {
            worksheet.write_number_with_format(row, column, 0.5, format)?;
        }
        LayoutValueFormat::DateTime => {
            worksheet.write_datetime_with_format(row, column, example_time, format)?;
        }
        LayoutValueFormat::Json => {
            worksheet.write_string_with_format(row, column, r#"{"sample":true}"#, format)?;
        }
    }
    Ok(())
}

fn fixed_datetime() -> Result<ExcelDateTime, WorkbookProbeError> {
    Ok(ExcelDateTime::from_ymd(2000, 1, 1)?.and_hms(0, 0, 0.0)?)
}

fn invalid_preview(message: impl Into<String>) -> WorkbookProbeError {
    invalid_generated_workbook("layout-preview.xlsx", message)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::adapters::workbook::load_workbook_layout;
    use crate::application::WorkbookProjectionV4;
    use suzushiro_content_digest::sha256_bytes;

    use super::super::super::super::package::PackageSnapshot;
    use super::super::super::super::rendering::assert_fixed_row_heights;
    use super::{build_layout_preview_workbook_bytes, verify_layout_preview_workbook};

    #[test]
    fn builds_a_deterministic_preview_with_every_registered_item() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let layout = load_workbook_layout(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
            &registry,
        )
        .unwrap();
        let path = Path::new("layout-preview.xlsx");

        let first = build_layout_preview_workbook_bytes(path, &layout).unwrap();
        let second = build_layout_preview_workbook_bytes(path, &layout).unwrap();

        assert_eq!(first.bytes, second.bytes);
        assert_eq!(first.generated_sheets, 8);
        assert_eq!(first.generated_fields, 180);
        assert_eq!(first.example_rows, 403 + 55 + 7);
        assert_eq!(sha256_bytes(&first.bytes).len(), 64);
        assert_fixed_row_heights(&first.bytes, first.generated_sheets);
        verify_layout_preview_workbook(path, &first.bytes, &layout).unwrap();
        let package = PackageSnapshot::from_bytes(&first.bytes, path).unwrap();
        for name in package.entry_names() {
            if name.starts_with("xl/worksheets/")
                && name.ends_with(".xml")
                && !name.contains("/_rels/")
            {
                let xml = std::str::from_utf8(package.part(name).unwrap()).unwrap();
                assert!(
                    !xml.contains("<sheetProtection"),
                    "{name} 不应启用工作表保护"
                );
            }
        }
    }
}

//! 按严格布局渲染数据工作簿的共用工作表、样式、表格和包收尾。

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use rust_xlsxwriter::{
    Color, DataValidation, ExcelDateTime, Format, FormatAlign, FormatBorder, Formula, Note, Table,
    TableColumn, TableStyle, Workbook, Worksheet, quote_sheet_name, row_col_to_cell_absolute,
};

use crate::application::{
    LayoutEditor, LayoutGenerationMode, LayoutHorizontalAlignment, LayoutValueFormat,
    LayoutVerticalAlignment, WORKBOOK_PROJECTION_SCHEMA_VERSION, WorkbookFieldLayout,
    WorkbookLayout, WorkbookLayoutStyle, WorkbookSheetLayout,
};

use super::WorkbookProbeError;
use super::editor::validate_cell_reference;
use super::package::{PackageSnapshot, canonicalize_generated_package, optional_attribute};
use super::worksheet_primitives::{MAX_VALIDATION_ROW, column_read_only_range};

const DATETIME_FORMAT: &str = "yyyy-mm-dd hh:mm:ss";
/// 舰船简短信息和四阶段属性按实际内容适配列宽。
fn uses_content_width(field: &WorkbookFieldLayout) -> bool {
    let key = field.stable_key();
    field.sheet_key() == "loadout_plan"
        && (matches!(
            key,
            "instance_id"
                | "ship_type"
                | "armor_type"
                | "current_stars"
                | "maximum_stars"
                | "level"
                | "maximum_level"
                | "experience_in_level"
                | "total_experience"
                | "next_level_experience"
                | "energy"
                | "intimacy"
                | "intimacy_maximum"
                | "combat_power"
                | "oil_total"
                | "locked"
                | "proposed"
        ) || (key.starts_with("stat_") && key.ends_with("_summary")))
}

/// 建立共享的工作表外观、列宽、隐藏列和表头批注。
pub(in crate::adapters::workbook) fn add_layout_worksheet<'a>(
    workbook: &'a mut Workbook,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
) -> Result<&'a mut Worksheet, WorkbookProbeError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(sheet.display_name())?;
    for (column_index, field) in fields.iter().enumerate() {
        let column = u16::try_from(column_index).map_err(integer_overflow)?;
        if !uses_content_width(field) {
            worksheet.set_column_width(column, f64::from(field.width().hundredths()) / 100.0)?;
        }
        if field.generation() == LayoutGenerationMode::Hidden {
            worksheet.set_column_hidden(column)?;
        }
        let note = Note::new(field.description()).set_author("AzurLaneWorkbook");
        worksheet.insert_note(0, column, &note)?;
    }
    Ok(worksheet)
}

/// 为已写入数据的工作表补齐表格、输入验证和冻结窗格。
pub(in crate::adapters::workbook) fn finish_layout_worksheet(
    worksheet: &mut Worksheet,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    formats: &WorkbookFormats,
    dictionary_ranges: &BTreeMap<String, DictionaryRange>,
    row_count: usize,
) -> Result<(), WorkbookProbeError> {
    for row in 0..=row_count.max(1) {
        worksheet.set_row_height(u32::try_from(row).map_err(integer_overflow)?, 40.0)?;
    }
    add_workbook_table(worksheet, sheet, fields, row_count, &formats.header)?;
    if fields.iter().any(|field| uses_content_width(field)) {
        worksheet.autofit();
        for (index, field) in fields.iter().enumerate() {
            if !uses_content_width(field) {
                worksheet.set_column_width(
                    u16::try_from(index).map_err(integer_overflow)?,
                    f64::from(field.width().hundredths()) / 100.0,
                )?;
            }
        }
    }
    add_input_validations(worksheet, fields, dictionary_ranges)?;
    if let Some(freeze_cell) = sheet.freeze_cell() {
        let coordinate = validate_cell_reference(freeze_cell)?;
        worksheet.set_freeze_panes(
            coordinate.row,
            u16::try_from(coordinate.column).map_err(integer_overflow)?,
        )?;
    }
    Ok(())
}

pub(in crate::adapters::workbook) fn write_dictionary_rows(
    worksheet: &mut Worksheet,
    layout: &WorkbookLayout,
    fields: &[&WorkbookFieldLayout],
    formats: &WorkbookFormats,
) -> Result<usize, WorkbookProbeError> {
    for (row_index, option) in layout.enum_options().iter().enumerate() {
        let row = u32::try_from(row_index + 1).map_err(integer_overflow)?;
        for (column_index, field) in fields.iter().enumerate() {
            let column = u16::try_from(column_index).map_err(integer_overflow)?;
            let format = formats.for_field(field, None)?;
            match field.stable_key() {
                "category_key" => worksheet.write_string_with_format(
                    row,
                    column,
                    option.category_key(),
                    &format,
                )?,
                "stable_value" => worksheet.write_string_with_format(
                    row,
                    column,
                    option.stable_value(),
                    &format,
                )?,
                "display_label" => {
                    worksheet.write_string_with_format(row, column, option.label(), &format)?
                }
                "object_ref" => worksheet.write_blank(row, column, &format)?,
                "description" => worksheet.write_string_with_format(
                    row,
                    column,
                    option.description(),
                    &format,
                )?,
                "layout_hash" => worksheet.write_string_with_format(
                    row,
                    column,
                    layout.content_sha256(),
                    &format,
                )?,
                "order" => worksheet.write_number_with_format(
                    row,
                    column,
                    f64::from(option.order()),
                    &format,
                )?,
                key => return Err(build_error(format!("字典字段 {key} 缺少预览映射"))),
            };
        }
    }
    Ok(layout.enum_options().len())
}

pub(in crate::adapters::workbook) fn write_schema_rows(
    worksheet: &mut Worksheet,
    layout: &WorkbookLayout,
    fields: &[&WorkbookFieldLayout],
    formats: &WorkbookFormats,
    omitted_items: &str,
    schema_values: &SchemaValues<'_>,
) -> Result<usize, WorkbookProbeError> {
    let sheets_by_key: BTreeMap<&str, &WorkbookSheetLayout> = layout
        .sheets()
        .iter()
        .map(|sheet| (sheet.stable_key(), sheet))
        .collect();
    for (row_index, target_field) in layout.fields().iter().enumerate() {
        let row = u32::try_from(row_index + 1).map_err(integer_overflow)?;
        let target_sheet = sheets_by_key
            .get(target_field.sheet_key())
            .copied()
            .ok_or_else(|| {
                build_error(format!("字段 {} 缺少所属工作表", target_field.stable_key()))
            })?;
        let generated_column = generated_column_index(layout, target_sheet, target_field);
        for (column_index, schema_field) in fields.iter().enumerate() {
            let column = u16::try_from(column_index).map_err(integer_overflow)?;
            let format = formats.for_field(schema_field, None)?;
            write_schema_value(
                worksheet,
                row,
                column,
                schema_field.stable_key(),
                layout,
                target_sheet,
                target_field,
                generated_column,
                omitted_items,
                schema_values,
                &format,
            )?;
        }
    }
    Ok(layout.fields().len())
}

#[allow(clippy::too_many_arguments)]
fn write_schema_value(
    worksheet: &mut Worksheet,
    row: u32,
    column: u16,
    key: &str,
    layout: &WorkbookLayout,
    sheet: &WorkbookSheetLayout,
    field: &WorkbookFieldLayout,
    generated_column: Option<u16>,
    omitted_items: &str,
    schema_values: &SchemaValues<'_>,
    format: &Format,
) -> Result<(), WorkbookProbeError> {
    if key == "read_only_range" {
        if field.editor() == LayoutEditor::ReadOnly {
            if let Some(generated_column) = generated_column {
                worksheet.write_string_with_format(
                    row,
                    column,
                    column_read_only_range(generated_column),
                    format,
                )?;
            } else {
                worksheet.write_blank(row, column, format)?;
            }
        } else {
            worksheet.write_blank(row, column, format)?;
        }
        return Ok(());
    }
    let text = match key {
        "layout_hash" => Some(layout.content_sha256()),
        "workbook_hash" => Some(schema_values.workbook_hash),
        "plan_hash" => Some(schema_values.plan_hash),
        "snapshot_hash" => Some(schema_values.snapshot_hash),
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
        "formula_hash" => Some(schema_values.formula_hash),
        "formula_key" => Some(schema_values.formula_key),
        "omitted_items" => Some(omitted_items),
        "sheet_default_filter" => Some(boolean_label(sheet.default_filter())),
        "sheet_required" => Some(boolean_label(sheet.required())),
        "field_wrap" => Some(boolean_label(field.wrap())),
        "field_required" => Some(boolean_label(field.required())),
        _ => None,
    };
    if let Some(text) = text {
        if text.is_empty() {
            worksheet.write_blank(row, column, format)?;
        } else {
            worksheet.write_string_with_format(row, column, text, format)?;
        }
        return Ok(());
    }
    match key {
        "workbook_schema_version" => worksheet.write_number_with_format(
            row,
            column,
            f64::from(WORKBOOK_PROJECTION_SCHEMA_VERSION),
            format,
        )?,
        "layout_schema_version" => worksheet.write_number_with_format(
            row,
            column,
            f64::from(layout.schema_version()),
            format,
        )?,
        "sheet_order" => {
            worksheet.write_number_with_format(row, column, f64::from(sheet.order()), format)?
        }
        "field_order" => {
            worksheet.write_number_with_format(row, column, f64::from(field.order()), format)?
        }
        "field_width_hundredths" => worksheet.write_number_with_format(
            row,
            column,
            f64::from(field.width().hundredths()),
            format,
        )?,
        "column_index" => worksheet.write_number_with_format(
            row,
            column,
            generated_column.map_or(0.0, |value| f64::from(value) + 1.0),
            format,
        )?,
        "created_at" => {
            worksheet.write_datetime_with_format(row, column, schema_values.created_at, format)?
        }
        _ => return Err(build_error(format!("schema 字段 {key} 缺少预览映射"))),
    };
    Ok(())
}

pub(in crate::adapters::workbook) fn generated_column_index(
    layout: &WorkbookLayout,
    sheet: &WorkbookSheetLayout,
    target: &WorkbookFieldLayout,
) -> Option<u16> {
    if sheet.generation() == LayoutGenerationMode::Omitted
        || target.generation() == LayoutGenerationMode::Omitted
    {
        return None;
    }
    layout
        .generated_fields_for_sheet(sheet.stable_key())
        .iter()
        .position(|field| field.stable_key() == target.stable_key())
        .and_then(|index| u16::try_from(index).ok())
}

/// 返回工作表在 OOXML 中使用的稳定表格名称。
pub(in crate::adapters::workbook) fn table_name(sheet: &WorkbookSheetLayout) -> String {
    if sheet.stable_key().starts_with("ship_technology:") {
        format!(
            "AZLW_ship_technology_{}",
            suzushiro_content_digest::sha256_bytes(sheet.stable_key().as_bytes())
        )
    } else {
        format!("AZLW_{}", sheet.stable_key())
    }
}
pub(in crate::adapters::workbook) fn add_workbook_table(
    worksheet: &mut Worksheet,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    row_count: usize,
    header_format: &Format,
) -> Result<(), WorkbookProbeError> {
    if fields.is_empty() {
        return Err(build_error(format!(
            "生成工作表 {} 没有可写字段",
            sheet.stable_key()
        )));
    }
    // Excel 表格至少需要一行数据区；空语义表使用一个全空占位行保持结构一致。
    let last_row = u32::try_from(row_count.max(1)).map_err(integer_overflow)?;
    let last_column = u16::try_from(fields.len() - 1).map_err(integer_overflow)?;
    let columns: Vec<TableColumn> = fields
        .iter()
        .map(|field| {
            TableColumn::new()
                .set_header(field.display_name())
                .set_header_format(header_format)
        })
        .collect();
    let table = Table::new()
        .set_name(table_name(sheet))
        .set_style(TableStyle::None)
        .set_banded_rows(false)
        .set_autofilter(sheet.default_filter())
        .set_columns(&columns);
    worksheet.add_table(0, 0, last_row, last_column, &table)?;
    Ok(())
}

fn add_input_validations(
    worksheet: &mut Worksheet,
    fields: &[&WorkbookFieldLayout],
    dictionary_ranges: &BTreeMap<String, DictionaryRange>,
) -> Result<(), WorkbookProbeError> {
    for (column_index, field) in fields.iter().enumerate() {
        let column = u16::try_from(column_index).map_err(integer_overflow)?;
        let validation = match field.editor() {
            LayoutEditor::Boolean => Some(DataValidation::new().allow_list_strings(&["是", "否"])?),
            LayoutEditor::Enumeration => {
                let category = field.enum_category().ok_or_else(|| {
                    build_error(format!("枚举字段 {} 缺少枚举分类", field.stable_key()))
                })?;
                let source = dictionary_ranges.get(category).ok_or_else(|| {
                    build_error(format!("枚举字段 {} 缺少字典范围", field.stable_key()))
                })?;
                Some(
                    DataValidation::new()
                        .allow_list_formula(Formula::new(format!("={}", source.name))),
                )
            }
            LayoutEditor::ReadOnly | LayoutEditor::Integer | LayoutEditor::Text => None,
        };
        if let Some(validation) = validation {
            worksheet.add_data_validation(1, column, MAX_VALIDATION_ROW, column, &validation)?;
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(in crate::adapters::workbook) struct DictionaryRange {
    name: String,
    first_row: u32,
    last_row: u32,
}

pub(in crate::adapters::workbook) fn dictionary_ranges(
    layout: &WorkbookLayout,
) -> Result<BTreeMap<String, DictionaryRange>, WorkbookProbeError> {
    let mut ranges: BTreeMap<String, DictionaryRange> = BTreeMap::new();
    for (index, option) in layout.enum_options().iter().enumerate() {
        let row = u32::try_from(index + 1).map_err(integer_overflow)?;
        if option.category_key() == "inventory_operation" && option.stable_value() == "keep" {
            continue;
        }
        ranges
            .entry(option.category_key().to_owned())
            .and_modify(|range| range.last_row = row)
            .or_insert_with(|| DictionaryRange {
                name: format!("AZLW_Enum_{}", option.category_key()),
                first_row: row,
                last_row: row,
            });
    }
    Ok(ranges)
}

pub(in crate::adapters::workbook) fn define_dictionary_names(
    workbook: &mut Workbook,
    layout: &WorkbookLayout,
    ranges: &BTreeMap<String, DictionaryRange>,
) -> Result<(), WorkbookProbeError> {
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "dictionaries")
        .ok_or_else(|| build_error("布局缺少 dictionaries 工作表"))?;
    let label_column = layout
        .generated_fields_for_sheet("dictionaries")
        .iter()
        .position(|field| field.stable_key() == "display_label")
        .ok_or_else(|| build_error("字典工作表缺少可写 display_label 字段"))?;
    let label_column = u16::try_from(label_column).map_err(integer_overflow)?;
    for range in ranges.values() {
        let first = row_col_to_cell_absolute(range.first_row, label_column);
        let last = row_col_to_cell_absolute(range.last_row, label_column);
        let formula = format!("={}!{first}:{last}", quote_sheet_name(sheet.display_name()));
        workbook.define_name(&range.name, &formula)?;
    }
    Ok(())
}

pub(in crate::adapters::workbook) struct WorkbookFormats {
    header: Format,
    styles: BTreeMap<String, WorkbookLayoutStyle>,
}

impl WorkbookFormats {
    pub(in crate::adapters::workbook) fn new(
        layout: &WorkbookLayout,
    ) -> Result<Self, WorkbookProbeError> {
        let styles: BTreeMap<String, WorkbookLayoutStyle> = layout
            .styles()
            .iter()
            .cloned()
            .map(|style| (style.stable_key().to_owned(), style))
            .collect();
        for key in [
            "read_only",
            "input",
            "unowned",
            "warning",
            "error",
            "current_state",
        ] {
            if !styles.contains_key(key) {
                return Err(build_error(format!("布局缺少 {key} 样式")));
            }
        }
        let header = Format::new()
            .set_bold()
            .set_font_color(Color::RGB(0x000000))
            .set_background_color(Color::RGB(0xD9EAF7))
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter)
            .set_text_wrap();
        Ok(Self { header, styles })
    }

    pub(in crate::adapters::workbook) fn for_field(
        &self,
        field: &WorkbookFieldLayout,
        override_style: Option<&str>,
    ) -> Result<Format, WorkbookProbeError> {
        let style_key = override_style.unwrap_or_else(|| {
            if field.sheet_key() == "loadout_plan" && field.stable_key() == "technology_bonus" {
                "read_only"
            } else if field.editor() == LayoutEditor::ReadOnly {
                if is_current_state_field(field) {
                    "current_state"
                } else {
                    "read_only"
                }
            } else {
                "input"
            }
        });
        let style = self
            .styles
            .get(style_key)
            .ok_or_else(|| build_error(format!("布局缺少 {style_key} 样式")))?;
        let mut format = Format::new()
            .set_background_color(parse_color(style.background_color())?)
            .set_font_color(parse_color(style.font_color())?)
            .set_border(FormatBorder::Thin)
            .set_num_format(value_number_format(field.value_format()))
            .set_align(horizontal_alignment(style.horizontal_alignment()))
            .set_align(vertical_alignment(style.vertical_alignment()));
        if style.bold() {
            format = format.set_bold();
        }
        if style.wrap() || field.wrap() {
            format = format.set_text_wrap();
        }
        if field.editor() != LayoutEditor::ReadOnly && override_style != Some("unowned") {
            format = format.set_unlocked();
        }
        Ok(format)
    }
}

/// 写入 schema 工作表时使用的工作簿身份与生成时间。
pub(in crate::adapters::workbook) struct SchemaValues<'a> {
    pub(in crate::adapters::workbook) workbook_hash: &'a str,
    pub(in crate::adapters::workbook) plan_hash: &'a str,
    pub(in crate::adapters::workbook) snapshot_hash: &'a str,
    pub(in crate::adapters::workbook) formula_hash: &'a str,
    pub(in crate::adapters::workbook) formula_key: &'a str,
    pub(in crate::adapters::workbook) created_at: &'a ExcelDateTime,
}

fn parse_color(value: &str) -> Result<Color, WorkbookProbeError> {
    u32::from_str_radix(value, 16)
        .map(Color::RGB)
        .map_err(|error| build_error(format!("颜色 {value} 不能转换为 RGB: {error}")))
}

const fn horizontal_alignment(value: LayoutHorizontalAlignment) -> FormatAlign {
    match value {
        LayoutHorizontalAlignment::Left => FormatAlign::Left,
        LayoutHorizontalAlignment::Center => FormatAlign::Center,
        LayoutHorizontalAlignment::Right => FormatAlign::Right,
    }
}

const fn vertical_alignment(value: LayoutVerticalAlignment) -> FormatAlign {
    match value {
        LayoutVerticalAlignment::Top => FormatAlign::Top,
        LayoutVerticalAlignment::Center => FormatAlign::VerticalCenter,
        LayoutVerticalAlignment::Bottom => FormatAlign::Bottom,
    }
}

const fn value_number_format(value: LayoutValueFormat) -> &'static str {
    match value {
        LayoutValueFormat::Text | LayoutValueFormat::Json => "@",
        LayoutValueFormat::Integer => "0",
        LayoutValueFormat::Decimal => "0.00",
        LayoutValueFormat::Percentage => "0.00%",
        LayoutValueFormat::DateTime => DATETIME_FORMAT,
    }
}

const fn boolean_label(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}

pub(in crate::adapters::workbook) fn omitted_items_json(
    layout: &WorkbookLayout,
) -> Result<String, WorkbookProbeError> {
    let mut items: Vec<String> = layout
        .sheets()
        .iter()
        .filter(|sheet| sheet.generation() == LayoutGenerationMode::Omitted)
        .map(|sheet| format!("sheet:{}", sheet.stable_key()))
        .chain(
            layout
                .fields()
                .iter()
                .filter(|field| {
                    field.generation() == LayoutGenerationMode::Omitted
                        || layout.sheets().iter().any(|sheet| {
                            sheet.stable_key() == field.sheet_key()
                                && sheet.generation() == LayoutGenerationMode::Omitted
                        })
                })
                .map(|field| format!("field:{}.{}", field.sheet_key(), field.stable_key())),
        )
        .collect();
    items.sort();
    items.dedup();
    serde_json::to_string(&items).map_err(|error| build_error(format!("编码省略项失败: {error}")))
}

/// workbook.xml 中一张工作表的顺序相关可见状态。
pub(in crate::adapters::workbook) struct WorkbookSheetState {
    pub(in crate::adapters::workbook) name: String,
    pub(in crate::adapters::workbook) hidden: bool,
}

/// 从 workbook.xml 提取有序工作表名称及隐藏状态，供不同产物验证器复用。
pub(in crate::adapters::workbook) fn workbook_sheet_states(
    bytes: &[u8],
    artifact_name: &str,
) -> Result<Vec<WorkbookSheetState>, WorkbookProbeError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut states = Vec::new();
    loop {
        match reader
            .read_event()
            .map_err(|source| invalid_generated_workbook(artifact_name, source.to_string()))?
        {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"sheet" =>
            {
                let name = optional_attribute(&reader, "xl/workbook.xml", element, b"name")?
                    .ok_or_else(|| {
                        invalid_generated_workbook(artifact_name, "工作表缺少 name 属性")
                    })?;
                let state = optional_attribute(&reader, "xl/workbook.xml", element, b"state")?;
                let hidden = match state.as_deref() {
                    None | Some("visible") => false,
                    Some("hidden") => true,
                    Some(value) => {
                        return Err(invalid_generated_workbook(
                            artifact_name,
                            format!("工作表 {name} 的 state {value:?} 不受支持"),
                        ));
                    }
                };
                states.push(WorkbookSheetState { name, hidden });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(states)
}

pub(in crate::adapters::workbook) fn build_error(message: impl Into<String>) -> WorkbookProbeError {
    WorkbookProbeError::WorkbookBuild {
        message: message.into(),
    }
}

/// 建立带具体产物名称的 OOXML 结构错误，避免共享解析器丢失诊断对象。
pub(in crate::adapters::workbook) fn invalid_generated_workbook(
    artifact_name: &str,
    message: impl Into<String>,
) -> WorkbookProbeError {
    WorkbookProbeError::InvalidOoxml {
        part: artifact_name.to_owned(),
        message: message.into(),
    }
}

pub(in crate::adapters::workbook) fn integer_overflow(
    error: impl std::fmt::Display,
) -> WorkbookProbeError {
    build_error(format!("整数转换失败: {error}"))
}

/// 将生成行的磅值精确写入 OOXML，消除写表库内部整数像素量化造成的偏差。
pub(in crate::adapters::workbook) fn finalize_generated_workbook(
    bytes: &[u8],
    path: &Path,
) -> Result<Vec<u8>, WorkbookProbeError> {
    use super::package::rewrite_package_from_bytes;
    use quick_xml::Writer;

    let package = PackageSnapshot::from_bytes(bytes, path)?;
    let mut replacements = BTreeMap::new();
    for name in package
        .entry_names()
        .filter(|name| name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml"))
    {
        let mut reader = Reader::from_reader(package.part(name)?);
        let mut writer = Writer::new(Vec::new());
        loop {
            let mut event = reader
                .read_event()
                .map_err(|error| build_error(format!("读取 {name} 行高失败: {error}")))?;
            match &mut event {
                Event::Start(element) | Event::Empty(element)
                    if element.local_name().as_ref() == b"row" =>
                {
                    let mut replacement = element.to_owned();
                    replacement.clear_attributes();
                    for attribute in element.attributes() {
                        let attribute = attribute.map_err(|error| {
                            build_error(format!("读取 {name} 行属性失败: {error}"))
                        })?;
                        if !matches!(attribute.key.as_ref(), b"ht" | b"customHeight") {
                            replacement.push_attribute(attribute);
                        }
                    }
                    replacement.push_attribute(("ht", "40"));
                    replacement.push_attribute(("customHeight", "1"));
                    *element = replacement;
                }
                Event::Eof => break,
                _ => {}
            }
            writer
                .write_event(event)
                .map_err(|error| build_error(format!("写入 {name} 行高失败: {error}")))?;
        }
        replacements.insert(name.to_owned(), writer.into_inner());
    }
    let output = rewrite_package_from_bytes(
        bytes,
        Cursor::new(Vec::new()),
        path,
        path,
        &replacements,
        &[],
    )?;
    Ok(canonicalize_generated_package(&output.into_inner(), path)?)
}

#[cfg(test)]
pub(in crate::adapters::workbook) fn assert_fixed_row_heights(
    bytes: &[u8],
    expected_sheets: usize,
) {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut sheets = 0;
    for index in 0..archive.len() {
        let mut part = archive.by_index(index).unwrap();
        if !part.name().starts_with("xl/worksheets/sheet") || !part.name().ends_with(".xml") {
            continue;
        }
        sheets += 1;
        let name = part.name().to_owned();
        let mut xml = String::new();
        part.read_to_string(&mut xml).unwrap();
        let mut reader = Reader::from_str(&xml);
        let mut rows = 0;
        loop {
            match reader.read_event().unwrap() {
                Event::Start(element) | Event::Empty(element)
                    if element.local_name().as_ref() == b"row" =>
                {
                    rows += 1;
                    assert_eq!(
                        optional_attribute(&reader, &name, &element, b"ht")
                            .unwrap()
                            .as_deref(),
                        Some("40"),
                        "{name}"
                    );
                    assert_eq!(
                        optional_attribute(&reader, &name, &element, b"customHeight")
                            .unwrap()
                            .as_deref(),
                        Some("1"),
                        "{name}"
                    );
                }
                Event::Eof => break,
                _ => {}
            }
        }
        assert!(rows >= 2, "{name} 必须包含表头和数据或占位行");
    }
    assert_eq!(sheets, expected_sheets);
}

/// 数据字段按语义区分静态资料和会随账号状态刷新的只读现状。
fn is_current_state_field(field: &WorkbookFieldLayout) -> bool {
    let key = field.stable_key();
    match field.sheet_key() {
        "loadout_plan" => {
            key.starts_with("stat_")
                || key.starts_with("skills_")
                || (key.starts_with("slot_") && !key.ends_with("_allowed_equipment_types"))
                || matches!(
                    key,
                    "config_id"
                        | "skin_id"
                        | "fleet_status"
                        | "intimacy_stage"
                        | "read_errors"
                        | "current_stars"
                        | "level"
                        | "experience_in_level"
                        | "total_experience"
                        | "next_level_experience"
                        | "energy"
                        | "proficiency"
                        | "intimacy"
                        | "intimacy_maximum"
                        | "propose_time"
                        | "create_time"
                        | "combat_power"
                        | "oil_start"
                        | "oil_end"
                        | "oil_total"
                        | "learned_skill_count"
                        | "locked"
                        | "proposed"
                        | "data_complete"
                )
        }
        "equipment_inventory" => {
            matches!(
                key,
                "source_ref"
                    | "source_type"
                    | "runtime_id"
                    | "ship_instance_id"
                    | "ship_name"
                    | "current_enhance_level"
                    | "quantity"
                    | "slot_index"
                    | "family_owned_enhance_distribution"
                    | "family_warehouse_quantity"
                    | "family_equipped_quantity"
                    | "family_owned_quantity"
                    | "blueprint_count"
                    | "craftable_by_gold"
                    | "craftable_by_materials"
                    | "craftable_actual"
                    | "family_potential_quantity"
                    | "locked"
                    | "protected"
                    | "dismantlable"
                    | "data_complete"
                    | "read_errors"
            ) || key.starts_with("planned_")
                || key == "simulated_remaining_count"
        }
        "resource_recipes" | "raw_data" => true,
        _ => false,
    }
}

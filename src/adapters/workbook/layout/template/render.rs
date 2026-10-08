//! 负责将布局模板模型渲染为可复现、受保护且带严格下拉约束的 XLSX 包。

use std::collections::BTreeMap;
use std::path::Path;

use rust_xlsxwriter::{
    DataValidation, DocProperties, ExcelDateTime, Format, FormatAlign, FormatBorder, Formula,
    ProtectionOptions, Table, TableColumn, TableStyle, Workbook, Worksheet, row_col_to_cell,
    row_col_to_cell_absolute,
};

use crate::application::LayoutValueFormat;

use super::presentation::editor_label;
use super::{
    ENUM_LABEL_COLUMN, ENUM_TABLE_FIRST_ROW, GENERATION_OPTIONAL_NAME, GENERATION_REQUIRED_NAME,
    LayoutWorkbookTemplate, TABLE_ENUMS, TABLE_FIELDS, TABLE_INFO, TABLE_SHEETS, TABLE_STYLES,
    TemplateEnumOption, TemplateField, TemplateSheet, TemplateStyle, WorkbookProbeError,
    integer_overflow, template_error,
};
use crate::adapters::workbook::layout::{
    ENUM_HEADERS, FIELD_HEADERS, FIELD_SETTINGS, FORMAT_SETTINGS, INFO_HEADERS, SHEET_HEADERS,
    SHEET_SETTINGS, STYLE_HEADERS,
};
use crate::adapters::workbook::package::canonicalize_generated_package;

pub(super) fn build_workbook(
    path: &Path,
    template: &LayoutWorkbookTemplate,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let mut workbook = Workbook::new();
    let creation_time = ExcelDateTime::from_ymd(2000, 1, 1)?;
    let properties = DocProperties::new().set_creation_datetime(&creation_time);
    workbook.set_properties(&properties);

    let formats = TemplateFormats::new();
    let validation_sources = ControlValidationSources::new(&template.enum_options)?;
    write_sheet_settings(
        &mut workbook,
        &template.sheets,
        &formats,
        &validation_sources,
    )?;
    write_field_settings(
        &mut workbook,
        &template.fields,
        &formats,
        &validation_sources,
    )?;
    write_format_settings(&mut workbook, template, &formats)?;
    validation_sources.define_names(&mut workbook)?;
    let bytes = workbook
        .save_to_buffer()
        .map_err(WorkbookProbeError::from)?;
    Ok(canonicalize_generated_package(&bytes, path)?)
}

/// 让配置表下拉直接引用可编辑枚举标签，并按注册约束区分允许值集合。
struct ControlValidationSources {
    generation_required: NamedValidationSource,
    generation_optional: NamedValidationSource,
    value_formats: BTreeMap<LayoutValueFormat, NamedValidationSource>,
}

impl ControlValidationSources {
    fn new(enum_options: &[TemplateEnumOption]) -> Result<Self, WorkbookProbeError> {
        let visible_row = enum_label_row(enum_options, "generation_mode", "visible")?;
        let hidden_row = enum_label_row(enum_options, "generation_mode", "hidden")?;
        let omitted_row = enum_label_row(enum_options, "generation_mode", "omitted")?;
        if hidden_row != visible_row + 1 || omitted_row != hidden_row + 1 {
            return Err(template_error(
                "生成方式枚举必须按 visible、hidden、omitted 连续排列",
            ));
        }

        let mut value_formats = BTreeMap::new();
        for (format, stable_value, name) in [
            (LayoutValueFormat::Text, "text", "AZLW_FormatText"),
            (LayoutValueFormat::Integer, "integer", "AZLW_FormatInteger"),
            (LayoutValueFormat::Decimal, "decimal", "AZLW_FormatDecimal"),
            (
                LayoutValueFormat::Percentage,
                "percentage",
                "AZLW_FormatPercentage",
            ),
            (
                LayoutValueFormat::DateTime,
                "date_time",
                "AZLW_FormatDateTime",
            ),
            (LayoutValueFormat::Json, "json", "AZLW_FormatJson"),
        ] {
            let row = enum_label_row(enum_options, "value_format", stable_value)?;
            value_formats.insert(format, NamedValidationSource::new(name, row, row));
        }

        Ok(Self {
            generation_required: NamedValidationSource::new(
                GENERATION_REQUIRED_NAME,
                visible_row,
                hidden_row,
            ),
            generation_optional: NamedValidationSource::new(
                GENERATION_OPTIONAL_NAME,
                visible_row,
                omitted_row,
            ),
            value_formats,
        })
    }

    fn define_names(&self, workbook: &mut Workbook) -> Result<(), WorkbookProbeError> {
        self.generation_required.define(workbook)?;
        self.generation_optional.define(workbook)?;
        for source in self.value_formats.values() {
            source.define(workbook)?;
        }
        Ok(())
    }

    fn value_format(
        &self,
        value: LayoutValueFormat,
    ) -> Result<&NamedValidationSource, WorkbookProbeError> {
        self.value_formats
            .get(&value)
            .ok_or_else(|| template_error(format!("值格式 {value:?} 缺少下拉命名范围")))
    }
}

/// 一个映射到“格式与下拉”标签单元格的工作簿级命名范围。
struct NamedValidationSource {
    name: &'static str,
    first_row: u32,
    last_row: u32,
}

impl NamedValidationSource {
    const fn new(name: &'static str, first_row: u32, last_row: u32) -> Self {
        Self {
            name,
            first_row,
            last_row,
        }
    }

    fn define(&self, workbook: &mut Workbook) -> Result<(), WorkbookProbeError> {
        let first_cell = row_col_to_cell_absolute(self.first_row, ENUM_LABEL_COLUMN);
        let last_cell = row_col_to_cell_absolute(self.last_row, ENUM_LABEL_COLUMN);
        let range = if first_cell == last_cell {
            first_cell
        } else {
            format!("{first_cell}:{last_cell}")
        };
        workbook.define_name(self.name, &format!("='{FORMAT_SETTINGS}'!{range}"))?;
        Ok(())
    }
}

fn enum_label_row(
    enum_options: &[TemplateEnumOption],
    category: &str,
    stable_value: &str,
) -> Result<u32, WorkbookProbeError> {
    let index = enum_options
        .iter()
        .position(|option| option.category_key == category && option.stable_value == stable_value)
        .ok_or_else(|| {
            template_error(format!("枚举 {category}.{stable_value} 缺少下拉标签来源"))
        })?;
    data_row(ENUM_TABLE_FIRST_ROW, index)
}

struct TemplateFormats {
    header: Format,
    locked_text: Format,
    locked_wrapped: Format,
    locked_integer: Format,
    locked_boolean: Format,
    editable_text: Format,
    editable_wrapped: Format,
    editable_integer: Format,
    editable_decimal: Format,
    editable_boolean: Format,
}

impl TemplateFormats {
    fn new() -> Self {
        let header = Format::new()
            .set_bold()
            .set_font_color("#FFFFFF")
            .set_background_color("#1F4E78")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter)
            .set_text_wrap();
        let locked_text = Format::new()
            .set_num_format("@")
            .set_background_color("#F2F2F2")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        let locked_wrapped = locked_text.clone().set_text_wrap();
        let locked_integer = Format::new()
            .set_num_format("0")
            .set_background_color("#F2F2F2")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        let locked_boolean = Format::new()
            .set_background_color("#F2F2F2")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        let editable_text = Format::new()
            .set_unlocked()
            .set_num_format("@")
            .set_background_color("#E2F0D9")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        let editable_wrapped = editable_text.clone().set_text_wrap();
        let editable_integer = Format::new()
            .set_unlocked()
            .set_num_format("0")
            .set_background_color("#E2F0D9")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        let editable_decimal = Format::new()
            .set_unlocked()
            .set_num_format("0.00")
            .set_background_color("#E2F0D9")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        let editable_boolean = Format::new()
            .set_unlocked()
            .set_background_color("#E2F0D9")
            .set_border(FormatBorder::Thin)
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);

        Self {
            header,
            locked_text,
            locked_wrapped,
            locked_integer,
            locked_boolean,
            editable_text,
            editable_wrapped,
            editable_integer,
            editable_decimal,
            editable_boolean,
        }
    }
}

fn write_sheet_settings(
    workbook: &mut Workbook,
    sheets: &[TemplateSheet],
    formats: &TemplateFormats,
    validation_sources: &ControlValidationSources,
) -> Result<(), WorkbookProbeError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(SHEET_SETTINGS)?;
    worksheet.set_row_height(0, 30.0)?;
    let mut required_generation_rows = Vec::new();
    let mut optional_generation_rows = Vec::new();

    for (index, sheet) in sheets.iter().enumerate() {
        let row = data_row(0, index)?;
        if sheet.required {
            required_generation_rows.push(row);
        } else {
            optional_generation_rows.push(row);
        }
        worksheet.write_string_with_format(row, 0, &sheet.stable_key, &formats.locked_text)?;
        worksheet.write_string_with_format(
            row,
            1,
            &sheet.generation_label,
            &formats.editable_text,
        )?;
        worksheet.write_string_with_format(row, 2, &sheet.display_name, &formats.editable_text)?;
        worksheet.write_number_with_format(
            row,
            3,
            f64::from(sheet.order),
            &formats.editable_integer,
        )?;
        if let Some(freeze_cell) = &sheet.freeze_cell {
            worksheet.write_string_with_format(row, 4, freeze_cell, &formats.editable_text)?;
        } else {
            worksheet.write_blank(row, 4, &formats.editable_text)?;
        }
        worksheet.write_boolean_with_format(
            row,
            5,
            sheet.default_filter,
            &formats.editable_boolean,
        )?;
        worksheet.write_string_with_format(
            row,
            6,
            &sheet.description,
            &formats.editable_wrapped,
        )?;
        worksheet.write_boolean_with_format(row, 7, sheet.required, &formats.locked_boolean)?;
    }

    add_configuration_table(
        worksheet,
        0,
        0,
        TABLE_SHEETS,
        &SHEET_HEADERS,
        sheets.len(),
        &formats.header,
    )?;
    set_column_widths(
        worksheet,
        0,
        &[24.0, 12.0, 20.0, 9.0, 12.0, 12.0, 44.0, 10.0],
    )?;
    let last_row = table_last_row(0, sheets.len())?;
    add_named_list_validation(
        worksheet,
        1,
        &required_generation_rows,
        validation_sources.generation_required.name,
    )?;
    add_named_list_validation(
        worksheet,
        1,
        &optional_generation_rows,
        validation_sources.generation_optional.name,
    )?;
    add_list_validation(worksheet, 5, 1, last_row, &["是", "否"])?;
    finish_configuration_sheet(worksheet)
}

fn write_field_settings(
    workbook: &mut Workbook,
    fields: &[TemplateField],
    formats: &TemplateFormats,
    validation_sources: &ControlValidationSources,
) -> Result<(), WorkbookProbeError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(FIELD_SETTINGS)?;
    worksheet.set_row_height(0, 30.0)?;
    let mut required_generation_rows = Vec::new();
    let mut optional_generation_rows = Vec::new();
    let mut value_format_rows: BTreeMap<LayoutValueFormat, Vec<u32>> = BTreeMap::new();

    for (index, field) in fields.iter().enumerate() {
        let row = data_row(0, index)?;
        if field.required {
            required_generation_rows.push(row);
        } else {
            optional_generation_rows.push(row);
        }
        if field.allowed_format_labels.len() > 1 {
            let labels: Vec<&str> = field
                .allowed_format_labels
                .iter()
                .map(String::as_str)
                .collect();
            add_list_validation(worksheet, 6, row, row, &labels)?;
        } else {
            value_format_rows
                .entry(field.value_format)
                .or_default()
                .push(row);
        }

        worksheet.write_string_with_format(row, 0, &field.sheet_key, &formats.locked_text)?;
        worksheet.write_string_with_format(row, 1, &field.stable_key, &formats.locked_text)?;
        worksheet.write_string_with_format(
            row,
            2,
            &field.generation_label,
            &formats.editable_text,
        )?;
        worksheet.write_string_with_format(row, 3, &field.display_name, &formats.editable_text)?;
        worksheet.write_number_with_format(
            row,
            4,
            f64::from(field.order),
            &formats.editable_integer,
        )?;
        worksheet.write_number_with_format(row, 5, field.width, &formats.editable_decimal)?;
        worksheet.write_string_with_format(
            row,
            6,
            &field.value_format_label,
            &formats.editable_text,
        )?;
        worksheet.write_boolean_with_format(row, 7, field.wrap, &formats.editable_boolean)?;
        worksheet.write_string_with_format(
            row,
            8,
            &field.description,
            &formats.editable_wrapped,
        )?;
        worksheet.write_string_with_format(row, 9, &field.model_path, &formats.locked_text)?;
        worksheet.write_string_with_format(
            row,
            10,
            editor_label(field.editor),
            &formats.locked_text,
        )?;
        worksheet.write_boolean_with_format(row, 11, field.required, &formats.locked_boolean)?;
    }

    add_configuration_table(
        worksheet,
        0,
        0,
        TABLE_FIELDS,
        &FIELD_HEADERS,
        fields.len(),
        &formats.header,
    )?;
    set_column_widths(
        worksheet,
        0,
        &[
            24.0, 30.0, 12.0, 24.0, 9.0, 10.0, 12.0, 9.0, 42.0, 56.0, 12.0, 10.0,
        ],
    )?;
    let last_row = table_last_row(0, fields.len())?;
    add_named_list_validation(
        worksheet,
        2,
        &required_generation_rows,
        validation_sources.generation_required.name,
    )?;
    add_named_list_validation(
        worksheet,
        2,
        &optional_generation_rows,
        validation_sources.generation_optional.name,
    )?;
    for (value_format, rows) in value_format_rows {
        add_named_list_validation(
            worksheet,
            6,
            &rows,
            validation_sources.value_format(value_format)?.name,
        )?;
    }
    add_list_validation(worksheet, 7, 1, last_row, &["是", "否"])?;
    finish_configuration_sheet(worksheet)
}

fn write_format_settings(
    workbook: &mut Workbook,
    template: &LayoutWorkbookTemplate,
    formats: &TemplateFormats,
) -> Result<(), WorkbookProbeError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(FORMAT_SETTINGS)?;
    worksheet.set_row_height(0, 30.0)?;

    write_info_table(worksheet, template, formats)?;
    let enum_start = ENUM_TABLE_FIRST_ROW;
    write_enum_table(worksheet, enum_start, &template.enum_options, formats)?;
    let style_start = enum_start
        .checked_add(u32::try_from(template.enum_options.len()).map_err(integer_overflow)?)
        .and_then(|value| value.checked_add(2))
        .ok_or_else(|| template_error("样式表起始行溢出"))?;
    write_style_table(worksheet, style_start, &template.styles, formats)?;

    set_column_widths(
        worksheet,
        0,
        &[24.0, 32.0, 24.0, 10.0, 48.0, 14.0, 10.0, 48.0],
    )?;
    finish_configuration_sheet(worksheet)
}

fn write_info_table(
    worksheet: &mut Worksheet,
    template: &LayoutWorkbookTemplate,
    formats: &TemplateFormats,
) -> Result<(), WorkbookProbeError> {
    worksheet.write_string_with_format(1, 0, "layout_schema_version", &formats.locked_text)?;
    worksheet.write_number_with_format(
        1,
        1,
        f64::from(template.schema_version),
        &formats.locked_integer,
    )?;
    worksheet.write_string_with_format(1, 2, "布局配置结构版本", &formats.locked_wrapped)?;
    worksheet.write_string_with_format(2, 0, "template_name", &formats.locked_text)?;
    worksheet.write_string_with_format(2, 1, &template.template_name, &formats.editable_wrapped)?;
    worksheet.write_string_with_format(2, 2, "模板显示名称", &formats.locked_wrapped)?;
    worksheet.write_string_with_format(3, 0, "layout_purpose", &formats.locked_text)?;
    worksheet.write_string_with_format(3, 1, &template.purpose, &formats.editable_wrapped)?;
    worksheet.write_string_with_format(3, 2, "布局用途", &formats.locked_wrapped)?;

    add_configuration_table(
        worksheet,
        0,
        0,
        TABLE_INFO,
        &INFO_HEADERS,
        3,
        &formats.header,
    )
}

fn write_enum_table(
    worksheet: &mut Worksheet,
    first_row: u32,
    enum_options: &[TemplateEnumOption],
    formats: &TemplateFormats,
) -> Result<(), WorkbookProbeError> {
    for (index, option) in enum_options.iter().enumerate() {
        let row = data_row(first_row, index)?;
        worksheet.write_string_with_format(row, 0, &option.category_key, &formats.locked_text)?;
        worksheet.write_string_with_format(row, 1, &option.stable_value, &formats.locked_text)?;
        worksheet.write_string_with_format(row, 2, &option.label, &formats.editable_text)?;
        worksheet.write_number_with_format(
            row,
            3,
            f64::from(option.order),
            &formats.editable_integer,
        )?;
        worksheet.write_string_with_format(row, 4, &option.description, &formats.locked_wrapped)?;
    }

    add_configuration_table(
        worksheet,
        first_row,
        0,
        TABLE_ENUMS,
        &ENUM_HEADERS,
        enum_options.len(),
        &formats.header,
    )
}

fn write_style_table(
    worksheet: &mut Worksheet,
    first_row: u32,
    styles: &[TemplateStyle],
    formats: &TemplateFormats,
) -> Result<(), WorkbookProbeError> {
    for (index, style) in styles.iter().enumerate() {
        let row = data_row(first_row, index)?;
        worksheet.write_string_with_format(row, 0, &style.stable_key, &formats.locked_text)?;
        worksheet.write_string_with_format(
            row,
            1,
            &style.background_color,
            &formats.editable_text,
        )?;
        worksheet.write_string_with_format(row, 2, &style.font_color, &formats.editable_text)?;
        worksheet.write_boolean_with_format(row, 3, style.bold, &formats.editable_boolean)?;
        worksheet.write_string_with_format(
            row,
            4,
            &style.horizontal_alignment,
            &formats.editable_text,
        )?;
        worksheet.write_string_with_format(
            row,
            5,
            &style.vertical_alignment,
            &formats.editable_text,
        )?;
        worksheet.write_boolean_with_format(row, 6, style.wrap, &formats.editable_boolean)?;
        worksheet.write_string_with_format(row, 7, &style.description, &formats.locked_wrapped)?;
    }

    add_configuration_table(
        worksheet,
        first_row,
        0,
        TABLE_STYLES,
        &STYLE_HEADERS,
        styles.len(),
        &formats.header,
    )?;
    let first_data_row = first_row
        .checked_add(1)
        .ok_or_else(|| template_error("样式数据起始行溢出"))?;
    let last_row = table_last_row(first_row, styles.len())?;
    add_list_validation(worksheet, 3, first_data_row, last_row, &["是", "否"])?;
    add_list_validation(worksheet, 4, first_data_row, last_row, &["左", "中", "右"])?;
    add_list_validation(worksheet, 5, first_data_row, last_row, &["上", "中", "下"])?;
    add_list_validation(worksheet, 6, first_data_row, last_row, &["是", "否"])
}

fn add_configuration_table<const N: usize>(
    worksheet: &mut Worksheet,
    first_row: u32,
    first_column: u16,
    name: &str,
    headers: &[&str; N],
    row_count: usize,
    header_format: &Format,
) -> Result<(), WorkbookProbeError> {
    if row_count == 0 || N == 0 {
        return Err(template_error(format!("表格 {name} 不能为空")));
    }
    let last_row = table_last_row(first_row, row_count)?;
    let column_count = u16::try_from(N).map_err(integer_overflow)?;
    let last_column = first_column
        .checked_add(column_count - 1)
        .ok_or_else(|| template_error(format!("表格 {name} 列范围溢出")))?;
    let columns: Vec<TableColumn> = headers
        .iter()
        .map(|header| {
            TableColumn::new()
                .set_header(*header)
                .set_header_format(header_format)
        })
        .collect();
    let table = Table::new()
        .set_name(name)
        .set_style(TableStyle::None)
        .set_banded_rows(false)
        .set_columns(&columns);
    worksheet.add_table(first_row, first_column, last_row, last_column, &table)?;
    Ok(())
}

fn add_list_validation(
    worksheet: &mut Worksheet,
    column: u16,
    first_row: u32,
    last_row: u32,
    values: &[&str],
) -> Result<(), WorkbookProbeError> {
    let validation = DataValidation::new().allow_list_strings(values)?;
    worksheet.add_data_validation(first_row, column, last_row, column, &validation)?;
    Ok(())
}

/// 把同一注册约束下的非连续单元格合并为一条命名范围下拉规则。
fn add_named_list_validation(
    worksheet: &mut Worksheet,
    column: u16,
    rows: &[u32],
    source_name: &str,
) -> Result<(), WorkbookProbeError> {
    let Some(first_row) = rows.first().copied() else {
        return Ok(());
    };
    let targets = rows_to_multi_range(column, rows)?;
    let validation = DataValidation::new()
        .allow_list_formula(Formula::new(format!("={source_name}")))
        .set_multi_range(targets);
    worksheet.add_data_validation(first_row, column, first_row, column, &validation)?;
    Ok(())
}

/// 将严格递增的零基行号压缩为 Excel 接受的非连续目标区域。
fn rows_to_multi_range(column: u16, rows: &[u32]) -> Result<String, WorkbookProbeError> {
    let Some(first_row) = rows.first().copied() else {
        return Err(template_error("下拉目标行不能为空"));
    };
    let mut ranges = Vec::new();
    let mut range_start = first_row;
    let mut previous = first_row;
    for row in rows.iter().copied().skip(1) {
        if row <= previous {
            return Err(template_error("下拉目标行必须严格递增"));
        }
        if row != previous + 1 {
            ranges.push(cell_range(column, range_start, previous));
            range_start = row;
        }
        previous = row;
    }
    ranges.push(cell_range(column, range_start, previous));
    Ok(ranges.join(" "))
}

fn cell_range(column: u16, first_row: u32, last_row: u32) -> String {
    let first_cell = row_col_to_cell(first_row, column);
    if first_row == last_row {
        first_cell
    } else {
        format!("{first_cell}:{}", row_col_to_cell(last_row, column))
    }
}

fn set_column_widths(
    worksheet: &mut Worksheet,
    first_column: u16,
    widths: &[f64],
) -> Result<(), WorkbookProbeError> {
    for (offset, width) in widths.iter().enumerate() {
        let column = first_column
            .checked_add(u16::try_from(offset).map_err(integer_overflow)?)
            .ok_or_else(|| template_error("配置表列宽范围溢出"))?;
        worksheet.set_column_width(column, *width)?;
    }
    Ok(())
}

fn finish_configuration_sheet(worksheet: &mut Worksheet) -> Result<(), WorkbookProbeError> {
    worksheet.set_freeze_panes(1, 0)?;
    // 下拉约束按物理配置行绑定，保护状态下禁止排序以免规则与稳定键错位。
    let options = ProtectionOptions {
        format_columns: true,
        use_autofilter: true,
        ..ProtectionOptions::default()
    };
    worksheet.protect_with_options(&options);
    Ok(())
}

fn data_row(first_row: u32, index: usize) -> Result<u32, WorkbookProbeError> {
    first_row
        .checked_add(u32::try_from(index).map_err(integer_overflow)?)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| template_error("配置表数据行溢出"))
}

fn table_last_row(first_row: u32, row_count: usize) -> Result<u32, WorkbookProbeError> {
    first_row
        .checked_add(u32::try_from(row_count).map_err(integer_overflow)?)
        .ok_or_else(|| template_error("配置表行范围溢出"))
}

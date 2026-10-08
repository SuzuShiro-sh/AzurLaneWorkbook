//! 将受控 Excel 表格单元格解析成带来源行号的布局配置记录。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook_from_rs};

use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::editor::validate_cell_reference;
use crate::application::{
    LAYOUT_SCHEMA_VERSION, LayoutColumnWidth, LayoutEditor, LayoutGenerationMode,
    LayoutHorizontalAlignment, LayoutValueFormat, LayoutVerticalAlignment,
};

use super::{
    FIELD_SETTINGS, FORMAT_SETTINGS, LayoutTable, LayoutTableKind, LayoutTables, SHEET_SETTINGS,
    WorkbookLayoutError,
};

type LayoutWorkbook<'a> = Xlsx<Cursor<&'a [u8]>>;

/// 尚未与程序注册表核对的完整布局记录集合。
#[derive(Clone)]
pub(super) struct ParsedLayout {
    pub(super) schema_version: u32,
    pub(super) template_name: String,
    pub(super) purpose: String,
    pub(super) sheets: Vec<ParsedSheet>,
    pub(super) fields: Vec<ParsedField>,
    pub(super) enum_options: Vec<ParsedEnumOption>,
    pub(super) styles: Vec<ParsedStyle>,
    pub(super) control_labels: ControlEnumLabels,
}

/// 一行工作表设置及其 Excel 来源行号。
#[derive(Clone)]
pub(super) struct ParsedSheet {
    pub(super) row: u32,
    pub(super) stable_key: String,
    pub(super) generation: LayoutGenerationMode,
    pub(super) generation_label: String,
    pub(super) display_name: String,
    pub(super) order: u32,
    pub(super) freeze_cell: Option<String>,
    pub(super) default_filter: bool,
    pub(super) description: String,
    pub(super) required: bool,
}

/// 一行字段设置及其 Excel 来源行号。
#[derive(Clone)]
pub(super) struct ParsedField {
    pub(super) row: u32,
    pub(super) sheet_key: String,
    pub(super) stable_key: String,
    pub(super) generation: LayoutGenerationMode,
    pub(super) generation_label: String,
    pub(super) display_name: String,
    pub(super) order: u32,
    pub(super) width: LayoutColumnWidth,
    pub(super) value_format: LayoutValueFormat,
    pub(super) value_format_label: String,
    pub(super) wrap: bool,
    pub(super) description: String,
    pub(super) model_path: String,
    pub(super) editor: LayoutEditor,
    pub(super) required: bool,
}

/// 一行枚举设置及其 Excel 来源行号。
#[derive(Clone)]
pub(super) struct ParsedEnumOption {
    pub(super) row: u32,
    pub(super) category_key: String,
    pub(super) stable_value: String,
    pub(super) label: String,
    pub(super) order: u32,
    pub(super) description: String,
}

/// 一行样式设置及其 Excel 来源行号。
#[derive(Clone)]
pub(super) struct ParsedStyle {
    pub(super) row: u32,
    pub(super) stable_key: String,
    pub(super) background_color: String,
    pub(super) font_color: String,
    pub(super) bold: bool,
    pub(super) horizontal_alignment: LayoutHorizontalAlignment,
    pub(super) vertical_alignment: LayoutVerticalAlignment,
    pub(super) wrap: bool,
    pub(super) description: String,
}

/// 由锁定稳定值和可编辑中文标签建立的布局控制项双向映射。
#[derive(Clone)]
pub(super) struct ControlEnumLabels {
    generation_by_label: BTreeMap<String, LayoutGenerationMode>,
    generation_by_value: BTreeMap<LayoutGenerationMode, String>,
    missing_generation: Vec<String>,
    format_by_label: BTreeMap<String, LayoutValueFormat>,
    format_by_value: BTreeMap<LayoutValueFormat, String>,
    missing_formats: Vec<String>,
}

impl ControlEnumLabels {
    /// 从枚举表建立无歧义的生成方式和值格式映射。
    pub(super) fn new(
        options: &[ParsedEnumOption],
        require_complete: bool,
    ) -> Result<Self, WorkbookLayoutError> {
        let mut labels = Self {
            generation_by_label: BTreeMap::new(),
            generation_by_value: BTreeMap::new(),
            missing_generation: Vec::new(),
            format_by_label: BTreeMap::new(),
            format_by_value: BTreeMap::new(),
            missing_formats: Vec::new(),
        };
        for option in options {
            match option.category_key.as_str() {
                "generation_mode" => {
                    let value =
                        generation_from_stable_value(&option.stable_value).ok_or_else(|| {
                            WorkbookLayoutError::mismatch(
                                FORMAT_SETTINGS,
                                Some(option.row),
                                Some(format!("generation_mode.{}", option.stable_value)),
                                option.stable_value.clone(),
                                "visible、hidden 或 omitted",
                                "生成方式枚举包含程序不支持的稳定值",
                            )
                        })?;
                    insert_control_label(
                        &mut labels.generation_by_label,
                        &mut labels.generation_by_value,
                        option,
                        value,
                    )?;
                }
                "value_format" => {
                    let value =
                        value_format_from_stable_value(&option.stable_value).ok_or_else(|| {
                            WorkbookLayoutError::mismatch(
                                FORMAT_SETTINGS,
                                Some(option.row),
                                Some(format!("value_format.{}", option.stable_value)),
                                option.stable_value.clone(),
                                "text、integer、decimal、percentage、date_time 或 json",
                                "值格式枚举包含程序不支持的稳定值",
                            )
                        })?;
                    insert_control_label(
                        &mut labels.format_by_label,
                        &mut labels.format_by_value,
                        option,
                        value,
                    )?;
                }
                _ => {}
            }
        }

        for value in [
            LayoutGenerationMode::Visible,
            LayoutGenerationMode::Hidden,
            LayoutGenerationMode::Omitted,
        ] {
            if !labels.generation_by_value.contains_key(&value) {
                labels.missing_generation.push(format!(
                    "enum:generation_mode.{}",
                    LayoutGenerationMode::stable_value(value)
                ));
            }
        }
        for value in [
            LayoutValueFormat::Text,
            LayoutValueFormat::Integer,
            LayoutValueFormat::Decimal,
            LayoutValueFormat::Percentage,
            LayoutValueFormat::DateTime,
            LayoutValueFormat::Json,
        ] {
            if !labels.format_by_value.contains_key(&value) {
                labels.missing_formats.push(format!(
                    "enum:value_format.{}",
                    LayoutValueFormat::stable_value(value)
                ));
            }
        }
        let missing: Vec<String> = labels
            .missing_generation
            .iter()
            .chain(&labels.missing_formats)
            .cloned()
            .collect();
        if require_complete && !missing.is_empty() {
            return Err(WorkbookLayoutError::UpgradeRequired { missing });
        }
        Ok(labels)
    }

    /// 将配置单元格中的当前标签解析为稳定生成方式。
    fn parse_generation(
        &self,
        label: &str,
        sheet: &str,
        row: u32,
        key: &str,
    ) -> Result<LayoutGenerationMode, WorkbookLayoutError> {
        if let Some(value) = self.generation_by_label.get(label) {
            return Ok(*value);
        }
        if !self.missing_generation.is_empty() {
            return Err(WorkbookLayoutError::UpgradeRequired {
                missing: self.missing_generation.clone(),
            });
        }
        Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            label,
            self.generation_by_label
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("、"),
            "未知生成方式标签",
        ))
    }

    /// 将配置单元格中的当前标签解析为稳定值格式。
    fn parse_value_format(
        &self,
        label: &str,
        sheet: &str,
        row: u32,
        key: &str,
    ) -> Result<LayoutValueFormat, WorkbookLayoutError> {
        if let Some(value) = self.format_by_label.get(label) {
            return Ok(*value);
        }
        if !self.missing_formats.is_empty() {
            return Err(WorkbookLayoutError::UpgradeRequired {
                missing: self.missing_formats.clone(),
            });
        }
        Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            label,
            self.format_by_label
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("、"),
            "未知值格式标签",
        ))
    }

    /// 返回稳定生成方式在当前布局中的可编辑标签。
    pub(super) fn generation_label(&self, value: LayoutGenerationMode) -> Option<&str> {
        self.generation_by_value.get(&value).map(String::as_str)
    }

    /// 返回稳定值格式在当前布局中的可编辑标签。
    pub(super) fn value_format_label(&self, value: LayoutValueFormat) -> Option<&str> {
        self.format_by_value.get(&value).map(String::as_str)
    }
}

/// 将一条控制枚举同时登记到标签索引和稳定值索引，并拒绝歧义。
fn insert_control_label<T>(
    by_label: &mut BTreeMap<String, T>,
    by_value: &mut BTreeMap<T, String>,
    option: &ParsedEnumOption,
    value: T,
) -> Result<(), WorkbookLayoutError>
where
    T: Copy + Ord + std::fmt::Debug,
{
    if let Some(previous) = by_label.insert(option.label.clone(), value) {
        return Err(WorkbookLayoutError::mismatch(
            FORMAT_SETTINGS,
            Some(option.row),
            Some(format!("{}.{}", option.category_key, option.stable_value)),
            option.label.clone(),
            format!("同一分类内唯一；该标签已经映射到 {previous:?}"),
            "布局控制枚举标签重复",
        ));
    }
    if let Some(previous_label) = by_value.insert(value, option.label.clone()) {
        return Err(WorkbookLayoutError::mismatch(
            FORMAT_SETTINGS,
            Some(option.row),
            Some(format!("{}.{}", option.category_key, option.stable_value)),
            option.stable_value.clone(),
            format!("稳定值唯一；此前标签为 {previous_label}"),
            "布局控制枚举稳定值重复",
        ));
    }
    Ok(())
}

/// 将生成方式稳定值转换为强类型语义。
fn generation_from_stable_value(value: &str) -> Option<LayoutGenerationMode> {
    match value {
        "visible" => Some(LayoutGenerationMode::Visible),
        "hidden" => Some(LayoutGenerationMode::Hidden),
        "omitted" => Some(LayoutGenerationMode::Omitted),
        _ => None,
    }
}

/// 将值格式稳定值转换为强类型语义。
fn value_format_from_stable_value(value: &str) -> Option<LayoutValueFormat> {
    match value {
        "text" => Some(LayoutValueFormat::Text),
        "integer" => Some(LayoutValueFormat::Integer),
        "decimal" => Some(LayoutValueFormat::Decimal),
        "percentage" => Some(LayoutValueFormat::Percentage),
        "date_time" => Some(LayoutValueFormat::DateTime),
        "json" => Some(LayoutValueFormat::Json),
        _ => None,
    }
}

/// 用独立 XLSX 读取器验证工作表和表头后解析全部配置行。
pub(super) fn parse_layout_workbook(
    bytes: &[u8],
    tables: &LayoutTables,
) -> Result<ParsedLayout, WorkbookLayoutError> {
    parse_layout_workbook_with_mode(bytes, tables, false)
}

/// 兼容读取固定表结构，允许迁移层观察旧版本和缺少的注册项。
pub(super) fn parse_layout_workbook_for_upgrade(
    bytes: &[u8],
    tables: &LayoutTables,
) -> Result<ParsedLayout, WorkbookLayoutError> {
    parse_layout_workbook_with_mode(bytes, tables, true)
}

fn parse_layout_workbook_with_mode(
    bytes: &[u8],
    tables: &LayoutTables,
    for_upgrade: bool,
) -> Result<ParsedLayout, WorkbookLayoutError> {
    let mut workbook: LayoutWorkbook<'_> = open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    validate_sheet_set(&workbook)?;

    let sheet_table = required_table(tables, LayoutTableKind::Sheets)?;
    let field_table = required_table(tables, LayoutTableKind::Fields)?;
    let info_table = required_table(tables, LayoutTableKind::Info)?;
    let enum_table = required_table(tables, LayoutTableKind::Enums)?;
    let style_table = required_table(tables, LayoutTableKind::Styles)?;

    let sheet_range = worksheet_range(&mut workbook, SHEET_SETTINGS)?;
    let field_range = worksheet_range(&mut workbook, FIELD_SETTINGS)?;
    let format_range = worksheet_range(&mut workbook, FORMAT_SETTINGS)?;
    validate_header_cells(&sheet_range, sheet_table)?;
    validate_header_cells(&field_range, field_table)?;
    validate_header_cells(&format_range, info_table)?;
    validate_header_cells(&format_range, enum_table)?;
    validate_header_cells(&format_range, style_table)?;

    let (schema_version, template_name, purpose) =
        parse_info(&format_range, info_table, for_upgrade)?;
    let enum_options = parse_enum_options(&format_range, enum_table)?;
    let control_labels = ControlEnumLabels::new(&enum_options, !for_upgrade)?;
    Ok(ParsedLayout {
        schema_version,
        template_name,
        purpose,
        sheets: parse_sheets(&sheet_range, sheet_table, &control_labels)?,
        fields: parse_fields(&field_range, field_table, &control_labels)?,
        enum_options,
        styles: parse_styles(&format_range, style_table)?,
        control_labels,
    })
}

/// 要求工作簿只包含三张固定配置工作表，顺序可以调整。
fn validate_sheet_set(workbook: &LayoutWorkbook<'_>) -> Result<(), WorkbookLayoutError> {
    let actual: Vec<String> = workbook.sheet_names().to_vec();
    let actual_set: BTreeSet<&str> = actual.iter().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [SHEET_SETTINGS, FIELD_SETTINGS, FORMAT_SETTINGS]
        .into_iter()
        .collect();
    if actual.len() != expected.len() || actual_set != expected {
        return Err(WorkbookLayoutError::invalid(
            "xl/workbook.xml",
            None,
            None,
            format!("配置工作簿必须且只能包含 {expected:?}，实际为 {actual:?}"),
        ));
    }
    Ok(())
}

/// 统一把语义读取失败保留为 XLSX 底层错误。
fn worksheet_range(
    workbook: &mut LayoutWorkbook<'_>,
    sheet_name: &str,
) -> Result<calamine::Range<Data>, WorkbookLayoutError> {
    workbook
        .worksheet_range(sheet_name)
        .map_err(|source| WorkbookProbeError::XlsxRead { source }.into())
}

/// 取得已经通过包预检的固定表格。
fn required_table(
    tables: &LayoutTables,
    kind: LayoutTableKind,
) -> Result<&LayoutTable, WorkbookLayoutError> {
    tables.get(&kind).ok_or_else(|| {
        WorkbookLayoutError::invalid(
            kind.sheet_name(),
            None,
            None,
            "内部表格索引缺少已完成预检的配置表",
        )
    })
}

/// 将 OOXML tableColumn 名称与工作表中的实际表头值再次交叉核对。
fn validate_header_cells(
    range: &calamine::Range<Data>,
    table: &LayoutTable,
) -> Result<(), WorkbookLayoutError> {
    for (offset, expected) in table.headers.iter().enumerate() {
        let column = table.first.column
            + u32::try_from(offset).map_err(|_| {
                WorkbookLayoutError::invalid(
                    table.sheet_name,
                    Some(table.first.row + 1),
                    Some(table.name.clone()),
                    "表头列偏移无法在当前平台表示",
                )
            })?;
        match range.get_value((table.first.row, column)) {
            Some(Data::String(actual)) if actual == expected => {}
            Some(actual) => {
                return Err(WorkbookLayoutError::mismatch(
                    table.sheet_name,
                    Some(table.first.row + 1),
                    Some(expected.clone()),
                    format!("{actual:?}"),
                    expected,
                    "表头单元格与固定契约不一致",
                ));
            }
            None => {
                return Err(WorkbookLayoutError::mismatch(
                    table.sheet_name,
                    Some(table.first.row + 1),
                    Some(expected.clone()),
                    "<缺失>",
                    expected,
                    "表头单元格缺失",
                ));
            }
        }
    }
    Ok(())
}

/// 解析三个固定布局信息项，并允许升级层观察不受支持的 schema。
fn parse_info(
    range: &calamine::Range<Data>,
    table: &LayoutTable,
    allow_unsupported_schema: bool,
) -> Result<(u32, String, String), WorkbookLayoutError> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut schema_version: Option<u32> = None;
    let mut schema_row: Option<u32> = None;
    let mut template_name: Option<String> = None;
    let mut purpose: Option<String> = None;
    for row in data_rows(table) {
        let row_number = row + 1;
        let key = required_text(
            cell(range, table, row, 0),
            FORMAT_SETTINGS,
            row_number,
            "配置项稳定键",
        )?;
        if !seen.insert(key.clone()) {
            return Err(WorkbookLayoutError::invalid(
                FORMAT_SETTINGS,
                Some(row_number),
                Some(key),
                "配置项稳定键重复",
            ));
        }
        match key.as_str() {
            "layout_schema_version" => {
                schema_row = Some(row_number);
                schema_version = Some(parse_nonnegative_u32(
                    cell(range, table, row, 1),
                    FORMAT_SETTINGS,
                    row_number,
                    &key,
                )?);
            }
            "template_name" => {
                let value = required_text(
                    cell(range, table, row, 1),
                    FORMAT_SETTINGS,
                    row_number,
                    &key,
                )?;
                validate_layout_info_text(&key, row_number, &value)?;
                template_name = Some(value);
            }
            "layout_purpose" => {
                let value = required_text(
                    cell(range, table, row, 1),
                    FORMAT_SETTINGS,
                    row_number,
                    &key,
                )?;
                validate_layout_info_text(&key, row_number, &value)?;
                purpose = Some(value);
            }
            _ => {
                return Err(WorkbookLayoutError::invalid(
                    FORMAT_SETTINGS,
                    Some(row_number),
                    Some(key),
                    "未知布局配置项",
                ));
            }
        }
        optional_text(
            cell(range, table, row, 2),
            FORMAT_SETTINGS,
            row_number,
            "说明",
        )?;
    }

    let missing: Vec<String> = [
        ("layout_schema_version", schema_version.is_none()),
        ("template_name", template_name.is_none()),
        ("layout_purpose", purpose.is_none()),
    ]
    .into_iter()
    .filter(|(_, missing)| *missing)
    .map(|(key, _)| format!("config:{key}"))
    .collect();
    if !missing.is_empty() {
        return Err(WorkbookLayoutError::UpgradeRequired { missing });
    }
    let schema_version = schema_version.expect("缺失配置已在前面返回");
    if schema_version < LAYOUT_SCHEMA_VERSION && !allow_unsupported_schema {
        return Err(WorkbookLayoutError::upgrade_required_at(
            vec![format!(
                "layout_schema_version:{schema_version}->{LAYOUT_SCHEMA_VERSION}"
            )],
            FORMAT_SETTINGS,
            schema_row.expect("schema 配置存在时必须保留来源行"),
            "layout_schema_version",
            schema_version.to_string(),
            LAYOUT_SCHEMA_VERSION.to_string(),
        ));
    }
    if schema_version > LAYOUT_SCHEMA_VERSION && !allow_unsupported_schema {
        return Err(WorkbookLayoutError::mismatch(
            FORMAT_SETTINGS,
            schema_row,
            Some("layout_schema_version".to_owned()),
            schema_version.to_string(),
            LAYOUT_SCHEMA_VERSION.to_string(),
            "布局 schema 高于程序支持版本",
        ));
    }
    Ok((
        schema_version,
        template_name.expect("缺失配置已在前面返回"),
        purpose.expect("缺失配置已在前面返回"),
    ))
}

/// 拒绝会破坏日志和界面文本边界的布局名称控制字符，并保留配置行号。
fn validate_layout_info_text(key: &str, row: u32, value: &str) -> Result<(), WorkbookLayoutError> {
    if value.chars().any(char::is_control) {
        return Err(WorkbookLayoutError::invalid(
            FORMAT_SETTINGS,
            Some(row),
            Some(key.to_owned()),
            "布局信息不能包含控制字符",
        ));
    }
    Ok(())
}

/// 逐行解析工作表生成方式、显示属性和锁定标记。
fn parse_sheets(
    range: &calamine::Range<Data>,
    table: &LayoutTable,
    control_labels: &ControlEnumLabels,
) -> Result<Vec<ParsedSheet>, WorkbookLayoutError> {
    data_rows(table)
        .map(|row| {
            let row_number = row + 1;
            let stable_key = required_text(
                cell(range, table, row, 0),
                SHEET_SETTINGS,
                row_number,
                "稳定键",
            )?;
            reject_wildcard(SHEET_SETTINGS, row_number, &stable_key, "稳定键")?;
            let generation_label = required_text(
                cell(range, table, row, 1),
                SHEET_SETTINGS,
                row_number,
                "生成方式",
            )?;
            let generation = control_labels.parse_generation(
                &generation_label,
                SHEET_SETTINGS,
                row_number,
                "生成方式",
            )?;
            let freeze_cell = optional_nonempty_text(
                cell(range, table, row, 4),
                SHEET_SETTINGS,
                row_number,
                &stable_key,
            )?;
            if let Some(reference) = &freeze_cell
                && let Err(error) = validate_cell_reference(reference)
            {
                return Err(WorkbookLayoutError::mismatch(
                    SHEET_SETTINGS,
                    Some(row_number),
                    Some(stable_key.clone()),
                    reference,
                    "Excel A1 单元格引用",
                    format!("冻结位置不是有效单元格引用: {error}"),
                ));
            }
            Ok(ParsedSheet {
                row: row_number,
                stable_key,
                generation,
                generation_label,
                display_name: required_text(
                    cell(range, table, row, 2),
                    SHEET_SETTINGS,
                    row_number,
                    "显示名称",
                )?,
                order: parse_positive_u32(
                    cell(range, table, row, 3),
                    SHEET_SETTINGS,
                    row_number,
                    "顺序",
                )?,
                freeze_cell,
                default_filter: parse_bool(
                    cell(range, table, row, 5),
                    SHEET_SETTINGS,
                    row_number,
                    "默认筛选",
                )?,
                description: optional_text(
                    cell(range, table, row, 6),
                    SHEET_SETTINGS,
                    row_number,
                    "说明",
                )?,
                required: parse_bool(
                    cell(range, table, row, 7),
                    SHEET_SETTINGS,
                    row_number,
                    "必需",
                )?,
            })
        })
        .collect()
}

/// 逐行解析字段显示属性、模型路径和锁定编辑契约。
fn parse_fields(
    range: &calamine::Range<Data>,
    table: &LayoutTable,
    control_labels: &ControlEnumLabels,
) -> Result<Vec<ParsedField>, WorkbookLayoutError> {
    data_rows(table)
        .map(|row| {
            let row_number = row + 1;
            let sheet_key = required_text(
                cell(range, table, row, 0),
                FIELD_SETTINGS,
                row_number,
                "工作表稳定键",
            )?;
            let stable_key = required_text(
                cell(range, table, row, 1),
                FIELD_SETTINGS,
                row_number,
                "稳定字段键",
            )?;
            let context_key = format!("{sheet_key}.{stable_key}");
            reject_wildcard(FIELD_SETTINGS, row_number, &sheet_key, &context_key)?;
            reject_wildcard(FIELD_SETTINGS, row_number, &stable_key, &context_key)?;
            let model_path = required_text(
                cell(range, table, row, 9),
                FIELD_SETTINGS,
                row_number,
                &context_key,
            )?;
            reject_wildcard(FIELD_SETTINGS, row_number, &model_path, &context_key)?;
            let generation_label = required_text(
                cell(range, table, row, 2),
                FIELD_SETTINGS,
                row_number,
                &context_key,
            )?;
            let generation = control_labels.parse_generation(
                &generation_label,
                FIELD_SETTINGS,
                row_number,
                &context_key,
            )?;
            let value_format_label = required_text(
                cell(range, table, row, 6),
                FIELD_SETTINGS,
                row_number,
                &context_key,
            )?;
            let value_format = control_labels.parse_value_format(
                &value_format_label,
                FIELD_SETTINGS,
                row_number,
                &context_key,
            )?;
            Ok(ParsedField {
                row: row_number,
                sheet_key,
                stable_key,
                generation,
                generation_label,
                display_name: required_text(
                    cell(range, table, row, 3),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                order: parse_positive_u32(
                    cell(range, table, row, 4),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                width: parse_width(
                    cell(range, table, row, 5),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                value_format,
                value_format_label,
                wrap: parse_bool(
                    cell(range, table, row, 7),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                description: optional_text(
                    cell(range, table, row, 8),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                model_path,
                editor: parse_editor(
                    cell(range, table, row, 10),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                required: parse_bool(
                    cell(range, table, row, 11),
                    FIELD_SETTINGS,
                    row_number,
                    &context_key,
                )?,
            })
        })
        .collect()
}

/// 逐行解析枚举稳定值及其可编辑标签和顺序。
fn parse_enum_options(
    range: &calamine::Range<Data>,
    table: &LayoutTable,
) -> Result<Vec<ParsedEnumOption>, WorkbookLayoutError> {
    data_rows(table)
        .map(|row| {
            let row_number = row + 1;
            let category_key = required_text(
                cell(range, table, row, 0),
                FORMAT_SETTINGS,
                row_number,
                "枚举分类稳定键",
            )?;
            let stable_value = required_text(
                cell(range, table, row, 1),
                FORMAT_SETTINGS,
                row_number,
                "枚举稳定值",
            )?;
            let context_key = format!("{category_key}.{stable_value}");
            reject_wildcard(FORMAT_SETTINGS, row_number, &category_key, &context_key)?;
            reject_wildcard(FORMAT_SETTINGS, row_number, &stable_value, &context_key)?;
            Ok(ParsedEnumOption {
                row: row_number,
                category_key,
                stable_value,
                label: required_text(
                    cell(range, table, row, 2),
                    FORMAT_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                order: parse_positive_u32(
                    cell(range, table, row, 3),
                    FORMAT_SETTINGS,
                    row_number,
                    &context_key,
                )?,
                description: optional_text(
                    cell(range, table, row, 4),
                    FORMAT_SETTINGS,
                    row_number,
                    &context_key,
                )?,
            })
        })
        .collect()
}

/// 逐行解析样式颜色、对齐和换行设置。
fn parse_styles(
    range: &calamine::Range<Data>,
    table: &LayoutTable,
) -> Result<Vec<ParsedStyle>, WorkbookLayoutError> {
    data_rows(table)
        .map(|row| {
            let row_number = row + 1;
            let stable_key = required_text(
                cell(range, table, row, 0),
                FORMAT_SETTINGS,
                row_number,
                "样式稳定键",
            )?;
            reject_wildcard(FORMAT_SETTINGS, row_number, &stable_key, "样式稳定键")?;
            Ok(ParsedStyle {
                row: row_number,
                stable_key: stable_key.clone(),
                background_color: parse_color(
                    cell(range, table, row, 1),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
                font_color: parse_color(
                    cell(range, table, row, 2),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
                bold: parse_bool(
                    cell(range, table, row, 3),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
                horizontal_alignment: parse_horizontal_alignment(
                    cell(range, table, row, 4),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
                vertical_alignment: parse_vertical_alignment(
                    cell(range, table, row, 5),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
                wrap: parse_bool(
                    cell(range, table, row, 6),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
                description: optional_text(
                    cell(range, table, row, 7),
                    FORMAT_SETTINGS,
                    row_number,
                    &stable_key,
                )?,
            })
        })
        .collect()
}

/// 返回跳过表头后的绝对零基数据行范围。
fn data_rows(table: &LayoutTable) -> impl Iterator<Item = u32> {
    (table.first.row + 1)..=table.last.row
}

/// 按表格起始列和相对列偏移读取一个绝对行单元格。
fn cell<'a>(
    range: &'a calamine::Range<Data>,
    table: &LayoutTable,
    row: u32,
    column_offset: u32,
) -> Option<&'a Data> {
    range.get_value((row, table.first.column + column_offset))
}

/// 读取非空且无首尾空白的严格文本。
fn required_text(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<String, WorkbookLayoutError> {
    match value {
        Some(Data::String(value)) if !value.is_empty() && value.trim() == value => {
            Ok(value.clone())
        }
        Some(actual) => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            format!("{actual:?}"),
            "非空且无首尾空白的文本",
            "文本单元格格式无效",
        )),
        None => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            "<缺失>",
            "非空且无首尾空白的文本",
            "文本单元格缺失",
        )),
    }
}

/// 读取允许空白单元格的严格文本。
fn optional_text(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<String, WorkbookLayoutError> {
    match value {
        None | Some(Data::Empty) => Ok(String::new()),
        Some(Data::String(value)) if value.trim() == value => Ok(value.clone()),
        Some(actual) => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            format!("{actual:?}"),
            "无首尾空白的文本或空白",
            "可选文本单元格格式无效",
        )),
    }
}

/// 将空白文本规范为无值，保留非空文本。
fn optional_nonempty_text(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<Option<String>, WorkbookLayoutError> {
    let value = optional_text(value, sheet, row, key)?;
    Ok((!value.is_empty()).then_some(value))
}

/// 接受 Excel 布尔值或模板下拉使用的“是”“否”。
fn parse_bool(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<bool, WorkbookLayoutError> {
    match value {
        Some(Data::Bool(value)) => Ok(*value),
        Some(Data::String(value)) if value == "是" => Ok(true),
        Some(Data::String(value)) if value == "否" => Ok(false),
        Some(actual) => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            format!("{actual:?}"),
            "是、否或 Excel 布尔值",
            "布尔单元格格式无效",
        )),
        None => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            "<缺失>",
            "是、否或 Excel 布尔值",
            "布尔单元格缺失",
        )),
    }
}

/// 读取没有小数部分且大于零的顺序值。
fn parse_positive_u32(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<u32, WorkbookLayoutError> {
    parse_u32(value, sheet, row, key, true)
}

/// 读取允许零的非负整数版本号。
fn parse_nonnegative_u32(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<u32, WorkbookLayoutError> {
    parse_u32(value, sheet, row, key, false)
}

/// 按是否要求正数解析 Excel 整数单元格。
fn parse_u32(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
    positive: bool,
) -> Result<u32, WorkbookLayoutError> {
    let parsed = match value {
        Some(Data::Int(value)) => u32::try_from(*value).ok(),
        Some(Data::Float(value))
            if value.is_finite()
                && *value >= 0.0
                && value.fract() == 0.0
                && *value <= f64::from(u32::MAX) =>
        {
            Some(*value as u32)
        }
        _ => None,
    };
    let parsed = if positive {
        parsed.filter(|value| *value > 0)
    } else {
        parsed
    };
    parsed.ok_or_else(|| {
        let expected = if positive {
            "正整数"
        } else {
            "非负整数"
        };
        WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            format!("{value:?}"),
            expected,
            format!("单元格必须是{expected}"),
        )
    })
}

/// 将 Excel 数值列宽无损转换成百分之一列宽。
fn parse_width(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<LayoutColumnWidth, WorkbookLayoutError> {
    let numeric = match value {
        Some(Data::Int(value)) => Some(*value as f64),
        Some(Data::Float(value)) => Some(*value),
        _ => None,
    };
    let Some(numeric) = numeric.filter(|value| value.is_finite()) else {
        return Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            format!("{value:?}"),
            "0.01 至 255.00 的数值",
            "列宽必须是数值",
        ));
    };
    let scaled = numeric * 100.0;
    let rounded = scaled.round();
    if !(1.0..=25_500.0).contains(&rounded) || (scaled - rounded).abs() > 1e-7 {
        return Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            numeric.to_string(),
            "0.01 至 255.00 且最多两位小数",
            "列宽必须位于 0.01 至 255.00 且最多保留两位小数",
        ));
    }
    LayoutColumnWidth::from_hundredths(rounded as u16).map_err(WorkbookLayoutError::from)
}

/// 将锁定的中文编辑器名称转换为强类型枚举。
fn parse_editor(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<LayoutEditor, WorkbookLayoutError> {
    match required_text(value, sheet, row, key)?.as_str() {
        "只读" => Ok(LayoutEditor::ReadOnly),
        "是非" => Ok(LayoutEditor::Boolean),
        "枚举" => Ok(LayoutEditor::Enumeration),
        "整数" => Ok(LayoutEditor::Integer),
        "文本" => Ok(LayoutEditor::Text),
        actual => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            actual,
            "只读、是非、枚举、整数或文本",
            "未知编辑器",
        )),
    }
}

/// 将固定中文水平对齐名称转换为强类型枚举。
fn parse_horizontal_alignment(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<LayoutHorizontalAlignment, WorkbookLayoutError> {
    match required_text(value, sheet, row, key)?.as_str() {
        "左" => Ok(LayoutHorizontalAlignment::Left),
        "中" => Ok(LayoutHorizontalAlignment::Center),
        "右" => Ok(LayoutHorizontalAlignment::Right),
        actual => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            actual,
            "左、中或右",
            "未知水平对齐方式",
        )),
    }
}

/// 将固定中文垂直对齐名称转换为强类型枚举。
fn parse_vertical_alignment(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<LayoutVerticalAlignment, WorkbookLayoutError> {
    match required_text(value, sheet, row, key)?.as_str() {
        "上" => Ok(LayoutVerticalAlignment::Top),
        "中" => Ok(LayoutVerticalAlignment::Center),
        "下" => Ok(LayoutVerticalAlignment::Bottom),
        actual => Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            actual,
            "上、中或下",
            "未知垂直对齐方式",
        )),
    }
}

/// 校验六位 RGB 并规范化为大写十六进制。
fn parse_color(
    value: Option<&Data>,
    sheet: &str,
    row: u32,
    key: &str,
) -> Result<String, WorkbookLayoutError> {
    let value = required_text(value, sheet, row, key)?;
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            value,
            "六位 RGB 十六进制",
            "颜色必须是六位 RGB 十六进制",
        ));
    }
    Ok(value.to_ascii_uppercase())
}

/// 拒绝稳定键和模型路径中的任意通配表达式。
fn reject_wildcard(
    sheet: &str,
    row: u32,
    value: &str,
    key: &str,
) -> Result<(), WorkbookLayoutError> {
    if value.contains('*') {
        Err(WorkbookLayoutError::mismatch(
            sheet,
            Some(row),
            Some(key.to_owned()),
            value,
            "不包含通配符的稳定值",
            "布局配置不允许通配符",
        ))
    } else {
        Ok(())
    }
}

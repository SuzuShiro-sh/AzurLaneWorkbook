//! 独立解析并核验数据工作簿的 OOXML 表、名称范围、数据验证与物理单元格结构。

use std::collections::{BTreeMap, BTreeSet};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use rust_xlsxwriter::{quote_sheet_name, row_col_to_cell, row_col_to_cell_absolute};

use crate::application::{
    LayoutEditor, LayoutGenerationMode, WorkbookFieldLayout, WorkbookLayout, WorkbookSheetLayout,
};

use super::{WorkbookProbeError, invalid};
use crate::adapters::workbook::editor::validate_cell_reference;
use crate::adapters::workbook::package::{PackageSnapshot, optional_attribute, required_attribute};
use crate::adapters::workbook::rendering::{integer_overflow, table_name};
use crate::adapters::workbook::worksheet_primitives::MAX_VALIDATION_ROW;
use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;

#[derive(Debug)]
pub(super) struct TableDefinition {
    reference: String,
    columns: Vec<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct DataValidationDefinition {
    validation_type: String,
    allow_blank: String,
    show_input_message: String,
    show_error_message: String,
    formula: String,
}

/// 从所有 table 部件提取稳定名称、范围和列名，用于独立结构核验。
pub(super) fn read_table_definitions(
    package: &PackageSnapshot,
) -> Result<BTreeMap<String, TableDefinition>, WorkbookProbeError> {
    let mut definitions = BTreeMap::new();
    for part in package
        .entry_names()
        .filter(|name| name.starts_with("xl/tables/table") && name.ends_with(".xml"))
    {
        let mut reader = Reader::from_reader(package.part(part)?);
        reader.config_mut().trim_text(false);
        let mut table_name = None;
        let mut reference = None;
        let mut columns = Vec::new();
        loop {
            match reader
                .read_event()
                .map_err(|error| invalid(format!("解析 {part} 失败: {error}")))?
            {
                Event::Start(ref element) | Event::Empty(ref element)
                    if element.local_name().as_ref() == b"table" =>
                {
                    table_name = optional_attribute(&reader, part, element, b"name")?;
                    reference = optional_attribute(&reader, part, element, b"ref")?;
                }
                Event::Start(ref element) | Event::Empty(ref element)
                    if element.local_name().as_ref() == b"tableColumn" =>
                {
                    let name = optional_attribute(&reader, part, element, b"name")?
                        .ok_or_else(|| invalid(format!("{part} 的 tableColumn 缺少 name")))?;
                    columns.push(name);
                }
                Event::Eof => break,
                _ => {}
            }
        }
        let table_name = table_name.ok_or_else(|| invalid(format!("{part} 缺少 table name")))?;
        let reference = reference.ok_or_else(|| invalid(format!("{part} 缺少 table ref")))?;
        if definitions
            .insert(table_name.clone(), TableDefinition { reference, columns })
            .is_some()
        {
            return Err(invalid(format!("表格名称 {table_name} 重复")));
        }
    }
    Ok(definitions)
}

/// 核对指定工作表的表格名称、物理范围和布局列顺序。
pub(super) fn verify_table_definition(
    tables: &BTreeMap<String, TableDefinition>,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    row_count: usize,
) -> Result<(), WorkbookProbeError> {
    let name = table_name(sheet);
    let table = tables
        .get(&name)
        .ok_or_else(|| invalid(format!("工作表 {} 缺少表格 {name}", sheet.display_name())))?;
    let last_column = u16::try_from(fields.len() - 1).map_err(integer_overflow)?;
    let last_row = u32::try_from(row_count.max(1)).map_err(integer_overflow)?;
    let expected_reference = format!("A1:{}", row_col_to_cell(last_row, last_column));
    if table.reference != expected_reference {
        return Err(invalid(format!(
            "表格 {name} 范围错误: expected={expected_reference}, actual={}",
            table.reference
        )));
    }
    let expected_columns: Vec<&str> = fields.iter().map(|field| field.display_name()).collect();
    if table.columns.iter().map(String::as_str).collect::<Vec<_>>() != expected_columns {
        return Err(invalid(format!("表格 {name} 的列名与布局不一致")));
    }
    Ok(())
}

/// 核对字典分类的全局命名范围及其标签列边界。
pub(super) fn verify_dictionary_names(
    package: &PackageSnapshot,
    layout: &WorkbookLayout,
) -> Result<(), WorkbookProbeError> {
    let expected = expected_dictionary_names(layout)?;
    let actual = read_defined_names(package)?;
    if actual != expected {
        return Err(invalid(format!(
            "字典命名范围错误: expected={expected:?}, actual={actual:?}"
        )));
    }
    Ok(())
}

/// 从连续枚举分类推导写入器应建立的全局命名范围。
fn expected_dictionary_names(
    layout: &WorkbookLayout,
) -> Result<BTreeMap<String, String>, WorkbookProbeError> {
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "dictionaries")
        .ok_or_else(|| invalid("布局缺少 dictionaries 工作表"))?;
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let label_column = fields
        .iter()
        .position(|field| field.stable_key() == "display_label")
        .ok_or_else(|| invalid("字典工作表缺少可写 display_label 字段"))?;
    let label_column = u16::try_from(label_column).map_err(integer_overflow)?;
    let mut ranges: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for (index, option) in layout.enum_options().iter().enumerate() {
        let row = u32::try_from(index + 1).map_err(integer_overflow)?;
        if option.category_key() == "inventory_operation" && option.stable_value() == "keep" {
            continue;
        }
        ranges
            .entry(option.category_key())
            .and_modify(|range| range.1 = row)
            .or_insert((row, row));
    }
    Ok(ranges
        .into_iter()
        .map(|(category, (first_row, last_row))| {
            let first = row_col_to_cell_absolute(first_row, label_column);
            let last = row_col_to_cell_absolute(last_row, label_column);
            (
                format!("AZLW_Enum_{category}"),
                format!("{}!{first}:{last}", quote_sheet_name(sheet.display_name())),
            )
        })
        .collect())
}

/// 读取 workbook.xml 中的全部命名范围并拒绝空项和重复名称。
fn read_defined_names(
    package: &PackageSnapshot,
) -> Result<BTreeMap<String, String>, WorkbookProbeError> {
    let part = "xl/workbook.xml";
    let mut reader = Reader::from_reader(package.part(part)?);
    reader.config_mut().trim_text(false);
    let mut names = BTreeMap::new();
    loop {
        match reader
            .read_event()
            .map_err(|error| invalid(format!("解析 {part} 失败: {error}")))?
        {
            Event::Start(ref element) if element.local_name().as_ref() == b"definedName" => {
                let name = required_attribute(&reader, part, element, b"name")?;
                let formula = reader
                    .read_text(element.name())
                    .map_err(|error| invalid(format!("读取 {part} 命名范围失败: {error}")))?
                    .decode()
                    .map_err(|error| invalid(format!("解码 {part} 命名范围失败: {error}")))?
                    .into_owned();
                if names.insert(name.clone(), formula).is_some() {
                    return Err(invalid(format!("工作簿命名范围 {name} 重复")));
                }
            }
            Event::Empty(ref element) if element.local_name().as_ref() == b"definedName" => {
                return Err(invalid("工作簿包含没有公式的空命名范围"));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(names)
}

/// 核对每张生成表的可编辑列下拉类型、目标区域和稳定字典公式。
pub(super) fn verify_data_validations(
    package: &PackageSnapshot,
    layout: &WorkbookLayout,
    projection: &crate::application::WorkbookProjectionV4,
    outputs: &[super::technology_views::OutputSheet<'_>],
) -> Result<(), WorkbookProbeError> {
    for output in outputs {
        let sheet = output.layout.as_ref();
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let mut expected = expected_data_validations(&fields)?;
        if sheet.stable_key() == "loadout_plan" {
            for ((row, column), formula) in
                super::equipment_choices::equipment_validations(layout, projection, &fields)?
            {
                expected.insert(
                    row_col_to_cell(row, column),
                    DataValidationDefinition {
                        validation_type: "list".to_owned(),
                        allow_blank: "1".to_owned(),
                        show_input_message: "1".to_owned(),
                        show_error_message: "1".to_owned(),
                        formula,
                    },
                );
            }
        }
        let part = worksheet_part_name(package, sheet.display_name())?;
        let actual = read_data_validations(package, &part)?;
        if actual != expected {
            return Err(invalid(format!(
                "工作表 {} 的数据验证错误: expected={expected:?}, actual={actual:?}",
                sheet.display_name()
            )));
        }
    }
    Ok(())
}

/// 按字段编辑器类型推导每列唯一的数据验证目标和公式。
fn expected_data_validations(
    fields: &[&WorkbookFieldLayout],
) -> Result<BTreeMap<String, DataValidationDefinition>, WorkbookProbeError> {
    let mut validations = BTreeMap::new();
    for (index, field) in fields.iter().enumerate() {
        let formula = match field.editor() {
            LayoutEditor::Boolean => Some("\"是,否\"".to_owned()),
            LayoutEditor::Enumeration => Some(format!(
                "AZLW_Enum_{}",
                field.enum_category().ok_or_else(|| {
                    invalid(format!("枚举字段 {} 缺少枚举分类", field.stable_key()))
                })?
            )),
            LayoutEditor::ReadOnly | LayoutEditor::Integer | LayoutEditor::Text => None,
        };
        let Some(formula) = formula else {
            continue;
        };
        let column = u16::try_from(index).map_err(integer_overflow)?;
        let target = format!(
            "{}:{}",
            row_col_to_cell(1, column),
            row_col_to_cell(MAX_VALIDATION_ROW, column)
        );
        let definition = DataValidationDefinition {
            validation_type: "list".to_owned(),
            allow_blank: "1".to_owned(),
            show_input_message: "1".to_owned(),
            show_error_message: "1".to_owned(),
            formula,
        };
        if validations.insert(target.clone(), definition).is_some() {
            return Err(invalid(format!("数据验证目标区域 {target} 重复")));
        }
    }
    Ok(validations)
}

/// 解析工作表数据验证，要求声明数量、目标区域和公式结构相互一致。
fn read_data_validations(
    package: &PackageSnapshot,
    part: &str,
) -> Result<BTreeMap<String, DataValidationDefinition>, WorkbookProbeError> {
    let mut reader = Reader::from_reader(package.part(part)?);
    reader.config_mut().trim_text(false);
    let mut declared_count = None;
    let mut active: Option<(String, DataValidationDefinition, bool)> = None;
    let mut validations = BTreeMap::new();
    loop {
        match reader
            .read_event()
            .map_err(|error| invalid(format!("解析 {part} 失败: {error}")))?
        {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"dataValidations" =>
            {
                if declared_count.is_some() {
                    return Err(invalid(format!("{part} 包含重复 dataValidations 容器")));
                }
                let count = required_attribute(&reader, part, element, b"count")?
                    .parse::<usize>()
                    .map_err(|error| invalid(format!("{part} 的数据验证数量无效: {error}")))?;
                declared_count = Some(count);
            }
            Event::Start(ref element) if element.local_name().as_ref() == b"dataValidation" => {
                if active.is_some() {
                    return Err(invalid(format!("{part} 包含嵌套数据验证")));
                }
                let target = required_attribute(&reader, part, element, b"sqref")?;
                let definition = DataValidationDefinition {
                    validation_type: required_attribute(&reader, part, element, b"type")?,
                    allow_blank: required_attribute(&reader, part, element, b"allowBlank")?,
                    show_input_message: required_attribute(
                        &reader,
                        part,
                        element,
                        b"showInputMessage",
                    )?,
                    show_error_message: required_attribute(
                        &reader,
                        part,
                        element,
                        b"showErrorMessage",
                    )?,
                    formula: String::new(),
                };
                active = Some((target, definition, false));
            }
            Event::Empty(ref element) if element.local_name().as_ref() == b"dataValidation" => {
                return Err(invalid(format!("{part} 包含没有下拉公式的空数据验证")));
            }
            Event::Start(ref element) if element.local_name().as_ref() == b"formula1" => {
                let (_, definition, formula_seen) = active
                    .as_mut()
                    .ok_or_else(|| invalid(format!("{part} 的 formula1 不属于数据验证")))?;
                if *formula_seen {
                    return Err(invalid(format!("{part} 的数据验证包含重复 formula1")));
                }
                definition.formula = reader
                    .read_text(element.name())
                    .map_err(|error| invalid(format!("读取 {part} 下拉公式失败: {error}")))?
                    .decode()
                    .map_err(|error| invalid(format!("解码 {part} 下拉公式失败: {error}")))?
                    .into_owned();
                *formula_seen = true;
            }
            Event::End(ref element) if element.local_name().as_ref() == b"dataValidation" => {
                let (target, definition, formula_seen) = active
                    .take()
                    .ok_or_else(|| invalid(format!("{part} 的数据验证结束标签没有起始标签")))?;
                if !formula_seen {
                    return Err(invalid(format!("{part} 的数据验证缺少 formula1")));
                }
                if validations.insert(target.clone(), definition).is_some() {
                    return Err(invalid(format!("{part} 的数据验证目标 {target} 重复")));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if active.is_some() {
        return Err(invalid(format!("{part} 的数据验证没有结束")));
    }
    let expected_count = if validations.is_empty() {
        None
    } else {
        Some(validations.len())
    };
    if declared_count != expected_count {
        return Err(invalid(format!(
            "{part} 的数据验证声明数量错误: expected={expected_count:?}, actual={declared_count:?}"
        )));
    }
    Ok(validations)
}

/// 正式状态投影只允许字面值，任何工作表单元格公式都视为产物污染。
pub(in crate::adapters::workbook) fn reject_cell_formulas(
    package: &PackageSnapshot,
) -> Result<(), WorkbookProbeError> {
    for part in package
        .entry_names()
        .filter(|name| name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml"))
    {
        reject_part_cell_formulas(package, part)?;
    }
    Ok(())
}

/// 注册工作表始终只接受字面值；独立用户表不参与可信投影。
pub(in crate::adapters::workbook) fn reject_registered_cell_formulas(
    package: &PackageSnapshot,
    layout: &WorkbookLayout,
) -> Result<(), WorkbookProbeError> {
    for sheet in layout.sheets().iter().filter(|sheet| {
        sheet.generation() != LayoutGenerationMode::Omitted
            && sheet.stable_key() != "ship_technology"
    }) {
        let part = worksheet_part_name(package, sheet.display_name())?;
        reject_part_cell_formulas(package, &part)?;
    }
    Ok(())
}

pub(super) fn reject_part_cell_formulas(
    package: &PackageSnapshot,
    part: &str,
) -> Result<(), WorkbookProbeError> {
    let mut reader = Reader::from_reader(package.part(part)?);
    reader.config_mut().trim_text(false);
    loop {
        match reader
            .read_event()
            .map_err(|error| invalid(format!("解析 {part} 失败: {error}")))?
        {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"f" =>
            {
                return Err(invalid(format!("工作表部件 {part} 包含单元格公式")));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// 核对表头、数据行和格式化空白单元格都真实存在于工作表 XML 中。
pub(in crate::adapters::workbook) fn verify_physical_table_cells(
    package: &PackageSnapshot,
    part: &str,
    row_count: usize,
    column_count: usize,
) -> Result<(), WorkbookProbeError> {
    let expected_rows = row_count
        .max(1)
        .checked_add(1)
        .ok_or_else(|| invalid("工作表物理行数溢出"))?;
    let expected_rows_u32 = u32::try_from(expected_rows).map_err(integer_overflow)?;
    let expected_columns_u32 = u32::try_from(column_count).map_err(integer_overflow)?;
    let expected_cells = expected_rows
        .checked_mul(column_count)
        .ok_or_else(|| invalid("工作表物理单元格数溢出"))?;
    let mut reader = Reader::from_reader(package.part(part)?);
    reader.config_mut().trim_text(false);
    const MAX_WORKSHEET_XML_DEPTH: usize = 1_024;
    let mut depth = 0_usize;
    let mut sheet_data_seen = false;
    let mut sheet_data_depth = None;
    let mut current_row: Option<(u32, usize, usize)> = None;
    let mut current_cell_depth = None;
    let mut rows = BTreeSet::new();
    let mut cells = BTreeSet::new();
    loop {
        match reader
            .read_event()
            .map_err(|error| invalid(format!("解析 {part} 失败: {error}")))?
        {
            Event::Start(ref element) => {
                if depth >= MAX_WORKSHEET_XML_DEPTH {
                    return Err(invalid(format!("{part} 的 XML 嵌套深度超过上限")));
                }
                match element.local_name().as_ref() {
                    // 只认工作表根的直接子项。扩展里的同名节点不属于这次替换的数据区。
                    b"sheetData" if depth == 1 => {
                        if sheet_data_seen || sheet_data_depth.is_some() {
                            return Err(invalid(format!("{part} 包含重复 sheetData")));
                        }
                        sheet_data_seen = true;
                        sheet_data_depth = Some(depth);
                    }
                    b"row" if sheet_data_depth.is_some() => {
                        let sheet_depth = sheet_data_depth.expect("已确认 sheetData 深度");
                        if depth != sheet_depth + 1 || current_row.is_some() {
                            return Err(invalid(format!(
                                "{part} 的 row 不是 sheetData 的直接子元素"
                            )));
                        }
                        let row = register_physical_row(
                            &reader,
                            part,
                            element,
                            expected_rows_u32,
                            &mut rows,
                        )?;
                        current_row = Some((row, depth, 0));
                    }
                    b"c" if sheet_data_depth.is_some() => {
                        let Some((row, row_depth, count)) = current_row.as_mut() else {
                            return Err(invalid(format!("{part} 的 c 不在物理 row 内")));
                        };
                        if depth != *row_depth + 1 || current_cell_depth.is_some() {
                            return Err(invalid(format!("{part} 的 c 不是物理 row 的直接子元素")));
                        }
                        register_physical_cell(
                            &reader,
                            part,
                            element,
                            *row,
                            expected_columns_u32,
                            &mut cells,
                        )?;
                        *count = count
                            .checked_add(1)
                            .ok_or_else(|| invalid(format!("{part} 的物理单元格计数溢出")))?;
                        current_cell_depth = Some(depth);
                    }
                    _ => {}
                }
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid(format!("{part} 的 XML 深度溢出")))?;
            }
            Event::Empty(ref element) => match element.local_name().as_ref() {
                b"sheetData" if depth == 1 => {
                    if sheet_data_seen || sheet_data_depth.is_some() {
                        return Err(invalid(format!("{part} 包含重复 sheetData")));
                    }
                    sheet_data_seen = true;
                }
                b"row" if sheet_data_depth.is_some() => {
                    let sheet_depth = sheet_data_depth.expect("已确认 sheetData 深度");
                    if depth != sheet_depth + 1 || current_row.is_some() {
                        return Err(invalid(format!(
                            "{part} 的 row 不是 sheetData 的直接子元素"
                        )));
                    }
                    let _ = register_physical_row(
                        &reader,
                        part,
                        element,
                        expected_rows_u32,
                        &mut rows,
                    )?;
                    if column_count != 0 {
                        return Err(invalid(format!(
                            "{part} 的空物理行缺少 {column_count} 个单元格"
                        )));
                    }
                }
                b"c" if sheet_data_depth.is_some() => {
                    let Some((row, row_depth, count)) = current_row.as_mut() else {
                        return Err(invalid(format!("{part} 的 c 不在物理 row 内")));
                    };
                    if depth != *row_depth + 1 || current_cell_depth.is_some() {
                        return Err(invalid(format!("{part} 的 c 不是物理 row 的直接子元素")));
                    }
                    register_physical_cell(
                        &reader,
                        part,
                        element,
                        *row,
                        expected_columns_u32,
                        &mut cells,
                    )?;
                    *count = count
                        .checked_add(1)
                        .ok_or_else(|| invalid(format!("{part} 的物理单元格计数溢出")))?;
                }
                _ => {}
            },
            Event::End(ref element) => {
                let element_depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid(format!("{part} 的 XML 结束标签层级无效")))?;
                match element.local_name().as_ref() {
                    b"c" if current_cell_depth == Some(element_depth) => {
                        current_cell_depth = None;
                    }
                    b"c" if sheet_data_depth.is_some() => {
                        return Err(invalid(format!("{part} 的 c 结束层级无效")));
                    }
                    b"row"
                        if current_row
                            .as_ref()
                            .is_some_and(|(_, row_depth, _)| *row_depth == element_depth) =>
                    {
                        let (_, _, count) = current_row.take().expect("已确认物理行存在");
                        if count != column_count {
                            return Err(invalid(format!(
                                "{part} 的物理行单元格数错误: expected={column_count}, actual={count}"
                            )));
                        }
                    }
                    b"row" if sheet_data_depth.is_some() => {
                        return Err(invalid(format!("{part} 的 row 结束层级无效")));
                    }
                    b"sheetData" if sheet_data_depth == Some(element_depth) => {
                        if current_row.is_some() || current_cell_depth.is_some() {
                            return Err(invalid(format!(
                                "{part} 在 sheetData 结束前仍有未闭合元素"
                            )));
                        }
                        sheet_data_depth = None;
                    }
                    b"sheetData" if element_depth == 1 && sheet_data_seen => {
                        return Err(invalid(format!("{part} 的 sheetData 结束层级无效")));
                    }
                    _ => {}
                }
                depth = element_depth;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if depth != 0
        || sheet_data_depth.is_some()
        || current_row.is_some()
        || current_cell_depth.is_some()
    {
        return Err(invalid(format!("{part} 的物理表格 XML 未正常闭合")));
    }
    if !sheet_data_seen || rows.len() != expected_rows {
        return Err(invalid(format!(
            "{part} 的物理行不完整: expected={expected_rows}, actual={}",
            rows.len()
        )));
    }
    for expected_row in 1..=expected_rows_u32 {
        if !rows.contains(&expected_row) {
            return Err(invalid(format!("{part} 缺少物理行 {expected_row}")));
        }
    }
    if cells.len() != expected_cells {
        return Err(invalid(format!(
            "{part} 的物理单元格不完整: expected={expected_cells}, actual={}",
            cells.len()
        )));
    }
    Ok(())
}

/// 登记直接位于 sheetData 下的唯一物理行。
fn register_physical_row(
    reader: &Reader<&[u8]>,
    part: &str,
    element: &BytesStart<'_>,
    expected_rows: u32,
    rows: &mut BTreeSet<u32>,
) -> Result<u32, WorkbookProbeError> {
    let row = required_attribute(reader, part, element, b"r")?
        .parse::<u32>()
        .map_err(|error| invalid(format!("{part} 的物理行号无效: {error}")))?;
    if row == 0 || row > expected_rows || !rows.insert(row) {
        return Err(invalid(format!("{part} 的物理行 {row} 超出预期范围或重复")));
    }
    Ok(row)
}

/// 登记直接位于父行下且引用行号一致的唯一物理单元格。
fn register_physical_cell(
    reader: &Reader<&[u8]>,
    part: &str,
    element: &BytesStart<'_>,
    parent_row: u32,
    expected_columns: u32,
    cells: &mut BTreeSet<(u32, u32)>,
) -> Result<(), WorkbookProbeError> {
    let reference = required_attribute(reader, part, element, b"r")?;
    let coordinate = validate_cell_reference(&reference)?;
    if coordinate.row != parent_row - 1
        || coordinate.column >= expected_columns
        || !cells.insert((coordinate.row, coordinate.column))
    {
        return Err(invalid(format!(
            "{part} 的物理单元格 {reference} 与父行不一致、超出范围或重复"
        )));
    }
    Ok(())
}

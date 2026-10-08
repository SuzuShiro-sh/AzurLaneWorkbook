//! 将结果行定点写入既有数据工作簿，并严格限制 OOXML 部件变化范围。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::ops::Range;
use std::path::Path;

use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook_from_rs};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use rust_xlsxwriter::row_col_to_cell;
use serde::Serialize;

use crate::application::{
    LayoutGenerationMode, WorkbookFieldLayout, WorkbookLayout, WorkbookProjectionRow,
    WorkbookProjectionValue, WorkbookSheetLayout,
};
use suzushiro_content_digest::sha256_bytes;

use super::super::WorkbookProbeError;
use super::super::atomic::edit_workbook_atomically_with_pre_publish;
use super::super::editor::{
    PackagePreservationEvidence, compare_packages, reject_external_content, reject_macro_content,
    write_package_to_new_file,
};
use super::super::package::{
    MAX_PART_BYTES, MAX_RAW_PACKAGE_BYTES, PackageSnapshot, cleanup_created_file,
    optional_attribute, read_bounded_workbook_bytes, required_attribute, write_new_file_bytes,
};
use super::super::projection_writer::{
    EnumLabels, enum_labels, excel_datetime_from_unix_millis, verify_physical_table_cells,
    verify_projection_cell,
};
use super::super::reader::validate_schema_snapshot;
use super::super::sheet_parts::worksheet_table_part_name;
use super::super::worksheet_primitives::{namespace_prefix, qualified_name};
use suzushiro_xlsx_toolkit::paths::validate_new_xlsx_destination;
use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;

mod snapshot;

const EXECUTION_RESULTS_SHEET_KEY: &str = "execution_results";
#[cfg(test)]
const EXECUTION_RESULTS_TABLE_NAME: &str = "AZLW_execution_results";

/// 新文件中的执行结果及包级保真验证证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionResultsWriteEvidence {
    pub sheet_name: String,
    pub worksheet_part: String,
    pub table_part: String,
    pub row_count: usize,
    pub source_package_sha256: String,
    pub output_package_sha256: String,
    pub package: PackagePreservationEvidence,
}

/// 原位写回经过临时包验证和平台原子替换后的证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AtomicExecutionResultsWriteEvidence {
    pub write: ExecutionResultsWriteEvidence,
    pub replacement_method: String,
    pub temporary_file_removed: bool,
}

/// 在同目录临时文件完成全部验证后，原子替换既有数据工作簿。
pub fn write_execution_results_atomically(
    workbook_path: &Path,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
) -> Result<AtomicExecutionResultsWriteEvidence, WorkbookProbeError> {
    write_result_rows_atomically(
        workbook_path,
        None,
        layout,
        rows,
        EXECUTION_RESULTS_SHEET_KEY,
    )
}

/// 仅在源工作簿仍与执行前备份摘要一致时原子写回执行结果。
pub fn write_execution_results_atomically_if_source_matches(
    workbook_path: &Path,
    expected_source_package_sha256: &str,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
) -> Result<AtomicExecutionResultsWriteEvidence, WorkbookProbeError> {
    write_result_rows_atomically(
        workbook_path,
        Some(expected_source_package_sha256),
        layout,
        rows,
        EXECUTION_RESULTS_SHEET_KEY,
    )
}

/// 把结果及独立最终快照作为同一原子产物发布；无最终快照时仅更新结果。
pub(super) fn write_execution_snapshot_atomically_if_source_matches(
    workbook_path: &Path,
    expected_source_package_sha256: &str,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
    final_projection: Option<&crate::application::WorkbookProjectionV4>,
    recorded_at_unix_millis: i64,
) -> Result<AtomicExecutionResultsWriteEvidence, WorkbookProbeError> {
    let Some(projection) = final_projection else {
        return write_execution_results_atomically_if_source_matches(
            workbook_path,
            expected_source_package_sha256,
            layout,
            rows,
        );
    };
    verify_source_package_sha256(workbook_path, expected_source_package_sha256)?;
    let atomic = edit_workbook_atomically_with_pre_publish(
        workbook_path,
        |temporary_path| {
            snapshot::write_snapshot_to_new_file(
                workbook_path,
                temporary_path,
                layout,
                rows,
                projection,
                recorded_at_unix_millis,
            )
        },
        |current| verify_source_package_sha256(current, expected_source_package_sha256),
    )?;
    Ok(AtomicExecutionResultsWriteEvidence {
        write: atomic.value,
        replacement_method: atomic.replacement_method.to_owned(),
        temporary_file_removed: atomic.temporary_file_removed,
    })
}

pub(super) fn write_result_rows_atomically(
    workbook_path: &Path,
    expected_source_package_sha256: Option<&str>,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
    sheet_key: &str,
) -> Result<AtomicExecutionResultsWriteEvidence, WorkbookProbeError> {
    if let Some(expected) = expected_source_package_sha256 {
        verify_source_package_sha256(workbook_path, expected)?;
    }
    let atomic = edit_workbook_atomically_with_pre_publish(
        workbook_path,
        |temporary_path| {
            write_result_rows_to_new_file(workbook_path, temporary_path, layout, rows, sheet_key)
        },
        |current_workbook_path| match expected_source_package_sha256 {
            Some(expected) => verify_source_package_sha256(current_workbook_path, expected),
            None => Ok(()),
        },
    )?;
    Ok(AtomicExecutionResultsWriteEvidence {
        write: atomic.value,
        replacement_method: atomic.replacement_method.to_owned(),
        temporary_file_removed: atomic.temporary_file_removed,
    })
}

fn verify_source_package_sha256(
    workbook_path: &Path,
    expected: &str,
) -> Result<(), WorkbookProbeError> {
    let actual = super::super::document::source_digest(workbook_path, "待写回数据工作簿身份核对")?;
    if actual != expected {
        return Err(WorkbookProbeError::SourceChanged {
            path: workbook_path.to_path_buf(),
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(())
}

/// 只替换执行结果 worksheet 和必要时变化的 table 部件，并写入新的排他目标。
pub fn write_execution_results_to_new_file(
    source_path: &Path,
    destination_path: &Path,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
) -> Result<ExecutionResultsWriteEvidence, WorkbookProbeError> {
    write_result_rows_to_new_file(
        source_path,
        destination_path,
        layout,
        rows,
        EXECUTION_RESULTS_SHEET_KEY,
    )
}

fn write_result_rows_to_new_file(
    source_path: &Path,
    destination_path: &Path,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
    sheet_key: &str,
) -> Result<ExecutionResultsWriteEvidence, WorkbookProbeError> {
    validate_new_xlsx_destination(destination_path)?;
    validate_result_rows(rows, sheet_key)?;
    let (sheet, fields) = result_sheet_layout(layout, sheet_key)?;
    let labels = enum_labels(layout);

    let source_document =
        super::super::document::WorkbookDocument::read(source_path, "待写回数据工作簿")?;
    let source_bytes = source_document.bytes();
    let source_package = source_document.package();
    let worksheet_part = worksheet_part_name(source_package, sheet.display_name())?;
    let table_part = worksheet_table_part_name(source_package, &worksheet_part)?;

    let new_reference = table_reference(rows.len(), fields.len())?;
    let table_edit = rewrite_named_table_part(
        &table_part,
        source_package.part(&table_part)?,
        &fields,
        sheet.default_filter(),
        &new_reference,
        &format!("AZLW_{sheet_key}"),
    )?;
    let source_physical_rows =
        table_data_row_capacity(&table_edit.original_reference, fields.len())?;
    verify_physical_table_cells(
        source_package,
        &worksheet_part,
        source_physical_rows,
        fields.len(),
    )?;
    verify_headers(source_bytes, layout, sheet, &fields)?;
    let worksheet_bytes = rewrite_worksheet_part(
        &worksheet_part,
        source_package.part(&worksheet_part)?,
        &table_edit.original_reference,
        &new_reference,
        &fields,
        rows,
        &labels,
    )?;

    let mut replacements = BTreeMap::new();
    if worksheet_bytes != source_package.part(&worksheet_part)? {
        replacements.insert(worksheet_part.clone(), worksheet_bytes);
    }
    if table_edit.bytes != source_package.part(&table_part)? {
        replacements.insert(table_part.clone(), table_edit.bytes);
    }
    if replacements.is_empty() {
        write_new_file_bytes(destination_path, source_bytes)?;
    } else {
        write_package_to_new_file(source_bytes, source_path, destination_path, &replacements)?;
    }

    let validation = (|| {
        let destination_bytes = read_bounded_workbook_bytes(
            destination_path,
            MAX_RAW_PACKAGE_BYTES,
            "结果写回临时工作簿",
        )?;
        let destination_package =
            PackageSnapshot::from_bytes(&destination_bytes, destination_path)?;
        let expected_changed_parts: Vec<&str> = replacements.keys().map(String::as_str).collect();
        let package = compare_packages(
            source_package,
            &destination_package,
            &expected_changed_parts,
        )?;
        verify_result_rows_snapshot(
            destination_path,
            &destination_bytes,
            layout,
            rows,
            &worksheet_part,
            &table_part,
            sheet_key,
        )?;
        Ok::<_, WorkbookProbeError>((destination_bytes, package))
    })();
    let (destination_bytes, package) = match validation {
        Ok(value) => value,
        Err(operation) => return Err(cleanup_created_file(destination_path, operation)),
    };

    Ok(ExecutionResultsWriteEvidence {
        sheet_name: sheet.display_name().to_owned(),
        worksheet_part,
        table_part,
        row_count: rows.len(),
        source_package_sha256: sha256_bytes(source_bytes),
        output_package_sha256: sha256_bytes(&destination_bytes),
        package,
    })
}

/// 重新通过包关系、物理单元格和独立 XLSX 读取器核对写回结果。
fn verify_result_rows_snapshot(
    path: &Path,
    bytes: &[u8],
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
    expected_worksheet_part: &str,
    expected_table_part: &str,
    sheet_key: &str,
) -> Result<(), WorkbookProbeError> {
    let (sheet, fields) = result_sheet_layout(layout, sheet_key)?;
    let labels = enum_labels(layout);
    let package = PackageSnapshot::from_bytes(bytes, path)?;
    reject_external_content(&package)?;
    reject_macro_content(&package)?;
    let worksheet_part = worksheet_part_name(&package, sheet.display_name())?;
    let table_part = worksheet_table_part_name(&package, &worksheet_part)?;
    if worksheet_part != expected_worksheet_part || table_part != expected_table_part {
        return Err(invalid(
            expected_worksheet_part,
            format!("写回前后目标部件关系发生变化: worksheet={worksheet_part}, table={table_part}"),
        ));
    }

    let expected_reference = table_reference(rows.len(), fields.len())?;
    let table_edit = rewrite_named_table_part(
        &table_part,
        package.part(&table_part)?,
        &fields,
        sheet.default_filter(),
        &expected_reference,
        &format!("AZLW_{sheet_key}"),
    )?;
    if table_edit.original_reference != expected_reference
        || table_edit.bytes != package.part(&table_part)?
    {
        return Err(invalid(&table_part, "写回后的表格范围未稳定落盘"));
    }
    verify_physical_table_cells(&package, &worksheet_part, rows.len(), fields.len())?;
    let expected_worksheet = rewrite_worksheet_part(
        &worksheet_part,
        package.part(&worksheet_part)?,
        &expected_reference,
        &expected_reference,
        &fields,
        rows,
        &labels,
    )?;
    if expected_worksheet != package.part(&worksheet_part)? {
        return Err(invalid(
            &worksheet_part,
            "写回后的工作表行不能通过相同输入稳定重建",
        ));
    }
    verify_projection_values(bytes, layout, sheet, &fields, rows, &labels)
}

/// 要求调用方提供的行仍是生产注册表校验后的完整、唯一、稳定排序结果。
fn validate_result_rows(
    rows: &[WorkbookProjectionRow],
    sheet_key: &str,
) -> Result<(), WorkbookProbeError> {
    let input = rows
        .iter()
        .map(|row| {
            (
                row.object_ref().to_owned(),
                row.values()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        })
        .collect();
    let validated =
        WorkbookProjectionRow::validated_for_sheet(sheet_key, input).map_err(|source| {
            WorkbookProbeError::WorkbookBuild {
                message: format!("{sheet_key} 结果行未通过投影注册表校验: {source}"),
            }
        })?;
    if validated != rows {
        return Err(WorkbookProbeError::WorkbookBuild {
            message: "结果行顺序不是稳定对象引用顺序".to_owned(),
        });
    }
    Ok(())
}

/// 读取当前布局中实际生成的目标工作表和列顺序。
#[cfg(test)]
fn execution_results_layout(
    layout: &WorkbookLayout,
) -> Result<(&WorkbookSheetLayout, Vec<&WorkbookFieldLayout>), WorkbookProbeError> {
    result_sheet_layout(layout, EXECUTION_RESULTS_SHEET_KEY)
}

fn result_sheet_layout<'a>(
    layout: &'a WorkbookLayout,
    sheet_key: &str,
) -> Result<(&'a WorkbookSheetLayout, Vec<&'a WorkbookFieldLayout>), WorkbookProbeError> {
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == sheet_key)
        .ok_or_else(|| WorkbookProbeError::WorkbookBuild {
            message: format!("布局缺少 {sheet_key} 工作表"),
        })?;
    if sheet.generation() == LayoutGenerationMode::Omitted {
        return Err(WorkbookProbeError::WorkbookBuild {
            message: format!("布局省略了 {sheet_key} 工作表，不能写回结果"),
        });
    }
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    if fields.is_empty() {
        return Err(WorkbookProbeError::WorkbookBuild {
            message: format!("{sheet_key} 工作表没有可生成字段"),
        });
    }
    Ok((sheet, fields))
}

/// 使用独立 XLSX 读取器确认目标工作表的显示列与当前布局一致。
fn verify_headers(
    bytes: &[u8],
    layout: &WorkbookLayout,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
) -> Result<(), WorkbookProbeError> {
    let mut workbook: Xlsx<Cursor<&[u8]>> = open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    validate_schema_snapshot(&mut workbook, layout)
        .map_err(|source| invalid("schema", format!("布局快照校验失败: {source}")))?;
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    if range.width() != fields.len() || range.height() == 0 {
        return Err(invalid(
            sheet.display_name(),
            format!(
                "工作表表头范围错误: expected_columns={}, actual_columns={}, height={}",
                fields.len(),
                range.width(),
                range.height()
            ),
        ));
    }
    for (column, field) in fields.iter().enumerate() {
        let column = u32::try_from(column).map_err(integer_overflow)?;
        match range.get_value((0, column)) {
            Some(Data::String(value)) if value == field.display_name() => {}
            actual => {
                return Err(invalid(
                    sheet.display_name(),
                    format!(
                        "字段 {} 表头错误: expected={:?}, actual={actual:?}",
                        field.stable_key(),
                        field.display_name()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 通过 Calamine 逐格核对写回后的显示值和行边界。
fn verify_projection_values(
    bytes: &[u8],
    layout: &WorkbookLayout,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    rows: &[WorkbookProjectionRow],
    labels: &EnumLabels,
) -> Result<(), WorkbookProbeError> {
    verify_headers(bytes, layout, sheet, fields)?;
    let mut workbook: Xlsx<Cursor<&[u8]>> = open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    let maximum_height = rows
        .len()
        .max(1)
        .checked_add(1)
        .ok_or_else(|| invalid(sheet.display_name(), "执行结果语义行数溢出"))?;
    if range.height() > maximum_height {
        return Err(invalid(
            sheet.display_name(),
            format!(
                "工作表包含写回范围之外的语义行: maximum={maximum_height}, actual={}",
                range.height()
            ),
        ));
    }
    for (row_index, row) in rows.iter().enumerate() {
        let excel_row = u32::try_from(row_index + 1).map_err(integer_overflow)?;
        for (column_index, field) in fields.iter().enumerate() {
            let column = u32::try_from(column_index).map_err(integer_overflow)?;
            let value = row.value(field.stable_key()).ok_or_else(|| {
                invalid(
                    sheet.display_name(),
                    format!(
                        "对象 {} 缺少布局字段 {}",
                        row.object_ref(),
                        field.stable_key()
                    ),
                )
            })?;
            verify_projection_cell(
                range.get_value((excel_row, column)),
                value,
                labels,
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

struct TableEdit {
    bytes: Vec<u8>,
    original_reference: String,
}

/// 校验表格名称、列和范围，并只替换根表格与自动筛选的 `ref` 属性。
#[cfg(test)]
fn rewrite_table_part(
    part_name: &str,
    bytes: &[u8],
    fields: &[&WorkbookFieldLayout],
    expected_auto_filter: bool,
    new_reference: &str,
) -> Result<TableEdit, WorkbookProbeError> {
    rewrite_named_table_part(
        part_name,
        bytes,
        fields,
        expected_auto_filter,
        new_reference,
        EXECUTION_RESULTS_TABLE_NAME,
    )
}

/// 复用相同列与范围验证，更新任一已登记投影表的物理行范围。
fn rewrite_named_table_part(
    part_name: &str,
    bytes: &[u8],
    fields: &[&WorkbookFieldLayout],
    expected_auto_filter: bool,
    new_reference: &str,
    expected_table_name: &str,
) -> Result<TableEdit, WorkbookProbeError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut original_reference: Option<String> = None;
    let mut auto_filter_reference: Option<String> = None;
    let mut declared_columns: Option<usize> = None;
    let mut columns: Vec<String> = Vec::new();
    let mut column_identifiers: BTreeSet<u64> = BTreeSet::new();
    let mut replacements: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    loop {
        let event_start = reader_position(&reader, part_name)?;
        let event = reader
            .read_event()
            .map_err(|source| invalid(part_name, source.to_string()))?;
        let event_end = reader_position(&reader, part_name)?;
        let event_is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"table" =>
            {
                if original_reference.is_some() {
                    return Err(invalid(part_name, "table 根元素重复"));
                }
                let name = required_attribute(&reader, part_name, element, b"name")?;
                let display_name = required_attribute(&reader, part_name, element, b"displayName")?;
                if name != expected_table_name || display_name != expected_table_name {
                    return Err(invalid(
                        part_name,
                        format!("目标表格名称错误: name={name}, displayName={display_name}"),
                    ));
                }
                let header_rows =
                    optional_attribute(&reader, part_name, element, b"headerRowCount")?
                        .unwrap_or_else(|| "1".to_owned());
                let total_rows =
                    optional_attribute(&reader, part_name, element, b"totalsRowCount")?
                        .unwrap_or_else(|| "0".to_owned());
                if header_rows != "1" || total_rows != "0" {
                    return Err(invalid(
                        part_name,
                        format!(
                            "执行结果表格只允许一个表头且不能有汇总行: header={header_rows}, totals={total_rows}"
                        ),
                    ));
                }
                original_reference = Some(required_attribute(&reader, part_name, element, b"ref")?);
                replacements.push((
                    event_start..event_end,
                    replace_ref_attribute(part_name, element, new_reference, event_is_empty)?,
                ));
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"autoFilter" =>
            {
                if auto_filter_reference.is_some() {
                    return Err(invalid(part_name, "autoFilter 元素重复"));
                }
                auto_filter_reference =
                    Some(required_attribute(&reader, part_name, element, b"ref")?);
                replacements.push((
                    event_start..event_end,
                    replace_ref_attribute(part_name, element, new_reference, event_is_empty)?,
                ));
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tableColumns" =>
            {
                if declared_columns.is_some() {
                    return Err(invalid(part_name, "tableColumns 元素重复"));
                }
                let count = required_attribute(&reader, part_name, element, b"count")?;
                declared_columns = Some(
                    count
                        .parse()
                        .map_err(|_| invalid(part_name, "tableColumns.count 不是非负整数"))?,
                );
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tableColumn" =>
            {
                let identifier = required_attribute(&reader, part_name, element, b"id")?
                    .parse::<u64>()
                    .map_err(|_| invalid(part_name, "tableColumn.id 不是正整数"))?;
                if identifier == 0 || !column_identifiers.insert(identifier) {
                    return Err(invalid(part_name, "tableColumn.id 必须为唯一正整数"));
                }
                columns.push(required_attribute(&reader, part_name, element, b"name")?);
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if matches!(
                    element.local_name().as_ref(),
                    b"calculatedColumnFormula" | b"totalsRowFormula"
                ) =>
            {
                return Err(invalid(part_name, "执行结果表格不能包含计算列或汇总公式"));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let original_reference =
        original_reference.ok_or_else(|| invalid(part_name, "缺少 table 根元素"))?;
    if expected_auto_filter && auto_filter_reference.as_deref() != Some(original_reference.as_str())
    {
        return Err(invalid(
            part_name,
            format!(
                "table.ref 与 autoFilter.ref 不一致: table={original_reference}, filter={auto_filter_reference:?}"
            ),
        ));
    }
    if !expected_auto_filter && auto_filter_reference.is_some() {
        return Err(invalid(
            part_name,
            "布局已关闭默认筛选，但表格仍包含 autoFilter",
        ));
    }
    if declared_columns != Some(columns.len()) || columns.len() != fields.len() {
        return Err(invalid(
            part_name,
            format!(
                "表格列数量错误: declared={declared_columns:?}, actual={}, expected={}",
                columns.len(),
                fields.len()
            ),
        ));
    }
    for (actual, field) in columns.iter().zip(fields) {
        if actual != field.display_name() {
            return Err(invalid(
                part_name,
                format!(
                    "表格列名错误: field={}, expected={:?}, actual={actual:?}",
                    field.stable_key(),
                    field.display_name()
                ),
            ));
        }
    }
    table_data_row_capacity(&original_reference, fields.len())?;
    let rewritten = if original_reference == new_reference {
        bytes.to_vec()
    } else {
        apply_replacements(part_name, bytes, replacements)?
    };
    Ok(TableEdit {
        bytes: rewritten,
        original_reference,
    })
}

struct WorksheetTemplate {
    dimension: Range<usize>,
    dimension_element: BytesStart<'static>,
    dimension_is_empty: bool,
    data_body: Range<usize>,
    names: WorksheetXmlNames,
    cell_styles: Vec<Option<String>>,
}

struct WorksheetXmlNames {
    row: String,
    cell: String,
    inline_string: String,
    text: String,
    value: String,
}

/// 保留工作表中表头和所有非数据节点，只重建数据行并同步 dimension。
#[allow(clippy::too_many_arguments)]
fn rewrite_worksheet_part(
    part_name: &str,
    bytes: &[u8],
    expected_reference: &str,
    new_reference: &str,
    fields: &[&WorkbookFieldLayout],
    rows: &[WorkbookProjectionRow],
    labels: &EnumLabels,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let template = worksheet_template(part_name, bytes, expected_reference, fields.len())?;
    let dimension = replace_ref_attribute(
        part_name,
        &template.dimension_element,
        new_reference,
        template.dimension_is_empty,
    )?;
    let data_rows = render_data_rows(part_name, &template, fields, rows, labels)?;
    apply_replacements(
        part_name,
        bytes,
        vec![
            (template.dimension, dimension),
            (template.data_body, data_rows),
        ],
    )
}

/// 提取 dimension、表头之后的数据区间和首个数据行的逐列样式。
fn worksheet_template(
    part_name: &str,
    bytes: &[u8],
    expected_reference: &str,
    field_count: usize,
) -> Result<WorksheetTemplate, WorkbookProbeError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut dimension: Option<(Range<usize>, BytesStart<'static>, bool)> = None;
    let mut in_sheet_data = false;
    let mut current_row: Option<u32> = None;
    let mut previous_row: Option<u32> = None;
    let mut header_end: Option<usize> = None;
    let mut data_end: Option<usize> = None;
    let mut row_name: Option<String> = None;
    let mut cell_name: Option<String> = None;
    let mut cell_styles: Vec<Option<String>> = Vec::new();
    loop {
        let event_start = reader_position(&reader, part_name)?;
        let event = reader
            .read_event()
            .map_err(|source| invalid(part_name, source.to_string()))?;
        let event_end = reader_position(&reader, part_name)?;
        let event_is_empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"dimension" =>
            {
                if dimension.is_some() {
                    return Err(invalid(part_name, "dimension 元素重复"));
                }
                let reference = required_attribute(&reader, part_name, element, b"ref")?;
                if reference != expected_reference {
                    return Err(invalid(
                        part_name,
                        format!(
                            "dimension 与表格范围不一致: expected={expected_reference}, actual={reference}"
                        ),
                    ));
                }
                dimension = Some((event_start..event_end, element.to_owned(), event_is_empty));
            }
            Event::Start(ref element) if element.local_name().as_ref() == b"sheetData" => {
                if in_sheet_data || data_end.is_some() {
                    return Err(invalid(part_name, "sheetData 元素重复"));
                }
                in_sheet_data = true;
            }
            Event::Start(ref element)
                if in_sheet_data && element.local_name().as_ref() == b"row" =>
            {
                if current_row.is_some() {
                    return Err(invalid(part_name, "工作表数据行发生嵌套"));
                }
                let row = required_attribute(&reader, part_name, element, b"r")?
                    .parse::<u32>()
                    .map_err(|_| invalid(part_name, "row.r 不是正整数"))?;
                if previous_row.is_some_and(|previous| row <= previous) {
                    return Err(invalid(part_name, "工作表物理行必须按行号严格递增"));
                }
                previous_row = Some(row);
                current_row = Some(row);
                if row == 2 {
                    row_name = Some(qualified_element_name(part_name, element)?);
                }
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if current_row == Some(2) && element.local_name().as_ref() == b"c" =>
            {
                let column = cell_styles.len();
                if column >= field_count {
                    return Err(invalid(part_name, "第二行包含超出布局范围的单元格"));
                }
                let expected = row_col_to_cell(1, u16::try_from(column).map_err(integer_overflow)?);
                let actual = required_attribute(&reader, part_name, element, b"r")?;
                if actual != expected {
                    return Err(invalid(
                        part_name,
                        format!("第二行单元格顺序错误: expected={expected}, actual={actual}"),
                    ));
                }
                let name = qualified_element_name(part_name, element)?;
                if let Some(existing) = &cell_name {
                    if existing != &name {
                        return Err(invalid(part_name, "第二行单元格使用了不一致的命名空间前缀"));
                    }
                } else {
                    cell_name = Some(name);
                }
                cell_styles.push(optional_attribute(&reader, part_name, element, b"s")?);
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if in_sheet_data && element.local_name().as_ref() == b"f" =>
            {
                return Err(invalid(part_name, "执行结果工作表不能包含单元格公式"));
            }
            Event::End(ref element) if element.local_name().as_ref() == b"row" => {
                let row = current_row
                    .take()
                    .ok_or_else(|| invalid(part_name, "row 结束标签没有对应开始标签"))?;
                if row == 1 && header_end.replace(event_end).is_some() {
                    return Err(invalid(part_name, "表头行重复"));
                }
            }
            Event::End(ref element) if element.local_name().as_ref() == b"sheetData" => {
                if !in_sheet_data || current_row.is_some() {
                    return Err(invalid(part_name, "sheetData 结束位置无效"));
                }
                in_sheet_data = false;
                data_end = Some(event_start);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let (dimension, dimension_element, dimension_is_empty) =
        dimension.ok_or_else(|| invalid(part_name, "缺少 dimension 元素"))?;
    let data_body = header_end.ok_or_else(|| invalid(part_name, "缺少第一行表头"))?
        ..data_end.ok_or_else(|| invalid(part_name, "缺少 sheetData 结束标签"))?;
    if data_body.start > data_body.end {
        return Err(invalid(part_name, "表头行位于 sheetData 结束标签之后"));
    }
    if cell_styles.len() != field_count {
        return Err(invalid(
            part_name,
            format!(
                "第二行样式模板列数错误: expected={field_count}, actual={}",
                cell_styles.len()
            ),
        ));
    }
    let row = row_name.ok_or_else(|| invalid(part_name, "第二行缺少 row 元素"))?;
    let cell = cell_name.ok_or_else(|| invalid(part_name, "第二行缺少 c 元素"))?;
    let prefix = namespace_prefix(&cell);
    let inline_string = qualified_name(prefix, "is");
    let text = qualified_name(prefix, "t");
    let value = qualified_name(prefix, "v");
    Ok(WorksheetTemplate {
        dimension,
        dimension_element,
        dimension_is_empty,
        data_body,
        names: WorksheetXmlNames {
            row,
            cell,
            inline_string,
            text,
            value,
        },
        cell_styles,
    })
}

/// 依据强类型投影值重建一行或零结果时的完整空白占位行。
fn render_data_rows(
    part_name: &str,
    template: &WorksheetTemplate,
    fields: &[&WorkbookFieldLayout],
    rows: &[WorkbookProjectionRow],
    labels: &EnumLabels,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let physical_rows = rows.len().max(1);
    let mut writer = Writer::new(Vec::new());
    for row_index in 0..physical_rows {
        let one_based_row = u32::try_from(row_index)
            .map_err(integer_overflow)?
            .checked_add(2)
            .ok_or_else(|| invalid(part_name, "执行结果行号溢出"))?;
        let row_number = one_based_row.to_string();
        let mut row_element = BytesStart::new(template.names.row.as_str());
        row_element.push_attribute(("r", row_number.as_str()));
        writer
            .write_event(Event::Start(row_element))
            .map_err(|source| invalid(part_name, source.to_string()))?;
        for (column_index, field) in fields.iter().enumerate() {
            let column = u16::try_from(column_index).map_err(integer_overflow)?;
            let reference = row_col_to_cell(one_based_row - 1, column);
            let value = rows
                .get(row_index)
                .map(|row| {
                    row.value(field.stable_key()).ok_or_else(|| {
                        invalid(
                            part_name,
                            format!(
                                "对象 {} 缺少布局字段 {}",
                                row.object_ref(),
                                field.stable_key()
                            ),
                        )
                    })
                })
                .transpose()?;
            write_cell(
                &mut writer,
                part_name,
                &template.names,
                &reference,
                template.cell_styles[column_index].as_deref(),
                field,
                value,
                labels,
            )?;
        }
        writer
            .write_event(Event::End(BytesEnd::new(template.names.row.as_str())))
            .map_err(|source| invalid(part_name, source.to_string()))?;
        if u64::try_from(writer.get_ref().len()).unwrap_or(u64::MAX) > MAX_PART_BYTES {
            return Err(invalid(
                part_name,
                format!("执行结果行 XML 超过单部件上限 {MAX_PART_BYTES}"),
            ));
        }
    }
    Ok(writer.into_inner())
}

/// 将一个投影值写为无公式的内联文本、数字、日期或格式化空白单元格。
#[allow(clippy::too_many_arguments)]
fn write_cell(
    writer: &mut Writer<Vec<u8>>,
    part_name: &str,
    names: &WorksheetXmlNames,
    reference: &str,
    style: Option<&str>,
    field: &WorkbookFieldLayout,
    value: Option<&WorkbookProjectionValue>,
    labels: &EnumLabels,
) -> Result<(), WorkbookProbeError> {
    let mut cell = BytesStart::new(names.cell.as_str());
    cell.push_attribute(("r", reference));
    if let Some(style) = style {
        cell.push_attribute(("s", style));
    }
    match value.unwrap_or(&WorkbookProjectionValue::Blank) {
        WorkbookProjectionValue::Blank => writer
            .write_event(Event::Empty(cell))
            .map_err(|source| invalid(part_name, source.to_string())),
        WorkbookProjectionValue::Text(value) | WorkbookProjectionValue::Json(value) => {
            write_inline_text_cell(writer, part_name, names, cell, value)
        }
        WorkbookProjectionValue::Integer(value) => {
            write_number_cell(writer, part_name, names, cell, &value.to_string())
        }
        WorkbookProjectionValue::Decimal(value) => {
            write_number_cell(writer, part_name, names, cell, &value.to_string())
        }
        WorkbookProjectionValue::DateTimeUnixMillis(value) => {
            let serial = excel_datetime_from_unix_millis(*value)?.to_excel();
            write_number_cell(writer, part_name, names, cell, &serial.to_string())
        }
        WorkbookProjectionValue::Boolean(value) => write_inline_text_cell(
            writer,
            part_name,
            names,
            cell,
            if *value { "是" } else { "否" },
        ),
        WorkbookProjectionValue::Enumeration {
            category_key,
            stable_value,
        } => {
            if field.enum_category() != Some(category_key.as_str()) {
                return Err(invalid(
                    part_name,
                    format!(
                        "字段 {} 的枚举分类错误: layout={:?}, value={category_key}",
                        field.stable_key(),
                        field.enum_category()
                    ),
                ));
            }
            let label = labels
                .get(category_key.as_str())
                .and_then(|values| values.get(stable_value.as_str()))
                .ok_or_else(|| {
                    invalid(
                        part_name,
                        format!("找不到枚举显示标签 {category_key}.{stable_value}"),
                    )
                })?;
            write_inline_text_cell(writer, part_name, names, cell, label)
        }
    }
}

fn write_inline_text_cell(
    writer: &mut Writer<Vec<u8>>,
    part_name: &str,
    names: &WorksheetXmlNames,
    mut cell: BytesStart<'_>,
    value: &str,
) -> Result<(), WorkbookProbeError> {
    cell.push_attribute(("t", "inlineStr"));
    writer
        .write_event(Event::Start(cell))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::Start(BytesStart::new(names.inline_string.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    let mut text = BytesStart::new(names.text.as_str());
    text.push_attribute(("xml:space", "preserve"));
    writer
        .write_event(Event::Start(text))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::Text(BytesText::new(value)))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(names.text.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(names.inline_string.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(names.cell.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))
}

fn write_number_cell(
    writer: &mut Writer<Vec<u8>>,
    part_name: &str,
    names: &WorksheetXmlNames,
    cell: BytesStart<'_>,
    value: &str,
) -> Result<(), WorkbookProbeError> {
    writer
        .write_event(Event::Start(cell))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::Start(BytesStart::new(names.value.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::Text(BytesText::new(value)))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(names.value.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(names.cell.as_str())))
        .map_err(|source| invalid(part_name, source.to_string()))
}

/// 保留元素名称和非 ref 属性，仅替换范围属性。
fn replace_ref_attribute(
    part_name: &str,
    element: &BytesStart<'_>,
    reference: &str,
    empty: bool,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let mut replacement = element.to_owned();
    replacement.clear_attributes();
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|source| invalid(part_name, source.to_string()))?;
        if attribute.key.local_name().as_ref() != b"ref" {
            replacement.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    replacement.push_attribute(("ref", reference));
    let mut writer = Writer::new(Vec::new());
    writer
        .write_event(if empty {
            Event::Empty(replacement)
        } else {
            Event::Start(replacement)
        })
        .map_err(|source| invalid(part_name, source.to_string()))?;
    Ok(writer.into_inner())
}

/// 按源字节偏移应用互不相交的局部替换。
fn apply_replacements(
    part_name: &str,
    source: &[u8],
    mut replacements: Vec<(Range<usize>, Vec<u8>)>,
) -> Result<Vec<u8>, WorkbookProbeError> {
    replacements.sort_by_key(|(range, _)| range.start);
    let mut cursor = 0_usize;
    let mut output = Vec::with_capacity(source.len());
    for (range, bytes) in replacements {
        if range.start < cursor || range.end < range.start || range.end > source.len() {
            return Err(invalid(part_name, "XML 局部替换范围重叠或越界"));
        }
        output.extend_from_slice(&source[cursor..range.start]);
        output.extend_from_slice(&bytes);
        cursor = range.end;
    }
    output.extend_from_slice(&source[cursor..]);
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > MAX_PART_BYTES {
        return Err(invalid(
            part_name,
            format!("重写后的 XML 超过单部件上限 {MAX_PART_BYTES}"),
        ));
    }
    Ok(output)
}

/// 根据语义行数和列数生成包含空表占位行的 Excel 表格范围。
fn table_reference(row_count: usize, column_count: usize) -> Result<String, WorkbookProbeError> {
    let last_row = u32::try_from(row_count.max(1)).map_err(integer_overflow)?;
    let last_column = column_count
        .checked_sub(1)
        .ok_or_else(|| invalid("worksheet", "结果表格没有列"))?;
    let last_column = u16::try_from(last_column).map_err(integer_overflow)?;
    Ok(format!("A1:{}", row_col_to_cell(last_row, last_column)))
}

/// 校验既有表格从 A1 开始、列宽精确匹配，并返回物理数据行容量。
fn table_data_row_capacity(
    reference: &str,
    column_count: usize,
) -> Result<usize, WorkbookProbeError> {
    let (first, last) = reference.split_once(':').ok_or_else(|| {
        invalid(
            "worksheet",
            format!("表格范围 {reference:?} 不是 A1:B2 形式"),
        )
    })?;
    if first != "A1" || last.contains(':') {
        return Err(invalid(
            "worksheet",
            format!("结果表格必须从 A1 开始: {reference}"),
        ));
    }
    let coordinate = super::super::editor::validate_cell_reference(last)?;
    let expected_column = u32::try_from(
        column_count
            .checked_sub(1)
            .ok_or_else(|| invalid("worksheet", "结果表格没有列"))?,
    )
    .map_err(integer_overflow)?;
    if coordinate.column != expected_column || coordinate.row < 1 {
        return Err(invalid(
            "worksheet",
            format!(
                "结果表格范围与布局不一致: reference={reference}, expected_last_column={expected_column}"
            ),
        ));
    }
    usize::try_from(coordinate.row).map_err(integer_overflow)
}

fn qualified_element_name(
    part_name: &str,
    element: &BytesStart<'_>,
) -> Result<String, WorkbookProbeError> {
    std::str::from_utf8(element.name().as_ref())
        .map(str::to_owned)
        .map_err(|source| invalid(part_name, source.to_string()))
}

fn reader_position(reader: &Reader<&[u8]>, part_name: &str) -> Result<usize, WorkbookProbeError> {
    usize::try_from(reader.buffer_position())
        .map_err(|_| invalid(part_name, "XML 偏移无法在当前平台表示"))
}

fn integer_overflow(error: impl std::fmt::Display) -> WorkbookProbeError {
    invalid("worksheet", format!("Excel 行列整数转换失败: {error}"))
}

fn invalid(part: impl Into<String>, message: impl Into<String>) -> WorkbookProbeError {
    WorkbookProbeError::InvalidOoxml {
        part: part.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;

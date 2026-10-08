//! 将独立最终快照与执行结果合并到原包，并在原子发布前验证语义及保留证据。

use std::collections::BTreeMap;
use std::path::Path;

use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::editor::{compare_packages, write_package_to_new_file};
use crate::adapters::workbook::package::{
    MAX_PART_BYTES, MAX_RAW_PACKAGE_BYTES, PackageSnapshot, cleanup_created_file,
    read_bounded_workbook_bytes,
};
use crate::adapters::workbook::projection_writer::{
    build_refreshed_sheet_donor, reject_registered_cell_formulas,
    verify_refreshed_projection_workbook,
};
use crate::adapters::workbook::sheet_parts::{rename_workbook_sheets, worksheet_table_part_name};
use crate::application::{WorkbookLayout, WorkbookProjectionRow, WorkbookProjectionV4};
use suzushiro_content_digest::sha256_bytes;
use suzushiro_xlsx_toolkit::paths::validate_new_xlsx_destination;
use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;

use super::{ExecutionResultsWriteEvidence, invalid, rewrite_named_table_part, table_reference};
use projection_merge::{REFRESHED_SHEETS, merge_projection};
use xml_merge::{merge_shared_strings, merge_styles, merge_worksheet};

mod projection_merge;
mod xml_merge;

pub(super) fn write_snapshot_to_new_file(
    source_path: &Path,
    destination_path: &Path,
    layout: &WorkbookLayout,
    rows: &[WorkbookProjectionRow],
    projection: &WorkbookProjectionV4,
    recorded_at_unix_millis: i64,
) -> Result<ExecutionResultsWriteEvidence, WorkbookProbeError> {
    validate_new_xlsx_destination(destination_path)?;
    super::validate_result_rows(rows, super::EXECUTION_RESULTS_SHEET_KEY)?;
    let source_document = crate::adapters::workbook::document::WorkbookDocument::read(
        source_path,
        "执行后状态写回源工作簿",
    )?;
    let source_bytes = source_document.bytes();
    let source = source_document.package();
    reject_registered_cell_formulas(source, layout)?;
    let projection = merge_projection(source_bytes, layout, projection, rows)?;
    let bindings =
        crate::adapters::workbook::projection_writer::technology_views::source_sheet_bindings(
            source_bytes,
            source,
            layout,
            &projection,
        )?;
    let generated = build_refreshed_sheet_donor(
        destination_path,
        layout,
        &projection,
        recorded_at_unix_millis,
        REFRESHED_SHEETS,
    )?;
    let generated = PackageSnapshot::from_bytes(&generated.bytes, destination_path)?;
    let (strings, string_mapping) = merge_shared_strings(
        source.part("xl/sharedStrings.xml")?,
        generated.part("xl/sharedStrings.xml")?,
    )?;
    let (styles, mapping) = merge_styles(
        source.part("xl/styles.xml")?,
        generated.part("xl/styles.xml")?,
    )?;
    let mut replacements = BTreeMap::new();
    let names: BTreeMap<_, _> = bindings
        .iter()
        .filter(|(expected, actual)| expected != actual)
        .map(|(expected, actual)| (actual.clone(), expected.clone()))
        .collect();
    if !names.is_empty() {
        replacements.insert(
            "xl/workbook.xml".to_owned(),
            rename_workbook_sheets(source.part("xl/workbook.xml")?, &names)?,
        );
    }
    if strings != source.part("xl/sharedStrings.xml")? {
        replacements.insert("xl/sharedStrings.xml".to_owned(), strings);
    }
    if styles != source.part("xl/styles.xml")? {
        replacements.insert("xl/styles.xml".to_owned(), styles);
    }
    let outputs = crate::adapters::workbook::projection_writer::technology_views::output_sheets(
        layout,
        &projection,
    )?;
    for output in outputs.iter().filter(|output| {
        REFRESHED_SHEETS.contains(&crate::application::technology_template_key(
            output.layout.stable_key(),
        ))
    }) {
        let sheet = output.layout.as_ref();
        let source_part = worksheet_part_name(source, &bindings[sheet.display_name()])?;
        let generated_part = worksheet_part_name(&generated, sheet.display_name())?;
        let worksheet = merge_worksheet(
            source.part(&source_part)?,
            generated.part(&generated_part)?,
            &string_mapping,
            &mapping,
        )?;
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let row_count = if sheet.stable_key() == "schema" {
            layout.fields().len()
        } else if sheet.stable_key() == "dictionaries" {
            layout.enum_options().len()
                + projection
                    .sheet("dictionaries")
                    .ok_or_else(|| invalid("dictionaries", "缺少字典投影"))?
                    .rows()
                    .len()
        } else {
            output.projection.rows().len()
        };
        let table_part = worksheet_table_part_name(source, &source_part)?;
        let table = rewrite_named_table_part(
            &table_part,
            source.part(&table_part)?,
            &fields,
            sheet.default_filter(),
            &table_reference(row_count, fields.len())?,
            &crate::adapters::workbook::rendering::table_name(sheet),
        )?;
        if worksheet != source.part(&source_part)? {
            replacements.insert(source_part, worksheet);
        }
        if table.bytes != source.part(&table_part)? {
            replacements.insert(table_part, table.bytes);
        }
    }
    if replacements
        .values()
        .any(|bytes| bytes.len() as u64 > MAX_PART_BYTES)
    {
        return Err(invalid("snapshot", "更新部件超过工作簿单部件大小上限"));
    }
    write_package_to_new_file(source_bytes, source_path, destination_path, &replacements)?;
    let validation = (|| {
        let bytes = read_bounded_workbook_bytes(
            destination_path,
            MAX_RAW_PACKAGE_BYTES,
            "执行后状态临时工作簿",
        )?;
        let package = PackageSnapshot::from_bytes(&bytes, destination_path)?;
        let allowed: Vec<_> = replacements.keys().map(String::as_str).collect();
        let preservation = compare_packages(source, &package, &allowed)?;
        verify_refreshed_projection_workbook(destination_path, &bytes, layout, &projection)?;
        let sheet = layout
            .sheets()
            .iter()
            .find(|sheet| sheet.stable_key() == "execution_results")
            .ok_or_else(|| invalid("execution_results", "布局缺少结果表"))?;
        let worksheet_part = worksheet_part_name(&package, sheet.display_name())?;
        let table_part = worksheet_table_part_name(&package, &worksheet_part)?;
        Ok(ExecutionResultsWriteEvidence {
            sheet_name: sheet.display_name().to_owned(),
            worksheet_part,
            table_part,
            row_count: rows.len(),
            source_package_sha256: sha256_bytes(source_bytes),
            output_package_sha256: sha256_bytes(&bytes),
            package: preservation,
        })
    })();
    validation.map_err(|error| cleanup_created_file(destination_path, error))
}

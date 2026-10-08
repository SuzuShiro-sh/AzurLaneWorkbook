//! 覆盖执行结果工作表写入、校验与原子发布的单元测试。

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook};
use rust_xlsxwriter::row_col_to_cell;

use super::{
    apply_replacements, execution_results_layout, replace_ref_attribute, rewrite_table_part,
    table_reference, worksheet_template, write_execution_results_atomically,
    write_execution_results_atomically_if_source_matches, write_execution_results_to_new_file,
};
use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state;
use crate::adapters::workbook::package::{PackageSnapshot, rewrite_package_from_bytes};
use crate::adapters::workbook::projection_writer::{
    build_projection_workbook_bytes, verify_physical_table_cells,
};
use crate::adapters::workbook::reference::inspection::{
    worksheet_part_name, worksheet_table_part_name,
};
use crate::adapters::workbook::{edit_text_cell_to_new_file, load_workbook_layout};
use crate::application::test_support::execution_workbook_report_fixture;
use crate::application::{
    WorkbookLayout, WorkbookProjectionRow, WorkbookProjectionV4, project_execution_report_rows,
    project_game_state_to_workbook,
};
use suzushiro_content_digest::sha256_bytes;

const GENERATED_AT_UNIX_MILLIS: i64 = 1_700_000_000_123;
static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn atomically_rewrites_only_the_resolved_execution_parts() {
    let directory = TestDirectory::new("atomic");
    let layout = default_layout();
    let workbook_path = directory.path().join("plan.xlsx");
    let source_bytes = write_generated_workbook(&workbook_path, &layout);
    let source_package = PackageSnapshot::from_bytes(&source_bytes, &workbook_path).unwrap();
    let (sheet, _) = execution_results_layout(&layout).unwrap();
    let worksheet_part = worksheet_part_name(&source_package, sheet.display_name()).unwrap();
    let table_part = worksheet_table_part_name(&source_package, &worksheet_part).unwrap();
    assert_ne!(worksheet_part, "xl/worksheets/sheet1.xml");
    let rows = execution_rows(GENERATED_AT_UNIX_MILLIS);

    #[cfg(unix)]
    let original_mode = {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&workbook_path, fs::Permissions::from_mode(0o640)).unwrap();
        fs::metadata(&workbook_path).unwrap().permissions().mode() & 0o777
    };
    let evidence = write_execution_results_atomically(&workbook_path, &layout, &rows).unwrap();

    let output_bytes = fs::read(&workbook_path).unwrap();
    let mut expected_changed_parts = vec![worksheet_part.clone(), table_part.clone()];
    expected_changed_parts.sort();
    assert_eq!(evidence.write.sheet_name, sheet.display_name());
    assert_eq!(evidence.write.worksheet_part, worksheet_part);
    assert_eq!(evidence.write.table_part, table_part);
    assert_eq!(evidence.write.row_count, rows.len());
    assert_eq!(
        evidence.write.source_package_sha256,
        sha256_bytes(&source_bytes)
    );
    assert_eq!(
        evidence.write.output_package_sha256,
        sha256_bytes(&output_bytes)
    );
    assert_eq!(evidence.write.package.changed_parts, expected_changed_parts);
    assert!(evidence.temporary_file_removed);
    assert_ne!(source_bytes, output_bytes);
    assert_no_atomic_temporary_files(directory.path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&workbook_path).unwrap().permissions().mode() & 0o777,
            original_mode
        );
    }
}

#[test]
fn rejects_a_source_that_no_longer_matches_the_execution_backup() {
    let directory = TestDirectory::new("source-changed");
    let layout = default_layout();
    let workbook_path = directory.path().join("plan.xlsx");
    let source_bytes = write_generated_workbook(&workbook_path, &layout);
    let expected = sha256_bytes(&source_bytes);
    let mut changed_bytes = source_bytes.clone();
    changed_bytes.extend_from_slice(b"changed-after-backup");
    fs::write(&workbook_path, &changed_bytes).unwrap();

    let error = write_execution_results_atomically_if_source_matches(
        &workbook_path,
        &expected,
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        crate::adapters::workbook::WorkbookProbeError::SourceChanged {
            ref expected,
            ref actual,
            ..
        } if expected == &sha256_bytes(&source_bytes)
            && actual == &sha256_bytes(&changed_bytes)
    ));
    assert_eq!(fs::read(&workbook_path).unwrap(), changed_bytes);
    assert_no_atomic_temporary_files(directory.path());
}

#[test]
fn same_row_count_reuses_the_table_part_byte_for_byte() {
    let directory = TestDirectory::new("same-row-count");
    let layout = default_layout();
    let source_path = directory.path().join("source.xlsx");
    write_generated_workbook(&source_path, &layout);
    let populated_path = directory.path().join("populated.xlsx");
    let first_rows = execution_rows(GENERATED_AT_UNIX_MILLIS);
    write_execution_results_to_new_file(&source_path, &populated_path, &layout, &first_rows)
        .unwrap();
    let populated_before = fs::read(&populated_path).unwrap();
    let populated_package =
        PackageSnapshot::from_bytes(&populated_before, &populated_path).unwrap();
    let (sheet, _) = execution_results_layout(&layout).unwrap();
    let worksheet_part = worksheet_part_name(&populated_package, sheet.display_name()).unwrap();
    let table_part = worksheet_table_part_name(&populated_package, &worksheet_part).unwrap();
    let table_before = populated_package.part(&table_part).unwrap().to_vec();
    let updated_path = directory.path().join("updated.xlsx");
    let updated_rows = execution_rows(GENERATED_AT_UNIX_MILLIS + 1);

    let evidence =
        write_execution_results_to_new_file(&populated_path, &updated_path, &layout, &updated_rows)
            .unwrap();

    let updated_bytes = fs::read(&updated_path).unwrap();
    let updated_package = PackageSnapshot::from_bytes(&updated_bytes, &updated_path).unwrap();
    assert_eq!(evidence.package.changed_parts, vec![worksheet_part]);
    assert_eq!(updated_package.part(&table_part).unwrap(), table_before);
    assert_eq!(fs::read(&populated_path).unwrap(), populated_before);
}

#[test]
fn writes_a_valid_layout_that_disables_the_execution_table_filter() {
    let directory = TestDirectory::new("filter-disabled");
    let layout = layout_with_execution_filter_disabled(directory.path());
    let (sheet, _) = execution_results_layout(&layout).unwrap();
    assert!(!sheet.default_filter());
    let source_path = directory.path().join("source.xlsx");
    write_generated_workbook(&source_path, &layout);
    let destination_path = directory.path().join("destination.xlsx");
    let rows = execution_rows(GENERATED_AT_UNIX_MILLIS);

    let evidence =
        write_execution_results_to_new_file(&source_path, &destination_path, &layout, &rows)
            .unwrap();

    assert_eq!(evidence.row_count, rows.len());
    assert!(destination_path.is_file());
}

#[test]
fn clearing_results_restores_one_formatted_placeholder_row() {
    let directory = TestDirectory::new("clear");
    let layout = default_layout();
    let source_path = directory.path().join("source.xlsx");
    write_generated_workbook(&source_path, &layout);
    let populated_path = directory.path().join("populated.xlsx");
    write_execution_results_to_new_file(
        &source_path,
        &populated_path,
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
    )
    .unwrap();
    let cleared_path = directory.path().join("cleared.xlsx");

    let evidence =
        write_execution_results_to_new_file(&populated_path, &cleared_path, &layout, &[]).unwrap();

    let cleared_bytes = fs::read(&cleared_path).unwrap();
    let package = PackageSnapshot::from_bytes(&cleared_bytes, &cleared_path).unwrap();
    let (sheet, fields) = execution_results_layout(&layout).unwrap();
    let expected_reference = table_reference(0, fields.len()).unwrap();
    let table = rewrite_table_part(
        &evidence.table_part,
        package.part(&evidence.table_part).unwrap(),
        &fields,
        sheet.default_filter(),
        &expected_reference,
    )
    .unwrap();
    assert_eq!(evidence.row_count, 0);
    assert_eq!(table.original_reference, expected_reference);
    assert_eq!(table.bytes, package.part(&evidence.table_part).unwrap());
    verify_physical_table_cells(&package, &evidence.worksheet_part, 0, fields.len()).unwrap();
}

#[test]
fn rejects_noncanonical_rows_before_creating_the_destination() {
    let directory = TestDirectory::new("row-order");
    let layout = default_layout();
    let source_path = directory.path().join("source.xlsx");
    let source_before = write_generated_workbook(&source_path, &layout);
    let destination_path = directory.path().join("destination.xlsx");
    let mut rows = execution_rows(GENERATED_AT_UNIX_MILLIS);
    rows.reverse();

    let error =
        write_execution_results_to_new_file(&source_path, &destination_path, &layout, &rows)
            .unwrap_err();

    assert!(error.to_string().contains("顺序不是稳定对象引用顺序"));
    assert!(!destination_path.exists());
    assert_eq!(fs::read(source_path).unwrap(), source_before);
}

#[test]
fn rejects_source_rows_that_are_not_in_physical_order() {
    let source = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <dimension ref="A1:A2"/>
  <sheetData>
<row r="2"><c r="A2"/></row>
<row r="1"><c r="A1" t="inlineStr"><is><t>Header</t></is></c></row>
  </sheetData>
</worksheet>"#;

    let error = match worksheet_template("xl/worksheets/sheet.xml", source, "A1:A2", 1) {
        Ok(_) => panic!("乱序物理行必须在切片重写前被拒绝"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("工作表物理行必须按行号严格递增"));
}

#[test]
fn rejects_a_sparse_far_cell_before_calamine_reads_the_sheet() {
    let directory = TestDirectory::new("far-cell");
    let layout = default_layout();
    let valid_path = directory.path().join("valid.xlsx");
    let valid_bytes = write_generated_workbook(&valid_path, &layout);
    let valid_package = PackageSnapshot::from_bytes(&valid_bytes, &valid_path).unwrap();
    let (sheet, fields) = execution_results_layout(&layout).unwrap();
    let worksheet_part = worksheet_part_name(&valid_package, sheet.display_name()).unwrap();
    let table_part = worksheet_table_part_name(&valid_package, &worksheet_part).unwrap();
    let table = rewrite_table_part(
        &table_part,
        valid_package.part(&table_part).unwrap(),
        &fields,
        sheet.default_filter(),
        &table_reference(0, fields.len()).unwrap(),
    )
    .unwrap();
    let template = worksheet_template(
        &worksheet_part,
        valid_package.part(&worksheet_part).unwrap(),
        &table.original_reference,
        fields.len(),
    )
    .unwrap();
    let mut invalid_data_body =
        valid_package.part(&worksheet_part).unwrap()[template.data_body.clone()].to_vec();
    invalid_data_body.extend_from_slice(b"<row r=\"1048576\"><c r=\"XFD1048576\"/></row>");
    let invalid_worksheet = apply_replacements(
        &worksheet_part,
        valid_package.part(&worksheet_part).unwrap(),
        vec![(template.data_body, invalid_data_body)],
    )
    .unwrap();
    let malformed_path = directory.path().join("malformed.xlsx");
    let malformed_bytes = rewrite_package_from_bytes(
        &valid_bytes,
        Cursor::new(Vec::new()),
        &valid_path,
        &malformed_path,
        &BTreeMap::from([(worksheet_part, invalid_worksheet)]),
        &[],
    )
    .unwrap()
    .into_inner();
    fs::write(&malformed_path, &malformed_bytes).unwrap();
    let destination_path = directory.path().join("destination.xlsx");

    let error = write_execution_results_to_new_file(
        &malformed_path,
        &destination_path,
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
    )
    .unwrap_err();

    assert!(error.to_string().contains("超出预期范围或重复"));
    assert!(!destination_path.exists());
    assert_eq!(fs::read(malformed_path).unwrap(), malformed_bytes);
}

#[test]
fn rejects_a_dimension_that_disagrees_with_the_table_before_writing() {
    let directory = TestDirectory::new("dimension-mismatch");
    let layout = default_layout();
    let valid_path = directory.path().join("valid.xlsx");
    let valid_bytes = write_generated_workbook(&valid_path, &layout);
    let valid_package = PackageSnapshot::from_bytes(&valid_bytes, &valid_path).unwrap();
    let (sheet, fields) = execution_results_layout(&layout).unwrap();
    let worksheet_part = worksheet_part_name(&valid_package, sheet.display_name()).unwrap();
    let table_part = worksheet_table_part_name(&valid_package, &worksheet_part).unwrap();
    let table = rewrite_table_part(
        &table_part,
        valid_package.part(&table_part).unwrap(),
        &fields,
        sheet.default_filter(),
        &table_reference(0, fields.len()).unwrap(),
    )
    .unwrap();
    let template = worksheet_template(
        &worksheet_part,
        valid_package.part(&worksheet_part).unwrap(),
        &table.original_reference,
        fields.len(),
    )
    .unwrap();
    let invalid_dimension = replace_ref_attribute(
        &worksheet_part,
        &template.dimension_element,
        &table_reference(2, fields.len()).unwrap(),
        template.dimension_is_empty,
    )
    .unwrap();
    let invalid_worksheet = apply_replacements(
        &worksheet_part,
        valid_package.part(&worksheet_part).unwrap(),
        vec![(template.dimension, invalid_dimension)],
    )
    .unwrap();
    let malformed_path = directory.path().join("malformed.xlsx");
    let malformed_bytes = rewrite_package_from_bytes(
        &valid_bytes,
        Cursor::new(Vec::new()),
        &valid_path,
        &malformed_path,
        &BTreeMap::from([(worksheet_part, invalid_worksheet)]),
        &[],
    )
    .unwrap()
    .into_inner();
    fs::write(&malformed_path, &malformed_bytes).unwrap();
    let destination_path = directory.path().join("destination.xlsx");

    let error = write_execution_results_to_new_file(
        &malformed_path,
        &destination_path,
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
    )
    .unwrap_err();

    assert!(error.to_string().contains("dimension 与表格范围不一致"));
    assert!(!destination_path.exists());
    assert_eq!(fs::read(malformed_path).unwrap(), malformed_bytes);
}

#[test]
fn rejects_a_source_whose_schema_snapshot_does_not_match_the_layout() {
    let directory = TestDirectory::new("schema-mismatch");
    let layout = default_layout();
    let valid_path = directory.path().join("valid.xlsx");
    let valid_before = write_generated_workbook(&valid_path, &layout);
    let schema_sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "schema")
        .unwrap();
    let schema_fields = layout.generated_fields_for_sheet(schema_sheet.stable_key());
    let layout_hash_column = schema_fields
        .iter()
        .position(|field| field.stable_key() == "layout_hash")
        .unwrap();
    let layout_hash_cell = row_col_to_cell(1, u16::try_from(layout_hash_column).unwrap());
    let mismatched_path = directory.path().join("mismatched.xlsx");
    edit_text_cell_to_new_file(
        &valid_path,
        &mismatched_path,
        schema_sheet.display_name(),
        &layout_hash_cell,
        &"0".repeat(64),
    )
    .unwrap();
    let mismatched_before = fs::read(&mismatched_path).unwrap();
    let destination_path = directory.path().join("destination.xlsx");

    let error = write_execution_results_to_new_file(
        &mismatched_path,
        &destination_path,
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
    )
    .unwrap_err();

    assert!(error.to_string().contains("layout_hash"));
    assert!(!destination_path.exists());
    assert_eq!(fs::read(mismatched_path).unwrap(), mismatched_before);
    assert_eq!(fs::read(valid_path).unwrap(), valid_before);
}

#[test]
fn refuses_to_overwrite_an_existing_destination() {
    let directory = TestDirectory::new("destination-conflict");
    let layout = default_layout();
    let source_path = directory.path().join("source.xlsx");
    let source_before = write_generated_workbook(&source_path, &layout);
    let destination_path = directory.path().join("occupied.xlsx");
    fs::write(&destination_path, b"existing-user-content").unwrap();

    let error = write_execution_results_to_new_file(
        &source_path,
        &destination_path,
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        super::WorkbookProbeError::InvalidPath { .. }
    ));
    assert_eq!(
        fs::read(destination_path).unwrap(),
        b"existing-user-content"
    );
    assert_eq!(fs::read(source_path).unwrap(), source_before);
}

fn default_layout() -> WorkbookLayout {
    crate::adapters::workbook::layout::template::full_test_layout()
}

fn layout_with_execution_filter_disabled(directory: &Path) -> WorkbookLayout {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx");
    let mut workbook: Xlsx<_> = open_workbook(&source).unwrap();
    let range = workbook.worksheet_range("工作表设置").unwrap();
    let row = range
        .rows()
        .position(|values| {
            matches!(values.first(), Some(Data::String(value)) if value == "execution_results")
        })
        .unwrap();
    let filter_cell = row_col_to_cell(u32::try_from(row).unwrap(), 5);
    let edited = directory.join("workbook-layout-no-execution-filter.xlsx");
    edit_text_cell_to_new_file(&source, &edited, "工作表设置", &filter_cell, "否").unwrap();
    load_workbook_layout(&edited, &WorkbookProjectionV4::layout_registry().unwrap()).unwrap()
}

fn write_generated_workbook(path: &Path, layout: &WorkbookLayout) -> Vec<u8> {
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let build =
        build_projection_workbook_bytes(path, layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    fs::write(path, &build.bytes).unwrap();
    build.bytes
}

fn execution_rows(executed_at_unix_millis: i64) -> Vec<WorkbookProjectionRow> {
    project_execution_report_rows(
        &execution_workbook_report_fixture(),
        executed_at_unix_millis,
    )
    .unwrap()
}

fn assert_no_atomic_temporary_files(directory: &Path) {
    let names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|name| !name.contains(".azlw-")),
        "{names:?}"
    );
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let identifier = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let parent = home.join("suzushiro/scratch/azlw-execution-results-writer-tests");
        fs::create_dir_all(&parent).expect("应建立执行结果写回测试根目录");
        let path = parent.join(format!("{label}-{}-{identifier}", std::process::id()));
        fs::create_dir(&path).expect("测试目录不得与残留目录重名");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("应清理执行结果写回测试目录");
        }
    }
}

#[test]
fn snapshot_refresh_preserves_inputs_and_history_and_reuses_styles_on_second_publish() {
    use super::write_execution_snapshot_atomically_if_source_matches;
    use crate::application::{LayoutEditor, WorkbookProjectionValue as V};
    let directory = TestDirectory::new("snapshot-input-history");
    let layout = default_layout();
    let path = directory.path().join("plan.xlsx");
    let final_projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let mut replacements = BTreeMap::new();
    let loadout: Vec<_> = final_projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| {
            let mut values = row.values().clone();
            if matches!(row.value("instance_id"), Some(V::Text(_))) {
                values.insert(
                    "slot_1_note".to_owned(),
                    V::text(format!("保留目标:{}", row.object_ref())),
                );
                values.insert(
                    "slot_1_target_equipment_family".to_owned(),
                    V::text("目标装备"),
                );
                values.insert("slot_1_allocation_priority".to_owned(), V::Integer(7));
            }
            (row.object_ref().to_owned(), values)
        })
        .collect();
    replacements.insert("loadout_plan".to_owned(), loadout.clone());
    replacements.insert(
        "equipment_inventory".to_owned(),
        final_projection
            .sheet("equipment_inventory")
            .unwrap()
            .rows()
            .iter()
            .map(|row| {
                let mut values = row.values().clone();
                values.insert("processing_quantity".to_owned(), V::Integer(2));
                (row.object_ref().to_owned(), values)
            })
            .collect(),
    );
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    for key in ["plan_data", "check_results"] {
        let values = registry
            .fields()
            .iter()
            .filter(|field| field.sheet_key() == key)
            .map(|field| (field.stable_key().to_owned(), V::Blank))
            .collect::<BTreeMap<_, _>>();
        let mut values = values;
        let text_field = layout
            .fields()
            .iter()
            .find(|field| {
                field.sheet_key() == key
                    && field.value_format() == crate::application::LayoutValueFormat::Text
                    && field.enum_category().is_none()
            })
            .unwrap();
        values.insert(text_field.stable_key().to_owned(), V::text("保留历史证据"));
        replacements.insert(key.to_owned(), vec![("history:1".to_owned(), values)]);
    }
    let initial = final_projection
        .clone()
        .with_replaced_rows(replacements)
        .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    let seeded = PackageSnapshot::from_bytes(&bytes, &path).unwrap();
    let loadout_sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap();
    let loadout_part = worksheet_part_name(&seeded, loadout_sheet.display_name()).unwrap();
    let seeded_xml = String::from_utf8(seeded.part(&loadout_part).unwrap().to_vec()).unwrap();
    let extension = "<extLst><ext uri=\"urn:fixture:extension\"><custom:sheetData xmlns:custom=\"urn:fixture:extension\"><custom:payload>KEEP_EXTENSION_PAYLOAD</custom:payload></custom:sheetData></ext></extLst></worksheet>";
    assert!(seeded_xml.contains("</worksheet>"));
    let bytes = rewrite_package_from_bytes(
        &bytes,
        Cursor::new(Vec::new()),
        &path,
        &path,
        &BTreeMap::from([(
            loadout_part.clone(),
            seeded_xml
                .replacen("</worksheet>", extension, 1)
                .into_bytes(),
        )]),
        &[],
    )
    .unwrap()
    .into_inner();
    fs::write(&path, &bytes).unwrap();
    let original = PackageSnapshot::from_bytes(&bytes, &path).unwrap();
    let rows = execution_rows(GENERATED_AT_UNIX_MILLIS + 1000);
    let evidence = write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &rows,
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap();
    let once = fs::read(&path).unwrap();
    let package = PackageSnapshot::from_bytes(&once, &path).unwrap();
    let refreshed_loadout =
        String::from_utf8(package.part(&loadout_part).unwrap().to_vec()).unwrap();
    assert!(
        refreshed_loadout.contains("KEEP_EXTENSION_PAYLOAD"),
        "{refreshed_loadout}"
    );
    assert!(!refreshed_loadout.contains("<custom:sheetData/>"));
    for key in ["plan_data", "check_results"] {
        let sheet = layout
            .sheets()
            .iter()
            .find(|sheet| sheet.stable_key() == key)
            .unwrap();
        let part = worksheet_part_name(&original, sheet.display_name()).unwrap();
        assert_eq!(
            original.part(&part).unwrap(),
            package.part(&part).unwrap(),
            "{key}"
        );
    }
    assert!(package.part("xl/sharedStrings.xml").is_ok());
    assert!(
        evidence
            .write
            .package
            .changed_parts
            .iter()
            .any(|part| part == "xl/styles.xml")
    );
    let mut xlsx: Xlsx<_> = open_workbook(&path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "equipment_inventory")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let inventory = xlsx.worksheet_range(sheet.display_name()).unwrap();
    for (column, field) in fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.editor() != LayoutEditor::ReadOnly)
    {
        for row in 1..inventory.height() {
            assert!(
                matches!(
                    inventory.get_value((row as u32, column as u32)),
                    None | Some(Data::Empty)
                ),
                "{}",
                field.stable_key()
            );
        }
    }
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let values = xlsx.worksheet_range(sheet.display_name()).unwrap();
    for (index, (_, expected)) in loadout.iter().enumerate() {
        for (column, field) in fields.iter().enumerate().filter(|(_, field)| {
            field.editor() != LayoutEditor::ReadOnly && field.stable_key().starts_with("slot_")
        }) {
            super::verify_projection_cell(
                values.get_value(((index + 1) as u32, column as u32)),
                &expected[field.stable_key()],
                &super::enum_labels(&layout),
                field.stable_key(),
            )
            .unwrap();
        }
    }
    drop(xlsx);
    write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&once),
        &layout,
        &rows,
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap();
    let twice = fs::read(&path).unwrap();
    let twice = PackageSnapshot::from_bytes(&twice, &path).unwrap();
    assert!(
        String::from_utf8_lossy(twice.part(&loadout_part).unwrap())
            .contains("KEEP_EXTENSION_PAYLOAD")
    );
    assert_eq!(
        package.part("xl/styles.xml").unwrap(),
        twice.part("xl/styles.xml").unwrap()
    );
    assert_eq!(
        package.part("xl/sharedStrings.xml").unwrap(),
        twice.part("xl/sharedStrings.xml").unwrap()
    );
    assert_no_atomic_temporary_files(directory.path());
}

#[test]
fn snapshot_refresh_rejects_missing_instance_with_input_and_preserves_the_original() {
    use crate::application::WorkbookProjectionValue as V;
    let directory = TestDirectory::new("snapshot-orphan");
    let layout = default_layout();
    let path = directory.path().join("plan.xlsx");
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let mut rows: Vec<_> = projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    let owned = rows
        .iter_mut()
        .find(|(_, values)| matches!(values.get("instance_id"), Some(V::Text(_))))
        .unwrap();
    owned
        .1
        .insert("slot_1_note".to_owned(), V::text("不可丢失的用户目标"));
    let removed = owned.0.clone();
    let initial = projection
        .clone()
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    let final_rows = projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .filter(|row| row.object_ref() != removed)
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    let final_projection = projection
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), final_rows)]))
        .unwrap();
    let error = super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap_err();
    assert!(error.to_string().contains("仍有用户输入的实例"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_no_atomic_temporary_files(directory.path());
}

#[test]
fn snapshot_refresh_source_guard_rejects_changed_package() {
    let directory = TestDirectory::new("snapshot-source-guard");
    let layout = default_layout();
    let path = directory.path().join("plan.xlsx");
    let bytes = write_generated_workbook(&path, &layout);
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let error = super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &"0".repeat(64),
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
        Some(&projection),
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        super::WorkbookProbeError::SourceChanged { .. }
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_no_atomic_temporary_files(directory.path());
}

#[test]
fn snapshot_refresh_empty_results_keeps_inventory_readable_with_no_operations() {
    let directory = TestDirectory::new("snapshot-empty-results");
    let layout = default_layout();
    let path = directory.path().join("plan.xlsx");
    let bytes = write_generated_workbook(&path, &layout);
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let evidence = super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &[],
        Some(&projection),
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
    assert_eq!(evidence.write.row_count, 0);
    let plan = crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(&path, &layout)
        .unwrap()
        .inventory;
    assert!(plan.actions().is_empty());
    assert_no_atomic_temporary_files(directory.path());
}

#[test]
fn snapshot_refresh_keeps_input_with_instance_when_row_position_changes() {
    use crate::application::WorkbookProjectionValue as V;
    let directory = TestDirectory::new("snapshot-instance-order");
    let layout = default_layout();
    let path = directory.path().join("plan.xlsx");
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let mut rows: Vec<_> = projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    let owned = rows
        .iter_mut()
        .find(|(_, values)| matches!(values.get("instance_id"), Some(V::Text(_))))
        .unwrap();
    let instance = owned.1["instance_id"].clone();
    owned
        .1
        .insert("slot_1_note".to_owned(), V::text("跟随舰船实例"));
    let initial = projection
        .clone()
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    let mut final_rows: Vec<_> = projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    let mut inserted = final_rows
        .iter()
        .find(|(_, values)| values.get("instance_id") == Some(&instance))
        .unwrap()
        .1
        .clone();
    inserted.insert("instance_id".to_owned(), V::text("999999999"));
    inserted.insert("source_ref".to_owned(), V::text("owned:999999999"));
    final_rows.push(("000-new-instance".to_owned(), inserted));
    let final_projection = projection
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), final_rows)]))
        .unwrap();
    super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &[],
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
    let mut xlsx: Xlsx<_> = open_workbook(&path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let id_col = fields
        .iter()
        .position(|field| field.stable_key() == "instance_id")
        .unwrap();
    let note_col = fields
        .iter()
        .position(|field| field.stable_key() == "slot_1_note")
        .unwrap();
    let range = xlsx.worksheet_range(sheet.display_name()).unwrap();
    let V::Text(instance) = instance else {
        panic!("实例必须为文本")
    };
    let row = range
        .rows()
        .skip(1)
        .find(|row| row[id_col] == Data::String(instance.clone()))
        .unwrap();
    assert_eq!(row[note_col], Data::String("跟随舰船实例".to_owned()));
    let new_row = range
        .rows()
        .skip(1)
        .find(|row| row[id_col] == Data::String("999999999".to_owned()))
        .unwrap();
    assert!(matches!(new_row[note_col], Data::Empty));
}

#[test]
#[ignore = "需要 AZLW_SST_WORKBOOK 和 AZLW_SST_LAYOUT 指定真实工作簿及其布局，在副本上验证刷新"]
fn snapshot_refresh_real_workbook_copy_with_fixture_final_state() {
    let source = std::env::var_os("AZLW_SST_WORKBOOK").expect("指定工作簿路径");
    let layout_path = std::env::var_os("AZLW_SST_LAYOUT").expect("指定生成该工作簿的布局路径");
    let directory = TestDirectory::new("snapshot-real-copy");
    let path = directory.path().join("copy.xlsx");
    let bytes = fs::read(source).unwrap();
    fs::write(&path, &bytes).unwrap();
    let layout = load_workbook_layout(
        &PathBuf::from(layout_path),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let mut workbook = Xlsx::new(Cursor::new(bytes.as_slice())).unwrap();
    let layout =
        crate::adapters::workbook::reader::select_layout_snapshot(&mut workbook, &layout).unwrap();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let evidence = super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &[],
        Some(&projection),
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
    assert_ne!(
        evidence.write.source_package_sha256,
        evidence.write.output_package_sha256
    );
    let plan = crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(&path, &layout)
        .unwrap()
        .inventory;
    assert!(plan.actions().is_empty());
    eprintln!(
        "real workbook copy: source_bytes={}, output_bytes={}, changed_parts={}",
        bytes.len(),
        fs::metadata(&path).unwrap().len(),
        evidence.write.package.changed_parts.len()
    );
}

#[test]
fn snapshot_refresh_preserves_compact_goal_and_clears_inventory_operation() {
    use crate::application::WorkbookProjectionValue as V;
    let directory = TestDirectory::new("snapshot-compact-inputs");
    let layout = load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let path = directory.path().join("plan.xlsx");
    let final_projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let mut replacements = BTreeMap::new();
    let loadout = final_projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| {
            let mut values = row.values().clone();
            if matches!(row.value("instance_id"), Some(V::Text(_))) {
                values.insert("slot_2_target_equipment_family".to_owned(), V::text("卸下"));
                values.insert("slot_3_target_equipment_family".to_owned(), V::text("拆解"));
                values.insert(
                    "slot_1_target_equipment_family".to_owned(),
                    V::text("维修设施〔1000〕"),
                );
            }
            (row.object_ref().to_owned(), values)
        })
        .collect();
    replacements.insert("loadout_plan".to_owned(), loadout);
    let inventory = final_projection
        .sheet("equipment_inventory")
        .unwrap()
        .rows()
        .iter()
        .map(|row| {
            let mut values = row.values().clone();
            if matches!(row.value("source_type"), Some(V::Text(source)) if source == "ship") {
                values.insert("ship_name".to_owned(), V::text("刷新前舰船名称"));
            }
            values.insert(
                "operation".to_owned(),
                V::enumeration("inventory_operation", "enhance"),
            );
            values.insert("processing_quantity".to_owned(), V::Integer(1));
            values.insert("target_enhance_level".to_owned(), V::Integer(1));
            (row.object_ref().to_owned(), values)
        })
        .collect();
    replacements.insert("equipment_inventory".to_owned(), inventory);
    let initial = final_projection
        .clone()
        .with_replaced_rows(replacements)
        .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &[],
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
    let desired = crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(&path, &layout)
        .unwrap()
        .desired;
    assert_eq!(desired.len(), 1);
    let inventory = crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(&path, &layout)
        .unwrap()
        .inventory;
    assert!(inventory.is_empty());
    let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
    let range = workbook.worksheet_range("装备总表").unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "equipment_inventory")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let column = |key: &str| {
        fields
            .iter()
            .position(|field| field.stable_key() == key)
            .unwrap() as u32
    };
    for expected in final_projection
        .sheet("equipment_inventory")
        .unwrap()
        .rows()
    {
        if !matches!(expected.value("source_type"),Some(V::Text(source)) if source == "ship") {
            continue;
        }
        let Some(V::Text(ship_id)) = expected.value("ship_instance_id") else {
            panic!("舰船实例ID")
        };
        let Some(V::Integer(slot)) = expected.value("slot_index") else {
            panic!("装备槽位")
        };
        let row = (1..range.height() as u32)
            .find(|row| {
                range.get_value((*row, column("ship_instance_id")))
                    == Some(&Data::String(ship_id.clone()))
                    && range.get_value((*row, column("slot_index")))
                        == Some(&Data::Float(*slot as f64))
            })
            .expect("最终装备保留舰船与槽位归属");
        for key in ["name", "ship_name", "config_id"] {
            let Some(V::Text(text)) = expected.value(key) else {
                panic!("装备文本字段")
            };
            assert_eq!(
                range.get_value((row, column(key))),
                Some(&Data::String(text.clone()))
            );
        }
        assert_eq!(
            range.get_value((row, column("source_type"))),
            Some(&Data::String("舰船".to_owned()))
        );
    }
}

#[test]
fn snapshot_refresh_rebuilds_equipment_choices_after_materials_are_consumed() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_only_composable_equipment;
    use crate::application::project_game_state_to_workbook;
    let directory = TestDirectory::new("equipment-choices-refresh");
    let path = directory.path().join("workbook.xlsx");
    let layout = default_layout();
    let initial =
        project_game_state_to_workbook(&golden_game_state_with_only_composable_equipment(30))
            .unwrap();
    let final_projection =
        project_game_state_to_workbook(&golden_game_state_with_only_composable_equipment(0))
            .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap();
    let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "dictionaries")
        .unwrap();
    let range = workbook.worksheet_range(sheet.display_name()).unwrap();
    assert!(
        !range
            .cells()
            .any(|(_, _, cell)| matches!(cell, Data::String(value) if value.starts_with("合成×")))
    );
    assert_eq!(
        range.height(),
        layout.enum_options().len()
            + final_projection.sheet("dictionaries").unwrap().rows().len()
            + 1
    );
}

#[test]
fn snapshot_refresh_updates_ship_display_name_and_original_name_link() {
    use crate::adapters::device::game_state_mapper::golden_fixture::named_ship_game_state;
    let directory = TestDirectory::new("ship-name-refresh");
    let path = directory.path().join("workbook.xlsx");
    let layout = default_layout();
    let initial = project_game_state_to_workbook(&named_ship_game_state("拉菲", true)).unwrap();
    let final_projection =
        project_game_state_to_workbook(&named_ship_game_state("我的昵称", true)).unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &[],
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap();
    let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
    let range = workbook.worksheet_range("配装计划").unwrap();
    let column = range
        .rows()
        .next()
        .unwrap()
        .iter()
        .position(|cell| *cell == "舰船名称")
        .unwrap();
    assert_eq!(
        range.get((1, column)).unwrap().to_string(),
        "我的昵称(拉菲)"
    );
    let package = PackageSnapshot::read(&path).unwrap();
    let part = worksheet_part_name(&package, "配装计划").unwrap();
    let rels =
        crate::adapters::workbook::reference::inspection::worksheet_relationships_name(&part)
            .unwrap();
    let links = crate::adapters::workbook::package::parse_relationships(
        &rels,
        package.part(&rels).unwrap(),
    )
    .unwrap();
    let url = crate::adapters::workbook::ship_wiki::ship_wiki_url("拉菲").unwrap();
    assert!(links.iter().any(|link| link.external && link.target == url));
    assert!(
        !links
            .iter()
            .any(|link| link.external && link.target.contains("我的昵称"))
    );
}

#[test]
fn snapshot_refresh_updates_technology_bonus_from_final_state() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology;
    use crate::application::WorkbookProjectionValue as V;
    let directory = TestDirectory::new("technology-bonus-refresh");
    let path = directory.path().join("workbook.xlsx");
    let layout = load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let initial = project_game_state_to_workbook(&golden_game_state_with_technology()).unwrap();
    let mut rows: Vec<_> = initial
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    rows[0].1.insert(
        "technology_bonus".to_owned(),
        V::text("战巡、战列、航战-命中"),
    );
    let final_projection = initial
        .clone()
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    fs::write(&path, &bytes).unwrap();
    super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &[],
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap();
    let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
    assert_eq!(workbook.sheet_names().len(), 8);
    let range = workbook.worksheet_range("配装计划").unwrap();
    let column = range
        .rows()
        .next()
        .unwrap()
        .iter()
        .position(|cell| *cell == "科技加成")
        .unwrap();
    assert_eq!(
        range.get((1, column)).unwrap().to_string(),
        "战巡、战列、航战-命中"
    );
}

#[test]
fn snapshot_refresh_updates_technology_category_rows() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology;
    use crate::application::project_game_state_to_workbook;
    let directory = TestDirectory::new("technology-category-refresh");
    let path = directory.path().join("workbook.xlsx");
    let layout = crate::adapters::workbook::layout::template::technology_test_layout();
    let initial = project_game_state_to_workbook(&golden_game_state_with_technology()).unwrap();
    let mut rows: Vec<_> = initial
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    rows[0].1.insert(
        "level".to_owned(),
        crate::application::WorkbookProjectionValue::Integer(121),
    );
    let final_projection = initial
        .clone()
        .with_replaced_rows(std::collections::BTreeMap::from([(
            "loadout_plan".to_owned(),
            rows,
        )]))
        .unwrap();
    let bytes = build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
        .unwrap()
        .bytes;
    let package = PackageSnapshot::from_bytes(&bytes, &path).unwrap();
    let names = BTreeMap::from([
        ("驱逐、导驱-耐久".to_owned(), "驱逐_导驱_耐久".to_owned()),
        ("驱逐、导驱-炮击".to_owned(), "驱逐_导驱_炮击".to_owned()),
    ]);
    let legacy = crate::adapters::workbook::reference::inspection::rename_workbook_sheets(
        package.part("xl/workbook.xml").unwrap(),
        &names,
    )
    .unwrap();
    let bytes = rewrite_package_from_bytes(
        &bytes,
        Cursor::new(Vec::new()),
        &path,
        &path,
        &BTreeMap::from([("xl/workbook.xml".to_owned(), legacy)]),
        &[],
    )
    .unwrap()
    .into_inner();
    fs::write(&path, &bytes).unwrap();
    super::write_execution_snapshot_atomically_if_source_matches(
        &path,
        &sha256_bytes(&bytes),
        &layout,
        &execution_rows(GENERATED_AT_UNIX_MILLIS),
        Some(&final_projection),
        GENERATED_AT_UNIX_MILLIS + 1000,
    )
    .unwrap();
    let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
    for name in ["驱逐、导驱-耐久", "驱逐、导驱-炮击"] {
        let range = workbook.worksheet_range(name).unwrap();
        assert_eq!(range.get_value((1, 7)), Some(&Data::Float(121.0)));
    }
}

#[test]
fn technology_writeback_preflight_rejects_missing_tables_and_name_collisions() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology;
    use crate::adapters::workbook::projection_writer::technology_views::source_sheet_bindings;
    let layout = crate::adapters::workbook::layout::template::technology_test_layout();
    let projection = project_game_state_to_workbook(&golden_game_state_with_technology()).unwrap();
    let path = Path::new("technology-preflight.xlsx");
    let bytes =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap()
            .bytes;
    let package = PackageSnapshot::from_bytes(&bytes, path).unwrap();
    assert!(source_sheet_bindings(&bytes, &package, &layout, &projection).is_ok());
    let part = worksheet_part_name(&package, "驱逐、导驱-耐久").unwrap();
    let table_part = worksheet_table_part_name(&package, &part).unwrap();
    let xml = String::from_utf8(package.part(&table_part).unwrap().to_vec()).unwrap();
    let replacements = BTreeMap::from([(
        table_part,
        xml.replace("AZLW_ship_technology_", "missing_")
            .into_bytes(),
    )]);
    let missing = rewrite_package_from_bytes(
        &bytes,
        Cursor::new(Vec::new()),
        path,
        path,
        &replacements,
        &[],
    )
    .unwrap()
    .into_inner();
    let malformed = PackageSnapshot::from_bytes(&missing, path).unwrap();
    assert!(
        source_sheet_bindings(&missing, &malformed, &layout, &projection)
            .unwrap_err()
            .to_string()
            .contains("分类表集合")
    );
    let xml = String::from_utf8(package.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
    let collision = xml
        .replace("name=\"驱逐、导驱-耐久\"", "name=\"驱逐_导驱_耐久\"")
        .replace("name=\"驱逐、导驱-炮击\"", "name=\"驱逐_导驱_耐久\"");
    let duplicate = rewrite_package_from_bytes(
        &bytes,
        Cursor::new(Vec::new()),
        path,
        path,
        &BTreeMap::from([("xl/workbook.xml".to_owned(), collision.into_bytes())]),
        &[],
    )
    .unwrap()
    .into_inner();
    let malformed = PackageSnapshot::from_bytes(&duplicate, path).unwrap();
    assert!(source_sheet_bindings(&duplicate, &malformed, &layout, &projection).is_err());
}

#[test]
fn snapshot_refresh_preserves_short_equipment_selection_bindings() {
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_only_composable_equipment;
    use crate::application::WorkbookProjectionValue as V;
    for materials in [15, 0] {
        let directory = TestDirectory::new("short-choice-refresh");
        let path = directory.path().join("workbook.xlsx");
        let layout = load_workbook_layout(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
            &WorkbookProjectionV4::layout_registry().unwrap(),
        )
        .unwrap();
        let initial =
            project_game_state_to_workbook(&golden_game_state_with_only_composable_equipment(30))
                .unwrap();
        let choice = initial
            .sheet("dictionaries")
            .unwrap()
            .rows()
            .iter()
            .find(|row| row.value("stable_value") == Some(&V::text("1000|compose")))
            .unwrap();
        let label = choice.value("display_label").unwrap().clone();
        let loadout = initial
            .sheet("loadout_plan")
            .unwrap()
            .rows()
            .iter()
            .map(|row| {
                let mut values = row.values().clone();
                values.insert("slot_2_target_equipment_family".to_owned(), label.clone());
                (row.object_ref().to_owned(), values)
            })
            .collect();
        let initial = initial
            .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), loadout)]))
            .unwrap();
        let final_projection = project_game_state_to_workbook(
            &golden_game_state_with_only_composable_equipment(materials),
        )
        .unwrap();
        let bytes =
            build_projection_workbook_bytes(&path, &layout, &initial, GENERATED_AT_UNIX_MILLIS)
                .unwrap()
                .bytes;
        fs::write(&path, &bytes).unwrap();
        super::write_execution_snapshot_atomically_if_source_matches(
            &path,
            &sha256_bytes(&bytes),
            &layout,
            &execution_rows(GENERATED_AT_UNIX_MILLIS),
            Some(&final_projection),
            GENERATED_AT_UNIX_MILLIS + 1000,
        )
        .unwrap();
        let desired =
            crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(&path, &layout)
                .unwrap()
                .desired;
        assert!(
            matches!(desired.slots()[0].target(), crate::domain::SlotTarget::Equipment(equipment) if equipment.family_id().get() == 1000 && equipment.source_policy() == crate::domain::SourcePolicy::ComposeOnly)
        );
        let mut workbook: Xlsx<_> = open_workbook(&path).unwrap();
        let sheet = layout
            .sheets()
            .iter()
            .find(|sheet| sheet.stable_key() == "loadout_plan")
            .unwrap();
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let column = fields
            .iter()
            .position(|field| field.stable_key() == "slot_2_target_equipment_family")
            .unwrap();
        let range = workbook.worksheet_range(sheet.display_name()).unwrap();
        let expected = final_projection
            .sheet("dictionaries")
            .unwrap()
            .rows()
            .iter()
            .find(|row| row.value("stable_value") == Some(&V::text("1000|compose")))
            .and_then(|row| row.value("display_label"))
            .unwrap_or(&label);
        let V::Text(expected) = expected else {
            panic!("应为文字")
        };
        assert_eq!(
            range.get_value((1, column as u32)),
            Some(&Data::String(expected.clone()))
        );
    }
}

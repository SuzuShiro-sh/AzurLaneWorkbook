//! 通过复制后的正式可执行文件验证布局预览的固定路径、刷新和失败保留契约。

use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use azur_lane_workbook::adapters::workbook::edit_text_cell_to_new_file;
use calamine::{Data, Reader, Xlsx, open_workbook};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

mod common;

use common::TestDirectory;

#[test]
fn refreshes_the_fixed_preview_from_an_unrelated_working_directory() {
    let fixture = TestDirectory::new("azlw-layout-preview-cli-tests", "refresh");
    let executable = common::copy_executable(fixture.path());
    let layout_path = copy_root_layout(fixture.path());
    let working_directory = fixture.path().join("unrelated-working-directory");
    fs::create_dir(&working_directory).unwrap();

    let first_output = run_preview(&executable, &working_directory);

    assert!(
        first_output.status.success(),
        "命令失败: {}",
        stderr(&first_output)
    );
    assert!(first_output.stderr.is_empty());
    let first_report: serde_json::Value = serde_json::from_slice(&first_output.stdout).unwrap();
    assert_eq!(first_report["message"], "工作簿布局预览已刷新");
    assert_eq!(
        first_report["output_path"],
        "data/workbooks/layout-preview.xlsx"
    );
    assert_eq!(first_report["layout_schema_version"], 1);
    assert_eq!(first_report["workbook_schema_version"], 19);
    assert_eq!(first_report["generated_sheets"], 8);
    assert_eq!(first_report["hidden_sheets"], 4);
    assert_eq!(first_report["omitted_sheets"], 2);
    assert_eq!(first_report["generated_fields"], 180);
    assert_eq!(first_report["hidden_fields"], 14);
    assert_eq!(first_report["omitted_fields"], 223);
    assert_eq!(first_report["example_rows"], 465);
    let preview_path =
        common::resource_root(fixture.path()).join("data/workbooks/layout-preview.xlsx");
    let first_bytes = fs::read(&preview_path).unwrap();
    assert_eq!(
        first_report["output_package_sha256"],
        sha256_hex(&first_bytes)
    );
    assert_preview_header(&preview_path, "舰船实例ID", 8);

    replace_layout_cell(&layout_path, "字段设置", "D2", "自定义示例列");
    replace_layout_cell(&layout_path, "字段设置", "C2", "隐藏");
    replace_layout_cell(&layout_path, "工作表设置", "B8", "不生成");
    let second_output = run_preview(&executable, fixture.path());

    assert!(
        second_output.status.success(),
        "命令失败: {}",
        stderr(&second_output)
    );
    assert!(second_output.stderr.is_empty());
    let second_report: serde_json::Value = serde_json::from_slice(&second_output.stdout).unwrap();
    let second_bytes = fs::read(&preview_path).unwrap();
    assert_ne!(first_bytes, second_bytes);
    assert_ne!(
        first_report["layout_content_sha256"],
        second_report["layout_content_sha256"]
    );
    assert_eq!(
        second_report["output_package_sha256"],
        sha256_hex(&second_bytes)
    );
    assert_eq!(second_report["generated_sheets"], 8);
    assert_eq!(second_report["hidden_sheets"], 4);
    assert_eq!(second_report["omitted_sheets"], 2);
    assert_eq!(second_report["generated_fields"], 180);
    assert_eq!(second_report["hidden_fields"], 15);
    assert_eq!(second_report["omitted_fields"], 223);
    assert_eq!(second_report["example_rows"], 465);
    assert_preview_header(&preview_path, "自定义示例列", 8);
    assert_ooxml_features(&second_bytes);
    assert_eq!(
        directory_entries(preview_path.parent().unwrap()),
        vec![
            preview_path
                .parent()
                .unwrap()
                .join(".layout-preview.xlsx.write.lock"),
            preview_path,
        ]
    );
}

#[test]
fn keeps_the_existing_preview_when_the_layout_becomes_invalid() {
    let fixture = TestDirectory::new("azlw-layout-preview-cli-tests", "preserve");
    let executable = common::copy_executable(fixture.path());
    let layout_path = copy_root_layout(fixture.path());
    let first_output = run_preview(&executable, fixture.path());
    assert!(
        first_output.status.success(),
        "命令失败: {}",
        stderr(&first_output)
    );
    let preview_path =
        common::resource_root(fixture.path()).join("data/workbooks/layout-preview.xlsx");
    let preview_before = fs::read(&preview_path).unwrap();
    replace_layout_cell(&layout_path, "字段设置", "B2", "unknown_preview_field");

    let failed_output = run_preview(&executable, fixture.path());

    assert!(!failed_output.status.success());
    assert!(failed_output.stdout.is_empty());
    assert_eq!(fs::read(&preview_path).unwrap(), preview_before);
    assert_eq!(
        directory_entries(preview_path.parent().unwrap()),
        vec![
            preview_path
                .parent()
                .unwrap()
                .join(".layout-preview.xlsx.write.lock"),
            preview_path,
        ]
    );
    let error = stderr(&failed_output);
    for expected in [
        "布局预览失败 [LAYOUT_INVALID]",
        "工作表：字段设置",
        "配置行：2",
        "unknown_preview_field",
        "原因：",
        "修复建议：",
        "layout-preview",
        "已有预览保持不变",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
}

#[test]
fn reports_an_output_path_conflict_without_creating_preview_files() {
    let fixture = TestDirectory::new("azlw-layout-preview-cli-tests", "output-conflict");
    let executable = common::copy_executable(fixture.path());
    copy_root_layout(fixture.path());
    fs::write(
        common::resource_root(fixture.path()).join("data"),
        b"path conflict",
    )
    .unwrap();

    let failed_output = run_preview(&executable, fixture.path());

    assert!(!failed_output.status.success());
    assert!(failed_output.stdout.is_empty());
    let error = stderr(&failed_output);
    for expected in [
        "布局预览失败 [APPLICATION_INITIALIZATION_FAILED]",
        "布局预览输出路径不可用",
        "输出文件：",
        "layout-preview.xlsx",
        "可写的普通目录",
        "已有预览保持不变",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    let resources = common::resource_root(fixture.path());
    assert_eq!(
        directory_entries(&resources),
        vec![
            resources.join("data"),
            resources.join("workbook-layout.xlsx"),
        ]
    );
}

fn run_preview(executable: &Path, working_directory: &Path) -> Output {
    Command::new(executable)
        .arg("layout-preview")
        .current_dir(working_directory)
        .output()
        .unwrap()
}

fn replace_layout_cell(path: &Path, sheet: &str, cell: &str, value: &str) {
    let replacement = path.with_file_name("workbook-layout.replacement.xlsx");
    edit_text_cell_to_new_file(path, &replacement, sheet, cell, value).unwrap();
    fs::remove_file(path).unwrap();
    fs::rename(replacement, path).unwrap();
}

fn assert_preview_header(path: &Path, expected: &str, expected_sheet_count: usize) {
    let mut workbook: Xlsx<BufReader<File>> = open_workbook(path).unwrap();
    assert_eq!(workbook.sheet_names().len(), expected_sheet_count);
    assert_eq!(workbook.sheet_names()[0], "配装计划");
    let range = workbook.worksheet_range("配装计划").unwrap();
    assert_eq!(
        range.get_value((0, 0)),
        Some(&Data::String(expected.to_owned()))
    );
    assert!(range.height() >= 2);
}

fn assert_ooxml_features(bytes: &[u8]) {
    let sheet = zip_part(bytes, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains("hidden=\"1\""));
    assert!(sheet.contains("<pane "));
    assert!(!sheet.contains("<sheetProtection "));
    let inventory = zip_part(bytes, "xl/worksheets/sheet2.xml");
    assert!(inventory.contains("<dataValidations "));
    assert!(sheet.contains("<tableParts "));

    let styles = zip_part(bytes, "xl/styles.xml");
    for color in [
        "FFF2F2F2", "FFE2F0D9", "FFFCE4D6", "FFFFC7CE", "FF9C5700", "FF9C0006",
    ] {
        assert!(styles.contains(color), "样式缺少颜色 {color}");
    }
    let workbook = zip_part(bytes, "xl/workbook.xml");
    assert!(workbook.contains("AZLW_Enum_"));
    assert!(zip_part(bytes, "xl/comments1.xml").contains("<comments"));
    let shared_strings = zip_part(bytes, "xl/sharedStrings.xml");
    assert!(shared_strings.contains("WorkbookProjectionV4.equipment_inventory[].source_ref"));
}

fn zip_part(bytes: &[u8], name: &str) -> String {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut part = archive.by_name(name).unwrap();
    let mut value = String::new();
    part.read_to_string(&mut value).unwrap();
    value
}

fn copy_root_layout(install: &Path) -> PathBuf {
    let destination = common::resource_root(install).join("workbook-layout.xlsx");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &destination,
    )
    .unwrap();
    destination
}

fn directory_entries(path: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    entries
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

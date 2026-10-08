//! 通过复制后的正式可执行文件验证布局升级的固定路径和非覆盖发布契约。

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use azur_lane_workbook::adapters::workbook::load_workbook_layout;
use azur_lane_workbook::application::WorkbookProjectionV4;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

mod common;

use common::TestDirectory;

#[test]
fn upgrades_the_root_layout_to_a_strictly_valid_independent_file() {
    let fixture = TestDirectory::new("azlw-layout-upgrade-cli-tests", "success");
    let executable = common::copy_executable(fixture.path());
    let source = copy_root_layout(fixture.path());
    let source_before = fs::read(&source).unwrap();
    let working_directory = fixture.path().join("unrelated-working-directory");
    fs::create_dir(&working_directory).unwrap();

    let output = Command::new(executable)
        .arg("layout-upgrade")
        .current_dir(working_directory)
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(&source).unwrap(), source_before);
    let updated =
        common::resource_root(fixture.path()).join("data/workbooks/workbook-layout.updated.xlsx");
    let layout =
        load_workbook_layout(&updated, &WorkbookProjectionV4::layout_registry().unwrap()).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "工作簿布局升级完成");
    assert_eq!(report["source_path"], "workbook-layout.xlsx");
    assert_eq!(
        report["output_path"],
        "data/workbooks/workbook-layout.updated.xlsx"
    );
    assert_eq!(report["source_schema_version"], 1);
    assert_eq!(report["target_schema_version"], 1);
    assert_eq!(report["preserved"]["sheets"], 10);
    assert_eq!(report["preserved"]["fields"], 403);
    assert_eq!(report["preserved"]["enum_options"], 55);
    assert_eq!(report["preserved"]["styles"], 6);
    assert_eq!(report["added"]["sheets"], 0);
    assert_eq!(report["added"]["fields"], 0);
    assert_eq!(report["added_items"], serde_json::json!([]));
    assert_eq!(report["output_content_sha256"], layout.content_sha256());
    assert_eq!(report["source_package_sha256"], sha256_hex(&source_before));
    assert_eq!(
        report["output_package_sha256"],
        sha256_hex(&fs::read(&updated).unwrap())
    );
    assert_eq!(directory_entries(updated.parent().unwrap()), vec![updated]);
}

#[test]
fn refuses_to_overwrite_an_existing_upgrade_output() {
    let fixture = TestDirectory::new("azlw-layout-upgrade-cli-tests", "existing-output");
    let executable = common::copy_executable(fixture.path());
    let source = copy_root_layout(fixture.path());
    let source_before = fs::read(&source).unwrap();
    let output_directory = common::resource_root(fixture.path()).join("data/workbooks");
    fs::create_dir_all(&output_directory).unwrap();
    let updated = output_directory.join("workbook-layout.updated.xlsx");
    fs::write(&updated, b"existing-output").unwrap();

    let output = Command::new(executable)
        .arg("layout-upgrade")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&source).unwrap(), source_before);
    assert_eq!(fs::read(&updated).unwrap(), b"existing-output");
    assert_eq!(directory_entries(&output_directory), vec![updated]);
    let error = stderr(&output);
    assert!(error.contains("布局升级失败 [LAYOUT_MIGRATION_FAILED]"));
    assert!(error.contains("输出文件："));
    assert!(error.contains("layout-upgrade"));
}

#[test]
fn rejects_unmodeled_package_parts_without_leaving_an_output() {
    let fixture = TestDirectory::new("azlw-layout-upgrade-cli-tests", "unsupported-part");
    let executable = common::copy_executable(fixture.path());
    let source = copy_root_layout(fixture.path());
    let with_extra_part = append_zip_part(&fs::read(&source).unwrap());
    fs::write(&source, &with_extra_part).unwrap();

    let output = Command::new(executable)
        .arg("layout-upgrade")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&source).unwrap(), with_extra_part);
    assert!(
        !common::resource_root(fixture.path())
            .join("data/workbooks")
            .exists()
    );
    let error = stderr(&output);
    assert!(error.contains("布局升级失败 [LAYOUT_MIGRATION_FAILED]"));
    assert!(error.contains("不支持迁移的部件：customXml/user-note.xml"));
    assert!(error.contains("不会静默丢弃"));
}

fn copy_root_layout(install: &Path) -> PathBuf {
    let destination = common::resource_root(install).join("workbook-layout.xlsx");
    fs::copy(root_layout(), &destination).unwrap();
    destination
}

fn root_layout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx")
}

fn append_zip_part(bytes: &[u8]) -> Vec<u8> {
    let mut source = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..source.len() {
        let entry = source.by_index(index).unwrap();
        writer.raw_copy_file(entry).unwrap();
    }
    writer
        .start_file("customXml/user-note.xml", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"<user-note/>").unwrap();
    writer.finish().unwrap().into_inner()
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

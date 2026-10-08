//! 通过复制后的正式可执行文件验证布局检查命令不依赖进程工作目录。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use azur_lane_workbook::adapters::workbook::edit_text_cell_to_new_file;

mod common;

use common::TestDirectory;

#[test]
fn checks_the_root_layout_and_prints_the_projection_summary() {
    let fixture = TestDirectory::new("azlw-layout-check-cli-tests", "valid");
    let executable: PathBuf = common::copy_executable(fixture.path());
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        common::resource_root(fixture.path()).join("workbook-layout.xlsx"),
    )
    .unwrap();
    let working_directory: PathBuf = fixture.path().join("unrelated-working-directory");
    fs::create_dir(&working_directory).unwrap();

    let output: Output = Command::new(executable)
        .arg("layout-check")
        .current_dir(working_directory)
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "工作簿布局检查通过");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["sheet_count"], 10);
    assert_eq!(report["field_count"], 403);
    assert_eq!(report["enum_option_count"], 55);
    assert_eq!(report["style_count"], 6);
    assert_eq!(report["content_sha256"].as_str().unwrap().len(), 64);
}

#[test]
fn reports_the_sheet_row_key_cause_and_repair_for_an_invalid_layout() {
    let fixture = TestDirectory::new("azlw-layout-check-cli-tests", "invalid");
    let executable: PathBuf = common::copy_executable(fixture.path());
    edit_text_cell_to_new_file(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &common::resource_root(fixture.path()).join("workbook-layout.xlsx"),
        "字段设置",
        "B2",
        "unknown_field",
    )
    .unwrap();

    let output: Output = Command::new(executable)
        .arg("layout-check")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: String = stderr(&output);
    for expected in [
        "布局检查失败 [LAYOUT_INVALID]",
        "工作表：字段设置",
        "配置行：2",
        "稳定键：",
        "unknown_field",
        "原因：",
        "修复建议：",
        "layout-check",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

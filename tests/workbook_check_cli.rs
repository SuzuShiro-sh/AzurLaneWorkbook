//! 通过复制后的正式可执行文件验证工作簿检查命令的路径、输入和运行态边界。

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use azur_lane_workbook::adapters::workbook::edit_text_cell_to_new_file;

mod common;

use common::TestDirectory;

#[test]
fn rejects_a_workbook_outside_the_controlled_directory_before_runtime_access() {
    let fixture = TestDirectory::new("azlw-workbook-check-cli-tests", "outside-path");
    let executable = common::copy_executable(fixture.path());

    let output = Command::new(executable)
        .args(["check", "../outside.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿检查失败 [WORKBOOK_INVALID]",
        "阶段：workbook.select",
        "data/workbooks",
        "只传入文件名",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    assert!(!error.contains("game.bootstrap"));
    assert!(!error.contains("plan.check"));
}

#[test]
fn rejects_a_missing_workbook_before_runtime_access() {
    let fixture = TestDirectory::new("azlw-workbook-check-cli-tests", "missing-workbook");
    let executable = common::copy_executable(fixture.path());
    fs::create_dir_all(common::resource_root(fixture.path()).join("data/workbooks")).unwrap();

    let output = Command::new(executable)
        .args(["check", "missing.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿检查失败 [WORKBOOK_INVALID]",
        "阶段：workbook.select",
        "missing.xlsx",
        "data/workbooks",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    assert!(!error.contains("game.bootstrap"));
    assert!(!error.contains("plan.check"));
}

#[test]
fn rejects_invalid_workbook_content_before_reading_game_state() {
    let fixture = TestDirectory::new("azlw-workbook-check-cli-tests", "invalid-content");
    let executable = common::prepare_install(fixture.path());
    let workbook_directory = common::resource_root(fixture.path()).join("data/workbooks");
    fs::create_dir_all(&workbook_directory).unwrap();
    fs::write(
        workbook_directory.join("invalid.xlsx"),
        b"not an xlsx package",
    )
    .unwrap();

    let output = Command::new(executable)
        .args(["check", "invalid.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿检查失败 [WORKBOOK_INVALID]",
        "阶段：workbook.plan.load",
        "工作簿文件：",
        "数据工作簿未通过严格读取校验",
        "修正该工作簿",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    assert!(!error.contains("game.bootstrap"));
    assert!(!error.contains("plan.check"));
}

#[test]
fn check_save_rejects_invalid_workbook_without_creating_history() {
    let fixture = TestDirectory::new("azlw-workbook-check-cli-tests", "invalid-content-save");
    let executable = common::prepare_install(fixture.path());
    let workbook_directory = common::resource_root(fixture.path()).join("data/workbooks");
    fs::create_dir_all(&workbook_directory).unwrap();
    fs::write(
        workbook_directory.join("invalid.xlsx"),
        b"not an xlsx package",
    )
    .unwrap();

    let output = Command::new(executable)
        .args(["check-save", "invalid.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("工作簿检查失败 [WORKBOOK_INVALID]"));
    assert!(error.contains("工作簿未通过严格读取校验"));
    assert!(
        !common::resource_root(fixture.path())
            .join("data/history")
            .exists()
    );
}

#[test]
fn identifies_a_root_layout_failure_as_a_layout_file_error() {
    let fixture = TestDirectory::new("azlw-workbook-check-cli-tests", "invalid-layout");
    let executable = common::copy_executable(fixture.path());
    let source_layout = Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx");
    let resources = common::resource_root(fixture.path());
    edit_text_cell_to_new_file(
        &source_layout,
        &resources.join("workbook-layout.xlsx"),
        "字段设置",
        "B2",
        "unknown_field",
    )
    .unwrap();
    let workbook_directory = resources.join("data/workbooks");
    fs::create_dir_all(&workbook_directory).unwrap();
    fs::copy(&source_layout, workbook_directory.join("input.xlsx")).unwrap();

    let output = Command::new(executable)
        .args(["check", "input.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿检查失败 [LAYOUT_INVALID]",
        "阶段：workbook.layout.load",
        "布局文件：",
        "unknown_field",
        "layout-check",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    assert!(!error.contains("工作簿文件："));
    assert!(!error.contains("game.bootstrap"));
    assert!(!error.contains("plan.check"));
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

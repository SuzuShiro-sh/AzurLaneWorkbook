//! 验证 workbooks 命令通过应用服务读取受控工作簿目录。

use std::fs;
use std::process::Command;
mod common;

#[test]
fn lists_sorted_xlsx_workbooks_and_ignores_other_regular_files() {
    let fixture = common::TestDirectory::new("azlw-workbook-catalog-cli-tests", "list");
    let executable = common::copy_executable(fixture.path());
    let workbook_directory = common::resource_root(fixture.path()).join("data/workbooks");
    fs::create_dir_all(&workbook_directory).unwrap();
    fs::write(workbook_directory.join("zeta.xlsx"), b"123").unwrap();
    fs::write(workbook_directory.join("Alpha.XLSX"), b"12").unwrap();
    fs::write(workbook_directory.join("notes.txt"), b"ignored").unwrap();
    fs::write(workbook_directory.join("~$zeta.xlsx"), b"lock").unwrap();

    let output = Command::new(executable)
        .arg("workbooks")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "工作簿目录读取完成");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["directory"], "data/workbooks");
    assert_eq!(report["workbooks"][0]["name"], "Alpha.XLSX");
    assert_eq!(
        report["workbooks"][0]["relative_path"],
        "data/workbooks/Alpha.XLSX"
    );
    assert_eq!(report["workbooks"][0]["size_bytes"], 2);
    assert_eq!(report["workbooks"][0]["kind"], "workbook");
    assert_eq!(report["workbooks"][1]["name"], "zeta.xlsx");
    assert_eq!(report["workbooks"][1]["size_bytes"], 3);
    assert_eq!(report["workbooks"].as_array().unwrap().len(), 2);
}

#[test]
fn reports_an_empty_catalog_without_creating_runtime_data() {
    let fixture = common::TestDirectory::new("azlw-workbook-catalog-cli-tests", "empty");
    let executable = common::copy_executable(fixture.path());

    let output = Command::new(executable)
        .arg("workbooks")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["workbooks"].as_array().unwrap().len(), 0);
    assert!(!common::resource_root(fixture.path()).join("data").exists());
}

#[cfg(unix)]
#[test]
fn rejects_a_workbook_directory_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = common::TestDirectory::new("azlw-workbook-catalog-cli-tests", "symlink");
    let executable = common::copy_executable(fixture.path());
    let resources = common::resource_root(fixture.path());
    fs::create_dir_all(resources.join("data")).unwrap();
    let outside = fixture.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, resources.join("data/workbooks")).unwrap();

    let output = Command::new(executable)
        .arg("workbooks")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿目录读取失败 [WORKBOOK_INVALID]",
        "阶段：workbook.catalog",
        "data/workbooks",
        "普通目录",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

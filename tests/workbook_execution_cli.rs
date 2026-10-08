//! 通过复制后的正式可执行文件验证工作簿执行命令的路径和零写失败边界。

use std::fs;
use std::process::{Command, Output};

mod common;

use common::TestDirectory;

#[test]
fn rejects_a_workbook_outside_the_controlled_directory_before_runtime_access() {
    let fixture = TestDirectory::new("azlw-workbook-execution-cli-tests", "outside-path");
    let executable = common::copy_executable(fixture.path());

    let output = Command::new(executable)
        .args(["execute", "../outside.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿执行失败 [WORKBOOK_INVALID]",
        "阶段：workbook.select",
        "data/workbooks",
        "重新运行 execute",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    assert!(!error.contains("game.bootstrap"));
    assert!(
        !common::resource_root(fixture.path())
            .join("data/backups")
            .exists()
    );
    assert!(
        !common::resource_root(fixture.path())
            .join("data/history")
            .exists()
    );
}

#[test]
fn rejects_a_missing_workbook_before_runtime_access() {
    let fixture = TestDirectory::new("azlw-workbook-execution-cli-tests", "missing-workbook");
    let executable = common::copy_executable(fixture.path());
    fs::create_dir_all(common::resource_root(fixture.path()).join("data/workbooks")).unwrap();

    let output = Command::new(executable)
        .args(["execute", "missing.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("工作簿执行失败 [WORKBOOK_INVALID]"));
    assert!(error.contains("missing.xlsx"));
    assert!(!error.contains("game.bootstrap"));
    assert!(
        !common::resource_root(fixture.path())
            .join("data/backups")
            .exists()
    );
    assert!(
        !common::resource_root(fixture.path())
            .join("data/history")
            .exists()
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn missing_execution_capability_creates_no_backup_or_history() {
    let fixture = TestDirectory::new(
        "azlw-workbook-execution-cli-tests",
        "missing-execution-port",
    );
    let executable = common::copy_executable(fixture.path());
    fs::create_dir_all(common::resource_root(fixture.path()).join("data/workbooks")).unwrap();
    fs::write(
        common::resource_root(fixture.path()).join("data/workbooks/plan.xlsx"),
        b"path validation fixture",
    )
    .unwrap();

    let output = Command::new(executable)
        .args(["execute", "plan.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    for expected in [
        "工作簿执行失败 [CAPABILITY_MISSING]",
        "阶段：plan.execute",
        "缺少端口：execution",
        "Windows 完整发布环境",
        "本次未发送游戏写命令",
    ] {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
    assert!(
        !common::resource_root(fixture.path())
            .join("data/backups")
            .exists()
    );
    assert!(
        !common::resource_root(fixture.path())
            .join("data/history")
            .exists()
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

//! 验证 open 命令的受控路径、默认程序调用和 data 目录只读边界。

use std::fs;
use std::process::{Command, Output};
mod common;

#[test]
fn rejects_paths_outside_the_controlled_workbook_directory() {
    let fixture = common::TestDirectory::new("azlw-workbook-open-cli-tests", "outside");
    let executable = common::copy_executable(fixture.path());

    let output = Command::new(executable)
        .args(["open", "../outside.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_failure(
        &output,
        [
            "工作簿打开失败 [WORKBOOK_INVALID]",
            "阶段：workbook.select",
            "data/workbooks",
            "只传入文件名",
        ],
    );
}

#[test]
fn rejects_missing_workbooks_before_starting_a_launcher() {
    let fixture = common::TestDirectory::new("azlw-workbook-open-cli-tests", "missing");
    let executable = common::copy_executable(fixture.path());
    fs::create_dir_all(common::resource_root(fixture.path()).join("data/workbooks")).unwrap();

    let output = Command::new(executable)
        .args(["open", "missing.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_failure(
        &output,
        [
            "工作簿打开失败 [WORKBOOK_INVALID]",
            "阶段：workbook.select",
            "missing.xlsx",
            "data/workbooks",
        ],
    );
}

#[cfg(target_os = "linux")]
#[test]
fn passes_the_canonical_workbook_to_the_default_launcher_without_writing_data() {
    let fixture = common::TestDirectory::new("azlw-workbook-open-cli-tests", "launch");
    let executable = common::copy_executable(fixture.path());
    let workbook = common::resource_root(fixture.path()).join("data/workbooks/plan.xlsx");
    fs::create_dir_all(workbook.parent().unwrap()).unwrap();
    fs::write(&workbook, b"fixture xlsx bytes").unwrap();
    let launcher_directory = fixture.path().join("launcher");
    fs::create_dir_all(&launcher_directory).unwrap();
    let marker = fixture.path().join("launcher-marker.txt");
    let launcher = launcher_directory.join("xdg-open");
    fs::write(
        &launcher,
        b"#!/bin/sh\nprintf '%s' \"$1\" > \"$AZLW_OPEN_MARKER\"\n",
    )
    .unwrap();
    make_executable(&launcher);
    let before = data_snapshot(fixture.path());

    let output = Command::new(&executable)
        .args(["open", "plan.xlsx"])
        .env("PATH", &launcher_directory)
        .env("AZLW_OPEN_MARKER", &marker)
        .current_dir(fixture.path().join("unrelated"))
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "工作簿已交给系统默认程序打开");
    assert_eq!(report["workbook_path"], "data/workbooks/plan.xlsx");
    assert_eq!(report["launcher"], "xdg-open");
    wait_for_marker(&marker);
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        fs::canonicalize(workbook).unwrap()
    );
    assert_eq!(data_snapshot(fixture.path()), before);
}

#[cfg(target_os = "linux")]
#[test]
fn reports_launcher_start_failure_without_writing_data() {
    let fixture = common::TestDirectory::new("azlw-workbook-open-cli-tests", "launcher-failure");
    let executable = common::copy_executable(fixture.path());
    let workbook = common::resource_root(fixture.path()).join("data/workbooks/plan.xlsx");
    fs::create_dir_all(workbook.parent().unwrap()).unwrap();
    fs::write(&workbook, b"fixture xlsx bytes").unwrap();
    let empty_path = fixture.path().join("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let before = data_snapshot(fixture.path());

    let output = Command::new(executable)
        .args(["open", "plan.xlsx"])
        .env("PATH", empty_path)
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_failure(
        &output,
        [
            "工作簿打开失败 [WORKBOOK_OPEN_FAILED]",
            "阶段：workbook.open",
            "系统默认程序",
            "工作簿内容未修改",
        ],
    );
    assert_eq!(data_snapshot(fixture.path()), before);
}

fn assert_failure<const N: usize>(output: &Output, expected: [&str; N]) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(output);
    for fragment in expected {
        assert!(error.contains(fragment), "诊断缺少 {fragment:?}: {error}");
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[cfg(target_os = "linux")]
fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
enum DataEntry {
    Directory,
    File(Vec<u8>),
}

#[cfg(target_os = "linux")]
fn data_snapshot(root: &Path) -> Option<Vec<(String, DataEntry)>> {
    let data = common::resource_root(root).join("data");
    if !data.exists() {
        return None;
    }
    let mut entries = Vec::new();
    collect_entries(&data, &data, &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Some(entries)
}

#[cfg(target_os = "linux")]
fn collect_entries(root: &Path, directory: &Path, entries: &mut Vec<(String, DataEntry)>) {
    let mut children: Vec<fs::DirEntry> = fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        if metadata.is_dir() {
            entries.push((relative, DataEntry::Directory));
            collect_entries(root, &path, entries);
        } else {
            entries.push((relative, DataEntry::File(fs::read(path).unwrap())));
        }
    }
}

#[cfg(target_os = "linux")]
fn wait_for_marker(path: &Path) {
    for _ in 0..100 {
        if path.is_file() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("默认打开器没有写入测试标记: {}", path.display());
}

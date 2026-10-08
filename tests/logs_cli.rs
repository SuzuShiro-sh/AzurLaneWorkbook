//! 通过复制后的正式可执行文件验证 logs 命令的摘要和路径边界。

use std::fs;
use std::process::{Command, Output};

mod common;

#[test]
fn filters_log_details_before_selecting_tail() {
    let fixture = common::TestDirectory::new("azlw-logs-cli-tests", "details");
    let executable = common::copy_executable(fixture.path());
    let logs = common::resource_root(fixture.path()).join("data/logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(logs.join("001-runtime.jsonl"),"{\"status\":\"error\",\"id\":1}\n{\"status\":\"error\",\"id\":2}\n{\"status\":\"ok\",\"id\":3}\n").unwrap();
    let output = Command::new(executable)
        .args([
            "logs",
            "show",
            "001-runtime.jsonl",
            "--tail",
            "1",
            "--status",
            "error",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["records"],
        serde_json::json!([{"status":"error","id":2}])
    );
    assert_eq!(fs::read_dir(logs).unwrap().count(), 1);
}

#[test]
fn lists_known_logs_without_printing_their_contents() {
    let fixture = common::TestDirectory::new("azlw-logs-cli-tests", "valid");
    let executable = common::copy_executable(fixture.path());
    let logs = common::resource_root(fixture.path()).join("data/logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        logs.join("003-runtime.jsonl"),
        br#"{"stage":"host.start","details":{"serial":"127.0.0.1:16384"}}"#,
    )
    .unwrap();
    fs::write(logs.join("001-adb.log"), b"adb secret output").unwrap();

    fs::write(logs.join("002-app.log"), b"operation private details").unwrap();

    let output = Command::new(executable)
        .arg("logs")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "日志目录读取完成");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["directory"], "data/logs");
    assert_eq!(report["entries"].as_array().unwrap().len(), 3);
    assert_eq!(report["entries"][0]["kind"], "adb_server");
    assert_eq!(report["entries"][1]["kind"], "operation");
    assert_eq!(report["entries"][2]["kind"], "runtime_probe");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("operation private details"));
    assert!(report["entries"][0]["file_sha256"].as_str().unwrap().len() == 64);
    let output_text = String::from_utf8(output.stdout).unwrap();
    assert!(!output_text.contains("adb secret output"));
    assert!(!output_text.contains("127.0.0.1:16384"));
    assert!(!output_text.contains(fixture.path().to_string_lossy().as_ref()));
}

#[test]
fn reports_an_empty_log_directory_without_creating_runtime_data() {
    let fixture = common::TestDirectory::new("azlw-logs-cli-tests", "empty");
    let executable = common::copy_executable(fixture.path());

    let output = Command::new(executable)
        .arg("logs")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"].as_array().unwrap().len(), 0);
    assert!(!common::resource_root(fixture.path()).join("data").exists());
}

#[test]
fn rejects_unknown_log_files_instead_of_skipping_them() {
    let fixture = common::TestDirectory::new("azlw-logs-cli-tests", "invalid");
    let executable = common::copy_executable(fixture.path());
    let logs = common::resource_root(fixture.path()).join("data/logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(logs.join("notes.txt"), b"unexpected").unwrap();

    let output = Command::new(executable)
        .arg("logs")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "日志目录部分文件读取失败");
    assert_eq!(report["entries"].as_array().unwrap().len(), 0);
    assert_eq!(
        report["failures"][0]["relative_path"],
        "data/logs/notes.txt"
    );
    assert_eq!(report["failures"][0]["error_code"], "LOG_READ_FAILED");
    assert!(
        report["failures"][0]["message"]
            .as_str()
            .unwrap()
            .contains("日志格式")
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("unexpected"));
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn numbered_logs_share_the_catalog() {
    let fixture = common::TestDirectory::new("azlw-logs-cli-tests", "numbered");
    let executable = common::copy_executable(fixture.path());
    let logs = common::resource_root(fixture.path()).join("data/logs");
    fs::create_dir_all(&logs).unwrap();
    let names = [
        "001-app.log",
        "002-adb.log",
        "010-runtime.jsonl",
        "1000-adb.log",
    ];
    for name in names {
        fs::write(logs.join(name), b"private log").unwrap();
    }
    let output = Command::new(executable)
        .arg("logs")
        .current_dir(fixture.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let actual: Vec<_> = report["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["relative_path"].as_str().unwrap())
        .collect();
    let mut expected: Vec<_> = names
        .into_iter()
        .map(|name| format!("data/logs/{name}"))
        .collect();
    expected.sort();
    assert_eq!(actual, expected);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private log"));
}

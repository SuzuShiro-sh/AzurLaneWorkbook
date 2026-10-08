//! 通过复制后的正式可执行文件验证 history 命令的目录、schema 和摘要边界。

use azur_lane_workbook::application::{EXECUTION_SCHEMA_VERSION, PLAN_SCHEMA_VERSION};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod common;

#[test]
fn reads_selected_history_details_without_creating_logs() {
    let fixture = common::TestDirectory::new("azlw-history-cli-tests", "details");
    let executable = common::copy_executable(fixture.path());
    let root = common::resource_root(fixture.path());
    let history = root.join("data/history");
    fs::create_dir_all(&history).unwrap();
    write_check(&history, 1_700_000_000_100);
    let output = Command::new(executable)
        .args([
            "history",
            "show",
            "1700000000100-check.json",
            "--fields",
            "schema_version",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"schema_version":1})
    );
    assert!(!root.join("data/logs").exists());
}

#[test]
fn lists_check_and_execution_history_in_newest_first_order() {
    let fixture = common::TestDirectory::new("azlw-history-cli-tests", "valid");
    let executable = common::copy_executable(fixture.path());
    let history = common::resource_root(fixture.path()).join("data/history");
    fs::create_dir_all(&history).unwrap();
    write_check(&history, 1_700_000_000_100);
    write_execution(&history, 1_700_000_000_200);

    let output = Command::new(executable)
        .arg("history")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "历史目录读取完成");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["directory"], "data/history");
    assert_eq!(report["entries"].as_array().unwrap().len(), 2);
    assert_eq!(report["entries"][0]["kind"], "execution");
    assert_eq!(
        report["entries"][0]["timestamp_unix_millis"],
        1_700_000_000_200_i64
    );
    assert_eq!(report["entries"][1]["kind"], "check");
    assert_eq!(
        report["entries"][1]["timestamp_unix_millis"],
        1_700_000_000_100_i64
    );
    let execution = &report["entries"][0];
    assert_eq!(execution["schema_version"], 1);
    assert_eq!(execution["workbook_name"], "plan.xlsx");
    assert_eq!(
        execution["relative_path"],
        "data/history/1700000000200-exec.json"
    );
    assert_eq!(execution["plan_content_sha256"], "b".repeat(64));
    assert_eq!(execution["execution_status"], "success");
    assert_eq!(execution["may_have_writes"], false);
    assert_eq!(execution["target_fingerprint_sha256"], "d".repeat(64));
    assert!(execution["size_bytes"].as_u64().unwrap() > 0);
    assert_eq!(execution["file_sha256"].as_str().unwrap().len(), 64);
    let check = &report["entries"][1];
    assert_eq!(check["schema_version"], 1);
    assert_eq!(check["workbook_name"], "plan.xlsx");
    assert_eq!(
        check["relative_path"],
        "data/history/1700000000100-check.json"
    );
    assert_eq!(check["plan_content_sha256"], "a".repeat(64));
    assert!(check["execution_status"].is_null());
    assert!(check["may_have_writes"].is_null());
    assert!(check["target_fingerprint_sha256"].is_null());
    assert!(check["size_bytes"].as_u64().unwrap() > 0);
    assert_eq!(check["file_sha256"].as_str().unwrap().len(), 64);
    let rendered = report.to_string();
    assert!(!rendered.contains(fixture.path().to_string_lossy().as_ref()));
    assert!(!rendered.contains(":\\"));
}

#[test]
fn reports_an_empty_history_directory_without_creating_runtime_data() {
    let fixture = common::TestDirectory::new("azlw-history-cli-tests", "empty");
    let executable = common::copy_executable(fixture.path());

    let output = Command::new(executable)
        .arg("history")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["entries"].as_array().unwrap().len(), 0);
    assert!(!common::resource_root(fixture.path()).join("data").exists());
}

#[test]
fn rejects_an_unrecognized_history_file_instead_of_skipping_it() {
    let fixture = common::TestDirectory::new("azlw-history-cli-tests", "invalid");
    let executable = common::copy_executable(fixture.path());
    let history = common::resource_root(fixture.path()).join("data/history");
    fs::create_dir_all(&history).unwrap();
    fs::write(history.join("notes.txt"), b"unexpected").unwrap();

    let output = Command::new(executable)
        .arg("history")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "历史目录部分文件读取失败");
    assert_eq!(report["entries"].as_array().unwrap().len(), 0);
    assert_eq!(
        report["failures"][0]["relative_path"],
        "data/history/notes.txt"
    );
    assert_eq!(report["failures"][0]["error_code"], "HISTORY_READ_FAILED");
    assert!(
        report["failures"][0]["message"]
            .as_str()
            .unwrap()
            .contains("check.json")
    );
}

fn write_check(history: &Path, timestamp: i64) {
    let digest = "a".repeat(64);
    let document = serde_json::json!({
        "schema_version": 1,
        "plan_schema_version": PLAN_SCHEMA_VERSION,
        "workbook_name": "plan.xlsx",
        "checked_at_unix_millis": timestamp,
        "plan_content_sha256": digest,
        "report": {},
    });
    fs::write(
        history.join(format!("{timestamp:03}-check.json")),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
}

fn write_execution(history: &Path, timestamp: i64) {
    let plan_digest = "b".repeat(64);
    let report_digest = "c".repeat(64);
    let document = serde_json::json!({
        "schema_version": 1,
        "execution_schema_version": EXECUTION_SCHEMA_VERSION,
        "plan_schema_version": PLAN_SCHEMA_VERSION,
        "workbook_name": "plan.xlsx",
        "recorded_at_unix_millis": timestamp,
        "target_fingerprint_sha256": "d".repeat(64),
        "plan_content_sha256": plan_digest,
        "report_content_sha256": report_digest,
        "report_status": "success",
        "may_have_writes": false,
        "report": {},
    });
    fs::write(
        history.join(format!("{timestamp:03}-exec.json")),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

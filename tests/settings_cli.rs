//! 通过复制后的正式可执行文件验证 settings 命令只输出脱敏配置摘要。

use std::fs;
use std::process::{Command, Output};
mod common;

const AUTO_SETTINGS: &str = r#"{
    "schema_version": 1,
    "device": {
        "mode": "auto",
        "adb_path": "runtime/adb/adb.exe",
        "serial": "127.0.0.1:16384",
        "instance": "0",
        "game_package": "com.example.game"
    },
    "runtime": {
        "connect_timeout_seconds": 30,
        "startup_timeout_seconds": 180
    }
}"#;

#[test]
fn prints_a_deidentified_settings_summary() {
    let fixture = common::TestDirectory::new("azlw-settings-cli-tests", "valid");
    let executable = common::copy_executable(fixture.path());
    fs::write(
        common::resource_root(fixture.path()).join("settings.json"),
        AUTO_SETTINGS,
    )
    .unwrap();

    let output = Command::new(executable)
        .arg("settings")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "运行设置读取完成");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["settings"]["device_mode"], "auto");
    assert_eq!(report["settings"]["adb_path_configured"], true);
    assert_eq!(report["settings"]["serial_configured"], true);
    assert_eq!(report["settings"]["instance_configured"], true);
    assert_eq!(report["settings"]["game_package_configured"], true);
    assert_eq!(report["settings"]["connect_timeout_seconds"], 30);
    assert!(!report.to_string().contains("127.0.0.1:16384"));
    assert!(!report.to_string().contains("com.example.game"));
}

#[test]
fn reports_invalid_settings_without_leaking_the_tool_root_path() {
    let fixture = common::TestDirectory::new("azlw-settings-cli-tests", "invalid");
    let executable = common::copy_executable(fixture.path());
    fs::write(
        common::resource_root(fixture.path()).join("settings.json"),
        b"{",
    )
    .unwrap();

    let output = Command::new(executable)
        .arg("settings")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("运行设置读取失败 [SETTINGS_INVALID]"));
    assert!(error.contains("阶段：settings.read"));
    assert!(error.contains("settings.json"));
    assert!(!error.contains(fixture.path().to_string_lossy().as_ref()));
}

#[test]
fn reports_an_unsafe_adb_path_without_leaking_the_tool_root_path() {
    let fixture = common::TestDirectory::new("azlw-settings-cli-tests", "unsafe-adb");
    let executable = common::copy_executable(fixture.path());
    let settings = AUTO_SETTINGS.replace(
        "\"adb_path\": \"runtime/adb/adb.exe\"",
        "\"adb_path\": \"../outside/adb.exe\"",
    );
    fs::write(
        common::resource_root(fixture.path()).join("settings.json"),
        settings,
    )
    .unwrap();

    let output = Command::new(executable)
        .arg("settings")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("运行设置读取失败 [SETTINGS_INVALID]"));
    assert!(error.contains("device.adb_path"));
    assert!(!error.contains(fixture.path().to_string_lossy().as_ref()));
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn reports_log_creation_failure_without_changing_the_settings_result() {
    let fixture = common::TestDirectory::new("azlw-settings-cli-tests", "log-blocked");
    let executable = common::copy_executable(fixture.path());
    let root = common::resource_root(fixture.path());
    fs::write(root.join("settings.json"), AUTO_SETTINGS).unwrap();
    let data = root.join("data");
    fs::write(&data, b"not-a-directory").unwrap();

    let output = Command::new(&executable)
        .arg("settings")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["message"], "运行设置读取完成");
    assert!(!report.to_string().contains("操作日志失败"));
    let error = stderr(&output);
    assert!(error.contains("操作日志失败"));
    assert!(error.contains("创建操作日志失败"));
    assert!(error.contains("路径段必须是普通目录"));
    assert_eq!(fs::read(&data).unwrap(), b"not-a-directory");
}

#[test]
fn reports_log_creation_failure_together_with_invalid_settings() {
    let fixture = common::TestDirectory::new("azlw-settings-cli-tests", "log-and-settings");
    let executable = common::copy_executable(fixture.path());
    let root = common::resource_root(fixture.path());
    fs::write(root.join("settings.json"), b"{").unwrap();
    fs::write(root.join("data"), b"not-a-directory").unwrap();

    let output = Command::new(executable)
        .arg("settings")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(&output);
    assert!(error.contains("运行设置读取失败 [SETTINGS_INVALID]"));
    assert!(error.contains("操作日志失败"));
    assert!(error.contains("创建操作日志失败"));
    assert!(error.contains("路径段必须是普通目录"));
}

#[test]
fn preferences_are_persisted_and_invalid_values_leave_the_file_unchanged() {
    let fixture = common::TestDirectory::new("azlw-settings-cli-tests", "preferences");
    let executable = common::copy_executable(fixture.path());
    let path = common::resource_root(fixture.path()).join("settings.json");
    fs::write(&path, AUTO_SETTINGS).unwrap();
    let run = |arguments: &[&str]| Command::new(&executable).args(arguments).output().unwrap();
    let initial = run(&["settings", "preferences"]);
    assert!(initial.status.success(), "{}", stderr(&initial));
    let defaults: serde_json::Value = serde_json::from_slice(&initial.stdout).unwrap();
    assert_eq!(defaults["detailed_diagnostics"], true);
    assert_eq!(defaults["unload_after_sync"], false);
    assert_eq!(defaults["acquisition_update_policy"], "use_cache");
    for (key, value) in [
        ("ship_acquisition_enabled", "true"),
        ("acquisition_update_policy", "refresh"),
        ("detailed_diagnostics", "false"),
        ("unload_after_sync", "true"),
    ] {
        let result = run(&["settings", "set", key, value]);
        assert!(result.status.success(), "{}", stderr(&result));
    }
    let saved = fs::read(&path).unwrap();
    let current = run(&["settings", "preferences"]);
    let preferences: serde_json::Value = serde_json::from_slice(&current.stdout).unwrap();
    assert_eq!(preferences["ship_acquisition_enabled"], true);
    assert_eq!(preferences["acquisition_update_policy"], "refresh");
    assert_eq!(preferences["detailed_diagnostics"], false);
    assert_eq!(preferences["unload_after_sync"], true);
    for arguments in [
        ["settings", "set", "unload_after_sync", "yes"],
        ["settings", "set", "acquisition_update_policy", "weekly"],
        ["settings", "set", "unknown", "true"],
    ] {
        assert!(!run(&arguments).status.success());
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
}

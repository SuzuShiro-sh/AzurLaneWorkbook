use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::process::{Command, Stdio};

use super::{AdbBundle, IsolatedAdbError, parse_revision};
use crate::config::validate_serial;

fn config(root: &Path) -> crate::AdbConfig {
    crate::AdbConfig {
        root: root.into(),
        executable: "runtime/custom-adb/adb.exe".into(),
        state_directory: "state".into(),
        log_directory: "logs".into(),
    }
}
#[derive(Debug)]
struct TestLog;
impl crate::AdbLogSink for TestLog {
    fn create(&self, directory: &Path, name: &str) -> std::io::Result<(PathBuf, std::fs::File)> {
        let path = directory.join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((path, file))
    }
    fn write_event(
        &self,
        file: &mut std::fs::File,
        stage: &str,
        status: &str,
        details: serde_json::Value,
    ) -> std::io::Result<()> {
        use std::io::Write;
        serde_json::to_writer(
            &mut *file,
            &serde_json::json!({"stage":stage,"status":status,"details":details}),
        )?;
        file.write_all(b"\n")
    }
}

/// 只接受唯一、受限且非空的 Platform-Tools 修订号。
#[test]
fn revision_parser_rejects_missing_duplicate_and_ambiguous_values() {
    assert_eq!(
        parse_revision("Pkg.UserSrc=false\nPkg.Revision=37.0.1\n").unwrap(),
        "37.0.1"
    );
    assert!(parse_revision("Pkg.UserSrc=false\n").is_err());
    assert!(parse_revision("Pkg.Revision=37.0.1\nPkg.Revision=37.0.2\n").is_err());
    assert!(parse_revision("Pkg.Revision=37.0.1-beta\n").is_err());
    assert!(parse_revision("Pkg.Revision=37..1\n").is_err());
}

/// 独立服务只连接 调用方给出的非零回环 TCP 地址。
#[test]
fn serial_parser_rejects_remote_and_zero_port_targets() {
    assert_eq!(
        validate_serial("127.0.0.1:16384").unwrap().to_string(),
        "127.0.0.1:16384"
    );
    assert!(validate_serial("192.0.2.1:16384").is_err());
    assert!(validate_serial("127.0.0.1:0").is_err());
    assert!(validate_serial("not-an-address").is_err());
}

#[test]
#[ignore = "AZLW_MEASURE_MODE=adb"]
fn measure_bundle_load_when_requested() {
    if std::env::var("AZLW_MEASURE_MODE").ok().as_deref() != Some("adb") {
        return;
    }
    let root = test_root("bundle-load-measure");
    let bundle_root = root.join("runtime/custom-adb");
    fs::create_dir_all(&bundle_root).unwrap();
    fs::write(bundle_root.join("adb.exe"), b"fixture").unwrap();
    fs::write(bundle_root.join("AdbWinApi.dll"), b"fixture").unwrap();
    fs::write(bundle_root.join("NOTICE.txt"), b"fixture").unwrap();
    fs::write(
        bundle_root.join("source.properties"),
        b"Pkg.Revision=37.0.1\n",
    )
    .unwrap();
    let started = std::time::Instant::now();
    let first = AdbBundle::load(config(&root), std::sync::Arc::new(TestLog)).unwrap();
    let first_us = started.elapsed().as_micros();
    let repeats = 100_u32;
    let batch_started = std::time::Instant::now();
    for _ in 0..repeats {
        let again = AdbBundle::load(config(&root), std::sync::Arc::new(TestLog)).unwrap();
        assert_eq!(again.revision(), first.revision());
    }
    let batch_us = batch_started.elapsed().as_micros();
    println!(
        "\nMEASURE stage=adb_bundle_load first_us={first_us} repeats={repeats} batch_us={batch_us} per_repeat_us={} server_start=not_measured auth=not_measured",
        batch_us / u128::from(repeats)
    );
}

/// 自定义 adb.exe 只能与同目录的 DLL、许可和版本属性组成完整闭包。
#[test]
fn custom_executable_loads_its_sibling_bundle() {
    let root: PathBuf = test_root("custom-bundle");
    let bundle_root: PathBuf = root.join("runtime/custom-adb");
    fs::create_dir_all(&bundle_root).unwrap();
    fs::write(bundle_root.join("adb.exe"), b"fixture").unwrap();
    fs::write(bundle_root.join("AdbWinApi.dll"), b"fixture").unwrap();
    fs::write(bundle_root.join("NOTICE.txt"), b"fixture").unwrap();
    fs::write(
        bundle_root.join("source.properties"),
        b"Pkg.Revision=37.0.1\n",
    )
    .unwrap();

    let bundle: AdbBundle = AdbBundle::load(config(&root), std::sync::Arc::new(TestLog)).unwrap();
    let relative_files: Vec<String> = bundle
        .files()
        .into_iter()
        .map(|path: &Path| {
            path.strip_prefix(bundle.tool_root())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();

    assert_eq!(bundle.revision(), "37.0.1");
    assert_eq!(
        relative_files,
        [
            "runtime/custom-adb/adb.exe",
            "runtime/custom-adb/AdbWinApi.dll",
            "runtime/custom-adb/NOTICE.txt",
            "runtime/custom-adb/source.properties",
        ]
    );
    for (state, logs) in [
        ("runtime", "logs"),
        ("runtime/custom-adb/temp", "logs"),
        ("state", "state/logs"),
    ] {
        let mut overlapping = config(&root);
        overlapping.state_directory = state.into();
        overlapping.log_directory = logs.into();
        assert!(matches!(
            AdbBundle::load(overlapping, std::sync::Arc::new(TestLog)),
            Err(IsolatedAdbError::InvalidBundle { .. })
        ));
        assert_eq!(fs::read(bundle_root.join("adb.exe")).unwrap(), b"fixture");
    }
    drop(bundle);
    fs::write(
        bundle_root.join("source.properties"),
        b"Pkg.Revision=invalid\n",
    )
    .unwrap();
    let error: IsolatedAdbError =
        AdbBundle::load(config(&root), std::sync::Arc::new(TestLog)).unwrap_err();
    match error {
        IsolatedAdbError::InvalidBundle { path, .. } => {
            assert!(path.ends_with("runtime/custom-adb/source.properties"));
        }
        other => panic!("应报告自定义 source.properties，实际为 {other}"),
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn lifecycle_events_append_without_changing_raw_output() {
    let root = test_root("log-append");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("server.log");
    fs::write(&path, b"raw stdout\nraw stderr\n").unwrap();
    crate::server::append_server_event(
        &TestLog,
        &path,
        "adb.server.failure",
        "error",
        serde_json::json!({"message": "fixture failure"}),
    )
    .unwrap();
    crate::server::append_server_event(
        &TestLog,
        &path,
        "adb.server.stopped",
        "ok",
        serde_json::json!({}),
    )
    .unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("raw stdout\nraw stderr\n"));
    let events: Vec<serde_json::Value> = text
        .lines()
        .skip(2)
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["stage"], "adb.server.failure");
    assert_eq!(events[1]["stage"], "adb.server.stopped");
    assert!(!text.contains('\r'));
    assert!(
        crate::server::append_server_event(
            &TestLog,
            &root.join("missing.log"),
            "test",
            "error",
            serde_json::json!({})
        )
        .is_err()
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "AZLW_MEASURE_MODE=adb-server"]
fn measure_owned_server_start_when_requested() {
    if std::env::var("AZLW_MEASURE_MODE").ok().as_deref() != Some("adb-server") {
        return;
    }
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.dependencies/tools/platform-tools/37.0.1/windows-x64");
    let root = test_root("server-measure");
    let bundle_dir = root.join("runtime/platform-tools");
    fs::create_dir_all(&bundle_dir).unwrap();
    for name in [
        "adb.exe",
        "AdbWinApi.dll",
        "NOTICE.txt",
        "source.properties",
    ] {
        fs::copy(source.join(name), bundle_dir.join(name)).unwrap();
    }
    let config = crate::AdbConfig {
        root: root.clone(),
        executable: PathBuf::from("runtime/platform-tools/adb.exe"),
        state_directory: PathBuf::from("state"),
        log_directory: PathBuf::from("logs"),
    };
    for label in ["first", "second"] {
        let started = std::time::Instant::now();
        let bundle = AdbBundle::load(config.clone(), std::sync::Arc::new(TestLog)).unwrap();
        let mut server = crate::OwnedAdbServer::start(bundle, "127.0.0.1:1").unwrap();
        let start_us = started.elapsed().as_micros();
        let stop_started = std::time::Instant::now();
        let evidence = server.shutdown().unwrap();
        println!(
            "\nMEASURE stage=adb_server sample={label} start_us={start_us} stop_us={} port={} process_stopped={} connect=not_called",
            stop_started.elapsed().as_micros(),
            server.port(),
            evidence.process_stopped
        );
    }
    fs::remove_dir_all(root).unwrap();
}

fn test_root(label: &str) -> PathBuf {
    let home: PathBuf = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("测试需要 HOME 或 USERPROFILE");
    let mut bytes: [u8; 16] = [0; 16];
    getrandom::fill(&mut bytes).unwrap();
    home.join("suzushiro/scratch/adb-tests").join(format!(
        "{label}-{}-{:032x}",
        std::process::id(),
        u128::from_le_bytes(bytes)
    ))
}

#[test]
fn client_arguments_place_port_before_serial() {
    let arguments = crate::adb_client_arguments(
        Some(61_234),
        Some("127.0.0.1:16384"),
        &["shell".to_owned(), "id".to_owned()],
    );

    assert_eq!(
        arguments,
        ["-P", "61234", "-s", "127.0.0.1:16384", "shell", "id"]
    );
}

/// Windows 服务参数使用动态回环端口并约束唯一设备，不包含默认 5037。
#[cfg(target_os = "windows")]
#[test]
fn server_arguments_are_loopback_owned_and_single_target() {
    let arguments: Vec<String> = crate::server::server_arguments(61_234, "127.0.0.1:16384");

    assert_eq!(
        arguments,
        [
            "-L",
            "tcp:localhost:61234",
            "--one-device",
            "127.0.0.1:16384",
            "server",
            "nodaemon",
        ]
    );
    assert!(
        !arguments
            .iter()
            .any(|value: &String| value.contains("5037"))
    );
}

/// 启动失败回收会终止并等待刚创建的子进程，不留下后台服务。
#[cfg(target_os = "windows")]
#[test]
fn starting_process_cleanup_waits_for_child_exit() {
    let executable: PathBuf =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/ping.exe");
    let mut child = Command::new(&executable)
        .args(["-n", "30", "127.0.0.1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    crate::cleanup::kill_and_wait_process(
        &mut child,
        &executable,
        "test.stop_child",
        "test.wait_child",
    )
    .unwrap();

    assert!(child.try_wait().unwrap().is_some());
}

//! 覆盖 CTest 清单、ELF 校验、远端命令和诊断边界的单元测试。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::device::{build_remote_test_invocation, validate_boot_id};
use super::host::windows_command_path_text;
use super::inventory::{NativeTestCommand, parse_ctest_inventory, validate_x86_64_elf};
use super::report::bounded_diagnostic;
use super::{
    ANDROID_LINKER64, ELF64_HEADER_BYTES, ELF64_PROGRAM_HEADER_BYTES, MAXIMUM_DIAGNOSTIC_BYTES,
    NativeTestRunnerError,
};
use crate::adapters::tool_root::ToolRoot;

const EXPECTED_TEST_COUNT: usize = 8;
const EXPECTED_TEST_NAMES: [&str; EXPECTED_TEST_COUNT] = [
    "azlw-agent-protocol-test",
    "azlw-agent-rpc-lifecycle-test",
    "azlw-bag-snapshot-test",
    "azlw-equipment-command-test",
    "azlw-loader-policy-test",
    "azlw-module-identity-test",
    "azlw-ship-catalog-snapshot-test",
    "azlw-secure-channel-test",
];
const EXTRA_ARGUMENT_NAME: &str = "sample-input.bin";

/// CTest 清单保留设备测试及其文件参数。
#[test]
fn inventory_parses_exact_device_tests_and_fixture() {
    let fixture = TestDirectory::new("valid");
    let encoded = create_inventory(&fixture.root, EXPECTED_TEST_COUNT, true);
    let root = ToolRoot::open(&fixture.root).unwrap();

    let plan = parse_ctest_inventory(&encoded, &root).unwrap();

    assert_eq!(plan.tests.len(), EXPECTED_TEST_COUNT);
    assert_eq!(plan.local_artifacts.len(), EXPECTED_TEST_COUNT + 1);
    assert_eq!(plan.tests[7].name, "azlw-secure-channel-test");
    assert_eq!(plan.tests[7].arguments, [EXTRA_ARGUMENT_NAME]);
    assert!(
        plan.artifacts
            .iter()
            .all(|artifact| artifact.sha256.len() == 64)
    );
}

/// 空清单和主机可执行项不得进入设备执行流程。
#[test]
fn inventory_rejects_empty_or_host_enabled_contracts() {
    let fixture = TestDirectory::new("invalid");
    let root = ToolRoot::open(&fixture.root).unwrap();
    for encoded in [
        create_inventory(&fixture.root, 0, true),
        create_inventory(&fixture.root, EXPECTED_TEST_COUNT, false),
    ] {
        assert!(parse_ctest_inventory(&encoded, &root).is_err());
    }
}

/// 测试集合及顺序由当前构建清单决定，不依赖运行器中的旧列表。
#[test]
fn inventory_accepts_changed_test_count_order_and_file_arguments() {
    let fixture = TestDirectory::new("changed-inventory");
    let root = ToolRoot::open(&fixture.root).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_str(&create_inventory(&fixture.root, 2, true)).unwrap();
    let tests = value["tests"].as_array_mut().unwrap();
    tests.swap(0, 1);
    let extra = fixture.root.join("tests/added-check");
    write_elf(&extra);
    let mut added = tests[0].clone();
    added["name"] = json!("added-check");
    added["command"] = json!([extra.to_string_lossy()]);
    tests.push(added);
    let plan = parse_ctest_inventory(&value.to_string(), &root).unwrap();
    assert_eq!(plan.tests.len(), 3);
    assert_eq!(plan.tests[2].name, "added-check");
    let mut invalid_name = value.clone();
    invalid_name["tests"][0]["name"] = json!("test;id");
    assert!(parse_ctest_inventory(&invalid_name.to_string(), &root).is_err());
    let mut duplicate = value.clone();
    duplicate["tests"]
        .as_array_mut()
        .unwrap()
        .push(value["tests"][0].clone());
    assert!(parse_ctest_inventory(&duplicate.to_string(), &root).is_err());
}

/// 清单类型、重复属性、错误工作目录和 tests/ 外产物均不得被规范化后接受。
#[test]
fn inventory_rejects_contract_and_path_ambiguity() {
    let fixture = TestDirectory::new("contract-ambiguity");
    let root = ToolRoot::open(&fixture.root).unwrap();
    let valid = create_inventory(&fixture.root, EXPECTED_TEST_COUNT, true);

    let mut wrong_kind: serde_json::Value = serde_json::from_str(&valid).unwrap();
    wrong_kind["kind"] = json!("ctest-info");
    let mut wrong_version: serde_json::Value = serde_json::from_str(&valid).unwrap();
    wrong_version["version"]["minor"] = json!(1);
    let mut duplicate_property: serde_json::Value = serde_json::from_str(&valid).unwrap();
    duplicate_property["tests"][0]["properties"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "DISABLED", "value": true}));
    let wrong_working_directory = fixture.root.join("wrong-working-directory");
    fs::create_dir(&wrong_working_directory).unwrap();
    let mut wrong_working: serde_json::Value = serde_json::from_str(&valid).unwrap();
    wrong_working["tests"][0]["properties"][3]["value"] =
        json!(wrong_working_directory.to_string_lossy());
    let outside_artifact = fixture.root.join(EXPECTED_TEST_NAMES[0]);
    write_elf(&outside_artifact);
    let mut outside_tests: serde_json::Value = serde_json::from_str(&valid).unwrap();
    outside_tests["tests"][0]["command"][0] = json!(outside_artifact.to_string_lossy());

    for encoded in [
        wrong_kind.to_string(),
        wrong_version.to_string(),
        duplicate_property.to_string(),
        wrong_working.to_string(),
        outside_tests.to_string(),
    ] {
        assert!(matches!(
            parse_ctest_inventory(&encoded, &root),
            Err(NativeTestRunnerError::InvalidInventory { .. })
        ));
    }
}

/// ELF 的 Android 解释器和 PT_LOAD 文件边界必须形成可验证证据。
#[test]
fn elf_rejects_wrong_interpreter_and_out_of_bounds_load_segment() {
    let fixture = TestDirectory::new("invalid-elf");
    let wrong_interpreter = fixture.root.join("wrong-interpreter");
    write_elf(&wrong_interpreter);
    let mut bytes = fs::read(&wrong_interpreter).unwrap();
    let interpreter = bytes
        .windows(ANDROID_LINKER64.len())
        .position(|window| window == ANDROID_LINKER64)
        .unwrap();
    bytes[interpreter] = b'!';
    fs::write(&wrong_interpreter, bytes).unwrap();
    assert!(matches!(
        validate_x86_64_elf(&wrong_interpreter, true),
        Err(NativeTestRunnerError::InvalidElf { .. })
    ));

    let invalid_load = fixture.root.join("invalid-load");
    write_elf(&invalid_load);
    let mut bytes = fs::read(&invalid_load).unwrap();
    let load_size = ELF64_HEADER_BYTES + 32;
    bytes[load_size..load_size + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&invalid_load, bytes).unwrap();
    assert!(matches!(
        validate_x86_64_elf(&invalid_load, true),
        Err(NativeTestRunnerError::InvalidElf { .. })
    ));
}

/// 远端包装命令先持久化 PID 与启动时间，再以原进程身份执行测试。
#[test]
fn remote_invocation_records_identity_before_exec() {
    let test = NativeTestCommand {
        name: "azlw-equipment-command-test".to_owned(),
        arguments: vec![EXTRA_ARGUMENT_NAME.to_owned()],
    };
    let invocation = build_remote_test_invocation(
        "/data/local/tmp/azlw-native-tests-00000000-0000-0000-0000-000000000000",
        12,
        &test,
    );

    assert!(invocation.command.contains("printf '%s\\n' \"$$\""));
    assert!(
        invocation
            .command
            .contains("awk '{print $22}' /proc/$$/stat")
    );
    assert!(
        invocation
            .command
            .ends_with("exec ./azlw-equipment-command-test ./sample-input.bin")
    );
    assert!(invocation.pid_path.ends_with(".azlw-test-12.pid"));
    assert!(invocation.start_time_path.ends_with(".azlw-test-12.start"));
}

/// 诊断上限按 UTF-8 字符边界截断，不能因多字节文本而产生无效切片。
#[test]
fn diagnostic_truncation_preserves_utf8_boundaries() {
    let input = "测".repeat(MAXIMUM_DIAGNOSTIC_BYTES);
    let bounded = bounded_diagnostic(&input);

    assert!(bounded.ends_with("...[diagnostic truncated]"));
    assert!(bounded.len() < input.len());
    assert!(bounded.is_char_boundary(bounded.len()));
}

/// boot_id 必须保持固定小写 UUID，不能接受大写、缺段或附加文本。
#[test]
fn boot_id_requires_canonical_lower_uuid() {
    validate_boot_id("e8d164ab-2173-4e71-9697-5f7e52ab4d45").unwrap();
    for invalid in [
        "E8d164ab-2173-4e71-9697-5f7e52ab4d45",
        "e8d164ab21734e7196975f7e52ab4d45",
        "e8d164ab-2173-4e71-9697-5f7e52ab4d45 trailing",
    ] {
        assert!(validate_boot_id(invalid).is_err());
    }
}

/// Windows 子命令接受普通路径并只剥离本地盘符扩展前缀，拒绝扩展 UNC。
#[test]
fn windows_command_paths_avoid_cmd_unc_current_directories() {
    assert_eq!(
        windows_command_path_text(Path::new(r"\\?\E:\repo\target"), "fixture").unwrap(),
        r"E:\repo\target"
    );
    assert_eq!(
        windows_command_path_text(Path::new(r"E:\repo\target"), "fixture").unwrap(),
        r"E:\repo\target"
    );
    assert!(
        windows_command_path_text(Path::new(r"\\?\UNC\server\share\target"), "fixture").is_err()
    );
}

fn create_inventory(root: &Path, count: usize, valid_properties: bool) -> String {
    assert!(count <= EXPECTED_TEST_NAMES.len());
    let artifact_root = root.join("tests");
    fs::create_dir_all(&artifact_root).unwrap();
    let mut tests = Vec::new();
    for (index, expected_name) in EXPECTED_TEST_NAMES.iter().take(count).enumerate() {
        let name = (*expected_name).to_owned();
        let path = artifact_root.join(&name);
        write_elf(&path);
        let mut command = vec![path.to_string_lossy().into_owned()];
        if index == EXPECTED_TEST_COUNT - 1 {
            let fixture = artifact_root.join(EXTRA_ARGUMENT_NAME);
            write_elf(&fixture);
            command.push(fixture.to_string_lossy().into_owned());
        }
        let properties = if valid_properties {
            vec![
                json!({"name": "DISABLED", "value": true}),
                json!({"name": "LABELS", "value": ["android-device"]}),
                json!({"name": "TIMEOUT", "value": 180.0}),
                json!({
                    "name": "WORKING_DIRECTORY",
                    "value": artifact_root.to_string_lossy(),
                }),
            ]
        } else {
            vec![json!({"name": "DISABLED", "value": false})]
        };
        tests.push(json!({
            "name": name,
            "command": command,
            "properties": properties,
        }));
    }
    json!({
        "kind": "ctestInfo",
        "version": {"major": 1, "minor": 0},
        "tests": tests,
    })
    .to_string()
}

fn write_elf(path: &Path) {
    if path.exists() {
        return;
    }
    let is_executable =
        path.file_name().and_then(|name| name.to_str()) != Some(EXTRA_ARGUMENT_NAME);
    let program_header_count = if is_executable { 2_u16 } else { 1_u16 };
    let interpreter_offset =
        ELF64_HEADER_BYTES + usize::from(program_header_count * ELF64_PROGRAM_HEADER_BYTES);
    let file_size = interpreter_offset
        + if is_executable {
            ANDROID_LINKER64.len()
        } else {
            0
        };
    let mut bytes = vec![0_u8; file_size];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&(ELF64_HEADER_BYTES as u64).to_le_bytes());
    bytes[52..54].copy_from_slice(&(ELF64_HEADER_BYTES as u16).to_le_bytes());
    bytes[54..56].copy_from_slice(&ELF64_PROGRAM_HEADER_BYTES.to_le_bytes());
    bytes[56..58].copy_from_slice(&program_header_count.to_le_bytes());

    let load_header = ELF64_HEADER_BYTES;
    bytes[load_header..load_header + 4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[load_header + 32..load_header + 40].copy_from_slice(&(file_size as u64).to_le_bytes());
    bytes[load_header + 40..load_header + 48].copy_from_slice(&(file_size as u64).to_le_bytes());
    bytes[load_header + 48..load_header + 56].copy_from_slice(&4096_u64.to_le_bytes());
    if is_executable {
        let interpreter_header = ELF64_HEADER_BYTES + usize::from(ELF64_PROGRAM_HEADER_BYTES);
        bytes[interpreter_header..interpreter_header + 4].copy_from_slice(&3_u32.to_le_bytes());
        bytes[interpreter_header + 8..interpreter_header + 16]
            .copy_from_slice(&(interpreter_offset as u64).to_le_bytes());
        bytes[interpreter_header + 32..interpreter_header + 40]
            .copy_from_slice(&(ANDROID_LINKER64.len() as u64).to_le_bytes());
        bytes[interpreter_offset..].copy_from_slice(ANDROID_LINKER64);
    }
    fs::write(path, bytes).unwrap();
}

struct TestDirectory {
    root: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).expect("测试需要操作系统随机源");
        let root = std::env::temp_dir().join(format!(
            "azlw-native-test-runner-{label}-{}-{}",
            std::process::id(),
            u64::from_le_bytes(random)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn report_success_requires_complete_results_and_cleanup() {
    use super::contracts::*;
    let mut report = NativeTestRunReport {
        schema_version: 1,
        status: "failed",
        session_id: suzushiro_session_core::SessionId::generate().unwrap(),
        serial: "127.0.0.1:16384".into(),
        android_boot_id: None,
        android_api: None,
        android_abi: None,
        adb_revision: None,
        adb_server_port: None,
        adb_server_process_id: None,
        adb_server_log: None,
        remote_directory: String::new(),
        build_directory: PathBuf::new(),
        inventory_sha256: String::new(),
        configured_test_count: 1,
        passed_test_count: 1,
        failed_test_count: 0,
        infra_failed_test_count: 0,
        blocked_test_count: 0,
        duration_ms: 0,
        host_commands: vec![],
        artifacts: vec![NativeTestArtifactEvidence {
            filename: "sample-check".into(),
            size_bytes: 1,
            sha256: "a".repeat(64),
            device_sha256_verified: true,
        }],
        tests: vec![NativeTestCaseReport {
            name: "sample-check".into(),
            status: "passed",
            exit_code: Some(0),
            duration_ms: 0,
            stdout: None,
            stderr: None,
            remote_process: None,
            error: None,
        }],
        cleanup: NativeTestCleanupEvidence {
            remote_processes_stopped: true,
            remote_directory_removed: true,
            adb_process_stopped: true,
            adb_port_released: true,
            adb_temporary_root_removed: true,
        },
        failures: vec![],
    };
    assert!(report.results_complete());
    assert!(!report.passed());
    report.status = "passed";
    assert!(report.passed());
    let failures: &[fn(&mut NativeTestRunReport)] = &[
        |r| r.configured_test_count = 0,
        |r| r.tests.clear(),
        |r| r.tests[0].exit_code = Some(1),
        |r| r.passed_test_count = 0,
        |r| r.failed_test_count = 1,
        |r| r.infra_failed_test_count = 1,
        |r| r.blocked_test_count = 1,
        |r| r.artifacts[0].device_sha256_verified = false,
        |r| r.cleanup.remote_processes_stopped = false,
        |r| r.cleanup.remote_directory_removed = false,
        |r| r.cleanup.adb_process_stopped = false,
        |r| r.cleanup.adb_port_released = false,
        |r| r.cleanup.adb_temporary_root_removed = false,
        |r| {
            r.failures.push(NativeTestFailureEvidence {
                stage: "cleanup".into(),
                message: "failed".into(),
            })
        },
    ];
    for fail in failures {
        let mut invalid = report.clone();
        fail(&mut invalid);
        assert!(!invalid.results_complete());
        assert!(!invalid.passed());
    }
}

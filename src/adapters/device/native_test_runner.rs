//! 从 CTest 清单构建并在唯一 Android x86_64 目标上执行 Native 测试。

#[cfg(target_os = "windows")]
use std::time::Instant;

#[cfg(target_os = "windows")]
use suzushiro_session_core::SessionId;

#[cfg(target_os = "windows")]
use suzushiro_content_digest::sha256_bytes;

mod contracts;
#[cfg(any(target_os = "windows", test))]
mod device;
#[cfg(any(target_os = "windows", test))]
mod host;
#[cfg(any(target_os = "windows", test))]
mod inventory;
#[cfg(any(target_os = "windows", test))]
mod report;

pub use contracts::{
    NativeTestArtifactEvidence, NativeTestCaseReport, NativeTestCleanupEvidence,
    NativeTestFailureEvidence, NativeTestHostCommandEvidence, NativeTestOutputEvidence,
    NativeTestRemoteProcessEvidence, NativeTestRunOutcome, NativeTestRunReport,
    NativeTestRunnerError, NativeTestRunnerOptions, NativeTestToolchain,
};

#[cfg(target_os = "windows")]
use device::execute_device_tests;
#[cfg(target_os = "windows")]
use host::{ValidatedOptions, path_text, run_host_checked, validate_options};
#[cfg(target_os = "windows")]
use inventory::{NativeTestPlan, parse_ctest_inventory};
#[cfg(target_os = "windows")]
use report::{duration_millis, publish_report};

#[cfg(any(target_os = "windows", test))]
const ANDROID_DEVICE_LABEL: &str = "android-device";
#[cfg(target_os = "windows")]
const MINIMUM_ANDROID_API: u32 = 21;
#[cfg(target_os = "windows")]
const MAXIMUM_REPORT_BYTES: u64 = 32 * 1024 * 1024;
#[cfg(any(target_os = "windows", test))]
const MAXIMUM_TEST_ARGUMENTS: usize = 1;
#[cfg(any(target_os = "windows", test))]
const MAXIMUM_DIAGNOSTIC_BYTES: usize = 16 * 1024;
#[cfg(any(target_os = "windows", test))]
const CTEST_TIMEOUT_SECONDS: f64 = 180.0;
#[cfg(any(target_os = "windows", test))]
const MAXIMUM_PROGRAM_HEADERS: u16 = 128;
#[cfg(any(target_os = "windows", test))]
const MAXIMUM_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;
#[cfg(any(target_os = "windows", test))]
const MAXIMUM_TOTAL_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(any(target_os = "windows", test))]
const ELF64_HEADER_BYTES: usize = 64;
#[cfg(any(target_os = "windows", test))]
const ELF64_PROGRAM_HEADER_BYTES: u16 = 56;
#[cfg(any(target_os = "windows", test))]
const ANDROID_LINKER64: &[u8] = b"/system/bin/linker64\0";

/// 在固定 Windows 工具链和唯一 Android 目标上完成构建、执行与证据发布。
#[cfg(target_os = "windows")]
pub fn run_native_tests(
    options: NativeTestRunnerOptions,
) -> Result<NativeTestRunOutcome, NativeTestRunnerError> {
    let started: Instant = Instant::now();
    let validated: ValidatedOptions = validate_options(options)?;
    let configure = run_host_checked(
        &validated.cmake_executable,
        &[
            "-S".to_owned(),
            path_text(
                &validated.repository_root.as_path().join("native"),
                "Native 源目录",
            )?,
            "-B".to_owned(),
            path_text(validated.build_root.as_path(), "Native 构建目录")?,
            "-G".to_owned(),
            "Ninja".to_owned(),
            format!(
                "-DCMAKE_MAKE_PROGRAM={}",
                path_text(&validated.ninja_executable, "Ninja")?
            ),
            format!(
                "-DCMAKE_TOOLCHAIN_FILE={}",
                path_text(&validated.ndk_toolchain, "Android NDK toolchain")?
            ),
            "-DANDROID_ABI=x86_64".to_owned(),
            "-DANDROID_PLATFORM=android-21".to_owned(),
            "-DCMAKE_BUILD_TYPE=Release".to_owned(),
            "-DAZLW_BUILD_TESTS=ON".to_owned(),
        ],
        "native.configure",
    )?;
    let build = run_host_checked(
        &validated.cmake_executable,
        &[
            "--build".to_owned(),
            path_text(validated.build_root.as_path(), "Native 构建目录")?,
            "--parallel".to_owned(),
        ],
        "native.build",
    )?;

    let inventory_command = run_host_checked(
        &validated.ctest_executable,
        &[
            "--test-dir".to_owned(),
            path_text(validated.build_root.as_path(), "CTest 构建目录")?,
            "--show-only=json-v1".to_owned(),
        ],
        "native.inventory",
    )?;
    let inventory_sha256: String = sha256_bytes(inventory_command.stdout.as_bytes());
    let mut plan: NativeTestPlan =
        parse_ctest_inventory(&inventory_command.stdout, &validated.build_root)?;
    let session_id: SessionId =
        SessionId::generate().map_err(|error| NativeTestRunnerError::InvalidOptions {
            message: format!("生成 Native 测试会话 ID 失败: {error}"),
        })?;

    let device = execute_device_tests(
        &validated.tool_root,
        &validated.serial,
        session_id,
        &mut plan,
    );
    let passed_test_count = device
        .tests
        .iter()
        .filter(|test| test.status == "passed")
        .count();
    let failed_test_count = device
        .tests
        .iter()
        .filter(|test| test.status == "failed")
        .count();
    let infra_failed_test_count = device
        .tests
        .iter()
        .filter(|test| test.status == "infra_failed")
        .count();
    let blocked_test_count = device
        .tests
        .iter()
        .filter(|test| test.status == "blocked")
        .count();
    let mut report = NativeTestRunReport {
        schema_version: 1,
        status: "failed",
        session_id,
        serial: validated.serial,
        android_boot_id: device.android_boot_id,
        android_api: device.android_api,
        android_abi: device.android_abi,
        adb_revision: device.adb_revision,
        adb_server_port: device.adb_server_port,
        adb_server_process_id: device.adb_server_process_id,
        adb_server_log: device.adb_server_log,
        remote_directory: device.remote_directory,
        build_directory: validated.build_relative,
        inventory_sha256,
        configured_test_count: plan.tests.len(),
        passed_test_count,
        failed_test_count,
        infra_failed_test_count,
        blocked_test_count,
        duration_ms: duration_millis(started.elapsed()),
        host_commands: vec![
            configure.evidence,
            build.evidence,
            inventory_command.evidence,
        ],
        artifacts: plan.artifacts,
        tests: device.tests,
        cleanup: device.cleanup,
        failures: device.failures,
    };
    if report.results_complete() {
        report.status = "passed";
    }
    publish_report(&validated.tool_root, &report)
}

/// 非 Windows 构建保留可测试 API，但拒绝伪装成实机执行成功。
#[cfg(not(target_os = "windows"))]
pub fn run_native_tests(
    options: NativeTestRunnerOptions,
) -> Result<NativeTestRunOutcome, NativeTestRunnerError> {
    let NativeTestRunnerOptions {
        repository_root,
        tool_root,
        toolchain,
        build_relative,
        serial,
    } = options;
    let NativeTestToolchain {
        cmake_executable,
        ctest_executable,
        ninja_executable,
        ndk_toolchain,
    } = toolchain;
    drop((
        repository_root,
        tool_root,
        cmake_executable,
        ctest_executable,
        ninja_executable,
        ndk_toolchain,
        build_relative,
        serial,
    ));
    Err(NativeTestRunnerError::UnsupportedHost)
}

#[cfg(test)]
mod tests;

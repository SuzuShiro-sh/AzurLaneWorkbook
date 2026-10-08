//! 负责在唯一 Android 目标上执行 Native 测试并收集清理证据。

#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::time::Instant;

#[cfg(target_os = "windows")]
use suzushiro_session_core::SessionId;

#[cfg(target_os = "windows")]
use super::MINIMUM_ANDROID_API;
use super::contracts::NativeTestRunnerError;
#[cfg(target_os = "windows")]
use super::contracts::{
    NativeTestCaseReport, NativeTestCleanupEvidence, NativeTestFailureEvidence,
    NativeTestRemoteProcessEvidence,
};
#[cfg(target_os = "windows")]
use super::host::path_text;
use super::inventory::NativeTestCommand;
#[cfg(target_os = "windows")]
use super::inventory::NativeTestPlan;
#[cfg(target_os = "windows")]
use super::report::{bounded_diagnostic, duration_millis, publish_test_output};
#[cfg(target_os = "windows")]
use crate::adapters::tool_root::ToolRoot;
#[cfg(target_os = "windows")]
use suzushiro_adb::{IsolatedAdbError, OwnedAdbServer};

#[cfg(target_os = "windows")]
pub(super) struct DeviceExecution {
    pub(super) android_boot_id: Option<String>,
    pub(super) android_api: Option<u32>,
    pub(super) android_abi: Option<String>,
    pub(super) adb_revision: Option<String>,
    pub(super) adb_server_port: Option<u16>,
    pub(super) adb_server_process_id: Option<u32>,
    pub(super) adb_server_log: Option<PathBuf>,
    pub(super) remote_directory: String,
    pub(super) tests: Vec<NativeTestCaseReport>,
    pub(super) cleanup: NativeTestCleanupEvidence,
    pub(super) failures: Vec<NativeTestFailureEvidence>,
}

#[cfg(target_os = "windows")]
impl DeviceExecution {
    fn new(remote_directory: String, plan: &NativeTestPlan) -> Self {
        Self {
            android_boot_id: None,
            android_api: None,
            android_abi: None,
            adb_revision: None,
            adb_server_port: None,
            adb_server_process_id: None,
            adb_server_log: None,
            remote_directory,
            tests: plan
                .tests
                .iter()
                .map(|test| NativeTestCaseReport::blocked(test.name.clone()))
                .collect(),
            cleanup: NativeTestCleanupEvidence::default(),
            failures: Vec::new(),
        }
    }

    fn record_failure(&mut self, error: &NativeTestRunnerError) {
        self.failures.push(NativeTestFailureEvidence {
            stage: error.stage().to_owned(),
            message: bounded_diagnostic(&error.to_string()),
        });
    }
}

#[cfg(target_os = "windows")]
impl NativeTestCaseReport {
    fn blocked(name: String) -> Self {
        Self {
            name,
            status: "blocked",
            exit_code: None,
            duration_ms: 0,
            stdout: None,
            stderr: None,
            remote_process: None,
            error: None,
        }
    }
}

#[cfg(target_os = "windows")]
struct RemoteSessionGuard<'a> {
    server: &'a OwnedAdbServer,
    remote_directory: String,
    cleanup_required: bool,
    cleaned: bool,
}

#[cfg(target_os = "windows")]
impl<'a> RemoteSessionGuard<'a> {
    fn new(server: &'a OwnedAdbServer, remote_directory: String) -> Self {
        Self {
            server,
            remote_directory,
            cleanup_required: false,
            cleaned: false,
        }
    }

    fn create(&mut self) -> Result<(), NativeTestRunnerError> {
        self.cleanup_required = true;
        self.server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "mkdir".to_owned(),
                    self.remote_directory.clone(),
                ],
                "adb.create_remote_directory",
            )
            .map_err(|error| device_error("adb.create_remote_directory", error))?;
        Ok(())
    }

    fn cleanup(
        &mut self,
    ) -> (
        Result<bool, NativeTestRunnerError>,
        Result<bool, NativeTestRunnerError>,
    ) {
        if !self.cleanup_required {
            self.cleaned = true;
            return (Ok(true), Ok(true));
        }
        let processes =
            cleanup_owned_remote_processes(self.server, &self.remote_directory).map(|_| true);
        let directory = cleanup_remote_directory(self.server, &self.remote_directory);
        self.cleaned = processes.is_ok() && directory.is_ok();
        (processes, directory)
    }
}

#[cfg(target_os = "windows")]
impl Drop for RemoteSessionGuard<'_> {
    fn drop(&mut self) {
        if self.cleanup_required && !self.cleaned {
            if let Err(error) = cleanup_owned_remote_processes(self.server, &self.remote_directory)
            {
                eprintln!("Native 远端进程兜底清理失败: {error}");
            }
            if let Err(error) = cleanup_remote_directory(self.server, &self.remote_directory) {
                eprintln!("Native 远端目录兜底清理失败: {error}");
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub(super) fn execute_device_tests(
    tool_root: &ToolRoot,
    serial: &str,
    session_id: SessionId,
    plan: &mut NativeTestPlan,
) -> DeviceExecution {
    let remote_directory = format!("/data/local/tmp/azlw-native-tests-{session_id}");
    let mut execution = DeviceExecution::new(remote_directory.clone(), plan);
    let bundle =
        match crate::adapters::device::adb_config::load_adb_bundle(tool_root.as_path(), None) {
            Ok(bundle) => bundle,
            Err(error) => {
                let error = device_error("adb.bundle", error);
                execution.record_failure(&error);
                return execution;
            }
        };
    execution.adb_revision = Some(bundle.revision().to_owned());
    let mut server = match OwnedAdbServer::start(bundle, serial.to_owned()) {
        Ok(server) => server,
        Err(error) => {
            let error = device_error("adb.start", error);
            execution.record_failure(&error);
            return execution;
        }
    };
    execution.adb_server_port = Some(server.port());
    execution.adb_server_process_id = Some(server.process_id());
    execution.adb_server_log = Some(
        server
            .log_path()
            .strip_prefix(tool_root.as_path())
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| server.log_path().to_path_buf()),
    );
    let mut remote_guard = None;

    let operation = (|| {
        server
            .connect_target()
            .map_err(|error| device_error("adb.connect", error))?;
        let android_abi = server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "getprop".to_owned(),
                    "ro.product.cpu.abi".to_owned(),
                ],
                "adb.device_abi",
            )
            .map_err(|error| device_error("adb.device_abi", error))?;
        if android_abi.trim() != "x86_64" {
            return Err(NativeTestRunnerError::Device {
                stage: "adb.device_abi",
                message: format!("设备 ABI 必须是 x86_64，实际为 {android_abi:?}"),
            });
        }
        execution.android_abi = Some(android_abi.trim().to_owned());
        let api_text = server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "getprop".to_owned(),
                    "ro.build.version.sdk".to_owned(),
                ],
                "adb.device_api",
            )
            .map_err(|error| device_error("adb.device_api", error))?;
        let android_api: u32 =
            api_text
                .trim()
                .parse()
                .map_err(|_| NativeTestRunnerError::Device {
                    stage: "adb.device_api",
                    message: format!("设备 API 不是无符号十进制整数: {api_text:?}"),
                })?;
        if android_api < MINIMUM_ANDROID_API {
            return Err(NativeTestRunnerError::Device {
                stage: "adb.device_api",
                message: format!("设备 API {android_api} 低于 {MINIMUM_ANDROID_API}"),
            });
        }
        execution.android_api = Some(android_api);
        let android_boot_id = server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "cat".to_owned(),
                    "/proc/sys/kernel/random/boot_id".to_owned(),
                ],
                "adb.device_boot_id",
            )
            .map_err(|error| device_error("adb.device_boot_id", error))?;
        validate_boot_id(&android_boot_id)?;
        execution.android_boot_id = Some(android_boot_id.trim().to_owned());
        server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "test".to_owned(),
                    "-x".to_owned(),
                    "/system/bin/linker64".to_owned(),
                ],
                "adb.device_linker",
            )
            .map_err(|error| device_error("adb.device_linker", error))?;
        server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "command -v sha256sum >/dev/null".to_owned(),
                ],
                "adb.device_sha256sum",
            )
            .map_err(|error| device_error("adb.device_sha256sum", error))?;
        remote_guard = Some(RemoteSessionGuard::new(&server, remote_directory.clone()));
        remote_guard.as_mut().expect("远端 guard 已建立").create()?;

        for (filename, local_path) in &plan.local_artifacts {
            server
                .run_target_checked(
                    &[
                        "push".to_owned(),
                        path_text(local_path, "Native 测试产物")?,
                        format!("{remote_directory}/{filename}"),
                    ],
                    "adb.push_artifact",
                )
                .map_err(|error| device_error("adb.push_artifact", error))?;
            let expected_sha256 = plan
                .artifacts
                .iter()
                .find(|artifact| artifact.filename == *filename)
                .map(|artifact| artifact.sha256.clone())
                .ok_or_else(|| NativeTestRunnerError::InvalidInventory {
                    message: format!("缺少 Native 产物摘要: {filename}"),
                })?;
            verify_remote_artifact(
                &server,
                &format!("{remote_directory}/{filename}"),
                &expected_sha256,
            )?;
            if let Some(artifact) = plan
                .artifacts
                .iter_mut()
                .find(|artifact| artifact.filename == *filename)
            {
                artifact.device_sha256_verified = true;
            }
        }
        server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    format!("chmod 700 {remote_directory}/*"),
                ],
                "adb.chmod_artifacts",
            )
            .map_err(|error| device_error("adb.chmod_artifacts", error))?;

        for (index, test) in plan.tests.iter().enumerate() {
            let (report, infrastructure_error) = run_native_test_case(
                &server,
                tool_root,
                session_id,
                &remote_directory,
                index,
                test,
            );
            execution.tests[index] = report;
            if let Some(error) = infrastructure_error {
                return Err(error);
            }
        }
        let final_boot_id = server
            .run_target_checked(
                &[
                    "shell".to_owned(),
                    "cat".to_owned(),
                    "/proc/sys/kernel/random/boot_id".to_owned(),
                ],
                "adb.verify_device_boot_id",
            )
            .map_err(|error| device_error("adb.verify_device_boot_id", error))?;
        validate_boot_id(&final_boot_id)?;
        if final_boot_id.trim() != android_boot_id.trim() {
            return Err(NativeTestRunnerError::Device {
                stage: "adb.verify_device_boot_id",
                message: format!(
                    "Native 测试期间设备发生重启: before={:?}, after={:?}",
                    android_boot_id.trim(),
                    final_boot_id.trim()
                ),
            });
        }
        Ok(())
    })();

    let (remote_process_cleanup, remote_cleanup) = match remote_guard.as_mut() {
        Some(guard) => guard.cleanup(),
        None => (Ok(true), Ok(true)),
    };
    drop(remote_guard);
    let shutdown = server
        .shutdown()
        .map_err(|error| device_error("adb.shutdown", error));

    if let Err(error) = operation {
        execution.record_failure(&error);
    }
    match remote_process_cleanup {
        Ok(stopped) => execution.cleanup.remote_processes_stopped = stopped,
        Err(error) => execution.record_failure(&error),
    }
    match remote_cleanup {
        Ok(removed) => {
            execution.cleanup.remote_directory_removed = removed;
        }
        Err(error) => execution.record_failure(&error),
    }
    match shutdown {
        Ok(shutdown) => {
            execution.cleanup.adb_process_stopped = shutdown.process_stopped;
            execution.cleanup.adb_port_released = shutdown.port_released;
            execution.cleanup.adb_temporary_root_removed = shutdown.temporary_root_removed;
        }
        Err(error) => execution.record_failure(&error),
    }
    execution
}

#[cfg(target_os = "windows")]
fn cleanup_remote_directory(
    server: &OwnedAdbServer,
    remote_directory: &str,
) -> Result<bool, NativeTestRunnerError> {
    server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "rm".to_owned(),
                "-rf".to_owned(),
                remote_directory.to_owned(),
            ],
            "adb.remove_remote_directory",
        )
        .map_err(|error| device_error("adb.remove_remote_directory", error))?;
    server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "test".to_owned(),
                "!".to_owned(),
                "-e".to_owned(),
                remote_directory.to_owned(),
            ],
            "adb.verify_remote_directory_removed",
        )
        .map_err(|error| device_error("adb.verify_remote_directory_removed", error))?;
    Ok(true)
}

#[cfg(target_os = "windows")]
fn verify_remote_artifact(
    server: &OwnedAdbServer,
    remote_path: &str,
    expected_sha256: &str,
) -> Result<(), NativeTestRunnerError> {
    let output = server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "sha256sum".to_owned(),
                remote_path.to_owned(),
            ],
            "adb.verify_artifact_sha256",
        )
        .map_err(|error| device_error("adb.verify_artifact_sha256", error))?;
    let mut fields = output.split_whitespace();
    let actual_sha256 = fields.next().unwrap_or_default();
    let actual_path = fields.next().unwrap_or_default();
    if fields.next().is_some() || actual_sha256 != expected_sha256 || actual_path != remote_path {
        return Err(NativeTestRunnerError::Device {
            stage: "adb.verify_artifact_sha256",
            message: format!(
                "设备端产物摘要不匹配: path={remote_path}, expected={expected_sha256}, output={output:?}"
            ),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
pub(super) struct RemoteTestInvocation {
    pub(super) command: String,
    pub(super) pid_path: String,
    pub(super) start_time_path: String,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
struct RemoteProcessIdentity {
    pid: u32,
    start_time: u64,
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn build_remote_test_invocation(
    remote_directory: &str,
    index: usize,
    test: &NativeTestCommand,
) -> RemoteTestInvocation {
    let marker = format!(".azlw-test-{index:02}");
    let pid_path = format!("{remote_directory}/{marker}.pid");
    let start_time_path = format!("{remote_directory}/{marker}.start");
    let mut executable = format!("./{}", test.name);
    for argument in &test.arguments {
        executable.push_str(" ./");
        executable.push_str(argument);
    }
    let command = format!(
        "cd {remote_directory} && rm -f {pid_path} {start_time_path} && printf '%s\\n' \"$$\" > {pid_path} && awk '{{print $22}}' /proc/$$/stat > {start_time_path} && exec {executable}"
    );
    RemoteTestInvocation {
        command,
        pid_path,
        start_time_path,
    }
}

#[cfg(target_os = "windows")]
fn read_remote_process_identity(
    server: &OwnedAdbServer,
    invocation: &RemoteTestInvocation,
) -> Result<RemoteProcessIdentity, NativeTestRunnerError> {
    let output = server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "cat".to_owned(),
                invocation.pid_path.clone(),
                invocation.start_time_path.clone(),
            ],
            "adb.read_native_test_identity",
        )
        .map_err(|error| device_error("adb.read_native_test_identity", error))?;
    let values: Vec<&str> = output.lines().map(str::trim).collect();
    if values.len() != 2 {
        return Err(NativeTestRunnerError::Device {
            stage: "adb.read_native_test_identity",
            message: format!("远端测试身份必须包含 PID 和启动时间两行: {output:?}"),
        });
    }
    let pid: u32 = values[0]
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0 && values[0] == pid.to_string())
        .ok_or_else(|| NativeTestRunnerError::Device {
            stage: "adb.read_native_test_identity",
            message: format!("远端测试 PID 无效: {:?}", values[0]),
        })?;
    let start_time: u64 = values[1]
        .parse::<u64>()
        .ok()
        .filter(|start_time| *start_time != 0 && values[1] == start_time.to_string())
        .ok_or_else(|| NativeTestRunnerError::Device {
            stage: "adb.read_native_test_identity",
            message: format!("远端测试启动时间无效: {:?}", values[1]),
        })?;
    Ok(RemoteProcessIdentity { pid, start_time })
}

#[cfg(target_os = "windows")]
fn stop_tracked_remote_process(
    server: &OwnedAdbServer,
    identity: RemoteProcessIdentity,
) -> Result<NativeTestRemoteProcessEvidence, NativeTestRunnerError> {
    let script = format!(
        "azlw_stat=/proc/{}/stat; if [ ! -r \"$azlw_stat\" ]; then echo already_stopped; exit 0; fi; azlw_start=$(awk '{{print $22}}' \"$azlw_stat\"); if [ \"$azlw_start\" != \"{}\" ]; then echo pid_reused; exit 0; fi; kill -TERM {} 2>/dev/null || true; sleep 1; azlw_action=terminated; if [ -r \"$azlw_stat\" ] && [ \"$(awk '{{print $22}}' \"$azlw_stat\")\" = \"{}\" ]; then kill -KILL {} 2>/dev/null || true; sleep 1; azlw_action=killed; fi; if [ -r \"$azlw_stat\" ] && [ \"$(awk '{{print $22}}' \"$azlw_stat\")\" = \"{}\" ]; then echo still_running; exit 1; fi; echo \"$azlw_action\"",
        identity.pid,
        identity.start_time,
        identity.pid,
        identity.start_time,
        identity.pid,
        identity.start_time,
    );
    let status = server
        .run_target_status(
            &["shell".to_owned(), script],
            "adb.stop_native_test_process",
        )
        .map_err(|error| device_error("adb.stop_native_test_process", error))?;
    let action = status.stdout.trim();
    if status.exit_code != 0
        || !status.stderr.trim().is_empty()
        || !matches!(
            action,
            "already_stopped" | "pid_reused" | "terminated" | "killed"
        )
    {
        return Err(NativeTestRunnerError::Device {
            stage: "adb.stop_native_test_process",
            message: format!(
                "远端测试进程没有形成终止证据: exit={}, stdout={:?}, stderr={:?}",
                status.exit_code, status.stdout, status.stderr
            ),
        });
    }
    Ok(NativeTestRemoteProcessEvidence {
        pid: identity.pid,
        start_time: identity.start_time,
        stopped: true,
        cleanup_action: action.to_owned(),
    })
}

#[cfg(target_os = "windows")]
fn cleanup_owned_remote_processes(
    server: &OwnedAdbServer,
    remote_directory: &str,
) -> Result<u32, NativeTestRunnerError> {
    let script = format!(
        "azlw_found=0; for azlw_proc in /proc/[0-9]*; do azlw_pid=${{azlw_proc##*/}}; azlw_cwd=$(readlink \"$azlw_proc/cwd\" 2>/dev/null); azlw_exe=$(readlink \"$azlw_proc/exe\" 2>/dev/null); azlw_owned=0; case \"$azlw_cwd\" in {remote_directory}|{remote_directory}/*) azlw_owned=1;; esac; case \"$azlw_exe\" in {remote_directory}/*) azlw_owned=1;; esac; if [ \"$azlw_owned\" -eq 1 ]; then azlw_found=$((azlw_found + 1)); kill -TERM \"$azlw_pid\" 2>/dev/null || true; fi; done; if [ \"$azlw_found\" -gt 0 ]; then sleep 1; for azlw_proc in /proc/[0-9]*; do azlw_pid=${{azlw_proc##*/}}; azlw_cwd=$(readlink \"$azlw_proc/cwd\" 2>/dev/null); azlw_exe=$(readlink \"$azlw_proc/exe\" 2>/dev/null); azlw_owned=0; case \"$azlw_cwd\" in {remote_directory}|{remote_directory}/*) azlw_owned=1;; esac; case \"$azlw_exe\" in {remote_directory}/*) azlw_owned=1;; esac; if [ \"$azlw_owned\" -eq 1 ]; then kill -KILL \"$azlw_pid\" 2>/dev/null || true; fi; done; sleep 1; fi; for azlw_proc in /proc/[0-9]*; do azlw_cwd=$(readlink \"$azlw_proc/cwd\" 2>/dev/null); azlw_exe=$(readlink \"$azlw_proc/exe\" 2>/dev/null); case \"$azlw_cwd\" in {remote_directory}|{remote_directory}/*) echo residue; exit 1;; esac; case \"$azlw_exe\" in {remote_directory}/*) echo residue; exit 1;; esac; done; echo \"$azlw_found\""
    );
    let status = server
        .run_target_status(
            &["shell".to_owned(), script],
            "adb.cleanup_native_test_processes",
        )
        .map_err(|error| device_error("adb.cleanup_native_test_processes", error))?;
    let count_text = status.stdout.trim();
    let count: u32 = count_text
        .parse()
        .map_err(|_| NativeTestRunnerError::Device {
            stage: "adb.cleanup_native_test_processes",
            message: format!(
                "远端进程清理没有返回规范计数: exit={}, stdout={:?}, stderr={:?}",
                status.exit_code, status.stdout, status.stderr
            ),
        })?;
    if status.exit_code != 0 || !status.stderr.trim().is_empty() || count_text != count.to_string()
    {
        return Err(NativeTestRunnerError::Device {
            stage: "adb.cleanup_native_test_processes",
            message: format!(
                "远端进程清理没有形成完整证据: exit={}, stdout={:?}, stderr={:?}",
                status.exit_code, status.stdout, status.stderr
            ),
        });
    }
    Ok(count)
}

#[cfg(target_os = "windows")]
fn run_native_test_case(
    server: &OwnedAdbServer,
    tool_root: &ToolRoot,
    session_id: SessionId,
    remote_directory: &str,
    index: usize,
    test: &NativeTestCommand,
) -> (NativeTestCaseReport, Option<NativeTestRunnerError>) {
    let started = Instant::now();
    let invocation = build_remote_test_invocation(remote_directory, index + 1, test);
    let run_result = server
        .run_target_status(
            &["shell".to_owned(), invocation.command.clone()],
            "adb.run_native_test",
        )
        .map_err(|error| device_error("adb.run_native_test", error));
    let mut errors = Vec::new();
    if let Err(error) = &run_result {
        errors.push(error.to_string());
    }

    let remote_process = match read_remote_process_identity(server, &invocation) {
        Ok(identity) => match stop_tracked_remote_process(server, identity) {
            Ok(evidence) => Some(evidence),
            Err(error) => {
                errors.push(error.to_string());
                Some(NativeTestRemoteProcessEvidence {
                    pid: identity.pid,
                    start_time: identity.start_time,
                    stopped: false,
                    cleanup_action: "unproven".to_owned(),
                })
            }
        },
        Err(error) => {
            errors.push(error.to_string());
            None
        }
    };
    match cleanup_owned_remote_processes(server, remote_directory) {
        Ok(0) => {}
        Ok(count) => errors.push(format!(
            "测试 {} 留下 {count} 个额外远端进程，已定向终止",
            test.name
        )),
        Err(error) => errors.push(error.to_string()),
    }

    let mut exit_code = None;
    let mut stdout = None;
    let mut stderr = None;
    if let Ok(status) = &run_result {
        exit_code = Some(status.exit_code);
        match publish_test_output(
            tool_root,
            session_id,
            index + 1,
            &test.name,
            "stdout",
            &status.stdout,
        ) {
            Ok(evidence) => stdout = Some(evidence),
            Err(error) => errors.push(error.to_string()),
        }
        match publish_test_output(
            tool_root,
            session_id,
            index + 1,
            &test.name,
            "stderr",
            &status.stderr,
        ) {
            Ok(evidence) => stderr = Some(evidence),
            Err(error) => errors.push(error.to_string()),
        }
    }

    let infrastructure_error = if errors.is_empty() {
        None
    } else {
        Some(NativeTestRunnerError::Device {
            stage: "adb.run_native_test",
            message: bounded_diagnostic(&errors.join(" | ")),
        })
    };
    let status = if infrastructure_error.is_some() {
        "infra_failed"
    } else if exit_code == Some(0) {
        "passed"
    } else {
        "failed"
    };
    let error = infrastructure_error
        .as_ref()
        .map(ToString::to_string)
        .map(|message| bounded_diagnostic(&message));
    (
        NativeTestCaseReport {
            name: test.name.clone(),
            status,
            exit_code,
            duration_ms: duration_millis(started.elapsed()),
            stdout,
            stderr,
            remote_process,
            error,
        },
        infrastructure_error,
    )
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn validate_boot_id(value: &str) -> Result<(), NativeTestRunnerError> {
    let trimmed = value.trim();
    if trimmed.len() != 36
        || !trimmed.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
            }
        })
    {
        return Err(NativeTestRunnerError::Device {
            stage: "adb.device_boot_id",
            message: format!("设备 boot_id 不是规范小写 UUID: {value:?}"),
        });
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn device_error(stage: &'static str, error: IsolatedAdbError) -> NativeTestRunnerError {
    NativeTestRunnerError::Device {
        stage,
        message: error.to_string(),
    }
}

//! 提供跨原生 Windows 与 WSL 边界的有界宿主命令执行能力。

#[cfg(any(not(target_os = "windows"), test))]
use std::io::Read;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
#[cfg(not(target_os = "windows"))]
use std::thread;
use std::time::Duration;
#[cfg(not(target_os = "windows"))]
use std::time::Instant;

use serde::Serialize;
use thiserror::Error;
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// 命令截止之后，终止本次进程作业并排空管道还可使用的固定时间。
const HOST_COMMAND_CLEANUP_BUDGET: Duration = Duration::from_secs(1);

#[cfg(target_os = "windows")]
mod windows_supervise;

/// 保存宿主原生进程的退出状态及分离的标准输出和错误输出。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeOutput {
    /// 子进程退出码；操作系统没有提供退出码时为 `-1`。
    pub exit_code: i32,
    /// 按所选文本模式解码后的标准输出。
    pub stdout: String,
    /// 按所选文本模式解码后的标准错误输出。
    pub stderr: String,
}

#[derive(Clone, Copy)]
enum NativeTextMode {
    #[cfg(target_os = "windows")]
    WindowsCodePage(u32),
    StrictUtf8,
    #[cfg(any(target_os = "windows", test))]
    LossyUtf8,
}

/// 声明宿主子进程的工作目录和环境继承边界。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeCommandPolicy {
    clear_environment: bool,
    working_directory: Option<String>,
    environment: Vec<NativeEnvironmentVariable>,
}

impl NativeCommandPolicy {
    /// 保留父进程环境，供外部安装组件使用。
    #[must_use]
    pub fn inherited() -> Self {
        Self {
            clear_environment: false,
            working_directory: None,
            environment: Vec::new(),
        }
    }

    /// 使用调用方给出的最小环境，并把工作目录固定到已验证位置。
    #[cfg(any(target_os = "windows", test))]
    #[must_use]
    pub fn isolated(
        working_directory: impl Into<String>,
        environment: Vec<(String, String)>,
    ) -> Self {
        Self {
            clear_environment: true,
            working_directory: Some(working_directory.into()),
            environment: environment
                .into_iter()
                .map(|(name, value): (String, String)| NativeEnvironmentVariable { name, value })
                .collect(),
        }
    }

    /// 把策略应用到直接启动的原生子进程。
    #[cfg(any(target_os = "windows", test))]
    pub fn apply_to(&self, command: &mut Command) {
        if self.clear_environment {
            command.env_clear();
        }
        if let Some(working_directory) = &self.working_directory {
            command.current_dir(working_directory);
        }
        for variable in &self.environment {
            command.env(&variable.name, &variable.value);
        }
    }
}

/// 禁止控制台子进程为无控制台宿主逐次创建前台窗口。
#[cfg(target_os = "windows")]
pub fn hide_console_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

/// 可序列化到 WSL PowerShell 边界的单个环境变量。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct NativeEnvironmentVariable {
    name: String,
    value: String,
}

/// 宿主原生命令未在既定时限和输出边界内完成。
#[derive(Debug, Error)]
pub enum NativeCommandError {
    /// 命令参数无法编码，或进程启动、等待、读取与解码失败。
    #[error("主机命令阶段 {stage} 失败: {message}")]
    Failed {
        /// 由调用方提供的稳定操作阶段。
        stage: &'static str,
        /// 可展示的精确失败原因。
        message: String,
    },
    /// WSL 到 PowerShell 的结构化参数无法编码为 JSON。
    #[cfg(not(target_os = "windows"))]
    #[error("主机命令阶段 {stage} 编码参数失败: {source}")]
    Json {
        /// 参数编码失败的稳定阶段。
        stage: &'static str,
        /// 底层 JSON 编码错误。
        #[source]
        source: serde_json::Error,
    },
}

/// 以继承环境启动宿主命令，同时限制执行时长、输出尺寸和文本编码。
pub fn run_native(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
) -> Result<NativeOutput, NativeCommandError> {
    run_native_with_policy(
        executable,
        arguments,
        stage,
        &NativeCommandPolicy::inherited(),
    )
}

/// 按显式进程策略执行宿主命令，同时保留统一的超时和输出边界。
pub fn run_native_with_policy(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
    policy: &NativeCommandPolicy,
) -> Result<NativeOutput, NativeCommandError> {
    run_native_with_policy_timeout(executable, arguments, stage, policy, HOST_COMMAND_TIMEOUT)
}

/// 以继承环境启动宿主命令，并使用调用方给出的超时。
pub fn run_native_with_timeout(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
    timeout: Duration,
) -> Result<NativeOutput, NativeCommandError> {
    run_native_with_text_mode(
        executable,
        arguments,
        stage,
        &NativeCommandPolicy::inherited(),
        NativeTextMode::StrictUtf8,
        timeout,
    )
}

/// 按显式进程策略执行宿主命令，并使用调用方给出的超时。
pub fn run_native_with_policy_timeout(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
    policy: &NativeCommandPolicy,
    timeout: Duration,
) -> Result<NativeOutput, NativeCommandError> {
    run_native_with_text_mode(
        executable,
        arguments,
        stage,
        policy,
        NativeTextMode::StrictUtf8,
        timeout,
    )
}

/// 执行只需退出状态的宿主命令，并容忍不可控的人类提示文本使用本地编码。
#[cfg(target_os = "windows")]
pub fn run_native_lossy_with_policy(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
    policy: &NativeCommandPolicy,
) -> Result<NativeOutput, NativeCommandError> {
    run_native_with_text_mode(
        executable,
        arguments,
        stage,
        policy,
        NativeTextMode::LossyUtf8,
        HOST_COMMAND_TIMEOUT,
    )
}

/// 使用指定代码页解码，并在调用方给出的时间内回收超时子进程。
#[cfg(target_os = "windows")]
pub fn run_native_with_code_page_timeout(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
    code_page: u32,
    timeout: Duration,
) -> Result<NativeOutput, NativeCommandError> {
    run_native_with_text_mode(
        executable,
        arguments,
        stage,
        &NativeCommandPolicy::inherited(),
        NativeTextMode::WindowsCodePage(code_page),
        timeout,
    )
}

fn run_native_with_text_mode(
    executable: &str,
    arguments: &[String],
    stage: &'static str,
    policy: &NativeCommandPolicy,
    text_mode: NativeTextMode,
    timeout: Duration,
) -> Result<NativeOutput, NativeCommandError> {
    #[cfg(target_os = "windows")]
    let mut command: Command = {
        let mut command = Command::new(executable);
        command.args(arguments);
        policy.apply_to(&mut command);
        command.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
                | windows_sys::Win32::System::Threading::CREATE_SUSPENDED,
        );
        command
    };

    #[cfg(not(target_os = "windows"))]
    let mut command: Command = {
        const SCRIPT: &str = r#"$ErrorActionPreference = 'Stop';
$OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false);
$nativeArguments = @((ConvertFrom-Json -InputObject $env:SUZUSHIRO_HOST_COMMAND_ARGS_JSON));
$startInfo = [System.Diagnostics.ProcessStartInfo]::new();
$startInfo.FileName = $env:SUZUSHIRO_HOST_COMMAND_EXE;
$startInfo.UseShellExecute = $false;
$startInfo.RedirectStandardOutput = $true;
$startInfo.RedirectStandardError = $true;
$policy = ConvertFrom-Json -InputObject $env:SUZUSHIRO_HOST_COMMAND_POLICY_JSON;
if ($policy.clear_environment) {
    $startInfo.Environment.Clear();
}
if ($null -ne $policy.working_directory) {
    $startInfo.WorkingDirectory = [string]$policy.working_directory;
}
foreach ($entry in @($policy.environment)) {
    $startInfo.Environment[[string]$entry.name] = [string]$entry.value;
}
foreach ($nativeArgument in $nativeArguments) {
    [void]$startInfo.ArgumentList.Add([string]$nativeArgument);
}
$nativeProcess = [System.Diagnostics.Process]::new();
$nativeProcess.StartInfo = $startInfo;
if (-not $nativeProcess.Start()) { exit 127 };
$stdoutTask = $nativeProcess.StandardOutput.ReadToEndAsync();
$stderrTask = $nativeProcess.StandardError.ReadToEndAsync();
$timeoutMs = 170000
if (-not [string]::IsNullOrWhiteSpace($env:SUZUSHIRO_HOST_COMMAND_TIMEOUT_MS)) {
    $timeoutMs = [int]$env:SUZUSHIRO_HOST_COMMAND_TIMEOUT_MS
}
$cleanupMs = 1000
if (-not [string]::IsNullOrWhiteSpace($env:SUZUSHIRO_HOST_COMMAND_CLEANUP_MS)) {
    $cleanupMs = [int]$env:SUZUSHIRO_HOST_COMMAND_CLEANUP_MS
}
$watch = [Diagnostics.Stopwatch]::StartNew()
$exited = $nativeProcess.WaitForExit($timeoutMs)
if (-not $exited) {
    try {
        $nativeProcess.Kill($true)
    } catch {
        [Console]::Error.WriteLine("终止超时原生命令失败: {0}", $_.Exception.Message)
    }
    if (-not $nativeProcess.WaitForExit($cleanupMs)) {
        [Console]::Error.WriteLine("超过收尾预算 {0} 毫秒，原生命令仍未退出", $cleanupMs)
        exit 124
    }
    exit 124
}
$remainingMs = [Math]::Max(0, $timeoutMs - [int]$watch.ElapsedMilliseconds)
$stdoutDone = $stdoutTask.Wait($remainingMs)
$stderrDone = $stderrTask.Wait(0)
if (-not $stdoutDone -or -not $stderrDone) {
    try {
        $nativeProcess.Kill($true)
    } catch {
        [Console]::Error.WriteLine("关闭仍占用输出管道的原生命令失败: {0}", $_.Exception.Message)
    }
    $stdoutDone = $stdoutTask.Wait($cleanupMs)
    $stderrDone = $stderrTask.Wait($cleanupMs)
    if (-not $stdoutDone -or -not $stderrDone) {
        [Console]::Error.WriteLine("原生命令已退出，但输出管道在收尾预算 {0} 毫秒内未关闭", $cleanupMs)
        exit 124
    }
}
[Console]::Out.Write($stdoutTask.GetAwaiter().GetResult());
[Console]::Error.Write($stderrTask.GetAwaiter().GetResult());
exit $nativeProcess.ExitCode"#;
        let encoded_arguments =
            serde_json::to_string(arguments).map_err(|source| NativeCommandError::Json {
                stage: "host.encode_native_arguments",
                source,
            })?;
        let encoded_policy =
            serde_json::to_string(policy).map_err(|source| NativeCommandError::Json {
                stage: "host.encode_native_policy",
                source,
            })?;
        let mut command = Command::new("pwsh.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ]);
        command.env("SUZUSHIRO_HOST_COMMAND_EXE", executable);
        command.env("SUZUSHIRO_HOST_COMMAND_ARGS_JSON", encoded_arguments);
        command.env("SUZUSHIRO_HOST_COMMAND_POLICY_JSON", encoded_policy);
        command.env(
            "SUZUSHIRO_HOST_COMMAND_TIMEOUT_MS",
            timeout.as_millis().min(i32::MAX as u128).to_string(),
        );
        command.env(
            "SUZUSHIRO_HOST_COMMAND_CLEANUP_MS",
            HOST_COMMAND_CLEANUP_BUDGET
                .as_millis()
                .min(i32::MAX as u128)
                .to_string(),
        );
        // WSL 只会把 WSLENV 明确登记的变量传给 Windows 子进程。
        command.env(
            "WSLENV",
            "SUZUSHIRO_HOST_COMMAND_EXE:SUZUSHIRO_HOST_COMMAND_ARGS_JSON:SUZUSHIRO_HOST_COMMAND_POLICY_JSON:SUZUSHIRO_HOST_COMMAND_TIMEOUT_MS:SUZUSHIRO_HOST_COMMAND_CLEANUP_MS",
        );
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|source| NativeCommandError::Failed {
            stage,
            message: format!("启动失败: {source}"),
        })?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| NativeCommandError::Failed {
            stage,
            message: "无法接管 stdout 管道".to_owned(),
        })?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| NativeCommandError::Failed {
            stage,
            message: "无法接管 stderr 管道".to_owned(),
        })?;
    let collected = collect_command_output(&mut child, stdout_pipe, stderr_pipe, stage, timeout)?;
    if collected.stdout_exceeded || collected.stderr_exceeded {
        return Err(NativeCommandError::Failed {
            stage,
            message: "命令输出超过 16 MiB 上限".to_owned(),
        });
    }
    let stdout = decode_native_text(collected.stdout, "stdout", stage, text_mode)?;
    let stderr = decode_native_text(collected.stderr, "stderr", stage, text_mode)?;
    Ok(NativeOutput {
        exit_code: collected.exit_code,
        stdout,
        stderr,
    })
}

struct CollectedOutput {
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_exceeded: bool,
    stderr_exceeded: bool,
}

#[cfg(target_os = "windows")]
fn collect_command_output(
    child: &mut std::process::Child,
    stdout_pipe: std::process::ChildStdout,
    stderr_pipe: std::process::ChildStderr,
    stage: &'static str,
    timeout: Duration,
) -> Result<CollectedOutput, NativeCommandError> {
    windows_supervise::collect_output(child, stdout_pipe, stderr_pipe, stage, timeout)
}

#[cfg(not(target_os = "windows"))]
fn collect_command_output(
    child: &mut std::process::Child,
    stdout_pipe: std::process::ChildStdout,
    stderr_pipe: std::process::ChildStderr,
    stage: &'static str,
    timeout: Duration,
) -> Result<CollectedOutput, NativeCommandError> {
    let stdout_reader = thread::spawn(move || read_process_pipe(stdout_pipe));
    let stderr_reader = thread::spawn(move || read_process_pipe(stderr_pipe));
    let deadline = Instant::now() + timeout + HOST_COMMAND_CLEANUP_BUDGET;
    loop {
        if child
            .try_wait()
            .map_err(|source| NativeCommandError::Failed {
                stage,
                message: format!("等待失败: {source}"),
            })?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let kill_error = child.kill().err().map(|error| error.to_string());
            let cleanup_deadline = Instant::now() + HOST_COMMAND_CLEANUP_BUDGET;
            while Instant::now() < cleanup_deadline {
                if child
                    .try_wait()
                    .map_err(|source| NativeCommandError::Failed {
                        stage,
                        message: format!("等待失败: {source}"),
                    })?
                    .is_some()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            let still_running = child
                .try_wait()
                .map_err(|source| NativeCommandError::Failed {
                    stage,
                    message: format!("等待失败: {source}"),
                })?
                .is_none();
            let mut detail = String::new();
            if let Some(error) = kill_error {
                detail.push_str(&format!("终止本次命令进程失败: {error}"));
            }
            if still_running {
                if !detail.is_empty() {
                    detail.push('；');
                }
                detail.push_str("收尾预算内进程仍未退出");
            }
            return Err(NativeCommandError::Failed {
                stage,
                message: timeout_failure_message(timeout, &detail),
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().map_err(|source| NativeCommandError::Failed {
        stage,
        message: format!("读取退出状态失败: {source}"),
    })?;
    let (stdout, stdout_exceeded) = join_reader(stdout_reader, "stdout", stage)?;
    let (stderr, stderr_exceeded) = join_reader(stderr_reader, "stderr", stage)?;
    Ok(CollectedOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
        stdout_exceeded,
        stderr_exceeded,
    })
}

#[cfg(not(target_os = "windows"))]
fn join_reader(
    reader: thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
    stream: &str,
    stage: &'static str,
) -> Result<(Vec<u8>, bool), NativeCommandError> {
    reader
        .join()
        .map_err(|_| NativeCommandError::Failed {
            stage,
            message: format!("{stream} 读取线程异常退出"),
        })?
        .map_err(|source| NativeCommandError::Failed {
            stage,
            message: format!("读取 {stream} 失败: {source}"),
        })
}

fn timeout_failure_message(timeout: Duration, detail: &str) -> String {
    let mut message = format!(
        "超过 {}仍未退出；收尾预算 {}",
        format_duration_budget(timeout),
        format_duration_budget(HOST_COMMAND_CLEANUP_BUDGET),
    );
    if !detail.is_empty() {
        message.push('；');
        message.push_str(detail);
    }
    message
}

fn format_duration_budget(timeout: Duration) -> String {
    if timeout.subsec_nanos() == 0 {
        format!("{} 秒", timeout.as_secs())
    } else {
        format!("{} 毫秒", timeout.as_millis())
    }
}

/// 机器协议保持严格 UTF-8；非结构化诊断可替换无效字节用于报错展示。
fn decode_native_text(
    bytes: Vec<u8>,
    stream: &'static str,
    stage: &'static str,
    text_mode: NativeTextMode,
) -> Result<String, NativeCommandError> {
    match text_mode {
        #[cfg(target_os = "windows")]
        NativeTextMode::WindowsCodePage(code_page) => {
            use windows_sys::Win32::Globalization::{MB_ERR_INVALID_CHARS, MultiByteToWideChar};
            if bytes.is_empty() {
                return Ok(String::new());
            }
            let count = unsafe {
                MultiByteToWideChar(
                    code_page,
                    MB_ERR_INVALID_CHARS,
                    bytes.as_ptr(),
                    bytes.len() as i32,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if count == 0 {
                return Err(NativeCommandError::Failed {
                    stage,
                    message: format!(
                        "{stream} 不满足代码页 {code_page}: {}",
                        std::io::Error::last_os_error()
                    ),
                });
            }
            let mut wide = vec![0u16; count as usize];
            let written = unsafe {
                MultiByteToWideChar(
                    code_page,
                    MB_ERR_INVALID_CHARS,
                    bytes.as_ptr(),
                    bytes.len() as i32,
                    wide.as_mut_ptr(),
                    count,
                )
            };
            if written != count {
                return Err(NativeCommandError::Failed {
                    stage,
                    message: format!(
                        "{stream} 代码页转换失败: {}",
                        std::io::Error::last_os_error()
                    ),
                });
            }
            String::from_utf16(&wide).map_err(|error| NativeCommandError::Failed {
                stage,
                message: format!("{stream} 转换后不是有效 UTF-16: {error}"),
            })
        }
        NativeTextMode::StrictUtf8 => {
            String::from_utf8(bytes).map_err(|source| NativeCommandError::Failed {
                stage,
                message: format!("{stream} 不是 UTF-8: {source}"),
            })
        }
        #[cfg(any(target_os = "windows", test))]
        NativeTextMode::LossyUtf8 => Ok(String::from_utf8_lossy(&bytes).into_owned()),
    }
}

/// 并发排空子进程管道，只保留固定上限并报告是否发生截断。
#[cfg(any(not(target_os = "windows"), test))]
fn read_process_pipe(mut reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((output, exceeded));
        }
        if output.len() < MAX_OUTPUT_BYTES {
            let remaining = MAX_OUTPUT_BYTES - output.len();
            let copied = remaining.min(count);
            output.extend_from_slice(&buffer[..copied]);
            if copied < count {
                exceeded = true;
            }
        } else {
            exceeded = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    #[cfg(target_os = "windows")]
    use std::env;
    use std::path::Path;
    use std::process::{Command, Stdio};
    #[cfg(target_os = "windows")]
    use std::sync::{Mutex, MutexGuard};
    #[cfg(target_os = "windows")]
    use std::time::Duration;

    use super::{
        MAX_OUTPUT_BYTES, NativeCommandError, NativeCommandPolicy, NativeTextMode,
        decode_native_text, read_process_pipe,
    };
    #[cfg(target_os = "windows")]
    use super::{run_native_with_policy, run_native_with_timeout};

    #[cfg(target_os = "windows")]
    #[test]
    fn declared_code_page_preserves_chinese_protocol_names() {
        let text = decode_native_text(
            vec![0xc0, 0xd7, 0xb5, 0xe7],
            "stdout",
            "test.codepage",
            NativeTextMode::WindowsCodePage(936),
        )
        .unwrap();
        assert_eq!(text, "雷电");
        assert!(
            decode_native_text(
                vec![0xc0],
                "stdout",
                "test.codepage",
                NativeTextMode::WindowsCodePage(936)
            )
            .is_err()
        );
    }

    #[test]
    fn text_decoding_keeps_machine_protocol_strict() {
        let invalid = vec![b'o', b'k', 0x80];
        let error = decode_native_text(
            invalid.clone(),
            "stdout",
            "test.strict_utf8",
            NativeTextMode::StrictUtf8,
        )
        .unwrap_err();
        assert!(matches!(
            &error,
            NativeCommandError::Failed { stage, message }
                if *stage == "test.strict_utf8"
                    && message == "stdout 不是 UTF-8: invalid utf-8 sequence of 1 bytes from index 2"
        ));
        assert_eq!(
            error.to_string(),
            "主机命令阶段 test.strict_utf8 失败: stdout 不是 UTF-8: invalid utf-8 sequence of 1 bytes from index 2"
        );
        assert_eq!(
            decode_native_text(
                invalid,
                "stdout",
                "test.lossy_utf8",
                NativeTextMode::LossyUtf8,
            )
            .unwrap(),
            format!("ok{}", char::REPLACEMENT_CHARACTER)
        );
    }

    #[test]
    fn policy_has_stable_structured_representation() {
        let policy = NativeCommandPolicy::isolated(
            "C:\\runtime",
            vec![("SystemRoot".to_owned(), "C:\\Windows".to_owned())],
        );

        assert_eq!(
            serde_json::to_value(policy).unwrap(),
            serde_json::json!({
                "clear_environment": true,
                "working_directory": "C:\\runtime",
                "environment": [
                    {"name": "SystemRoot", "value": "C:\\Windows"}
                ]
            })
        );
    }

    #[test]
    fn pipe_reader_bounds_retained_output() {
        let input = vec![b'x'; MAX_OUTPUT_BYTES + 1];
        let (output, exceeded) = read_process_pipe(input.as_slice()).unwrap();

        assert_eq!(output.len(), MAX_OUTPUT_BYTES);
        assert!(exceeded);
    }

    #[test]
    fn isolated_policy_has_explicit_process_boundary() {
        let policy = NativeCommandPolicy::isolated(
            "/bundle/runtime/adb",
            vec![
                ("SystemRoot".to_owned(), "C:\\Windows".to_owned()),
                ("HOME".to_owned(), "C:\\bundle\\data\\adb\\home".to_owned()),
            ],
        );
        let mut command = Command::new("unused-test-command");
        policy.apply_to(&mut command);
        let environment: BTreeMap<String, Option<String>> = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|item| item.to_string_lossy().into_owned()),
                )
            })
            .collect();

        assert!(policy.clear_environment);
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new("/bundle/runtime/adb"))
        );
        assert_eq!(environment.len(), 2);
        assert_eq!(
            environment.get("SystemRoot"),
            Some(&Some("C:\\Windows".to_owned()))
        );
        assert_eq!(
            environment.get("HOME"),
            Some(&Some("C:\\bundle\\data\\adb\\home".to_owned()))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn isolated_policy_clears_real_child_environment() {
        let system_root = env::var("SystemRoot").expect("Windows 必须提供 SystemRoot");
        let executable = env::current_exe().expect("测试程序路径必须存在");
        let working_directory = env::current_dir()
            .expect("测试工作目录必须存在")
            .to_str()
            .expect("测试工作目录必须是 Unicode")
            .to_owned();
        let policy = NativeCommandPolicy::isolated(
            working_directory,
            vec![
                ("SystemRoot".to_owned(), system_root),
                ("SUZUSHIRO_CHILD_MARKER".to_owned(), "isolated".to_owned()),
            ],
        );
        let output = run_native_with_policy(
            executable.to_str().expect("测试程序路径必须是 Unicode"),
            &[
                "--exact".to_owned(),
                "tests::isolated_child_environment_helper".to_owned(),
                "--nocapture".to_owned(),
            ],
            "test.isolated_child_environment",
            &policy,
        )
        .expect("隔离子进程应当成功");

        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        assert!(output.stdout.contains(
            "SUZUSHIRO_CHILD_ENV marker=isolated path=false userprofile=false console=false"
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_command_timeout_stops_hanging_child() {
        use std::time::Instant;

        let started = Instant::now();
        let error = run_native_with_timeout(
            "ping.exe",
            &["-n".to_owned(), "30".to_owned(), "127.0.0.1".to_owned()],
            "test.timeout",
            Duration::from_secs(1),
        )
        .expect_err("超时命令必须失败");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "超时后仍运行 {:?}",
            started.elapsed()
        );
        assert!(error.to_string().contains("超过 1 秒仍未退出"), "{error}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn code_page_command_obeys_timeout_and_preserves_decoding() {
        let executable = std::env::current_exe().unwrap();
        let started = std::time::Instant::now();
        let error = super::run_native_with_code_page_timeout(
            executable.to_str().unwrap(),
            &[
                "--exact".into(),
                "tests::sleeping_child_helper".into(),
                "--nocapture".into(),
                "--ignored".into(),
            ],
            "test.code_page_timeout",
            936,
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.to_string().contains("仍未退出"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let output = super::run_native_with_code_page_timeout(
            executable.to_str().unwrap(),
            &[
                "--exact".into(),
                "tests::code_page_output_helper".into(),
                "--nocapture".into(),
                "--ignored".into(),
            ],
            "test.code_page_output",
            936,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("中文"), "{}", output.stdout);
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "仅由超时测试启动的子进程运行"]
    fn sleeping_child_helper() {
        std::thread::sleep(Duration::from_secs(30));
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "仅由代码页测试启动的子进程运行"]
    fn code_page_output_helper() {
        use std::io::Write;
        std::io::stdout()
            .write_all(&[0xd6, 0xd0, 0xce, 0xc4])
            .unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn isolated_child_environment_helper() {
        if env::var("SUZUSHIRO_CHILD_MARKER").as_deref() != Ok("isolated") {
            return;
        }
        println!(
            "SUZUSHIRO_CHILD_ENV marker=isolated path={} userprofile={} console={}",
            env::var_os("PATH").is_some(),
            env::var_os("USERPROFILE").is_some(),
            unsafe { !windows_sys::Win32::System::Console::GetConsoleWindow().is_null() }
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn missing_executable_fails_before_the_deadline() {
        let started = std::time::Instant::now();
        let error = run_native_with_timeout(
            "azlw-missing-host-command.exe",
            &[],
            "test.missing_executable",
            Duration::from_secs(5),
        )
        .expect_err("不存在的程序必须启动失败");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(error.to_string().contains("启动失败"), "{error}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn empty_and_short_output_return_without_waiting_for_the_budget() {
        let started = std::time::Instant::now();
        let empty = run_native_with_timeout(
            "cmd.exe",
            &["/c".to_owned(), "exit".to_owned(), "0".to_owned()],
            "test.empty_output",
            Duration::from_secs(5),
        )
        .expect("空输出命令应当成功");
        assert_eq!(empty.exit_code, 0, "{}", empty.stderr);
        assert!(empty.stdout.is_empty(), "{:?}", empty.stdout);
        assert!(started.elapsed() < Duration::from_secs(2));

        let short = run_native_with_timeout(
            "cmd.exe",
            &["/c".to_owned(), "echo".to_owned(), "azlw-short".to_owned()],
            "test.short_output",
            Duration::from_secs(5),
        )
        .expect("短输出命令应当成功");
        assert_eq!(short.exit_code, 0, "{}", short.stderr);
        assert!(short.stdout.contains("azlw-short"), "{}", short.stdout);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn descendant_holding_pipes_finishes_inside_the_deadline() {
        let _lock = pipe_holder_lock();
        let fast_pids = run_pipe_holder(
            "tests::pipe_holder_parent_exit_helper",
            "parent-exits",
            Duration::from_millis(500),
            true,
        );
        assert_eq!(fast_pids.len(), 1);
        assert_process_gone(fast_pids[0]);

        let slow_parent = run_pipe_holder(
            "tests::pipe_holder_parent_block_helper",
            "parent-blocks",
            Duration::from_millis(500),
            false,
        );
        assert_eq!(slow_parent.len(), 1);
        assert_process_gone(slow_parent[0]);

        let split = run_pipe_holder(
            "tests::pipe_holder_parent_split_helper",
            "parent-split",
            Duration::from_millis(500),
            true,
        );
        assert_eq!(split.len(), 2);
        assert_process_gone(split[0]);
        assert_process_gone(split[1]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn repeated_descendant_commands_do_not_accumulate_handles_or_threads() {
        let _lock = pipe_holder_lock();
        let handles_before = current_handle_count();
        let threads_before = current_thread_count();
        for _ in 0..3 {
            let pids = run_pipe_holder(
                "tests::pipe_holder_parent_exit_helper",
                "parent-exits",
                Duration::from_millis(500),
                true,
            );
            assert_process_gone(pids[0]);
        }
        let handles_after = current_handle_count();
        let threads_after = current_thread_count();
        assert!(
            handles_after <= handles_before.saturating_add(8),
            "句柄 {handles_before} -> {handles_after}"
        );
        assert!(
            threads_after <= threads_before.saturating_add(2),
            "线程 {threads_before} -> {threads_after}"
        );
    }

    #[cfg(target_os = "windows")]
    fn run_pipe_holder(
        parent_test: &str,
        label: &str,
        timeout: Duration,
        expect_ok: bool,
    ) -> Vec<u32> {
        let pid_path = descendant_pid_path(label);
        let _ = std::fs::remove_file(&pid_path);
        let executable = env::current_exe().expect("测试程序路径必须存在");
        let started = std::time::Instant::now();
        let result = run_native_with_timeout(
            executable.to_str().expect("测试程序路径必须是 Unicode"),
            &[
                "--exact".to_owned(),
                parent_test.to_owned(),
                "--nocapture".to_owned(),
                "--ignored".to_owned(),
            ],
            "test.descendant_pipe",
            timeout,
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(2000),
            "{parent_test} 耗时 {elapsed:?}，结果 {result:?}"
        );
        match result {
            Ok(output) => {
                assert!(expect_ok, "{parent_test} 不应成功: {output:?}");
                assert_eq!(output.exit_code, 0, "{}", output.stderr);
                if label == "parent-exits" {
                    assert!(output.stdout.contains("parent-exited"), "{}", output.stdout);
                }
            }
            Err(error) => {
                assert!(!expect_ok, "{parent_test} 应当成功: {error}");
                let text = error.to_string();
                assert!(text.contains("仍未退出"), "{text}");
                assert!(!text.contains("超过 0 秒"), "{text}");
                assert!(text.contains("收尾预算"), "{text}");
            }
        }
        let contents = std::fs::read_to_string(&pid_path).unwrap_or_else(|error| {
            panic!("{parent_test} 没有留下后代进程号 {pid_path:?}: {error}")
        });
        let _ = std::fs::remove_file(&pid_path);
        contents
            .lines()
            .map(|line| line.trim().parse::<u32>().expect("后代进程号"))
            .collect()
    }

    #[cfg(target_os = "windows")]
    fn pipe_holder_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn descendant_pid_path(label: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!("azlw-host-command-{label}.pid"))
    }

    #[cfg(target_os = "windows")]
    fn assert_process_gone(process_id: u32) {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
            if handle.is_null() {
                return;
            }
            let mut code = 0u32;
            let known = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            assert!(
                known == 0 || code != STILL_ACTIVE as u32,
                "后代进程 {process_id} 仍在运行"
            );
        }
    }

    #[cfg(target_os = "windows")]
    fn current_handle_count() -> u32 {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

        let mut count = 0u32;
        let known = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
        assert_ne!(known, 0, "{}", std::io::Error::last_os_error());
        count
    }

    #[cfg(target_os = "windows")]
    fn current_thread_count() -> usize {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;

        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            assert!(!snapshot.is_null());
            let process_id = GetCurrentProcessId();
            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..THREADENTRY32::default()
            };
            let mut count = 0usize;
            let mut has_entry = Thread32First(snapshot, &mut entry) != 0;
            while has_entry {
                if entry.th32OwnerProcessID == process_id {
                    count += 1;
                }
                has_entry = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            count
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "仅由后代管道测试启动"]
    fn pipe_holder_parent_exit_helper() {
        let descendant = spawn_holding_descendant("parent-exits", true);
        // 测试要让后代在父进程退出后继续持有管道，不能在这里等待它。
        std::mem::forget(descendant);
        println!("parent-exited");
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "仅由后代管道测试启动"]
    fn pipe_holder_parent_block_helper() {
        let descendant = spawn_holding_descendant("parent-blocks", true);
        std::mem::forget(descendant);
        std::thread::sleep(Duration::from_secs(30));
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "仅由后代管道测试启动"]
    fn pipe_holder_parent_split_helper() {
        let stdout_child = spawn_stream_descendant(Stdio::inherit(), Stdio::null());
        let stderr_child = spawn_stream_descendant(Stdio::null(), Stdio::inherit());
        write_descendant_pids("parent-split", &[stdout_child.id(), stderr_child.id()]);
        // 测试要让两个后代分别持有管道，不能在这里等待它们。
        std::mem::forget(stdout_child);
        std::mem::forget(stderr_child);
        println!("parent-split");
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "仅由后代管道测试启动"]
    fn pipe_holder_sleep_helper() {
        std::thread::sleep(Duration::from_secs(30));
    }

    #[cfg(target_os = "windows")]
    fn spawn_holding_descendant(label: &str, record_pid: bool) -> std::process::Child {
        let child = spawn_stream_descendant(Stdio::inherit(), Stdio::inherit());
        if record_pid {
            write_descendant_pids(label, &[child.id()]);
        }
        child
    }

    #[cfg(target_os = "windows")]
    fn spawn_stream_descendant(stdout: Stdio, stderr: Stdio) -> std::process::Child {
        let executable = env::current_exe().expect("测试程序路径必须存在");
        Command::new(executable)
            .args([
                "--exact",
                "tests::pipe_holder_sleep_helper",
                "--nocapture",
                "--ignored",
            ])
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("后代进程应当启动")
    }

    #[cfg(target_os = "windows")]
    fn write_descendant_pids(label: &str, process_ids: &[u32]) {
        use std::io::Write;

        let path = descendant_pid_path(label);
        let mut file = std::fs::File::create(&path).expect("应能写下后代进程号");
        for process_id in process_ids {
            writeln!(file, "{process_id}").expect("应能写下后代进程号");
        }
        file.sync_all().expect("应能同步后代进程号");
    }
}

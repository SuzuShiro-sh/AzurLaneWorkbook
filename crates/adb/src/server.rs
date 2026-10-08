//! 独立 ADB 服务的启动、持有与停止。
use crate::cleanup::{
    kill_and_wait_process, start_error_after_process_cleanup, wait_for_port_release,
};
use crate::config::validate_serial;
use crate::environment::windows_path;
use crate::keys::prepare_key;
use crate::{AdbBundle, AdbLogSink, IsolatedAdbError};
use std::{
    fs::{File, OpenOptions},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use suzushiro_host_command::hide_console_window;
const SERVER_START_ATTEMPTS: u32 = 8;
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// 工具持有的 ADB 服务停止后形成的定向清理证据。
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdbShutdownEvidence {
    pub process_stopped: bool,
    pub port_released: bool,
    pub temporary_root_removed: bool,
}

/// 以 `server nodaemon` 持有的独立 ADB 服务及其唯一目标。
#[cfg(target_os = "windows")]
pub struct OwnedAdbServer {
    pub(crate) bundle: AdbBundle,
    pub(crate) serial: String,
    pub(crate) port: u16,
    pub(crate) process_id: u32,
    pub(crate) log_path: PathBuf,
    pub(crate) child: Option<Child>,
}

#[cfg(target_os = "windows")]
impl OwnedAdbServer {
    /// 准备工具内密钥，并在动态回环端口启动由当前进程持有的 ADB 服务。
    pub fn start(bundle: AdbBundle, serial: impl Into<String>) -> Result<Self, IsolatedAdbError> {
        let serial: String = serial.into();
        validate_serial(&serial)?;
        prepare_key(&bundle)?;

        let mut failures: Vec<String> = Vec::new();
        for attempt in 1..=SERVER_START_ATTEMPTS {
            match start_once(bundle.clone(), serial.clone()) {
                Ok(server) => return Ok(server),
                Err(error) => failures.push(format!("第 {attempt} 次: {error}")),
            }
        }
        if let Err(error) = bundle
            .tool_root
            .remove_directory_if_exists(&bundle.state_relative.join("temp"))
        {
            return Err(IsolatedAdbError::ServerStartCleanup {
                attempts: SERVER_START_ATTEMPTS,
                messages: failures.join(" | "),
                cleanup: error.to_string(),
            });
        }
        Err(IsolatedAdbError::ServerStart {
            attempts: SERVER_START_ATTEMPTS,
            messages: failures.join(" | "),
        })
    }

    /// 返回独立 ADB 服务监听的动态本地端口。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 返回当前进程直接持有的 ADB 服务进程标识。
    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    /// 返回 ADB 服务端标准输出和错误输出共用的工具内日志。
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// 只允许独立服务连接从调用方取得的单个回环地址。
    pub fn connect_target(&self) -> Result<(), IsolatedAdbError> {
        self.run_global_lossy_checked(
            &["connect".to_owned(), self.serial.clone()],
            "adb.connect_target",
        )?;
        let state: String =
            self.run_target_checked(&["get-state".to_owned()], "adb.verify_target_state")?;
        if state.trim() != "device" {
            return Err(IsolatedAdbError::InvalidOutput {
                stage: "adb.verify_target_state",
                message: format!("唯一目标状态不是 device: {state:?}"),
            });
        }
        Ok(())
    }

    /// 终止直接持有的服务进程，并验证端口和临时目录均已释放。
    pub fn shutdown(&mut self) -> Result<AdbShutdownEvidence, IsolatedAdbError> {
        let evidence: AdbShutdownEvidence = self.stop_owned_process()?;
        self.child = None;
        Ok(evidence)
    }

    /// 只终止当前对象直接持有的子进程，不发送影响其他服务的 `kill-server`。
    fn stop_owned_process(&mut self) -> Result<AdbShutdownEvidence, IsolatedAdbError> {
        let child: &mut Child = self
            .child
            .as_mut()
            .ok_or(IsolatedAdbError::AlreadyStopped)?;
        kill_and_wait_process(
            child,
            &self.bundle.executable,
            "adb.stop_owned_server",
            "adb.wait_owned_server",
        )?;
        let process_stopped: bool = child
            .try_wait()
            .map_err(|source| IsolatedAdbError::Io {
                stage: "adb.confirm_owned_server_stopped",
                path: self.bundle.executable.clone(),
                source,
            })?
            .is_some();
        let port_released: bool = wait_for_port_release(self.port, SERVER_STOP_TIMEOUT);
        self.bundle
            .tool_root
            .remove_directory_if_exists(&self.bundle.state_relative.join("temp"))?;
        let temporary_root_removed: bool = !self.bundle.temporary_root.exists();
        if !process_stopped || !port_released || !temporary_root_removed {
            return Err(IsolatedAdbError::Cleanup {
                process_stopped,
                port_released,
                temporary_root_removed,
            });
        }
        append_server_event(
            self.bundle.log_sink.as_ref(),
            &self.log_path,
            "adb.server.stopped",
            "ok",
            serde_json::json!({"process_id": self.process_id, "process_stopped": process_stopped, "port_released": port_released}),
        )?;
        Ok(AdbShutdownEvidence {
            process_stopped,
            port_released,
            temporary_root_removed,
        })
    }
}

#[cfg(target_os = "windows")]
impl Drop for OwnedAdbServer {
    /// 调用方提前返回时仍定向终止当前对象持有的 ADB 子进程。
    fn drop(&mut self) {
        if self.child.is_some()
            && let Err(error) = self.stop_owned_process()
        {
            eprintln!("隔离 ADB 兜底清理失败: {error}");
        }
    }
}

/// 构造不含系统默认 5037 的前台服务参数，并约束服务只接纳唯一目标。
#[cfg(target_os = "windows")]
pub(crate) fn server_arguments(port: u16, serial: &str) -> Vec<String> {
    vec![
        "-L".to_owned(),
        format!("tcp:localhost:{port}"),
        "--one-device".to_owned(),
        serial.to_owned(),
        "server".to_owned(),
        "nodaemon".to_owned(),
    ]
}

/// 在唯一动态端口启动一次前台 ADB 服务，并等待自有子进程开始监听。
#[cfg(target_os = "windows")]
fn start_once(bundle: AdbBundle, serial: String) -> Result<OwnedAdbServer, IsolatedAdbError> {
    let reservation: TcpListener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(|source| IsolatedAdbError::Io {
            stage: "adb.reserve_server_port",
            path: bundle.state_root.clone(),
            source,
        })?;
    let port: u16 = reservation
        .local_addr()
        .map_err(|source| IsolatedAdbError::Io {
            stage: "adb.read_reserved_port",
            path: bundle.state_root.clone(),
            source,
        })?
        .port();
    drop(reservation);

    bundle
        .tool_root
        .ensure_directory(&bundle.state_relative.join("temp"))?;
    let timestamp_ms: u128 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let (log_path, mut stdout_file) = bundle
        .log_sink
        .create(
            &bundle.log_root,
            &format!("adb-server-{timestamp_ms}-{port}.log"),
        )
        .map_err(|source| IsolatedAdbError::Io {
            stage: "adb.create_server_log",
            path: bundle.log_root.clone(),
            source,
        })?;
    bundle
        .log_sink
        .write_event(
            &mut stdout_file,
            "adb.server.start",
            "starting",
            serde_json::json!({"port": port, "revision": bundle.revision}),
        )
        .map_err(|source| IsolatedAdbError::Io {
            stage: "adb.write_server_log",
            path: log_path.clone(),
            source,
        })?;
    let failure_log_path = log_path.clone();
    let failure_log_sink = Arc::clone(&bundle.log_sink);
    let result = (|| {
        let stderr_file: File = stdout_file
            .try_clone()
            .map_err(|source| IsolatedAdbError::Io {
                stage: "adb.clone_server_log",
                path: log_path.clone(),
                source,
            })?;
        let executable: String = windows_path(&bundle.executable)?;
        let mut command: Command = Command::new(&executable);
        bundle.process_policy().apply_to(&mut command);
        hide_console_window(&mut command);
        command
            .args(server_arguments(port, &serial))
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        let mut child: Child = command.spawn().map_err(|source| IsolatedAdbError::Io {
            stage: "adb.start_owned_server",
            path: bundle.executable.clone(),
            source,
        })?;
        let process_id: u32 = child.id();
        let deadline: Instant = Instant::now() + SERVER_READY_TIMEOUT;
        let address: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(IsolatedAdbError::InvalidOutput {
                        stage: "adb.start_owned_server",
                        message: format!(
                            "自有服务在监听前退出: {status}; 日志={}",
                            log_path.display()
                        ),
                    });
                }
                Ok(None) => {}
                Err(source) => {
                    let operation = IsolatedAdbError::Io {
                        stage: "adb.inspect_starting_server",
                        path: bundle.executable.clone(),
                        source,
                    };
                    return Err(start_error_after_process_cleanup(
                        &mut child,
                        &bundle.executable,
                        operation,
                    ));
                }
            }
            if TcpStream::connect_timeout(&address.into(), Duration::from_millis(200)).is_ok() {
                return Ok(OwnedAdbServer {
                    bundle,
                    serial,
                    port,
                    process_id,
                    log_path,
                    child: Some(child),
                });
            }
            if Instant::now() >= deadline {
                let operation = IsolatedAdbError::InvalidOutput {
                    stage: "adb.start_owned_server",
                    message: format!("10 秒内未监听回环端口 {port}; 日志={}", log_path.display()),
                };
                return Err(start_error_after_process_cleanup(
                    &mut child,
                    &bundle.executable,
                    operation,
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    })();
    if let Err(error) = &result
        && let Err(log_error) = append_server_event(
            failure_log_sink.as_ref(),
            &failure_log_path,
            "adb.server.failure",
            "error",
            serde_json::json!({"message": error.to_string()}),
        )
    {
        return Err(IsolatedAdbError::LogWrite {
            operation: error.to_string(),
            log_error: log_error.to_string(),
        });
    }
    result
}

/// 原始输出与生命周期事件追加到同一日志，文件始终保留在日志目录。
#[cfg(target_os = "windows")]
pub(crate) fn append_server_event(
    sink: &dyn AdbLogSink,
    path: &Path,
    stage: &str,
    status: &str,
    details: serde_json::Value,
) -> Result<(), IsolatedAdbError> {
    let mut log = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|source| IsolatedAdbError::Io {
            stage: "adb.open_server_log",
            path: path.to_path_buf(),
            source,
        })?;
    sink.write_event(&mut log, stage, status, details)
        .map_err(|source| IsolatedAdbError::Io {
            stage: "adb.write_server_log",
            path: path.to_path_buf(),
            source,
        })
}

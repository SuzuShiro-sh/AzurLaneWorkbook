//! 仅清理当前对象直接持有的子进程和端口。
use crate::IsolatedAdbError;
use std::{
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};
/// 启动失败时定向终止刚创建的子进程，并在双重失败时保留两侧原因。
#[cfg(target_os = "windows")]
pub(crate) fn start_error_after_process_cleanup(
    child: &mut Child,
    executable: &Path,
    operation: IsolatedAdbError,
) -> IsolatedAdbError {
    match kill_and_wait_process(
        child,
        executable,
        "adb.stop_starting_server",
        "adb.wait_starting_server",
    ) {
        Ok(()) => operation,
        Err(cleanup) => IsolatedAdbError::StartingProcessCleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        },
    }
}

/// 处理退出竞态后等待子进程，避免启动失败路径遗留孤立 ADB 服务。
#[cfg(target_os = "windows")]
pub(crate) fn kill_and_wait_process(
    child: &mut Child,
    executable: &Path,
    stop_stage: &'static str,
    wait_stage: &'static str,
) -> Result<(), IsolatedAdbError> {
    if let Err(kill_source) = child.kill() {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) | Err(_) => {
                return Err(IsolatedAdbError::Io {
                    stage: stop_stage,
                    path: executable.to_path_buf(),
                    source: kill_source,
                });
            }
        }
    }
    child.wait().map_err(|source| IsolatedAdbError::Io {
        stage: wait_stage,
        path: executable.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// 等待当前子进程使用的回环端口不再接受连接。
#[cfg(target_os = "windows")]
pub(crate) fn wait_for_port_release(port: u16, timeout: Duration) -> bool {
    let deadline: Instant = Instant::now() + timeout;
    let address: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address.into(), Duration::from_millis(100)).is_err() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

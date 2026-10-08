use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// 资源与可写目录均相对于调用方提供的受控根目录。
#[derive(Clone, Debug)]
pub struct AdbConfig {
    /// 已存在的受控根目录。
    pub root: PathBuf,
    /// ADB 可执行文件相对路径，其依赖文件位于同目录。
    pub executable: PathBuf,
    /// 独占可写状态目录；与资源、日志目录互不包含，不供多个服务并发复用。
    pub state_directory: PathBuf,
    /// 生命周期及服务输出的持久化目录。
    pub log_directory: PathBuf,
}

/// 调用方提供日志文件和事件写入策略；每个方法须保留 I/O 失败信息。
pub trait AdbLogSink: std::fmt::Debug + Send + Sync {
    fn create(&self, directory: &Path, name: &str) -> io::Result<(PathBuf, File)>;
    fn write_event(
        &self,
        file: &mut File,
        stage: &str,
        status: &str,
        details: serde_json::Value,
    ) -> io::Result<()>;
}

#[cfg(any(target_os = "windows", test))]
use crate::IsolatedAdbError;
#[cfg(any(target_os = "windows", test))]
use std::net::SocketAddr;

#[cfg(any(target_os = "windows", test))]
pub(crate) fn validate_serial(serial: &str) -> Result<SocketAddr, IsolatedAdbError> {
    let address: SocketAddr = serial
        .parse()
        .map_err(|_| IsolatedAdbError::InvalidSerial {
            serial: serial.to_owned(),
        })?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(IsolatedAdbError::InvalidSerial {
            serial: serial.to_owned(),
        });
    }
    Ok(address)
}

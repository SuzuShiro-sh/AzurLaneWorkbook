use std::path::PathBuf;
use thiserror::Error;

/// 模拟器发现、协议与参数错误；由调用方转换为自己的业务错误。
#[derive(Debug, Error)]
pub enum EmulatorError {
    #[error("模拟器参数 {field} 无效: {message}")]
    InvalidOption {
        field: &'static str,
        message: String,
    },
    #[error("模拟器发现失败: {message}")]
    Discovery { message: String },
    #[error("模拟器安装不唯一: {message}")]
    AmbiguousManager { message: String },
    #[cfg(target_os = "windows")]
    #[error("模拟器安装登记读取失败: {message}")]
    Registry { message: String },
    #[cfg(target_os = "windows")]
    #[error("模拟器命令 {stage} 失败: {message}")]
    ManagerCommand {
        stage: &'static str,
        message: String,
    },
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

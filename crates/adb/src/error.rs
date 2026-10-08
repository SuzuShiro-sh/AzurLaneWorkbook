//! ADB 资源、进程与清理错误。
use std::path::PathBuf;
use suzushiro_controlled_root::ControlledRootError;
#[cfg(target_os = "windows")]
use suzushiro_host_command::NativeCommandError;
use thiserror::Error;

/// 随包文件、密钥、独立进程或定向客户端命令不满足隔离契约。
#[derive(Debug, Error)]
pub enum IsolatedAdbError {
    /// 随包文件缺失、越界、过大或属性不完整。
    #[error("随包 ADB 文件 {path} 无效: {message}")]
    InvalidBundle { path: PathBuf, message: String },
    /// 调用方给出的序列号不是非零回环地址。
    #[cfg(any(target_os = "windows", test))]
    #[error("ADB 唯一目标必须是非零端口的回环地址，实际为 {serial:?}")]
    InvalidSerial { serial: String },
    /// 工具内文件系统操作失败。
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 工具根目录或其受控相对路径不满足零越界约束。
    #[error(transparent)]
    ControlledRoot(#[from] ControlledRootError),
    /// 共享有界原生命令执行器失败。
    #[cfg(target_os = "windows")]
    #[error(transparent)]
    NativeCommand(#[from] NativeCommandError),
    /// Windows 缺少构造最小 ADB 环境所需的系统根目录。
    #[cfg(target_os = "windows")]
    #[error("宿主环境变量 {name} 无效: {message}")]
    HostEnvironment { name: &'static str, message: String },
    /// ADB 客户端返回非零状态。
    #[cfg(target_os = "windows")]
    #[error("ADB 阶段 {stage} 返回 {exit_code}，stdout={stdout:?}，stderr={stderr:?}")]
    CommandStatus {
        stage: &'static str,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    /// ADB 输出没有形成要求的唯一目标状态。
    #[cfg(target_os = "windows")]
    #[error("ADB 阶段 {stage} 输出无效: {message}")]
    InvalidOutput {
        stage: &'static str,
        message: String,
    },
    /// 保留业务错误和日志持久化错误。
    #[cfg(target_os = "windows")]
    #[error("{operation}; 日志写入失败: {log_error}")]
    LogWrite {
        operation: String,
        log_error: String,
    },
    /// 所有动态端口启动尝试均失败。
    #[cfg(target_os = "windows")]
    #[error("隔离 ADB 服务连续 {attempts} 次启动失败: {messages}")]
    ServerStart { attempts: u32, messages: String },
    /// ADB 服务启动失败后，其临时目录也无法完成定向清理。
    #[cfg(target_os = "windows")]
    #[error("隔离 ADB 服务连续 {attempts} 次启动失败: {messages}; 临时目录清理同时失败: {cleanup}")]
    ServerStartCleanup {
        attempts: u32,
        messages: String,
        cleanup: String,
    },
    /// ADB 服务启动失败后，刚创建的子进程也无法确认回收。
    #[cfg(target_os = "windows")]
    #[error("隔离 ADB 服务启动失败: {operation}; 子进程清理同时失败: {cleanup}")]
    StartingProcessCleanup { operation: String, cleanup: String },
    /// 密钥创建或发布失败后，当前调用的暂存文件也无法全部删除。
    #[cfg(target_os = "windows")]
    #[error("ADB 密钥操作失败: {operation}; 暂存文件清理同时失败: {cleanup}")]
    KeyStagingCleanup { operation: String, cleanup: String },
    /// ADB 服务对象已经停止，不能重复清理。
    #[cfg(target_os = "windows")]
    #[error("隔离 ADB 服务已经停止")]
    AlreadyStopped,
    /// 定向终止后仍有当前会话资源未释放。
    #[cfg(target_os = "windows")]
    #[error(
        "隔离 ADB 清理不完整: process_stopped={process_stopped}, port_released={port_released}, temporary_root_removed={temporary_root_removed}"
    )]
    Cleanup {
        process_stopped: bool,
        port_released: bool,
        temporary_root_removed: bool,
    },
}

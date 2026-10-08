//! 模拟器安装发现、厂商协议与统一实例模型；不拥有游戏或 ADB 会话。
#[cfg(target_os = "windows")]
mod adapter;
#[cfg(target_os = "windows")]
pub mod discovery;
#[cfg(target_os = "windows")]
mod endpoint;
mod error;
mod instance;
#[cfg(target_os = "windows")]
mod ldplayer;
#[cfg(target_os = "windows")]
pub mod manager_command;
#[cfg(target_os = "windows")]
mod mumu12;
#[cfg(target_os = "windows")]
mod process_image;
#[cfg(target_os = "windows")]
mod registry;
#[cfg(target_os = "windows")]
pub mod transport;
#[cfg(target_os = "windows")]
pub use adapter::{EmulatorAdapter, INSTANCE_QUERY_TIMEOUT, InstanceQueryReport, RootTransport};
pub use error::EmulatorError;
pub use instance::{EmulatorInstance, TargetState, validate_instance_index, validate_serial};
#[cfg(target_os = "windows")]
pub use registry::{ADAPTERS, adapter_for_manager, split_selection};

#[cfg(test)]
#[cfg(target_os = "windows")]
mod tests;

mod command;
pub use command::RemoteShellOutput;

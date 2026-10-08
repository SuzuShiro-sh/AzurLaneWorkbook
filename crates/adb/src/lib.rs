//! 随包 ADB 资源、受控进程环境、独立服务生命周期与定向命令。
mod bundle;
mod command;
mod config;
mod error;
pub use bundle::{AdbBundle, parse_revision};
pub use command::{adb_client_arguments, run_adb_client};
pub use config::{AdbConfig, AdbLogSink};
pub use error::IsolatedAdbError;
#[cfg(target_os = "windows")]
mod cleanup;
#[cfg(target_os = "windows")]
mod client;
#[cfg(target_os = "windows")]
mod environment;
#[cfg(target_os = "windows")]
mod keys;
#[cfg(target_os = "windows")]
mod server;
#[cfg(target_os = "windows")]
pub use client::AdbCommandStatus;
#[cfg(target_os = "windows")]
pub use server::{AdbShutdownEvidence, OwnedAdbServer};
#[cfg(test)]
mod tests;

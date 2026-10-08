//! 外部系统适配器。

/// 单个发布文件和运行态 loader 共用的最大读取边界。
pub(crate) const MAXIMUM_RELEASE_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// 需要扫描版本标记并部署到目标进程的 Agent 最大读取边界。
pub(crate) const MAXIMUM_AGENT_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub mod device;
mod diagnostics;
mod file_snapshot;
mod history;
mod json_artifact;
pub(crate) mod numbered_files;
pub mod release;
pub mod settings;
pub mod tool_root;
pub mod workbook;

pub use diagnostics::journal::RelatedLogSink;
pub(crate) use diagnostics::journal::{create_numbered_log, write_event};
pub(crate) use diagnostics::log_catalog;
pub(crate) use diagnostics::open as diagnostic_open;
pub(crate) use history::catalog as history_catalog;
pub(crate) use history::check as check_history;
pub use history::execution as execution_history;
pub use release::assembly as release_assembly;
pub(crate) use settings::port as settings_port;

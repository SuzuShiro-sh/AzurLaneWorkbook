//! 组织主程序的命令解析、执行和诊断边界。

mod artifact_details;
mod command;
mod diagnostics;
mod equipment_actions;
mod help;
mod query;
mod runner;

pub(super) use command::{StartupCommand, parse_command};
pub(super) use runner::run;

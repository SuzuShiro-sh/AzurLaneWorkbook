//! 提供正式原生窗口入口以及共享的发布、布局维护、检查和数据生成命令。

use std::env;
use std::ffi::OsString;
use std::process::ExitCode;

use azur_lane_workbook::interfaces::native_gui::show_error_dialog;

mod cli;

use cli::{StartupCommand, parse_command, run};

const APPLICATION_ERROR_EXIT_CODE: u8 = 1;

/// 无参数启动原生窗口；显式子命令执行对应的发布、布局或数据生成操作。
fn main() -> ExitCode {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    // CLI 保留控制台以支持直接输出和等待；GUI 启动前释放本进程的控制台关联。
    #[cfg(target_os = "windows")]
    if arguments.is_empty() && unsafe { windows_sys::Win32::System::Console::FreeConsole() } == 0 {
        show_error_dialog(&format!(
            "释放控制台失败：{}",
            std::io::Error::last_os_error()
        ));
        return ExitCode::from(APPLICATION_ERROR_EXIT_CODE);
    }
    let command: StartupCommand = match parse_command(&arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(APPLICATION_ERROR_EXIT_CODE);
        }
    };
    let uses_error_dialog = command == StartupCommand::Gui;
    match run(command) {
        Ok(outcome) => ExitCode::from(outcome.exit_code_value()),
        Err(error) => {
            if uses_error_dialog {
                show_error_dialog(&error);
            } else {
                eprintln!("{error}");
            }
            ExitCode::from(APPLICATION_ERROR_EXIT_CODE)
        }
    }
}

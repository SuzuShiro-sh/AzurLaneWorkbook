//! 通过现有原生命令接口调用 模拟器管理器，保留命令阶段与失败输出。

use super::EmulatorError;
use crate::command::{NativeOutput, run_native};
use std::path::Path;
use std::time::Duration;
use suzushiro_host_command::run_native_with_timeout;

/// 运行 模拟器管理器 并把非零退出状态转换为带完整输出的阶段错误。
pub fn manager_checked(
    manager: &Path,
    arguments: &[String],
    stage: &'static str,
) -> Result<String, EmulatorError> {
    let executable: String = windows_path(manager, "manager")?;
    let output: NativeOutput = run_native(&executable, arguments, stage).map_err(|error| {
        EmulatorError::ManagerCommand {
            stage,
            message: error.to_string(),
        }
    })?;
    checked_output(output, stage)
}

/// 将原生 Windows 路径转换为可直接交给子进程的 Unicode 文本。
pub fn windows_path(path: &Path, field: &'static str) -> Result<String, EmulatorError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| EmulatorError::InvalidOption {
            field,
            message: format!("Windows 路径不是有效 Unicode: {}", path.display()),
        })
}

/// 查询命令使用显式预算，沿用宿主执行器的超时回收和输出边界。
pub fn manager_checked_with_timeout(
    manager: &Path,
    arguments: &[String],
    stage: &'static str,
    timeout: Duration,
) -> Result<String, EmulatorError> {
    let executable = windows_path(manager, "manager")?;
    let output =
        run_native_with_timeout(&executable, arguments, stage, timeout).map_err(|error| {
            EmulatorError::ManagerCommand {
                stage,
                message: error.to_string(),
            }
        })?;
    checked_output(output, stage)
}

fn checked_output(output: NativeOutput, stage: &'static str) -> Result<String, EmulatorError> {
    if output.exit_code != 0 {
        return Err(EmulatorError::ManagerCommand {
            stage,
            message: format!(
                "返回 {}，stdout={:?}，stderr={:?}",
                output.exit_code, output.stdout, output.stderr
            ),
        });
    }
    Ok(output.stdout.trim().to_owned())
}

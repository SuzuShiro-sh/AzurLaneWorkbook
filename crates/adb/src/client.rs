//! 绑定唯一目标和独立端口的 ADB 客户端命令。
use crate::command::{adb_client_arguments, run_adb_client};
use crate::environment::windows_path;
use crate::{IsolatedAdbError, OwnedAdbServer};
use suzushiro_host_command::{NativeOutput, run_native_lossy_with_policy};

/// 保留无需成功退出的定向 ADB 客户端结果。
#[cfg(target_os = "windows")]
pub struct AdbCommandStatus {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl OwnedAdbServer {
    /// 对唯一序列号执行命令，并拒绝任何非零状态。
    pub fn run_target_checked(
        &self,
        arguments: &[String],
        stage: &'static str,
    ) -> Result<String, IsolatedAdbError> {
        let output: AdbCommandStatus = self.run_target_status(arguments, stage)?;
        if output.exit_code != 0 {
            return Err(IsolatedAdbError::CommandStatus {
                stage,
                exit_code: output.exit_code,
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        Ok(output.stdout.trim().to_owned())
    }

    /// 对唯一序列号执行命令并保留非零状态，供进程未启动等预期分支判断。
    pub fn run_target_status(
        &self,
        arguments: &[String],
        stage: &'static str,
    ) -> Result<AdbCommandStatus, IsolatedAdbError> {
        self.run_status(Some(self.serial.as_str()), arguments, stage)
    }

    /// `adb connect` 的本地化提示不参与判定，只保留退出状态和可读诊断。
    pub(crate) fn run_global_lossy_checked(
        &self,
        arguments: &[String],
        stage: &'static str,
    ) -> Result<(), IsolatedAdbError> {
        let executable: String = windows_path(&self.bundle.executable)?;
        let complete_arguments = adb_client_arguments(Some(self.port), None, arguments);
        let output: NativeOutput = run_native_lossy_with_policy(
            &executable,
            &complete_arguments,
            stage,
            self.bundle.process_policy(),
        )?;
        if output.exit_code != 0 {
            return Err(IsolatedAdbError::CommandStatus {
                stage,
                exit_code: output.exit_code,
                stdout: output.stdout,
                stderr: output.stderr,
            });
        }
        Ok(())
    }

    fn run_status(
        &self,
        serial: Option<&str>,
        arguments: &[String],
        stage: &'static str,
    ) -> Result<AdbCommandStatus, IsolatedAdbError> {
        let executable: String = windows_path(&self.bundle.executable)?;
        let complete_arguments = adb_client_arguments(Some(self.port), serial, arguments);
        let output: NativeOutput = run_adb_client(
            &executable,
            Some(self.bundle.process_policy()),
            &complete_arguments,
            stage,
            None,
        )?;
        Ok(AdbCommandStatus {
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

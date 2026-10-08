//! 复用宿主命令执行器，统一远端 shell 的退出状态回执。

pub use suzushiro_host_command::{
    NativeCommandError, NativeOutput, run_native, run_native_with_timeout,
};

const REMOTE_EXIT_PREFIX: &str = "__SUZUSHIRO_REMOTE_EXIT__=";
const MAX_REMOTE_EXIT_CODE: i32 = 255;

/// 保存远端 root shell 的远端退出状态和合并输出。
pub struct RemoteShellOutput {
    pub exit_code: i32,
    pub stdout: String,
}

/// 厂商适配器提供命令参数，统一执行器负责传输状态和远端退出回执。
pub fn run_manager_root_arguments(
    manager_executable: &str,
    arguments: &[String],
    stage: &'static str,
    timeout: Option<std::time::Duration>,
) -> Result<RemoteShellOutput, NativeCommandError> {
    let output = match timeout {
        Some(timeout) => run_native_with_timeout(manager_executable, arguments, stage, timeout)?,
        None => run_native(manager_executable, arguments, stage)?,
    };
    if output.exit_code != 0 {
        return Err(NativeCommandError::Failed {
            stage,
            message: format!(
                "管理器返回 {}，stdout={:?}，stderr={:?}",
                output.exit_code, output.stdout, output.stderr
            ),
        });
    }
    if !output.stderr.trim().is_empty() {
        return Err(NativeCommandError::Failed {
            stage,
            message: format!("管理器传输产生未归属 stderr: {:?}", output.stderr),
        });
    }
    parse_remote_shell_output(&output.stdout, stage)
}

/// 为不传播远端状态的传输建立统一退出回执。
pub fn wrap_remote_command(command: &str) -> String {
    format!("( {command} ) 2>&1; remote_status=$?; echo; echo {REMOTE_EXIT_PREFIX}$remote_status")
}

/// 只接受输出末尾的单个十进制状态尾标，避免把管理器固定退出码当成远端成功。
pub fn parse_remote_shell_output(
    stdout: &str,
    stage: &'static str,
) -> Result<RemoteShellOutput, NativeCommandError> {
    let trimmed = stdout.trim_end_matches(['\r', '\n']);
    let (payload, marker) = match trimmed.rsplit_once('\n') {
        Some((payload, marker)) => (payload, marker.trim_end_matches('\r')),
        None => ("", trimmed),
    };
    let status_text =
        marker
            .strip_prefix(REMOTE_EXIT_PREFIX)
            .ok_or_else(|| NativeCommandError::Failed {
                stage,
                message: format!("远端 root shell 缺少末尾状态尾标: {stdout:?}"),
            })?;
    if status_text.is_empty()
        || !status_text.bytes().all(|byte| byte.is_ascii_digit())
        || status_text.len() > 3
    {
        return Err(NativeCommandError::Failed {
            stage,
            message: format!("远端 root shell 状态无效: {status_text:?}"),
        });
    }
    let exit_code: i32 = status_text
        .parse()
        .map_err(|_| NativeCommandError::Failed {
            stage,
            message: format!("远端 root shell 状态无法解析: {status_text:?}"),
        })?;
    if !(0..=MAX_REMOTE_EXIT_CODE).contains(&exit_code) || status_text != exit_code.to_string() {
        return Err(NativeCommandError::Failed {
            stage,
            message: format!("远端 root shell 状态超出规范: {status_text:?}"),
        });
    }
    Ok(RemoteShellOutput {
        exit_code,
        stdout: payload.trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_remote_shell_output;

    /// 状态解析保留合并输出，并恢复成功与远端失败状态。
    #[test]
    fn remote_shell_output_restores_remote_status_and_output() {
        let success =
            parse_remote_shell_output("root-ok\r\n\r\n__SUZUSHIRO_REMOTE_EXIT__=0\r\n", "test")
                .unwrap();
        assert_eq!(success.exit_code, 0);
        assert_eq!(success.stdout, "root-ok");

        let failure = parse_remote_shell_output(
            "root-out\r\nroot-err\r\n\r\n__SUZUSHIRO_REMOTE_EXIT__=9\r\n",
            "test",
        )
        .unwrap();
        assert_eq!(failure.exit_code, 9);
        assert_eq!(failure.stdout, "root-out\r\nroot-err");
    }

    /// 缺失、非末尾或超出 shell 范围的状态尾标都不得伪装成成功。
    #[test]
    fn remote_shell_output_rejects_invalid_status_markers() {
        for output in [
            "root-out\n",
            "__SUZUSHIRO_REMOTE_EXIT__=0\ntrailing\n",
            "__SUZUSHIRO_REMOTE_EXIT__=-1\n",
            "__SUZUSHIRO_REMOTE_EXIT__=00\n",
            "__SUZUSHIRO_REMOTE_EXIT__=256\n",
            "__SUZUSHIRO_REMOTE_EXIT__=text\n",
        ] {
            assert!(parse_remote_shell_output(output, "test").is_err());
        }
    }
}

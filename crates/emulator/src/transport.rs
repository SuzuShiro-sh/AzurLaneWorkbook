//! 在调用方拥有的管理器或隔离 ADB 通道上执行 root 命令，不创建会话资源。
use super::{EmulatorAdapter, RootTransport};
use crate::command::{
    NativeCommandError, RemoteShellOutput, parse_remote_shell_output, run_manager_root_arguments,
};
use std::time::Duration;

pub struct RootShell<'a> {
    pub adapter: &'a dyn EmulatorAdapter,
    pub manager: &'a str,
    pub index: &'a str,
}

impl RootShell<'_> {
    /// 调用方只提供已绑定目标的 ADB 执行能力与错误映射，传输分派和回执解析由此统一完成。
    pub fn run<E>(
        &self,
        command: &str,
        stage: &'static str,
        timeout: Option<Duration>,
        adb: impl FnOnce(&[String], &'static str, Option<Duration>) -> Result<String, E>,
        native_error: impl Fn(NativeCommandError) -> E,
    ) -> Result<RemoteShellOutput, E> {
        let arguments = self.adapter.root_arguments(self.index, command);
        match self.adapter.root_transport() {
            RootTransport::Manager => {
                run_manager_root_arguments(self.manager, &arguments, stage, timeout)
                    .map_err(native_error)
            }
            RootTransport::AdbSu => {
                let output = adb(&arguments, stage, timeout)?;
                parse_remote_shell_output(&output, stage).map_err(native_error)
            }
        }
    }
}

pub fn adb_root_arguments(command: &str) -> Vec<String> {
    let wrapped = crate::command::wrap_remote_command(command);
    let quoted = format!("'{}'", wrapped.replace('\'', "'\"'\"'"));
    vec!["shell".to_owned(), format!("su -c {quoted}")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter_for_manager;
    use std::path::Path;

    #[test]
    fn bound_adb_receives_arguments_stage_and_timeout_and_keeps_remote_failure() {
        let shell = RootShell {
            adapter: adapter_for_manager(Path::new("ldconsole.exe")).unwrap(),
            manager: "ldconsole.exe",
            index: "4",
        };
        let receipt = shell
            .run(
                "printf '%s' value; exit 7",
                "test.root",
                Some(Duration::from_secs(3)),
                |arguments, stage, timeout| {
                    assert_eq!(arguments[0], "shell");
                    assert!(arguments[1].starts_with("su -c '"));
                    assert!(arguments[1].contains("exit 7"));
                    assert_eq!(stage, "test.root");
                    assert_eq!(timeout, Some(Duration::from_secs(3)));
                    Ok::<_, String>("value\n__SUZUSHIRO_REMOTE_EXIT__=7\n".to_owned())
                },
                |error| error.to_string(),
            )
            .unwrap();
        assert_eq!(receipt.exit_code, 7);
        assert_eq!(receipt.stdout, "value");
    }

    #[test]
    fn bound_adb_preserves_transport_errors_and_rejects_missing_receipts() {
        let shell = RootShell {
            adapter: adapter_for_manager(Path::new("ldconsole.exe")).unwrap(),
            manager: "ldconsole.exe",
            index: "0",
        };
        let error = shell
            .run(
                "id",
                "test.root",
                None,
                |_, _, _| Err::<String, _>("adb disconnected".to_owned()),
                |error| error.to_string(),
            )
            .err()
            .unwrap();
        assert_eq!(error, "adb disconnected");
        assert!(
            shell
                .run(
                    "id",
                    "test.root",
                    None,
                    |_, _, _| Ok::<_, String>("uid=0(root)".to_owned()),
                    |error| error.to_string()
                )
                .is_err()
        );
    }
}

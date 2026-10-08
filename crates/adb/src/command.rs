//! 在已经运行的 ADB 服务上组装并执行客户端命令。
//! 服务进程的启动和停止仍由拥有者负责。

use std::time::Duration;

use suzushiro_host_command::{
    NativeCommandError, NativeCommandPolicy, NativeOutput, run_native, run_native_with_policy,
    run_native_with_policy_timeout, run_native_with_timeout,
};

/// 把可选服务端口和目标序列号放在调用方参数前面。
pub fn adb_client_arguments(
    server_port: Option<u16>,
    serial: Option<&str>,
    arguments: &[String],
) -> Vec<String> {
    let mut complete_arguments = Vec::with_capacity(arguments.len() + 4);
    if let Some(port) = server_port {
        complete_arguments.push("-P".to_owned());
        complete_arguments.push(port.to_string());
    }
    if let Some(serial) = serial {
        complete_arguments.push("-s".to_owned());
        complete_arguments.push(serial.to_owned());
    }
    complete_arguments.extend_from_slice(arguments);
    complete_arguments
}

/// 按调用方的进程策略和剩余时间执行已经组装好的 ADB 参数。
pub fn run_adb_client(
    executable: &str,
    process_policy: Option<&NativeCommandPolicy>,
    arguments: &[String],
    stage: &'static str,
    timeout: Option<Duration>,
) -> Result<NativeOutput, NativeCommandError> {
    match (process_policy, timeout) {
        (Some(policy), Some(timeout)) => {
            run_native_with_policy_timeout(executable, arguments, stage, policy, timeout)
        }
        (Some(policy), None) => run_native_with_policy(executable, arguments, stage, policy),
        (None, Some(timeout)) => run_native_with_timeout(executable, arguments, stage, timeout),
        (None, None) => run_native(executable, arguments, stage),
    }
}

//! 启动指定模拟器实例和游戏，并等待可连接的目标进程。

#[cfg(target_os = "windows")]
use super::{EXPECTED_ABI, POLL_INTERVAL, PortableProbeError, TargetEvidence, adb_failure};
#[cfg(target_os = "windows")]
use std::{
    collections::BTreeMap,
    path::Path,
    thread,
    time::{Duration, Instant},
};
#[cfg(target_os = "windows")]
use suzushiro_adb::{AdbCommandStatus, OwnedAdbServer};
#[cfg(target_os = "windows")]
use suzushiro_emulator::EmulatorInstance;
#[cfg(target_os = "windows")]
use suzushiro_emulator::adapter_for_manager;
#[cfg(target_os = "windows")]
use suzushiro_emulator::manager_command::manager_checked;

/// 启动或等待指定实例，并只接受同一索引最终进入完整就绪状态。
#[cfg(target_os = "windows")]
pub(super) fn wait_for_ready_instance(
    manager: &Path,
    index: &str,
    timeout: Duration,
) -> Result<EmulatorInstance, PortableProbeError> {
    let deadline: Instant = Instant::now() + timeout;
    let mut last_state: String = String::new();
    while Instant::now() < deadline {
        let instances: BTreeMap<String, EmulatorInstance> = adapter_for_manager(manager)?
            .query_instances(
                manager,
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(suzushiro_emulator::INSTANCE_QUERY_TIMEOUT),
            )?
            .instances;
        if let Some(instance) = instances.get(index) {
            last_state = format!(
                "process={}, android={}",
                instance.is_process_started, instance.is_android_started
            );
            if instance.is_ready() {
                return Ok(instance.clone());
            }
        } else {
            last_state = "实例从 info -v all 消失".to_owned();
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
    Err(PortableProbeError::Discovery {
        message: format!(
            "等待实例 {index} 就绪超过 {} 秒，最后状态: {last_state}",
            timeout.as_secs()
        ),
    })
}

/// 联合验证 ADB 序列号、ABI、包、root 和唯一 PID，并遵守入口的游戏启动策略。
#[cfg(target_os = "windows")]
pub(super) fn verify_target_and_start_game(
    manager: &Path,
    instance_index: &str,
    serial: &str,
    target_package: &str,
    startup_timeout: Duration,
    allow_game_launch: bool,
    server: &OwnedAdbServer,
) -> Result<TargetEvidence, PortableProbeError> {
    let reported_serial: String = server
        .run_target_checked(&["get-serialno".to_owned()], "target.get_serial")
        .map_err(|source| adb_failure("adb.target", source))?;
    if reported_serial.trim() != serial {
        return Err(PortableProbeError::Target {
            stage: "target.get_serial",
            message: format!("ADB 返回 {reported_serial:?}，期望 {serial:?}"),
        });
    }
    let abi: String = server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "getprop".to_owned(),
                "ro.product.cpu.abi".to_owned(),
            ],
            "target.read_abi",
        )
        .map_err(|source| adb_failure("adb.target", source))?;
    let abi_list: String = server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "getprop".to_owned(),
                "ro.product.cpu.abilist".to_owned(),
            ],
            "target.read_abi_list",
        )
        .map_err(|source| adb_failure("adb.target", source))?;
    if abi.trim() != EXPECTED_ABI
        || !abi_list
            .split(',')
            .any(|candidate: &str| candidate.trim() == EXPECTED_ABI)
    {
        return Err(PortableProbeError::Target {
            stage: "target.verify_abi",
            message: format!("只支持 {EXPECTED_ABI}，实际 abi={abi:?}, abilist={abi_list:?}"),
        });
    }
    let package_output: String = server
        .run_target_checked(
            &[
                "shell".to_owned(),
                "pm".to_owned(),
                "path".to_owned(),
                target_package.to_owned(),
            ],
            "target.verify_package",
        )
        .map_err(|source| adb_failure("adb.target", source))?;
    let package_paths: Vec<String> = package_output
        .lines()
        .map(str::trim)
        .filter(|line: &&str| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if package_paths.is_empty()
        || !package_paths
            .iter()
            .all(|line: &String| line.starts_with("package:/") && line.ends_with(".apk"))
    {
        return Err(PortableProbeError::Target {
            stage: "target.verify_package",
            message: format!("目标包路径无效: {package_output:?}"),
        });
    }
    let adapter = adapter_for_manager(manager)?;
    let executable = suzushiro_emulator::manager_command::windows_path(manager, "manager")?;
    let receipt = suzushiro_emulator::transport::RootShell {
        adapter,
        manager: &executable,
        index: instance_index,
    }
    .run(
        "id",
        "target.verify_root",
        None,
        |arguments, stage, _| {
            server
                .run_target_checked(arguments, stage)
                .map_err(|source| adb_failure(stage, source))
        },
        |error| PortableProbeError::Target {
            stage: "target.verify_root",
            message: error.to_string(),
        },
    )?;
    if receipt.exit_code != 0 {
        return Err(PortableProbeError::Target {
            stage: "target.verify_root",
            message: format!("root 命令失败，请检查模拟器 root 设置: {}", receipt.stdout),
        });
    }
    let root_identity = receipt.stdout;
    if !root_identity.contains("uid=0(root)") {
        return Err(PortableProbeError::Target {
            stage: "target.verify_root",
            message: format!("模拟器 root shell 身份不是 root: {root_identity:?}"),
        });
    }
    let initial_pid: Option<u32> = read_target_pid(server, target_package)?;
    let game_started_by_probe = game_launch_required(initial_pid, allow_game_launch)?;
    if game_started_by_probe {
        server
            .run_target_checked(&adb_game_launch_arguments(target_package), "game.launch")
            .map_err(|source| adb_failure("adb.game_launch", source))?;
    }
    let process_id: u32 = wait_for_target_pid(server, target_package, startup_timeout)?;
    Ok(TargetEvidence {
        serial: serial.to_owned(),
        abi: abi.trim().to_owned(),
        abi_list: abi_list.trim().to_owned(),
        package_name: target_package.to_owned(),
        package_paths,
        process_id,
        root_identity: root_identity.trim().to_owned(),
        game_started_by_probe,
    })
}

/// 交互操作只接受用户已经启动的游戏，独立启动流程可显式允许启动。
#[cfg(target_os = "windows")]
fn game_launch_required(
    initial_pid: Option<u32>,
    allow_game_launch: bool,
) -> Result<bool, PortableProbeError> {
    if initial_pid.is_some() {
        return Ok(false);
    }
    if allow_game_launch {
        return Ok(true);
    }
    Err(PortableProbeError::Target {
        stage: "target.require_running_game",
        message: "游戏尚未启动，请启动碧蓝航线并进入服务器后再同步".to_owned(),
    })
}

/// 构造只面向已验证 ADB serial 和固定目标包的 Android 启动命令。
#[cfg(any(target_os = "windows", test))]
pub(super) fn adb_game_launch_arguments(target_package: &str) -> Vec<String> {
    vec![
        "shell".to_owned(),
        "monkey".to_owned(),
        "-p".to_owned(),
        target_package.to_owned(),
        "-c".to_owned(),
        "android.intent.category.LAUNCHER".to_owned(),
        "1".to_owned(),
    ]
}

/// 使用提供方声明的启动命令顺序启动所选实例。
#[cfg(target_os = "windows")]
pub(super) fn launch_manager_instance(
    manager: &Path,
    instance_index: &str,
) -> Result<(), PortableProbeError> {
    let mut failures = Vec::new();
    for arguments in adapter_for_manager(manager)?.launch_arguments(instance_index) {
        match manager_checked(manager, &arguments, "instance.launch") {
            Ok(_) => return Ok(()),
            Err(error) => failures.push(error.to_string()),
        }
    }
    Err(PortableProbeError::ManagerCommand {
        stage: "instance.launch",
        message: failures.join("; "),
    })
}

/// pidof 的空非零状态表示尚未启动，其他输出必须是唯一正整数。
#[cfg(target_os = "windows")]
fn read_target_pid(
    server: &OwnedAdbServer,
    target_package: &str,
) -> Result<Option<u32>, PortableProbeError> {
    let output: AdbCommandStatus = server
        .run_target_status(
            &[
                "shell".to_owned(),
                "pidof".to_owned(),
                target_package.to_owned(),
            ],
            "target.read_pid",
        )
        .map_err(|source| adb_failure("adb.target", source))?;
    if output.exit_code == 0 {
        return parse_single_pid(&output.stdout).map(Some);
    }
    if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
        return Ok(None);
    }
    Err(PortableProbeError::Target {
        stage: "target.read_pid",
        message: format!(
            "pidof 返回 {}，stdout={:?}，stderr={:?}",
            output.exit_code, output.stdout, output.stderr
        ),
    })
}

/// 有界等待管理器启动的游戏形成唯一 PID。
#[cfg(target_os = "windows")]
fn wait_for_target_pid(
    server: &OwnedAdbServer,
    target_package: &str,
    timeout: Duration,
) -> Result<u32, PortableProbeError> {
    let deadline: Instant = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(pid) = read_target_pid(server, target_package)? {
            return Ok(pid);
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(PortableProbeError::Target {
        stage: "target.wait_pid",
        message: format!(
            "启动 {target_package} 后 {} 秒内没有唯一 PID",
            timeout.as_secs()
        ),
    })
}

/// PID 输出必须只包含一个非零十进制整数。
#[cfg(target_os = "windows")]
fn parse_single_pid(output: &str) -> Result<u32, PortableProbeError> {
    let values: Vec<&str> = output.split_whitespace().collect();
    if values.len() != 1 {
        return Err(PortableProbeError::Target {
            stage: "target.parse_pid",
            message: format!("期望唯一 PID，实际为 {output:?}"),
        });
    }
    let pid: u32 = values[0].parse().map_err(|_| PortableProbeError::Target {
        stage: "target.parse_pid",
        message: format!("PID 不是正整数: {output:?}"),
    })?;
    if pid == 0 {
        return Err(PortableProbeError::Target {
            stage: "target.parse_pid",
            message: "PID 不得为 0".to_owned(),
        });
    }
    Ok(pid)
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn interactive_sync_never_launches_an_absent_game() {
        let error = game_launch_required(None, false).unwrap_err();
        assert!(matches!(
            error,
            PortableProbeError::Target {
                stage: "target.require_running_game",
                ..
            }
        ));
        assert!(!game_launch_required(Some(4770), false).unwrap());
        assert!(game_launch_required(None, true).unwrap());
        assert!(!game_launch_required(Some(4770), true).unwrap());
    }
}

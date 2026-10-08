//! 绑定单个模拟器实例，并统一执行管理器、ADB 与 root shell 命令。

use std::path::Path;
#[cfg(not(target_os = "windows"))]
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use suzushiro_emulator::{RootTransport, adapter_for_manager};

use super::process_evidence::{
    ModuleReadiness, ProcessEvidence, ProcessIdentity, filter_azlw_lines, module_map_probe_command,
    non_empty_lines, parse_proc_stat_start_time, parse_sha256_first, parse_single_pid,
    parse_status_u32, thread_name_probe_command,
};
#[cfg(target_os = "windows")]
use super::process_evidence::{PostUnloadMappingEvidence, validate_post_unload_mappings};
use super::{RuntimeProbeError, RuntimeProbeOptions};
use suzushiro_adb::{adb_client_arguments, run_adb_client};
use suzushiro_emulator::RemoteShellOutput;
use suzushiro_host_command::NativeCommandPolicy;

const TARGET_MODULE_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(super) const PROCESS_PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

fn probe_command_timeout(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining.min(PROCESS_PROBE_COMMAND_TIMEOUT))
    }
}

#[derive(Clone)]
/// 封装绑定到单个模拟器实例和 ADB 序列号的设备命令入口。
pub(super) struct DeviceBridge {
    manager_executable: String,
    adb_executable: String,
    #[cfg(target_os = "windows")]
    adb_server_port: Option<u16>,
    #[cfg(target_os = "windows")]
    adb_process_policy: Option<NativeCommandPolicy>,
    vm_index: String,
    serial: String,
}

pub(super) struct TargetIdentityVerification {
    pub(super) manager_info: &'static str,
    pub(super) cross_channel_boot_id_matched: bool,
}

impl DeviceBridge {
    /// 从已校验选项复制命令路径和唯一实例身份。
    pub(super) fn new(options: &RuntimeProbeOptions) -> Self {
        Self {
            manager_executable: options.manager_executable.clone(),
            adb_executable: options.adb_executable.clone(),
            #[cfg(target_os = "windows")]
            adb_server_port: options.adb_server_port,
            #[cfg(target_os = "windows")]
            adb_process_policy: options.adb_process_policy.clone(),
            vm_index: options.vm_index.clone(),
            serial: options.serial.clone(),
        }
    }

    /// 联合核对管理器实例、root 身份和 ADB 在线状态。
    pub(super) fn verify_target(&self) -> Result<TargetIdentityVerification, RuntimeProbeError> {
        self.verify_manager_instance()?;
        let identity: String = self.root_checked("id", "target.root_identity")?;
        if !identity.contains("uid=0(root)") {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "target.root_identity",
                message: format!("目标实例的 root shell 不是 root: {identity}"),
            });
        }
        let state: String = self.adb_checked(&["get-state".to_owned()], "target.adb_state")?;
        if state.trim() != "device" {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "target.adb_state",
                message: format!("ADB 状态不是 device: {state:?}"),
            });
        }
        #[cfg(target_os = "windows")]
        let cross_channel_boot_id_matched =
            if self.adapter("target.identity")?.root_transport() == RootTransport::Manager {
                self.verify_cross_channel_boot_identity()?;
                true
            } else {
                false
            };
        #[cfg(not(target_os = "windows"))]
        let cross_channel_boot_id_matched = false;
        Ok(TargetIdentityVerification {
            manager_info: "verified",
            cross_channel_boot_id_matched,
        })
    }

    /// 通过提供方重新查询实例，确认当前端点仍属于绑定目标。
    pub(super) fn verify_manager_instance(&self) -> Result<(), RuntimeProbeError> {
        #[cfg(target_os = "windows")]
        {
            let adapter = self.adapter("target.manager_info")?;
            let instances = adapter
                .instances(Path::new(&self.manager_executable))
                .map_err(|error| RuntimeProbeError::InvalidOutput {
                    stage: "target.manager_info",
                    message: error.to_string(),
                })?;
            if !instances.get(&self.vm_index).is_some_and(|instance| {
                instance.is_ready() && instance.serial().is_ok_and(|serial| serial == self.serial)
            }) {
                return Err(RuntimeProbeError::InvalidOutput {
                    stage: "target.manager_info",
                    message: "管理器实例或已归属 ADB 端点发生变化".to_owned(),
                });
            }
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(RuntimeProbeError::InvalidOption {
                field: "platform",
                message: "模拟器适配器需要原生 Windows 环境".to_owned(),
            })
        }
    }

    /// 读取 Manager root 与精确 ADB serial 所见的内核启动标识，证明两条通道属于同一实例。
    pub(super) fn verify_cross_channel_boot_identity(&self) -> Result<(), RuntimeProbeError> {
        const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
        let manager_boot_id = parse_boot_id(
            &self.root_checked(&format!("cat {BOOT_ID_PATH}"), "target.manager_boot_id")?,
            "target.manager_boot_id",
        )?;
        let adb_boot_id = parse_boot_id(
            &self.adb_checked(
                &[
                    "shell".to_owned(),
                    "cat".to_owned(),
                    BOOT_ID_PATH.to_owned(),
                ],
                "target.adb_boot_id",
            )?,
            "target.adb_boot_id",
        )?;
        if manager_boot_id != adb_boot_id {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "target.cross_channel_identity",
                message: "管理器 root 与 ADB serial 对应不同 Android 启动实例".to_owned(),
            });
        }
        Ok(())
    }

    /// 要求目标包名只对应一个正整数 PID。
    pub(super) fn discover_single_pid(
        &self,
        package_name: &str,
        stage: &'static str,
    ) -> Result<u32, RuntimeProbeError> {
        let output: String = self.root_checked(&format!("pidof {package_name}"), stage)?;
        parse_single_pid(&output, stage)
    }

    /// 记录当前模块文件摘要，不与构建时的参考模块比较。
    pub(super) fn read_module_sha256(
        &self,
        process_id: u32,
        module_name: &str,
    ) -> Result<String, RuntimeProbeError> {
        let command = format!(
            "module=$(awk -v separator=/ -v name='{module_name}' '$3 ~ /^0+$/ {{ n=split($6,p,separator); if(p[n]==name) {{ print $6; exit }} }}' /proc/{process_id}/maps) || exit 1; \
             test -n \"$module\" || exit 1; sha256sum \"$module\""
        );
        let output = self.root_checked(&command, "target.module_hash")?;
        parse_sha256_first(&output, "target.module_hash")
    }

    /// 等待 profile 指定模块进入唯一目标进程，并容忍首次启动期的短暂 PID 变化。
    pub(super) fn wait_for_loaded_module(
        &self,
        package_name: &str,
        module_name: &str,
        initial_process_id: u32,
        timeout: Duration,
    ) -> Result<ModuleReadiness, RuntimeProbeError> {
        let deadline: Instant = Instant::now() + timeout;
        let mut retries: u32 = 0;
        let mut last_state: String =
            format!("初始 PID {initial_process_id} 尚未加载 {module_name}");
        loop {
            let pid_output: RemoteShellOutput =
                self.root_status(&format!("pidof {package_name}"), "target.wait_module_pid")?;
            let process_id: Option<u32> = match pid_output.exit_code {
                0 => match parse_single_pid(&pid_output.stdout, "target.wait_module_pid") {
                    Ok(process_id) => Some(process_id),
                    Err(_) => {
                        last_state = format!("PID 输出暂时不唯一: {:?}", pid_output.stdout);
                        None
                    }
                },
                1 if pid_output.stdout.is_empty() => {
                    last_state = "目标进程在启动期暂时不存在".to_owned();
                    None
                }
                exit_code => {
                    return Err(RuntimeProbeError::DeviceCommand {
                        stage: "target.wait_module_pid",
                        exit_code,
                        output: pid_output.stdout,
                    });
                }
            };

            if let Some(process_id) = process_id {
                let module_output: RemoteShellOutput = self.root_status(
                    &module_map_probe_command(process_id, module_name),
                    "target.wait_module_map",
                )?;
                match module_output.exit_code {
                    0 => {
                        return Ok(ModuleReadiness {
                            process_id,
                            retries,
                        });
                    }
                    1 => {
                        last_state = format!("PID {process_id} 尚未加载 {module_name}");
                    }
                    2 => {
                        let process_output: RemoteShellOutput = self.root_status(
                            &format!("test -d /proc/{process_id}"),
                            "target.wait_module_process",
                        )?;
                        match process_output.exit_code {
                            1 => {
                                last_state = format!("PID {process_id} 在读取 maps 前已经退出");
                            }
                            0 => {
                                return Err(RuntimeProbeError::DeviceCommand {
                                    stage: "target.wait_module_map",
                                    exit_code: module_output.exit_code,
                                    output: module_output.stdout,
                                });
                            }
                            exit_code => {
                                return Err(RuntimeProbeError::DeviceCommand {
                                    stage: "target.wait_module_process",
                                    exit_code,
                                    output: process_output.stdout,
                                });
                            }
                        }
                    }
                    exit_code => {
                        return Err(RuntimeProbeError::DeviceCommand {
                            stage: "target.wait_module_map",
                            exit_code,
                            output: module_output.stdout,
                        });
                    }
                }
            }

            if Instant::now() >= deadline {
                return Err(RuntimeProbeError::InvalidOutput {
                    stage: "target.wait_module",
                    message: format!(
                        "{} 秒内目标模块 {module_name} 未就绪，最后状态: {last_state}",
                        timeout.as_secs()
                    ),
                });
            }
            retries += 1;
            thread::sleep(TARGET_MODULE_POLL_INTERVAL);
        }
    }

    /// 等待游戏重启为唯一进程，并用前后两次包查询绑定 PID 与 proc 启动时刻。
    pub(super) fn wait_for_single_process_identity(
        &self,
        package_name: &str,
        timeout: Duration,
    ) -> Result<ProcessIdentity, RuntimeProbeError> {
        let deadline: Instant = Instant::now() + timeout;
        let mut last_output: String = String::new();
        while Instant::now() < deadline {
            let Some(command_timeout) = probe_command_timeout(deadline) else {
                break;
            };
            let result: RemoteShellOutput = self.root_status_with_timeout(
                &format!("pidof {package_name}"),
                "cleanup.wait_game_pid",
                command_timeout,
            )?;
            last_output = result.stdout;
            if result.exit_code == 0
                && let Ok(pid) = parse_single_pid(&last_output, "cleanup.wait_game_pid")
            {
                let Some(command_timeout) = probe_command_timeout(deadline) else {
                    break;
                };
                let stat: RemoteShellOutput = self.root_status_with_timeout(
                    &format!("cat /proc/{pid}/stat"),
                    "cleanup.wait_game_identity",
                    command_timeout,
                )?;
                if stat.exit_code == 0 {
                    match parse_proc_stat_start_time(
                        &stat.stdout,
                        pid,
                        "cleanup.wait_game_identity",
                    ) {
                        Ok(process_start_time) => {
                            let Some(command_timeout) = probe_command_timeout(deadline) else {
                                break;
                            };
                            let confirmation: RemoteShellOutput = self.root_status_with_timeout(
                                &format!("pidof {package_name}"),
                                "cleanup.confirm_game_pid",
                                command_timeout,
                            )?;
                            last_output = confirmation.stdout;
                            let confirmed_pid: Option<u32> =
                                parse_single_pid(&last_output, "cleanup.confirm_game_pid").ok();
                            if confirmation.exit_code == 0 && confirmed_pid == Some(pid) {
                                return Ok(ProcessIdentity {
                                    process_id: pid,
                                    process_start_time,
                                });
                            }
                        }
                        Err(error) => last_output = error.to_string(),
                    }
                } else {
                    last_output = stat.stdout;
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
        Err(RuntimeProbeError::InvalidOutput {
            stage: "cleanup.wait_game_identity",
            message: format!("等待唯一游戏进程身份超时，最后输出 {last_output:?}"),
        })
    }

    /// 一次查询旧进程实例；PID 消失或启动时刻变化都证明原实例已经退出。
    pub(super) fn process_instance_absent(
        &self,
        process_id: u32,
        expected_process_start_time: Option<u64>,
        command_timeout: Duration,
    ) -> Result<bool, RuntimeProbeError> {
        let expected_process_start_time: u64 =
            expected_process_start_time.ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "cleanup.wait_old_process_identity",
                message: "缺少旧游戏进程启动时刻，不能安全判断原实例是否退出".to_owned(),
            })?;
        let result: RemoteShellOutput = self.root_status_with_timeout(
            &format!(
                "if test ! -e /proc/{process_id}/stat; then echo AZLW_PROCESS_ABSENT; \
                 exit 0; fi; cat /proc/{process_id}/stat || exit 75"
            ),
            "cleanup.wait_old_process_identity",
            command_timeout,
        )?;
        if result.exit_code == 0 && result.stdout.trim() == "AZLW_PROCESS_ABSENT" {
            return Ok(true);
        }
        if result.exit_code == 0 {
            let actual_process_start_time: u64 = parse_proc_stat_start_time(
                &result.stdout,
                process_id,
                "cleanup.wait_old_process_identity",
            )?;
            return Ok(actual_process_start_time != expected_process_start_time);
        }
        if result.exit_code == 75 {
            return Ok(false);
        }
        Err(RuntimeProbeError::DeviceCommand {
            stage: "cleanup.wait_old_process_identity",
            exit_code: result.exit_code,
            output: result.stdout,
        })
    }

    /// 轮询旧进程实例；PID 消失或启动时刻变化都证明原实例已经退出。
    pub(super) fn wait_for_process_instance_absent(
        &self,
        process_id: u32,
        expected_process_start_time: Option<u64>,
        timeout: Duration,
    ) -> Result<bool, RuntimeProbeError> {
        let deadline: Instant = Instant::now() + timeout;
        while Instant::now() < deadline {
            let Some(command_timeout) = probe_command_timeout(deadline) else {
                break;
            };
            if self.process_instance_absent(
                process_id,
                expected_process_start_time,
                command_timeout,
            )? {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(200));
        }
        Ok(false)
    }

    /// 复核目标进程与精确 memfd 身份，并记录原地址在稍后快照中的复用情况。
    #[cfg(target_os = "windows")]
    pub(super) fn verify_agent_unloaded(
        &self,
        process_id: u32,
        expected_process_start_time: u64,
        agent_start: u64,
        agent_size: u64,
        agent_mapping_name: &str,
    ) -> Result<PostUnloadMappingEvidence, RuntimeProbeError> {
        let stat: String = self.root_checked(
            &format!("cat /proc/{process_id}/stat"),
            "shutdown.verify_process_identity",
        )?;
        let actual_process_start_time: u64 =
            parse_proc_stat_start_time(&stat, process_id, "shutdown.verify_process_identity")?;
        if actual_process_start_time != expected_process_start_time {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_process_identity",
                message: format!(
                    "目标进程启动时刻发生变化: expected={expected_process_start_time}, actual={actual_process_start_time}"
                ),
            });
        }
        let maps: String = self.root_checked(
            &format!("cat /proc/{process_id}/maps"),
            "shutdown.verify_agent_maps",
        )?;
        validate_post_unload_mappings(&maps, agent_start, agent_size, agent_mapping_name)
    }

    /// 解析并约束属于目标包的唯一安全启动组件文本。
    pub(super) fn resolve_launch_component(
        &self,
        package_name: &str,
    ) -> Result<String, RuntimeProbeError> {
        let output: String = self.root_checked(
            &format!("cmd package resolve-activity --brief {package_name}"),
            "target.resolve_activity",
        )?;
        let component: String = output
            .lines()
            .map(str::trim)
            .find(|line: &&str| line.contains('/'))
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "target.resolve_activity",
                message: format!("未解析到启动组件: {output:?}"),
            })?
            .to_owned();
        if !component.starts_with(&format!("{package_name}/"))
            || !component.bytes().all(|byte: u8| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'$' | b'/')
            })
        {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "target.resolve_activity",
                message: format!("启动组件不属于目标包或包含歧义字符: {component}"),
            });
        }
        Ok(component)
    }

    /// 收集进程状态、映射摘要及带项目标识的可观测运行痕迹。
    pub(super) fn collect_process_evidence(
        &self,
        process_id: u32,
    ) -> Result<ProcessEvidence, RuntimeProbeError> {
        self.collect_process_evidence_with_timeout(process_id, None)
    }

    pub(super) fn collect_process_evidence_with_timeout(
        &self,
        process_id: u32,
        timeout: Option<Duration>,
    ) -> Result<ProcessEvidence, RuntimeProbeError> {
        let process_start_time: u64 = self.read_process_start_time_with_timeout(
            process_id,
            "evidence.process_identity_start",
            timeout,
        )?;
        let status: String = self.finish_root_checked(
            &format!("cat /proc/{process_id}/status"),
            "evidence.process_status",
            timeout,
        )?;
        let tracer_pid: u32 = parse_status_u32(&status, "TracerPid", "evidence.tracer_pid")?;
        let task_ids: String = self.finish_root_checked(
            &format!("ls -1 /proc/{process_id}/task"),
            "evidence.thread_count",
            timeout,
        )?;
        let thread_count: u32 = u32::try_from(non_empty_lines(&task_ids).len()).map_err(|_| {
            RuntimeProbeError::InvalidOutput {
                stage: "evidence.thread_count",
                message: "线程数量超过 u32".to_owned(),
            }
        })?;
        let maps_output: String = self.finish_root_checked(
            &format!("sha256sum /proc/{process_id}/maps"),
            "evidence.maps_hash",
            timeout,
        )?;
        let maps_sha256: String = parse_sha256_first(&maps_output, "evidence.maps_hash")?;
        let maps: String = self.finish_root_checked(
            &format!("cat /proc/{process_id}/maps"),
            "evidence.azlw_maps",
            timeout,
        )?;
        let thread_names: String = self.finish_root_checked(
            &thread_name_probe_command(process_id),
            "evidence.thread_names",
            timeout,
        )?;
        let sockets: String =
            self.finish_root_checked("cat /proc/net/unix", "evidence.sockets", timeout)?;
        let processes: String = self.finish_root_checked("ps -A", "evidence.processes", timeout)?;
        let final_process_start_time: u64 = self.read_process_start_time_with_timeout(
            process_id,
            "evidence.process_identity_end",
            timeout,
        )?;
        if final_process_start_time != process_start_time {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "evidence.process_identity",
                message: format!(
                    "采集进程证据期间 PID {process_id} 启动时刻发生变化: before={process_start_time}, after={final_process_start_time}"
                ),
            });
        }

        Ok(ProcessEvidence {
            process_id,
            process_start_time,
            tracer_pid,
            thread_count,
            maps_sha256,
            azlw_map_lines: filter_azlw_lines(&maps),
            azlw_thread_names: non_empty_lines(&thread_names)
                .into_iter()
                .filter(|line: &String| line.to_ascii_lowercase().contains("azlw"))
                .collect(),
            azlw_socket_lines: filter_azlw_lines(&sockets),
            azlw_process_lines: filter_azlw_lines(&processes),
        })
    }

    /// 读取 proc stat 的启动时刻字段，用 PID 与启动时刻共同绑定进程实例。
    pub(super) fn read_process_start_time(
        &self,
        process_id: u32,
        stage: &'static str,
    ) -> Result<u64, RuntimeProbeError> {
        self.read_process_start_time_with_timeout(process_id, stage, None)
    }

    fn read_process_start_time_with_timeout(
        &self,
        process_id: u32,
        stage: &'static str,
        timeout: Option<Duration>,
    ) -> Result<u64, RuntimeProbeError> {
        let stat: String =
            self.finish_root_checked(&format!("cat /proc/{process_id}/stat"), stage, timeout)?;
        parse_proc_stat_start_time(&stat, process_id, stage)
    }

    /// 将宿主路径转换为 Windows 原生路径后推送到指定设备路径。
    pub(super) fn adb_push(
        &self,
        source: &Path,
        destination: &str,
        stage: &'static str,
    ) -> Result<(), RuntimeProbeError> {
        let source_text: String = native_host_path(source)?;
        self.adb_checked(
            &["push".to_owned(), source_text, destination.to_owned()],
            stage,
        )?;
        Ok(())
    }

    /// 返回排序后的全局端口转发快照，便于清理后做精确比较。
    pub(super) fn forward_list(&self) -> Result<Vec<String>, RuntimeProbeError> {
        self.forward_list_with_timeout(None)
    }

    pub(super) fn forward_list_with_timeout(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Vec<String>, RuntimeProbeError> {
        let arguments = adb_client_arguments(
            self.adb_server_port_argument(),
            None,
            &["forward".to_owned(), "--list".to_owned()],
        );
        let output: String = self.finish_adb_client(&arguments, "forward.list", timeout)?;
        let mut lines: Vec<String> = non_empty_lines(&output);
        lines.sort();
        Ok(lines)
    }

    /// 自动附加唯一序列号执行 ADB 命令，并要求成功退出。
    pub(super) fn adb_checked(
        &self,
        arguments: &[String],
        stage: &'static str,
    ) -> Result<String, RuntimeProbeError> {
        self.adb_checked_with_timeout(arguments, stage, None)
    }

    pub(super) fn adb_checked_with_timeout(
        &self,
        arguments: &[String],
        stage: &'static str,
        timeout: Option<Duration>,
    ) -> Result<String, RuntimeProbeError> {
        let complete_arguments = adb_client_arguments(
            self.adb_server_port_argument(),
            Some(&self.serial),
            arguments,
        );
        self.finish_adb_client(&complete_arguments, stage, timeout)
    }

    fn finish_adb_client(
        &self,
        arguments: &[String],
        stage: &'static str,
        timeout: Option<Duration>,
    ) -> Result<String, RuntimeProbeError> {
        let output = run_adb_client(
            &self.adb_executable,
            self.adb_process_policy(),
            arguments,
            stage,
            timeout,
        )?;
        if output.exit_code != 0 {
            return Err(RuntimeProbeError::HostCommand {
                stage,
                message: format!(
                    "ADB 返回 {}，stdout={:?}，stderr={:?}",
                    output.exit_code, output.stdout, output.stderr
                ),
            });
        }
        Ok(output.stdout.trim().to_owned())
    }

    fn adb_server_port_argument(&self) -> Option<u16> {
        #[cfg(target_os = "windows")]
        {
            self.adb_server_port
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    fn adb_process_policy(&self) -> Option<&NativeCommandPolicy> {
        #[cfg(target_os = "windows")]
        {
            self.adb_process_policy.as_ref()
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    /// 让所有 ADB 客户端路径共享同一独立服务端口参数，避免回落到系统 5037。
    #[cfg(test)]
    pub(super) fn adb_complete_arguments(&self, arguments: &[String]) -> Vec<String> {
        adb_client_arguments(self.adb_server_port_argument(), None, arguments)
    }

    /// 执行 root shell 命令，并把非零状态转换为带阶段信息的错误。
    pub(super) fn root_checked(
        &self,
        command: &str,
        stage: &'static str,
    ) -> Result<String, RuntimeProbeError> {
        self.finish_root_checked(command, stage, None)
    }

    pub(super) fn root_checked_timed(
        &self,
        command: &str,
        stage: &'static str,
        timeout: Duration,
    ) -> Result<String, RuntimeProbeError> {
        self.finish_root_checked(command, stage, Some(timeout))
    }

    fn finish_root_checked(
        &self,
        command: &str,
        stage: &'static str,
        timeout: Option<Duration>,
    ) -> Result<String, RuntimeProbeError> {
        let output: RemoteShellOutput = match timeout {
            Some(timeout) => self.root_status_with_timeout(command, stage, timeout)?,
            None => self.root_status(command, stage)?,
        };
        if output.exit_code != 0 {
            return Err(RuntimeProbeError::DeviceCommand {
                stage,
                exit_code: output.exit_code,
                output: output.stdout,
            });
        }
        Ok(output.stdout)
    }

    #[cfg(target_os = "windows")]
    fn adapter(
        &self,
        stage: &'static str,
    ) -> Result<&'static dyn suzushiro_emulator::EmulatorAdapter, RuntimeProbeError> {
        adapter_for_manager(Path::new(&self.manager_executable)).map_err(|error| {
            RuntimeProbeError::InvalidOutput {
                stage,
                message: error.to_string(),
            }
        })
    }

    #[cfg(target_os = "windows")]
    fn adapter_root_status(
        &self,
        command: &str,
        stage: &'static str,
        timeout: Option<Duration>,
    ) -> Result<RemoteShellOutput, RuntimeProbeError> {
        let adapter = self.adapter(stage)?;
        suzushiro_emulator::transport::RootShell {
            adapter,
            manager: &self.manager_executable,
            index: &self.vm_index,
        }
        .run(
            command,
            stage,
            timeout,
            |arguments, stage, timeout| self.adb_checked_with_timeout(arguments, stage, timeout),
            RuntimeProbeError::from,
        )
    }

    /// 执行 root shell 命令并保留退出码及合并后的标准输出与错误输出。
    pub(super) fn root_status(
        &self,
        command: &str,
        stage: &'static str,
    ) -> Result<RemoteShellOutput, RuntimeProbeError> {
        #[cfg(target_os = "windows")]
        {
            self.adapter_root_status(command, stage, None)
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(RuntimeProbeError::InvalidOption {
                field: "platform",
                message: "模拟器适配器需要原生 Windows 环境".to_owned(),
            })
        }
    }

    pub(super) fn root_status_with_timeout(
        &self,
        command: &str,
        stage: &'static str,
        timeout: Duration,
    ) -> Result<RemoteShellOutput, RuntimeProbeError> {
        #[cfg(target_os = "windows")]
        {
            self.adapter_root_status(command, stage, Some(timeout))
        }
        #[cfg(not(target_os = "windows"))]
        {
            Err(RuntimeProbeError::InvalidOption {
                field: "platform",
                message: "模拟器适配器需要原生 Windows 环境".to_owned(),
            })
        }
    }
}

pub(super) fn parse_boot_id(
    output: &str,
    stage: &'static str,
) -> Result<String, RuntimeProbeError> {
    let value = output.trim();
    let valid = value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !valid {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: "Android boot_id 不是规范 UUID".to_owned(),
        });
    }
    Ok(value.to_ascii_lowercase())
}

/// 将当前平台路径转换为 Windows 原生程序可接收的 Unicode 路径。
#[cfg(target_os = "windows")]
fn native_host_path(path: &Path) -> Result<String, RuntimeProbeError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| RuntimeProbeError::InvalidToolAsset {
            path: path.to_path_buf(),
            message: "Windows 原生路径不是有效 Unicode".to_owned(),
        })
}

/// 在 WSL 中通过 wslpath 转换为 Windows 原生 Unicode 路径。
#[cfg(not(target_os = "windows"))]
fn native_host_path(path: &Path) -> Result<String, RuntimeProbeError> {
    let output: Output = Command::new("wslpath")
        .args(["-w", "--", path.to_str().unwrap_or_default()])
        .output()
        .map_err(|source: std::io::Error| RuntimeProbeError::HostCommand {
            stage: "host.translate_path",
            message: format!("启动 wslpath 失败: {source}"),
        })?;
    if !output.status.success() {
        return Err(RuntimeProbeError::HostCommand {
            stage: "host.translate_path",
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout)
        .map(|value: String| value.trim().to_owned())
        .map_err(
            |source: std::string::FromUtf8Error| RuntimeProbeError::HostCommand {
                stage: "host.translate_path",
                message: format!("wslpath 输出不是 UTF-8: {source}"),
            },
        )
}

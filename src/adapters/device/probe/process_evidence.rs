//! 解析并验证目标进程身份、映射和残留检测证据。

use serde::{Deserialize, Serialize};
use suzushiro_text_format::is_canonical_sha256;

use super::RuntimeProbeError;

/// 宿主在原生卸载证明完成后，对稍后映射快照记录的地址占用情况。
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PostUnloadMappingEvidence {
    pub(super) inert_anonymous_overlap_count: usize,
    pub(super) reused_address_overlap_count: usize,
}

/// 加载前、加载后或清理后的进程级检测面。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessEvidence {
    pub process_id: u32,
    pub process_start_time: u64,
    pub tracer_pid: u32,
    pub thread_count: u32,
    pub maps_sha256: String,
    pub azlw_map_lines: Vec<String>,
    pub azlw_thread_names: Vec<String>,
    pub azlw_socket_lines: Vec<String>,
    pub azlw_process_lines: Vec<String>,
}

/// 保存游戏首次启动后目标运行库进入进程映射的有界等待结果。
pub(super) struct ModuleReadiness {
    pub(super) process_id: u32,
    pub(super) retries: u32,
}

/// 用 PID 与 proc 启动时刻绑定一次不会被 PID 复用混淆的进程实例。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessIdentity {
    pub(super) process_id: u32,
    pub(super) process_start_time: u64,
}

/// 从命令输出解析唯一非零 PID，拒绝多进程和附加文本。
pub(super) fn parse_single_pid(
    output: &str,
    stage: &'static str,
) -> Result<u32, RuntimeProbeError> {
    let values: Vec<&str> = output.split_whitespace().collect();
    if values.len() != 1 {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("期望唯一 PID，实际输出 {output:?}"),
        });
    }
    let process_id: u32 = values[0]
        .parse()
        .map_err(|_| RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("PID 不是正整数: {output:?}"),
        })?;
    if process_id == 0 {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: "PID 不得为 0".to_owned(),
        });
    }
    Ok(process_id)
}

/// 读取命令输出首列的小写 SHA-256，并拒绝长度或字符不规范的摘要。
pub(super) fn parse_sha256_first(
    output: &str,
    stage: &'static str,
) -> Result<String, RuntimeProbeError> {
    let digest: &str = output.split_whitespace().next().unwrap_or_default();
    if !is_canonical_sha256(digest) {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("SHA-256 输出无效: {output:?}"),
        });
    }
    Ok(digest.to_owned())
}

/// 规范化多行命令输出，移除空行并保留原有顺序。
pub(super) fn non_empty_lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line: &&str| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 从命令输出中筛出包含项目运行标识的行。
pub(super) fn filter_azlw_lines(value: &str) -> Vec<String> {
    non_empty_lines(value)
        .into_iter()
        .filter(|line: &String| line.to_ascii_lowercase().contains("azlw"))
        .collect()
}

/// 从 proc 状态文本中读取指定的非负整数属性。
pub(super) fn parse_status_u32(
    status: &str,
    field: &str,
    stage: &'static str,
) -> Result<u32, RuntimeProbeError> {
    let value: &str = status
        .lines()
        .find_map(|line: &str| {
            let (name, rest): (&str, &str) = line.split_once(':')?;
            (name.trim() == field).then_some(rest.trim())
        })
        .ok_or_else(|| RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("/proc/status 缺少 {field}"),
        })?;
    value.parse().map_err(|_| RuntimeProbeError::InvalidOutput {
        stage,
        message: format!("{field} 不是非负整数: {value:?}"),
    })
}

/// 按 Linux `/proc/<pid>/stat` 语法读取第 22 字段，兼容线程名中的空格和右括号。
pub(super) fn parse_proc_stat_start_time(
    stat: &str,
    expected_id: u32,
    stage: &'static str,
) -> Result<u64, RuntimeProbeError> {
    let stat: &str = stat.trim();
    let (id_text, remainder): (&str, &str) =
        stat.split_once(" (")
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage,
                message: format!("/proc/stat 缺少线程标识或名称边界: {stat:?}"),
            })?;
    let actual_id: u32 = id_text
        .parse()
        .map_err(|_| RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("/proc/stat 线程标识不是正整数: {id_text:?}"),
        })?;
    if actual_id != expected_id {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("/proc/stat 标识不匹配: expected={expected_id}, actual={actual_id}"),
        });
    }
    let close_index: usize =
        remainder
            .rfind(") ")
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage,
                message: format!("/proc/stat 缺少线程名称结束边界: {stat:?}"),
            })?;
    let fields_after_name: &str = &remainder[close_index + 2..];
    let start_time: &str = fields_after_name
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("/proc/stat 缺少 starttime 字段: {stat:?}"),
        })?;
    let start_time: u64 = start_time
        .parse()
        .map_err(|_| RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("/proc/stat starttime 不是无符号整数: {start_time:?}"),
        })?;
    if start_time == 0 {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: "/proc/stat starttime 不得为 0".to_owned(),
        });
    }
    Ok(start_time)
}

/// 原生 unloader 已紧邻 dlclose 严格拒绝可访问残留；稍后的宿主快照只拒绝精确
/// memfd 身份，并区分不可访问占位与已被游戏重新分配的旧地址。
#[cfg(any(target_os = "windows", test))]
pub(super) fn validate_post_unload_mappings(
    maps: &str,
    agent_start: u64,
    agent_size: u64,
    agent_mapping_name: &str,
) -> Result<PostUnloadMappingEvidence, RuntimeProbeError> {
    let agent_end: u64 =
        agent_start
            .checked_add(agent_size)
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: "Agent 地址范围溢出".to_owned(),
            })?;
    if agent_start == 0 || agent_size == 0 {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "shutdown.verify_agent_maps",
            message: "Agent 地址范围不得为空".to_owned(),
        });
    }
    if maps.trim().is_empty() {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "shutdown.verify_agent_maps",
            message: "Agent 卸载后 maps 不得为空".to_owned(),
        });
    }

    let mut inert_anonymous_overlap_count: usize = 0;
    let mut reused_address_overlap_count: usize = 0;
    for (index, line) in maps.lines().enumerate() {
        let mut fields = line.split_ascii_whitespace();
        let range: &str = fields
            .next()
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("maps 第 {} 行缺少地址范围", index + 1),
            })?;
        let mut require_field = |field_name: &str| -> Result<&str, RuntimeProbeError> {
            fields
                .next()
                .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                    stage: "shutdown.verify_agent_maps",
                    message: format!("maps 第 {} 行缺少 {field_name}", index + 1),
                })
        };
        let permissions: &str = require_field("permissions")?;
        let offset: &str = require_field("offset")?;
        let device: &str = require_field("device")?;
        let inode: &str = require_field("inode")?;
        let pathname: String = fields.collect::<Vec<&str>>().join(" ");
        let (start, end): (&str, &str) =
            range
                .split_once('-')
                .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                    stage: "shutdown.verify_agent_maps",
                    message: format!("maps 第 {} 行地址范围无效: {range:?}", index + 1),
                })?;
        let start: u64 =
            u64::from_str_radix(start, 16).map_err(|_| RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("maps 第 {} 行起始地址无效: {range:?}", index + 1),
            })?;
        let end: u64 =
            u64::from_str_radix(end, 16).map_err(|_| RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("maps 第 {} 行结束地址无效: {range:?}", index + 1),
            })?;
        if start >= end {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("maps 第 {} 行地址范围为空: {range:?}", index + 1),
            });
        }
        let normalized_pathname: &str = pathname
            .strip_suffix(" (deleted)")
            .unwrap_or(pathname.as_str());
        let normalized_agent_mapping_name: &str = agent_mapping_name
            .strip_suffix(" (deleted)")
            .unwrap_or(agent_mapping_name);
        let exact_agent_mapping = normalized_pathname == normalized_agent_mapping_name;
        if exact_agent_mapping {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("Agent 卸载后仍存在映射: {line}"),
            });
        }
        if start >= agent_end || end <= agent_start {
            continue;
        }
        let offset: u64 =
            u64::from_str_radix(offset, 16).map_err(|_| RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("maps 第 {} 行偏移无效: {offset:?}", index + 1),
            })?;
        let inode: u64 = inode
            .parse()
            .map_err(|_| RuntimeProbeError::InvalidOutput {
                stage: "shutdown.verify_agent_maps",
                message: format!("maps 第 {} 行 inode 无效: {inode:?}", index + 1),
            })?;
        let inert_anonymous = permissions == "---p"
            && offset == 0
            && device == "00:00"
            && inode == 0
            && pathname.is_empty();
        if inert_anonymous {
            inert_anonymous_overlap_count += 1;
        } else {
            reused_address_overlap_count += 1;
        }
    }
    Ok(PostUnloadMappingEvidence {
        inert_anonymous_overlap_count,
        reused_address_overlap_count,
    })
}

/// 构造只匹配 profile 模块文件名的进程映射探针命令。
pub(super) fn module_map_probe_command(process_id: u32, module_name: &str) -> String {
    format!("grep -F -q /{module_name} /proc/{process_id}/maps")
}

/// 逐个读取线程名；线程在枚举后退出属于正常竞态，进程消失或其他错误仍须上报。
pub(super) fn thread_name_probe_command(process_id: u32) -> String {
    format!(
        "test -d /proc/{process_id}/task || exit 1; \
         for azlw_task in /proc/{process_id}/task/*/comm; do \
         cat \"$azlw_task\" 2>/dev/null || test ! -e \"$azlw_task\" || exit 1; \
         done; test -d /proc/{process_id}/task"
    )
}

/// 构造仅保留已知 PID 的空证据，用于清理失败后的完整错误报告。
pub(super) fn empty_process_evidence(process_id: u32) -> ProcessEvidence {
    ProcessEvidence {
        process_id,
        process_start_time: 0,
        tracer_pid: 0,
        thread_count: 0,
        maps_sha256: String::new(),
        azlw_map_lines: Vec::new(),
        azlw_thread_names: Vec::new(),
        azlw_socket_lines: Vec::new(),
        azlw_process_lines: Vec::new(),
    }
}

/// 要求清理后的进程证据仍绑定加载前的 PID 与 proc 启动时刻。
pub(super) fn validate_preserved_process_identity(
    evidence: &ProcessEvidence,
    expected_process_id: u32,
    expected_process_start_time: Option<u64>,
    stage: &'static str,
) -> Result<(), RuntimeProbeError> {
    let Some(expected_start_time) = expected_process_start_time else {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: "缺少原游戏进程启动时刻，不能证明进程实例保持不变".to_owned(),
        });
    };
    if evidence.process_id != expected_process_id
        || evidence.process_start_time != expected_start_time
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: format!(
                "游戏进程实例发生变化: expected_pid={expected_process_id}, actual_pid={}, expected_start_time={expected_start_time}, actual_start_time={}",
                evidence.process_id, evidence.process_start_time
            ),
        });
    }
    Ok(())
}

/// 确认加载前不存在跟踪器或本项目遗留的映射、线程、套接字和进程。
pub(super) fn validate_clean_baseline(evidence: &ProcessEvidence) -> Result<(), RuntimeProbeError> {
    if evidence.tracer_pid != 0
        || !evidence.azlw_map_lines.is_empty()
        || !evidence.azlw_thread_names.is_empty()
        || !evidence.azlw_socket_lines.is_empty()
        || !evidence.azlw_process_lines.is_empty()
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "target.clean_baseline",
            message: format!(
                "加载前检测面不干净: TracerPid={}, maps={}, threads={}, sockets={}, processes={}",
                evidence.tracer_pid,
                evidence.azlw_map_lines.len(),
                evidence.azlw_thread_names.len(),
                evidence.azlw_socket_lines.len(),
                evidence.azlw_process_lines.len()
            ),
        });
    }
    Ok(())
}

/// 优雅卸载后仍必须保留原游戏进程，同时清除跟踪器和全部项目运行痕迹。
pub(super) fn validate_graceful_unload_evidence(
    evidence: &ProcessEvidence,
) -> Result<(), RuntimeProbeError> {
    if evidence.process_id == 0
        || evidence.process_start_time == 0
        || evidence.tracer_pid != 0
        || !evidence.azlw_map_lines.is_empty()
        || !evidence.azlw_thread_names.is_empty()
        || !evidence.azlw_socket_lines.is_empty()
        || !evidence.azlw_process_lines.is_empty()
    {
        return Err(RuntimeProbeError::InvalidOutput {
            stage: "shutdown.verify_process_evidence",
            message: format!(
                "优雅卸载后进程证据不干净: pid={}, start_time={}, tracer={}, maps={}, threads={}, sockets={}, processes={}",
                evidence.process_id,
                evidence.process_start_time,
                evidence.tracer_pid,
                evidence.azlw_map_lines.len(),
                evidence.azlw_thread_names.len(),
                evidence.azlw_socket_lines.len(),
                evidence.azlw_process_lines.len()
            ),
        });
    }
    Ok(())
}

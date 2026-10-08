//! 解码并严格验证 loader 与 unloader 的唯一结构化收据。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::runtime::RuntimeAddress;
use super::RuntimeProbeError;
use crate::adapters::settings::{AgentMappingMode, AgentVisibilityMode};

pub(super) const MAXIMUM_FAILURE_RECEIPT_MESSAGE_BYTES: usize = 4 * 1024;
const ELF_HEADER_SIZE: usize = 64;

/// loader 唯一允许输出的结构化结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoaderReceipt {
    pub status: String,
    pub code: String,
    pub message: String,
    pub process_id: u32,
    pub process_start_time: u64,
    pub agent_handle: RuntimeAddress,
    pub agent_base: RuntimeAddress,
    pub agent_load_size: u64,
    pub finalize_address: RuntimeAddress,
    pub agent_mapping_name: String,
    pub agent_mapping_mode: AgentMappingMode,
    pub agent_visibility_mode: AgentVisibilityMode,
    pub agent_soinfo_address: Option<RuntimeAddress>,
    pub agent_protected_elf_header: Option<String>,
    pub anonymous_segment_count: usize,
    pub anonymous_byte_count: u64,
}

/// loader 失败后对目标进程现场能够作出的严格结论。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LoaderTargetState {
    Unchanged,
    Restored,
    Unknown,
}

impl LoaderTargetState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Restored => "restored",
            Self::Unknown => "unknown",
        }
    }
}

/// loader 非零退出时唯一允许返回的严格失败收据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LoaderFailureReceipt {
    pub(super) status: String,
    pub(super) code: String,
    pub(super) message: String,
    pub(super) process_id: u32,
    pub(super) target_state: LoaderTargetState,
}

impl LoaderFailureReceipt {
    /// 失败收据必须绑定本次 PID，且错误码、退出码和目标状态相互一致。
    fn validate(
        &self,
        expected_process_id: u32,
        exit_code: i32,
    ) -> Result<LoaderTargetState, RuntimeProbeError> {
        if self.status != "error"
            || self.code.is_empty()
            || self.message.trim().is_empty()
            || self.message.len() > MAXIMUM_FAILURE_RECEIPT_MESSAGE_BYTES
            || self.process_id != expected_process_id
        {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.failure_receipt",
                message: format!("loader 失败收据未绑定本次目标: {self:?}"),
            });
        }
        let state_matches_code: bool = matches!(
            (exit_code, self.code.as_str(), self.target_state),
            (
                10,
                "bootstrap_invalid" | "target_pid_mismatch",
                LoaderTargetState::Unchanged
            ) | (
                11,
                "identity_rejected" | "module_rejected" | "agent_abi_mismatch",
                LoaderTargetState::Unchanged,
            ) | (
                12,
                "ptrace_attach_failed",
                LoaderTargetState::Restored | LoaderTargetState::Unknown,
            ) | (
                13,
                "memory_init_failed",
                LoaderTargetState::Restored | LoaderTargetState::Unknown,
            ) | (13, "injector_init_failed", LoaderTargetState::Unknown)
                | (
                    14,
                    "agent_start_failed" | "agent_identity_incomplete",
                    LoaderTargetState::Unknown,
                )
                | (
                    15,
                    "session_file_cleanup_failed"
                        | "ptrace_detach_failed"
                        | "remote_scratch_cleanup_failed",
                    LoaderTargetState::Unknown,
                )
                | (11, "process_identity_changed", LoaderTargetState::Unknown)
        );
        if !state_matches_code {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.failure_receipt",
                message: format!(
                    "loader 失败收据的退出码、错误码与目标状态不一致: exit_code={exit_code}, receipt={self:?}"
                ),
            });
        }
        Ok(self.target_state)
    }
}

/// cleanup loader 唯一允许返回的严格卸载收据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UnloadReceipt {
    status: String,
    code: String,
    message: String,
    process_id: u32,
}

impl UnloadReceipt {
    /// 卸载成功必须继续绑定原目标 PID，并使用固定状态码。
    pub(super) fn validate_unloaded(
        &self,
        expected_process_id: u32,
    ) -> Result<(), RuntimeProbeError> {
        if self.status != "ok"
            || self.code != "unloaded"
            || self.message.trim().is_empty()
            || self.message.len() > MAXIMUM_FAILURE_RECEIPT_MESSAGE_BYTES
            || self.message.chars().any(char::is_control)
            || self.process_id != expected_process_id
        {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "unloader.receipt",
                message: format!("unloader 收据未声明本次 PID 卸载成功: {self:?}"),
            });
        }
        Ok(())
    }

    /// 卸载失败收据必须绑定当前目标，并与固定退出码分类一致。
    #[cfg(any(target_os = "windows", test))]
    fn validate_failed(
        &self,
        expected_process_id: u32,
        exit_code: i32,
    ) -> Result<(), RuntimeProbeError> {
        let process_matches: bool = if exit_code == 10 && self.code == "unload_config_invalid" {
            self.process_id == 0
        } else {
            self.process_id == expected_process_id
        };
        let code_matches: bool = match exit_code {
            10 => self.code == "unload_config_invalid",
            11 => matches!(
                self.code.as_str(),
                "unload_identity_rejected" | "unload_identity_changed"
            ),
            12 => self.code == "unload_ptrace_attach_failed",
            15 => matches!(
                self.code.as_str(),
                "unload_ptrace_detach_failed"
                    | "unload_remote_stack_cleanup_failed"
                    | "unload_carrier_exited_before_dlclose"
                    | "unload_ptrace_detach_except_carrier_failed"
                    | "unload_carrier_trace_lost"
                    | "unload_wait_capture_failed"
                    | "unload_carrier_exit_failed"
                    | "unload_carrier_exit_unverified"
                    | "unload_file_cleanup_failed"
            ),
            16 => matches!(
                self.code.as_str(),
                "unload_deadline_exceeded"
                    | "unload_memory_init_failed"
                    | "unload_injector_init_failed"
                    | "unload_remote_stack_prepare_failed"
                    | "agent_finalize_call_failed"
                    | "agent_finalize_rejected"
                    | "agent_finalize_incomplete"
                    | "agent_elf_header_restore_failed"
                    | "agent_solist_restore_failed"
                    | "unload_carrier_primitives_missing"
                    | "unload_wait_state_conflict"
                    | "agent_dlclose_failed"
                    | "unload_carrier_exit_invalid"
                    | "agent_mapping_remains"
                    | "agent_not_quiescent"
            ),
            _ => false,
        };
        if self.status != "error"
            || self.message.trim().is_empty()
            || self.message.len() > MAXIMUM_FAILURE_RECEIPT_MESSAGE_BYTES
            || !process_matches
            || !code_matches
        {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "unloader.failure_receipt",
                message: format!(
                    "unloader 失败收据与本次命令不一致: exit_code={exit_code}, receipt={self:?}"
                ),
            });
        }
        Ok(())
    }
}

impl LoaderReceipt {
    /// 严格解码 loader 返回的固定小写 ELF 头保护证据。
    pub(super) fn protected_elf_header_bytes(
        &self,
    ) -> Result<Option<[u8; ELF_HEADER_SIZE]>, RuntimeProbeError> {
        let Some(encoded) = &self.agent_protected_elf_header else {
            return Ok(None);
        };
        if encoded.len() != ELF_HEADER_SIZE * 2
            || !encoded
                .bytes()
                .all(|value: u8| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
        {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: "loader ELF 头保护证据不是固定小写十六进制".to_owned(),
            });
        }
        let mut decoded: [u8; ELF_HEADER_SIZE] = [0; ELF_HEADER_SIZE];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            let nibble = |value: u8| -> u8 {
                match value {
                    b'0'..=b'9' => value - b'0',
                    b'a'..=b'f' => value - b'a' + 10,
                    _ => unreachable!("格式已在解码前验证"),
                }
            };
            decoded[index] = (nibble(pair[0]) << 4) | nibble(pair[1]);
        }
        if decoded.iter().all(|value: &u8| *value == 0) || decoded.starts_with(b"\x7fELF") {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: "loader ELF 头保护证据未证明远程头已替换".to_owned(),
            });
        }
        Ok(Some(decoded))
    }

    /// 加载成功收据必须形成可编码进固定卸载结构的完整 Agent 地址范围。
    pub(super) fn validate_loaded(
        &self,
        expected_process_id: u32,
    ) -> Result<(), RuntimeProbeError> {
        if self.status != "ok" || self.code != "loaded" || self.process_id != expected_process_id {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: format!("loader 收据未声明本次 PID 加载成功: {self:?}"),
            });
        }
        let agent_end: u64 = self
            .agent_base
            .get()
            .checked_add(self.agent_load_size)
            .ok_or_else(|| RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: "Agent 加载地址范围溢出".to_owned(),
            })?;
        let protected_elf_header: Option<[u8; ELF_HEADER_SIZE]> =
            self.protected_elf_header_bytes()?;
        if self.process_start_time == 0
            || self.agent_load_size == 0
            || self.agent_load_size > 256 * 1024 * 1024
            || !(self.agent_base.get()..agent_end).contains(&self.finalize_address.get())
            || !self.agent_mapping_name.starts_with("/memfd:")
            || self.agent_mapping_name.len() >= 64
            || !self.agent_mapping_name.is_ascii()
            || self.agent_mapping_name.as_bytes().contains(&0)
            || match self.agent_mapping_mode {
                AgentMappingMode::Memfd => {
                    self.anonymous_segment_count != 0 || self.anonymous_byte_count != 0
                }
                AgentMappingMode::AnonymousRemap => {
                    self.anonymous_segment_count == 0
                        || self.anonymous_byte_count == 0
                        || self.anonymous_byte_count > self.agent_load_size
                }
            }
            || match self.agent_visibility_mode {
                AgentVisibilityMode::Normal => {
                    self.agent_soinfo_address.is_some() || protected_elf_header.is_some()
                }
                AgentVisibilityMode::SolistHidden => {
                    self.agent_soinfo_address.is_none() || protected_elf_header.is_some()
                }
                AgentVisibilityMode::SolistAndElfHeader => {
                    self.agent_soinfo_address.is_none() || protected_elf_header.is_none()
                }
            }
        {
            return Err(RuntimeProbeError::InvalidOutput {
                stage: "loader.receipt",
                message: format!("loader Agent 卸载身份无效: {self:?}"),
            });
        }
        Ok(())
    }
}

/// 提取唯一收据负载，拒绝缺失或重复行造成的状态歧义。
fn unique_receipt_payload<'a>(
    output: &'a str,
    stage: &'static str,
) -> Result<&'a str, RuntimeProbeError> {
    const PREFIX: &str = "AZLW_RECEIPT ";
    let mut lines = output
        .lines()
        .filter(|line: &&str| line.starts_with(PREFIX));
    let line: &str = lines
        .next()
        .ok_or_else(|| RuntimeProbeError::InvalidOutput {
            stage,
            message: format!("输出缺少 AZLW_RECEIPT: {output:?}"),
        })?;
    if lines.next().is_some() {
        return Err(RuntimeProbeError::InvalidOutput {
            stage,
            message: "输出包含重复 AZLW_RECEIPT".to_owned(),
        });
    }
    Ok(&line[PREFIX.len()..])
}

/// 从带固定前缀的唯一行严格解码加载成功收据。
pub(super) fn parse_loader_receipt(output: &str) -> Result<LoaderReceipt, RuntimeProbeError> {
    serde_json::from_str(unique_receipt_payload(output, "loader.receipt")?).map_err(|source| {
        RuntimeProbeError::Json {
            stage: "loader.receipt",
            source,
        }
    })
}

/// 从非零 loader 输出严格解码目标现场状态。
fn parse_loader_failure_receipt(output: &str) -> Result<LoaderFailureReceipt, RuntimeProbeError> {
    serde_json::from_str(unique_receipt_payload(output, "loader.failure_receipt")?).map_err(
        |source| RuntimeProbeError::Json {
            stage: "loader.failure_receipt",
            source,
        },
    )
}

/// 可信收据同时决定清理策略并保留结构化诊断；未进入 loader 时没有收据。
pub(super) struct LoaderFailureDecision {
    pub(super) requires_restart: bool,
    pub(super) receipt: Option<LoaderFailureReceipt>,
}

pub(super) fn loader_failure_decision(
    output: &str,
    exit_code: i32,
    loader_entered: bool,
    expected_process_id: u32,
) -> Result<LoaderFailureDecision, RuntimeProbeError> {
    if !loader_entered {
        return Ok(LoaderFailureDecision {
            requires_restart: !matches!(exit_code, 70..=72),
            receipt: None,
        });
    }
    let receipt: LoaderFailureReceipt = parse_loader_failure_receipt(output)?;
    let target_state: LoaderTargetState = receipt.validate(expected_process_id, exit_code)?;
    Ok(LoaderFailureDecision {
        requires_restart: matches!(target_state, LoaderTargetState::Unknown),
        receipt: Some(receipt),
    })
}

/// JSONL 独立保存收据，不让 loader 的诊断前导文本挤掉稳定错误字段。
pub(super) fn loader_failure_journal_details(
    exit_code: i32,
    receipt: &LoaderFailureReceipt,
) -> Value {
    json!({
        "exit_code": exit_code,
        "receipt": receipt,
    })
}

#[cfg(test)]
pub(super) fn loader_failure_requires_restart(
    output: &str,
    exit_code: i32,
    loader_entered: bool,
    expected_process_id: u32,
) -> Result<bool, RuntimeProbeError> {
    Ok(
        loader_failure_decision(output, exit_code, loader_entered, expected_process_id)?
            .requires_restart,
    )
}

/// 从 cleanup loader 输出严格解码唯一卸载收据。
#[cfg(any(target_os = "windows", test))]
pub(super) fn parse_unload_receipt(output: &str) -> Result<UnloadReceipt, RuntimeProbeError> {
    serde_json::from_str(unique_receipt_payload(output, "unloader.receipt")?).map_err(|source| {
        RuntimeProbeError::Json {
            stage: "unloader.receipt",
            source,
        }
    })
}

/// 严格关联卸载命令状态、进入标记和唯一收据，保留可信失败诊断。
#[cfg(any(target_os = "windows", test))]
pub(super) fn validate_unloader_result(
    output: &str,
    exit_code: i32,
    unloader_entered: bool,
    expected_process_id: u32,
) -> Result<UnloadReceipt, RuntimeProbeError> {
    if !unloader_entered {
        return if exit_code == 0 {
            Err(RuntimeProbeError::InvalidOutput {
                stage: "unloader.execute",
                message: "设备命令成功但未声明进入 unloader".to_owned(),
            })
        } else {
            Err(RuntimeProbeError::DeviceCommand {
                stage: "unloader.execute",
                exit_code,
                output: output.to_owned(),
            })
        };
    }

    let receipt: UnloadReceipt = parse_unload_receipt(output)?;
    if exit_code == 0 {
        receipt.validate_unloaded(expected_process_id)?;
        return Ok(receipt);
    }

    receipt.validate_failed(expected_process_id, exit_code)?;
    Err(RuntimeProbeError::UnloaderRejected {
        exit_code,
        code: receipt.code,
        message: receipt.message,
        process_id: receipt.process_id,
    })
}

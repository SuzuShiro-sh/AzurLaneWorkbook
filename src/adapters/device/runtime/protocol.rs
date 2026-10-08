//! 定义 RPC v1 的线上数据结构、边界校验和稳定错误类型。

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use suzushiro_capability_core::validate_capability_entry;
pub use suzushiro_capability_core::{CapabilitiesResult, CapabilityStatus};
pub use suzushiro_rpc_core::{RequestId, RetryDirective, SessionEffect};
use suzushiro_rpc_core::{
    RpcError as CoreRpcError, RpcRequest as CoreRpcRequest, RpcResponse as CoreRpcResponse,
};
use suzushiro_text_format::{is_canonical_sha256, is_lower_hex_with_len};
use thiserror::Error;

use crate::adapters::device::session::SessionId;

// 正式构建和测试统一读取仓库根目录的 RPC v1 JSON 契约。
mod runtime_rpc_contract {
    include!(concat!(env!("OUT_DIR"), "/runtime_rpc_contract.rs"));
}

mod catalog;
mod commands;
mod equipment;
mod owned_query;
mod state;
pub use owned_query::{OwnedQuery, OwnedQueryKind, OwnedQueryResult};

pub(crate) use catalog::ShipCatalogPagePayload;
pub use catalog::{
    ShipCatalogPageReadError, ShipCatalogPageResult, ShipCatalogRecord, ShipCatalogTableKey,
};
pub(crate) use commands::EquipmentCommandLookupPayload;
pub use commands::{
    EquipmentCommandAction, EquipmentCommandActionKind, EquipmentCommandEquipment,
    EquipmentCommandMaterialCost, RuntimeEquipmentCommand,
};
pub use equipment::{
    ComposeRecipePageReadError, ComposeRecipePageResult, EquipmentComposeRecipe,
    EquipmentConfigBatchResult, EquipmentConfigPageReadError, EquipmentConfigPageResult,
    EquipmentConfigSource, EquipmentReferenceNameBatchResult, EquipmentWeaponBatchResult,
    RuntimeEquipmentAttributeName, RuntimeEquipmentConfig, RuntimeEquipmentNationName,
    RuntimeEquipmentShipTypeName, RuntimeEquipmentTypeName, RuntimeEquipmentWeaponDetail,
    RuntimeSkillEffectDetail, RuntimeSkillEffectSource, SkillEffectBatchResult, SkillEffectQuery,
};
pub(crate) use equipment::{
    EquipmentCatalogPagePayload, EquipmentConfigBatchPayload, EquipmentReferenceNameBatchPayload,
    EquipmentWeaponBatchPayload, SkillEffectBatchPayload,
};
pub use state::{
    AccountBeforeResult, BagItem, ComposeRecipe, DockSnapshot, EquipmentReadError, PlayerResources,
    ReadError, RuntimeEquipment, RuntimeFleetKind, RuntimeFleetMembership, RuntimeFleetTeam,
    RuntimeShip, RuntimeShipAttributes, RuntimeShipClassification, RuntimeShipDetail,
    RuntimeShipOilCost, RuntimeShipSkill, RuntimeShipSkillDetail, RuntimeShipSlot,
    RuntimeShipSlotRule, ShipDetailReadError, ShipDetailSource, ShipReadError, SnapshotBagResult,
    SnapshotOwnedStateResult, SnapshotShipDetailsResult, WarehouseEquipment, WarehouseSnapshot,
};
pub(crate) use state::{
    SnapshotBagPayload, SnapshotOwnedStatePayload, SnapshotShipDetailsPayload,
    validate_full_state_read_options,
};

pub(crate) use runtime_rpc_contract::{
    EXPECTED_AGENT_VERSION, MAX_DOCK_PAGE_SIZE, MAX_EQUIPMENT_CATALOG_ITEMS,
    MAX_EQUIPMENT_FRAME_SIZE, MAX_EQUIPMENT_PAGE_SIZE, MAX_EQUIPMENT_REFERENCE_BATCH_SIZE,
    MAX_EQUIPMENT_WEAPON_BATCH_SIZE, MAX_HANDSHAKE_ATTEMPTS, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    MAX_SHIP_CATALOG_FRAME_SIZE, MAX_SHIP_CATALOG_ITEMS, MAX_SHIP_CATALOG_PAGE_SIZE,
    MAX_SKILL_EFFECT_BATCH_SIZE, MAX_SNAPSHOT_ITEMS, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS,
    PROTOCOL_VERSION, UNAUTHENTICATED_PROBE_CONNECTIONS,
};
use runtime_rpc_contract::{
    MAX_ENHANCE_MATERIALS, MAX_FLEET_TEAM_SHIPS, MAX_SHIP_FLEET_MEMBERSHIPS, MAX_SHIP_SKILLS,
    MAX_SHIP_SLOT_EQUIPMENT_TYPES, RPC_OPERATION_NAMES, SHIP_EQUIPMENT_SLOT_COUNT,
};

const _: () = assert!(
    MAX_SHIP_CATALOG_FRAME_SIZE > 0
        && MAX_SHIP_CATALOG_PAGE_SIZE >= MAX_SHIP_CATALOG_FRAME_SIZE
        && MAX_SHIP_CATALOG_PAGE_SIZE.is_multiple_of(MAX_SHIP_CATALOG_FRAME_SIZE)
        && MAX_EQUIPMENT_FRAME_SIZE > 0
        && MAX_EQUIPMENT_PAGE_SIZE >= MAX_EQUIPMENT_FRAME_SIZE
        && MAX_EQUIPMENT_PAGE_SIZE.is_multiple_of(MAX_EQUIPMENT_FRAME_SIZE)
);

// 握手后的能力响应必须完整返回这些稳定键，包括明确禁用的写能力。
const REQUIRED_CAPABILITY_KEYS: [&str; 15] = [
    "runtime.health",
    "runtime.main_thread_queue",
    "read.bag",
    "read.owned_state",
    "read.ship_details",
    "read.equipment_configs",
    "read.compose_recipes",
    "read.equipment_weapons",
    "read.skill_effects",
    "read.equipment_reference_names",
    "write.equip",
    "write.unequip",
    "write.compose",
    "write.enhance",
    "write.destroy",
];

/// 设备端 agent 支持的处理器架构。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAbi {
    /// MuMu12 的 x86-64 进程。
    X86_64,
}

impl Display for RuntimeAbi {
    /// 输出线上协议使用的稳定架构标识。
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::X86_64 => "x86_64",
        })
    }
}

/// agent 返回的握手成功消息类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HandshakeResponseKind {
    HandshakeOk,
}

/// agent 完成凭据核验后返回的身份声明。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HandshakeResponse {
    pub protocol_version: u32,
    pub message_type: HandshakeResponseKind,
    pub session_id: SessionId,
    pub agent_version: String,
    pub process_id: u32,
    pub package_name: String,
    pub abi: RuntimeAbi,
}

/// 宿主在连接前已经独立验证的目标身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedAgent {
    session_id: SessionId,
    process_id: u32,
    package_name: String,
    abi: RuntimeAbi,
}

impl ExpectedAgent {
    /// 创建待核对身份；PID 和包名必须先通过设备发现流程取得。
    pub fn new(
        session_id: SessionId,
        process_id: u32,
        package_name: impl Into<String>,
        abi: RuntimeAbi,
    ) -> Result<Self, RuntimeProtocolError> {
        let package_name: String = package_name.into();
        validate_positive_u32("process_id", process_id)?;
        validate_package_name(&package_name)?;
        Ok(Self {
            session_id,
            process_id,
            package_name,
            abi,
        })
    }

    /// 返回仅用于组装握手请求的预期会话标识。
    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }
}

/// 握手成功后固定到当前连接的 agent 身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentIdentity {
    /// agent 自报版本。
    pub agent_version: String,
    /// 目标游戏进程 PID。
    pub process_id: u32,
    /// 目标游戏包名。
    pub package_name: String,
    /// 目标游戏进程架构。
    pub abi: RuntimeAbi,
}

impl HandshakeResponse {
    /// 将 agent 自报身份与设备发现结果逐项核对，并构造连接固定身份。
    pub fn verify(self, expected: &ExpectedAgent) -> Result<AgentIdentity, RuntimeProtocolError> {
        validate_protocol_version(self.protocol_version)?;
        validate_identity_field(
            "session_id",
            &expected.session_id.to_string(),
            &self.session_id.to_string(),
        )?;
        validate_identity_field(
            "process_id",
            &expected.process_id.to_string(),
            &self.process_id.to_string(),
        )?;
        validate_identity_field("package_name", &expected.package_name, &self.package_name)?;
        validate_identity_field("abi", &expected.abi.to_string(), &self.abi.to_string())?;
        if self.agent_version != EXPECTED_AGENT_VERSION {
            return Err(RuntimeProtocolError::new(
                "agent_version_mismatch",
                format!(
                    "agent 版本应为 {EXPECTED_AGENT_VERSION}，实际为 {}",
                    self.agent_version
                ),
            ));
        }

        Ok(AgentIdentity {
            agent_version: self.agent_version,
            process_id: self.process_id,
            package_name: self.package_name,
            abi: self.abi,
        })
    }
}

/// 运行态协议允许的严格 RPC 操作。声明顺序与共享操作目录一致。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub(crate) enum RpcOperation {
    Health,
    Capabilities,
    SnapshotBag,
    SnapshotResources,
    SnapshotOwnedState,
    QueryOwned,
    SnapshotShipDetails,
    SnapshotAccountBefore,
    SnapshotShipCatalog,
    SnapshotEquipmentConfigs,
    SnapshotComposeRecipes,
    SnapshotEquipmentWeapons,
    SnapshotSkillEffects,
    SnapshotEquipmentReferenceNames,
    ExecuteEquipmentCommand,
    QueryEquipmentCommand,
    CancelEquipmentCommand,
    Shutdown,
}

const _: () = assert!(RPC_OPERATION_NAMES.len() == 18);

/// 线上固定为 16 位小写十六进制的 64 位运行态地址。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeAddress(u64);

impl RuntimeAddress {
    /// 返回已通过固定格式校验的原始地址值。
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

impl Serialize for RuntimeAddress {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("{:016x}", self.0))
    }
}

impl<'de> Deserialize<'de> for RuntimeAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value: &str = <&str>::deserialize(deserializer)?;
        if !is_lower_hex_with_len(value, 16) {
            return Err(serde::de::Error::custom(
                "运行态地址必须是 16 位小写十六进制字符串",
            ));
        }
        let address: u64 = u64::from_str_radix(value, 16).map_err(serde::de::Error::custom)?;
        if address == 0 {
            return Err(serde::de::Error::custom("运行态地址不得为 0"));
        }
        Ok(Self(address))
    }
}

/// 运行态操作与载荷组成的通用 RPC 请求信封。
pub(crate) type RpcRequest<T> = CoreRpcRequest<RpcOperation, T>;

/// 校验运行态超时边界并构造冻结协议版本的 RPC 请求。
pub(crate) fn build_rpc_request<T>(
    request_id: RequestId,
    operation: RpcOperation,
    timeout_ms: u32,
    payload: T,
) -> Result<RpcRequest<T>, RuntimeProtocolError> {
    validate_timeout(timeout_ms)?;
    Ok(RpcRequest::new(
        PROTOCOL_VERSION,
        request_id,
        operation,
        timeout_ms,
        payload,
    ))
}

/// 不需要业务参数的 RPC 使用的严格空载荷。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyPayload {}

/// 使用通用信封承载运行态结果或 agent 稳定错误。
pub(crate) type RpcResponse<T> = CoreRpcResponse<T, AgentError>;

/// agent 返回的稳定失败信息与 JSON 诊断详情。
pub type AgentError = CoreRpcError<BTreeMap<String, Value>>;

/// 校验错误码、阶段、消息和详情键均满足运行态协议约束。
pub(crate) fn validate_agent_error(error: &AgentError) -> Result<(), RuntimeProtocolError> {
    validate_stable_token("error.code", &error.code)?;
    validate_stage(&error.stage)?;
    validate_non_empty("error.message", &error.message, 4096)?;
    for key in error.details.keys() {
        validate_stable_token("error.details key", key)?;
    }
    Ok(())
}

/// `health` 成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResult {
    /// agent 版本。
    pub agent_version: String,
    /// 目标游戏进程 PID。
    pub process_id: u32,
    /// 目标游戏包名。
    pub package_name: String,
    /// 目标游戏进程架构。
    pub abi: RuntimeAbi,
    /// 当前会话状态。
    pub session_state: RuntimeSessionState,
    /// 主线程任务队列是否可接受读取任务。
    pub main_thread_queue_ready: bool,
    /// Agent 观察到主线程 Lua 状态更换时递增的静态目录代次。
    pub catalog_generation: u64,
}

impl HealthResult {
    /// 确认健康响应仍指向握手时固定的同一 agent 与目标进程。
    pub(crate) fn validate(&self, identity: &AgentIdentity) -> Result<(), RuntimeProtocolError> {
        validate_agent_version(&self.agent_version)?;
        validate_identity_field(
            "agent_version",
            &identity.agent_version,
            &self.agent_version,
        )?;
        validate_identity_field(
            "process_id",
            &identity.process_id.to_string(),
            &self.process_id.to_string(),
        )?;
        validate_identity_field("package_name", &identity.package_name, &self.package_name)?;
        validate_identity_field("abi", &identity.abi.to_string(), &self.abi.to_string())?;
        Ok(())
    }
}

/// 运行态协议仅接受已完成握手且可响应 RPC 的 ready 状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSessionState {
    /// 已完成握手。
    Ready,
}

/// Agent 对原装备命令当前能够确认的状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentCommandStatus {
    /// 后续主线程回读已经同时匹配槽位和相关仓库数量的完整后态。
    Success,
    /// Agent 已获得无部分写入的确定失败证据。
    Failed,
    /// 命令已经派发，但当前证据仍不足以确认最终结果。
    Unknown,
}

/// 装备命令当前拥有的最强设备端生命周期证据。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentCommandPhase {
    /// 官方通知已派发，Agent 正在后续帧观察局部状态。
    Observing,
    /// 完整预期后态已经观察到。
    Succeeded,
    /// 已取得无部分写入的确定失败证据。
    Failed,
    /// 等待已停止，但不能排除游戏服务器稍后完成命令。
    Uncertain,
}

/// execute、query 和 cancel 返回的同一原命令状态收据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentCommandReceipt {
    pub schema_version: u32,
    pub command_id: String,
    pub status: EquipmentCommandStatus,
    pub phase: EquipmentCommandPhase,
    pub write_dispatched: bool,
    pub cancel_requested: bool,
    pub observation_count: u32,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub error_code: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub message: Option<String>,
}

impl EquipmentCommandReceipt {
    /// 核对回执仍描述请求中的原命令，并验证状态、阶段和诊断字段一致。
    pub(crate) fn validate(&self, expected_command_id: &str) -> Result<(), RuntimeProtocolError> {
        if self.schema_version != 1 {
            return Err(RuntimeProtocolError::new(
                "equipment_command_receipt_schema_unsupported",
                format!(
                    "equipment_command receipt schema_version 必须为 1，实际为 {}",
                    self.schema_version
                ),
            ));
        }
        validate_sha256("equipment_command_receipt.command_id", &self.command_id)?;
        validate_identity_field(
            "equipment_command_receipt.command_id",
            expected_command_id,
            &self.command_id,
        )?;
        if !self.write_dispatched {
            return Err(RuntimeProtocolError::new(
                "equipment_command_receipt_not_dispatched",
                "确定未派发的命令必须返回 AgentError，不能伪造成状态收据",
            ));
        }
        let state_matches = matches!(
            (self.status, self.phase),
            (
                EquipmentCommandStatus::Success,
                EquipmentCommandPhase::Succeeded
            ) | (
                EquipmentCommandStatus::Failed,
                EquipmentCommandPhase::Failed
            ) | (
                EquipmentCommandStatus::Unknown,
                EquipmentCommandPhase::Observing | EquipmentCommandPhase::Uncertain
            )
        );
        if !state_matches {
            return Err(RuntimeProtocolError::new(
                "equipment_command_receipt_state_invalid",
                "装备命令 status 与 phase 不一致",
            ));
        }
        match (&self.error_code, &self.message) {
            (Some(code), Some(message)) => {
                validate_stable_token("equipment_command_receipt.error_code", code)?;
                validate_non_empty("equipment_command_receipt.message", message, 4096)?;
            }
            (None, None) => {}
            _ => {
                return Err(RuntimeProtocolError::new(
                    "equipment_command_receipt_diagnostics_invalid",
                    "装备命令 error_code 与 message 必须同时出现或同时为空",
                ));
            }
        }
        if self.status == EquipmentCommandStatus::Success && self.error_code.is_some() {
            return Err(RuntimeProtocolError::new(
                "equipment_command_receipt_diagnostics_invalid",
                "成功回执不得携带错误诊断",
            ));
        }
        Ok(())
    }
}

/// `shutdown` 成功后只允许出现的排空状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownState {
    /// Agent 已封闭任务队列，等待宿主冻结和卸载。
    Prepared,
}

/// 宿主进入 ptrace 冻结前必须逐项核对的 Agent 排空收据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShutdownPreparedResult {
    pub state: ShutdownState,
    pub session_id: SessionId,
    pub process_id: u32,
    pub worker_tid: u32,
    pub worker_start_time: u64,
    pub hook_target: RuntimeAddress,
    pub trampoline_start: RuntimeAddress,
    pub trampoline_size: u64,
}

impl ShutdownPreparedResult {
    /// 关闭收据必须继续绑定握手身份，并形成两个无溢出的非空执行区间。
    pub(crate) fn validate(
        &self,
        expected_session_id: SessionId,
        identity: &AgentIdentity,
    ) -> Result<(), RuntimeProtocolError> {
        validate_identity_field(
            "shutdown.session_id",
            &expected_session_id.to_string(),
            &self.session_id.to_string(),
        )?;
        validate_identity_field(
            "shutdown.process_id",
            &identity.process_id.to_string(),
            &self.process_id.to_string(),
        )?;
        validate_positive_u32("shutdown.worker_tid", self.worker_tid)?;
        validate_positive_u64("shutdown.worker_start_time", self.worker_start_time)?;
        if self.worker_tid == self.process_id {
            return Err(RuntimeProtocolError::new(
                "shutdown_worker_is_main_thread",
                "shutdown worker_tid 必须指向独立工作线程",
            ));
        }
        validate_positive_u64("shutdown.trampoline_size", self.trampoline_size)?;
        if self.trampoline_size > 16 * 1024 * 1024
            || self
                .trampoline_start
                .get()
                .checked_add(self.trampoline_size)
                .is_none()
        {
            return Err(RuntimeProtocolError::new(
                "shutdown_trampoline_range_invalid",
                "shutdown trampoline 地址范围无效或超过 16 MiB",
            ));
        }
        if self.hook_target == self.trampoline_start {
            return Err(RuntimeProtocolError::new(
                "shutdown_executable_ranges_overlap",
                "shutdown Hook 目标与 trampoline 起始地址不得相同",
            ));
        }
        Ok(())
    }
}

/// 验证基础能力格式和当前协议的稳定能力集合及写入状态策略。
pub(crate) fn validate_capabilities_result(
    result: &CapabilitiesResult,
) -> Result<(), RuntimeProtocolError> {
    for required_key in REQUIRED_CAPABILITY_KEYS {
        if !result.capabilities.contains_key(required_key) {
            return Err(RuntimeProtocolError::new(
                "required_capability_missing",
                format!("capabilities 缺少稳定能力键 {required_key}"),
            ));
        }
    }

    for (key, status) in &result.capabilities {
        validate_capability_entry(key, status).map_err(|source| {
            let (code, message) = source.into_parts();
            RuntimeProtocolError::new(code, message)
        })?;
        match key.as_str() {
            "write.equip" | "write.unequip" | "write.destroy" => {
                let allowed = if status.available {
                    status.reason_code == "ready"
                } else {
                    matches!(
                        status.reason_code.as_str(),
                        "mvp_read_only" | "main_thread_not_ready" | "owned_state_not_ready"
                    )
                };
                if !allowed {
                    return Err(RuntimeProtocolError::new(
                        "equipment_write_capability_invalid",
                        format!("能力 {key} 的 available 与 reason_code 组合不受支持"),
                    ));
                }
            }
            "write.compose" => {
                let allowed = if status.available {
                    status.reason_code == "ready"
                } else {
                    matches!(
                        status.reason_code.as_str(),
                        "mvp_read_only"
                            | "main_thread_not_ready"
                            | "owned_state_not_ready"
                            | "bag_proxy_not_ready"
                            | "compose_recipes_not_ready"
                    )
                };
                if !allowed {
                    return Err(RuntimeProtocolError::new(
                        "equipment_write_capability_invalid",
                        format!("能力 {key} 的 available 与 reason_code 组合不受支持"),
                    ));
                }
            }
            "write.enhance" => {
                let allowed = if status.available {
                    status.reason_code == "ready"
                } else {
                    matches!(
                        status.reason_code.as_str(),
                        "mvp_read_only"
                            | "main_thread_not_ready"
                            | "owned_state_not_ready"
                            | "bag_proxy_not_ready"
                            | "equipment_configs_not_ready"
                    )
                };
                if !allowed {
                    return Err(RuntimeProtocolError::new(
                        "equipment_write_capability_invalid",
                        format!("能力 {key} 的 available 与 reason_code 组合不受支持"),
                    ));
                }
            }
            _ if key.starts_with("write.") => {
                if status.available || status.reason_code != "mvp_read_only" {
                    return Err(RuntimeProtocolError::new(
                        "write_capability_not_supported",
                        format!(
                            "未实现写能力 {key} 必须为 available=false 且 reason_code=mvp_read_only"
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// 已收到格式正确但违反冻结契约的数据。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{code}: {message}")]
pub struct RuntimeProtocolError {
    /// 稳定错误码。
    pub code: &'static str,
    /// 可直接用于诊断的中文说明。
    pub message: String,
}

impl RuntimeProtocolError {
    /// 构造包含稳定错误码和中文诊断说明的协议错误。
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 拒绝与当前冻结版本不同的协议消息。
pub(crate) fn validate_protocol_version(actual: u32) -> Result<(), RuntimeProtocolError> {
    if actual != PROTOCOL_VERSION {
        return Err(RuntimeProtocolError::new(
            "protocol_version_mismatch",
            format!("协议版本应为 {PROTOCOL_VERSION}，实际为 {actual}"),
        ));
    }
    Ok(())
}

/// 确认响应严格对应当前顺序请求，防止串帧或乱序消费。
pub(crate) fn validate_request_id(
    expected: RequestId,
    actual: RequestId,
) -> Result<(), RuntimeProtocolError> {
    if actual != expected {
        return Err(RuntimeProtocolError::new(
            "request_id_mismatch",
            format!("响应请求号应为 {expected}，实际为 {actual}"),
        ));
    }
    Ok(())
}

/// 推进会话请求号，并把通用数值耗尽映射为当前协议的稳定错误。
pub(crate) fn next_request_id(current: RequestId) -> Result<RequestId, RuntimeProtocolError> {
    current.checked_next().map_err(|_| {
        RuntimeProtocolError::new(
            "request_id_exhausted",
            "当前会话的请求编号已经耗尽，必须建立新会话",
        )
    })
}

/// 将请求超时限制在 agent 可以可靠调度的冻结范围内。
fn validate_timeout(timeout_ms: u32) -> Result<(), RuntimeProtocolError> {
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(RuntimeProtocolError::new(
            "timeout_out_of_range",
            format!("timeout_ms 只允许 {MIN_TIMEOUT_MS} 至 {MAX_TIMEOUT_MS}，实际为 {timeout_ms}"),
        ));
    }
    Ok(())
}

/// 校验 agent 版本长度和允许出现在版本号中的 ASCII 字符。
fn validate_agent_version(value: &str) -> Result<(), RuntimeProtocolError> {
    validate_non_empty("agent_version", value, 64)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(RuntimeProtocolError::new(
            "agent_version_invalid",
            "agent_version 含有不允许的字符",
        ));
    }
    Ok(())
}

/// 按当前 Android 目标所需的白名单字符校验包名。
fn validate_package_name(value: &str) -> Result<(), RuntimeProtocolError> {
    validate_non_empty("package_name", value, 255)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_')
    }) || !value.contains('.')
    {
        return Err(RuntimeProtocolError::new(
            "package_name_invalid",
            format!("包名 {value} 不符合 Android 包名白名单"),
        ));
    }
    Ok(())
}

/// 对比单个身份字段，并以统一错误码报告连接目标漂移。
fn validate_identity_field(
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), RuntimeProtocolError> {
    if actual != expected {
        return Err(RuntimeProtocolError::new(
            "agent_identity_mismatch",
            format!("{field} 应为 {expected}，实际为 {actual}"),
        ));
    }
    Ok(())
}

/// 校验带命名空间的稳定错误阶段。
fn validate_stage(value: &str) -> Result<(), RuntimeProtocolError> {
    validate_stable_token("error.stage", value)?;
    if !value.contains('.') {
        return Err(RuntimeProtocolError::new(
            "error_stage_invalid",
            format!("错误阶段 {value} 缺少命名空间"),
        ));
    }
    Ok(())
}

/// 校验可供程序分支判断的小写稳定标识。
fn validate_stable_token(field: &'static str, value: &str) -> Result<(), RuntimeProtocolError> {
    validate_non_empty(field, value, 128)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    }) {
        return Err(RuntimeProtocolError::new(
            "stable_token_invalid",
            format!("{field}={value} 不是稳定小写标识"),
        ));
    }
    Ok(())
}

/// 校验文本经过裁剪后非空且不超过字节上限。
fn validate_non_empty(
    field: &'static str,
    value: &str,
    max_length: usize,
) -> Result<(), RuntimeProtocolError> {
    if value.trim().is_empty() || value.len() > max_length {
        return Err(RuntimeProtocolError::new(
            "text_field_invalid",
            format!("{field} 必须为 1 至 {max_length} 字节的非空文本"),
        ));
    }
    Ok(())
}

/// 校验允许空串的有界文本；UTF-8 合法性已由 serde JSON 解码保证。
fn validate_text(
    field: &'static str,
    value: &str,
    max_length: usize,
) -> Result<(), RuntimeProtocolError> {
    if value.len() > max_length {
        return Err(RuntimeProtocolError::new(
            "text_field_invalid",
            format!("{field} 最多允许 {max_length} 字节，实际为 {}", value.len()),
        ));
    }
    Ok(())
}

/// 校验规范小写 SHA-256 文本，供 profile 身份与快照来源交叉核对。
fn validate_sha256(field: &'static str, value: &str) -> Result<(), RuntimeProtocolError> {
    if !is_canonical_sha256(value) {
        return Err(RuntimeProtocolError::new(
            "sha256_invalid",
            format!("{field} 必须是 64 位小写十六进制 SHA-256"),
        ));
    }
    Ok(())
}

/// 校验 32 位标识或数量大于零。
fn validate_positive_u32(field: &'static str, value: u32) -> Result<(), RuntimeProtocolError> {
    if value == 0 {
        return Err(RuntimeProtocolError::new(
            "positive_integer_required",
            format!("{field} 必须大于 0"),
        ));
    }
    Ok(())
}

/// 校验 64 位标识或数量大于零。
fn validate_positive_u64(field: &'static str, value: u64) -> Result<(), RuntimeProtocolError> {
    if value == 0 {
        return Err(RuntimeProtocolError::new(
            "positive_integer_required",
            format!("{field} 必须大于 0"),
        ));
    }
    Ok(())
}

/// Lua 5.1 number 使用双精度表示，标识必须位于可精确表示的正整数范围。
fn validate_positive_lua_integer(
    field: &'static str,
    value: u64,
) -> Result<(), RuntimeProtocolError> {
    const MAXIMUM_EXACT_LUA_INTEGER: u64 = 9_007_199_254_740_991;
    if value == 0 || value > MAXIMUM_EXACT_LUA_INTEGER {
        return Err(RuntimeProtocolError::new(
            "positive_lua_integer_required",
            format!("{field} 必须是 Lua 可精确表示的正整数，实际为 {value}"),
        ));
    }
    Ok(())
}

/// 部分客户端分类使用零作为有效键，但仍须位于 Lua number 的精确整数范围。
fn validate_nonnegative_lua_integer(
    field: &'static str,
    value: u64,
) -> Result<(), RuntimeProtocolError> {
    const MAXIMUM_EXACT_LUA_INTEGER: u64 = 9_007_199_254_740_991;
    if value > MAXIMUM_EXACT_LUA_INTEGER {
        return Err(RuntimeProtocolError::new(
            "nonnegative_lua_integer_required",
            format!("{field} 必须是 Lua 可精确表示的非负整数，实际为 {value}"),
        ));
    }
    Ok(())
}

/// 页游标必须精确消费 `min(page_size, total_count - start_index)` 个目录位置。
fn validate_next_catalog_index(
    catalog: &'static str,
    start_index: u32,
    total_count: u32,
    page_size: u32,
    next_index: Option<u32>,
) -> Result<(), RuntimeProtocolError> {
    let expected_end = start_index.saturating_add(page_size).min(total_count);
    let expected_next = (expected_end < total_count).then_some(expected_end);
    if next_index != expected_next {
        return Err(RuntimeProtocolError::new(
            "catalog_next_index_invalid",
            format!("{catalog}.next_index 应为 {expected_next:?}，实际为 {next_index:?}"),
        ));
    }
    Ok(())
}

/// 原生 Lua JSON 允许普通 JSON 或显式混合表，但递归深度和字段数量必须有界。
fn validate_lua_json_value(
    field: &'static str,
    value: &Value,
    depth: usize,
) -> Result<(), RuntimeProtocolError> {
    const MAXIMUM_DEPTH: usize = 16;
    const MAXIMUM_ENTRIES: usize = 4_096;
    if depth > MAXIMUM_DEPTH {
        return Err(RuntimeProtocolError::new(
            "lua_value_depth_invalid",
            format!("{field} 超过宿主允许的递归深度"),
        ));
    }
    match value {
        Value::Array(values) => {
            if values.len() > MAXIMUM_ENTRIES {
                return Err(RuntimeProtocolError::new(
                    "lua_value_size_invalid",
                    format!("{field} 数组条目数超过 {MAXIMUM_ENTRIES}"),
                ));
            }
            for child in values {
                validate_lua_json_value(field, child, depth + 1)?;
            }
        }
        Value::Object(values) => {
            if values.len() > MAXIMUM_ENTRIES {
                return Err(RuntimeProtocolError::new(
                    "lua_value_size_invalid",
                    format!("{field} 对象字段数超过 {MAXIMUM_ENTRIES}"),
                ));
            }
            for child in values.values() {
                validate_lua_json_value(field, child, depth + 1)?;
            }
        }
        Value::Number(number) => {
            if number.as_f64().is_none_or(|number| !number.is_finite()) {
                return Err(RuntimeProtocolError::new(
                    "lua_value_number_invalid",
                    format!("{field} 包含不能表示为有限双精度数的 JSON 数字"),
                ));
            }
        }
        Value::String(text) if text.len() > 64 * 1024 => {
            return Err(RuntimeProtocolError::new(
                "lua_value_string_invalid",
                format!("{field} 包含超过 65536 字节的字符串"),
            ));
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod fixed_hex_contract_tests {
    use super::{RequestId, RetryDirective, RuntimeAddress, SessionEffect, next_request_id};

    #[test]
    fn request_id_requires_exact_lower_hex_wire_text() {
        let request_id: RequestId = serde_json::from_str(r#""00000000000000af""#).unwrap();
        assert_eq!(request_id.to_string(), "00000000000000af");

        for invalid in [
            r#""00000000000000AF""#,
            r#""00000000000000ag""#,
            r#""0000000000000af""#,
            r#""000000000000000af""#,
            r#""0000000000000中""#,
        ] {
            let error = serde_json::from_str::<RequestId>(invalid).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("request_id 必须是 16 位小写十六进制字符串")
            );
        }
    }

    #[test]
    fn request_id_starts_at_one_and_reports_stable_exhaustion() {
        assert_eq!(RequestId::FIRST.to_string(), "0000000000000001");
        assert_eq!(
            next_request_id(RequestId::FIRST).unwrap().to_string(),
            "0000000000000002"
        );

        let maximum: RequestId = serde_json::from_str(r#""ffffffffffffffff""#).unwrap();
        let error = next_request_id(maximum).unwrap_err();
        assert_eq!(error.code, "request_id_exhausted");
        assert_eq!(error.message, "当前会话的请求编号已经耗尽，必须建立新会话");
    }

    #[test]
    fn retry_and_session_enums_keep_exact_wire_names() {
        for (value, json) in [
            (RetryDirective::Never, r#""never""#),
            (RetryDirective::SameRequest, r#""same_request""#),
            (RetryDirective::AfterReconnect, r#""after_reconnect""#),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<RetryDirective>(json).unwrap(), value);
        }
        for (value, json) in [
            (SessionEffect::Unchanged, r#""unchanged""#),
            (SessionEffect::MustClose, r#""must_close""#),
            (SessionEffect::StateUnknown, r#""state_unknown""#),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<SessionEffect>(json).unwrap(), value);
        }

        assert!(serde_json::from_str::<RetryDirective>(r#""retry""#).is_err());
        assert!(serde_json::from_str::<SessionEffect>(r#""closed""#).is_err());
    }

    #[test]
    fn runtime_address_preserves_format_and_nonzero_rules() {
        let address: RuntimeAddress = serde_json::from_str(r#""00000000000000af""#).unwrap();
        assert_eq!(
            serde_json::to_string(&address).unwrap(),
            r#""00000000000000af""#
        );

        for invalid in [
            r#""00000000000000AF""#,
            r#""00000000000000ag""#,
            r#""0000000000000af""#,
            r#""000000000000000af""#,
        ] {
            let error = serde_json::from_str::<RuntimeAddress>(invalid).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("运行态地址必须是 16 位小写十六进制字符串")
            );
        }
        let zero = serde_json::from_str::<RuntimeAddress>(r#""0000000000000000""#).unwrap_err();
        assert!(zero.to_string().contains("运行态地址不得为 0"));
    }
}

#[cfg(test)]
mod rpc_envelope_contract_tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::{
        AgentError, EXPECTED_AGENT_VERSION, EmptyPayload, HandshakeResponse, HealthResult,
        MAX_DOCK_PAGE_SIZE, MAX_ENHANCE_MATERIALS, MAX_EQUIPMENT_CATALOG_ITEMS,
        MAX_EQUIPMENT_FRAME_SIZE, MAX_EQUIPMENT_PAGE_SIZE, MAX_EQUIPMENT_REFERENCE_BATCH_SIZE,
        MAX_EQUIPMENT_WEAPON_BATCH_SIZE, MAX_FLEET_TEAM_SHIPS, MAX_HANDSHAKE_ATTEMPTS,
        MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, MAX_SHIP_CATALOG_FRAME_SIZE, MAX_SHIP_CATALOG_ITEMS,
        MAX_SHIP_CATALOG_PAGE_SIZE, MAX_SHIP_FLEET_MEMBERSHIPS, MAX_SHIP_SKILLS,
        MAX_SHIP_SLOT_EQUIPMENT_TYPES, MAX_SKILL_EFFECT_BATCH_SIZE, MAX_SNAPSHOT_ITEMS,
        MAX_TIMEOUT_MS, MIN_TIMEOUT_MS, PROTOCOL_VERSION, RPC_OPERATION_NAMES, RequestId,
        RetryDirective, RpcOperation, RpcResponse, SHIP_EQUIPMENT_SLOT_COUNT, SessionEffect,
        SnapshotBagPayload, UNAUTHENTICATED_PROBE_CONNECTIONS, build_rpc_request,
        validate_agent_error,
    };

    const SHARED_CONTRACT_JSON: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/contracts/runtime-rpc-v1.json"
    ));

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SharedContract {
        description: String,
        schema_version: u32,
        constants: SharedConstants,
        operations: Vec<String>,
        frames: SharedFrames,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SharedConstants {
        protocol_version: u32,
        agent_version: String,
        minimum_timeout_ms: u32,
        maximum_timeout_ms: u32,
        maximum_handshake_attempts: u32,
        unauthenticated_probe_connections: u32,
        maximum_request_bytes: usize,
        maximum_response_bytes: usize,
        maximum_snapshot_items: u32,
        maximum_equipment_page_size: u32,
        maximum_equipment_frame_size: u32,
        maximum_equipment_catalog_items: u32,
        maximum_ship_catalog_page_size: u32,
        maximum_ship_catalog_frame_size: u32,
        maximum_dock_page_size: u32,
        maximum_ship_catalog_items: u32,
        maximum_equipment_weapon_batch_size: u32,
        maximum_skill_effect_batch_size: u32,
        maximum_equipment_reference_batch_size: u32,
        ship_equipment_slot_count: u32,
        maximum_ship_slot_equipment_type_count: u32,
        maximum_ship_skill_count: u32,
        maximum_fleet_team_ship_count: u32,
        maximum_ship_fleet_membership_count: u32,
        maximum_enhance_material_count: u32,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SharedFrames {
        handshake_response: String,
        snapshot_bag_request: String,
        unknown_operation_request: String,
        health_response: String,
        error_response: String,
    }

    fn shared_contract() -> SharedContract {
        serde_json::from_str(SHARED_CONTRACT_JSON).expect("shared runtime RPC contract must parse")
    }

    fn runtime_limit(value: usize) -> u32 {
        u32::try_from(value).expect("runtime RPC limits must fit the native uint32 contract")
    }

    #[test]
    fn shared_constants_match_rust_runtime() {
        let contract = shared_contract();

        assert!(!contract.description.trim().is_empty());
        assert_eq!(contract.schema_version, 1);
        assert_eq!(
            contract
                .operations
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            RPC_OPERATION_NAMES
        );
        let encoded: Vec<String> = [
            RpcOperation::Health,
            RpcOperation::Capabilities,
            RpcOperation::SnapshotBag,
            RpcOperation::SnapshotResources,
            RpcOperation::SnapshotOwnedState,
            RpcOperation::QueryOwned,
            RpcOperation::SnapshotShipDetails,
            RpcOperation::SnapshotAccountBefore,
            RpcOperation::SnapshotShipCatalog,
            RpcOperation::SnapshotEquipmentConfigs,
            RpcOperation::SnapshotComposeRecipes,
            RpcOperation::SnapshotEquipmentWeapons,
            RpcOperation::SnapshotSkillEffects,
            RpcOperation::SnapshotEquipmentReferenceNames,
            RpcOperation::ExecuteEquipmentCommand,
            RpcOperation::QueryEquipmentCommand,
            RpcOperation::CancelEquipmentCommand,
            RpcOperation::Shutdown,
        ]
        .into_iter()
        .map(|operation| {
            serde_json::to_value(operation)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
        assert_eq!(encoded, RPC_OPERATION_NAMES);
        assert_eq!(contract.constants.protocol_version, PROTOCOL_VERSION);
        assert_eq!(contract.constants.agent_version, EXPECTED_AGENT_VERSION);
        assert_eq!(contract.constants.minimum_timeout_ms, MIN_TIMEOUT_MS);
        assert_eq!(contract.constants.maximum_timeout_ms, MAX_TIMEOUT_MS);
        assert_eq!(
            contract.constants.maximum_handshake_attempts,
            MAX_HANDSHAKE_ATTEMPTS
        );
        assert_eq!(
            contract.constants.unauthenticated_probe_connections,
            UNAUTHENTICATED_PROBE_CONNECTIONS
        );
        assert_eq!(contract.constants.maximum_request_bytes, MAX_REQUEST_BYTES);
        assert_eq!(
            contract.constants.maximum_response_bytes,
            MAX_RESPONSE_BYTES
        );
        assert_eq!(
            contract.constants.maximum_snapshot_items,
            MAX_SNAPSHOT_ITEMS
        );
        assert_eq!(
            contract.constants.maximum_equipment_page_size,
            MAX_EQUIPMENT_PAGE_SIZE
        );
        assert_eq!(
            contract.constants.maximum_equipment_frame_size,
            MAX_EQUIPMENT_FRAME_SIZE
        );
        assert_eq!(
            contract.constants.maximum_equipment_catalog_items,
            MAX_EQUIPMENT_CATALOG_ITEMS
        );
        assert_eq!(
            contract.constants.maximum_ship_catalog_page_size,
            MAX_SHIP_CATALOG_PAGE_SIZE
        );
        assert_eq!(
            contract.constants.maximum_ship_catalog_frame_size,
            MAX_SHIP_CATALOG_FRAME_SIZE
        );
        assert_eq!(
            contract.constants.maximum_dock_page_size,
            MAX_DOCK_PAGE_SIZE
        );
        assert_eq!(
            contract.constants.maximum_ship_catalog_items,
            MAX_SHIP_CATALOG_ITEMS
        );
        assert_eq!(
            contract.constants.maximum_equipment_weapon_batch_size,
            runtime_limit(MAX_EQUIPMENT_WEAPON_BATCH_SIZE)
        );
        assert_eq!(
            contract.constants.maximum_skill_effect_batch_size,
            runtime_limit(MAX_SKILL_EFFECT_BATCH_SIZE)
        );
        assert_eq!(
            contract.constants.maximum_equipment_reference_batch_size,
            runtime_limit(MAX_EQUIPMENT_REFERENCE_BATCH_SIZE)
        );
        assert_eq!(
            contract.constants.ship_equipment_slot_count,
            runtime_limit(SHIP_EQUIPMENT_SLOT_COUNT)
        );
        assert_eq!(
            contract.constants.maximum_ship_slot_equipment_type_count,
            runtime_limit(MAX_SHIP_SLOT_EQUIPMENT_TYPES)
        );
        assert_eq!(
            contract.constants.maximum_ship_skill_count,
            runtime_limit(MAX_SHIP_SKILLS)
        );
        assert_eq!(
            contract.constants.maximum_fleet_team_ship_count,
            MAX_FLEET_TEAM_SHIPS
        );
        assert_eq!(
            contract.constants.maximum_ship_fleet_membership_count,
            runtime_limit(MAX_SHIP_FLEET_MEMBERSHIPS)
        );
        assert_eq!(
            contract.constants.maximum_enhance_material_count,
            runtime_limit(MAX_ENHANCE_MATERIALS)
        );
    }

    #[test]
    fn shared_frames_match_rust_envelopes() {
        let contract = shared_contract();
        let handshake_response: HandshakeResponse =
            serde_json::from_str(&contract.frames.handshake_response).unwrap();
        assert_eq!(
            serde_json::to_string(&handshake_response).unwrap(),
            contract.frames.handshake_response
        );

        let request = build_rpc_request(
            RequestId::FIRST,
            RpcOperation::SnapshotBag,
            5_000,
            SnapshotBagPayload::new(MAX_SNAPSHOT_ITEMS).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            contract.frames.snapshot_bag_request
        );

        let unknown_operation: Value =
            serde_json::from_str(&contract.frames.unknown_operation_request).unwrap();
        assert_eq!(
            unknown_operation,
            json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": "0000000000000003",
                "operation": "write_anything",
                "timeout_ms": 5_000,
                "payload": {},
            })
        );

        let health: RpcResponse<HealthResult> =
            serde_json::from_str(&contract.frames.health_response).unwrap();
        assert_eq!(
            serde_json::to_value(health).unwrap(),
            serde_json::from_str::<Value>(&contract.frames.health_response).unwrap()
        );

        let failure: RpcResponse<Value> =
            serde_json::from_str(&contract.frames.error_response).unwrap();
        assert_eq!(
            serde_json::to_value(failure).unwrap(),
            serde_json::from_str::<Value>(&contract.frames.error_response).unwrap()
        );
    }

    #[test]
    fn request_keeps_exact_wire_shape() {
        let request = build_rpc_request(
            RequestId::FIRST,
            RpcOperation::Health,
            1_000,
            EmptyPayload::default(),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "operation": "health",
                "timeout_ms": 1_000,
                "payload": {},
            })
        );
    }

    #[test]
    fn response_keeps_exact_success_and_error_wire_shapes() {
        let success: RpcResponse<Value> = serde_json::from_value(json!({
            "status": "ok",
            "protocol_version": PROTOCOL_VERSION,
            "request_id": "0000000000000001",
            "result": {"ready": true},
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(success).unwrap(),
            json!({
                "status": "ok",
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "result": {"ready": true},
            })
        );

        let failure: RpcResponse<Value> = serde_json::from_value(json!({
            "status": "error",
            "protocol_version": PROTOCOL_VERSION,
            "request_id": "0000000000000001",
            "error": {
                "code": "fixture_failed",
                "stage": "fixture.execute",
                "message": "样本执行失败",
                "retry": "same_request",
                "session_effect": "unchanged",
                "details": {"reason": "fixture"},
            },
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(failure).unwrap(),
            json!({
                "status": "error",
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "error": {
                    "code": "fixture_failed",
                    "stage": "fixture.execute",
                    "message": "样本执行失败",
                    "retry": "same_request",
                    "session_effect": "unchanged",
                    "details": {"reason": "fixture"},
                },
            })
        );
    }

    #[test]
    fn response_rejects_unknown_envelope_fields() {
        let error = serde_json::from_value::<RpcResponse<Value>>(json!({
            "status": "ok",
            "protocol_version": PROTOCOL_VERSION,
            "request_id": "0000000000000001",
            "result": {},
            "unexpected": true,
        }))
        .unwrap_err();

        assert!(error.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn error_requires_details_and_rejects_unknown_fields() {
        let missing_details = serde_json::from_value::<RpcResponse<Value>>(json!({
            "status": "error",
            "protocol_version": PROTOCOL_VERSION,
            "request_id": "0000000000000001",
            "error": {
                "code": "fixture_failed",
                "stage": "fixture.execute",
                "message": "样本执行失败",
                "retry": "never",
                "session_effect": "must_close",
            },
        }))
        .unwrap_err();
        assert!(
            missing_details
                .to_string()
                .contains("missing field `details`")
        );

        let unknown_field = serde_json::from_value::<AgentError>(json!({
            "code": "fixture_failed",
            "stage": "fixture.execute",
            "message": "样本执行失败",
            "retry": "never",
            "session_effect": "must_close",
            "details": {},
            "unexpected": true,
        }))
        .unwrap_err();
        assert!(
            unknown_field
                .to_string()
                .contains("unknown field `unexpected`")
        );

        let valid = AgentError {
            code: "fixture_failed".to_owned(),
            stage: "fixture.execute".to_owned(),
            message: "样本执行失败".to_owned(),
            retry: RetryDirective::Never,
            session_effect: SessionEffect::MustClose,
            details: BTreeMap::new(),
        };
        assert!(validate_agent_error(&valid).is_ok());
    }
}

/// 要求字段必须出现，同时允许其值显式为 `null`。
fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod capability_contract_tests {
    use std::collections::BTreeMap;

    use super::{
        CapabilitiesResult, CapabilityStatus, REQUIRED_CAPABILITY_KEYS, RuntimeProtocolError,
    };

    fn ready_capabilities() -> CapabilitiesResult {
        CapabilitiesResult {
            capabilities: REQUIRED_CAPABILITY_KEYS
                .into_iter()
                .map(|key| {
                    (
                        key.to_owned(),
                        CapabilityStatus {
                            available: true,
                            reason_code: "ready".to_owned(),
                            evidence: vec!["fixture".to_owned()],
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn validation_code(capabilities: CapabilitiesResult) -> &'static str {
        let error: RuntimeProtocolError =
            super::validate_capabilities_result(&capabilities).unwrap_err();
        error.code
    }

    #[test]
    fn preserves_capability_key_error_code() {
        let mut capabilities = ready_capabilities();
        capabilities.capabilities.insert(
            "invalid".to_owned(),
            CapabilityStatus {
                available: true,
                reason_code: "ready".to_owned(),
                evidence: vec!["fixture".to_owned()],
            },
        );

        assert_eq!(validation_code(capabilities), "capability_key_invalid");
    }

    #[test]
    fn preserves_capability_reason_error_code() {
        let mut capabilities = ready_capabilities();
        capabilities
            .capabilities
            .get_mut("read.bag")
            .unwrap()
            .reason_code = "Ready".to_owned();

        assert_eq!(validation_code(capabilities), "stable_token_invalid");
    }

    #[test]
    fn preserves_missing_capability_evidence_error_code() {
        let mut capabilities = ready_capabilities();
        capabilities
            .capabilities
            .get_mut("read.bag")
            .unwrap()
            .evidence
            .clear();

        assert_eq!(validation_code(capabilities), "capability_evidence_missing");
    }

    #[test]
    fn preserves_invalid_capability_evidence_error_code() {
        let mut capabilities = ready_capabilities();
        capabilities
            .capabilities
            .get_mut("read.bag")
            .unwrap()
            .evidence[0] = "  ".to_owned();

        assert_eq!(validation_code(capabilities), "text_field_invalid");
    }

    #[test]
    fn preserves_entry_policy_validation_order() {
        let mut capabilities = ready_capabilities();
        capabilities
            .capabilities
            .get_mut("write.compose")
            .unwrap()
            .reason_code = "compose_recipes_not_ready".to_owned();
        capabilities.capabilities.insert(
            "zz.invalid".to_owned(),
            CapabilityStatus {
                available: true,
                reason_code: "Invalid".to_owned(),
                evidence: vec!["fixture".to_owned()],
            },
        );

        assert_eq!(
            validation_code(capabilities),
            "equipment_write_capability_invalid"
        );
    }
}

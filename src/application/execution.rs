//! 定义配装计划的同会话执行契约、状态机和逐步回读核验。

pub(in crate::application) mod persistence;
pub(in crate::application) mod workbook;

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use suzushiro_content_digest::sha256_compact_json as stable_json_sha256;
use suzushiro_target_core::TargetFingerprint;
#[cfg(any(target_os = "windows", test))]
use suzushiro_text_format::is_canonical_sha256;

use super::{
    AppError, AppErrorCode, GameObservation, GamePort, PlanEnhanceCost, PlanEquipment, PlanSlot,
    PlanSource, PlanStep,
};
use crate::domain::GameState;

mod preflight;
mod readback;
mod state_machine;
#[cfg(test)]
mod test_fixtures;

#[cfg(test)]
pub(crate) use state_machine::execute_plan;
pub(crate) use state_machine::execute_plan_from_state_with_outcome;

/// 报告与独立最终回读分离，避免持久化报告携带完整游戏快照。
pub(crate) struct ExecutionOutcome {
    pub(crate) report: ExecutionReport,
    pub(crate) final_state: Option<GameObservation>,
}
#[cfg(test)]
pub(crate) use test_fixtures::{
    execution_workbook_compose_report_fixture, execution_workbook_dismantle_report_fixture,
    execution_workbook_enhance_report_fixture, execution_workbook_mismatch_report_fixture,
    execution_workbook_report_fixture,
};

/// 配装执行报告及命令摘要的稳定契约版本。
pub const EXECUTION_SCHEMA_VERSION: u32 = 4;

/// 主机只等待设备端已有命令收敛，不在该时限内重发写请求。
const COMMAND_RECEIPT_POLL_TIMEOUT: Duration = Duration::from_secs(15);

/// 查询间隔给游戏主线程留出处理服务器回包和更新本地代理状态的帧。
const COMMAND_RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 经过适配器认证的稳定执行目标，只保存设备、包、运行模块与舰船归属的 SHA-256 指纹。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionTargetIdentity {
    fingerprint_sha256: TargetFingerprint,
}

impl ExecutionTargetIdentity {
    /// 从适配器生成的稳定运行范围指纹建立目标身份。
    pub fn new(fingerprint_sha256: impl Into<String>) -> Result<Self, AppError> {
        let fingerprint_sha256 = fingerprint_sha256.into();
        let fingerprint_length = fingerprint_sha256.len();
        if let Ok(fingerprint_sha256) = TargetFingerprint::new(fingerprint_sha256) {
            return Ok(Self { fingerprint_sha256 });
        }
        Err(AppError::from_source(
            "execution.target_identity",
            AppErrorCode::RuntimeIncompatible,
            "执行目标身份必须是 64 位小写十六进制 SHA-256",
            std::io::Error::other("execution target fingerprint is not canonical SHA-256"),
        )
        .with_context("fingerprint_length", fingerprint_length.to_string()))
    }

    /// 返回不会暴露设备序列号或舰船归属明细原文的稳定目标指纹。
    pub fn fingerprint_sha256(&self) -> &str {
        self.fingerprint_sha256.as_str()
    }

    /// 从已认证设备范围和完整状态派生不受计划内装备变化影响的执行目标身份。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn from_runtime_scope(
        device_serial: &str,
        package_name: &str,
        state: &GameState,
    ) -> Result<Self, AppError> {
        if device_serial.is_empty() || package_name.is_empty() {
            return Err(target_identity_input_error(
                "设备序列号和游戏包名不能为空",
                "execution target device scope is incomplete",
            ));
        }
        if !is_canonical_sha256(state.source().module_sha256()) {
            return Err(target_identity_input_error(
                "完整状态缺少规范的运行模块身份",
                "game state module digest is not canonical SHA-256",
            ));
        }

        let mut ships: Vec<ExecutionTargetShip> = state
            .ships()
            .ships()
            .iter()
            .map(|ship| ExecutionTargetShip {
                instance_id: ship.identity().instance_id().get(),
                config_id: ship.identity().config_id(),
            })
            .collect();
        ships.sort_unstable_by_key(|ship| ship.instance_id);
        if ships
            .windows(2)
            .any(|pair| pair[0].instance_id == pair[1].instance_id)
        {
            return Err(target_identity_input_error(
                "完整状态包含重复的舰船实例",
                "game state contains duplicate ship instance identifiers",
            ));
        }

        let digest = execution_digest(
            "execution.target_identity",
            &ExecutionTargetIdentityDigestInput {
                schema_version: 2,
                device_serial,
                package_name,
                module_sha256: state.source().module_sha256(),
                ships,
            },
        )?;
        Self::new(digest)
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Serialize)]
struct ExecutionTargetIdentityDigestInput<'a> {
    schema_version: u32,
    device_serial: &'a str,
    package_name: &'a str,
    module_sha256: &'a str,
    ships: Vec<ExecutionTargetShip>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Serialize)]
struct ExecutionTargetShip {
    instance_id: u64,
    config_id: u64,
}

#[cfg(any(target_os = "windows", test))]
fn target_identity_input_error(message: &'static str, detail: &'static str) -> AppError {
    AppError::from_source(
        "execution.target_identity",
        AppErrorCode::RuntimeIncompatible,
        message,
        std::io::Error::other(detail),
    )
}

/// 与工作簿执行结果枚举保持一致的稳定状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionStatus {
    /// 命令和回读条件均已确认。
    Success,
    /// 命令明确失败，或回读状态不满足计划。
    Failed,
    /// 命令可能已经生效，但当前证据不足以确认最终结果。
    Unknown,
    /// 因取消或前序步骤停止而没有发送命令。
    NotExecuted,
}

/// 整份计划的聚合结论，与单步“是否发送”状态分开表达。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionReportStatus {
    /// 全部步骤以及最终装备和资源状态均已确认。
    Success,
    /// 至少一个步骤或最终装备、资源状态明确失败。
    Failed,
    /// 可能已经发生写入，但最终状态无法确认。
    Unknown,
    /// 调用方停止了尚未完成的计划。
    Cancelled,
}

impl ExecutionReportStatus {
    /// 返回 JSON、日志和后续工作簿摘要使用的稳定值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
        }
    }
}

impl ExecutionStatus {
    /// 返回工作簿、日志和 JSON 共用的稳定枚举值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::NotExecuted => "not_executed",
        }
    }
}

/// 一次顺序执行停止的稳定原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionStopReason {
    /// 全部步骤均已完成。
    Completed,
    /// 调用方在步骤边界请求取消。
    Cancelled,
    /// 命令在发送前失败或返回明确失败状态。
    CommandFailed,
    /// 命令可能已经发送，但查询后仍不能确认最终状态。
    CommandUnknown,
    /// 已确认命令成功，但无法取得必要的回读状态。
    ReadbackFailed,
    /// 回读成功，但实际装备或资源状态不符合计划步骤。
    ReadbackMismatch,
    /// 每一步均已完成，但最终装备或资源全量对账不符合计划。
    FinalStateMismatch,
    /// 步骤循环已经结束，但独立终态读取失败。
    FinalReadbackFailed,
}

impl ExecutionStopReason {
    /// 返回工作簿、日志和 JSON 共用的稳定枚举值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::CommandFailed => "command_failed",
            Self::CommandUnknown => "command_unknown",
            Self::ReadbackFailed => "readback_failed",
            Self::ReadbackMismatch => "readback_mismatch",
            Self::FinalStateMismatch => "final_state_mismatch",
            Self::FinalReadbackFailed => "final_readback_failed",
        }
    }
}

/// 运行态适配器能够执行的写动作；保持步骤不会转换成命令。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionAction {
    /// 清空一条舰船槽位。
    Unequip { slot: PlanSlot },
    /// 将规划来源中的装备放入目标槽位。
    Equip {
        slot: PlanSlot,
        source: PlanSource,
        equipment: PlanEquipment,
    },
    /// 销毁已经进入仓库的指定装备数量。
    Dismantle {
        source: PlanSource,
        equipment: PlanEquipment,
        quantity: u64,
    },
    /// 按已校验配方消耗资源并向仓库增加指定数量的装备。
    Compose {
        recipe_id: u64,
        equipment: PlanEquipment,
        quantity: u64,
        material_id: u64,
        material_quantity_per_unit: u64,
        gold_per_unit: u64,
    },
    /// 消耗已冻结的单级成本，把实际位置中的装备强化到相邻下一配置。
    Enhance {
        source: PlanSource,
        source_equipment: PlanEquipment,
        target_equipment: PlanEquipment,
        cost: PlanEnhanceCost,
    },
}

impl ExecutionAction {
    fn from_step(step: &PlanStep) -> Option<Self> {
        match step {
            PlanStep::Keep { .. } => None,
            PlanStep::Unequip { slot, .. } => Some(Self::Unequip { slot: *slot }),
            PlanStep::Dismantle {
                source,
                equipment,
                quantity,
                ..
            } => Some(Self::Dismantle {
                source: *source,
                equipment: *equipment,
                quantity: *quantity,
            }),
            PlanStep::Compose {
                recipe_id,
                equipment,
                quantity,
                material_id,
                material_quantity_per_unit,
                gold_per_unit,
                ..
            } => Some(Self::Compose {
                recipe_id: *recipe_id,
                equipment: *equipment,
                quantity: *quantity,
                material_id: *material_id,
                material_quantity_per_unit: *material_quantity_per_unit,
                gold_per_unit: *gold_per_unit,
            }),
            PlanStep::Enhance {
                source,
                source_equipment,
                target_equipment,
                cost,
                ..
            } => Some(Self::Enhance {
                source: *source,
                source_equipment: *source_equipment,
                target_equipment: *target_equipment,
                cost: cost.clone(),
            }),
            PlanStep::Equip {
                slot,
                source,
                equipment,
                ..
            } => Some(Self::Equip {
                slot: *slot,
                source: *source,
                equipment: *equipment,
            }),
        }
    }
}

/// 带确定性命令 ID 的单步执行请求。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionCommand {
    schema_version: u32,
    target_fingerprint_sha256: String,
    plan_hash: String,
    sequence: u32,
    pre_state_content_sha256: String,
    action: ExecutionAction,
    command_id: String,
}

impl ExecutionCommand {
    fn new(
        target_identity: &ExecutionTargetIdentity,
        plan_hash: &str,
        sequence: u32,
        pre_state_content_sha256: &str,
        action: ExecutionAction,
    ) -> Result<Self, AppError> {
        let digest_input = ExecutionCommandDigestInput {
            schema_version: EXECUTION_SCHEMA_VERSION,
            target_fingerprint_sha256: target_identity.fingerprint_sha256(),
            plan_hash,
            sequence,
            pre_state_content_sha256,
            action: &action,
        };
        let command_id = execution_digest("execution.command", &digest_input)?;
        Ok(Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            target_fingerprint_sha256: target_identity.fingerprint_sha256().to_owned(),
            plan_hash: plan_hash.to_owned(),
            sequence,
            pre_state_content_sha256: pre_state_content_sha256.to_owned(),
            action,
            command_id,
        })
    }

    /// 为适配器契约测试建立与生产执行器完全相同的确定性命令。
    #[cfg(test)]
    pub(crate) fn test_fixture(
        target_identity: &ExecutionTargetIdentity,
        plan_hash: &str,
        sequence: u32,
        pre_state_content_sha256: &str,
        action: ExecutionAction,
    ) -> Result<Self, AppError> {
        Self::new(
            target_identity,
            plan_hash,
            sequence,
            pre_state_content_sha256,
            action,
        )
    }

    /// 返回执行命令契约版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回命令必须作用于的稳定运行范围身份指纹。
    pub fn target_fingerprint_sha256(&self) -> &str {
        &self.target_fingerprint_sha256
    }

    /// 返回命令所属的不可变计划摘要。
    pub fn plan_hash(&self) -> &str {
        &self.plan_hash
    }

    /// 返回计划中的稳定步骤序号。
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// 返回主机发送前确认的完整状态摘要，用于同一会话内的审计、计划绑定和幂等标识。
    pub fn pre_state_content_sha256(&self) -> &str {
        &self.pre_state_content_sha256
    }

    /// 返回运行态适配器需要执行的动作。
    pub fn action(&self) -> ExecutionAction {
        self.action.clone()
    }

    /// 返回由计划、序号、前置状态和动作共同确定的幂等命令 ID。
    pub fn command_id(&self) -> &str {
        &self.command_id
    }
}

#[derive(Serialize)]
struct ExecutionCommandDigestInput<'a> {
    schema_version: u32,
    target_fingerprint_sha256: &'a str,
    plan_hash: &'a str,
    sequence: u32,
    pre_state_content_sha256: &'a str,
    action: &'a ExecutionAction,
}

/// 运行态对原命令最终状态的确认结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutionCommandReceipt {
    command_id: String,
    status: ExecutionStatus,
    response_summary: Option<String>,
    error_code: Option<String>,
    message: Option<String>,
    diagnostics: BTreeMap<String, String>,
}

impl ExecutionCommandReceipt {
    /// 建立发送、查询或取消操作返回的原命令状态。
    pub(crate) fn new(
        command_id: impl Into<String>,
        status: ExecutionStatus,
        response_summary: Option<String>,
        error_code: Option<String>,
        message: Option<String>,
        diagnostics: BTreeMap<String, String>,
    ) -> Self {
        Self {
            command_id: command_id.into(),
            status,
            response_summary,
            error_code,
            message,
            diagnostics,
        }
    }
}

/// 一次发送尝试的强类型边界，区分确定未发送和已经进入运行态的命令。
pub enum ExecutionSendResult {
    /// 命令确定没有进入运行态，因此不会产生任何写入。
    NotSent(AppError),
    /// 命令已经进入运行态；无法确认结果时回执必须使用 `Unknown`。
    Receipt(ExecutionCommandReceipt),
}

/// 整批执行预演中的一条写步骤。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionPreflightStep {
    action: ExecutionAction,
}

impl ExecutionPreflightStep {
    /// 返回适配器需要验证编码和能力支持的写动作。
    pub fn action(&self) -> ExecutionAction {
        self.action.clone()
    }
}

/// 在发送任何命令前交给适配器一次性检查的完整写步骤集合。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionPreflight {
    target_identity: ExecutionTargetIdentity,
    initial_state_content_sha256: String,
    steps: Vec<ExecutionPreflightStep>,
}

impl ExecutionPreflight {
    /// 返回目标设备、包、运行模块与舰船归属联合身份。
    pub const fn target_identity(&self) -> &ExecutionTargetIdentity {
        &self.target_identity
    }

    /// 返回预演所依据的真实完整状态摘要。
    pub fn initial_state_content_sha256(&self) -> &str {
        &self.initial_state_content_sha256
    }

    /// 返回按计划顺序排列的全部写步骤。
    pub fn steps(&self) -> &[ExecutionPreflightStep] {
        &self.steps
    }
}

/// 在一个经过身份认证的运行态会话中提供读取、整批预演、发送、查询和取消。
///
/// `send_command` 通过强类型结果区分确定未发送和已经进入运行态；发送后发生超时、断连或
/// 响应丢失时，适配器必须返回 `Unknown` 回执。`query_command` 与 `cancel_command` 返回的状态始终描述
/// 原命令，而不是查询或取消请求本身。明确的 `Failed` 必须表示命令未产生部分写入，
/// 否则同样应报告 `Unknown`。`pre_state_content_sha256` 绑定主机在同一会话中的重编译、
/// 审计和命令 ID；适配器必须把动作转换成明确的设备端局部前置条件，并在真正写入的同一
/// 主线程操作中先比较这些条件再派发通知。任一局部前置条件变化时必须在写入前明确拒绝，
/// 且不得产生部分写入。舰船来源在前序卸载后由同一会话按配置继续解析；端口在整个计划
/// 期间必须绑定同一认证目标和会话。
pub(crate) trait ExecutionPort: GamePort {
    /// 将当前活动连接锁定为本次执行会话；适配器必须拒绝后续读取切换认证会话。
    fn bind_current_session(&mut self) -> Result<(), AppError>;

    /// 使用刚从同一会话读取的完整状态派生位置无关的稳定目标指纹。
    fn target_identity(&mut self, state: &GameState) -> Result<ExecutionTargetIdentity, AppError>;

    /// 无状态修改地验证整批步骤的能力、状态来源和参数编码；任一步失败则整体失败。
    fn preflight_plan(&mut self, preflight: &ExecutionPreflight) -> Result<(), AppError>;

    /// 最多发送一次命令，不在端口内部进行无条件重试。
    fn send_command(&mut self, command: &ExecutionCommand) -> ExecutionSendResult;

    /// 查询一次原命令状态。`budget` 是本次调用允许阻塞的剩余时间，不得重发写命令。
    fn query_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<ExecutionCommandReceipt, AppError>;

    /// 请求取消原命令，并返回当前能够确认的原命令状态。
    fn cancel_command(
        &mut self,
        command_id: &str,
        budget: Duration,
    ) -> Result<ExecutionCommandReceipt, AppError>;
}

/// 调用方提供的协作式取消信号；执行器不会中断已经确认成功的回读。
pub trait ExecutionCancellation {
    /// 返回调用方是否请求停止发送后续命令，或收敛当前未知命令的取消状态。
    ///
    /// 执行器一旦观察到 `true`，本次执行便不会再发送新的命令；调用方无需维持单调信号。
    fn is_cancelled(&self) -> bool;
}

/// 永不请求取消的默认执行信号。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoExecutionCancellation;

impl ExecutionCancellation for NoExecutionCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// 回读证据中的一件装备，只保留稳定配置和强化等级。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionEquipmentState {
    config_id: u64,
    enhance_level: u8,
}

impl ExecutionEquipmentState {
    /// 返回装备配置 ID。
    pub const fn config_id(self) -> u64 {
        self.config_id
    }

    /// 返回强化等级。
    pub const fn enhance_level(self) -> u8 {
        self.enhance_level
    }
}

/// 某条舰船槽位在一次回读中的结构化状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionSlotState {
    /// 完整状态中缺少这条槽位。
    Missing,
    /// 槽位存在且为空。
    Empty,
    /// 槽位存在且装有一件确定装备。
    Equipped(ExecutionEquipmentState),
}

/// 一条相关舰船槽位在步骤前、预期步骤后和实际回读后的值。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionSlotReadback {
    slot: PlanSlot,
    before: ExecutionSlotState,
    expected_after: ExecutionSlotState,
    actual_after: ExecutionSlotState,
}

impl ExecutionSlotReadback {
    /// 返回被核对的舰船槽位。
    pub const fn slot(self) -> PlanSlot {
        self.slot
    }

    /// 返回步骤前已经确认的槽位状态。
    pub const fn before(self) -> ExecutionSlotState {
        self.before
    }

    /// 返回应用层完整模拟得到的预期槽位状态。
    pub const fn expected_after(self) -> ExecutionSlotState {
        self.expected_after
    }

    /// 返回运行态写后完整读取取得的实际槽位状态。
    pub const fn actual_after(self) -> ExecutionSlotState {
        self.actual_after
    }
}

/// 一个相关仓库配置在步骤前、预期步骤后和实际回读后的数量。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionWarehouseReadback {
    config_id: u64,
    before_quantity: u64,
    expected_after_quantity: u64,
    actual_after_quantity: u64,
}

impl ExecutionWarehouseReadback {
    /// 返回仓库装备配置 ID。
    pub const fn config_id(self) -> u64 {
        self.config_id
    }

    /// 返回步骤前数量。
    pub const fn before_quantity(self) -> u64 {
        self.before_quantity
    }

    /// 返回完整模拟得到的预期数量。
    pub const fn expected_after_quantity(self) -> u64 {
        self.expected_after_quantity
    }

    /// 返回写后完整读取取得的实际数量。
    pub const fn actual_after_quantity(self) -> u64 {
        self.actual_after_quantity
    }
}

/// 资源写步骤前、预期步骤后和实际回读后的物资数量。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionGoldReadback {
    before: u64,
    expected_after: u64,
    actual_after: u64,
}

impl ExecutionGoldReadback {
    /// 返回资源写入前的物资数量。
    pub const fn before(self) -> u64 {
        self.before
    }

    /// 返回按静态配方或拆解产物计算的预期物资数量。
    pub const fn expected_after(self) -> u64 {
        self.expected_after
    }

    /// 返回写后完整状态中的实际物资数量。
    pub const fn actual_after(self) -> u64 {
        self.actual_after
    }
}

/// 一种背包材料在资源写步骤前、预期步骤后和实际回读后的数量。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionMaterialReadback {
    item_id: u64,
    before_quantity: u64,
    expected_after_quantity: u64,
    actual_after_quantity: u64,
}

impl ExecutionMaterialReadback {
    /// 返回背包物品 ID。
    pub const fn item_id(self) -> u64 {
        self.item_id
    }

    /// 返回资源写入前数量。
    pub const fn before_quantity(self) -> u64 {
        self.before_quantity
    }

    /// 返回按静态配方或拆解产物计算的预期数量。
    pub const fn expected_after_quantity(self) -> u64 {
        self.expected_after_quantity
    }

    /// 返回写后完整状态中的实际数量。
    pub const fn actual_after_quantity(self) -> u64 {
        self.actual_after_quantity
    }
}

/// 装备或资源状态与预期状态的第一处稳定差异。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionStateMismatch {
    /// 舰船槽位缺失、意外为空或装备配置不符合预期。
    Slot {
        slot: PlanSlot,
        expected: ExecutionSlotState,
        actual: ExecutionSlotState,
    },
    /// 某个仓库配置的实际数量不符合预期。
    Warehouse {
        config_id: u64,
        expected_quantity: u64,
        actual_quantity: u64,
    },
    /// 物资没有按静态资源变化模型收敛。
    Gold {
        expected_quantity: u64,
        actual_quantity: u64,
    },
    /// 某种背包材料没有按静态资源变化模型收敛。
    Material {
        item_id: u64,
        expected_quantity: u64,
        actual_quantity: u64,
    },
}

/// 一条写步骤的结构化槽位、来源、仓库和全量差异证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionReadbackEvidence {
    matches_expected: bool,
    target_slot: Option<ExecutionSlotReadback>,
    source: Option<PlanSource>,
    source_slot: Option<ExecutionSlotReadback>,
    warehouse: Vec<ExecutionWarehouseReadback>,
    gold: Option<ExecutionGoldReadback>,
    materials: Vec<ExecutionMaterialReadback>,
    dismantled_source_quantity: Option<u64>,
    composed_output_quantity: Option<u64>,
    enhanced_target_quantity: Option<u64>,
    first_mismatch: Option<ExecutionStateMismatch>,
}

impl ExecutionReadbackEvidence {
    /// 返回实际装备和必要资源状态是否与该步骤的完整预期状态一致。
    pub const fn matches_expected(&self) -> bool {
        self.matches_expected
    }

    /// 返回目标槽位的结构化回读证据；纯库存动作没有目标槽位。
    pub const fn target_slot(&self) -> Option<ExecutionSlotReadback> {
        self.target_slot
    }

    /// 返回装备步骤使用的稳定来源引用。
    pub const fn source(&self) -> Option<PlanSource> {
        self.source
    }

    /// 返回舰船来源槽位的结构化证据；仓库来源和卸装步骤为空。
    pub const fn source_slot(&self) -> Option<ExecutionSlotReadback> {
        self.source_slot
    }

    /// 返回该动作直接相关的仓库数量证据。
    pub fn warehouse(&self) -> &[ExecutionWarehouseReadback] {
        &self.warehouse
    }

    /// 返回拆解、合成或强化步骤的物资回读证据；其他动作为空。
    pub const fn gold(&self) -> Option<ExecutionGoldReadback> {
        self.gold
    }

    /// 返回拆解、合成或强化步骤涉及或发生偏差的背包材料证据。
    pub fn materials(&self) -> &[ExecutionMaterialReadback] {
        &self.materials
    }

    /// 返回命令请求拆解的来源数量；实际变化仍以仓库和资源回读为准。
    pub const fn dismantled_source_quantity(&self) -> Option<u64> {
        self.dismantled_source_quantity
    }

    /// 返回命令请求合成的产物数量；实际变化仍以仓库和资源回读为准。
    pub const fn composed_output_quantity(&self) -> Option<u64> {
        self.composed_output_quantity
    }

    /// 返回命令请求强化的目标装备数量；单级强化命令固定为一件。
    pub const fn enhanced_target_quantity(&self) -> Option<u64> {
        self.enhanced_target_quantity
    }

    /// 返回装备或资源状态中的第一处差异。
    pub const fn first_mismatch(&self) -> Option<&ExecutionStateMismatch> {
        self.first_mismatch.as_ref()
    }
}

/// 一条命令是否可能、实际或完整地改变了游戏状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionWriteEffect {
    /// 端口证明未产生写入，或该计划步骤本身不发送命令。
    None,
    /// 命令可能产生写入，但当前证据不能证明是否发生。
    Possible,
    /// 回读证明状态发生变化，但变化不符合完整模拟结果。
    StateChangedMismatch,
    /// 命令回执仍未知，但独立终态观察到该步骤的完整预期后态。
    ExpectedPostStateObserved,
    /// 回读证明装备和必要资源状态与完整模拟结果一致。
    Verified,
}

impl ExecutionWriteEffect {
    /// 返回工作簿、日志和 JSON 共用的稳定枚举值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Possible => "possible",
            Self::StateChangedMismatch => "state_changed_mismatch",
            Self::ExpectedPostStateObserved => "expected_post_state_observed",
            Self::Verified => "verified",
        }
    }

    const fn may_have_write(self) -> bool {
        !matches!(self, Self::None)
    }

    const fn observed_state_change(self) -> bool {
        matches!(
            self,
            Self::StateChangedMismatch | Self::ExpectedPostStateObserved | Self::Verified
        )
    }
}

/// 独立终态读取相对于计划或已确认进度的结构化核验状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExecutionFinalVerificationStatus {
    /// 完整计划的最终装备和资源状态与模拟结果一致。
    Verified,
    /// 独立终态与完整计划或已确认进度不一致。
    Mismatch,
    /// 因计划提前停止，终态只能作为未完成执行的证据。
    Incomplete,
    /// 无法取得独立终态。
    Unavailable,
    /// 命令回执仍未知，但独立终态与该步骤的预期后态一致。
    UnconfirmedStepReached,
    /// 命令回执仍未知，且独立终态仍与该步骤的前态一致。
    UnconfirmedStepNotObserved,
}

impl ExecutionFinalVerificationStatus {
    /// 返回工作簿、日志和 JSON 共用的稳定枚举值。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Mismatch => "mismatch",
            Self::Incomplete => "incomplete",
            Self::Unavailable => "unavailable",
            Self::UnconfirmedStepReached => "unconfirmed_step_reached",
            Self::UnconfirmedStepNotObserved => "unconfirmed_step_not_observed",
        }
    }
}

/// 一条计划步骤的命令、响应和回读证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionStepResult {
    sequence: u32,
    step_kind: &'static str,
    step: PlanStep,
    status: ExecutionStatus,
    command_id: Option<String>,
    pre_state_content_sha256: Option<String>,
    post_state_content_sha256: Option<String>,
    request_summary: Option<String>,
    response_summary: Option<String>,
    readback_summary: Option<String>,
    readback_evidence: Option<ExecutionReadbackEvidence>,
    write_acknowledged: bool,
    write_effect: ExecutionWriteEffect,
    error_code: Option<String>,
    message: String,
    diagnostics: BTreeMap<String, String>,
}

impl ExecutionStepResult {
    /// 返回计划步骤序号。
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// 返回步骤的稳定类型。
    pub const fn step_kind(&self) -> &'static str {
        self.step_kind
    }

    /// 返回原始不可变计划步骤。
    pub fn step(&self) -> PlanStep {
        self.step.clone()
    }

    /// 返回该步骤最终能够确认的状态。
    pub const fn status(&self) -> ExecutionStatus {
        self.status
    }

    /// 返回写步骤的确定性命令 ID；保持和未执行步骤为空。
    pub fn command_id(&self) -> Option<&str> {
        self.command_id.as_deref()
    }

    /// 返回执行该步骤前最后确认的完整状态摘要。
    pub fn pre_state_content_sha256(&self) -> Option<&str> {
        self.pre_state_content_sha256.as_deref()
    }

    /// 返回步骤后最后一次全量观察到的状态摘要；是否完成确认需结合状态和写入影响判断。
    pub fn post_state_content_sha256(&self) -> Option<&str> {
        self.post_state_content_sha256.as_deref()
    }

    /// 返回发送给运行态的稳定请求摘要。
    pub fn request_summary(&self) -> Option<&str> {
        self.request_summary.as_deref()
    }

    /// 返回运行态回执摘要。
    pub fn response_summary(&self) -> Option<&str> {
        self.response_summary.as_deref()
    }

    /// 返回回读校验摘要。
    pub fn readback_summary(&self) -> Option<&str> {
        self.readback_summary.as_deref()
    }

    /// 返回可以直接映射到工作簿执行结果的结构化回读证据。
    pub const fn readback_evidence(&self) -> Option<&ExecutionReadbackEvidence> {
        self.readback_evidence.as_ref()
    }

    /// 返回运行态是否明确确认原命令成功。
    pub const fn write_acknowledged(&self) -> bool {
        self.write_acknowledged
    }

    /// 返回当前证据能够确认的写入影响。
    pub const fn write_effect(&self) -> ExecutionWriteEffect {
        self.write_effect
    }

    /// 返回适配器或应用层提供的稳定错误码。
    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }

    /// 返回面向用户的步骤结论。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 返回按键稳定排序的诊断上下文。
    pub const fn diagnostics(&self) -> &BTreeMap<String, String> {
        &self.diagnostics
    }
}

/// 一份不可变计划的顺序执行和回读闭环报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionReport {
    schema_version: u32,
    plan_schema_version: u32,
    target_identity: ExecutionTargetIdentity,
    plan_hash: String,
    initial_state_content_sha256: String,
    final_state_content_sha256: Option<String>,
    status: ExecutionReportStatus,
    stop_reason: ExecutionStopReason,
    acknowledged_write_count: usize,
    observed_state_change_count: usize,
    verified_write_count: usize,
    may_have_writes: bool,
    final_verification_status: ExecutionFinalVerificationStatus,
    final_verification_summary: Option<String>,
    steps: Vec<ExecutionStepResult>,
    content_sha256: String,
}

impl ExecutionReport {
    /// 返回执行报告契约版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回被执行计划的契约版本。
    pub const fn plan_schema_version(&self) -> u32 {
        self.plan_schema_version
    }

    /// 返回本次执行绑定的设备、包、运行模块与舰船归属联合身份。
    pub const fn target_identity(&self) -> &ExecutionTargetIdentity {
        &self.target_identity
    }

    /// 返回被执行计划的稳定摘要。
    pub fn plan_hash(&self) -> &str {
        &self.plan_hash
    }

    /// 返回执行前从同一会话读取的完整状态摘要。
    pub fn initial_state_content_sha256(&self) -> &str {
        &self.initial_state_content_sha256
    }

    /// 返回最后一次独立全量读取的状态摘要；读取失败时为空，命令未知时也可能存在。
    pub fn final_state_content_sha256(&self) -> Option<&str> {
        self.final_state_content_sha256.as_deref()
    }

    /// 返回整份执行报告的最终状态。
    pub const fn status(&self) -> ExecutionReportStatus {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn with_status(mut self, status: ExecutionReportStatus) -> Self {
        self.status = status;
        self
    }

    /// 返回执行停止原因。
    pub const fn stop_reason(&self) -> ExecutionStopReason {
        self.stop_reason
    }

    /// 返回运行态明确确认成功的写命令数量，不等同于完整回读通过数量。
    pub const fn acknowledged_write_count(&self) -> usize {
        self.acknowledged_write_count
    }

    /// 返回回读证明装备语义状态确实发生变化的步骤数量。
    pub const fn observed_state_change_count(&self) -> usize {
        self.observed_state_change_count
    }

    /// 返回完整步骤预期通过全量回读的写命令数量。
    pub const fn verified_write_count(&self) -> usize {
        self.verified_write_count
    }

    /// 返回本次执行是否存在任何已经发生或尚不能排除的写入。
    pub const fn may_have_writes(&self) -> bool {
        self.may_have_writes
    }

    /// 返回独立终态相对于计划或已确认进度的结构化核验状态。
    pub const fn final_verification_status(&self) -> ExecutionFinalVerificationStatus {
        self.final_verification_status
    }

    /// 返回全部步骤结束后的装备、仓库和资源对账结论。
    pub fn final_verification_summary(&self) -> Option<&str> {
        self.final_verification_summary.as_deref()
    }

    /// 返回与计划一一对应的步骤结果。
    pub fn steps(&self) -> &[ExecutionStepResult] {
        &self.steps
    }

    /// 返回不包含自身字段的报告内容 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

#[derive(Serialize)]
struct ExecutionReportDigestInput<'a> {
    schema_version: u32,
    plan_schema_version: u32,
    target_identity: &'a ExecutionTargetIdentity,
    plan_hash: &'a str,
    initial_state_content_sha256: &'a str,
    final_state_content_sha256: &'a Option<String>,
    status: ExecutionReportStatus,
    stop_reason: ExecutionStopReason,
    acknowledged_write_count: usize,
    observed_state_change_count: usize,
    verified_write_count: usize,
    may_have_writes: bool,
    final_verification_status: ExecutionFinalVerificationStatus,
    final_verification_summary: &'a Option<String>,
    steps: &'a [ExecutionStepResult],
}

/// 将写动作格式化为稳定的人类可读请求摘要。
fn request_summary(action: &ExecutionAction) -> String {
    match action {
        ExecutionAction::Unequip { slot } => format!(
            "卸下舰船 {} 槽位 {} 的装备",
            slot.ship_instance_id(),
            slot.slot_index()
        ),
        ExecutionAction::Equip {
            slot,
            source,
            equipment,
        } => format!(
            "从 {} 装备配置 {}、强化等级 {} 到舰船 {} 槽位 {}",
            source_summary(*source),
            equipment.config_id(),
            equipment.enhance_level(),
            slot.ship_instance_id(),
            slot.slot_index()
        ),
        ExecutionAction::Dismantle {
            source,
            equipment,
            quantity,
        } => format!(
            "从 {} 拆解 {} 件配置 {}、强化等级 {} 的装备",
            source_summary(*source),
            quantity,
            equipment.config_id(),
            equipment.enhance_level()
        ),
        ExecutionAction::Compose {
            recipe_id,
            equipment,
            quantity,
            ..
        } => format!(
            "按配方 {recipe_id} 合成 {quantity} 件配置 {} 的装备",
            equipment.config_id()
        ),
        ExecutionAction::Enhance {
            source,
            source_equipment,
            target_equipment,
            ..
        } => format!(
            "在{}将配置 {}、强化等级 {} 提升为配置 {}、强化等级 {}",
            source_summary(*source),
            source_equipment.config_id(),
            source_equipment.enhance_level(),
            target_equipment.config_id(),
            target_equipment.enhance_level()
        ),
    }
}

fn source_summary(source: PlanSource) -> String {
    match source {
        PlanSource::Warehouse { config_id } => format!("仓库配置 {config_id}"),
        PlanSource::ShipSlot {
            ship_instance_id,
            slot_index,
        } => format!("舰船 {ship_instance_id} 槽位 {slot_index}"),
        PlanSource::Compose { recipe_id } => format!("合成配方 {recipe_id}"),
    }
}

fn error_diagnostics(error: &AppError) -> BTreeMap<String, String> {
    let mut diagnostics = BTreeMap::from([("stage".to_owned(), error.stage().to_owned())]);
    diagnostics.extend(
        error
            .context()
            .iter()
            .map(|(key, value)| (format!("context.{key}"), value.clone())),
    );
    diagnostics
}

fn state_changed_error(expected: &str, actual: &str) -> AppError {
    AppError::from_source(
        "plan.execute.precondition",
        AppErrorCode::EquipmentStateChanged,
        "执行前游戏状态已发生变化，请重新读取并检查计划",
        std::io::Error::other("execution state digest does not match compiled plan"),
    )
    .with_context("expected_state_content_sha256", expected)
    .with_context("actual_state_content_sha256", actual)
}

fn execution_digest<T: Serialize>(stage: &'static str, value: &T) -> Result<String, AppError> {
    stable_json_sha256(value).map_err(|source| {
        AppError::from_source(
            stage,
            AppErrorCode::RuntimeIncompatible,
            "执行契约摘要编码失败",
            source,
        )
    })
}

#[cfg(test)]
mod tests;

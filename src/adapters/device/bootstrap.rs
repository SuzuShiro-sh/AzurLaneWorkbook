//! Rust 宿主与原生 loader 共用的固定长度启动配置编码器。

use thiserror::Error;

use super::session::{SessionId, SessionSecret};
use crate::adapters::settings::{AgentMappingMode, AgentVisibilityMode};
use suzushiro_text_format::is_canonical_sha256;

// 与原生 `BootstrapConfigV2` 同步冻结的总长度、字段容量和输入边界。
const BOOTSTRAP_SIZE: usize = 288;
const BOOTSTRAP_MAGIC: &[u8; 8] = b"AZLWCFG2";
const BOOTSTRAP_VERSION: u32 = 2;
const MIN_TIMEOUT_MS: u32 = 1;
const MAX_TIMEOUT_MS: u32 = 30_000;
const SESSION_ID_SIZE: usize = 16;
const SESSION_SECRET_SIZE: usize = 32;
const PACKAGE_CAPACITY: usize = 64;
const MODULE_CAPACITY: usize = 32;
const SHA256_TEXT_SIZE: usize = 64;
const PROLOGUE_SIZE: usize = 14;

// 单字节对齐结构中各字段的起始偏移，修改时必须同步原生静态断言。
const SCHEMA_VERSION_OFFSET: usize = 8;
const TOTAL_SIZE_OFFSET: usize = 12;
const TARGET_PID_OFFSET: usize = 16;
const TIMEOUT_MS_OFFSET: usize = 20;
const SESSION_ID_OFFSET: usize = 24;
const SESSION_SECRET_OFFSET: usize = 40;
const CHANNEL_ID_OFFSET: usize = 72;
const MAPPING_ID_OFFSET: usize = 88;
const PACKAGE_OFFSET: usize = 104;
const MODULE_OFFSET: usize = 168;
const SHA256_OFFSET: usize = 200;
const TARGET_SYMBOL_OFFSET: usize = 265;
const PROLOGUE_OFFSET: usize = 273;
const BOOTSTRAP_RUNTIME_POLICY_OFFSET: usize = 287;

// 与原生 `AgentUnloadConfigV1` 同步冻结的 512 字节布局。
#[cfg(any(target_os = "windows", test))]
const UNLOAD_SIZE: usize = 512;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_MAGIC: &[u8; 8] = b"AZLWUNL1";
#[cfg(any(target_os = "windows", test))]
const UNLOAD_VERSION: u32 = 1;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_PROCESS_START_OFFSET: usize = 40;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_PACKAGE_OFFSET: usize = 48;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_MODULE_OFFSET: usize = 112;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_MODULE_SHA256_OFFSET: usize = 144;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_AGENT_SHA256_OFFSET: usize = 209;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_MAPPING_OFFSET: usize = 274;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_MAPPING_CAPACITY: usize = 64;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_TARGET_SYMBOL_OFFSET: usize = 338;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_PROLOGUE_OFFSET: usize = 346;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_AGENT_HANDLE_OFFSET: usize = 360;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_AGENT_BASE_OFFSET: usize = 368;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_AGENT_LOAD_SIZE_OFFSET: usize = 376;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_FINALIZE_OFFSET: usize = 384;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_HOOK_TARGET_OFFSET: usize = 392;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_TRAMPOLINE_START_OFFSET: usize = 400;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_TRAMPOLINE_SIZE_OFFSET: usize = 408;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_WORKER_TID_OFFSET: usize = 416;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_WORKER_START_OFFSET: usize = 420;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_RUNTIME_POLICY_OFFSET: usize = 428;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_SOINFO_OFFSET: usize = 429;
#[cfg(any(target_os = "windows", test))]
const UNLOAD_PROTECTED_ELF_HEADER_OFFSET: usize = 437;
#[cfg(any(target_os = "windows", test))]
const ELF_HEADER_SIZE: usize = 64;

/// 已由加载收据和 shutdown 收据共同证明的卸载身份。
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeUnloadIdentity {
    pub process_start_time: u64,
    pub agent_handle: u64,
    pub agent_base: u64,
    pub agent_load_size: u64,
    pub finalize_address: u64,
    pub agent_mapping_name: String,
    pub hook_target: u64,
    pub trampoline_start: u64,
    pub trampoline_size: u64,
    pub rpc_worker_tid: u32,
    pub rpc_worker_start_time: u64,
    pub agent_mapping_mode: AgentMappingMode,
    pub agent_visibility_mode: AgentVisibilityMode,
    pub agent_soinfo_address: u64,
    pub protected_elf_header: [u8; ELF_HEADER_SIZE],
}

/// 已校验且可安全写入固定启动结构的游戏运行态配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeBootstrapProfile {
    package_name: String,
    module_name: String,
    module_sha256: String,
    target_symbol_offset: u64,
    expected_prologue: [u8; PROLOGUE_SIZE],
}

impl RuntimeBootstrapProfile {
    /// 构造只包含固定目标身份和已证明 Hook 位置的 profile。
    pub fn new(
        package_name: impl Into<String>,
        module_name: impl Into<String>,
        module_sha256: impl Into<String>,
        target_symbol_offset: u64,
        expected_prologue: [u8; PROLOGUE_SIZE],
    ) -> Result<Self, BootstrapEncodingError> {
        let package_name: String = package_name.into();
        let module_name: String = module_name.into();
        let module_sha256: String = module_sha256.into();

        validate_c_string("package_name", &package_name, PACKAGE_CAPACITY)?;
        validate_c_string("module_name", &module_name, MODULE_CAPACITY)?;
        validate_sha256(&module_sha256)?;
        if target_symbol_offset == 0 {
            return Err(BootstrapEncodingError::ZeroTargetSymbolOffset);
        }

        Ok(Self {
            package_name,
            module_name,
            module_sha256,
            target_symbol_offset,
            expected_prologue,
        })
    }

    /// 返回必须与目标进程 `cmdline` 完全一致的包名。
    pub fn package_name(&self) -> &str {
        &self.package_name
    }

    /// 返回需要在目标进程映射中定位的模块文件名。
    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    /// 返回 loader 必须在设备上重新核对的模块摘要。
    pub fn module_sha256(&self) -> &str {
        &self.module_sha256
    }

    /// 返回相对模块加载基址的目标函数偏移。
    pub fn target_symbol_offset(&self) -> u64 {
        self.target_symbol_offset
    }

    /// 返回安装 Hook 前必须逐字节匹配的函数前导。
    pub fn expected_prologue(&self) -> &[u8; PROLOGUE_SIZE] {
        &self.expected_prologue
    }
}

/// 按 `BootstrapConfigV2` 的小端、单字节对齐布局编码 288 字节启动文件。
/// 参数逐项对应冻结的原生协议字段，保留平铺签名便于审阅布局和拒绝边界。
#[allow(clippy::too_many_arguments)]
pub fn encode_bootstrap(
    target_pid: i32,
    timeout_ms: u32,
    session_id: SessionId,
    session_secret: &SessionSecret,
    channel_id: SessionId,
    mapping_id: SessionId,
    agent_mapping_mode: AgentMappingMode,
    agent_visibility_mode: AgentVisibilityMode,
    profile: &RuntimeBootstrapProfile,
) -> Result<[u8; BOOTSTRAP_SIZE], BootstrapEncodingError> {
    if target_pid <= 0 {
        return Err(BootstrapEncodingError::InvalidTargetPid { actual: target_pid });
    }
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(BootstrapEncodingError::InvalidTimeout { actual: timeout_ms });
    }

    let mut session_id_bytes: [u8; SESSION_ID_SIZE] = [0; SESSION_ID_SIZE];
    session_id.copy_bytes_to(&mut session_id_bytes);
    if session_id_bytes.iter().all(|value: &u8| *value == 0) {
        return Err(BootstrapEncodingError::ZeroSessionId);
    }

    let mut session_secret_bytes: [u8; SESSION_SECRET_SIZE] = [0; SESSION_SECRET_SIZE];
    session_secret.copy_bytes_to(&mut session_secret_bytes);
    if session_secret_bytes.iter().all(|value: &u8| *value == 0) {
        return Err(BootstrapEncodingError::ZeroSessionSecret);
    }

    let mut channel_id_bytes: [u8; SESSION_ID_SIZE] = [0; SESSION_ID_SIZE];
    channel_id.copy_bytes_to(&mut channel_id_bytes);
    if channel_id_bytes.iter().all(|value: &u8| *value == 0) {
        return Err(BootstrapEncodingError::ZeroChannelId);
    }

    let mut mapping_id_bytes: [u8; SESSION_ID_SIZE] = [0; SESSION_ID_SIZE];
    mapping_id.copy_bytes_to(&mut mapping_id_bytes);
    if mapping_id_bytes.iter().all(|value: &u8| *value == 0) {
        return Err(BootstrapEncodingError::ZeroMappingId);
    }

    let mut output: [u8; BOOTSTRAP_SIZE] = [0; BOOTSTRAP_SIZE];
    output[..BOOTSTRAP_MAGIC.len()].copy_from_slice(BOOTSTRAP_MAGIC);
    write_u32(&mut output, SCHEMA_VERSION_OFFSET, BOOTSTRAP_VERSION);
    write_u32(&mut output, TOTAL_SIZE_OFFSET, BOOTSTRAP_SIZE as u32);
    write_i32(&mut output, TARGET_PID_OFFSET, target_pid);
    write_u32(&mut output, TIMEOUT_MS_OFFSET, timeout_ms);
    output[SESSION_ID_OFFSET..SESSION_ID_OFFSET + SESSION_ID_SIZE]
        .copy_from_slice(&session_id_bytes);
    output[SESSION_SECRET_OFFSET..SESSION_SECRET_OFFSET + SESSION_SECRET_SIZE]
        .copy_from_slice(&session_secret_bytes);
    output[CHANNEL_ID_OFFSET..CHANNEL_ID_OFFSET + SESSION_ID_SIZE]
        .copy_from_slice(&channel_id_bytes);
    output[MAPPING_ID_OFFSET..MAPPING_ID_OFFSET + SESSION_ID_SIZE]
        .copy_from_slice(&mapping_id_bytes);
    write_c_string(&mut output, PACKAGE_OFFSET, &profile.package_name);
    write_c_string(&mut output, MODULE_OFFSET, &profile.module_name);
    output[SHA256_OFFSET..SHA256_OFFSET + SHA256_TEXT_SIZE]
        .copy_from_slice(profile.module_sha256.as_bytes());
    write_u64(
        &mut output,
        TARGET_SYMBOL_OFFSET,
        profile.target_symbol_offset,
    );
    output[PROLOGUE_OFFSET..PROLOGUE_OFFSET + PROLOGUE_SIZE]
        .copy_from_slice(&profile.expected_prologue);
    output[BOOTSTRAP_RUNTIME_POLICY_OFFSET] =
        encode_runtime_policy(agent_mapping_mode, agent_visibility_mode);

    // 原始会话密钥的临时副本不应继续驻留在宿主栈帧中。
    session_secret_bytes.fill(0);
    Ok(output)
}

/// 按原生紧凑布局编码不含密钥的一次性 Agent 卸载文件。
#[cfg(any(target_os = "windows", test))]
pub(crate) fn encode_unload(
    target_pid: i32,
    timeout_ms: u32,
    session_id: SessionId,
    profile: &RuntimeBootstrapProfile,
    agent_sha256: &str,
    identity: &RuntimeUnloadIdentity,
) -> Result<[u8; UNLOAD_SIZE], BootstrapEncodingError> {
    if target_pid <= 0 {
        return Err(BootstrapEncodingError::InvalidTargetPid { actual: target_pid });
    }
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(BootstrapEncodingError::InvalidTimeout { actual: timeout_ms });
    }
    validate_sha256(agent_sha256).map_err(|_| BootstrapEncodingError::InvalidAgentSha256)?;
    validate_c_string(
        "agent_mapping_name",
        &identity.agent_mapping_name,
        UNLOAD_MAPPING_CAPACITY,
    )?;
    if !identity.agent_mapping_name.starts_with("/memfd:") {
        return Err(BootstrapEncodingError::InvalidAgentMappingName);
    }
    if identity.process_start_time == 0
        || identity.agent_handle == 0
        || identity.agent_base == 0
        || identity.agent_load_size == 0
        || identity.finalize_address == 0
        || identity.hook_target == 0
        || identity.trampoline_start == 0
        || identity.trampoline_size == 0
        || identity.rpc_worker_tid == 0
        || identity.rpc_worker_tid > i32::MAX as u32
        || identity.rpc_worker_start_time == 0
    {
        return Err(BootstrapEncodingError::IncompleteUnloadIdentity);
    }
    if identity.rpc_worker_tid == target_pid as u32 {
        return Err(BootstrapEncodingError::WorkerIsMainThread);
    }
    let agent_end: u64 = identity
        .agent_base
        .checked_add(identity.agent_load_size)
        .ok_or(BootstrapEncodingError::InvalidUnloadRange)?;
    if !(identity.agent_base..agent_end).contains(&identity.finalize_address)
        || identity
            .trampoline_start
            .checked_add(identity.trampoline_size)
            .is_none()
    {
        return Err(BootstrapEncodingError::InvalidUnloadRange);
    }
    let protected_header_present: bool = identity
        .protected_elf_header
        .iter()
        .any(|value: &u8| *value != 0);
    let visibility_identity_valid: bool = match identity.agent_visibility_mode {
        AgentVisibilityMode::Normal => {
            identity.agent_soinfo_address == 0 && !protected_header_present
        }
        AgentVisibilityMode::SolistHidden => {
            identity.agent_soinfo_address != 0 && !protected_header_present
        }
        AgentVisibilityMode::SolistAndElfHeader => {
            identity.agent_soinfo_address != 0 && protected_header_present
        }
    };
    if !visibility_identity_valid {
        return Err(BootstrapEncodingError::InconsistentVisibilityIdentity);
    }

    let mut session_id_bytes: [u8; SESSION_ID_SIZE] = [0; SESSION_ID_SIZE];
    session_id.copy_bytes_to(&mut session_id_bytes);
    if session_id_bytes.iter().all(|value: &u8| *value == 0) {
        return Err(BootstrapEncodingError::ZeroSessionId);
    }

    let mut output: [u8; UNLOAD_SIZE] = [0; UNLOAD_SIZE];
    output[..UNLOAD_MAGIC.len()].copy_from_slice(UNLOAD_MAGIC);
    write_u32(&mut output, SCHEMA_VERSION_OFFSET, UNLOAD_VERSION);
    write_u32(&mut output, TOTAL_SIZE_OFFSET, UNLOAD_SIZE as u32);
    write_i32(&mut output, TARGET_PID_OFFSET, target_pid);
    write_u32(&mut output, TIMEOUT_MS_OFFSET, timeout_ms);
    output[SESSION_ID_OFFSET..SESSION_ID_OFFSET + SESSION_ID_SIZE]
        .copy_from_slice(&session_id_bytes);
    write_u64(
        &mut output,
        UNLOAD_PROCESS_START_OFFSET,
        identity.process_start_time,
    );
    write_c_string(&mut output, UNLOAD_PACKAGE_OFFSET, &profile.package_name);
    write_c_string(&mut output, UNLOAD_MODULE_OFFSET, &profile.module_name);
    output[UNLOAD_MODULE_SHA256_OFFSET..UNLOAD_MODULE_SHA256_OFFSET + SHA256_TEXT_SIZE]
        .copy_from_slice(profile.module_sha256.as_bytes());
    output[UNLOAD_AGENT_SHA256_OFFSET..UNLOAD_AGENT_SHA256_OFFSET + SHA256_TEXT_SIZE]
        .copy_from_slice(agent_sha256.as_bytes());
    write_c_string(
        &mut output,
        UNLOAD_MAPPING_OFFSET,
        &identity.agent_mapping_name,
    );
    write_u64(
        &mut output,
        UNLOAD_TARGET_SYMBOL_OFFSET,
        profile.target_symbol_offset,
    );
    output[UNLOAD_PROLOGUE_OFFSET..UNLOAD_PROLOGUE_OFFSET + PROLOGUE_SIZE]
        .copy_from_slice(&profile.expected_prologue);
    for (offset, value) in [
        (UNLOAD_AGENT_HANDLE_OFFSET, identity.agent_handle),
        (UNLOAD_AGENT_BASE_OFFSET, identity.agent_base),
        (UNLOAD_AGENT_LOAD_SIZE_OFFSET, identity.agent_load_size),
        (UNLOAD_FINALIZE_OFFSET, identity.finalize_address),
        (UNLOAD_HOOK_TARGET_OFFSET, identity.hook_target),
        (UNLOAD_TRAMPOLINE_START_OFFSET, identity.trampoline_start),
        (UNLOAD_TRAMPOLINE_SIZE_OFFSET, identity.trampoline_size),
        (UNLOAD_WORKER_START_OFFSET, identity.rpc_worker_start_time),
    ] {
        write_u64(&mut output, offset, value);
    }
    write_i32(
        &mut output,
        UNLOAD_WORKER_TID_OFFSET,
        identity.rpc_worker_tid as i32,
    );
    output[UNLOAD_RUNTIME_POLICY_OFFSET] =
        encode_runtime_policy(identity.agent_mapping_mode, identity.agent_visibility_mode);
    write_u64(
        &mut output,
        UNLOAD_SOINFO_OFFSET,
        identity.agent_soinfo_address,
    );
    output
        [UNLOAD_PROTECTED_ELF_HEADER_OFFSET..UNLOAD_PROTECTED_ELF_HEADER_OFFSET + ELF_HEADER_SIZE]
        .copy_from_slice(&identity.protected_elf_header);
    Ok(output)
}

/// 启动配置无法满足 loader 固定结构约束。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BootstrapEncodingError {
    /// PID 必须对应一个实际运行中的目标进程。
    #[error("target_pid 必须为正整数，实际为 {actual}")]
    InvalidTargetPid { actual: i32 },
    /// loader 和 agent 的等待上限只允许 1 至 30000 毫秒。
    #[error("timeout_ms 只允许 1 至 30000，实际为 {actual}")]
    InvalidTimeout { actual: u32 },
    /// 会话 ID 不能使用保留的全零值。
    #[error("session_id 不得为全零")]
    ZeroSessionId,
    /// 会话密钥不能使用全零值。
    #[error("session_secret 不得为全零")]
    ZeroSessionSecret,
    /// RPC 通道 ID 不能使用保留的全零值。
    #[error("channel_id 不得为全零")]
    ZeroChannelId,
    /// 匿名映射 ID 不能使用保留的全零值。
    #[error("mapping_id 不得为全零")]
    ZeroMappingId,
    /// C 字符串字段为空、包含 NUL、非 ASCII 或超过固定容量。
    #[error("{field} 必须是 1 至 {maximum} 字节的无 NUL ASCII 文本")]
    InvalidTextField { field: &'static str, maximum: usize },
    /// 模块摘要必须与设备端 `sha256sum` 的小写格式一致。
    #[error("module_sha256 必须是 64 位小写十六进制")]
    InvalidModuleSha256,
    /// Agent 摘要必须与设备上的精确卸载资产一致。
    #[error("agent_sha256 必须是 64 位小写十六进制")]
    InvalidAgentSha256,
    /// Agent 必须由加载器的随机 memfd 路径唯一标识。
    #[error("agent_mapping_name 必须以 /memfd: 开头")]
    InvalidAgentMappingName,
    /// 关闭收据缺少任一进程、线程、句柄或地址身份。
    #[error("卸载身份字段不得为 0")]
    IncompleteUnloadIdentity,
    /// RPC 工作线程必须独立于目标进程主线程，才能作为卸载载体。
    #[error("RPC worker TID 不得等于目标进程主线程 PID")]
    WorkerIsMainThread,
    /// Agent 或 trampoline 地址范围发生溢出或终结地址越界。
    #[error("卸载地址范围无效")]
    InvalidUnloadRange,
    /// 可见性模式必须与 loader 返回的 soinfo 身份同时存在或同时为空。
    #[error("Agent 可见性模式与 soinfo 卸载身份不一致")]
    InconsistentVisibilityIdentity,
    /// 目标函数偏移 0 被保留为未配置状态。
    #[error("target_symbol_offset 不得为 0")]
    ZeroTargetSymbolOffset,
}

/// 校验固定容量 C 字符串非空、纯 ASCII、无内嵌 NUL 且保留终止位。
fn validate_c_string(
    field: &'static str,
    value: &str,
    capacity: usize,
) -> Result<(), BootstrapEncodingError> {
    if value.is_empty()
        || value.len() >= capacity
        || !value.is_ascii()
        || value.as_bytes().contains(&0)
    {
        return Err(BootstrapEncodingError::InvalidTextField {
            field,
            maximum: capacity - 1,
        });
    }
    Ok(())
}

/// 校验与设备 `sha256sum` 输出一致的固定小写摘要文本。
fn validate_sha256(value: &str) -> Result<(), BootstrapEncodingError> {
    if !is_canonical_sha256(value) {
        return Err(BootstrapEncodingError::InvalidModuleSha256);
    }
    Ok(())
}

/// 在已验证容量的固定字段中写入文本和终止 NUL。
fn write_c_string(output: &mut [u8], offset: usize, value: &str) {
    let end: usize = offset + value.len();
    output[offset..end].copy_from_slice(value.as_bytes());
    output[end] = 0;
}

/// 按原生小端布局写入 32 位有符号整数。
fn write_i32(output: &mut [u8], offset: usize, value: i32) {
    output[offset..offset + size_of::<i32>()].copy_from_slice(&value.to_le_bytes());
}

/// 按原生小端布局写入 32 位无符号整数。
fn write_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

/// 按原生小端布局写入 64 位无符号整数。
fn write_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

/// 低两位保留映射模式，高两位编码可见性模式，兼容旧的 0/1 映射值。
const fn encode_runtime_policy(
    mapping_mode: AgentMappingMode,
    visibility_mode: AgentVisibilityMode,
) -> u8 {
    mapping_mode.wire_value() | (visibility_mode.wire_value() << 2)
}

#[cfg(test)]
mod tests {
    use super::{
        BOOTSTRAP_RUNTIME_POLICY_OFFSET, BOOTSTRAP_SIZE, BootstrapEncodingError, CHANNEL_ID_OFFSET,
        MAPPING_ID_OFFSET, PROLOGUE_OFFSET, RuntimeBootstrapProfile, RuntimeUnloadIdentity,
        SESSION_SECRET_OFFSET, SESSION_SECRET_SIZE, TARGET_SYMBOL_OFFSET, UNLOAD_AGENT_BASE_OFFSET,
        UNLOAD_AGENT_HANDLE_OFFSET, UNLOAD_AGENT_LOAD_SIZE_OFFSET, UNLOAD_FINALIZE_OFFSET,
        UNLOAD_HOOK_TARGET_OFFSET, UNLOAD_MAPPING_OFFSET, UNLOAD_PROCESS_START_OFFSET,
        UNLOAD_PROTECTED_ELF_HEADER_OFFSET, UNLOAD_RUNTIME_POLICY_OFFSET, UNLOAD_SIZE,
        UNLOAD_SOINFO_OFFSET, UNLOAD_TRAMPOLINE_SIZE_OFFSET, UNLOAD_TRAMPOLINE_START_OFFSET,
        UNLOAD_WORKER_START_OFFSET, UNLOAD_WORKER_TID_OFFSET, encode_bootstrap, encode_unload,
    };
    use crate::adapters::device::session::{SessionId, SessionSecret};
    use crate::adapters::settings::{AgentMappingMode, AgentVisibilityMode};

    // 固定样本覆盖摘要、偏移和 14 字节函数前导的完整布局。
    const MODULE_SHA256: &str = "fdf3106d1c35ffa6f0c3fb5a4f70e10698f22f8fb94266493eecd2671221126f";
    const PROLOGUE: [u8; 14] = [
        0x55, 0x53, 0x50, 0xf3, 0x0f, 0x11, 0x4c, 0x24, 0x04, 0xf3, 0x0f, 0x11, 0x04, 0x24,
    ];

    /// 验证 Rust 编码结果逐字段匹配原生紧凑结构。
    #[test]
    fn encoder_matches_native_packed_layout() {
        let session_id: SessionId = "00112233445566778899aabbccddeeff".parse().unwrap();
        let session_secret: SessionSecret =
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .parse()
                .unwrap();
        let channel_id: SessionId = "102132435465768798a9bacbdcedfe0f".parse().unwrap();
        let mapping_id: SessionId = "ffeeddccbbaa99887766554433221100".parse().unwrap();
        let profile: RuntimeBootstrapProfile = profile();

        let encoded: [u8; BOOTSTRAP_SIZE] = encode_bootstrap(
            12_345,
            5_000,
            session_id,
            &session_secret,
            channel_id,
            mapping_id,
            AgentMappingMode::Memfd,
            AgentVisibilityMode::Normal,
            &profile,
        )
        .unwrap();

        assert_eq!(&encoded[0..8], b"AZLWCFG2");
        assert_eq!(u32::from_le_bytes(encoded[8..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(encoded[12..16].try_into().unwrap()), 288);
        assert_eq!(
            i32::from_le_bytes(encoded[16..20].try_into().unwrap()),
            12_345
        );
        assert_eq!(
            u32::from_le_bytes(encoded[20..24].try_into().unwrap()),
            5_000
        );
        assert_eq!(
            &encoded[24..40],
            &[
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]
        );
        assert_eq!(
            &encoded[SESSION_SECRET_OFFSET..SESSION_SECRET_OFFSET + SESSION_SECRET_SIZE],
            &(0x00_u8..=0x1f).collect::<Vec<u8>>()
        );
        assert_eq!(
            &encoded[CHANNEL_ID_OFFSET..CHANNEL_ID_OFFSET + 16],
            &[
                0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xa9, 0xba, 0xcb, 0xdc, 0xed,
                0xfe, 0x0f
            ]
        );
        assert_eq!(
            &encoded[MAPPING_ID_OFFSET..MAPPING_ID_OFFSET + 16],
            &[
                0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22,
                0x11, 0x00
            ]
        );
        assert_eq!(&encoded[104..126], b"com.bilibili.azurlane\0");
        assert!(encoded[126..168].iter().all(|byte: &u8| *byte == 0));
        assert_eq!(&encoded[168..180], b"libtolua.so\0");
        assert_eq!(&encoded[200..264], MODULE_SHA256.as_bytes());
        assert_eq!(encoded[264], 0);
        assert_eq!(
            u64::from_le_bytes(
                encoded[TARGET_SYMBOL_OFFSET..PROLOGUE_OFFSET]
                    .try_into()
                    .unwrap()
            ),
            0x13c80
        );
        assert_eq!(&encoded[PROLOGUE_OFFSET..287], &PROLOGUE);
        assert_eq!(encoded[287], 0);

        let anonymous = encode_bootstrap(
            12_345,
            5_000,
            session_id,
            &session_secret,
            channel_id,
            mapping_id,
            AgentMappingMode::AnonymousRemap,
            AgentVisibilityMode::Normal,
            &profile,
        )
        .unwrap();
        assert_eq!(anonymous[BOOTSTRAP_RUNTIME_POLICY_OFFSET], 1);

        let hidden = encode_bootstrap(
            12_345,
            5_000,
            session_id,
            &session_secret,
            channel_id,
            mapping_id,
            AgentMappingMode::AnonymousRemap,
            AgentVisibilityMode::SolistHidden,
            &profile,
        )
        .unwrap();
        assert_eq!(hidden[BOOTSTRAP_RUNTIME_POLICY_OFFSET], 5);
    }

    /// 验证运行时 PID、超时和会话凭据的拒绝边界。
    #[test]
    fn encoder_rejects_invalid_runtime_values() {
        let valid_id: SessionId = "00112233445566778899aabbccddeeff".parse().unwrap();
        let zero_id: SessionId = "00000000000000000000000000000000".parse().unwrap();
        let valid_secret: SessionSecret =
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .parse()
                .unwrap();
        let zero_secret: SessionSecret =
            "0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap();
        let profile: RuntimeBootstrapProfile = profile();

        assert_eq!(
            encode_bootstrap(
                0,
                5_000,
                valid_id,
                &valid_secret,
                valid_id,
                valid_id,
                AgentMappingMode::Memfd,
                AgentVisibilityMode::Normal,
                &profile,
            ),
            Err(BootstrapEncodingError::InvalidTargetPid { actual: 0 })
        );
        assert_eq!(
            encode_bootstrap(
                1,
                0,
                valid_id,
                &valid_secret,
                valid_id,
                valid_id,
                AgentMappingMode::Memfd,
                AgentVisibilityMode::Normal,
                &profile,
            ),
            Err(BootstrapEncodingError::InvalidTimeout { actual: 0 })
        );
        assert_eq!(
            encode_bootstrap(
                1,
                5_000,
                zero_id,
                &valid_secret,
                valid_id,
                valid_id,
                AgentMappingMode::Memfd,
                AgentVisibilityMode::Normal,
                &profile,
            ),
            Err(BootstrapEncodingError::ZeroSessionId)
        );
        assert_eq!(
            encode_bootstrap(
                1,
                5_000,
                valid_id,
                &zero_secret,
                valid_id,
                valid_id,
                AgentMappingMode::Memfd,
                AgentVisibilityMode::Normal,
                &profile,
            ),
            Err(BootstrapEncodingError::ZeroSessionSecret)
        );
        assert_eq!(
            encode_bootstrap(
                1,
                5_000,
                valid_id,
                &valid_secret,
                zero_id,
                valid_id,
                AgentMappingMode::Memfd,
                AgentVisibilityMode::Normal,
                &profile,
            ),
            Err(BootstrapEncodingError::ZeroChannelId)
        );
        assert_eq!(
            encode_bootstrap(
                1,
                5_000,
                valid_id,
                &valid_secret,
                valid_id,
                zero_id,
                AgentMappingMode::Memfd,
                AgentVisibilityMode::Normal,
                &profile,
            ),
            Err(BootstrapEncodingError::ZeroMappingId)
        );
    }

    /// 验证卸载编码器逐字段匹配原生 512 字节紧凑布局且不写入会话密钥。
    #[test]
    fn unload_encoder_matches_native_packed_layout() {
        let session_id: SessionId = "00112233445566778899aabbccddeeff".parse().unwrap();
        let identity: RuntimeUnloadIdentity = unload_identity();
        let agent_sha256: String = "a".repeat(64);

        let encoded: [u8; UNLOAD_SIZE] = encode_unload(
            12_345,
            5_000,
            session_id,
            &profile(),
            &agent_sha256,
            &identity,
        )
        .unwrap();

        assert_eq!(&encoded[0..8], b"AZLWUNL1");
        assert_eq!(u32::from_le_bytes(encoded[8..12].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(encoded[12..16].try_into().unwrap()), 512);
        assert_eq!(
            u64::from_le_bytes(
                encoded[UNLOAD_PROCESS_START_OFFSET..UNLOAD_PROCESS_START_OFFSET + 8]
                    .try_into()
                    .unwrap()
            ),
            identity.process_start_time
        );
        assert_eq!(
            &encoded
                [UNLOAD_MAPPING_OFFSET..UNLOAD_MAPPING_OFFSET + identity.agent_mapping_name.len()],
            identity.agent_mapping_name.as_bytes()
        );
        for (offset, value) in [
            (UNLOAD_AGENT_HANDLE_OFFSET, identity.agent_handle),
            (UNLOAD_AGENT_BASE_OFFSET, identity.agent_base),
            (UNLOAD_AGENT_LOAD_SIZE_OFFSET, identity.agent_load_size),
            (UNLOAD_FINALIZE_OFFSET, identity.finalize_address),
            (UNLOAD_HOOK_TARGET_OFFSET, identity.hook_target),
            (UNLOAD_TRAMPOLINE_START_OFFSET, identity.trampoline_start),
            (UNLOAD_TRAMPOLINE_SIZE_OFFSET, identity.trampoline_size),
            (UNLOAD_WORKER_START_OFFSET, identity.rpc_worker_start_time),
        ] {
            assert_eq!(
                u64::from_le_bytes(encoded[offset..offset + 8].try_into().unwrap()),
                value
            );
        }
        assert_eq!(
            i32::from_le_bytes(
                encoded[UNLOAD_WORKER_TID_OFFSET..UNLOAD_WORKER_TID_OFFSET + 4]
                    .try_into()
                    .unwrap()
            ),
            identity.rpc_worker_tid as i32
        );
        assert_eq!(encoded[UNLOAD_RUNTIME_POLICY_OFFSET], 0);
        assert_eq!(
            u64::from_le_bytes(
                encoded[UNLOAD_SOINFO_OFFSET..UNLOAD_SOINFO_OFFSET + 8]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        assert!(
            encoded[UNLOAD_SOINFO_OFFSET + 8..]
                .iter()
                .all(|byte: &u8| *byte == 0)
        );

        let mut anonymous_identity = identity.clone();
        anonymous_identity.agent_mapping_mode = AgentMappingMode::AnonymousRemap;
        let anonymous = encode_unload(
            12_345,
            5_000,
            session_id,
            &profile(),
            &agent_sha256,
            &anonymous_identity,
        )
        .unwrap();
        assert_eq!(anonymous[UNLOAD_RUNTIME_POLICY_OFFSET], 1);

        let mut hidden_identity = anonymous_identity;
        hidden_identity.agent_visibility_mode = AgentVisibilityMode::SolistHidden;
        hidden_identity.agent_soinfo_address = 0x0000_7f40_0000_0000;
        let hidden = encode_unload(
            12_345,
            5_000,
            session_id,
            &profile(),
            &agent_sha256,
            &hidden_identity,
        )
        .unwrap();
        assert_eq!(hidden[UNLOAD_RUNTIME_POLICY_OFFSET], 5);
        assert_eq!(
            u64::from_le_bytes(
                hidden[UNLOAD_SOINFO_OFFSET..UNLOAD_SOINFO_OFFSET + 8]
                    .try_into()
                    .unwrap()
            ),
            hidden_identity.agent_soinfo_address
        );

        let mut strongest_identity = hidden_identity;
        strongest_identity.agent_visibility_mode = AgentVisibilityMode::SolistAndElfHeader;
        strongest_identity.protected_elf_header = [0xa5; 64];
        let strongest = encode_unload(
            12_345,
            5_000,
            session_id,
            &profile(),
            &agent_sha256,
            &strongest_identity,
        )
        .unwrap();
        assert_eq!(strongest[UNLOAD_RUNTIME_POLICY_OFFSET], 9);
        assert_eq!(
            &strongest[UNLOAD_PROTECTED_ELF_HEADER_OFFSET..UNLOAD_PROTECTED_ELF_HEADER_OFFSET + 64],
            &strongest_identity.protected_elf_header
        );
    }

    /// 验证卸载身份拒绝非 memfd 名、空线程身份和越界终结地址。
    #[test]
    fn unload_encoder_rejects_incomplete_or_ambiguous_identity() {
        let session_id: SessionId = "00112233445566778899aabbccddeeff".parse().unwrap();
        let agent_sha256: String = "a".repeat(64);
        let mut identity: RuntimeUnloadIdentity = unload_identity();
        identity.agent_mapping_name = "/data/local/tmp/agent.so".to_owned();
        assert_eq!(
            encode_unload(
                12_345,
                5_000,
                session_id,
                &profile(),
                &agent_sha256,
                &identity,
            ),
            Err(BootstrapEncodingError::InvalidAgentMappingName)
        );

        identity = unload_identity();
        identity.rpc_worker_tid = 0;
        assert_eq!(
            encode_unload(
                12_345,
                5_000,
                session_id,
                &profile(),
                &agent_sha256,
                &identity,
            ),
            Err(BootstrapEncodingError::IncompleteUnloadIdentity)
        );

        identity = unload_identity();
        identity.rpc_worker_tid = 12_345;
        assert_eq!(
            encode_unload(
                12_345,
                5_000,
                session_id,
                &profile(),
                &agent_sha256,
                &identity,
            ),
            Err(BootstrapEncodingError::WorkerIsMainThread)
        );

        identity = unload_identity();
        identity.finalize_address = identity.agent_base + identity.agent_load_size;
        assert_eq!(
            encode_unload(
                12_345,
                5_000,
                session_id,
                &profile(),
                &agent_sha256,
                &identity,
            ),
            Err(BootstrapEncodingError::InvalidUnloadRange)
        );

        identity = unload_identity();
        identity.protected_elf_header = [0xa5; 64];
        assert_eq!(
            encode_unload(
                12_345,
                5_000,
                session_id,
                &profile(),
                &agent_sha256,
                &identity,
            ),
            Err(BootstrapEncodingError::InconsistentVisibilityIdentity)
        );

        identity = unload_identity();
        identity.agent_visibility_mode = AgentVisibilityMode::SolistAndElfHeader;
        identity.agent_soinfo_address = 0x0000_7f40_0000_0000;
        assert_eq!(
            encode_unload(
                12_345,
                5_000,
                session_id,
                &profile(),
                &agent_sha256,
                &identity,
            ),
            Err(BootstrapEncodingError::InconsistentVisibilityIdentity)
        );
    }

    /// 验证 profile 不接受模糊文本、非规范摘要和未配置偏移。
    #[test]
    fn profile_rejects_ambiguous_or_unpinned_identity() {
        assert!(matches!(
            RuntimeBootstrapProfile::new("", "libtolua.so", MODULE_SHA256, 0x13c80, PROLOGUE),
            Err(BootstrapEncodingError::InvalidTextField {
                field: "package_name",
                ..
            })
        ));
        assert_eq!(
            RuntimeBootstrapProfile::new(
                "com.bilibili.azurlane",
                "libtolua.so",
                MODULE_SHA256.to_uppercase(),
                0x13c80,
                PROLOGUE,
            ),
            Err(BootstrapEncodingError::InvalidModuleSha256)
        );
        assert_eq!(
            RuntimeBootstrapProfile::new(
                "com.bilibili.azurlane",
                "libtolua.so",
                MODULE_SHA256,
                0,
                PROLOGUE,
            ),
            Err(BootstrapEncodingError::ZeroTargetSymbolOffset)
        );
    }

    /// 构造各测试共享的有效冻结 profile。
    fn profile() -> RuntimeBootstrapProfile {
        RuntimeBootstrapProfile::new(
            "com.bilibili.azurlane",
            "libtolua.so",
            MODULE_SHA256,
            0x13c80,
            PROLOGUE,
        )
        .unwrap()
    }

    /// 构造加载收据与 shutdown 收据能够共同提供的完整卸载身份。
    fn unload_identity() -> RuntimeUnloadIdentity {
        RuntimeUnloadIdentity {
            process_start_time: 0x0102_0304_0506_0708,
            agent_handle: 0x0000_7f00_0000_1000,
            agent_base: 0x0000_7f10_0000_0000,
            agent_load_size: 0x20_000,
            finalize_address: 0x0000_7f10_0000_1000,
            agent_mapping_name: "/memfd:azlw-fixture (deleted)".to_owned(),
            hook_target: 0x0000_7f20_0001_3c80,
            trampoline_start: 0x0000_7f30_0000_0000,
            trampoline_size: 0x1_000,
            rpc_worker_tid: 12_400,
            rpc_worker_start_time: 0x1112_1314_1516_1718,
            agent_mapping_mode: AgentMappingMode::Memfd,
            agent_visibility_mode: AgentVisibilityMode::Normal,
            agent_soinfo_address: 0,
            protected_elf_header: [0; 64],
        }
    }
}

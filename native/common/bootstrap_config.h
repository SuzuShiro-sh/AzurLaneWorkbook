// 定义宿主、加载器与 agent 共用的固定长度启动配置和稳定返回码。

#pragma once

#include <cstddef>
#include <cstdint>
#include <type_traits>

#include "runtime_rpc_contract.h"

namespace azlw {

/// 启动配置的线上标识、字段长度和允许边界。
inline constexpr std::uint8_t kBootstrapMagic[8] = {'A', 'Z', 'L', 'W', 'C', 'F', 'G', '2'};
inline constexpr std::uint32_t kBootstrapVersion = 2;
inline constexpr std::uint8_t kUnloadMagic[8] = {'A', 'Z', 'L', 'W', 'U', 'N', 'L', '1'};
inline constexpr std::uint32_t kUnloadVersion = 1;
inline constexpr std::size_t kSessionIdSize = 16;
inline constexpr std::size_t kSessionSecretSize = 32;
inline constexpr std::size_t kHookPrologueSize = 14;
inline constexpr std::size_t kAgentMappingNameSize = 64;
inline constexpr std::uint32_t kMinimumTimeoutMs = runtime_rpc_contract::kMinimumTimeoutMs;
inline constexpr std::uint32_t kMaximumTimeoutMs = runtime_rpc_contract::kMaximumTimeoutMs;

/// Agent 映射策略在线路结构中的稳定值。
enum class AgentMappingMode : std::uint8_t {
    Memfd = 0,
    AnonymousRemap = 1,
};

/// Agent 运行期间的 linker 可见性策略。
enum class AgentVisibilityMode : std::uint8_t {
    Normal = 0,
    SolistHidden = 1,
    SolistAndElfHeader = 2,
};

/// 冻结为单字节的映射与可见性组合策略。
struct AgentRuntimePolicy final {
    std::uint8_t value = 0;
};

inline constexpr std::uint8_t kAgentMappingModeMask = 0x03;
inline constexpr std::uint8_t kAgentVisibilityModeMask = 0x0c;
inline constexpr std::uint8_t kAgentVisibilityModeShift = 2;

/// 将两个独立策略编码为兼容旧映射值的单字节。
[[nodiscard]] constexpr AgentRuntimePolicy make_agent_runtime_policy(
    AgentMappingMode mapping_mode,
    AgentVisibilityMode visibility_mode) noexcept {
    return AgentRuntimePolicy{
        .value = static_cast<std::uint8_t>(
            static_cast<std::uint8_t>(mapping_mode) |
            (static_cast<std::uint8_t>(visibility_mode) << kAgentVisibilityModeShift)),
    };
}

/// 只接受已声明的映射、可见性值和全零保留位。
[[nodiscard]] constexpr bool is_valid_agent_runtime_policy(
    AgentRuntimePolicy policy) noexcept {
    const std::uint8_t mapping = policy.value & kAgentMappingModeMask;
    const std::uint8_t visibility =
        (policy.value & kAgentVisibilityModeMask) >> kAgentVisibilityModeShift;
    return (policy.value & ~(kAgentMappingModeMask | kAgentVisibilityModeMask)) == 0 &&
           mapping <= static_cast<std::uint8_t>(AgentMappingMode::AnonymousRemap) &&
           visibility <= static_cast<std::uint8_t>(AgentVisibilityMode::SolistAndElfHeader);
}

/// 调用方必须先通过 `is_valid_agent_runtime_policy`。
[[nodiscard]] constexpr AgentMappingMode agent_mapping_mode(
    AgentRuntimePolicy policy) noexcept {
    return static_cast<AgentMappingMode>(policy.value & kAgentMappingModeMask);
}

/// 调用方必须先通过 `is_valid_agent_runtime_policy`。
[[nodiscard]] constexpr AgentVisibilityMode agent_visibility_mode(
    AgentRuntimePolicy policy) noexcept {
    return static_cast<AgentVisibilityMode>(
        (policy.value & kAgentVisibilityModeMask) >> kAgentVisibilityModeShift);
}

/// 两种隐藏模式都需要摘除并在卸载前接回 linker solist。
[[nodiscard]] constexpr bool uses_solist_visibility(AgentVisibilityMode mode) noexcept {
    return mode != AgentVisibilityMode::Normal;
}

/// 最高可见性模式额外保护并在卸载前恢复完整 ELF 头。
[[nodiscard]] constexpr bool uses_elf_header_protection(AgentVisibilityMode mode) noexcept {
    return mode == AgentVisibilityMode::SolistAndElfHeader;
}

#pragma pack(push, 1)
/// loader 与 agent 之间唯一允许的固定长度启动数据，字段均由宿主写入。
struct BootstrapConfigV2 final {
    std::uint8_t magic[8];
    std::uint32_t schema_version;
    std::uint32_t total_size;
    std::int32_t target_pid;
    std::uint32_t timeout_ms;
    std::uint8_t session_id[kSessionIdSize];
    std::uint8_t session_secret[kSessionSecretSize];
    std::uint8_t channel_id[kSessionIdSize];
    std::uint8_t mapping_id[kSessionIdSize];
    char package_name[64];
    char module_name[32];
    char module_sha256[65];
    std::uint64_t target_symbol_offset;
    std::uint8_t expected_prologue[kHookPrologueSize];
    AgentRuntimePolicy agent_runtime_policy;
};

/// 宿主与 cleanup loader 之间的一次性卸载数据，不包含会话密钥。
struct AgentUnloadConfigV1 final {
    std::uint8_t magic[8];
    std::uint32_t schema_version;
    std::uint32_t total_size;
    std::int32_t target_pid;
    std::uint32_t timeout_ms;
    std::uint8_t session_id[kSessionIdSize];
    std::uint64_t process_start_time;
    char package_name[64];
    char module_name[32];
    char module_sha256[65];
    char agent_sha256[65];
    char agent_mapping_name[kAgentMappingNameSize];
    std::uint64_t target_symbol_offset;
    std::uint8_t expected_prologue[kHookPrologueSize];
    std::uint64_t agent_handle;
    std::uint64_t agent_base;
    std::uint64_t agent_load_size;
    std::uint64_t finalize_address;
    std::uint64_t hook_target;
    std::uint64_t trampoline_start;
    std::uint64_t trampoline_size;
    std::int32_t rpc_worker_tid;
    std::uint64_t rpc_worker_start_time;
    AgentRuntimePolicy agent_runtime_policy;
    std::uint64_t agent_soinfo_address;
    std::uint8_t protected_elf_header[64];
    std::uint8_t reserved[11];
};
#pragma pack(pop)

/// 二进制布局必须跨宿主编码器和两个原生组件保持为固定 288 字节。
static_assert(sizeof(BootstrapConfigV2) == 288);
static_assert(std::is_standard_layout_v<BootstrapConfigV2>);
static_assert(offsetof(BootstrapConfigV2, session_id) == 24);
static_assert(offsetof(BootstrapConfigV2, session_secret) == 40);
static_assert(offsetof(BootstrapConfigV2, channel_id) == 72);
static_assert(offsetof(BootstrapConfigV2, mapping_id) == 88);
static_assert(offsetof(BootstrapConfigV2, package_name) == 104);
static_assert(offsetof(BootstrapConfigV2, module_name) == 168);
static_assert(offsetof(BootstrapConfigV2, module_sha256) == 200);
static_assert(offsetof(BootstrapConfigV2, target_symbol_offset) == 265);
static_assert(offsetof(BootstrapConfigV2, expected_prologue) == 273);
static_assert(offsetof(BootstrapConfigV2, agent_runtime_policy) == 287);
static_assert(sizeof(AgentUnloadConfigV1) == 512);
static_assert(std::is_standard_layout_v<AgentUnloadConfigV1>);
static_assert(offsetof(AgentUnloadConfigV1, agent_runtime_policy) == 428);
static_assert(offsetof(AgentUnloadConfigV1, agent_soinfo_address) == 429);
static_assert(offsetof(AgentUnloadConfigV1, protected_elf_header) == 437);

/// `azlw_agent_start` 的稳定返回码；0 是唯一成功值。
enum class AgentStartCode : std::int32_t {
    Ok = 0,
    InvalidConfig = 10,
    IdentityMismatch = 11,
    ModuleMissing = 12,
    SymbolMissing = 13,
    PrologueMismatch = 14,
    HookInstallFailed = 15,
    SocketStartFailed = 16,
    AlreadyStarted = 17,
};

/// `azlw_agent_finalize_shutdown` 的稳定返回码；任何非零结果都禁止 dlclose。
enum class AgentFinalizeCode : std::int32_t {
    Ok = 0,
    NotPrepared = 20,
    RpcWorkerNotParked = 21,
    CallbackRunning = 22,
    RuntimeBusy = 23,
    HookUninstallFailed = 24,
};

}  // namespace azlw

// 实现加载器与 agent 共用的固定启动配置纯内存校验。

#include "bootstrap_validation.h"

#include <algorithm>
#include <cstring>
#include <iterator>
#include <limits>

namespace azlw {
namespace {

/// 确认固定容量字符数组内存在终止符，避免后续越界读取。
bool has_terminator(const char* value, std::size_t capacity) {
    return std::memchr(value, '\0', capacity) != nullptr;
}

/// 校验固定长度摘要只包含规范小写十六进制字符。
bool is_lower_hex(const char* value, std::size_t length) {
    for (std::size_t index = 0; index < length; ++index) {
        const char current = value[index];
        if (!((current >= '0' && current <= '9') || (current >= 'a' && current <= 'f'))) {
            return false;
        }
    }
    return true;
}

}  // namespace

// 一次完成版本、长度、会话身份和所有字段边界检查。
bool validate_bootstrap_config(const BootstrapConfigV2& config, std::string* error) {
    if (std::memcmp(config.magic, kBootstrapMagic, sizeof(kBootstrapMagic)) != 0) {
        *error = "bootstrap magic 不匹配";
        return false;
    }
    if (config.schema_version != kBootstrapVersion || config.total_size != sizeof(config)) {
        *error = "bootstrap schema 或长度不匹配";
        return false;
    }
    if (config.target_pid <= 0) {
        *error = "bootstrap target_pid 必须为正整数";
        return false;
    }
    if (config.timeout_ms < kMinimumTimeoutMs || config.timeout_ms > kMaximumTimeoutMs) {
        *error = "bootstrap timeout_ms 超出 1 至 30000";
        return false;
    }
    if (std::all_of(std::begin(config.session_id), std::end(config.session_id), [](std::uint8_t value) {
            return value == 0;
        }) ||
        std::all_of(
            std::begin(config.session_secret),
            std::end(config.session_secret),
            [](std::uint8_t value) { return value == 0; }) ||
        std::all_of(
            std::begin(config.channel_id),
            std::end(config.channel_id),
            [](std::uint8_t value) { return value == 0; }) ||
        std::all_of(
            std::begin(config.mapping_id),
            std::end(config.mapping_id),
            [](std::uint8_t value) { return value == 0; })) {
        *error = "bootstrap session_id、session_secret、channel_id 和 mapping_id 不得为全零";
        return false;
    }
    if (!has_terminator(config.package_name, sizeof(config.package_name)) || config.package_name[0] == '\0') {
        *error = "bootstrap package_name 缺少结尾或为空";
        return false;
    }
    if (!has_terminator(config.module_name, sizeof(config.module_name)) || config.module_name[0] == '\0') {
        *error = "bootstrap module_name 缺少结尾或为空";
        return false;
    }
    if (config.module_sha256[64] != '\0' || !is_lower_hex(config.module_sha256, 64)) {
        *error = "bootstrap module_sha256 必须是 64 位小写十六进制";
        return false;
    }
    if (config.target_symbol_offset == 0) {
        *error = "bootstrap target_symbol_offset 不得为 0";
        return false;
    }
    if (!is_valid_agent_runtime_policy(config.agent_runtime_policy)) {
        *error = "bootstrap agent_runtime_policy 无效";
        return false;
    }
    return true;
}

// 卸载结构不含密钥，但必须完整绑定进程、二进制、映射、Hook 和 RPC worker 身份。
bool validate_unload_config(const AgentUnloadConfigV1& config, std::string* error) {
    if (std::memcmp(config.magic, kUnloadMagic, sizeof(kUnloadMagic)) != 0) {
        *error = "unload magic 不匹配";
        return false;
    }
    if (config.schema_version != kUnloadVersion || config.total_size != sizeof(config)) {
        *error = "unload schema 或长度不匹配";
        return false;
    }
    if (config.target_pid <= 0 || config.process_start_time == 0) {
        *error = "unload 目标 PID 或进程启动时刻无效";
        return false;
    }
    if (config.timeout_ms < kMinimumTimeoutMs || config.timeout_ms > kMaximumTimeoutMs) {
        *error = "unload timeout_ms 超出 1 至 30000";
        return false;
    }
    if (std::all_of(std::begin(config.session_id), std::end(config.session_id), [](std::uint8_t value) {
            return value == 0;
        })) {
        *error = "unload session_id 不得为全零";
        return false;
    }
    if (!has_terminator(config.package_name, sizeof(config.package_name)) ||
        config.package_name[0] == '\0' ||
        !has_terminator(config.module_name, sizeof(config.module_name)) ||
        config.module_name[0] == '\0') {
        *error = "unload 包名或模块名缺少结尾或为空";
        return false;
    }
    if (config.module_sha256[64] != '\0' || !is_lower_hex(config.module_sha256, 64) ||
        config.agent_sha256[64] != '\0' || !is_lower_hex(config.agent_sha256, 64)) {
        *error = "unload SHA-256 必须是 64 位小写十六进制";
        return false;
    }
    if (!has_terminator(config.agent_mapping_name, sizeof(config.agent_mapping_name)) ||
        std::strncmp(config.agent_mapping_name, "/memfd:", 7) != 0) {
        *error = "unload agent_mapping_name 必须是完整 memfd 映射名";
        return false;
    }
    if (config.target_symbol_offset == 0 || config.agent_handle == 0 || config.agent_base == 0 ||
        config.agent_load_size == 0 || config.finalize_address == 0 || config.hook_target == 0 ||
        config.trampoline_start == 0 || config.trampoline_size == 0 ||
        config.rpc_worker_tid <= 0 || config.rpc_worker_tid == config.target_pid ||
        config.rpc_worker_start_time == 0) {
        *error = "unload 地址、映射长度或 RPC worker 身份不完整";
        return false;
    }
    if (config.agent_base > std::numeric_limits<std::uint64_t>::max() - config.agent_load_size ||
        config.trampoline_start >
            std::numeric_limits<std::uint64_t>::max() - config.trampoline_size) {
        *error = "unload 地址范围溢出";
        return false;
    }
    if (!is_valid_agent_runtime_policy(config.agent_runtime_policy)) {
        *error = "unload agent_runtime_policy 无效";
        return false;
    }
    const AgentVisibilityMode visibility_mode =
        agent_visibility_mode(config.agent_runtime_policy);
    const bool protected_header_present = std::any_of(
        std::begin(config.protected_elf_header),
        std::end(config.protected_elf_header),
        [](std::uint8_t value) { return value != 0; });
    const bool visibility_identity_valid =
        (visibility_mode == AgentVisibilityMode::Normal &&
         config.agent_soinfo_address == 0 && !protected_header_present) ||
        (visibility_mode == AgentVisibilityMode::SolistHidden &&
         config.agent_soinfo_address != 0 && !protected_header_present) ||
        (visibility_mode == AgentVisibilityMode::SolistAndElfHeader &&
         config.agent_soinfo_address != 0 && protected_header_present);
    if (!visibility_identity_valid) {
        *error = "unload solist 与 ELF 头身份不符合可见性策略";
        return false;
    }
    if (!std::all_of(std::begin(config.reserved), std::end(config.reserved), [](std::uint8_t value) {
            return value == 0;
        })) {
        *error = "unload reserved 必须为 0";
        return false;
    }
    return true;
}

// 使用固定小写编码，将二进制身份转换为路径、通道或匿名映射名称。
static std::string encode_id_hex(const std::uint8_t (&identifier)[kSessionIdSize]) {
    constexpr char kHex[] = "0123456789abcdef";
    std::string result;
    result.reserve(kSessionIdSize * 2);
    for (const std::uint8_t value : identifier) {
        result.push_back(kHex[value >> 4]);
        result.push_back(kHex[value & 0x0f]);
    }
    return result;
}

std::string bootstrap_session_id_hex(const BootstrapConfigV2& config) {
    return encode_id_hex(config.session_id);
}

std::string bootstrap_channel_id_hex(const BootstrapConfigV2& config) {
    return encode_id_hex(config.channel_id);
}

std::string bootstrap_mapping_id_hex(const BootstrapConfigV2& config) {
    return encode_id_hex(config.mapping_id);
}

std::string unload_session_id_hex(const AgentUnloadConfigV1& config) {
    return encode_id_hex(config.session_id);
}

}  // namespace azlw

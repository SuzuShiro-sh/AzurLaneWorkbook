// 定义 Agent 卸载后可容许的不可访问匿名地址占位特征。

#pragma once

#include <cstdint>
#include <string_view>

namespace azlw::loader {

/// 只保留判断映射身份所需的稳定字段，避免把第三方 maps 类型扩散到策略测试。
struct AgentMappingIdentity final {
    std::uintptr_t start_address = 0;
    std::uintptr_t end_address = 0;
    bool readable = false;
    bool writeable = false;
    bool executable = false;
    bool is_private = false;
    bool is_shared = false;
    std::uintptr_t offset = 0;
    std::string_view device;
    std::uint64_t inode = 0;
    std::string_view pathname;
};

/// 区分无关映射、可容许的不可访问地址复用，以及必须阻断卸载的残留。
enum class AgentMappingDisposition : std::uint8_t {
    Unrelated,
    InertAnonymousOverlap,
    Reject,
};

/// 去除 procfs 可能追加的单个 `(deleted)` 后缀，统一收据和当前映射的表示。
[[nodiscard]] constexpr std::string_view normalize_agent_mapping_name(
    std::string_view pathname) noexcept {
    constexpr std::string_view kDeletedSuffix = " (deleted)";
    if (pathname.size() >= kDeletedSuffix.size() &&
        pathname.substr(pathname.size() - kDeletedSuffix.size()) == kDeletedSuffix) {
        pathname.remove_suffix(kDeletedSuffix.size());
    }
    return pathname;
}

/// 收据或当前 maps 任一侧带有 `(deleted)` 时仍按同一 memfd 身份处理。
[[nodiscard]] constexpr bool matches_agent_mapping_name(
    std::string_view pathname,
    std::string_view agent_mapping_name) noexcept {
    return normalize_agent_mapping_name(pathname) ==
           normalize_agent_mapping_name(agent_mapping_name);
}

/// 不可访问、无文件身份且无名称的私有匿名映射不能承载已卸载 Agent 的代码或数据。
[[nodiscard]] constexpr bool is_inert_anonymous_mapping(
    const AgentMappingIdentity& mapping) noexcept {
    return !mapping.readable && !mapping.writeable && !mapping.executable &&
           mapping.is_private && !mapping.is_shared && mapping.offset == 0 &&
           mapping.device == "00:00" && mapping.inode == 0 && mapping.pathname.empty();
}

/// 活跃 Agent 匿名段只接受内核无文件身份映射和链接器生成的标准 `.bss` 匿名段。
[[nodiscard]] constexpr bool is_live_anonymous_agent_mapping(
    const AgentMappingIdentity& mapping) noexcept {
    return mapping.is_private && !mapping.is_shared && mapping.offset == 0 &&
           mapping.device == "00:00" && mapping.inode == 0 &&
           (mapping.pathname.empty() || mapping.pathname == "[anon:.bss]");
}

/// 精确名称在任意地址都属于残留；原范围内仅容许不可访问匿名地址复用。
[[nodiscard]] constexpr AgentMappingDisposition classify_agent_mapping(
    const AgentMappingIdentity& mapping,
    std::uintptr_t agent_start,
    std::uintptr_t agent_end,
    std::string_view agent_mapping_name) noexcept {
    if (matches_agent_mapping_name(mapping.pathname, agent_mapping_name)) {
        return AgentMappingDisposition::Reject;
    }
    if (mapping.start_address >= agent_end || mapping.end_address <= agent_start) {
        return AgentMappingDisposition::Unrelated;
    }
    return is_inert_anonymous_mapping(mapping)
               ? AgentMappingDisposition::InertAnonymousOverlap
               : AgentMappingDisposition::Reject;
}

}  // namespace azlw::loader

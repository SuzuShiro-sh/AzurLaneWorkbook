// 解析 loader 参数，选择加载或卸载事务，并输出结构化收据。

#include <algorithm>
#include <array>
#include <charconv>
#include <cstdint>
#include <iostream>
#include <limits>
#include <string>

#include <sys/types.h>

#include "agent_unloader.h"
#include "load_transaction.h"

namespace azlw::loader {
namespace {

/// loader 进程面向宿主返回的稳定退出状态。
enum class ExitCode : int {
    Ok = 0,
    InvalidArguments = 2,
    BootstrapInvalid = 10,
    IdentityRejected = 11,
    AttachFailed = 12,
    InjectorFailed = 13,
    AgentStartFailed = 14,
    CleanupFailed = 15,
    AgentUnloadFailed = 16,
};

enum class LoaderMode : std::uint8_t { Load, Unload };

/// 解析并去重后的最小命令行参数集合。
struct Arguments final {
    LoaderMode mode = LoaderMode::Load;
    pid_t pid = 0;
    std::string agent_path;
    std::string session_file;
};

/// 将远程指针编码为固定 16 位小写十六进制，避免 JSON 数字精度损失。
std::string fixed_hex_address(std::uintptr_t value) {
    std::array<char, 16> result{};
    result.fill('0');
    std::array<char, 16> digits{};
    const auto encoded = std::to_chars(digits.data(), digits.data() + digits.size(), value, 16);
    const std::size_t length = static_cast<std::size_t>(encoded.ptr - digits.data());
    std::copy(digits.data(), encoded.ptr, result.end() - static_cast<std::ptrdiff_t>(length));
    return std::string(result.data(), result.size());
}

/// 将 ELF 头保护证据编码为固定 128 位小写十六进制，供卸载票据原样回传。
std::string fixed_hex_header(const ElfHeaderBytes& value) {
    constexpr char kHex[] = "0123456789abcdef";
    std::string result;
    result.reserve(value.size() * 2);
    for (const std::uint8_t byte : value) {
        result.push_back(kHex[byte >> 4]);
        result.push_back(kHex[byte & 0x0f]);
    }
    return result;
}

/// 转义收据文本，保证所有失败消息仍形成单行有效 JSON。
std::string json_escape(const std::string& value) {
    std::string escaped;
    escaped.reserve(value.size() + 16);
    for (const unsigned char current : value) {
        switch (current) {
            case '"': escaped += "\\\""; break;
            case '\\': escaped += "\\\\"; break;
            case '\n': escaped += "\\n"; break;
            case '\r': escaped += "\\r"; break;
            case '\t': escaped += "\\t"; break;
            default:
                if (current < 0x20) {
                    constexpr char kHex[] = "0123456789abcdef";
                    escaped += "\\u00";
                    escaped.push_back(kHex[current >> 4]);
                    escaped.push_back(kHex[current & 0x0f]);
                } else {
                    escaped.push_back(static_cast<char>(current));
                }
                break;
        }
    }
    return escaped;
}

/// 输出唯一一条带稳定码的宿主收据，并返回对应进程退出码。
int finish(ExitCode code, const std::string& stable_code, const std::string& message, pid_t pid = 0) {
    std::cout << "AZLW_RECEIPT {\"status\":\"" << (code == ExitCode::Ok ? "ok" : "error")
              << "\",\"code\":\"" << stable_code << "\",\"message\":\"" << json_escape(message)
              << "\",\"process_id\":" << pid << "}" << std::endl;
    return static_cast<int>(code);
}

const char* target_state_name(LoadTargetState state) {
    switch (state) {
        case LoadTargetState::Unchanged:
            return "unchanged";
        case LoadTargetState::Restored:
            return "restored";
        case LoadTargetState::Unknown:
            return "unknown";
    }
    return "unknown";
}

const char* agent_mapping_mode_name(AgentMappingMode mode) {
    switch (mode) {
        case AgentMappingMode::Memfd:
            return "memfd";
        case AgentMappingMode::AnonymousRemap:
            return "anonymous_remap";
    }
    return "unknown";
}

const char* agent_visibility_mode_name(AgentVisibilityMode mode) {
    switch (mode) {
        case AgentVisibilityMode::Normal:
            return "normal";
        case AgentVisibilityMode::SolistHidden:
            return "solist_hidden";
        case AgentVisibilityMode::SolistAndElfHeader:
            return "solist_and_elf_header";
    }
    return "unknown";
}

/// 加载失败收据显式声明目标现场，供宿主决定是否需要重启兜底。
int finish_load_failure(
    ExitCode code,
    const std::string& stable_code,
    const std::string& message,
    pid_t pid,
    LoadTargetState target_state) {
    std::cout << "AZLW_RECEIPT {\"status\":\"error\",\"code\":\"" << stable_code
              << "\",\"message\":\"" << json_escape(message) << "\",\"process_id\":" << pid
              << ",\"target_state\":\"" << target_state_name(target_state) << "\"}"
              << std::endl;
    return static_cast<int>(code);
}

/// 成功加载收据持久化完整卸载所需的进程、句柄和精确映射身份。
int finish_loaded(
    pid_t pid,
    std::uint64_t process_start_time,
    std::uintptr_t agent_handle,
    std::uintptr_t agent_base,
    std::uintptr_t agent_load_size,
    std::uintptr_t finalize_address,
    const std::string& mapping_name,
    AgentMappingMode mapping_mode,
    const AnonymousRemapEvidence& remap_evidence,
    AgentVisibilityMode visibility_mode,
    const SolistVisibilityEvidence& solist_evidence,
    const ElfHeaderProtectionEvidence& elf_header_evidence) {
    std::cout << "AZLW_RECEIPT {\"status\":\"ok\",\"code\":\"loaded\","
              << "\"message\":\"专用 agent 已加载并启动\",\"process_id\":" << pid
              << ",\"process_start_time\":" << process_start_time
              << ",\"agent_handle\":\"" << fixed_hex_address(agent_handle)
              << "\",\"agent_base\":\"" << fixed_hex_address(agent_base)
              << "\",\"agent_load_size\":" << agent_load_size
              << ",\"finalize_address\":\"" << fixed_hex_address(finalize_address)
              << "\",\"agent_mapping_name\":\"" << json_escape(mapping_name)
              << "\",\"agent_mapping_mode\":\"" << agent_mapping_mode_name(mapping_mode)
              << "\",\"agent_visibility_mode\":\""
              << agent_visibility_mode_name(visibility_mode)
              << "\",\"agent_soinfo_address\":";
    if (solist_evidence.soinfo_address == 0) {
        std::cout << "null";
    } else {
        std::cout << "\"" << fixed_hex_address(solist_evidence.soinfo_address) << "\"";
    }
    std::cout << ",\"agent_protected_elf_header\":";
    if (std::all_of(
            elf_header_evidence.protected_header.begin(),
            elf_header_evidence.protected_header.end(),
            [](std::uint8_t value) { return value == 0; })) {
        std::cout << "null";
    } else {
        std::cout << "\"" << fixed_hex_header(elf_header_evidence.protected_header) << "\"";
    }
    std::cout
              << ",\"anonymous_segment_count\":" << remap_evidence.segment_count
              << ",\"anonymous_byte_count\":" << remap_evidence.byte_count
              << "}" << std::endl;
    return static_cast<int>(ExitCode::Ok);
}

/// 严格解析完整正整数 PID，不接受尾随字符或类型溢出。
bool parse_pid(const std::string& value, pid_t* pid) {
    std::int64_t parsed = 0;
    const auto result = std::from_chars(value.data(), value.data() + value.size(), parsed);
    if (result.ec != std::errc{} || result.ptr != value.data() + value.size() || parsed <= 0 ||
        parsed > std::numeric_limits<pid_t>::max()) {
        return false;
    }
    *pid = static_cast<pid_t>(parsed);
    return true;
}

/// 要求三个命名参数各出现一次，拒绝未知、重复和空值。
bool parse_arguments(int argc, char** argv, Arguments* arguments, std::string* error) {
    const bool unload = argc >= 2 && std::string(argv[1]) == "unload";
    if ((!unload && argc != 7) || (unload && argc != 6)) {
        *error = unload
                     ? "用法: loader unload --agent PATH --session-file PATH"
                     : "用法: loader --pid PID --agent PATH --session-file PATH";
        return false;
    }
    arguments->mode = unload ? LoaderMode::Unload : LoaderMode::Load;
    bool saw_pid = false;
    bool saw_agent = false;
    bool saw_session = false;
    for (int index = unload ? 2 : 1; index < argc; index += 2) {
        const std::string name = argv[index];
        const std::string value = argv[index + 1];
        if (!unload && name == "--pid" && !saw_pid) {
            saw_pid = parse_pid(value, &arguments->pid);
            if (!saw_pid) {
                *error = "--pid 必须是正整数";
                return false;
            }
        } else if (name == "--agent" && !saw_agent) {
            arguments->agent_path = value;
            saw_agent = !value.empty();
        } else if (name == "--session-file" && !saw_session) {
            arguments->session_file = value;
            saw_session = !value.empty();
        } else {
            *error = "存在未知、重复或空的 loader 参数";
            return false;
        }
    }
    return (unload || saw_pid) && saw_agent && saw_session;
}

ExitCode unload_exit_code(AgentUnloadStatus status) {
    switch (status) {
        case AgentUnloadStatus::Ok:
            return ExitCode::Ok;
        case AgentUnloadStatus::ConfigInvalid:
            return ExitCode::BootstrapInvalid;
        case AgentUnloadStatus::IdentityRejected:
            return ExitCode::IdentityRejected;
        case AgentUnloadStatus::AttachFailed:
            return ExitCode::AttachFailed;
        case AgentUnloadStatus::FinalizeFailed:
            return ExitCode::AgentUnloadFailed;
        case AgentUnloadStatus::CleanupFailed:
            return ExitCode::CleanupFailed;
    }
    return ExitCode::AgentUnloadFailed;
}

ExitCode load_exit_code(LoadExit exit_kind) {
    switch (exit_kind) {
        case LoadExit::Ok:
            return ExitCode::Ok;
        case LoadExit::BootstrapInvalid:
            return ExitCode::BootstrapInvalid;
        case LoadExit::IdentityRejected:
            return ExitCode::IdentityRejected;
        case LoadExit::AttachFailed:
            return ExitCode::AttachFailed;
        case LoadExit::InjectorFailed:
            return ExitCode::InjectorFailed;
        case LoadExit::AgentStartFailed:
            return ExitCode::AgentStartFailed;
        case LoadExit::CleanupFailed:
            return ExitCode::CleanupFailed;
    }
    return ExitCode::AgentStartFailed;
}

}  // namespace
}  // namespace azlw::loader

/// 解析参数、选择加载或卸载入口，并输出唯一一条宿主收据。
int main(int argc, char** argv) {
    using namespace azlw;
    using namespace azlw::loader;

    Arguments arguments;
    std::string error;
    if (!parse_arguments(argc, argv, &arguments, &error)) {
        return finish(ExitCode::InvalidArguments, "invalid_arguments", error);
    }
    if (arguments.mode == LoaderMode::Unload) {
        const AgentUnloadOutcome outcome =
            unload_agent(arguments.agent_path, arguments.session_file);
        return finish(
            unload_exit_code(outcome.status),
            outcome.stable_code,
            outcome.message,
            outcome.process_id);
    }

    LoadTransaction load;
    const LoadOutcome outcome = load.execute(
        static_cast<std::int32_t>(arguments.pid),
        arguments.agent_path,
        arguments.session_file);
    if (outcome.exit_kind == LoadExit::Ok) {
        const LoadedAgentEvidence& loaded = outcome.loaded;
        return finish_loaded(
            outcome.process_id,
            loaded.process_start_time,
            loaded.agent_handle,
            loaded.agent_base,
            loaded.agent_load_size,
            loaded.finalize_address,
            loaded.agent_mapping_name,
            loaded.mapping_mode,
            loaded.remap_evidence,
            loaded.visibility_mode,
            loaded.solist_evidence,
            loaded.elf_header_evidence);
    }
    return finish_load_failure(
        load_exit_code(outcome.exit_kind),
        outcome.stable_code,
        outcome.message,
        outcome.process_id,
        outcome.target_state);
}

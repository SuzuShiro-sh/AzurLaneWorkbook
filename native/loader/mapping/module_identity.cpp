// 实现进程、ELF、会话路径和已加载模块的多层身份核验。

#include "module_identity.h"

#include <algorithm>
#include <array>
#include <cerrno>
#include <charconv>
#include <chrono>
#include <cstring>
#include <elf.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <sstream>
#include <string_view>
#include <sys/stat.h>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>

#include <KittyMemoryEx.hpp>

#include "bootstrap_validation.h"

namespace azlw::loader {
namespace {

/// 将既有路径解析为规范绝对路径，并保留可定位错误。
bool canonicalize(const std::string& path, std::string* canonical, std::string* error) {
    std::array<char, PATH_MAX> buffer{};
    if (realpath(path.c_str(), buffer.data()) == nullptr) {
        *error = std::string("解析路径失败 ") + path + ": " + std::strerror(errno);
        return false;
    }
    *canonical = buffer.data();
    return true;
}

/// 按目录分隔边界判断子路径，避免简单字符串前缀误判。
bool starts_with_directory(const std::string& path, const std::string& directory) {
    return path.size() > directory.size() && path.compare(0, directory.size(), directory) == 0 &&
           path[directory.size()] == '/';
}

/// 要求会话目录真实存在、归 root 所有且不向其他用户开放。
bool verify_root_directory(const std::string& path, std::string* error) {
    struct stat info {};
    if (lstat(path.c_str(), &info) != 0) {
        *error = std::string("读取会话目录属性失败: ") + std::strerror(errno);
        return false;
    }
    if (!S_ISDIR(info.st_mode) || info.st_uid != 0 || (info.st_mode & 0077) != 0) {
        *error = "会话目录必须是 root 拥有且仅 root 可访问的真实目录";
        return false;
    }
    return true;
}

/// 要求运行资产是 root 所有、root 可读且不可被 group/other 改写的普通文件。
bool verify_root_asset(const std::string& path, std::string* error) {
    struct stat info {};
    if (lstat(path.c_str(), &info) != 0) {
        *error = std::string("读取资产属性失败: ") + std::strerror(errno);
        return false;
    }
    if (!S_ISREG(info.st_mode) || info.st_uid != 0 || (info.st_mode & 0022) != 0) {
        *error = "运行时资产必须是 root 拥有且不可被 group/other 写入的普通文件";
        return false;
    }
    if ((info.st_mode & S_IRUSR) == 0) {
        *error = "运行时资产缺少 root 读取权限";
        return false;
    }
    return true;
}

/// 读取固定 ELF 头并按调用场景决定是否拒绝符号链接。
bool verify_elf_x86_64_impl(const std::string& path, bool reject_symlink, std::string* error) {
    int flags = O_RDONLY | O_CLOEXEC;
    if (reject_symlink) {
        flags |= O_NOFOLLOW;
    }
    const int descriptor = open(path.c_str(), flags);
    if (descriptor < 0) {
        *error = std::string("打开 ELF 失败: ") + std::strerror(errno);
        return false;
    }
    Elf64_Ehdr header{};
    const ssize_t count = read(descriptor, &header, sizeof(header));
    close(descriptor);
    if (count != static_cast<ssize_t>(sizeof(header)) ||
        std::memcmp(header.e_ident, ELFMAG, SELFMAG) != 0 ||
        header.e_ident[EI_CLASS] != ELFCLASS64 || header.e_ident[EI_DATA] != ELFDATA2LSB ||
        header.e_machine != EM_X86_64) {
        *error = "ELF 不是小端 Android x86_64 目标";
        return false;
    }
    return true;
}

/// 从 `/proc/<pid>/cmdline` 读取 Android 进程的首个包名字段。
bool read_process_name(pid_t pid, std::string* name, std::string* error) {
    const std::string path = "/proc/" + std::to_string(pid) + "/cmdline";
    const int descriptor = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        *error = std::string("打开目标 cmdline 失败: ") + std::strerror(errno);
        return false;
    }
    std::array<char, 256> buffer{};
    ssize_t count;
    do {
        count = read(descriptor, buffer.data(), buffer.size() - 1);
    } while (count < 0 && errno == EINTR);
    const int read_error = count < 0 ? errno : 0;
    close(descriptor);
    if (count < 0) {
        *error = "读取目标 cmdline 失败: pid=" + std::to_string(pid) +
                 "，read_count=" + std::to_string(count) +
                 "，errno=" + std::to_string(read_error) +
                 " (" + std::strerror(read_error) + ")";
        return false;
    }
    if (count == 0) {
        *error = "目标 cmdline 为空: pid=" + std::to_string(pid) + "，read_count=0";
        return false;
    }
    *name = std::string(buffer.data());
    return true;
}

/// 校验外部散列工具输出的摘要字符集。
bool is_lower_hex(std::string_view value) {
    return std::all_of(value.begin(), value.end(), [](char current) {
        return (current >= '0' && current <= '9') || (current >= 'a' && current <= 'f');
    });
}

/// 在硬超时和输出上限内调用设备固定散列工具，避免阻塞加载事务。
bool sha256sum(
    const std::string& path,
    std::uint32_t timeout_ms,
    std::string* digest,
    std::string* error) {
    int descriptors[2]{};
    if (pipe2(descriptors, O_CLOEXEC | O_NONBLOCK) != 0) {
        *error = std::string("创建 sha256sum 管道失败: ") + std::strerror(errno);
        return false;
    }

    const pid_t child = fork();
    if (child < 0) {
        *error = std::string("启动 sha256sum 失败: ") + std::strerror(errno);
        close(descriptors[0]);
        close(descriptors[1]);
        return false;
    }
    if (child == 0) {
        dup2(descriptors[1], STDOUT_FILENO);
        close(descriptors[0]);
        close(descriptors[1]);
        execl("/system/bin/sha256sum", "sha256sum", path.c_str(), nullptr);
        _exit(127);
    }

    close(descriptors[1]);
    std::string output;
    output.reserve(256);
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    bool reached_eof = false;
    while (!reached_eof && std::chrono::steady_clock::now() < deadline) {
        const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now());
        struct pollfd poll_descriptor {
            descriptors[0], POLLIN | POLLHUP, 0
        };
        const int poll_result = poll(&poll_descriptor, 1, std::max(1, static_cast<int>(remaining.count())));
        if (poll_result < 0) {
            if (errno == EINTR) {
                continue;
            }
            *error = std::string("等待 sha256sum 失败: ") + std::strerror(errno);
            break;
        }
        if (poll_result == 0) {
            break;
        }
        std::array<char, 256> buffer{};
        while (true) {
            const ssize_t count = read(descriptors[0], buffer.data(), buffer.size());
            if (count > 0) {
                output.append(buffer.data(), static_cast<std::size_t>(count));
                if (output.size() > 1024) {
                    *error = "sha256sum 输出超过 1024 字节";
                    reached_eof = true;
                    break;
                }
                continue;
            }
            if (count == 0) {
                reached_eof = true;
            } else if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR) {
                *error = std::string("读取 sha256sum 输出失败: ") + std::strerror(errno);
                reached_eof = true;
            }
            break;
        }
    }
    close(descriptors[0]);

    int status = 0;
    pid_t wait_result = 0;
    while (wait_result == 0 && std::chrono::steady_clock::now() < deadline) {
        wait_result = waitpid(child, &status, WNOHANG);
        if (wait_result < 0 && errno == EINTR) {
            wait_result = 0;
            continue;
        }
        if (wait_result == 0) {
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
        }
    }
    if (wait_result == 0) {
        kill(child, SIGKILL);
        waitpid(child, &status, 0);
        if (error->empty()) {
            *error = "sha256sum 执行超时";
        }
        return false;
    }
    if (wait_result != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        if (error->empty()) {
            *error = "sha256sum 返回失败状态";
        }
        return false;
    }
    if (!error->empty()) {
        return false;
    }
    if (output.size() < 65 || output[64] != ' ' || !is_lower_hex(std::string_view(output).substr(0, 64))) {
        *error = "sha256sum 输出格式不符合预期";
        return false;
    }
    *digest = output.substr(0, 64);
    return true;
}

/// 共享启动和卸载路径规则，避免两种会话文件产生不同信任边界。
bool verify_session_asset_paths_impl(
    const std::string& session_id,
    const std::string& session_file,
    const std::string& agent_file,
    std::string* canonical_agent,
    std::string* error) {
    const std::string session_root = "/data/local/tmp/." + session_id;
    std::string canonical_root;
    std::string canonical_session;
    if (!canonicalize(session_root, &canonical_root, error) ||
        !canonicalize(session_file, &canonical_session, error) ||
        !canonicalize(agent_file, canonical_agent, error)) {
        return false;
    }
    if (canonical_root != session_root || !verify_root_directory(canonical_root, error)) {
        if (error->empty()) {
            *error = "会话目录不得经过符号链接重定向";
        }
        return false;
    }
    if (!starts_with_directory(canonical_session, canonical_root) ||
        !starts_with_directory(*canonical_agent, canonical_root)) {
        *error = "会话文件和 agent 必须位于当前 session_id 的设备暂存目录";
        return false;
    }
    if (canonical_agent->size() + 1 > 1024) {
        *error = "agent 规范路径超过远程缓冲区允许的 1024 字节";
        return false;
    }
    if (!verify_root_asset(canonical_session, error) || !verify_root_asset(*canonical_agent, error)) {
        return false;
    }
    return verify_elf_x86_64_impl(*canonical_agent, true, error);
}

/// 解析 Linux stat 中位于进程名右括号后的第 22 字段。
bool parse_start_time(std::string_view stat, std::uint64_t* start_time) {
    const std::size_t close_parenthesis = stat.rfind(')');
    if (close_parenthesis == std::string_view::npos || close_parenthesis + 2 >= stat.size()) {
        return false;
    }
    std::size_t cursor = close_parenthesis + 2;
    for (int field = 3; field <= 22; ++field) {
        while (cursor < stat.size() && stat[cursor] == ' ') {
            ++cursor;
        }
        const std::size_t end = stat.find(' ', cursor);
        const std::size_t token_end = end == std::string_view::npos ? stat.size() : end;
        if (cursor >= token_end) {
            return false;
        }
        if (field == 22) {
            const auto parsed = std::from_chars(
                stat.data() + cursor, stat.data() + token_end, *start_time);
            return parsed.ec == std::errc{} && parsed.ptr == stat.data() + token_end &&
                   *start_time != 0;
        }
        cursor = token_end;
    }
    return false;
}

}  // namespace

// 包名与 `/proc/<pid>/exe` 架构必须同时匹配，才接受 CLI 提供的 PID。
bool verify_process_identity(pid_t pid, const std::string& package_name, std::string* error) {
    std::string actual_name;
    if (!read_process_name(pid, &actual_name, error)) {
        return false;
    }
    if (actual_name != package_name) {
        *error = "目标进程包名不匹配，期望 " + package_name + "，实际 " + actual_name;
        return false;
    }
    // /proc/<pid>/exe 本身是内核提供的符号链接，只在该固定路径允许跟随。
    return verify_elf_x86_64_impl("/proc/" + std::to_string(pid) + "/exe", false, error);
}

bool read_process_start_time(pid_t pid, std::uint64_t* start_time, std::string* error) {
    const std::string path = "/proc/" + std::to_string(pid) + "/stat";
    const int descriptor = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        *error = std::string("打开目标 stat 失败: ") + std::strerror(errno);
        return false;
    }
    std::array<char, 1024> buffer{};
    const ssize_t count = read(descriptor, buffer.data(), buffer.size() - 1);
    const int close_result = close(descriptor);
    if (count <= 0 || close_result != 0 ||
        !parse_start_time(
            std::string_view(buffer.data(), static_cast<std::size_t>(count)), start_time)) {
        *error = "读取目标进程启动时刻失败";
        return false;
    }
    return true;
}

bool verify_process_instance(
    pid_t pid,
    const std::string& package_name,
    std::uint64_t expected_start_time,
    std::string* error) {
    if (!verify_process_identity(pid, package_name, error)) {
        return false;
    }
    std::uint64_t actual_start_time = 0;
    if (!read_process_start_time(pid, &actual_start_time, error)) {
        return false;
    }
    if (actual_start_time != expected_start_time) {
        *error = "目标 PID 已被复用或游戏进程已经重启";
        return false;
    }
    return true;
}

bool verify_thread_identity(
    pid_t pid,
    pid_t tid,
    std::uint64_t expected_start_time,
    std::string* error) {
    const std::string path = "/proc/" + std::to_string(pid) + "/task/" +
                             std::to_string(tid) + "/stat";
    const int descriptor = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        *error = std::string("检查 RPC worker stat 失败: ") + std::strerror(errno);
        return false;
    }
    std::array<char, 1024> buffer{};
    const ssize_t count = read(descriptor, buffer.data(), buffer.size() - 1);
    close(descriptor);
    std::uint64_t actual_start_time = 0;
    if (count <= 0 || !parse_start_time(
                          std::string_view(buffer.data(), static_cast<std::size_t>(count)),
                          &actual_start_time)) {
        *error = "读取 RPC worker 启动时刻失败";
        return false;
    }
    if (actual_start_time != expected_start_time) {
        *error = "RPC worker 已退出或 TID 已被其他线程复用";
        return false;
    }
    return true;
}

bool verify_thread_identity_gone(
    pid_t pid,
    pid_t tid,
    std::uint64_t expected_start_time,
    std::string* error) {
    const std::string path = "/proc/" + std::to_string(pid) + "/task/" +
                             std::to_string(tid) + "/stat";
    const int descriptor = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        if (errno == ENOENT) {
            return true;
        }
        *error = std::string("检查 RPC worker stat 失败: ") + std::strerror(errno);
        return false;
    }
    std::array<char, 1024> buffer{};
    const ssize_t count = read(descriptor, buffer.data(), buffer.size() - 1);
    close(descriptor);
    std::uint64_t actual_start_time = 0;
    if (count <= 0 || !parse_start_time(
                          std::string_view(buffer.data(), static_cast<std::size_t>(count)),
                          &actual_start_time)) {
        *error = "读取 RPC worker 启动时刻失败";
        return false;
    }
    if (actual_start_time == expected_start_time) {
        *error = "RPC worker 仍在运行，拒绝冻结终结";
        return false;
    }
    return true;
}

// 普通运行资产一律拒绝符号链接，防止校验对象在路径解析时被替换。
bool verify_elf_x86_64(const std::string& path, std::string* error) {
    return verify_elf_x86_64_impl(path, true, error);
}

// 规范路径、目录归属和文件权限共同限制资产来源，不信任调用方路径文本。
bool verify_session_asset_paths(
    const BootstrapConfigV2& config,
    const std::string& session_file,
    const std::string& agent_file,
    std::string* canonical_agent,
    std::string* error) {
    return verify_session_asset_paths_impl(
        bootstrap_session_id_hex(config), session_file, agent_file, canonical_agent, error);
}

bool verify_session_asset_paths(
    const AgentUnloadConfigV1& config,
    const std::string& session_file,
    const std::string& agent_file,
    std::string* canonical_agent,
    std::string* error) {
    return verify_session_asset_paths_impl(
        unload_session_id_hex(config), session_file, agent_file, canonical_agent, error);
}

bool verify_file_sha256(
    const std::string& path,
    const std::string& expected_sha256,
    std::uint32_t timeout_ms,
    std::string* error) {
    std::string actual_sha256;
    if (!sha256sum(path, timeout_ms, &actual_sha256, error)) {
        return false;
    }
    if (actual_sha256 != expected_sha256) {
        *error = "文件 SHA-256 不匹配，拒绝继续";
        return false;
    }
    return true;
}

// 只选取文件偏移为零的模块映射，再核对整文件摘要。
bool find_and_verify_module(
    pid_t pid,
    const std::string& module_name,
    const std::string& expected_sha256,
    std::uint32_t timeout_ms,
    std::string* module_path,
    std::string* error) {
    const auto maps = KittyMemoryEx::getMaps(pid, KittyMemoryEx::EProcMapFilter::EndWith, module_name);
    for (const auto& map : maps) {
        if (map.offset == 0 && !map.pathname.empty()) {
            *module_path = map.pathname;
            break;
        }
    }
    if (module_path->empty()) {
        *error = "目标进程未加载模块 " + module_name;
        return false;
    }

    return verify_file_sha256(*module_path, expected_sha256, timeout_ms, error);
}

}  // namespace azlw::loader

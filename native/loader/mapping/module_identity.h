// 声明目标进程、ELF、会话资产路径和目标模块的身份核验接口。

#pragma once

#include <cstdint>
#include <string>

#include <sys/types.h>

#include "bootstrap_config.h"

namespace azlw::loader {

/// 核对目标 PID 的包名和可执行文件架构。
bool verify_process_identity(pid_t pid, const std::string& package_name, std::string* error);
/// 读取目标主线程 `/proc` 启动时刻，用于排除 PID 复用。
bool read_process_start_time(pid_t pid, std::uint64_t* start_time, std::string* error);
/// 同时核对 PID 的包名、架构与启动时刻，拒绝作用于复用后的进程。
bool verify_process_instance(
    pid_t pid,
    const std::string& package_name,
    std::uint64_t expected_start_time,
    std::string* error);
/// 要求目标进程内的精确线程 TID 和启动时刻仍然匹配。
bool verify_thread_identity(
    pid_t pid,
    pid_t tid,
    std::uint64_t expected_start_time,
    std::string* error);
/// 确认收据中的精确 RPC worker 已退出；同一 TID 被新线程复用不视为残留。
bool verify_thread_identity_gone(
    pid_t pid,
    pid_t tid,
    std::uint64_t expected_start_time,
    std::string* error);
/// 拒绝符号链接，并确认指定资产是小端 x86_64 ELF。
bool verify_elf_x86_64(const std::string& path, std::string* error);
/// 将启动文件与 agent 限制在当前 root 会话目录，并返回规范 agent 路径。
bool verify_session_asset_paths(
    const BootstrapConfigV2& config,
    const std::string& session_file,
    const std::string& agent_file,
    std::string* canonical_agent,
    std::string* error);
/// 将卸载文件与 agent 限制在同一 root 会话目录，并返回规范 agent 路径。
bool verify_session_asset_paths(
    const AgentUnloadConfigV1& config,
    const std::string& session_file,
    const std::string& agent_file,
    std::string* canonical_agent,
    std::string* error);
/// 在硬超时内核对普通文件的 SHA-256。
bool verify_file_sha256(
    const std::string& path,
    const std::string& expected_sha256,
    std::uint32_t timeout_ms,
    std::string* error);
/// 查找目标进程已加载模块，并在超时内核对其 SHA-256。
bool find_and_verify_module(
    pid_t pid,
    const std::string& module_name,
    const std::string& expected_sha256,
    std::uint32_t timeout_ms,
    std::string* module_path,
    std::string* error);

}  // namespace azlw::loader

// 记录一次加载的阶段事实，并由这些事实决定成功和失败后的清理。

#pragma once

#include <cstdint>
#include <string>

#include "bootstrap_config.h"
#include "mapping/anonymous_remap.h"
#include "mapping/elf_header_protection.h"
#include "mapping/solist_visibility.h"

namespace azlw::loader {

/// 加载事务交给主入口打印收据时使用的退出类别。
enum class LoadExit : std::uint8_t {
    Ok,
    BootstrapInvalid,
    IdentityRejected,
    AttachFailed,
    InjectorFailed,
    AgentStartFailed,
    CleanupFailed,
};

/// 加载失败后目标进程相对加载前现场的可证明状态。
enum class LoadTargetState : std::uint8_t { Unchanged, Restored, Unknown };

/// 成功加载后写回收据所需的句柄、映射和可见性证据。
struct LoadedAgentEvidence final {
    std::uint64_t process_start_time = 0;
    std::uintptr_t agent_handle = 0;
    std::uintptr_t agent_base = 0;
    std::uintptr_t agent_load_size = 0;
    std::uintptr_t finalize_address = 0;
    std::string agent_mapping_name;
    azlw::AgentMappingMode mapping_mode = azlw::AgentMappingMode::Memfd;
    AnonymousRemapEvidence remap_evidence{};
    azlw::AgentVisibilityMode visibility_mode = azlw::AgentVisibilityMode::Normal;
    SolistVisibilityEvidence solist_evidence{};
    ElfHeaderProtectionEvidence elf_header_evidence{};
};

/// 一次加载的稳定结果。收据文本和进程退出码仍由 loader 主入口输出。
struct LoadOutcome final {
    LoadExit exit_kind = LoadExit::InjectorFailed;
    std::string stable_code;
    std::string message;
    std::int32_t process_id = 0;
    LoadTargetState target_state = LoadTargetState::Unchanged;
    LoadedAgentEvidence loaded{};
};

/// 失败加载允许的清理。未知或已启动都不能按未启动现场回滚。
enum class FailedLoadCleanup : std::uint8_t {
    RestoreUnstartedAgent,
    PreserveUnresolvedScene,
};

/// 一次加载回调和注入结果共用的阶段事实。
struct LoadTransaction final {
    bool callback_invoked = false;
    bool agent_started = false;
    bool agent_identity_ready = false;
    bool anonymous_transaction_invoked = false;
    bool solist_transaction_invoked = false;
    bool remote_close_attempted = false;
    bool remote_bootstrap_write_attempted = false;
    bool remote_bootstrap_scrubbed = false;
    bool injector_stack_snapshot_ready = false;
    bool injector_stack_restored = false;
    bool remote_scratch_evidence_failed = false;
    bool agent_start_state_unknown = false;
    std::int32_t agent_start_code = -1;
    std::uintptr_t finalize_address = 0;
    std::uintptr_t agent_base = 0;
    std::uintptr_t agent_load_size = 0;
    std::string agent_mapping_name;
    AnonymousRemapResult remap_result = AnonymousRemapResult::Restored;
    AnonymousRemapEvidence remap_evidence{};
    SolistVisibilityState solist_state = SolistVisibilityState::Linked;
    SolistVisibilityEvidence solist_evidence{};
    ElfHeaderProtectionState elf_header_state = ElfHeaderProtectionState::Original;
    ElfHeaderProtectionEvidence elf_header_evidence{};

    /// 远程启动区和注入器栈都已写后复核，且没有证据失败。
    [[nodiscard]] bool remote_scratch_ready() const noexcept {
        return remote_bootstrap_write_attempted && remote_bootstrap_scrubbed &&
               injector_stack_snapshot_ready && injector_stack_restored &&
               !remote_scratch_evidence_failed;
    }

    /// 注入结果、回调、启动码和远程暂存证据同时成立才算加载完成。
    [[nodiscard]] bool load_completed(bool injected_valid) const noexcept {
        return injected_valid && callback_invoked && agent_started && remote_scratch_ready();
    }

    /// 只有明确尚未启动时才允许恢复映射并关闭载体。
    [[nodiscard]] FailedLoadCleanup failed_cleanup() const noexcept {
        if (!agent_started && !agent_start_state_unknown) {
            return FailedLoadCleanup::RestoreUnstartedAgent;
        }
        return FailedLoadCleanup::PreserveUnresolvedScene;
    }

    /// 保留现场时的稳定诊断摘要。未知启动优先于“已启动但证据不完整”。
    [[nodiscard]] const char* preserve_scene_summary() const noexcept {
        return agent_start_state_unknown ? "Agent 启动状态未知"
                                         : "Agent 已启动但远程清理证据不完整";
    }

    /// 按校验、冻结、注入、入口前回调和失败清理的顺序推进这一次加载。
    [[nodiscard]] LoadOutcome execute(
        std::int32_t process_id,
        const std::string& agent_path,
        const std::string& session_file);
};

}  // namespace azlw::loader

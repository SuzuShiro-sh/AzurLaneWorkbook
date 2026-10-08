// 执行一次加载事务：入口前回调、远程证据和失败清理都写在同一个 LoadTransaction 上。

#include "load_transaction.h"

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <limits>
#include <span>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include <sys/types.h>

#include <KittyMemoryMgr.hpp>

#include "Injector/KittyInjector.hpp"
#include "mapping/anonymous_remap_kitty.h"
#include "mapping/elf_header_protection_kitty.h"
#include "mapping/module_identity.h"
#include "mapping/solist_visibility_kitty.h"
#include "process/injector_policy.h"
#include "process/remote_memory_evidence.h"
#include "process/thread_freeze.h"
#include "secure_memory.h"
#include "session/bootstrap_file.h"

namespace azlw::loader {
namespace {

/// 将 AndKitty 的实际传输字节数接入 loader 自己的写后读回证据边界。
class KittyRemoteMemoryEvidenceBackend final : public RemoteMemoryEvidenceBackend {
public:
    explicit KittyRemoteMemoryEvidenceBackend(KittyMemoryMgr* memory) : memory_(memory) {}

    std::size_t read(std::uintptr_t address, void* output, std::size_t size) override {
        return memory_->readMem(address, output, size);
    }

    std::size_t write(
        std::uintptr_t address,
        const void* input,
        std::size_t size) override {
        return memory_->writeMem(address, const_cast<void*>(input), size);
    }

private:
    KittyMemoryMgr* memory_;
};

/// 在保留首要失败原因的同时追加回滚证据。
void append_diagnostic(
    std::string* message,
    const std::string& context,
    const std::string& detail) {
    if (!message->empty()) {
        *message += "；";
    }
    *message += context + ": " + detail;
}

/// 在进程身份不变的前提下，确认一次完整可读的当前线程快照均已脱离 ptrace。
bool verify_target_restored(
    pid_t pid,
    std::uint64_t expected_start_time,
    const std::string& package_name,
    std::string* error) {
    if (!verify_process_instance(pid, package_name, expected_start_time, error)) {
        return false;
    }

    pid_t last_unreadable_tid = 0;
    constexpr int kMaximumSnapshotAttempts = 16;
    for (int attempt = 0; attempt < kMaximumSnapshotAttempts; ++attempt) {
        const std::vector<pid_t> threads = KittyMemoryEx::getAllThreads(pid);
        if (threads.empty() ||
            std::find(threads.begin(), threads.end(), pid) == threads.end()) {
            last_unreadable_tid = pid;
        } else {
            bool complete_snapshot = true;
            for (const pid_t tid : threads) {
                KittyMemoryEx::ProcStatus status{};
                if (!KittyMemoryEx::ProcStatus::parse(pid, tid, &status) ||
                    !status.contains("TracerPid")) {
                    last_unreadable_tid = tid;
                    complete_snapshot = false;
                    break;
                }
                const long long tracer_pid = status.getInt("TracerPid");
                if (tracer_pid != 0) {
                    *error = "分离后 tid=" + std::to_string(tid) + " 仍由 tracer=" +
                             std::to_string(tracer_pid) + " 跟踪";
                    return false;
                }
            }
            if (complete_snapshot) {
                return verify_process_instance(
                    pid, package_name, expected_start_time, error);
            }
        }
        if (attempt + 1 < kMaximumSnapshotAttempts) {
            std::this_thread::sleep_for(std::chrono::milliseconds(2));
        }
    }
    *error = "分离后在固定复核预算内持续无法读取 tid=" +
             std::to_string(last_unreadable_tid) + " 的完整线程状态";
    return false;
}

/// 只在显式分离和独立现场复核均成功时声明已恢复。
LoadTargetState rollback_ptrace(
    ThreadFreeze* freeze,
    pid_t pid,
    std::uint64_t process_start_time,
    const std::string& package_name,
    std::string* error) {
    std::string detach_error;
    if (!freeze->detach_all(&detach_error)) {
        append_diagnostic(error, "ptrace 回滚失败", detach_error);
        return LoadTargetState::Unknown;
    }
    std::string verification_error;
    if (!verify_target_restored(
            pid,
            process_start_time,
            package_name,
            &verification_error)) {
        append_diagnostic(error, "回滚后现场复核失败", verification_error);
        return LoadTargetState::Unknown;
    }
    return LoadTargetState::Restored;
}

/// 目标可能已经发生远程写入时仍显式释放 ptrace，但不把分离等同于现场恢复。
void detach_changed_target(ThreadFreeze* freeze, std::string* error) {
    std::string detach_error;
    if (!freeze->detach_all(&detach_error)) {
        append_diagnostic(error, "ptrace 清理失败", detach_error);
    }
}

LoadOutcome failed_load(
    LoadExit exit_kind,
    const char* stable_code,
    std::string message,
    pid_t pid,
    LoadTargetState target_state) {
    LoadOutcome outcome;
    outcome.exit_kind = exit_kind;
    outcome.stable_code = stable_code;
    outcome.message = std::move(message);
    outcome.process_id = static_cast<std::int32_t>(pid);
    outcome.target_state = target_state;
    return outcome;
}

/// 尚未启动时按 ELF 头、solist、匿名映射和载体句柄的原顺序恢复。
void restore_unstarted_agent(
    LoadTransaction& load,
    KittyMemoryMgr& memory,
    KittyInjector& injector,
    const inject_elf_info_t& injected,
    const ElfHeaderBytes& original_elf_header,
    std::string* error) {
    if (load.elf_header_state == ElfHeaderProtectionState::Protected) {
        std::string header_error;
        load.elf_header_state = restore_kitty_elf_header(
            &memory,
            load.agent_base,
            original_elf_header,
            load.elf_header_evidence,
            &header_error);
        if (load.elf_header_state != ElfHeaderProtectionState::Original) {
            append_diagnostic(error, "Agent ELF 头恢复失败", header_error);
        }
    }
    if (load.elf_header_state == ElfHeaderProtectionState::Original &&
        load.solist_state == SolistVisibilityState::Hidden) {
        std::string visibility_error;
        load.solist_state = restore_kitty_solisted_tail(
            &memory, load.solist_evidence, &visibility_error);
        if (load.solist_state != SolistVisibilityState::Linked) {
            append_diagnostic(error, "Agent solist 恢复失败", visibility_error);
        }
    }
    if (load.remap_result != AnonymousRemapResult::Unknown &&
        load.elf_header_state == ElfHeaderProtectionState::Original &&
        load.solist_state == SolistVisibilityState::Linked && injected.dl_handle != 0 &&
        injector._rdlclose != 0) {
        memory.trace.callFunctionFrom(
            injector._dl_caller, injector._rdlclose, injected.dl_handle);
    } else if (load.remap_result == AnonymousRemapResult::Unknown) {
        append_diagnostic(
            error,
            "Agent 清理降级",
            "匿名重映射现场未知，跳过可能扩大损坏的远程 dlclose");
    } else if (load.solist_state != SolistVisibilityState::Linked) {
        append_diagnostic(
            error,
            "Agent 清理降级",
            "solist 现场未证明接回，跳过不安全的远程 dlclose");
    } else if (load.elf_header_state != ElfHeaderProtectionState::Original) {
        append_diagnostic(
            error,
            "Agent 清理降级",
            "ELF 头现场未证明恢复，跳过不安全的远程 dlclose");
    }
    if (injected.memfd >= 0 && !load.anonymous_transaction_invoked &&
        !load.remote_close_attempted) {
        std::string close_error;
        if (!close_kitty_remote_memfd(
                &injector._rsyscall, injected.memfd, &close_error)) {
            append_diagnostic(error, "远程 memfd 清理失败", close_error);
        }
    }
}

/// 在 ELF 入口点之前写入启动配置，并把每个阶段事实记到当前加载事务。
void prepare_agent_entry(
    LoadTransaction& load,
    inject_elf_info_t& injected,
    KittyInjector& injector,
    KittyMemoryMgr& memory,
    RemoteMemoryEvidenceBackend& remote_memory,
    BootstrapFile& bootstrap,
    AgentMappingMode mapping_mode,
    AgentVisibilityMode visibility_mode,
    const ElfHeaderBytes& original_elf_header,
    std::string* error) {
    load.callback_invoked = true;
    const uintptr_t start_address = injected.elf.findSymbol("azlw_agent_start");
    load.finalize_address = injected.elf.findSymbol("azlw_agent_finalize_shutdown");
    if (start_address == 0 || load.finalize_address == 0 || injector._rbuffer == 0) {
        *error = "agent 缺少启动或终结导出，或远程缓冲区不可用";
        return;
    }

    const KittyMemoryEx::ProcMap agent_base_segment = injected.elf.baseSegment();
    load.agent_mapping_name = agent_base_segment.pathname;
    load.agent_base = injected.elf.base();
    load.agent_load_size = injected.elf.loadSize();
    const auto agent_segments = injected.elf.segments();
    const bool finalizer_executable = std::any_of(
        agent_segments.begin(),
        agent_segments.end(),
        [&load](const KittyMemoryEx::ProcMap& map) {
            return map.executable && map.contains(load.finalize_address);
        });
    if (!agent_base_segment.isValid() || load.agent_mapping_name.rfind("/memfd:", 0) != 0 ||
        load.agent_mapping_name.size() >= kAgentMappingNameSize || load.agent_base == 0 ||
        load.agent_load_size == 0 ||
        load.agent_base > std::numeric_limits<std::uintptr_t>::max() - load.agent_load_size ||
        load.finalize_address < load.agent_base ||
        load.finalize_address >= load.agent_base + load.agent_load_size ||
        !finalizer_executable || injected.memfd < 0) {
        *error = "Agent 加载后无法取得完整 memfd 映射、描述符或终结地址";
        return;
    }
    load.agent_identity_ready = true;

    if (mapping_mode == AgentMappingMode::AnonymousRemap) {
        load.anonymous_transaction_invoked = true;
        std::string remap_error;
        load.remap_result = remap_kitty_memfd_anonymously(
            &memory,
            &injector._rsyscall,
            load.agent_mapping_name,
            load.agent_base,
            load.agent_load_size,
            injected.memfd,
            &load.remap_evidence,
            &remap_error);
        if (load.remap_result != AnonymousRemapResult::Applied) {
            *error = "Agent 匿名重映射失败: " + remap_error;
            return;
        }
        injected.memfd = -1;
    } else {
        load.remote_close_attempted = true;
        if (!close_kitty_remote_memfd(&injector._rsyscall, injected.memfd, error)) {
            return;
        }
        injected.memfd = -1;
    }

    if (uses_solist_visibility(visibility_mode)) {
        load.solist_transaction_invoked = true;
        std::string visibility_error;
        load.solist_state = hide_kitty_solisted_tail(
            &memory,
            injected.soinfo.ptr,
            &load.solist_evidence,
            &visibility_error);
        if (load.solist_state != SolistVisibilityState::Hidden) {
            *error = "Agent solist 摘链失败: " + visibility_error;
            return;
        }
    }

    if (uses_elf_header_protection(visibility_mode)) {
        std::string header_error;
        load.elf_header_state = protect_kitty_elf_header(
            &memory,
            load.agent_base,
            original_elf_header,
            &load.elf_header_evidence,
            &header_error);
        if (load.elf_header_state != ElfHeaderProtectionState::Protected) {
            *error = "Agent ELF 头保护失败: " + header_error;
            return;
        }
    }

    constexpr std::uintptr_t kConfigOffset = 1024;
    const std::size_t remote_buffer_size = KT_PAGE_SIZE;
    if (injector._backup_rbuffer.size() != remote_buffer_size ||
        kConfigOffset > remote_buffer_size ||
        sizeof(BootstrapConfigV2) > remote_buffer_size - kConfigOffset ||
        injector._rbuffer >
            std::numeric_limits<std::uintptr_t>::max() - kConfigOffset) {
        *error = "AndKitty 远程栈页没有形成完整备份";
        load.remote_scratch_evidence_failed = true;
        return;
    }
    load.injector_stack_snapshot_ready = true;

    const std::uintptr_t remote_config = injector._rbuffer + kConfigOffset;
    const std::array<std::uint8_t, sizeof(BootstrapConfigV2)> zeros{};
    std::string remote_error;
    if (!verify_remote_memory(&remote_memory, remote_config, zeros, &remote_error)) {
        *error = "AndKitty 远程启动区未证明已清空: " + remote_error;
        load.remote_scratch_evidence_failed = true;
        return;
    }

    const auto config_bytes = std::span<const std::uint8_t>(
        reinterpret_cast<const std::uint8_t*>(&bootstrap.mutable_config()),
        sizeof(BootstrapConfigV2));
    load.remote_bootstrap_write_attempted = true;
    if (!write_and_verify_remote_memory(
            &remote_memory, remote_config, config_bytes, &remote_error)) {
        *error = "写入 agent bootstrap 未形成完整证据: " + remote_error;
        std::string scrub_error;
        load.remote_bootstrap_scrubbed = write_and_verify_remote_memory(
            &remote_memory, remote_config, zeros, &scrub_error);
        if (!load.remote_bootstrap_scrubbed) {
            append_diagnostic(error, "远程 bootstrap 清零失败", scrub_error);
            load.remote_scratch_evidence_failed = true;
        }
        return;
    }

    const kitty_rp_call_t result = memory.trace.callFunctionFrom(
        injector._dl_caller, start_address, remote_config, sizeof(BootstrapConfigV2));
    load.remote_bootstrap_scrubbed = write_and_verify_remote_memory(
        &remote_memory, remote_config, zeros, &remote_error);
    if (!load.remote_bootstrap_scrubbed) {
        append_diagnostic(error, "远程 bootstrap 清零失败", remote_error);
        load.remote_scratch_evidence_failed = true;
    }
    if (result.status != KT_RP_CALL_SUCCESS) {
        load.agent_start_state_unknown = true;
        append_diagnostic(
            error,
            "远程调用 azlw_agent_start 失败",
            "status=" + std::to_string(result.status));
        return;
    }
    load.agent_start_code = static_cast<std::int32_t>(result.result.val);
    load.agent_started = load.agent_start_code == static_cast<std::int32_t>(AgentStartCode::Ok);
    if (!load.agent_started) {
        append_diagnostic(
            error,
            "agent 启动返回稳定错误码",
            std::to_string(load.agent_start_code));
    }
}

}  // namespace

LoadOutcome LoadTransaction::execute(
    std::int32_t process_id,
    const std::string& agent_path,
    const std::string& session_file) {
    LoadTransaction& load = *this;
    const pid_t pid = static_cast<pid_t>(process_id);
    std::string error;

    BootstrapFile bootstrap;
    if (!bootstrap.load(session_file, &error)) {
        return failed_load(
            LoadExit::BootstrapInvalid,
            "bootstrap_invalid",
            std::move(error),
            pid,
            LoadTargetState::Unchanged);
    }
    const BootstrapConfigV2& config = bootstrap.config();
    const std::string package_name = config.package_name;
    const AgentMappingMode mapping_mode = azlw::agent_mapping_mode(config.agent_runtime_policy);
    const AgentVisibilityMode visibility_mode =
        azlw::agent_visibility_mode(config.agent_runtime_policy);
    if (pid != config.target_pid) {
        return failed_load(
            LoadExit::BootstrapInvalid,
            "target_pid_mismatch",
            "CLI PID 与会话文件不一致",
            pid,
            LoadTargetState::Unchanged);
    }

    std::string canonical_agent;
    std::uint64_t process_start_time = 0;
    if (!verify_session_asset_paths(
            config, bootstrap.path(), agent_path, &canonical_agent, &error) ||
        !verify_process_identity(pid, config.package_name, &error) ||
        !read_process_start_time(pid, &process_start_time, &error)) {
        return failed_load(
            LoadExit::IdentityRejected,
            "identity_rejected",
            std::move(error),
            pid,
            LoadTargetState::Unchanged);
    }
    ElfHeaderBytes original_elf_header{};
    if (uses_elf_header_protection(visibility_mode) &&
        !read_agent_elf_header(canonical_agent, &original_elf_header, &error)) {
        return failed_load(
            LoadExit::IdentityRejected,
            "agent_abi_mismatch",
            std::move(error),
            pid,
            LoadTargetState::Unchanged);
    }

    std::string module_path;
    if (!find_and_verify_module(
            pid,
            config.module_name,
            config.module_sha256,
            config.timeout_ms,
            &module_path,
            &error)) {
        return failed_load(
            LoadExit::IdentityRejected,
            "module_rejected",
            std::move(error),
            pid,
            LoadTargetState::Unchanged);
    }

    KittyInjector injector;
    bool needs_native_bridge = false;
    if (!injector.validateElf(canonical_agent, nullptr, &needs_native_bridge) ||
        needs_native_bridge) {
        return failed_load(
            LoadExit::IdentityRejected,
            "agent_abi_mismatch",
            "agent 必须是原生 x86_64 ELF",
            pid,
            LoadTargetState::Unchanged);
    }

    ThreadFreeze freeze;
    if (!freeze.attach_all(pid, config.timeout_ms, &error)) {
        const LoadTargetState target_state = rollback_ptrace(
            &freeze,
            pid,
            process_start_time,
            config.package_name,
            &error);
        return failed_load(
            LoadExit::AttachFailed,
            "ptrace_attach_failed",
            std::move(error),
            pid,
            target_state);
    }
    if (!verify_process_instance(
            pid,
            config.package_name,
            process_start_time,
            &error)) {
        const LoadTargetState target_state = rollback_ptrace(
            &freeze,
            pid,
            process_start_time,
            config.package_name,
            &error);
        return failed_load(
            LoadExit::IdentityRejected,
            "process_identity_changed",
            std::move(error),
            pid,
            target_state);
    }

    KittyMemoryMgr memory;
    memory.trace = KittyTraceMgr(pid, 0, true, static_cast<int>(config.timeout_ms));
    if (!memory.initialize(pid, EK_MEM_OP_SYSCALL, false)) {
        error = "AndKitty 内存管理初始化失败";
        const LoadTargetState target_state = rollback_ptrace(
            &freeze,
            pid,
            process_start_time,
            config.package_name,
            &error);
        return failed_load(
            LoadExit::InjectorFailed,
            "memory_init_failed",
            std::move(error),
            pid,
            target_state);
    }
    KittyRemoteMemoryEvidenceBackend remote_memory(&memory);

    inject_elf_config_t injector_config =
        make_injector_config(
            KittyUtils::Android::getSDK(),
            config.timeout_ms,
            mapping_id_hex(config));
    injector_config.beforeEntryPoint = [&](inject_elf_info_t& injected) {
        prepare_agent_entry(
            load,
            injected,
            injector,
            memory,
            remote_memory,
            bootstrap,
            mapping_mode,
            visibility_mode,
            original_elf_header,
            &error);
    };

    if (!injector.init(&memory, injector_config)) {
        error = "AndKitty loader 初始化失败";
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::InjectorFailed,
            "injector_init_failed",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }

    const inject_elf_info_t injected = injector.inject(canonical_agent);
    if (load.injector_stack_snapshot_ready) {
        std::string restore_error;
        load.injector_stack_restored = verify_remote_memory(
            &remote_memory,
            injector._rbuffer,
            std::span<const std::uint8_t>(
                injector._backup_rbuffer.data(), injector._backup_rbuffer.size()),
            &restore_error);
        if (!load.injector_stack_restored) {
            append_diagnostic(&error, "AndKitty 远程栈页恢复未通过复核", restore_error);
            load.remote_scratch_evidence_failed = true;
        }
    }
    if (!injector._backup_rbuffer.empty()) {
        secure_zero(injector._backup_rbuffer.data(), injector._backup_rbuffer.size());
        injector._backup_rbuffer.clear();
    }
    if (!load.load_completed(injected.is_valid())) {
        if (load.failed_cleanup() == FailedLoadCleanup::RestoreUnstartedAgent) {
            restore_unstarted_agent(
                load, memory, injector, injected, original_elf_header, &error);
        } else {
            append_diagnostic(
                &error,
                load.preserve_scene_summary(),
                "保留当前现场并要求宿主重启，跳过不安全的直接 dlclose");
        }
        if (error.empty()) {
            error = "agent dlopen 或启动回调未完成";
        }
        detach_changed_target(&freeze, &error);
        const LoadExit failure_code = load.remote_scratch_evidence_failed
                                          ? LoadExit::CleanupFailed
                                          : LoadExit::AgentStartFailed;
        const char* stable_code = load.remote_scratch_evidence_failed
                                      ? "remote_scratch_cleanup_failed"
                                      : "agent_start_failed";
        return failed_load(failure_code, stable_code, std::move(error), pid, LoadTargetState::Unknown);
    }
    if (!load.agent_identity_ready) {
        error = "Agent 加载后无法取得完整 memfd 映射或终结地址";
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::AgentStartFailed,
            "agent_identity_incomplete",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }
    if (uses_solist_visibility(visibility_mode) &&
        (!load.solist_transaction_invoked || load.solist_state != SolistVisibilityState::Hidden ||
         load.solist_evidence.soinfo_address == 0)) {
        error = "Agent 加载后缺少完整 solist 隐藏证据";
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::AgentStartFailed,
            "agent_identity_incomplete",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }
    if (uses_elf_header_protection(visibility_mode) &&
        (load.elf_header_state != ElfHeaderProtectionState::Protected ||
         std::all_of(
             load.elf_header_evidence.protected_header.begin(),
             load.elf_header_evidence.protected_header.end(),
             [](std::uint8_t value) { return value == 0; }))) {
        error = "Agent 加载后缺少完整 ELF 头保护证据";
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::AgentStartFailed,
            "agent_identity_incomplete",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }

    if (!bootstrap.remove_now(&error)) {
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::CleanupFailed,
            "session_file_cleanup_failed",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }
    secure_zero(&bootstrap.mutable_config(), sizeof(BootstrapConfigV2));

    std::uint64_t final_process_start_time = 0;
    if (!read_process_start_time(pid, &final_process_start_time, &error) ||
        final_process_start_time != process_start_time) {
        error = error.empty() ? "加载期间目标进程身份发生变化" : error;
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::IdentityRejected,
            "process_identity_changed",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }
    if (!freeze.detach_all(&error)) {
        detach_changed_target(&freeze, &error);
        return failed_load(
            LoadExit::CleanupFailed,
            "ptrace_detach_failed",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }
    std::string detach_verification_error;
    if (!verify_target_restored(
            pid,
            process_start_time,
            package_name,
            &detach_verification_error)) {
        error = "ptrace 分离后现场复核失败: " + detach_verification_error;
        return failed_load(
            LoadExit::CleanupFailed,
            "ptrace_detach_failed",
            std::move(error),
            pid,
            LoadTargetState::Unknown);
    }

    LoadOutcome outcome;
    outcome.exit_kind = LoadExit::Ok;
    outcome.stable_code = "loaded";
    outcome.message = "专用 agent 已加载并启动";
    outcome.process_id = static_cast<std::int32_t>(pid);
    outcome.loaded.process_start_time = process_start_time;
    outcome.loaded.agent_handle = injected.dl_handle;
    outcome.loaded.agent_base = load.agent_base;
    outcome.loaded.agent_load_size = load.agent_load_size;
    outcome.loaded.finalize_address = load.finalize_address;
    outcome.loaded.agent_mapping_name = load.agent_mapping_name;
    outcome.loaded.mapping_mode = mapping_mode;
    outcome.loaded.remap_evidence = load.remap_evidence;
    outcome.loaded.visibility_mode = visibility_mode;
    outcome.loaded.solist_evidence = load.solist_evidence;
    outcome.loaded.elf_header_evidence = load.elf_header_evidence;
    return outcome;
}

}  // namespace azlw::loader

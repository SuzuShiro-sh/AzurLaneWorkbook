// 实现 Agent 身份复核、线程静止证明、Hook 终结和远程 dlclose 事务。

#include "agent_unloader.h"

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include <KittyMemoryMgr.hpp>

#include "Injector/KittyInjector.hpp"
#include "mapping/agent_mapping_policy.h"
#include "bootstrap_config.h"
#include "mapping/elf_header_protection_kitty.h"
#include "process/injector_policy.h"
#include "mapping/module_identity.h"
#include "process/remote_call_stack.h"
#include "mapping/solist_visibility_kitty.h"
#include "process/thread_freeze.h"
#include "process/tracee_ptrace.h"
#include "process/tracee_wait_status.h"
#include "session/unload_file.h"

namespace azlw::loader {
namespace {

constexpr std::size_t kFinalizerStackBytes = 256 * 1024;

/// 保留远程调用失败阶段与原始 errno，避免把不同寄存器现场压成同一个状态码。
std::string remote_call_failure_diagnostic(
    const kitty_rp_call_t& result,
    const RemoteCallPtraceDiagnostic& ptrace_diagnostic) {
    const char* stage = ptrace_diagnostic.register_failure_observed
                            ? remote_call_register_stage_name(ptrace_diagnostic.stage)
                            : "unobserved";
    std::string diagnostic = "status=" + std::to_string(result.status) + ", stage=" + stage +
                             ", errno=" + std::to_string(ptrace_diagnostic.system_error) +
                             ", eintr_retries=" +
                             std::to_string(ptrace_diagnostic.eintr_retries);
    if (ptrace_diagnostic.system_error != 0) {
        diagnostic +=
            " (" + std::string(std::strerror(ptrace_diagnostic.system_error)) + ")";
    }
    if (ptrace_diagnostic.restore_failure_observed &&
        ptrace_diagnostic.stage != RemoteCallRegisterStage::RestoreRegsWrite) {
        diagnostic += ", restore_errno=" +
                      std::to_string(ptrace_diagnostic.restore_system_error);
        if (ptrace_diagnostic.restore_system_error != 0) {
            diagnostic += " (" +
                          std::string(std::strerror(ptrace_diagnostic.restore_system_error)) +
                          ")";
        }
    }
    return diagnostic;
}

constexpr char kTargetTerminationRequestMessage[] = "已请求终止旧目标进程";

void note_target_termination_request(std::string* message) {
    if (message == nullptr ||
        message->find(kTargetTerminationRequestMessage) != std::string::npos) {
        return;
    }
    if (!message->empty()) {
        *message += "；";
    }
    *message += kTargetTerminationRequestMessage;
}

void terminate_target_process(ThreadFreeze* freeze, std::string* message) {
    freeze->terminate_target();
    note_target_termination_request(message);
}

std::string target_termination_request_message(std::string message) {
    note_target_termination_request(&message);
    return message;
}

/// 每次破坏性操作前都重新核对 PID 启动时刻，拒绝作用于复用后的进程。
bool verify_unload_process_instance(const AgentUnloadConfigV1& config, std::string* error) {
    return verify_process_instance(
        config.target_pid, config.package_name, config.process_start_time, error);
}

/// 精确映射名、基址、加载跨度和终结函数执行权限必须同时匹配。
bool verify_agent_mapping(const AgentUnloadConfigV1& config, std::string* error) {
    const std::string mapping_name(config.agent_mapping_name);
    const AgentMappingMode mapping_mode = azlw::agent_mapping_mode(config.agent_runtime_policy);
    const std::uintptr_t agent_start = static_cast<std::uintptr_t>(config.agent_base);
    const std::uintptr_t agent_end =
        agent_start + static_cast<std::uintptr_t>(config.agent_load_size);
    const auto maps = KittyMemoryEx::getAllMaps(config.target_pid);
    if (maps.empty()) {
        *error = "目标进程映射为空，无法复核 Agent 身份";
        return false;
    }
    bool base_covered = false;
    bool finalizer_executable = false;
    for (const KittyMemoryEx::ProcMap& map : maps) {
        const AgentMappingIdentity identity{
            .start_address = map.startAddress,
            .end_address = map.endAddress,
            .readable = map.readable,
            .writeable = map.writeable,
            .executable = map.executable,
            .is_private = map.is_private,
            .is_shared = map.is_shared,
            .offset = map.offset,
            .device = map.dev,
            .inode = map.inode,
            .pathname = map.pathname,
        };
        const bool exact_memfd = matches_agent_mapping_name(map.pathname, mapping_name);
        if (mapping_mode == AgentMappingMode::AnonymousRemap && exact_memfd) {
            *error = "匿名模式下仍存在收据指定的 Agent memfd 映射";
            return false;
        }
        const bool overlaps = map.startAddress < agent_end && map.endAddress > agent_start;
        if (mapping_mode == AgentMappingMode::Memfd && !exact_memfd) {
            continue;
        }
        if (mapping_mode == AgentMappingMode::AnonymousRemap && !overlaps) {
            continue;
        }
        if (mapping_mode == AgentMappingMode::Memfd &&
            (map.startAddress < agent_start || map.endAddress > agent_end)) {
            *error = "Agent memfd 映射超出收据声明的加载区间";
            return false;
        }
        if (mapping_mode == AgentMappingMode::AnonymousRemap &&
            !is_live_anonymous_agent_mapping(identity)) {
            *error = "Agent 加载区间包含非私有匿名映射: " + map.toString();
            return false;
        }
        base_covered = base_covered || map.contains(agent_start);
        finalizer_executable = finalizer_executable ||
                               (map.executable && map.contains(config.finalize_address));
    }
    if (!base_covered || !finalizer_executable) {
        *error = mapping_mode == AgentMappingMode::Memfd
                     ? "Agent 基址或终结函数不属于精确 memfd 映射"
                     : "Agent 基址或终结函数不属于私有匿名映射";
        return false;
    }
    return true;
}

/// 按当前模块的真实导出地址核对 Hook，不依赖构建时的函数偏移。
bool verify_hook_target(
    const AgentUnloadConfigV1& config, const std::string& module_path, std::string* error) {
    KittyMemoryMgr memory;
    if (!memory.initialize(config.target_pid, EK_MEM_OP_SYSCALL, false)) {
        *error = "读取目标模块导出表失败";
        return false;
    }
    const auto maps = KittyMemoryEx::getMaps(
        config.target_pid, KittyMemoryEx::EProcMapFilter::EndWith, config.module_name);
    for (const auto& map : maps) {
        if (map.offset != 0 || map.pathname != module_path) {
            continue;
        }
        auto module = memory.elfScanner.createWithBase(map.startAddress);
        if (!module.isValid() || module.findSymbol("tolua_update") != config.hook_target) {
            continue;
        }
        for (const auto& segment : maps) {
            if (segment.pathname == module_path && segment.executable &&
                config.hook_target >= segment.startAddress &&
                config.hook_target < segment.endAddress) {
                return true;
            }
        }
    }
    *error = "Hook 地址与当前模块的 tolua_update 导出地址不一致";
    return false;
}

/// 冻结后重新核对模块规范路径、整文件摘要和 Hook 地址，拒绝同名重载竞态。
bool verify_frozen_module(
    const AgentUnloadConfigV1& config,
    const std::string& expected_path,
    std::uint32_t timeout_ms,
    std::string* error) {
    std::string actual_path;
    if (!find_and_verify_module(
            config.target_pid,
            config.module_name,
            config.module_sha256,
            timeout_ms,
            &actual_path,
            error)) {
        return false;
    }
    if (actual_path != expected_path) {
        *error = "冻结后目标模块路径发生变化，拒绝还原 Hook";
        return false;
    }
    return verify_hook_target(config, actual_path, error);
}

std::vector<ProtectedAddressRange> protected_ranges(const AgentUnloadConfigV1& config) {
    return {
        ProtectedAddressRange{
            .start = static_cast<std::uintptr_t>(config.agent_base),
            .end = static_cast<std::uintptr_t>(config.agent_base + config.agent_load_size),
            .label = "Agent 加载区间",
        },
        ProtectedAddressRange{
            .start = static_cast<std::uintptr_t>(config.hook_target),
            .end = static_cast<std::uintptr_t>(config.hook_target + kHookPrologueSize),
            .label = "tolua_update Hook 覆盖区间",
        },
        ProtectedAddressRange{
            .start = static_cast<std::uintptr_t>(config.trampoline_start),
            .end = static_cast<std::uintptr_t>(config.trampoline_start + config.trampoline_size),
            .label = "Hook trampoline",
        },
    };
}

/// Hook 终结后必须恢复冻结 profile 字节并释放匿名 trampoline。
bool verify_finalized_memory(
    KittyMemoryMgr* memory,
    const AgentUnloadConfigV1& config,
    std::string* error) {
    std::array<std::uint8_t, kHookPrologueSize> prologue{};
    if (memory->readMem(config.hook_target, prologue.data(), prologue.size()) != prologue.size() ||
        std::memcmp(prologue.data(), config.expected_prologue, prologue.size()) != 0) {
        *error = "Agent 终结后 tolua_update 前导字节未完整恢复";
        return false;
    }
    const KittyMemoryEx::ProcMap trampoline =
        KittyMemoryEx::getAddressMap(config.target_pid, config.trampoline_start);
    if (trampoline.isValid() && trampoline.contains(config.trampoline_start)) {
        *error = "Agent 终结后匿名 trampoline 映射仍然存在";
        return false;
    }
    return true;
}

/// dlclose 后拒绝 Agent 身份或可访问内容，仅容许地址被不可访问匿名页重新占用。
bool verify_agent_mapping_absent(
    const AgentUnloadConfigV1& config,
    std::size_t* inert_overlap_count,
    std::string* error) {
    *inert_overlap_count = 0;
    const std::uintptr_t start = static_cast<std::uintptr_t>(config.agent_base);
    const std::uintptr_t end = static_cast<std::uintptr_t>(config.agent_base + config.agent_load_size);
    const auto maps = KittyMemoryEx::getAllMaps(config.target_pid);
    if (maps.empty()) {
        *error = "dlclose 后无法读取目标进程映射";
        return false;
    }
    for (const KittyMemoryEx::ProcMap& map : maps) {
        const AgentMappingIdentity identity{
            .start_address = map.startAddress,
            .end_address = map.endAddress,
            .readable = map.readable,
            .writeable = map.writeable,
            .executable = map.executable,
            .is_private = map.is_private,
            .is_shared = map.is_shared,
            .offset = map.offset,
            .device = map.dev,
            .inode = map.inode,
            .pathname = map.pathname,
        };
        const AgentMappingDisposition disposition =
            classify_agent_mapping(identity, start, end, config.agent_mapping_name);
        if (disposition == AgentMappingDisposition::Unrelated) {
            continue;
        }
        if (disposition == AgentMappingDisposition::Reject) {
            *error = "dlclose 后仍存在 Agent 精确映射或加载区间内的可识别/可访问映射: " +
                     map.toString();
            return false;
        }
        ++*inert_overlap_count;
    }
    return true;
}

}  // namespace

AgentUnloadOutcome unload_agent(
    const std::string& agent_path,
    const std::string& session_file) {
    UnloadTransaction transaction;
    return transaction.execute(agent_path, session_file);
}

AgentUnloadOutcome UnloadTransaction::execute(
    const std::string& agent_path,
    const std::string& session_file) {
    UnloadTransaction& transaction = *this;
    UnloadFile unload;
    std::string error;
    if (!unload.load(session_file, &error)) {
        return transaction.result(AgentUnloadStatus::ConfigInvalid, "unload_config_invalid", error);
    }
    const AgentUnloadConfigV1& config = unload.config();
    const std::int32_t process_id = config.target_pid;
    transaction.process_id = process_id;
    const AgentVisibilityMode visibility_mode =
        azlw::agent_visibility_mode(config.agent_runtime_policy);
    const UnloadDeadline deadline(config.timeout_ms);

    std::string canonical_agent;
    std::string module_path;
    if (!verify_session_asset_paths(
            config, unload.path(), agent_path, &canonical_agent, &error) ||
        !verify_unload_process_instance(config, &error)) {
        return transaction.result(
            AgentUnloadStatus::IdentityRejected,
            "unload_identity_rejected",
            error);
    }
    const std::uint32_t agent_hash_timeout_ms = deadline.remaining();
    if (agent_hash_timeout_ms == 0) {
        return transaction.result(
            AgentUnloadStatus::FinalizeFailed,
            "unload_deadline_exceeded",
            "校验 Agent 文件前卸载总时限已耗尽");
    }
    if (!verify_file_sha256(
            canonical_agent, config.agent_sha256, agent_hash_timeout_ms, &error)) {
        return transaction.result(
            AgentUnloadStatus::IdentityRejected,
            "unload_identity_rejected",
            error);
    }
    ElfHeaderBytes original_elf_header{};
    if (uses_elf_header_protection(visibility_mode) &&
        !read_agent_elf_header(canonical_agent, &original_elf_header, &error)) {
        return transaction.result(
            AgentUnloadStatus::IdentityRejected,
            "unload_identity_rejected",
            error);
    }
    const std::uint32_t module_hash_timeout_ms = deadline.remaining();
    if (module_hash_timeout_ms == 0) {
        return transaction.result(
            AgentUnloadStatus::FinalizeFailed,
            "unload_deadline_exceeded",
            "校验目标模块前卸载总时限已耗尽");
    }
    if (!find_and_verify_module(
            config.target_pid,
            config.module_name,
            config.module_sha256,
            module_hash_timeout_ms,
            &module_path,
            &error) ||
        !verify_hook_target(config, module_path, &error) || !verify_agent_mapping(config, &error) ||
        !verify_thread_identity(
            config.target_pid,
            config.rpc_worker_tid,
            config.rpc_worker_start_time,
            &error)) {
        return transaction.result(
            AgentUnloadStatus::IdentityRejected,
            "unload_identity_rejected",
            error);
    }

    const std::vector<ProtectedAddressRange> ranges = protected_ranges(config);
    std::string last_busy_error = "目标线程尚未离开 Agent 执行区间";
    while (const std::uint32_t timeout_ms = deadline.remaining()) {
        ThreadFreeze freeze;
        if (!freeze.attach_all(config.target_pid, timeout_ms, &error)) {
            return transaction.result(
                AgentUnloadStatus::AttachFailed,
                "unload_ptrace_attach_failed",
                error);
        }
        const std::uint32_t identity_timeout_ms = deadline.remaining();
        if (identity_timeout_ms == 0) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_deadline_exceeded",
                "冻结完成后卸载总时限已耗尽");
        }
        if (!verify_unload_process_instance(config, &error) || !verify_agent_mapping(config, &error) ||
            !verify_thread_identity(
                config.target_pid,
                config.rpc_worker_tid,
                config.rpc_worker_start_time,
                &error) ||
            !verify_frozen_module(config, module_path, identity_timeout_ms, &error)) {
            return transaction.result(
                AgentUnloadStatus::IdentityRejected,
                "unload_identity_changed",
                "stage=after_freeze: " + error);
        }
        if (!freeze.verify_instruction_pointers_except(
                ranges, config.rpc_worker_tid, &error)) {
            last_busy_error = error;
            std::string detach_error;
            const std::uint32_t detach_timeout_ms = deadline.remaining();
            if (detach_timeout_ms == 0 ||
                !freeze.detach_all(detach_timeout_ms, &detach_error)) {
                return transaction.result(
                    AgentUnloadStatus::CleanupFailed,
                    "unload_ptrace_detach_failed",
                    detach_timeout_ms == 0 ? "分离线程前卸载总时限已耗尽" : detach_error);
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
            continue;
        }

        const std::uint32_t remote_timeout_ms = deadline.remaining();
        if (remote_timeout_ms == 0) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_deadline_exceeded",
                "远程终结前卸载总时限已耗尽");
        }
        KittyMemoryMgr memory;
        memory.trace = KittyTraceMgr(
            config.target_pid, 0, true, static_cast<int>(remote_timeout_ms));
        if (!memory.initialize(config.target_pid, EK_MEM_OP_SYSCALL, false)) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_memory_init_failed",
                "AndKitty 内存管理初始化失败");
        }
        KittyInjector injector;
        const inject_elf_config_t injector_config =
            make_injector_config(KittyUtils::Android::getSDK(), remote_timeout_ms);
        if (!injector.init(&memory, injector_config)) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_injector_init_failed",
                "AndKitty 远程调用器初始化失败");
        }

        const std::uint32_t finalize_timeout_ms = deadline.remaining();
        if (finalize_timeout_ms == 0) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_deadline_exceeded",
                "调用 Agent finalizer 前卸载总时限已耗尽");
        }
        memory.trace.setRemoteCallTimeout(static_cast<int>(finalize_timeout_ms));
        RemoteCallStack finalizer_stack;
        if (!finalizer_stack.prepare(
                &memory,
                &injector._rsyscall,
                kFinalizerStackBytes,
                &error)) {
            if (finalizer_stack.has_remote_mapping()) {
                error += "；远程栈无法回收";
                terminate_target_process(&freeze, &error);
            }
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_remote_stack_prepare_failed",
                error);
        }
        RemoteCallPtraceCapture finalize_ptrace_capture(config.target_pid);
        const kitty_rp_call_t finalize_result = memory.trace.callFunctionFrom(
            injector._dl_caller, config.finalize_address);
        const RemoteCallPtraceDiagnostic finalize_ptrace_diagnostic =
            finalize_ptrace_capture.finish();
        if (!finalizer_stack.restore_and_release(&error)) {
            if (finalize_ptrace_diagnostic.register_failure_observed) {
                error += "；Agent finalizer 诊断：" + remote_call_failure_diagnostic(
                                                        finalize_result,
                                                        finalize_ptrace_diagnostic);
            }
            if (finalizer_stack.has_remote_mapping()) {
                error += "；远程栈无法回收";
                terminate_target_process(&freeze, &error);
            }
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_remote_stack_cleanup_failed",
                error);
        }
        if (finalize_result.status != KT_RP_CALL_SUCCESS ||
            finalize_ptrace_diagnostic.register_failure_observed) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_finalize_call_failed",
                "远程调用 Agent finalizer 失败，" +
                    remote_call_failure_diagnostic(
                        finalize_result, finalize_ptrace_diagnostic));
        }
        const auto finalize_code = static_cast<AgentFinalizeCode>(
            static_cast<std::int32_t>(finalize_result.result.val));
        if (finalize_code == AgentFinalizeCode::RpcWorkerNotParked ||
            finalize_code == AgentFinalizeCode::CallbackRunning ||
            finalize_code == AgentFinalizeCode::RuntimeBusy) {
            last_busy_error = "Agent finalizer 返回可重试状态 " +
                              std::to_string(finalize_result.result.val);
            std::string detach_error;
            const std::uint32_t detach_timeout_ms = deadline.remaining();
            if (detach_timeout_ms == 0 ||
                !freeze.detach_all(detach_timeout_ms, &detach_error)) {
                return transaction.result(
                    AgentUnloadStatus::CleanupFailed,
                    "unload_ptrace_detach_failed",
                    detach_timeout_ms == 0 ? "分离线程前卸载总时限已耗尽" : detach_error);
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
            continue;
        }
        if (finalize_code != AgentFinalizeCode::Ok) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_finalize_rejected",
                "Agent finalizer 返回稳定错误码 " +
                    std::to_string(finalize_result.result.val));
        }
        if (!verify_finalized_memory(&memory, config, &error) ||
            !freeze.verify_instruction_pointers_except(
                ranges, config.rpc_worker_tid, &error)) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_finalize_incomplete",
                error);
        }
        const std::uintptr_t dl_caller = injector._dl_caller;
        const std::uintptr_t remote_dlclose = injector._rdlclose;
        const std::uintptr_t syscall_gadget = memory.trace.syscallGadget();
        if (dl_caller == 0 || remote_dlclose == 0 || syscall_gadget == 0) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_carrier_primitives_missing",
                "卸载载体所需的外部返回点、dlclose 或 syscall gadget 缺失");
        }

        if (uses_elf_header_protection(visibility_mode)) {
            ElfHeaderProtectionEvidence header_evidence{};
            std::copy(
                std::begin(config.protected_elf_header),
                std::end(config.protected_elf_header),
                header_evidence.protected_header.begin());
            const ElfHeaderProtectionState header_state = restore_kitty_elf_header(
                &memory,
                static_cast<std::uintptr_t>(config.agent_base),
                original_elf_header,
                header_evidence,
                &error);
            if (header_state != ElfHeaderProtectionState::Original) {
                terminate_target_process(&freeze, &error);
                return transaction.result(
                    AgentUnloadStatus::FinalizeFailed,
                    "agent_elf_header_restore_failed",
                    "Agent finalizer 完成后无法安全恢复 ELF 头: " + error);
            }
        }

        if (uses_solist_visibility(visibility_mode)) {
            const SolistVisibilityEvidence visibility_evidence{
                .soinfo_address = static_cast<std::uintptr_t>(config.agent_soinfo_address),
            };
            const SolistVisibilityState visibility_state =
                restore_kitty_solisted_tail(&memory, visibility_evidence, &error);
            if (visibility_state != SolistVisibilityState::Linked) {
                terminate_target_process(&freeze, &error);
                return transaction.result(
                    AgentUnloadStatus::FinalizeFailed,
                    "agent_solist_restore_failed",
                    "Agent finalizer 完成后无法安全接回 linker solist: " + error);
            }
        }

        const std::uint32_t detach_timeout_ms = deadline.remaining();
        if (detach_timeout_ms == 0) {
            std::string message = "分离普通线程前卸载总时限已耗尽";
            if (uses_solist_visibility(visibility_mode)) {
                terminate_target_process(&freeze, &message);
            }
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_deadline_exceeded",
                message);
        }
        const DetachExceptResult detach_result =
            freeze.detach_all_except(config.rpc_worker_tid, detach_timeout_ms, &error);
        const DlcloseCarrierSelection carrier_selection = select_dlclose_carrier(detach_result);
        if (carrier_selection == DlcloseCarrierSelection::Reject &&
            detach_result.status == DetachExceptStatus::CarrierExitedBeforeDlclose) {
            if (uses_solist_visibility(visibility_mode)) {
                terminate_target_process(&freeze, &error);
            }
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_carrier_exited_before_dlclose",
                error);
        }
        if (carrier_selection == DlcloseCarrierSelection::Reject) {
            if (uses_solist_visibility(visibility_mode)) {
                terminate_target_process(&freeze, &error);
            }
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_ptrace_detach_except_carrier_failed",
                error);
        }

        const std::uint32_t close_timeout_ms = deadline.remaining();
        if (close_timeout_ms == 0) {
            std::string message = "调用 dlclose 前卸载总时限已耗尽";
            terminate_target_process(&freeze, &message);
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_deadline_exceeded",
                message);
        }

        transaction.close_path = UnloadClosePath::ParkedCarrier;
        KittyTraceMgr carrier_trace(
            config.rpc_worker_tid,
            dl_caller,
            false,
            static_cast<int>(close_timeout_ms),
            syscall_gadget);
        if (!carrier_trace.isAttached()) {
            terminate_target_process(&freeze, &error);
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_carrier_trace_lost",
                target_termination_request_message("分离普通线程后失去卸载载体的 ptrace 所有权"));
        }
        TraceeWaitStatusCapture wait_capture(config.rpc_worker_tid);
        if (!wait_capture.active()) {
            terminate_target_process(&freeze, &error);
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_wait_capture_failed",
                target_termination_request_message("无法启用卸载载体终态观察器"));
        }
        RemoteCallPtraceCapture close_ptrace_capture(config.rpc_worker_tid);
        const kitty_rp_call_t close_result = carrier_trace.callFunctionFrom(
            dl_caller, remote_dlclose, config.agent_handle);
        const RemoteCallPtraceDiagnostic close_ptrace_diagnostic =
            close_ptrace_capture.finish();
        const TraceeWaitStatus close_wait_status = wait_capture.finish();
        if (close_ptrace_diagnostic.register_failure_observed) {
            terminate_target_process(&freeze, &error);
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_dlclose_failed",
                target_termination_request_message(
                    "卸载载体上的远程 dlclose 寄存器现场无效，" +
                    remote_call_failure_diagnostic(
                        close_result, close_ptrace_diagnostic)));
        }
        if (close_result.status == KT_RP_CALL_SUCCESS && close_wait_status.observed) {
            terminate_target_process(&freeze, &error);
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "unload_wait_state_conflict",
                target_termination_request_message(
                    "远程 dlclose 声明成功，但同时观察到卸载载体终态"));
        }
        if (close_result.status == KT_RP_CALL_SUCCESS && close_result.result.val != 0) {
            terminate_target_process(&freeze, &error);
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_dlclose_failed",
                target_termination_request_message("卸载载体上的远程 dlclose 返回非零结果"));
        }
        if (close_result.status == KT_RP_CALL_EXITED && close_wait_status.observed) {
            if (deadline.remaining() == 0) {
                terminate_target_process(&freeze, &error);
                return transaction.result(
                    AgentUnloadStatus::FinalizeFailed,
                    "unload_deadline_exceeded",
                    "接纳卸载载体终态前卸载总时限已耗尽，已请求终止旧目标进程");
            }
            if (!freeze.accept_exited_retained_thread(
                    config.rpc_worker_tid, close_wait_status.value, &error)) {
                terminate_target_process(&freeze, &error);
                return transaction.result(
                    AgentUnloadStatus::FinalizeFailed,
                    "unload_carrier_exit_invalid",
                    error);
            }
            transaction.close_path = UnloadClosePath::CarrierExitedDuringClose;
        } else if (close_result.status != KT_RP_CALL_SUCCESS) {
            terminate_target_process(&freeze, &error);
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_dlclose_failed",
                target_termination_request_message(
                    "卸载载体上的远程 dlclose 未返回成功，" +
                    remote_call_failure_diagnostic(
                        close_result, close_ptrace_diagnostic)));
        }

        if (transaction.close_path != UnloadClosePath::CarrierExitedDuringClose) {
            const std::uint32_t carrier_exit_timeout_ms = deadline.remaining();
            if (carrier_exit_timeout_ms == 0) {
                terminate_target_process(&freeze, &error);
                return transaction.result(
                    AgentUnloadStatus::FinalizeFailed,
                    "unload_deadline_exceeded",
                    "dlclose 返回后卸载总时限已耗尽，已请求终止旧目标进程");
            }
            if (!freeze.exit_retained_thread(
                    config.rpc_worker_tid,
                    syscall_gadget,
                    carrier_exit_timeout_ms,
                    &error)) {
                terminate_target_process(&freeze, &error);
                return transaction.result(
                    AgentUnloadStatus::CleanupFailed,
                    "unload_carrier_exit_failed",
                    error);
            }
        }
        if (!verify_thread_identity_gone(
                config.target_pid,
                config.rpc_worker_tid,
                config.rpc_worker_start_time,
                &error)) {
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_carrier_exit_unverified",
                error);
        }
        if (!verify_unload_process_instance(config, &error)) {
            return transaction.result(
                AgentUnloadStatus::IdentityRejected,
                "unload_identity_changed",
                "stage=after_carrier_exit: " + error);
        }
        std::size_t inert_overlap_count = 0;
        if (!verify_agent_mapping_absent(config, &inert_overlap_count, &error)) {
            return transaction.result(
                AgentUnloadStatus::FinalizeFailed,
                "agent_mapping_remains",
                error);
        }
        if (!unload.remove_now(&error)) {
            return transaction.result(
                AgentUnloadStatus::CleanupFailed,
                "unload_file_cleanup_failed",
                error);
        }
        return transaction.result(
            AgentUnloadStatus::Ok,
            "unloaded",
            transaction.success_message(inert_overlap_count));
    }

    return transaction.result(
        AgentUnloadStatus::FinalizeFailed,
        "agent_not_quiescent",
        last_busy_error);
}

}  // namespace azlw::loader

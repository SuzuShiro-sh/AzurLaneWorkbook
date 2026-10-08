// 实现链接期 ptrace 包装，在远程调用边界内重试 EINTR 并保留寄存器失败阶段。

#include "tracee_ptrace.h"

#include <cerrno>
#include <sys/ptrace.h>

namespace {

constexpr int kMaximumRegisterAttempts = 4;

struct CaptureState final {
    pid_t target_tid = 0;
    bool active = false;
    bool call_setup_failed = false;
    int register_reads = 0;
    int register_writes = 0;
    azlw::loader::RemoteCallPtraceDiagnostic diagnostic;
};

thread_local CaptureState capture_state;

#if defined(AZLW_PTRACE_TESTING)
thread_local azlw::loader::PtraceTestBeforeHook test_before_hook = nullptr;
thread_local azlw::loader::PtraceTestAfterHook test_after_hook = nullptr;
#endif

bool is_register_read(int operation) noexcept {
    return operation == PTRACE_GETREGSET || operation == PTRACE_GETREGS;
}

bool is_register_write(int operation) noexcept {
    return operation == PTRACE_SETREGSET || operation == PTRACE_SETREGS;
}

bool is_memory_setup_operation(int operation) noexcept {
    return operation == PTRACE_PEEKDATA || operation == PTRACE_POKEDATA;
}

bool captures_target(pid_t tid) noexcept {
    return capture_state.active && capture_state.target_tid == tid;
}

bool captures_operation(int operation, pid_t tid) noexcept {
    return captures_target(tid) &&
           (is_register_read(operation) || is_register_write(operation));
}

azlw::loader::RemoteCallRegisterStage register_failure_stage(int operation) noexcept {
    if (is_register_read(operation)) {
        return capture_state.register_reads == 0
                   ? azlw::loader::RemoteCallRegisterStage::InitialRegsRead
                   : azlw::loader::RemoteCallRegisterStage::ReturnRegsRead;
    }
    return capture_state.register_writes == 0 && !capture_state.call_setup_failed
               ? azlw::loader::RemoteCallRegisterStage::CallRegsWrite
               : azlw::loader::RemoteCallRegisterStage::RestoreRegsWrite;
}

void finish_register_operation(int operation) noexcept {
    if (is_register_read(operation)) {
        ++capture_state.register_reads;
    } else {
        ++capture_state.register_writes;
    }
}

void record_register_failure(
    azlw::loader::RemoteCallRegisterStage stage,
    int system_error) noexcept {
    if (!capture_state.diagnostic.register_failure_observed) {
        capture_state.diagnostic.register_failure_observed = true;
        capture_state.diagnostic.stage = stage;
        capture_state.diagnostic.system_error = system_error;
    }
    if (stage == azlw::loader::RemoteCallRegisterStage::RestoreRegsWrite) {
        capture_state.diagnostic.restore_failure_observed = true;
        capture_state.diagnostic.restore_system_error = system_error;
    }
}

void clear_capture() noexcept {
    capture_state = CaptureState{};
}

}  // namespace

namespace azlw::loader {

RemoteCallPtraceCapture::RemoteCallPtraceCapture(pid_t target_tid) noexcept {
    if (target_tid <= 0 || capture_state.active) {
        return;
    }
    capture_state.target_tid = target_tid;
    capture_state.active = true;
    active_ = true;
}

RemoteCallPtraceCapture::~RemoteCallPtraceCapture() {
    if (active_) {
        clear_capture();
    }
}

bool RemoteCallPtraceCapture::active() const noexcept {
    return active_;
}

RemoteCallPtraceDiagnostic RemoteCallPtraceCapture::finish() noexcept {
    if (!active_) {
        return {};
    }
    const RemoteCallPtraceDiagnostic result = capture_state.diagnostic;
    clear_capture();
    active_ = false;
    return result;
}

const char* remote_call_register_stage_name(RemoteCallRegisterStage stage) noexcept {
    switch (stage) {
        case RemoteCallRegisterStage::None:
            return "none";
        case RemoteCallRegisterStage::InitialRegsRead:
            return "initial_regs_read";
        case RemoteCallRegisterStage::CallRegsWrite:
            return "call_regs_write";
        case RemoteCallRegisterStage::ReturnRegsRead:
            return "return_regs_read";
        case RemoteCallRegisterStage::RestoreRegsWrite:
            return "restore_regs_write";
    }
    return "unknown";
}

#if defined(AZLW_PTRACE_TESTING)
bool install_ptrace_test_hooks(
    PtraceTestBeforeHook before_hook,
    PtraceTestAfterHook after_hook) noexcept {
    if (test_before_hook != nullptr || test_after_hook != nullptr) {
        return false;
    }
    test_before_hook = before_hook;
    test_after_hook = after_hook;
    return true;
}

void clear_ptrace_test_hooks() noexcept {
    test_before_hook = nullptr;
    test_after_hook = nullptr;
}
#endif

}  // namespace azlw::loader

extern "C" long __real_ptrace(int operation, ...);

/// 仅在显式观察的远程调用中重试寄存器 EINTR；其他 ptrace 保持逐次完整透传。
extern "C" long __wrap_ptrace(int operation, pid_t tid, void* address, void* data) {
    const bool observed_operation = captures_operation(operation, tid);
    const bool register_operation = is_register_read(operation) || is_register_write(operation);
    const azlw::loader::RemoteCallRegisterStage stage =
        observed_operation ? register_failure_stage(operation)
                           : azlw::loader::RemoteCallRegisterStage::None;
    const int maximum_attempts = observed_operation && register_operation
                                     ? kMaximumRegisterAttempts
                                     : 1;

    long result = -1;
    int operation_error = 0;
    for (int attempt = 0; attempt < maximum_attempts; ++attempt) {
#if defined(AZLW_PTRACE_TESTING)
        bool handled = false;
        if (test_before_hook != nullptr) {
            handled = test_before_hook(
                operation, tid, address, data, &result, &operation_error);
        }
        if (handled) {
            errno = operation_error;
        } else {
#endif
            errno = 0;
            result = __real_ptrace(operation, tid, address, data);
            operation_error = result == -1 ? errno : 0;
#if defined(AZLW_PTRACE_TESTING)
        }
        if (test_after_hook != nullptr) {
            test_after_hook(operation, tid, result);
        }
#endif

        if (result != -1 || operation_error != EINTR || attempt + 1 == maximum_attempts) {
            break;
        }
        ++capture_state.diagnostic.eintr_retries;
    }

    if (observed_operation && register_operation) {
        if (result == -1) {
            record_register_failure(stage, operation_error);
        }
        finish_register_operation(operation);
    } else if (captures_target(tid) && capture_state.register_writes == 0 &&
               is_memory_setup_operation(operation) && result == -1 && operation_error != 0) {
        capture_state.call_setup_failed = true;
    }
    if (result == -1) {
        errno = operation_error;
    }
    return result;
}

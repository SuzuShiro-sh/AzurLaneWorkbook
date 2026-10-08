// 声明远程调用期间的 ptrace 寄存器重试、失败阶段诊断和测试观察入口。

#pragma once

#include <sys/types.h>

namespace azlw::loader {

/// 标识远程调用中一次寄存器操作的明确职责。
enum class RemoteCallRegisterStage {
    None,
    InitialRegsRead,
    CallRegsWrite,
    ReturnRegsRead,
    RestoreRegsWrite,
};

/// 保存首个寄存器失败、独立的恢复失败和被安全重试的 EINTR 次数。
struct RemoteCallPtraceDiagnostic final {
    bool register_failure_observed = false;
    RemoteCallRegisterStage stage = RemoteCallRegisterStage::None;
    int system_error = 0;
    bool restore_failure_observed = false;
    int restore_system_error = 0;
    int eintr_retries = 0;
};

/// 仅观察指定 TID 的一次远程调用；同一线程不允许嵌套观察。
class RemoteCallPtraceCapture final {
public:
    explicit RemoteCallPtraceCapture(pid_t target_tid) noexcept;
    ~RemoteCallPtraceCapture();

    RemoteCallPtraceCapture(const RemoteCallPtraceCapture&) = delete;
    RemoteCallPtraceCapture& operator=(const RemoteCallPtraceCapture&) = delete;

    [[nodiscard]] bool active() const noexcept;
    RemoteCallPtraceDiagnostic finish() noexcept;

private:
    bool active_ = false;
};

const char* remote_call_register_stage_name(RemoteCallRegisterStage stage) noexcept;

#if defined(AZLW_PTRACE_TESTING)
using PtraceTestBeforeHook = bool (*)(
    int operation,
    pid_t tid,
    void* address,
    void* data,
    long* result,
    int* error_number);
using PtraceTestAfterHook = void (*)(int operation, pid_t tid, long result);

/// 设备 fixture 独占安装故障注入和成功调用观察器。
bool install_ptrace_test_hooks(
    PtraceTestBeforeHook before_hook,
    PtraceTestAfterHook after_hook) noexcept;
void clear_ptrace_test_hooks() noexcept;
#endif

}  // namespace azlw::loader

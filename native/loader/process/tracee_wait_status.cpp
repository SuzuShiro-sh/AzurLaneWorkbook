// 实现链接期 waitpid 包装，只记录显式启用的指定 ptrace 载体终态。

#include "tracee_wait_status.h"

#include <sys/wait.h>

namespace {

struct WaitCaptureState final {
    pid_t target_tid = 0;
    int wait_status = 0;
    bool active = false;
    bool observed = false;
};

thread_local WaitCaptureState capture_state;

void clear_capture() noexcept {
    capture_state = WaitCaptureState{};
}

}  // namespace

namespace azlw::loader {

TraceeWaitStatusCapture::TraceeWaitStatusCapture(pid_t target_tid) noexcept {
    if (target_tid <= 0 || capture_state.active) {
        return;
    }
    capture_state.target_tid = target_tid;
    capture_state.active = true;
    active_ = true;
}

TraceeWaitStatusCapture::~TraceeWaitStatusCapture() {
    if (active_) {
        clear_capture();
    }
}

bool TraceeWaitStatusCapture::active() const noexcept {
    return active_;
}

TraceeWaitStatus TraceeWaitStatusCapture::finish() noexcept {
    if (!active_) {
        return {};
    }
    const TraceeWaitStatus result{
        .observed = capture_state.observed,
        .value = capture_state.wait_status,
    };
    clear_capture();
    active_ = false;
    return result;
}

}  // namespace azlw::loader

extern "C" pid_t __real_waitpid(pid_t pid, int* status, int options);

/// 完整透传 libc waitpid，仅在调用成功且命中指定 TID 的终态时保存原始状态。
extern "C" pid_t __wrap_waitpid(pid_t pid, int* status, int options) {
    const pid_t result = __real_waitpid(pid, status, options);
    if (capture_state.active && status != nullptr && result == capture_state.target_tid &&
        (WIFEXITED(*status) || WIFSIGNALED(*status))) {
        capture_state.wait_status = *status;
        capture_state.observed = true;
    }
    return result;
}

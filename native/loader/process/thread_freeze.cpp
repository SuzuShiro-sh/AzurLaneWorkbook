// 实现注入临界区内的目标线程附加、稳定性确认和有序分离。

#include "thread_freeze.h"

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <csignal>
#include <cpuid.h>
#include <cstring>
#include <elf.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <thread>
#include <utility>
#include <unistd.h>

#include <KittyMemoryEx.hpp>

namespace azlw::loader {
namespace {

/// 覆盖当前 x86_64 XSAVE 组件并保留异常长度拒绝边界。
constexpr std::size_t kMaximumXstateBytes = 64 * 1024;
constexpr std::size_t kMinimumXstateBytes = 512;

}  // namespace

RetainedThreadExitKind classify_retained_thread_exit(int wait_status) noexcept {
    if (WIFEXITED(wait_status)) {
        return WEXITSTATUS(wait_status) == 0 ? RetainedThreadExitKind::Clean
                                            : RetainedThreadExitKind::NonZero;
    }
    if (WIFSIGNALED(wait_status)) {
        return RetainedThreadExitKind::Signaled;
    }
    return RetainedThreadExitKind::Unexpected;
}

// 即使调用方提前返回，也必须在原事务截止时间内释放所有权或终止旧目标。
ThreadFreeze::~ThreadFreeze() {
    std::string ignored;
    if (!detach_all(&ignored) && !attached_.empty()) {
        terminate_target();
    }
}

// SEIZE 只登记跟踪关系，线程停止由后续批量中断负责。
ThreadFreeze::SeizeResult ThreadFreeze::seize_one(
    pid_t tid,
    const std::chrono::steady_clock::time_point& deadline,
    std::string* error) {
    if (std::chrono::steady_clock::now() >= deadline) {
        *error = "线程冻结总时限已耗尽";
        return SeizeResult::Failed;
    }
    constexpr unsigned long kTraceOptions = PTRACE_O_EXITKILL | PTRACE_O_TRACESYSGOOD;
    if (ptrace(PTRACE_SEIZE, tid, nullptr, kTraceOptions) != 0) {
        if (errno == ESRCH && worker_exited(tid)) {
            return SeizeResult::Vanished;
        }
        *error = "PTRACE_SEIZE tid=" + std::to_string(tid) + " 失败: " + std::strerror(errno);
        return SeizeResult::Failed;
    }
    attached_.push_back(tid);
    return SeizeResult::Seized;
}

// 中断请求先快速发给整批线程，避免逐线程等待形成可观察的半冻结窗口。
bool ThreadFreeze::interrupt_and_wait(
    const std::vector<pid_t>& tids,
    const std::chrono::steady_clock::time_point& deadline,
    std::string* error) {
    std::vector<pid_t> interrupt_order = tids;
    const auto main = std::find(interrupt_order.begin(), interrupt_order.end(), pid_);
    if (main != interrupt_order.end()) {
        std::iter_swap(main, interrupt_order.end() - 1);
    }

    std::vector<pid_t> pending;
    pending.reserve(interrupt_order.size());
    for (const pid_t tid : interrupt_order) {
        if (std::find(attached_.begin(), attached_.end(), tid) == attached_.end()) {
            continue;
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            *error = "发送线程中断前冻结总时限已耗尽";
            return false;
        }

        errno = 0;
        if (ptrace(PTRACE_INTERRUPT, tid, nullptr, nullptr) != 0) {
            const int interrupt_error = errno;
            if (interrupt_error != ESRCH) {
                *error = "PTRACE_INTERRUPT tid=" + std::to_string(tid) + " 失败: " +
                         std::strerror(interrupt_error);
                return false;
            }
        }
        pending.push_back(tid);
    }

    const auto pending_main = std::find(pending.begin(), pending.end(), pid_);
    if (pending_main != pending.end()) {
        std::iter_swap(pending.begin(), pending_main);
    }
    for (const pid_t tid : pending) {
        if (std::find(attached_.begin(), attached_.end(), tid) == attached_.end()) {
            continue;
        }
        const StopResult stop = wait_for_stop(tid, deadline, tid != pid_, error);
        if (stop == StopResult::Exited) {
            forget_thread(tid);
            continue;
        }
        if (stop != StopResult::Stopped) {
            return false;
        }
    }
    return true;
}

// waitpid 已消费停止事件后，后续寄存器和分离操作才具有确定语义。
ThreadFreeze::StopResult ThreadFreeze::wait_for_stop(
    pid_t tid,
    const std::chrono::steady_clock::time_point& deadline,
    bool allow_worker_exit,
    std::string* error) const {
    while (std::chrono::steady_clock::now() < deadline) {
        int status = 0;
        errno = 0;
        const pid_t result = waitpid(tid, &status, __WALL | WNOHANG);
        if (result == tid) {
            if (WIFSTOPPED(status)) {
                return StopResult::Stopped;
            }
            if (allow_worker_exit && (WIFEXITED(status) || WIFSIGNALED(status)) &&
                !thread_vanished(pid_)) {
                return StopResult::Exited;
            }
            if (WIFSIGNALED(status)) {
                *error = "tid=" + std::to_string(tid) + " 在等待停止时被信号 " +
                         std::to_string(WTERMSIG(status)) + " 终止";
            } else {
                *error = "tid=" + std::to_string(tid) + " 未进入 ptrace-stop";
            }
            return StopResult::Failed;
        }
        if (result < 0 && errno != EINTR) {
            const int wait_error = errno;
            if (wait_error == ECHILD && allow_worker_exit && worker_exited(tid)) {
                return StopResult::Exited;
            }
            *error = "waitpid tid=" + std::to_string(tid) + " 失败: " +
                     std::strerror(wait_error);
            return StopResult::Failed;
        }
        std::this_thread::sleep_until(std::min(
            deadline,
            std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
    }
    KittyMemoryEx::ProcStatus status{};
    if (KittyMemoryEx::ProcStatus::parse(pid_, tid, &status) &&
        status.contains("TracerPid") && status.contains("State")) {
        *error = "等待 tid=" + std::to_string(tid) +
                 " 进入 ptrace-stop 超时，TracerPid=" +
                 std::to_string(status.getInt("TracerPid")) +
                 "，线程状态=" + status.getString("State");
    } else {
        *error = "等待 tid=" + std::to_string(tid) +
                 " 进入 ptrace-stop 超时，线程状态不可读";
    }
    return StopResult::Failed;
}

// 通过 `/proc/<pid>/task/<tid>` 判断线程是否已被内核回收。
bool ThreadFreeze::thread_vanished(pid_t tid) const {
    const std::string task_path =
        "/proc/" + std::to_string(pid_) + "/task/" + std::to_string(tid);
    errno = 0;
    return access(task_path.c_str(), F_OK) != 0 && errno == ENOENT;
}

// 主线程消失代表目标进程终止，只有工作线程消失可继续收敛。
bool ThreadFreeze::worker_vanished(pid_t tid) const {
    return tid != pid_ && thread_vanished(tid) && !thread_vanished(pid_);
}

// procfs 可能晚于 ptrace 报告线程退出，因此同时接受明确的内核终态。
bool ThreadFreeze::worker_exited(pid_t tid) const {
    if (tid == pid_ || thread_vanished(pid_)) {
        return false;
    }
    if (thread_vanished(tid)) {
        return true;
    }
    KittyMemoryEx::ProcStatus status{};
    if (!KittyMemoryEx::ProcStatus::parse(pid_, tid, &status)) {
        return false;
    }
    const std::string state = status.getString("State");
    return !state.empty() && (state.front() == 'X' || state.front() == 'Z');
}

// 终态必须由 waitpid 原始证据确认；仅凭 procfs 消失或 Z 状态不能推断退出原因。
bool ThreadFreeze::consume_worker_exit(
    pid_t tid,
    const std::chrono::steady_clock::time_point& deadline,
    int* wait_status,
    std::string* error) const {
    while (std::chrono::steady_clock::now() < deadline) {
        int status = 0;
        errno = 0;
        const pid_t result = waitpid(tid, &status, __WALL | WNOHANG);
        if (result == tid) {
            if ((WIFEXITED(status) || WIFSIGNALED(status)) && !thread_vanished(pid_)) {
                // exit_group 后主线程的 procfs 记录可能尚在，必须证明仍能操作其冻结现场。
                user_regs_struct main_registers{};
                std::string main_error;
                if (!read_registers(pid_, &main_registers, &main_error)) {
                    *error = "卸载载体已退出，raw_wait_status=" + std::to_string(status) +
                             "，主线程冻结现场不可用: " + main_error;
                    return false;
                }
                *wait_status = status;
                return true;
            }
            *error = "卸载载体终态无效，raw_wait_status=" + std::to_string(status) +
                     "，main_vanished=" + std::to_string(thread_vanished(pid_));
            return false;
        }
        if (result < 0 && errno != EINTR) {
            *error = "消费卸载载体终态失败: " + std::string(std::strerror(errno));
            return false;
        }
        std::this_thread::sleep_until(std::min(
            deadline,
            std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
    }
    *error = "等待卸载载体终态证据超时";
    return false;
}

// 已退出线程不再需要执行 PTRACE_DETACH。
void ThreadFreeze::forget_thread(pid_t tid) {
    attached_.erase(std::remove(attached_.begin(), attached_.end(), tid), attached_.end());
}

// x86_64 使用 GETREGSET 读取完整通用寄存器，和现有远程调用 ABI 保持一致。
bool ThreadFreeze::read_registers(
    pid_t tid,
    user_regs_struct* registers,
    std::string* error) const {
    iovec registers_view = {
        .iov_base = registers,
        .iov_len = sizeof(*registers),
    };
    if (ptrace(
            PTRACE_GETREGSET,
            tid,
            reinterpret_cast<void*>(NT_PRSTATUS),
            &registers_view) != 0) {
        *error = "PTRACE_GETREGSET tid=" + std::to_string(tid) + " 失败: " +
                 std::strerror(errno);
        return false;
    }
    if (registers_view.iov_len != sizeof(*registers)) {
        *error = "PTRACE_GETREGSET tid=" + std::to_string(tid) + " 返回长度不一致";
        return false;
    }
    return true;
}

// XSAVE 已由系统启用时保存完整扩展状态；FXSAVE 系统保存固定的 FPU、SSE 与 MXCSR。
bool ThreadFreeze::read_xstate(
    pid_t tid,
    unsigned int regset,
    std::vector<std::uint8_t>* xstate,
    std::string* error) const {
    const std::size_t capacity = regset == NT_X86_XSTATE ? kMaximumXstateBytes : kMinimumXstateBytes;
    std::vector<std::uint8_t> snapshot(capacity);
    iovec xstate_view = {
        .iov_base = snapshot.data(),
        .iov_len = snapshot.size(),
    };
    if (ptrace(
            PTRACE_GETREGSET,
            tid,
            reinterpret_cast<void*>(static_cast<std::uintptr_t>(regset)),
            &xstate_view) != 0) {
        *error = "PTRACE_GETREGSET processor_state=" + std::to_string(regset) + " tid=" + std::to_string(tid) +
                 " 失败: " + std::strerror(errno);
        return false;
    }
    if (xstate_view.iov_len < kMinimumXstateBytes ||
        xstate_view.iov_len > snapshot.size()) {
        *error = "PTRACE_GETREGSET processor_state=" + std::to_string(regset) + " tid=" + std::to_string(tid) +
                 " 返回长度无效";
        return false;
    }
    snapshot.resize(xstate_view.iov_len);
    *xstate = std::move(snapshot);
    return true;
}

// 工作线程全部停止后才跟踪主线程，避免运行中的线程观察到半完成的冻结事务。
bool ThreadFreeze::attach_all(pid_t pid, std::uint32_t timeout_ms, std::string* error) {
    if (pid <= 0 || timeout_ms == 0) {
        *error = "目标进程 PID 或线程冻结时限无效";
        return false;
    }
    if (pid_ != 0 || !attached_.empty()) {
        *error = "线程冻结对象仍持有上一轮 ptrace 线程";
        return false;
    }

    pid_ = pid;
    const auto deadline =
        std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    operation_deadline_ = deadline;
    bool worker_tracking_stable = false;
    for (int pass = 0; pass < 8; ++pass) {
        if (std::chrono::steady_clock::now() >= deadline) {
            *error = "线程冻结总时限已耗尽";
            return false;
        }
        auto threads = KittyMemoryEx::getAllThreads(pid);
        if (threads.empty() ||
            std::find(threads.begin(), threads.end(), pid) == threads.end()) {
            *error = "目标进程主线程不存在或没有可附加线程";
            return false;
        }
        std::sort(threads.begin(), threads.end());

        std::vector<pid_t> new_workers;
        for (const pid_t tid : threads) {
            if (tid == pid) {
                continue;
            }
            if (std::find(attached_.begin(), attached_.end(), tid) != attached_.end()) {
                continue;
            }
            const SeizeResult result = seize_one(tid, deadline, error);
            if (result == SeizeResult::Failed) {
                return false;
            }
            if (result == SeizeResult::Seized) {
                new_workers.push_back(tid);
            }
        }

        if (!interrupt_and_wait(new_workers, deadline, error)) {
            return false;
        }

        const auto latest = KittyMemoryEx::getAllThreads(pid);
        const bool all_workers_seized =
            std::find(latest.begin(), latest.end(), pid) != latest.end() &&
            std::all_of(latest.begin(), latest.end(), [this, pid](pid_t tid) {
                return tid == pid ||
                       std::find(attached_.begin(), attached_.end(), tid) != attached_.end() ||
                       (tid != pid_ && worker_exited(tid));
            });
        if (!all_workers_seized || !new_workers.empty()) {
            if (all_workers_seized) {
                std::this_thread::sleep_until(std::min(
                    deadline,
                    std::chrono::steady_clock::now() + std::chrono::milliseconds(10)));
            }
            continue;
        }
        worker_tracking_stable = true;
        break;
    }
    if (!worker_tracking_stable) {
        *error = "目标工作线程集合在 8 轮跟踪后仍不稳定";
        return false;
    }

    const SeizeResult main_result = seize_one(pid, deadline, error);
    if (main_result != SeizeResult::Seized) {
        if (main_result == SeizeResult::Vanished) {
            *error = "目标进程主线程在建立跟踪期间消失";
        }
        return false;
    }

    if (!interrupt_and_wait({pid}, deadline, error)) {
        return false;
    }

    bool frozen_stable = false;
    for (int pass = 0; pass < 8; ++pass) {
        if (std::chrono::steady_clock::now() >= deadline) {
            *error = "批量中断后线程收敛超过冻结总时限";
            return false;
        }
        const auto latest = KittyMemoryEx::getAllThreads(pid);
        if (latest.empty() || std::find(latest.begin(), latest.end(), pid) == latest.end() ||
            std::find(attached_.begin(), attached_.end(), pid) == attached_.end()) {
            *error = "批量中断后目标进程主线程已经消失";
            return false;
        }

        std::vector<pid_t> new_threads;
        for (const pid_t tid : latest) {
            if (std::find(attached_.begin(), attached_.end(), tid) != attached_.end() ||
                (tid != pid && worker_exited(tid))) {
                continue;
            }
            const SeizeResult result = seize_one(tid, deadline, error);
            if (result == SeizeResult::Failed) {
                return false;
            }
            if (result == SeizeResult::Seized) {
                new_threads.push_back(tid);
            }
        }
        if (new_threads.empty()) {
            frozen_stable = true;
            break;
        }
        if (!interrupt_and_wait(new_threads, deadline, error)) {
            return false;
        }
    }
    if (!frozen_stable) {
        *error = "批量中断后目标线程集合在 8 轮内仍未收敛";
        return false;
    }
    return capture_main_context(deadline, error);
}

// 两类现场都成功读取后才提交快照，避免半初始化状态进入远程调用。
bool ThreadFreeze::capture_main_context(
    const std::chrono::steady_clock::time_point& deadline,
    std::string* error) {
    if (std::chrono::steady_clock::now() >= deadline) {
        *error = "捕获主线程现场前冻结总时限已耗尽";
        return false;
    }
    // OSXSAVE 表示内核实际启用了 XSAVE；硬件支持但内核未启用时仍采用 FXSAVE。
    // 不按 ptrace 失败码降级，避免丢失已启用的 AVX 或其他扩展状态。
    unsigned int eax = 0, ebx = 0, ecx = 0, edx = 0;
    if (!__get_cpuid(1, &eax, &ebx, &ecx, &edx)) {
        *error = "读取处理器状态保存能力失败";
        return false;
    }
    const unsigned int regset = (ecx & bit_OSXSAVE) != 0 ? NT_X86_XSTATE : NT_PRFPREG;
    user_regs_struct registers{};
    std::vector<std::uint8_t> xstate;
    if (!read_registers(pid_, &registers, error) || !read_xstate(pid_, regset, &xstate, error)) {
        return false;
    }
    if (std::chrono::steady_clock::now() >= deadline) {
        *error = "捕获主线程现场超过冻结总时限";
        return false;
    }
    main_registers_ = registers;
    main_state_regset_ = regset;
    main_xstate_ = std::move(xstate);
    main_context_captured_ = true;
    return true;
}

// 停驻载体保留 Agent 调用栈；其余线程只以冻结现场的真实指令指针判定执行位置。
bool ThreadFreeze::verify_instruction_pointers_except(
    const std::vector<ProtectedAddressRange>& ranges,
    pid_t excluded_tid,
    std::string* error) const {
    if (pid_ <= 0 || attached_.empty() || ranges.empty() || excluded_tid <= 0 ||
        excluded_tid == pid_ ||
        std::find(attached_.begin(), attached_.end(), excluded_tid) == attached_.end()) {
        *error = "卸载线程静止检查参数无效";
        return false;
    }
    for (const ProtectedAddressRange& range : ranges) {
        if (range.start == 0 || range.start >= range.end || range.label.empty()) {
            *error = "卸载保护地址区间无效";
            return false;
        }
    }
    const auto contains = [&ranges](std::uintptr_t address, std::string* label) {
        for (const ProtectedAddressRange& range : ranges) {
            if (address >= range.start && address < range.end) {
                *label = range.label;
                return true;
            }
        }
        return false;
    };

    for (const pid_t tid : attached_) {
        if (tid == excluded_tid) {
            continue;
        }
        user_regs_struct registers{};
        if (!read_registers(tid, &registers, error)) {
            return false;
        }
        std::string matched_label;
        if (contains(static_cast<std::uintptr_t>(registers.rip), &matched_label)) {
            *error = "tid=" + std::to_string(tid) + " 的 RIP 仍位于 " + matched_label;
            return false;
        }
    }
    return true;
}

// 主线程必须仍在 ptrace stop 中；任一恢复失败时保留所有权供析构路径重试。
bool ThreadFreeze::restore_main_context(std::string* error) {
    if (!main_context_captured_) {
        return true;
    }
    iovec xstate = {
        .iov_base = main_xstate_.data(),
        .iov_len = main_xstate_.size(),
    };
    if (ptrace(PTRACE_SETREGSET, pid_, reinterpret_cast<void*>(static_cast<std::uintptr_t>(main_state_regset_)), &xstate) != 0) {
        *error = "恢复主线程原始处理器状态失败: " + std::string(std::strerror(errno));
        return false;
    }
    iovec registers = {
        .iov_base = &main_registers_,
        .iov_len = sizeof(main_registers_),
    };
    if (ptrace(PTRACE_SETREGSET, pid_, reinterpret_cast<void*>(NT_PRSTATUS), &registers) != 0) {
        *error = "恢复主线程原始寄存器失败: " + std::string(std::strerror(errno));
        return false;
    }
    // 分离失败后仍需重试恢复，快照保留到实际分离后再释放。
    return true;
}

// ESRCH 也可能表示线程尚未进入可操作的 ptrace-stop，必须结合 wait 与 procfs 证据判定。
ThreadFreeze::DetachResult ThreadFreeze::detach_one(
    pid_t tid,
    const std::chrono::steady_clock::time_point& deadline,
    bool allow_worker_exit,
    std::string* error) const {
    long long last_tracer_pid = -1;
    std::string last_state = "unknown";
    int last_wait_status = 0;
    bool wait_status_observed = false;
    int last_wait_error = 0;
    while (std::chrono::steady_clock::now() < deadline) {
        errno = 0;
        if (ptrace(PTRACE_DETACH, tid, nullptr, nullptr) == 0) {
            return DetachResult::Detached;
        }
        const int detach_error = errno;
        if (detach_error != ESRCH) {
            *error = "PTRACE_DETACH tid=" + std::to_string(tid) + " 失败: " +
                     std::strerror(detach_error);
            return DetachResult::Failed;
        }

        int pending_status = 0;
        errno = 0;
        const pid_t waited = waitpid(tid, &pending_status, __WALL | WNOHANG);
        if (waited == tid) {
            last_wait_status = pending_status;
            wait_status_observed = true;
            last_wait_error = 0;
            if (WIFSTOPPED(pending_status)) {
                continue;
            }
            if (WIFEXITED(pending_status) || WIFSIGNALED(pending_status)) {
                const bool main_vanished = thread_vanished(pid_);
                if (allow_worker_exit && !main_vanished) {
                    return DetachResult::Exited;
                }
                *error = "tid=" + std::to_string(tid) +
                         " 在分离时返回终态，raw_wait_status=" +
                         std::to_string(pending_status) +
                         "，exited=" + std::to_string(WIFEXITED(pending_status)) +
                         "，signaled=" + std::to_string(WIFSIGNALED(pending_status)) +
                         "，main_vanished=" + std::to_string(main_vanished);
                return DetachResult::Failed;
            }
        }
        if (waited < 0) {
            const int wait_error = errno;
            if (wait_error == EINTR) {
                continue;
            }
            last_wait_error = wait_error;
            if (wait_error == ECHILD && allow_worker_exit && worker_exited(tid)) {
                return DetachResult::Exited;
            }
            if (wait_error != ECHILD || !allow_worker_exit) {
                *error = "检查 tid=" + std::to_string(tid) + " 的 ptrace 事件失败: " +
                         std::strerror(wait_error);
                return DetachResult::Failed;
            }
        }

        KittyMemoryEx::ProcStatus status{};
        if (!KittyMemoryEx::ProcStatus::parse(pid_, tid, &status) ||
            !status.contains("TracerPid") || !status.contains("State")) {
            if (allow_worker_exit && worker_vanished(tid)) {
                return DetachResult::Exited;
            }
            if (thread_vanished(pid_)) {
                *error = "PTRACE_DETACH tid=" + std::to_string(tid) +
                         " 返回 ESRCH，且目标主进程已经消失";
                return DetachResult::Failed;
            }
            std::this_thread::sleep_until(std::min(
                deadline,
                std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
            continue;
        }
        last_tracer_pid = status.getInt("TracerPid");
        last_state = status.getString("State");
        const bool terminal_worker =
            allow_worker_exit && !last_state.empty() &&
            (last_state.front() == 'X' || last_state.front() == 'Z');
        if (terminal_worker) {
            if (thread_vanished(pid_)) {
                *error = "tid=" + std::to_string(tid) + " 已进入终态 " + last_state +
                         "，但目标主进程同时消失";
                return DetachResult::Failed;
            }
            std::this_thread::sleep_until(std::min(
                deadline,
                std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
            continue;
        }
        if (last_tracer_pid == 0) {
            return DetachResult::Detached;
        }
        if (last_tracer_pid != getpid()) {
            *error = "PTRACE_DETACH tid=" + std::to_string(tid) +
                     " 返回 ESRCH，TracerPid=" + std::to_string(last_tracer_pid) +
                     "，线程状态=" + last_state;
            return DetachResult::Failed;
        }

        const bool status_stopped =
            !last_state.empty() && (last_state.front() == 't' || last_state.front() == 'T');
        if (!status_stopped) {
            errno = 0;
            if (ptrace(PTRACE_INTERRUPT, tid, nullptr, nullptr) != 0) {
                const int interrupt_error = errno;
                if (interrupt_error == ESRCH && allow_worker_exit && worker_vanished(tid)) {
                    return DetachResult::Exited;
                }
                if (interrupt_error != ESRCH) {
                    *error = "重新中断 tid=" + std::to_string(tid) + " 失败: " +
                             std::strerror(interrupt_error);
                    return DetachResult::Failed;
                }
            } else {
                const StopResult stop = wait_for_stop(tid, deadline, allow_worker_exit, error);
                if (stop == StopResult::Exited) {
                    return DetachResult::Exited;
                }
                if (stop != StopResult::Stopped) {
                    return DetachResult::Failed;
                }
                continue;
            }
        }
        std::this_thread::sleep_until(std::min(
            deadline,
            std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
    }
    *error = "分离 tid=" + std::to_string(tid) + " 超时，TracerPid=" +
             std::to_string(last_tracer_pid) + "，线程状态=" + last_state +
             "，raw_wait_status=" +
             (wait_status_observed ? std::to_string(last_wait_status) : "none") +
             "，wait_error=" +
             (last_wait_error == 0 ? "none" : std::string(std::strerror(last_wait_error)));
    return DetachResult::Failed;
}

// 先恢复并分离主线程，再释放普通工作线程，只保留卸载载体。
DetachExceptResult ThreadFreeze::detach_all_except(
    pid_t retained_tid,
    std::uint32_t timeout_ms,
    std::string* error) {
    if (pid_ <= 0 || !main_context_captured_ ||
        retained_tid <= 0 || retained_tid == pid_ || timeout_ms == 0 ||
        std::find(attached_.begin(), attached_.end(), pid_) == attached_.end() ||
        std::find(attached_.begin(), attached_.end(), retained_tid) == attached_.end() ||
        thread_vanished(retained_tid)) {
        *error = "主线程或卸载载体未保持完整冻结状态";
        return {};
    }

    const auto deadline = bounded_deadline(timeout_ms);
    if (std::chrono::steady_clock::now() >= deadline) {
        *error = "分离普通线程前卸载总时限已耗尽";
        return {};
    }
    if (!restore_main_context(error)) {
        return {};
    }

    KittyMemoryEx::ProcStatus retained_status{};
    const bool retained_status_read =
        KittyMemoryEx::ProcStatus::parse(pid_, retained_tid, &retained_status) &&
        retained_status.contains("TracerPid") && retained_status.contains("State");
    if (!retained_status_read) {
        *error = "分离线程前无法确认卸载载体状态";
        return {};
    }
    const long long retained_tracer_pid = retained_status.getInt("TracerPid");
    const std::string retained_state = retained_status.getString("State");
    const bool retained_stopped =
        !retained_state.empty() &&
        (retained_state.front() == 't' || retained_state.front() == 'T');
    const bool retained_terminal =
        !retained_state.empty() &&
        (retained_state.front() == 'X' || retained_state.front() == 'Z');
    if (retained_tracer_pid == getpid() && retained_terminal) {
        int wait_status = 0;
        if (!consume_worker_exit(retained_tid, deadline, &wait_status, error)) {
            return {};
        }
        const RetainedThreadExitKind kind = classify_retained_thread_exit(wait_status);
        *error = "卸载载体在 dlclose 前进入终态，raw_wait_status=" +
                 std::to_string(wait_status) + "，exit_kind=" +
                 (kind == RetainedThreadExitKind::Clean
                      ? "clean"
                      : (kind == RetainedThreadExitKind::NonZero
                             ? "nonzero"
                             : (kind == RetainedThreadExitKind::Signaled ? "signaled"
                                                                         : "unexpected")));
        forget_thread(retained_tid);
        return DetachExceptResult{
            .status = DetachExceptStatus::CarrierExitedBeforeDlclose,
            .wait_status = wait_status,
        };
    }
    if (retained_tracer_pid != getpid() || !retained_stopped) {
        *error = "分离线程前卸载载体所有权无效，TracerPid=" +
                 std::to_string(retained_tracer_pid) + "，线程状态=" + retained_state;
        return {};
    }

    std::string main_detach_error;
    if (detach_one(pid_, deadline, false, &main_detach_error) != DetachResult::Detached) {
        *error = "分离主线程失败: " + main_detach_error;
        return {};
    }
    main_xstate_.clear();
    main_context_captured_ = false;
    forget_thread(pid_);

    bool success = true;
    std::vector<pid_t> remaining;
    remaining.reserve(attached_.size());
    for (const pid_t tid : attached_) {
        if (tid == pid_ || tid == retained_tid) {
            continue;
        }
        std::string detach_error;
        const DetachResult result = detach_one(tid, deadline, true, &detach_error);
        if (result == DetachResult::Failed) {
            if (success) {
                *error = "分离普通线程失败: " + detach_error;
            }
            success = false;
            remaining.push_back(tid);
        }
    }
    if (!success) {
        remaining.push_back(retained_tid);
        attached_ = std::move(remaining);
        return {};
    }

    user_regs_struct retained_registers{};
    if (!read_registers(retained_tid, &retained_registers, error)) {
        attached_ = {retained_tid};
        return {};
    }
    remaining.push_back(retained_tid);
    attached_ = std::move(remaining);
    return DetachExceptResult{
        .status = DetachExceptStatus::ReadyForCarrierDlclose,
        .wait_status = 0,
    };
}

// 载体已经停在 Agent 之外，直接执行单条 exit 系统调用，绝不恢复其旧 Agent 现场。
bool ThreadFreeze::exit_retained_thread(
    pid_t retained_tid,
    std::uintptr_t syscall_gadget,
    std::uint32_t timeout_ms,
    std::string* error) {
    if (pid_ <= 0 || retained_tid <= 0 || retained_tid == pid_ || syscall_gadget == 0 ||
        timeout_ms == 0 || attached_.size() != 1 || attached_.front() != retained_tid ||
        main_context_captured_) {
        *error = "卸载载体退出参数或 ptrace 所有权无效";
        return false;
    }
    const auto deadline = bounded_deadline(timeout_ms);
    if (std::chrono::steady_clock::now() >= deadline) {
        *error = "启动卸载载体退出前卸载总时限已耗尽";
        return false;
    }

    user_regs_struct registers{};
    if (!read_registers(retained_tid, &registers, error)) {
        return false;
    }
    registers.rip = syscall_gadget;
    registers.rax = SYS_exit;
    registers.rdi = 0;
    registers.orig_rax = 0;
    iovec registers_view = {
        .iov_base = &registers,
        .iov_len = sizeof(registers),
    };
    if (ptrace(
            PTRACE_SETREGSET,
            retained_tid,
            reinterpret_cast<void*>(NT_PRSTATUS),
            &registers_view) != 0) {
        *error = "设置卸载载体退出寄存器失败: " + std::string(std::strerror(errno));
        return false;
    }
    if (ptrace(PTRACE_SINGLESTEP, retained_tid, nullptr, nullptr) != 0) {
        *error = "启动卸载载体 exit 系统调用失败: " + std::string(std::strerror(errno));
        return false;
    }

    const auto finish = [this, retained_tid](bool reset_target) {
        forget_thread(retained_tid);
        if (reset_target && attached_.empty()) {
            reset_state();
        }
    };
    while (std::chrono::steady_clock::now() < deadline) {
        int status = 0;
        errno = 0;
        const pid_t result = waitpid(retained_tid, &status, __WALL | WNOHANG);
        if (result == retained_tid) {
            if (WIFEXITED(status)) {
                const int exit_code = WEXITSTATUS(status);
                const bool main_alive = !thread_vanished(pid_);
                if (exit_code == 0 && main_alive) {
                    finish(true);
                    return true;
                }
                finish(false);
                *error = exit_code == 0 ? "卸载载体退出时目标主进程同时消失"
                                        : "卸载载体以非零状态退出";
                return false;
            }
            if (WIFSIGNALED(status)) {
                const int signal_number = WTERMSIG(status);
                finish(false);
                *error = "卸载载体被信号 " + std::to_string(signal_number) + " 终止";
                return false;
            }
            if (WIFSTOPPED(status)) {
                *error = "卸载载体 exit 系统调用返回了停止事件 signal=" +
                         std::to_string(WSTOPSIG(status));
                return false;
            }
            *error = "卸载载体 exit 系统调用返回未知 wait 状态";
            return false;
        }
        if (result < 0 && errno != EINTR) {
            *error = "等待卸载载体退出失败: " + std::string(std::strerror(errno));
            return false;
        }
        std::this_thread::sleep_until(std::min(
            deadline,
            std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
    }
    *error = "等待卸载载体退出超时";
    return false;
}

// 远程调用已消费终态时不再 waitpid；只接纳正常退出，精确线程身份由调用方继续复核。
bool ThreadFreeze::accept_exited_retained_thread(
    pid_t retained_tid,
    int wait_status,
    std::string* error) {
    if (pid_ <= 0 || retained_tid <= 0 || retained_tid == pid_ || attached_.size() != 1 ||
        attached_.front() != retained_tid || main_context_captured_) {
        *error = "卸载载体终态接纳参数或 ptrace 所有权无效";
        return false;
    }

    switch (classify_retained_thread_exit(wait_status)) {
        case RetainedThreadExitKind::Clean:
            break;
        case RetainedThreadExitKind::NonZero:
            *error = "卸载载体在 dlclose 期间以非零状态 " +
                     std::to_string(WEXITSTATUS(wait_status)) + " 退出";
            return false;
        case RetainedThreadExitKind::Signaled:
            *error = "卸载载体在 dlclose 期间被信号 " +
                     std::to_string(WTERMSIG(wait_status)) + " 终止";
            return false;
        case RetainedThreadExitKind::Unexpected:
            *error = "卸载载体在 dlclose 期间返回非终态 wait 状态";
            return false;
    }
    if (thread_vanished(pid_)) {
        *error = "卸载载体退出时目标主进程同时消失";
        return false;
    }

    forget_thread(retained_tid);
    if (attached_.empty()) {
        reset_state();
    }
    return true;
}

// 载体寄存器指向一次性调用现场后不再具备恢复条件，失败路径直接终止旧目标。
void ThreadFreeze::terminate_target() noexcept {
    const pid_t target = pid_;
    if (target > 0) {
        kill(target, SIGKILL);
    }
    const auto fallback_deadline =
        std::chrono::steady_clock::now() + std::chrono::milliseconds(250);
    const auto deadline = operation_deadline_ == std::chrono::steady_clock::time_point{}
                              ? fallback_deadline
                              : std::min(operation_deadline_, fallback_deadline);
    while (!attached_.empty() && std::chrono::steady_clock::now() < deadline) {
        const std::vector<pid_t> pending = attached_;
        for (const pid_t tid : pending) {
            int status = 0;
            const pid_t result = waitpid(tid, &status, __WALL | WNOHANG);
            if (result == tid || (result < 0 && errno == ECHILD)) {
                forget_thread(tid);
            }
        }
        if (!attached_.empty()) {
            std::this_thread::sleep_until(std::min(
                deadline,
                std::chrono::steady_clock::now() + std::chrono::milliseconds(5)));
        }
    }
    attached_.clear();
    reset_state();
}

// 析构和无独立截止时间的回滚沿用附加阶段的同一绝对截止时间。
bool ThreadFreeze::detach_all(std::string* error) {
    if (attached_.empty()) {
        reset_state();
        return true;
    }
    if (operation_deadline_ == std::chrono::steady_clock::time_point{}) {
        *error = "线程分离缺少事务截止时间";
        return false;
    }
    return detach_all_until(operation_deadline_, error);
}

// 主线程现场与跟踪关系恢复完成后，才允许工作线程继续运行。
bool ThreadFreeze::detach_all(std::uint32_t timeout_ms, std::string* error) {
    if (timeout_ms == 0) {
        *error = "线程分离时限无效";
        return false;
    }
    return detach_all_until(bounded_deadline(timeout_ms), error);
}

bool ThreadFreeze::detach_all_until(
    const std::chrono::steady_clock::time_point& deadline,
    std::string* error) {
    if (std::chrono::steady_clock::now() >= deadline) {
        *error = "恢复并分离线程前事务总时限已耗尽";
        return false;
    }
    std::string register_error;
    if (!restore_main_context(&register_error)) {
        *error = register_error;
        return false;
    }

    const bool main_attached =
        std::find(attached_.begin(), attached_.end(), pid_) != attached_.end();
    if (main_attached) {
        if (detach_one(pid_, deadline, false, error) != DetachResult::Detached) {
            return false;
        }
        forget_thread(pid_);
        main_xstate_.clear();
        main_context_captured_ = false;
    }

    bool success = true;
    std::vector<pid_t> remaining;
    remaining.reserve(attached_.size());
    for (const pid_t tid : attached_) {
        std::string detach_error;
        if (detach_one(tid, deadline, true, &detach_error) == DetachResult::Failed) {
            if (success) {
                *error = "分离工作线程失败: " + detach_error;
            }
            success = false;
            remaining.push_back(tid);
        }
    }

    attached_ = std::move(remaining);
    if (attached_.empty()) {
        reset_state();
    }
    return success;
}

std::chrono::steady_clock::time_point ThreadFreeze::bounded_deadline(
    std::uint32_t timeout_ms) const {
    const auto requested =
        std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
    if (operation_deadline_ == std::chrono::steady_clock::time_point{}) {
        return requested;
    }
    return std::min(operation_deadline_, requested);
}

void ThreadFreeze::reset_state() noexcept {
    pid_ = 0;
    main_registers_ = {};
    main_xstate_.clear();
    main_context_captured_ = false;
    operation_deadline_ = {};
}

}  // namespace azlw::loader

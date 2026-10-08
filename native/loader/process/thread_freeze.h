// 声明注入期间负责附加、冻结并可靠分离目标线程的作用域对象。

#pragma once

#include <chrono>
#include <cstdint>
#include <string>
#include <vector>

#include <sys/types.h>
#include <sys/user.h>
#include <sys/wait.h>

namespace azlw::loader {

/// 卸载前不得由任何冻结线程继续执行的半开地址区间。
struct ProtectedAddressRange final {
    std::uintptr_t start = 0;
    std::uintptr_t end = 0;
    std::string label;
};

/// 描述 ptrace 已消费的卸载载体终态，避免仅凭通用调用状态码判定成功。
enum class RetainedThreadExitKind : std::uint8_t { Clean, NonZero, Signaled, Unexpected };

/// 将 waitpid 原始状态分类为可接纳的正常退出或明确失败。
RetainedThreadExitKind classify_retained_thread_exit(int wait_status) noexcept;

/// 区分载体可执行 dlclose、提前终止和线程分离失败。
enum class DetachExceptStatus : std::uint8_t {
    ReadyForCarrierDlclose,
    CarrierExitedBeforeDlclose,
    Failed,
};

/// 保留载体事务的强类型结果；只有提前终止时 `wait_status` 才有意义。
struct DetachExceptResult final {
    DetachExceptStatus status = DetachExceptStatus::Failed;
    int wait_status = 0;
};

/// 只有保持停驻的载体可以执行 dlclose，提前退出表示卸载前置条件失效。
enum class DlcloseCarrierSelection : std::uint8_t {
    RetainedWorker,
    Reject,
};

/// 依据分离结果和原始 wait 状态选择唯一可验证的 dlclose 执行线程。
[[nodiscard]] inline DlcloseCarrierSelection select_dlclose_carrier(
    const DetachExceptResult& result) noexcept {
    if (result.status == DetachExceptStatus::ReadyForCarrierDlclose) {
        return DlcloseCarrierSelection::RetainedWorker;
    }
    return DlcloseCarrierSelection::Reject;
}

/// 在远程调用期间批量附加并冻结线程，并完整恢复主线程执行现场。
class ThreadFreeze final {
public:
    /// 创建尚未附加任何线程的空作用域对象。
    ThreadFreeze() = default;
    /// 尝试分离仍由当前对象持有的全部线程。
    ~ThreadFreeze();

    /// ptrace 线程所有权不可复制。
    ThreadFreeze(const ThreadFreeze&) = delete;
    ThreadFreeze& operator=(const ThreadFreeze&) = delete;

    /// 先停止工作线程，再附加并停止主线程，随后确认全部线程已冻结。
    bool attach_all(pid_t pid, std::uint32_t timeout_ms, std::string* error);
    /// 在时限内恢复并分离主线程及其他线程，只保留指定工作线程作为卸载载体。
    DetachExceptResult detach_all_except(
        pid_t retained_tid,
        std::uint32_t timeout_ms,
        std::string* error);
    /// 让唯一保留的工作线程执行 exit(0)，并在时限内回收其 ptrace 终态。
    bool exit_retained_thread(
        pid_t retained_tid,
        std::uintptr_t syscall_gadget,
        std::uint32_t timeout_ms,
        std::string* error);
    /// 接纳远程调用已经消费的 exit(0) 终态，并释放该载体的分离责任。
    bool accept_exited_retained_thread(
        pid_t retained_tid,
        int wait_status,
        std::string* error);
    /// 卸载载体现场已经无法恢复时终止整个目标，避免继续执行失效地址。
    void terminate_target() noexcept;
    /// 使用冻结时的操作上限恢复并分离全部线程。
    bool detach_all(std::string* error);
    /// 在指定时限内恢复现场，先分离主线程，再分离工作线程。
    bool detach_all(std::uint32_t timeout_ms, std::string* error);
    /// 排除卸载载体后，核对其余线程的 RIP 均已离开保护区间。
    bool verify_instruction_pointers_except(
        const std::vector<ProtectedAddressRange>& ranges,
        pid_t excluded_tid,
        std::string* error) const;

private:
    /// 区分成功建立跟踪、建立期间正常消失和真实失败。
    enum class SeizeResult : std::uint8_t { Seized, Vanished, Failed };
    /// 区分重新取得 ptrace-stop、普通线程退出和停止失败。
    enum class StopResult : std::uint8_t { Stopped, Exited, Failed };
    /// 区分完成分离、普通线程退出和所有权不可信。
    enum class DetachResult : std::uint8_t { Detached, Exited, Failed };

    /// 只建立单个线程的 ptrace 所有权，停止操作由调用方统一发起。
    SeizeResult seize_one(
        pid_t tid,
        const std::chrono::steady_clock::time_point& deadline,
        std::string* error);
    /// 先向整批已跟踪线程发送中断，再统一等待各自进入 ptrace-stop。
    bool interrupt_and_wait(
        const std::vector<pid_t>& tids,
        const std::chrono::steady_clock::time_point& deadline,
        std::string* error);
    /// 等待已中断线程进入可执行 ptrace 操作的停止状态。
    StopResult wait_for_stop(
        pid_t tid,
        const std::chrono::steady_clock::time_point& deadline,
        bool allow_worker_exit,
        std::string* error) const;
    /// 分离单个线程；普通线程只有在内核终态已确认时才按退出收敛。
    DetachResult detach_one(
        pid_t tid,
        const std::chrono::steady_clock::time_point& deadline,
        bool allow_worker_exit,
        std::string* error) const;
    /// 判断指定线程是否已从当前目标的任务目录消失。
    bool thread_vanished(pid_t tid) const;
    /// 只将主进程仍存活时消失的非主线程视为可收敛事件。
    bool worker_vanished(pid_t tid) const;
    /// 只把主进程仍存活时已消失或进入内核终态的工作线程视为退出。
    bool worker_exited(pid_t tid) const;
    /// 消费仍由当前 tracer 持有的工作线程终态，并保留原始 wait 状态。
    bool consume_worker_exit(
        pid_t tid,
        const std::chrono::steady_clock::time_point& deadline,
        int* wait_status,
        std::string* error) const;
    /// 从分离责任列表移除已经退出的线程。
    void forget_thread(pid_t tid);
    /// 在冻结总时限内保存主线程通用寄存器与扩展处理器状态。
    bool capture_main_context(
        const std::chrono::steady_clock::time_point& deadline,
        std::string* error);
    /// 读取一个已附加 x86_64 线程的完整通用寄存器。
    bool read_registers(pid_t tid, user_regs_struct* registers, std::string* error) const;
    /// 读取一个已附加 x86_64 线程的 FPU、SIMD 与 MXCSR 状态。
    bool read_xstate(pid_t tid, unsigned int regset, std::vector<std::uint8_t>* xstate, std::string* error) const;
    /// 在分离主线程前恢复注入事务开始时的完整执行现场。
    bool restore_main_context(std::string* error);
    /// 将调用方提供的相对时限约束在本次附加事务的绝对截止时间内。
    std::chrono::steady_clock::time_point bounded_deadline(std::uint32_t timeout_ms) const;
    /// 使用已经确定的绝对截止时间恢复并分离全部线程。
    bool detach_all_until(
        const std::chrono::steady_clock::time_point& deadline,
        std::string* error);
    /// 清除不再持有任何 ptrace 线程时的事务状态。
    void reset_state() noexcept;

    pid_t pid_ = 0;
    std::vector<pid_t> attached_;
    user_regs_struct main_registers_{};
    std::vector<std::uint8_t> main_xstate_;
    unsigned int main_state_regset_ = 0;
    bool main_context_captured_ = false;
    std::chrono::steady_clock::time_point operation_deadline_{};
};

}  // namespace azlw::loader

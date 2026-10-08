// 声明主线程单槽任务队列。队列不解释具体任务载荷。

#pragma once

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <vector>

#include "runtime/lua_tasks.h"

namespace azlw::agent {

/// 排队等待的结果。调用方把它们映射成既有队列错误。
enum class TaskWaitStatus : std::uint8_t { Completed, Canceled, TimedOut, Busy, NotAccepting };

/// 一帧批次推进之后，队列任务应继续、发布结果，还是因为取消丢掉本帧。
enum class BatchFrameDisposition : std::uint8_t { Continue, Publish, Drop };

/// 生产调度和宿主夹具共用。取消优先于本帧是否已经读完。
inline BatchFrameDisposition decide_batch_frame(TaskState state, bool step_continues) noexcept {
    if (state != TaskState::Running) {
        return BatchFrameDisposition::Drop;
    }
    return step_continues ? BatchFrameDisposition::Continue : BatchFrameDisposition::Publish;
}

/// 只负责单槽排队、运行标记、等待、超时取消和关闭排空。
class MainThreadTaskQueue {
public:
    using TimePoint = std::chrono::steady_clock::time_point;

    /// 队列已有任务或正在执行时返回 Busy，不覆盖当前任务。
    /// `accepting` 为 false 时不入队，避免关闭已经开始后仍接收新任务。
    TaskWaitStatus enqueue_and_wait(
        const std::shared_ptr<LuaTask>& task,
        TimePoint deadline,
        bool accepting);

    /// 每帧至多取出一个任务。已取消的队首任务本帧不再继续取下一个。
    std::shared_ptr<LuaTask> take_for_execution(bool accepting);

    /// 主线程处理结束后清除运行标记并唤醒关闭等待。
    void finish_execution();

    struct CanceledWaiting {
        std::vector<std::shared_ptr<LuaTask>> canceled;
        std::shared_ptr<LuaTask> running;
    };

    /// 取消尚未开始的任务，并返回仍在执行的任务供调用方决定命令派发门。
    CanceledWaiting cancel_waiting();

    /// 等待当前任务结束。超时返回 false，队列仍可能有正在执行的任务。
    bool wait_until_idle(TimePoint deadline);

    /// 非阻塞确认没有运行中的任务且队列为空。拿不到锁时视为仍在使用。
    bool try_confirm_idle();

    /// 等到至少有一个任务入队或开始执行。用于关闭和超时测试，不代替调度。
    bool wait_until_pending(TimePoint deadline);

    /// 等待超时后、复核最终状态前执行一次。生产调用保持为空。
    void set_timeout_recheck_probe(std::function<void()> probe);

private:
    std::mutex mutex_;
    std::condition_variable drain_;
    std::condition_variable enqueued_;
    std::function<void()> timeout_recheck_probe_;
    bool running_ = false;
    std::shared_ptr<LuaTask> running_task_;
    std::deque<std::shared_ptr<LuaTask>> queue_;
};

}  // namespace azlw::agent

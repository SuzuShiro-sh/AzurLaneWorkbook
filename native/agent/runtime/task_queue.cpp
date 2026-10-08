// 实现主线程单槽任务队列的等待、超时和关闭排空。

#include "runtime/task_queue.h"

#include <algorithm>
#include <utility>

namespace azlw::agent {

TaskWaitStatus MainThreadTaskQueue::enqueue_and_wait(
    const std::shared_ptr<LuaTask>& task,
    TimePoint deadline,
    bool accepting) {
    {
        std::lock_guard lock(mutex_);
        if (!accepting) {
            return TaskWaitStatus::NotAccepting;
        }
        if (!queue_.empty() || running_) {
            return TaskWaitStatus::Busy;
        }
        queue_.push_back(task);
        enqueued_.notify_all();
    }

    std::unique_lock task_lock(task->mutex);
    if (task->condition.wait_until(task_lock, deadline, [&task] {
            return task->state == TaskState::Completed || task->state == TaskState::Canceled;
        })) {
        return task->state == TaskState::Completed ? TaskWaitStatus::Completed
                                                   : TaskWaitStatus::Canceled;
    }
    task_lock.unlock();

    std::function<void()> timeout_recheck_probe;
    {
        std::lock_guard queue_lock(mutex_);
        timeout_recheck_probe = std::move(timeout_recheck_probe_);
    }
    if (timeout_recheck_probe) {
        timeout_recheck_probe();
    }

    {
        std::lock_guard queue_lock(mutex_);
        const auto queued = std::find(queue_.begin(), queue_.end(), task);
        if (queued != queue_.end()) {
            queue_.erase(queued);
        }
    }
    {
        std::lock_guard timeout_lock(task->mutex);
        if (task->state == TaskState::Completed) {
            return TaskWaitStatus::Completed;
        }
        if (task->state == TaskState::Pending || task->state == TaskState::Running) {
            task->state = TaskState::Canceled;
        }
    }
    return TaskWaitStatus::TimedOut;
}

std::shared_ptr<LuaTask> MainThreadTaskQueue::take_for_execution(bool accepting) {
    std::lock_guard queue_lock(mutex_);
    if (running_task_) {
        std::shared_ptr<LuaTask> finished;
        {
            std::lock_guard task_lock(running_task_->mutex);
            if (running_task_->state == TaskState::Running) {
                return running_task_;
            }
            finished = std::move(running_task_);
            running_ = false;
        }
        finished.reset();
        drain_.notify_all();
    }
    if (!accepting || queue_.empty()) {
        return nullptr;
    }
    std::shared_ptr<LuaTask> task = queue_.front();
    queue_.pop_front();
    std::lock_guard task_lock(task->mutex);
    if (task->state == TaskState::Canceled) {
        return nullptr;
    }
    task->state = TaskState::Running;
    running_ = true;
    running_task_ = task;
    return task;
}

void MainThreadTaskQueue::finish_execution() {
    std::lock_guard queue_lock(mutex_);
    running_task_.reset();
    running_ = false;
    drain_.notify_all();
}

MainThreadTaskQueue::CanceledWaiting MainThreadTaskQueue::cancel_waiting() {
    CanceledWaiting drained;
    std::lock_guard queue_lock(mutex_);
    while (!queue_.empty()) {
        std::shared_ptr<LuaTask> task = queue_.front();
        queue_.pop_front();
        std::lock_guard task_lock(task->mutex);
        if (task->state == TaskState::Pending) {
            task->state = TaskState::Canceled;
            drained.canceled.push_back(std::move(task));
        }
    }
    drained.running = running_task_;
    return drained;
}

bool MainThreadTaskQueue::wait_until_idle(TimePoint deadline) {
    std::unique_lock queue_lock(mutex_);
    return drain_.wait_until(queue_lock, deadline, [this] { return !running_; });
}

bool MainThreadTaskQueue::wait_until_pending(TimePoint deadline) {
    std::unique_lock queue_lock(mutex_);
    return enqueued_.wait_until(queue_lock, deadline, [this] {
        return !queue_.empty() || running_;
    });
}

void MainThreadTaskQueue::set_timeout_recheck_probe(std::function<void()> probe) {
    std::lock_guard queue_lock(mutex_);
    timeout_recheck_probe_ = std::move(probe);
}

bool MainThreadTaskQueue::try_confirm_idle() {
    std::unique_lock queue_lock(mutex_, std::try_to_lock);
    if (!queue_lock.owns_lock() || running_ || !queue_.empty()) {
        return false;
    }
    return true;
}

}  // namespace azlw::agent

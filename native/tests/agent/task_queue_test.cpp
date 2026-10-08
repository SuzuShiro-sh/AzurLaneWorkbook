// 验证主线程任务队列的单槽、超时、关闭取消和执行完成后的等待，不依赖游戏进程。

#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <iostream>
#include <mutex>
#include <thread>

#include "runtime/task_queue.h"

namespace {

using azlw::agent::BagLuaTask;
using azlw::agent::LuaTask;
using azlw::agent::MainThreadTaskQueue;
using azlw::agent::TaskState;
using azlw::agent::TaskWaitStatus;

int failures = 0;

void expect(bool condition, const char* message) {
    if (!condition) {
        std::cerr << message << '\n';
        ++failures;
    }
}

std::shared_ptr<LuaTask> bag_task() {
    return std::make_shared<LuaTask>(BagLuaTask{.max_items = 1, .outcome = {}});
}

void test_single_slot_rejects_a_second_task() {
    MainThreadTaskQueue queue;
    auto first = bag_task();
    auto second = bag_task();
    std::thread waiter([&] {
        const auto status = queue.enqueue_and_wait(
            first,
            std::chrono::steady_clock::now() + std::chrono::seconds(2),
            true);
        expect(status == TaskWaitStatus::Completed, "第一个任务应在执行后完成");
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "第一个任务应先进入队列");
    const auto busy = queue.enqueue_and_wait(
        second,
        std::chrono::steady_clock::now() + std::chrono::milliseconds(50),
        true);
    expect(busy == TaskWaitStatus::Busy, "单槽队列不能同时接收第二个任务");
    auto running = queue.take_for_execution(true);
    expect(running == first, "主线程应取到第一个任务");
    {
        std::lock_guard lock(running->mutex);
        running->state = TaskState::Completed;
    }
    queue.finish_execution();
    running->condition.notify_one();
    waiter.join();
}

void test_timeout_cancels_a_task_that_never_starts() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    const auto status = queue.enqueue_and_wait(
        task,
        std::chrono::steady_clock::now() + std::chrono::milliseconds(30),
        true);
    expect(status == TaskWaitStatus::TimedOut, "无人执行的任务应超时");
    expect(task->state == TaskState::Canceled, "超时后仍未开始的任务应取消");
    expect(queue.take_for_execution(true) == nullptr, "已取消任务不应进入执行");
}

void test_shutdown_cancels_pending_work_and_drains_the_running_task() {
    MainThreadTaskQueue queue;
    auto pending = bag_task();
    std::thread waiter([&] {
        expect(
            queue.enqueue_and_wait(
                pending,
                std::chrono::steady_clock::now() + std::chrono::seconds(2),
                true) == TaskWaitStatus::Canceled,
            "关闭应取消尚未开始的任务");
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "关闭前应观察到已入队任务");
    const auto drained = queue.cancel_waiting();
    expect(drained.running == nullptr, "尚未取出的任务不应记为正在执行");
    expect(drained.canceled.size() == 1, "排空应取消等待中的任务");
    for (const auto& task : drained.canceled) {
        task->condition.notify_one();
    }
    waiter.join();

    auto running = bag_task();
    std::thread running_waiter([&] {
        expect(
            queue.enqueue_and_wait(
                running,
                std::chrono::steady_clock::now() + std::chrono::seconds(2),
                true) == TaskWaitStatus::Completed,
            "正在执行的任务完成后等待方应观察到完成");
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "执行前应观察到已入队任务");
    expect(queue.take_for_execution(true) == running, "应取出正在执行的任务");
    expect(
        !queue.wait_until_idle(std::chrono::steady_clock::now()),
        "任务仍在执行时排空等待应超时");
    {
        std::lock_guard lock(running->mutex);
        running->state = TaskState::Completed;
    }
    queue.finish_execution();
    running->condition.notify_one();
    running_waiter.join();
    expect(
        queue.wait_until_idle(std::chrono::steady_clock::now() + std::chrono::milliseconds(50)),
        "执行结束后队列应空闲");
    expect(queue.try_confirm_idle(), "空闲队列应能非阻塞确认");
}

void test_completion_after_the_wait_deadline_is_not_canceled() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    queue.set_timeout_recheck_probe([&] {
        std::lock_guard lock(task->mutex);
        task->state = TaskState::Completed;
    });
    const auto status = queue.enqueue_and_wait(task, std::chrono::steady_clock::now(), true);
    expect(status == TaskWaitStatus::Completed, "等待截止后已完成的任务应返回完成");
    expect(task->state == TaskState::Completed, "迟到完成不应再被标成取消");
    expect(queue.take_for_execution(true) == nullptr, "已完成任务不应留在队列中");
}

void test_running_worker_close_allows_only_the_later_completion() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    std::mutex gate;
    std::condition_variable ready;
    bool taken = false;
    bool release = false;
    TaskWaitStatus status = TaskWaitStatus::Busy;
    std::thread worker([&] {
        expect(
            queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
            "工作线程应在确定同步点看到已入队任务");
        auto running = queue.take_for_execution(true);
        expect(running == task, "工作线程应取到这次任务");
        {
            std::lock_guard lock(gate);
            taken = true;
        }
        ready.notify_one();
        std::unique_lock lock(gate);
        ready.wait(lock, [&] { return release; });
        {
            std::lock_guard task_lock(running->mutex);
            expect(running->state == TaskState::Running, "关闭不得把正在执行的任务改成取消");
            running->state = TaskState::Completed;
        }
        queue.finish_execution();
        running->condition.notify_one();
    });
    std::thread waiter([&] {
        status = queue.enqueue_and_wait(
            task,
            std::chrono::steady_clock::now() + std::chrono::seconds(2),
            true);
    });
    {
        std::unique_lock lock(gate);
        expect(ready.wait_for(lock, std::chrono::seconds(2), [&] { return taken; }), "应到达执行同步点");
    }
    const auto drained = queue.cancel_waiting();
    expect(drained.running == task, "关闭应看到唯一正在执行的任务");
    expect(drained.canceled.empty(), "正在执行的任务不能再被排成取消");
    {
        std::lock_guard lock(gate);
        release = true;
    }
    ready.notify_one();
    worker.join();
    waiter.join();
    expect(status == TaskWaitStatus::Completed, "执行后的完成是唯一等待终态");
    expect(task->state == TaskState::Completed, "任务不能同时处于完成和取消");
}

void test_close_before_deadline_is_canceled() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    TaskWaitStatus status = TaskWaitStatus::Busy;
    std::thread waiter([&] {
        status = queue.enqueue_and_wait(
            task,
            std::chrono::steady_clock::now() + std::chrono::seconds(2),
            true);
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "关闭前应先看到已入队任务");
    const auto drained = queue.cancel_waiting();
    expect(drained.canceled.size() == 1, "期限前关闭应取消这个尚未开始的任务");
    expect(drained.running == nullptr, "期限前关闭没有正在执行的任务");
    for (const auto& canceled : drained.canceled) {
        canceled->condition.notify_one();
    }
    waiter.join();
    expect(status == TaskWaitStatus::Canceled, "先关闭再等到期限时，等待结果是取消");
    expect(task->state == TaskState::Canceled, "先关闭的任务状态是取消");
}

void test_deadline_before_close_is_timed_out() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    queue.set_timeout_recheck_probe([&] {
        const auto drained = queue.cancel_waiting();
        expect(drained.canceled.size() == 1, "期限到达后关闭应取消尚未开始的任务");
        expect(drained.running == nullptr, "期限到达时任务还没有开始执行");
    });
    const auto status = queue.enqueue_and_wait(task, std::chrono::steady_clock::now(), true);
    expect(status == TaskWaitStatus::TimedOut, "先到期限再关闭时，等待结果是超时");
    expect(task->state == TaskState::Canceled, "超时复核中的关闭把任务标成取消");
    expect(queue.take_for_execution(true) == nullptr, "终态任务不应留在队列");
}

void test_expired_running_task_is_released_after_its_lock() {
    MainThreadTaskQueue queue;
    bool released_while_locked = false;
    auto task = std::shared_ptr<LuaTask>(
        new LuaTask(BagLuaTask{}),
        [&](LuaTask* object) {
            std::thread observer([&] {
                if (object->mutex.try_lock()) {
                    object->mutex.unlock();
                } else {
                    released_while_locked = true;
                }
            });
            observer.join();
            delete object;
        });
    std::thread waiter([&] {
        queue.enqueue_and_wait(
            task,
            std::chrono::steady_clock::now() + std::chrono::milliseconds(80),
            true);
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "超时释放测试应先看到已入队任务");
    auto running = queue.take_for_execution(true);
    expect(running == task, "超时释放测试应取到正在执行的任务");
    running.reset();
    waiter.join();
    task.reset();
    expect(queue.take_for_execution(true) == nullptr, "超时后的任务不应再次执行");
    expect(!released_while_locked, "最后一个任务引用必须在任务锁解开后释放");
}

void test_cancel_between_frames_stays_canceled() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    TaskWaitStatus status = TaskWaitStatus::Busy;
    std::thread waiter([&] {
        status = queue.enqueue_and_wait(
            task,
            std::chrono::steady_clock::now() + std::chrono::seconds(2),
            true);
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "帧间取消前应先看到已入队任务");
    auto running = queue.take_for_execution(true);
    expect(running == task, "帧间取消应作用在正在执行的任务上");
    {
        std::lock_guard lock(task->mutex);
        task->state = TaskState::Canceled;
    }
    running.reset();
    expect(queue.take_for_execution(true) == nullptr, "已取消的跨帧任务不能再次取出");
    task->condition.notify_one();
    waiter.join();
    expect(status == TaskWaitStatus::Canceled, "帧间取消的等待结果是取消");
    expect(task->state == TaskState::Canceled, "后续收尾不能把取消改回完成");
}

void test_catalog_cancel_does_not_publish_the_frame() {
    using azlw::agent::BatchFrameDisposition;
    using azlw::agent::ShipCatalogLuaTask;
    using azlw::agent::decide_batch_frame;
    expect(
        decide_batch_frame(TaskState::Running, true) == BatchFrameDisposition::Continue,
        "未取消的续帧应继续");
    expect(
        decide_batch_frame(TaskState::Running, false) == BatchFrameDisposition::Publish,
        "读完的帧应发布");
    expect(
        decide_batch_frame(TaskState::Canceled, false) == BatchFrameDisposition::Drop,
        "取消后不发布本帧");
    MainThreadTaskQueue queue;
    ShipCatalogLuaTask catalog;
    catalog.progress.outcome.success = true;
    catalog.progress.outcome.page.complete = true;
    auto task = std::make_shared<LuaTask>(std::move(catalog));
    TaskWaitStatus status = TaskWaitStatus::Busy;
    std::thread waiter([&] {
        status = queue.enqueue_and_wait(
            task,
            std::chrono::steady_clock::now() + std::chrono::seconds(2),
            true);
    });
    expect(
        queue.wait_until_pending(std::chrono::steady_clock::now() + std::chrono::seconds(2)),
        "图鉴任务应入队");
    auto running = queue.take_for_execution(true);
    expect(running == task, "图鉴任务应进入执行");
    {
        std::lock_guard lock(task->mutex);
        task->state = TaskState::Running;
    }
    expect(
        decide_batch_frame(TaskState::Running, true) == BatchFrameDisposition::Continue,
        "第一帧未完成时应继续");
    {
        std::lock_guard lock(task->mutex);
        task->state = TaskState::Canceled;
    }
    expect(
        decide_batch_frame(task->state, false) == BatchFrameDisposition::Drop,
        "帧间取消后不应发布");
    queue.finish_execution();
    task->condition.notify_one();
    waiter.join();
    expect(status == TaskWaitStatus::Canceled, "图鉴帧间取消的等待结果是取消");
    expect(task->state == TaskState::Canceled, "图鉴任务不能在取消后标成完成");
    const auto& published = std::get<ShipCatalogLuaTask>(task->body);
    expect(!published.outcome.success, "取消不能把帧内进度写成成功结果");
}

void test_not_accepting_does_not_enqueue() {
    MainThreadTaskQueue queue;
    auto task = bag_task();
    const auto status = queue.enqueue_and_wait(
        task,
        std::chrono::steady_clock::now() + std::chrono::seconds(1),
        false);
    expect(status == TaskWaitStatus::NotAccepting, "关闭后不应再接收任务");
    expect(queue.take_for_execution(true) == nullptr, "被拒绝的任务不应留在队列中");
}

}  // namespace

int main() {
    test_single_slot_rejects_a_second_task();
    test_timeout_cancels_a_task_that_never_starts();
    test_completion_after_the_wait_deadline_is_not_canceled();
    test_shutdown_cancels_pending_work_and_drains_the_running_task();
    test_running_worker_close_allows_only_the_later_completion();
    test_close_before_deadline_is_canceled();
    test_deadline_before_close_is_timed_out();
    test_expired_running_task_is_released_after_its_lock();
    test_cancel_between_frames_stays_canceled();
    test_catalog_cancel_does_not_publish_the_frame();
    test_not_accepting_does_not_enqueue();
    if (failures != 0) {
        std::cerr << failures << " 个任务队列断言失败\n";
        return EXIT_FAILURE;
    }
    return EXIT_SUCCESS;
}

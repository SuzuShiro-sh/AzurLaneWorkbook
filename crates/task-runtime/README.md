# suzushiro-task-runtime

`suzushiro-task-runtime` 提供轻量级异步后台任务执行框架，支持多线程任务派发、进度报告与协作式取消。

## 典型用法

```rust
use suzushiro_task_runtime::{BackgroundTask, TaskEvent, TaskFailure};

let task = BackgroundTask::new(|context| {
    // 报告进度并响应取消信号
    context.report_progress(50, "processing").map_err(TaskFailure::from_signal)?;
    Ok::<_, TaskFailure>("done")
});

let running = task.spawn(|| {});

loop {
    match running.try_recv() {
        Ok(TaskEvent::Succeeded { output }) => {
            assert_eq!(output, "done");
            break;
        }
        Ok(TaskEvent::Failed { failure }) => panic!("{failure:?}"),
        Ok(TaskEvent::Cancelled) => break,
        Ok(TaskEvent::Progress(progress)) => println!("进度百分比: {:?}", progress.percent()),
        Err(std::sync::mpsc::TryRecvError::Empty) => std::thread::yield_now(),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
    }
}

running.finish()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## 核心设计

- **协作式取消**：`running.cancel()` 标记取消请求，任务闭包通过 `context.is_cancelled()` 或在 `report_progress()` 时协同响应并安全退出。
- **阶段进度感知**：支持通过 `report_activity(Some((completed, total)), message)` 汇报分阶段定量进度，或使用 `None` 表达持续等待状态。
- **非阻塞通知**：通过非阻塞事件接收管道将任务阶段通知无缝对接至 GUI、CLI 或 MCP 事件循环中。

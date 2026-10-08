//! 固化后台任务运行时经公共接口暴露的进度、取消和异常终态契约。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use suzushiro_task_runtime::{BackgroundTask, RunningTask, TaskEvent, TaskFailure};

const EVENT_TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn public_api_delivers_progress_before_success_and_notifies_each_event() {
    let task: BackgroundTask = BackgroundTask::new(|context| {
        context
            .report_progress(40, "正在检查文件")
            .map_err(TaskFailure::from_signal)?;
        Ok("检查完成".to_owned())
    });
    let wake_count = Arc::new(AtomicUsize::new(0));
    let wake_counter = Arc::clone(&wake_count);
    let running = task.spawn(move || {
        wake_counter.fetch_add(1, Ordering::AcqRel);
    });

    let progress = receive_event(&running);
    let success = receive_event(&running);
    assert!(matches!(progress, TaskEvent::Progress(_)));
    assert_eq!(
        success,
        TaskEvent::Succeeded {
            output: "检查完成".to_owned(),
        }
    );
    running.finish().unwrap();
    assert_eq!(wake_count.load(Ordering::Acquire), 2);
}

#[test]
fn public_api_reports_cancellation_at_an_explicit_checkpoint() {
    let barrier = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&barrier);
    let task: BackgroundTask = BackgroundTask::new(move |context| {
        worker_barrier.wait();
        worker_barrier.wait();
        context
            .report_progress(50, "不应入队")
            .map_err(TaskFailure::from_signal)?;
        Ok("不应成功".to_owned())
    });
    let running = task.spawn(|| {});

    barrier.wait();
    running.cancel();
    barrier.wait();
    assert_eq!(receive_event(&running), TaskEvent::Cancelled);
    running.finish().unwrap();
}

#[test]
fn public_api_converts_worker_panic_into_a_diagnostic_failure() {
    let task: BackgroundTask =
        BackgroundTask::new(|_| -> Result<String, TaskFailure> { panic!("fixture panic") });
    let running = task.spawn(|| {});

    match receive_event(&running) {
        TaskEvent::Failed { failure } => {
            assert_eq!(failure.user_message(), "操作意外终止");
            assert!(failure.detail().contains("未捕获 panic"));
            assert!(failure.is_uncertain());
        }
        other => panic!("预期失败终态，实际为 {other:?}"),
    }
    running.finish().unwrap();
}

fn receive_event(running: &RunningTask) -> TaskEvent {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        match running.try_recv() {
            Ok(event) => return event,
            Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                thread::yield_now();
            }
            Err(error) => panic!("等待后台任务事件失败: {error}"),
        }
    }
}

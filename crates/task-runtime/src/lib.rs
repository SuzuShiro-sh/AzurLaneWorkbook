//! 提供与宿主框架和业务领域无关的后台任务、进度、取消和终态管理。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

use thiserror::Error;

type TaskAction<T> = dyn Fn(TaskContext<T>) -> Result<T, TaskFailure> + Send + Sync + 'static;
type TaskNotifier = dyn Fn() + Send + Sync + 'static;

/// 可以重复启动、但每次都建立独立取消令牌和事件通道的后台操作。
#[derive(Clone)]
pub struct BackgroundTask<T = String> {
    action: Arc<TaskAction<T>>,
}

impl<T> BackgroundTask<T>
where
    T: Send + 'static,
{
    /// 从线程安全闭包建立后台操作；调用方负责遵守宿主资源的线程约束。
    pub fn new<F>(action: F) -> Self
    where
        F: Fn(TaskContext<T>) -> Result<T, TaskFailure> + Send + Sync + 'static,
    {
        Self {
            action: Arc::new(action),
        }
    }

    /// 启动一次操作，并在每个事件入队后调用宿主提供的通知器。
    pub fn spawn<N>(&self, notifier: N) -> RunningTask<T>
    where
        N: Fn() + Send + Sync + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let cancellation: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let notifier: Arc<TaskNotifier> = Arc::new(notifier);
        let context: TaskContext<T> = TaskContext {
            sender: sender.clone(),
            cancellation: Arc::clone(&cancellation),
            notifier: Arc::clone(&notifier),
        };
        let action: Arc<TaskAction<T>> = Arc::clone(&self.action);
        let worker: JoinHandle<()> = thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| action(context.clone())));
            let event: TaskEvent<T> = match result {
                Ok(Ok(output)) => TaskEvent::Succeeded { output },
                Ok(Err(failure)) if failure.is_cancelled() => TaskEvent::Cancelled,
                Ok(Err(failure)) => TaskEvent::Failed { failure },
                Err(_) => TaskEvent::Failed {
                    failure: TaskFailure::unexpected_termination(
                        "操作意外终止",
                        "后台任务发生未捕获 panic，线程已经停止",
                    ),
                },
            };
            if sender.send(event).is_ok() {
                notifier();
            }
        });

        RunningTask {
            receiver,
            cancellation,
            worker: Some(worker),
        }
    }
}

/// 后台闭包用于报告进度和观察协作式取消的受限入口。
pub struct TaskContext<T = String> {
    sender: mpsc::Sender<TaskEvent<T>>,
    cancellation: Arc<AtomicBool>,
    notifier: Arc<TaskNotifier>,
}

impl<T> Clone for TaskContext<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            cancellation: Arc::clone(&self.cancellation),
            notifier: Arc::clone(&self.notifier),
        }
    }
}

impl<T> TaskContext<T> {
    /// 入队一条 0 至 100 的进度消息；取消或接收端关闭时明确返回错误。
    pub fn report_progress(
        &self,
        percent: u8,
        message: impl Into<String>,
    ) -> Result<(), TaskSignalError> {
        if percent > 100 {
            return Err(TaskSignalError::InvalidProgress { percent });
        }
        if self.is_cancelled() {
            return Err(TaskSignalError::Cancelled);
        }
        self.sender
            .send(TaskEvent::Progress(TaskProgress {
                percent: Some(percent),
                units: None,
                message: message.into(),
            }))
            .map_err(|_| TaskSignalError::ReceiverClosed)?;
        (self.notifier)();
        Ok(())
    }

    /// 上报实际阶段观测，不作为取消检查点，保证业务收尾流程继续执行。
    pub fn report_activity(
        &self,
        units: Option<(usize, usize)>,
        message: impl Into<String>,
    ) -> Result<(), TaskSignalError> {
        let percent = match units {
            Some((completed, total)) if total > 0 && completed <= total => {
                Some(((completed as u128 * 100) / total as u128) as u8)
            }
            Some((completed, total)) => {
                return Err(TaskSignalError::InvalidUnits { completed, total });
            }
            None => None,
        };
        self.sender
            .send(TaskEvent::Progress(TaskProgress {
                percent,
                units,
                message: message.into(),
            }))
            .map_err(|_| TaskSignalError::ReceiverClosed)?;
        (self.notifier)();
        Ok(())
    }

    /// 返回调用方是否已经请求当前操作尽快停止。
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }

    /// 把取消标志交给工作线程。标志与界面请求是同一个。
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancellation)
    }
}

/// 后台任务向事件消费者发送的稳定事件集合。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskEvent<T = String> {
    Progress(TaskProgress),
    Succeeded { output: T },
    Failed { failure: TaskFailure },
    Cancelled,
}

/// 单条用户可见的任务进度。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskProgress {
    percent: Option<u8>,
    units: Option<(usize, usize)>,
    message: String,
}

impl TaskProgress {
    /// 返回当前阶段的完成比例；未知总量不产生百分比。
    pub fn percent(&self) -> Option<u8> {
        self.percent
    }

    /// 返回实际完成项与总项数，百分比事件不包含计数。
    pub fn units(&self) -> Option<(usize, usize)> {
        self.units
    }

    /// 返回不包含底层实现细节的当前操作文本。
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// 同时保存简短失败摘要和供日志记录的完整失败上下文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskFailure {
    user_message: String,
    detail: String,
    cancelled: bool,
    uncertain: bool,
}

impl TaskFailure {
    /// 建立一条不会丢失底层失败原因的任务错误。
    pub fn new(user_message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            user_message: user_message.into(),
            detail: detail.into(),
            cancelled: false,
            uncertain: false,
        }
    }

    /// 建立一条无法判断副作用是否已经发生的内部异常终态。
    pub fn unexpected_termination(
        user_message: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            user_message: user_message.into(),
            detail: detail.into(),
            cancelled: false,
            uncertain: true,
        }
    }

    /// 把进度通道错误转换为可展示且可诊断的任务失败。
    pub fn from_signal(error: TaskSignalError) -> Self {
        Self {
            user_message: "操作已停止".to_owned(),
            detail: error.to_string(),
            cancelled: matches!(error, TaskSignalError::Cancelled),
            uncertain: false,
        }
    }

    /// 返回调用方可直接展示或重新映射的简短错误。
    pub fn user_message(&self) -> &str {
        &self.user_message
    }

    /// 返回日志和测试使用的完整错误上下文。
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// 返回操作是否在显式安全检查点观察到取消请求。
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// 返回后台线程是否在无法确认副作用终态的情况下异常结束。
    pub const fn is_uncertain(&self) -> bool {
        self.uncertain
    }
}

/// 仍在运行或等待调用方回收的单次后台任务。
pub struct RunningTask<T = String> {
    receiver: Receiver<TaskEvent<T>>,
    cancellation: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl<T> RunningTask<T> {
    /// 非阻塞读取下一条任务事件。
    pub fn try_recv(&self) -> Result<TaskEvent<T>, TryRecvError> {
        self.receiver.try_recv()
    }

    /// 幂等请求后台操作在下一个安全检查点停止。
    pub fn cancel(&self) {
        self.cancellation.store(true, Ordering::Release);
    }

    /// 在收到终态事件后回收工作线程，并报告线程边界异常。
    pub fn finish(mut self) -> Result<(), TaskJoinError> {
        let worker: JoinHandle<()> = self.worker.take().ok_or(TaskJoinError::AlreadyJoined)?;
        worker.join().map_err(|_| TaskJoinError::WorkerPanicked)
    }
}

impl<T> Drop for RunningTask<T> {
    /// 调用方提前释放任务句柄时仍发出取消请求；合作式任务随后自行结束。
    fn drop(&mut self) {
        self.cancel();
    }
}

/// 任务无法接受进度事件或已经收到取消请求。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TaskSignalError {
    #[error("任务进度 {percent} 超出 0 至 100")]
    InvalidProgress { percent: u8 },
    #[error("任务完成量 {completed}/{total} 无效")]
    InvalidUnits { completed: usize, total: usize },
    #[error("任务已收到取消请求")]
    Cancelled,
    #[error("任务事件接收端已经关闭")]
    ReceiverClosed,
}

/// 终态事件之后的工作线程无法正常回收。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TaskJoinError {
    #[error("工作线程已经回收")]
    AlreadyJoined,
    #[error("工作线程在终态事件之外异常退出")]
    WorkerPanicked,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::{BackgroundTask, TaskEvent};

    #[test]
    fn activity_keeps_cleanup_observable_after_cancellation() {
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let task: BackgroundTask<()> = BackgroundTask::new(move |context| {
            worker_barrier.wait();
            worker_barrier.wait();
            assert!(context.is_cancelled());
            context.report_activity(None, "正在清理").unwrap();
            Ok(())
        });
        let running = task.spawn(|| {});
        barrier.wait();
        running.cancel();
        barrier.wait();
        assert!(matches!(
            running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            TaskEvent::Progress(_)
        ));
        assert!(matches!(
            running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            TaskEvent::Succeeded { output: () }
        ));
        running.finish().unwrap();
    }

    #[test]
    fn activity_reports_real_units_and_unknown_stages_without_canceling_cleanup() {
        let task: BackgroundTask<()> = BackgroundTask::new(|context| {
            context.report_activity(Some((1, 3)), "写表").unwrap();
            context.report_activity(None, "清理").unwrap();
            assert!(context.report_activity(Some((1, 0)), "invalid").is_err());
            assert!(context.report_activity(Some((4, 3)), "invalid").is_err());
            context
                .report_activity(Some((usize::MAX, usize::MAX)), "大计数")
                .unwrap();
            Ok(())
        });
        let running = task.spawn(|| {});
        for (percent, units) in [
            (Some(33), Some((1, 3))),
            (None, None),
            (Some(100), Some((usize::MAX, usize::MAX))),
        ] {
            let TaskEvent::Progress(event) = running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
            else {
                panic!("expected progress")
            };
            assert_eq!(event.percent(), percent);
            assert_eq!(event.units(), units);
        }
        assert!(matches!(
            running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            TaskEvent::Succeeded { output: () }
        ));
        running.finish().unwrap();
    }

    #[test]
    fn successful_action_return_wins_over_late_cancellation() {
        let barrier: Arc<Barrier> = Arc::new(Barrier::new(2));
        let worker_barrier: Arc<Barrier> = Arc::clone(&barrier);
        let task: BackgroundTask = BackgroundTask::new(move |_| {
            worker_barrier.wait();
            worker_barrier.wait();
            Ok("不应展示".to_owned())
        });
        let running = task.spawn(|| {});

        barrier.wait();
        running.cancel();
        barrier.wait();
        assert_eq!(
            running
                .receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap(),
            TaskEvent::Succeeded {
                output: "不应展示".to_owned()
            }
        );
        running.finish().unwrap();
    }

    #[test]
    fn notifier_panic_keeps_the_terminal_event_and_fails_join() {
        let task: BackgroundTask = BackgroundTask::new(|_| Ok("完成".to_owned()));
        let running = task.spawn(|| panic!("fixture notifier panic"));
        let event: TaskEvent = running
            .receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();

        assert_eq!(
            event,
            TaskEvent::Succeeded {
                output: "完成".to_owned(),
            }
        );
        assert_eq!(running.finish(), Err(super::TaskJoinError::WorkerPanicked));
    }
}

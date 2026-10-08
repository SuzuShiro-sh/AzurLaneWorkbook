//! 业务终态中的阶段结果和会话清理事实。

use super::AppError;
use super::execution::ExecutionReportStatus;

/// 一次操作交给日志的终态。字段由这个类型生成，不从文案反推。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationTerminal {
    /// 业务和需要的收尾都已完成。
    Succeeded,
    /// 业务明确失败。
    Failed,
    /// 可能已经写入，但结果无法确认。
    Unknown,
    /// 调用方在完成前停止。
    Cancelled,
    /// 业务已经发生，但清理或保存没有全部完成。
    Incomplete,
}

impl OperationTerminal {
    /// 日志事件使用的稳定状态名。
    pub const fn status(self) -> &'static str {
        match self {
            Self::Succeeded => "ok",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
        }
    }

    /// 只有无法确认的执行结果才标记为不确定。
    pub const fn uncertain(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// 从执行报告状态得到日志终态，保留未知和取消。
    pub const fn from_execution(status: ExecutionReportStatus) -> Self {
        match status {
            ExecutionReportStatus::Success => Self::Succeeded,
            ExecutionReportStatus::Failed => Self::Failed,
            ExecutionReportStatus::Unknown => Self::Unknown,
            ExecutionReportStatus::Cancelled => Self::Cancelled,
        }
    }

    /// 收尾未完成时保留无法确认、已取消和业务失败。只有业务成功后的收尾缺口才是未完成。
    pub const fn with_incomplete_tail(self) -> Self {
        match self {
            Self::Unknown | Self::Cancelled | Self::Failed => self,
            Self::Succeeded | Self::Incomplete => Self::Incomplete,
        }
    }
}

/// 一个收尾阶段实际走到的结果。
#[derive(Debug)]
pub enum StageResult<T> {
    /// 前置阶段未通过，本阶段没有开始。
    NotAttempted,
    /// 本阶段已经尝试并失败。
    Failed(AppError),
    /// 本阶段已经完成。
    Completed(T),
}

impl<T> StageResult<T> {
    pub(crate) fn from_result(result: Result<T, AppError>) -> Self {
        match result {
            Ok(value) => Self::Completed(value),
            Err(error) => Self::Failed(error),
        }
    }

    pub(crate) const fn failed(&self) -> Option<&AppError> {
        match self {
            Self::Failed(error) => Some(error),
            Self::NotAttempted | Self::Completed(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn completed(&self) -> Option<&T> {
        match self {
            Self::Completed(value) => Some(value),
            Self::NotAttempted | Self::Failed(_) => None,
        }
    }
}

/// 游戏会话关闭的最终事实。
/// 错误上的诊断上下文继续保留原因和日志字段，调用方只匹配本类型。
#[derive(Debug)]
pub enum SessionCleanup {
    Completed,
    /// 正常卸载失败，恢复清理已经完成。
    Recovered(AppError),
    /// 清理没有完成。
    Failed(AppError),
}

impl SessionCleanup {
    pub(crate) fn from_shutdown(result: Result<(), AppError>) -> Self {
        match result {
            Ok(()) => Self::Completed,
            Err(error) => Self::from_error(error),
        }
    }

    /// 按关闭所有者给出的清理事实分类。没有事实时视为尚未完成。
    pub(crate) fn from_error(error: AppError) -> Self {
        match error.cleanup_fact() {
            Some(super::CleanupFact::Recovered) => Self::Recovered(error),
            _ => Self::Failed(error),
        }
    }

    pub const fn error(&self) -> Option<&AppError> {
        match self {
            Self::Completed => None,
            Self::Recovered(error) | Self::Failed(error) => Some(error),
        }
    }
}

//! 描述生成、检查、执行和读取的当前阶段，只记录已完成数量，不估算总耗时比例。

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationProgress {
    pub(crate) message: String,
    pub(crate) units: Option<(usize, usize)>,
}

impl OperationProgress {
    pub(crate) fn stage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            units: None,
        }
    }

    pub(crate) fn counted(message: impl Into<String>, completed: usize, total: usize) -> Self {
        Self {
            message: message.into(),
            units: Some((completed, total)),
        }
    }
}

/// 一次带进度的操作。进度转发失败只记下第一条，不替换业务结果。
pub(crate) struct ProgressDrive<T> {
    pub(crate) value: T,
    pub(crate) progress_error: Option<String>,
}

/// 把业务进度交给调用方的通知通道，并保留最先发生的通知失败。
pub(crate) fn drive_progress<T, E: std::fmt::Display>(
    mut operation: impl FnMut(&mut dyn FnMut(OperationProgress)) -> T,
    mut forward: impl FnMut(&OperationProgress) -> Result<(), E>,
) -> ProgressDrive<T> {
    let mut progress_error = None;
    let value = operation(&mut |progress| {
        if let Err(error) = forward(&progress) {
            progress_error.get_or_insert_with(|| error.to_string());
        }
    });
    ProgressDrive {
        value,
        progress_error,
    }
}

#[cfg(test)]
mod tests {
    use super::{OperationProgress, drive_progress};

    #[test]
    fn progress_forwarding_keeps_the_business_value_and_the_first_error() {
        let driven = drive_progress(
            |progress| {
                progress(OperationProgress::stage("读取"));
                progress(OperationProgress::counted("写入", 1, 2));
                7
            },
            |progress| {
                if progress.units.is_some() {
                    Err("通道已关闭")
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(driven.value, 7);
        assert_eq!(driven.progress_error.as_deref(), Some("通道已关闭"));

        let driven = drive_progress(
            |progress| {
                progress(OperationProgress::stage("完成"));
                "ok"
            },
            |_: &OperationProgress| Ok::<(), &str>(()),
        );
        assert_eq!(driven.value, "ok");
        assert!(driven.progress_error.is_none());
    }
}

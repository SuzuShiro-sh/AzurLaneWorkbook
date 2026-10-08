//! 获取方式缓存更新的应用结果。界面和命令行使用同一份事实。

use serde::{Deserialize, Serialize};

/// 补齐缺失或上次失败的资料，或明确刷新全部资料。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionUpdateMode {
    #[default]
    Missing,
    Refresh,
}

/// 一个舰船名称的缓存更新结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AcquisitionCacheUpdate {
    name: String,
    outcome: AcquisitionCacheUpdateOutcome,
}

/// 缓存更新的确定结果。不靠文案猜测。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionCacheUpdateOutcome {
    /// 本次已写入新的缓存。
    Updated,
    /// 已有有效缓存，本次无需联网。
    Cached,
    /// BWiki 明确没有收录页面。
    Missing,
    /// 在线更新没有替换已有缓存。
    KeptPrevious { detail: String },
    /// 没有写入缓存。
    Failed { detail: String },
    /// 取消后没有开始这项请求。
    NotStarted,
}

impl AcquisitionCacheUpdate {
    pub(crate) fn new(name: impl Into<String>, outcome: AcquisitionCacheUpdateOutcome) -> Self {
        Self {
            name: name.into(),
            outcome,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn outcome(&self) -> &AcquisitionCacheUpdateOutcome {
        &self.outcome
    }

    pub fn updated_count(items: &[Self]) -> usize {
        items
            .iter()
            .filter(|item| matches!(item.outcome, AcquisitionCacheUpdateOutcome::Updated))
            .count()
    }

    /// 从逐项事实得到唯一终态。已经更新的项目不能把取消写成成功。
    pub fn operation_terminal(items: &[Self]) -> super::OperationTerminal {
        if items
            .iter()
            .any(|item| matches!(item.outcome, AcquisitionCacheUpdateOutcome::NotStarted))
        {
            return super::OperationTerminal::Cancelled;
        }
        let completed = items.iter().any(|item| {
            matches!(
                item.outcome,
                AcquisitionCacheUpdateOutcome::Updated
                    | AcquisitionCacheUpdateOutcome::Cached
                    | AcquisitionCacheUpdateOutcome::Missing
            )
        });
        let unfinished = items.iter().any(|item| {
            matches!(
                item.outcome,
                AcquisitionCacheUpdateOutcome::Failed { .. }
                    | AcquisitionCacheUpdateOutcome::KeptPrevious { .. }
            )
        });
        if !unfinished {
            super::OperationTerminal::Succeeded
        } else if !completed {
            super::OperationTerminal::Failed
        } else {
            super::OperationTerminal::Incomplete
        }
    }
}

/// 调用已经组装好的缓存端口。引导层不在这里读取工作簿或发起网络请求。
pub(crate) fn update_ship_acquisition_cache<P>(
    port: &P,
    names: &[String],
    mode: AcquisitionUpdateMode,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<AcquisitionCacheUpdate>, crate::application::AppError>
where
    P: ShipAcquisitionCachePort + ?Sized,
{
    port.update_cache(names, mode, progress, is_cancelled)
}

/// 适配器实现的缓存更新能力。
pub(crate) trait ShipAcquisitionCachePort {
    fn update_cache(
        &self,
        names: &[String],
        mode: AcquisitionUpdateMode,
        progress: &mut dyn FnMut(crate::application::OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<AcquisitionCacheUpdate>, crate::application::AppError>;
}

#[cfg(test)]
mod tests {
    use super::{AcquisitionCacheUpdate, AcquisitionCacheUpdateOutcome};
    use crate::application::OperationTerminal;

    #[test]
    fn cached_and_missing_are_complete_but_do_not_hide_failures() {
        use AcquisitionCacheUpdateOutcome as Outcome;
        let mut items = vec![
            AcquisitionCacheUpdate::new("甲", Outcome::Cached),
            AcquisitionCacheUpdate::new("乙", Outcome::Missing),
        ];
        assert_eq!(
            AcquisitionCacheUpdate::operation_terminal(&items),
            OperationTerminal::Succeeded
        );
        assert_eq!(AcquisitionCacheUpdate::updated_count(&items), 0);
        items.push(AcquisitionCacheUpdate::new(
            "丙",
            Outcome::Failed {
                detail: "失败".to_owned(),
            },
        ));
        assert_eq!(
            AcquisitionCacheUpdate::operation_terminal(&items),
            OperationTerminal::Incomplete
        );
    }

    #[test]
    fn cancel_after_an_update_stays_cancelled() {
        let items = vec![
            AcquisitionCacheUpdate::new("标枪", AcquisitionCacheUpdateOutcome::Updated),
            AcquisitionCacheUpdate::new("拉菲", AcquisitionCacheUpdateOutcome::NotStarted),
        ];
        assert_eq!(
            AcquisitionCacheUpdate::operation_terminal(&items),
            OperationTerminal::Cancelled
        );
        assert_eq!(
            AcquisitionCacheUpdate::operation_terminal(&[AcquisitionCacheUpdate::new(
                "标枪",
                AcquisitionCacheUpdateOutcome::KeptPrevious {
                    detail: "保留".to_owned(),
                },
            )]),
            OperationTerminal::Failed
        );
        assert_eq!(
            AcquisitionCacheUpdate::operation_terminal(&[
                AcquisitionCacheUpdate::new("标枪", AcquisitionCacheUpdateOutcome::Updated),
                AcquisitionCacheUpdate::new(
                    "拉菲",
                    AcquisitionCacheUpdateOutcome::Failed {
                        detail: "失败".to_owned(),
                    },
                ),
            ]),
            OperationTerminal::Incomplete
        );
    }
}

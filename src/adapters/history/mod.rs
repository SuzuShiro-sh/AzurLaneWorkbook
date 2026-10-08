//! 提供各类历史记录共用的文件名输入、时间戳和相对路径规则。

pub(crate) mod catalog;
pub(crate) mod check;
pub mod execution;

use std::path::Path;
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

use thiserror::Error;

/// 所有不可覆盖历史记录共用的工具根目录相对位置。
pub(crate) use crate::application::HISTORY_DIRECTORY;
/// 历史记录继续通过本模块暴露规范 SHA-256 判断，避免消费者依赖摘要实现位置。
pub(crate) use suzushiro_text_format::is_canonical_sha256;

/// 历史记录共用元数据无法建立。
#[derive(Debug, Error)]
pub(crate) enum HistoryMetadataError {
    #[error("工作簿名称无效: {message}")]
    InvalidWorkbookName { message: String },
    #[error("读取历史记录时间失败: {0}")]
    Clock(#[source] SystemTimeError),
    #[error("历史记录时间超出 i64 Unix 毫秒范围")]
    ClockOverflow,
}

/// 验证名称只能引用工作簿目录内的单个 XLSX 文件。
pub(crate) fn validate_workbook_name(workbook_name: &str) -> Result<(), HistoryMetadataError> {
    if workbook_name.is_empty()
        || workbook_name.contains(['/', '\\'])
        || workbook_name.trim() != workbook_name
        || !workbook_name.to_ascii_lowercase().ends_with(".xlsx")
    {
        return Err(HistoryMetadataError::InvalidWorkbookName {
            message: "工作簿必须是 data/workbooks 内的单个 .xlsx 文件名".to_owned(),
        });
    }
    Ok(())
}

/// 返回当前 Unix 毫秒时间戳，并拒绝系统时钟和整数范围异常。
pub(crate) fn current_unix_millis() -> Result<i64, HistoryMetadataError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(HistoryMetadataError::Clock)?;
    i64::try_from(duration.as_millis()).map_err(|_| HistoryMetadataError::ClockOverflow)
}

/// 将工具根目录内的相对路径稳定编码为正斜杠文本。
pub(crate) fn relative_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::is_canonical_sha256;

    #[test]
    fn accepts_only_canonical_sha256_text() {
        assert!(is_canonical_sha256(&"a".repeat(64)));
        assert!(is_canonical_sha256(&"0".repeat(64)));
        assert!(!is_canonical_sha256(&"a".repeat(63)));
        assert!(!is_canonical_sha256(&"A".repeat(64)));
        assert!(!is_canonical_sha256(&"g".repeat(64)));
    }
}

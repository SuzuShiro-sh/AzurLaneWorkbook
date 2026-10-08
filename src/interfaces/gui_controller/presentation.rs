//! 把应用报告转换成界面展示数据。

use std::error::Error;
use std::path::Path;

use suzushiro_task_runtime::TaskFailure;

use super::GuiOperationOutput;
use crate::application::{
    AppError, DiagnosticArtifactRef, ExecutionHistoryReport, ExecutionResultsWriteReport,
    HistoryCatalogEntry, HistoryExecutionStatus, HistoryRecordKind, LogCatalogEntry, LogRecordKind,
    SessionCleanup, StageResult,
};
use crate::interfaces::gui_controller::{GuiDiagnosticItem, GuiInstanceItem};

/// 转换候选并把部分发现失败接入辅助信息的统一提示入口。
pub(crate) fn instance_catalog_snapshot(
    instances: crate::application::EmulatorInstanceCatalogReport,
    warnings: &mut Vec<TaskFailure>,
) -> (Vec<GuiInstanceItem>, Option<String>) {
    warnings.extend(
        instances
            .warnings()
            .iter()
            .map(|message| TaskFailure::new("部分模拟器信息尚未完全验证", message.clone())),
    );
    (
        instances
            .candidates()
            .iter()
            .map(|candidate| {
                GuiInstanceItem::explicit(
                    candidate.instance_id().to_owned(),
                    format!(
                        "{}: {}（{}，Android {}）",
                        candidate.instance_id(),
                        candidate.display_name(),
                        candidate.state().display_name(),
                        candidate.android_version()
                    ),
                    candidate.state(),
                )
            })
            .collect(),
        instances.selected_instance().map(str::to_owned),
    )
}

pub(crate) fn ordered_diagnostic_items(
    history: &[HistoryCatalogEntry],
    logs: &[LogCatalogEntry],
) -> Vec<GuiDiagnosticItem> {
    sort_diagnostic_items(
        history
            .iter()
            .map(history_diagnostic_item)
            .chain(logs.iter().map(log_diagnostic_item))
            .collect(),
    )
}

/// 新目录项和读取失败来源的旧目录项共用同一排序规则。
pub(super) fn sort_diagnostic_items(items: Vec<GuiDiagnosticItem>) -> Vec<GuiDiagnosticItem> {
    let capacity = items.len();
    let (mut numbered, mut timed): (Vec<_>, Vec<_>) =
        items.into_iter().partition(|item| item.sequence.is_some());
    numbered.sort_by(|left, right| {
        right.sequence.cmp(&left.sequence).then_with(|| {
            left.source()
                .relative_path()
                .cmp(right.source().relative_path())
        })
    });
    timed.sort_by(|left, right| {
        right
            .timestamp_unix_millis
            .cmp(&left.timestamp_unix_millis)
            .then_with(|| {
                left.source()
                    .relative_path()
                    .cmp(right.source().relative_path())
            })
    });
    // 编号固定日志创建先后；按记录时间插入历史和旧日志，不改变编号日志的相对顺序。
    let mut numbered = numbered.into_iter().peekable();
    let mut timed = timed.into_iter().peekable();
    let mut items = Vec::with_capacity(capacity);
    loop {
        match (numbered.peek(), timed.peek()) {
            (Some(log), Some(other))
                if log.timestamp_unix_millis >= other.timestamp_unix_millis =>
            {
                items.push(numbered.next().unwrap())
            }
            (Some(_), Some(_)) | (None, Some(_)) => items.push(timed.next().unwrap()),
            (Some(_), None) => items.push(numbered.next().unwrap()),
            (None, None) => break,
        }
    }
    items
}

fn history_diagnostic_item(entry: &HistoryCatalogEntry) -> GuiDiagnosticItem {
    let kind = match entry.kind() {
        HistoryRecordKind::Check => "计划检查",
        HistoryRecordKind::Execution => "计划执行",
    };
    let status = entry
        .execution_status()
        .map(history_status_name)
        .unwrap_or("已检查");
    GuiDiagnosticItem::new(
        format!(
            "历史 | {} | {} | {}",
            format_diagnostic_timestamp(entry.timestamp_unix_millis()),
            kind,
            status
        ),
        format!("历史 {kind}"),
        Some(status.to_owned()),
        Some(entry.workbook_name().to_owned()),
        DiagnosticArtifactRef::from_history(entry),
        entry.timestamp_unix_millis(),
        None,
    )
}

fn log_diagnostic_item(entry: &LogCatalogEntry) -> GuiDiagnosticItem {
    let kind = match entry.kind() {
        LogRecordKind::RuntimeProbe => "运行态探针",
        LogRecordKind::AdbServer => "ADB 服务",
        LogRecordKind::Operation => "操作日志",
    };
    GuiDiagnosticItem::new(
        format!(
            "日志 | {} | {} | {}",
            format_diagnostic_timestamp(entry.timestamp_unix_millis()),
            kind,
            diagnostic_file_name(entry.relative_path())
        ),
        format!("日志 {kind}"),
        None,
        None,
        DiagnosticArtifactRef::from_log(entry),
        entry.timestamp_unix_millis(),
        entry.sequence(),
    )
}

fn diagnostic_file_name(relative_path: &str) -> &str {
    Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(relative_path)
}

fn format_diagnostic_timestamp(millis: i64) -> String {
    #[cfg(windows)]
    if let Some(text) = windows_local_timestamp(millis) {
        return text;
    }
    format_utc_timestamp(millis)
}

fn format_utc_timestamp(millis: i64) -> String {
    let (year, month, day, hour, minute) = civil_from_unix_seconds(millis.div_euclid(1_000));
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

fn civil_from_unix_seconds(seconds: i64) -> (i32, u8, u8, u8, u8) {
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = (day_seconds / 3_600) as u8;
    let minute = ((day_seconds % 3_600) / 60) as u8;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute)
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u8, u8) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 };
    let era = era.div_euclid(146_097);
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year as i32, month as u8, day as u8)
}

#[cfg(windows)]
fn windows_local_timestamp(millis: i64) -> Option<String> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    if millis < 0 {
        return None;
    }
    let ticks = u64::try_from(millis)
        .ok()?
        .checked_mul(10_000)?
        .checked_add(116_444_736_000_000_000)?;
    let utc_filetime = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    if unsafe { FileTimeToSystemTime(&utc_filetime, &mut utc) } == 0 {
        return None;
    }
    let mut local = SYSTEMTIME::default();
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
    ))
}

fn history_status_name(status: HistoryExecutionStatus) -> &'static str {
    match status {
        HistoryExecutionStatus::Success => "成功",
        HistoryExecutionStatus::Failed => "失败",
        HistoryExecutionStatus::Unknown => "待确认",
        HistoryExecutionStatus::Cancelled => "已取消",
    }
}

/// 把一次初始化读到的目录变成界面结果。文案只在这里生成。
pub(crate) fn initialized_operation_output(
    startup: crate::application::WorkspaceStartupReport,
    preferred_instance: Option<&str>,
) -> (GuiOperationOutput, Option<String>) {
    let (workbook_names, instances, instance_error) = startup.into_parts();
    let mut warnings = Vec::new();
    let (items, selected) = instance_catalog_snapshot(instances, &mut warnings);
    if let Some(error) = instance_error {
        warnings.push(app_failure("模拟器 实例目录读取", error));
    }
    let snapshot =
        super::GuiSupportSnapshot::new(items, preferred_instance.map(str::to_owned).or(selected));
    let selected_for_agent = snapshot.selected_instance().map(str::to_owned);
    let summary = format!(
        "应用已就绪：{} 份工作簿，{} 个实例",
        workbook_names.len(),
        snapshot.instance_count(),
    );
    let output = if warnings.is_empty() {
        GuiOperationOutput::success(summary)
    } else {
        let detail = warnings
            .iter()
            .map(TaskFailure::detail)
            .collect::<Vec<_>>()
            .join("\n\n");
        GuiOperationOutput::success(format!(
            "{summary}；部分辅助信息读取失败，可用内容已保留，请查看错误详情"
        ))
        .with_diagnostic_detail(detail)
    };
    (
        output
            .with_workbooks(workbook_names)
            .with_support_snapshot(snapshot),
        selected_for_agent,
    )
}

pub(crate) fn workbook_name_from_relative_path(relative_path: &str) -> Result<String, TaskFailure> {
    Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            TaskFailure::new(
                "生成结果缺少有效工作簿名称",
                format!("生成报告返回的相对路径无效: {relative_path}"),
            )
        })
}

pub(crate) fn app_failure(operation: &str, error: AppError) -> TaskFailure {
    let user_message = error.message().to_owned();
    app_failure_with_message(operation, error, user_message)
}

/// 已发布工作簿但清理失败时返回可选中的核对终态。
pub(crate) fn generation_error_output(
    output_path: &str,
    cleanup: SessionCleanup,
) -> Result<GuiOperationOutput, TaskFailure> {
    let workbook_name = workbook_name_from_relative_path(output_path);
    let failure = generation_cleanup_failure(output_path, cleanup);
    match workbook_name {
        Ok(workbook_name) => Ok(GuiOperationOutput::from_execution(
            failure.user_message().to_owned(),
            crate::application::ExecutionReportStatus::Success,
            true,
        )
        .with_selected_workbook(workbook_name)
        .with_diagnostic_detail(failure.detail().to_owned())),
        Err(_) => Err(failure),
    }
}

pub(crate) fn check_failed_task(error: AppError, writeback: Result<(), AppError>) -> TaskFailure {
    let user_message = match &writeback {
        Ok(()) => "计划检查未通过，检查结果已写入工作簿".to_owned(),
        Err(writeback) => {
            format!(
                "计划检查未通过，检查结果也未能写回：{}",
                writeback.message()
            )
        }
    };
    let mut detail = String::new();
    append_app_error_block(&mut detail, "计划检查失败", &error);
    if let Err(writeback) = &writeback {
        append_app_error_block(&mut detail, "检查结果写回失败", writeback);
    }
    TaskFailure::new(user_message, detail)
}

/// 已读未检查或检查已通过但保存未完成时的界面说明和诊断详情。
pub(crate) fn present_check_tail(
    outcome: &crate::application::CheckAndSaveOutcome,
) -> (String, String) {
    use crate::application::CheckAndSaveOutcome;
    match outcome {
        CheckAndSaveOutcome::ReadWithoutCheck { cleanup } => {
            let summary = "已读取游戏状态，本次没有开始检查".to_owned();
            let mut detail = summary.clone();
            if let Some(error) = cleanup.error() {
                detail.push('\n');
                append_app_error_block(&mut detail, "会话清理未完成", error);
            }
            (summary, detail)
        }
        CheckAndSaveOutcome::SaveIncomplete {
            history, writeback, ..
        } => {
            let summary = match (history, writeback) {
                (Ok(history), Err(_)) => format!(
                    "计划检查已通过，记录已保存到 {}，但结果写回失败",
                    history.relative_path()
                ),
                (Err(_), Ok(())) => "计划检查已通过，结果已写回，但历史保存失败".to_owned(),
                (Err(_), Err(_)) => "计划检查已通过，但历史保存和结果写回都失败".to_owned(),
                (Ok(history), Ok(())) => {
                    format!("计划检查记录已保存：{}", history.relative_path())
                }
            };
            let mut detail = summary.clone();
            if let Err(error) = history {
                detail.push('\n');
                append_app_error_block(&mut detail, "历史保存失败", error);
            }
            if let Err(error) = writeback {
                detail.push('\n');
                append_app_error_block(&mut detail, "检查结果写回失败", error);
            }
            (summary, detail)
        }
        CheckAndSaveOutcome::Saved(_) | CheckAndSaveOutcome::CheckFailed { .. } => {
            (String::new(), String::new())
        }
    }
}

pub(crate) fn finished_execution_message(
    backup_path: &str,
    history: &StageResult<ExecutionHistoryReport>,
    writeback: &StageResult<ExecutionResultsWriteReport>,
    cleanup: &SessionCleanup,
) -> String {
    let evidence = match history {
        StageResult::Completed(history) => format!("执行历史：{}", history.relative_path()),
        StageResult::Failed(_) | StageResult::NotAttempted => {
            format!("备份：{backup_path}")
        }
    };
    let problem = writeback
        .failed()
        .or_else(|| history.failed())
        .or_else(|| cleanup.error());
    let problem = problem.map_or("收尾未完成", AppError::message);
    format!("{problem}；游戏执行已经结束，请依据{evidence}人工核对，禁止直接重试")
}

fn generation_cleanup_failure(output_path: &str, cleanup: SessionCleanup) -> TaskFailure {
    let (shutdown_detail, error) = match cleanup {
        SessionCleanup::Recovered(error) => ("游戏运行态正常卸载失败，恢复清理已完成", error),
        SessionCleanup::Failed(error) => ("游戏会话清理未完整确认", error),
        SessionCleanup::Completed => {
            return TaskFailure::new(
                format!(
                    "工作簿已经生成：{output_path}，但游戏会话清理未完整确认，请保留文件并核对运行态日志，禁止直接重试"
                ),
                "生成终态缺少会话清理错误".to_owned(),
            );
        }
    };
    let user_message = format!(
        "{}；工作簿已经生成：{output_path}，但{shutdown_detail}，请保留文件并核对运行态日志，禁止直接重试",
        error.message()
    );
    app_failure_with_message("同步并生成", error, user_message)
}

fn app_failure_with_message(operation: &str, error: AppError, user_message: String) -> TaskFailure {
    let mut detail = String::new();
    append_app_error_block(&mut detail, &format!("{operation}失败"), &error);
    TaskFailure::new(user_message, detail)
}

fn append_app_error_block(detail: &mut String, heading: &str, error: &AppError) {
    if !detail.is_empty() {
        detail.push('\n');
    }
    detail.push_str(&format!(
        "{heading} [{}]\n阶段：{}\n说明：{}",
        error.code(),
        error.stage(),
        error.message()
    ));
    if !error.context().is_empty() {
        detail.push_str("\n上下文：");
        for (key, value) in error.context() {
            detail.push_str(&format!("\n{key}={value}"));
        }
    }
    append_error_chain(detail, error);
}

pub(crate) fn append_error_chain(detail: &mut String, error: &(dyn Error + 'static)) {
    let mut source = error.source();
    while let Some(cause) = source {
        detail.push_str("\n原因：");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        diagnostic_file_name, format_diagnostic_timestamp, format_utc_timestamp,
        history_diagnostic_item, instance_catalog_snapshot, log_diagnostic_item,
        ordered_diagnostic_items,
    };

    #[test]
    fn partial_instance_discovery_keeps_selection_and_failure_details() {
        use crate::application::{
            EmulatorInstanceCandidate, EmulatorInstanceCatalogReport, EmulatorInstanceState,
        };
        let report = EmulatorInstanceCatalogReport::new(
            Some("ldplayer:0".into()),
            vec![
                EmulatorInstanceCandidate::new(
                    "ldplayer:0".into(),
                    "雷电测试实例".into(),
                    EmulatorInstanceState::Ready,
                    "9".into(),
                    true,
                )
                .unwrap(),
            ],
        )
        .with_warnings(vec!["MuMu 查询超时".into()]);
        let mut warnings = Vec::new();
        let (items, selected) = instance_catalog_snapshot(report, &mut warnings);
        assert_eq!(items.len(), 1);
        assert_eq!(selected.as_deref(), Some("ldplayer:0"));
        assert!(items[0].is_available());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].detail(), "MuMu 查询超时");
    }

    #[test]
    fn diagnostics_mix_history_and_logs_in_descending_time_order() {
        use crate::application::{HistoryCatalogEntry, LogCatalogEntry, LogRecordKind};
        let history = HistoryCatalogEntry::new_check(
            1,
            "plan.xlsx".into(),
            200,
            "data/history/middle.json".into(),
            1,
            "a".repeat(64),
            "b".repeat(64),
        );
        let logs = [
            LogCatalogEntry::new(
                LogRecordKind::RuntimeProbe,
                "data/logs/old.jsonl".into(),
                1,
                "c".repeat(64),
                100,
                None,
            ),
            LogCatalogEntry::new(
                LogRecordKind::AdbServer,
                "data/logs/new.log".into(),
                1,
                "d".repeat(64),
                300,
                None,
            ),
        ];
        let items = ordered_diagnostic_items(&[history], &logs);
        for (item, expected) in items.iter().zip(["new.log", "middle.json", "old.jsonl"]) {
            assert!(
                item.source().relative_path().contains(expected),
                "{}",
                item.source().relative_path()
            );
        }
        let ties = [
            LogCatalogEntry::new(
                LogRecordKind::AdbServer,
                "data/logs/z.log".into(),
                1,
                "e".repeat(64),
                300,
                None,
            ),
            LogCatalogEntry::new(
                LogRecordKind::AdbServer,
                "data/logs/a.log".into(),
                1,
                "f".repeat(64),
                300,
                None,
            ),
        ];
        let items = ordered_diagnostic_items(&[], &ties);
        assert!(items[0].source().relative_path().contains("a.log"));
    }

    #[test]
    fn diagnostic_log_numbers_descend_even_when_timestamps_disagree() {
        use crate::application::{HistoryCatalogEntry, LogCatalogEntry, LogRecordKind};
        let logs = [(2, 500), (10, 300), (1, 100), (11, 300)].map(|(number, timestamp)| {
            LogCatalogEntry::new(
                LogRecordKind::Operation,
                format!("data/logs/{number:03}-app.log"),
                1,
                "a".repeat(64),
                timestamp,
                Some(number),
            )
        });
        let history = HistoryCatalogEntry::new_check(
            1,
            "plan.xlsx".into(),
            250,
            "data/history/check.json".into(),
            1,
            "b".repeat(64),
            "c".repeat(64),
        );
        let items = ordered_diagnostic_items(&[history], &logs);
        for (item, expected) in items.iter().zip([
            "011-app.log",
            "010-app.log",
            "002-app.log",
            "check.json",
            "001-app.log",
        ]) {
            assert!(
                item.source().relative_path().contains(expected),
                "{}",
                item.source().relative_path()
            );
        }
        assert_eq!(items.len(), 5);
    }

    #[test]
    fn utc_timestamp_formats_known_unix_millis() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01 00:00");
        assert_eq!(format_utc_timestamp(1_700_000_000_123), "2023-11-14 22:13");
    }

    #[test]
    fn diagnostic_labels_use_readable_time_and_file_name() {
        use crate::application::{HistoryCatalogEntry, LogCatalogEntry, LogRecordKind};

        let log = LogCatalogEntry::new(
            LogRecordKind::Operation,
            "data/logs/001-app.log".into(),
            12,
            "d".repeat(64),
            1_700_000_000_123,
            Some(1),
        );
        let item = log_diagnostic_item(&log);
        let timestamp = format_diagnostic_timestamp(1_700_000_000_123);
        assert!(!item.label().contains("1700000000123"));
        assert!(item.label().contains("操作日志"));
        assert!(item.label().contains("001-app.log"));
        assert!(!item.label().contains("data/logs/"));
        assert!(item.label().contains(&timestamp));
        assert_eq!(item.source().relative_path(), "data/logs/001-app.log");
        assert_eq!(item.source().file_sha256(), "d".repeat(64));
        assert_eq!(item.kind_label(), "日志 操作日志");

        let history = HistoryCatalogEntry::new_check(
            1,
            "plan.xlsx".into(),
            1_700_000_000_123,
            "data/history/check.json".into(),
            1,
            "a".repeat(64),
            "b".repeat(64),
        );
        let item = history_diagnostic_item(&history);
        assert!(!item.label().contains("1700000000123"));
        assert!(item.label().contains("计划检查"));
        assert!(item.label().contains(&timestamp));
        assert_eq!(item.source().relative_path(), "data/history/check.json");
        assert_eq!(item.workbook_name(), Some("plan.xlsx"));
        assert_eq!(item.status_label(), Some("已检查"));
    }

    #[test]
    fn diagnostic_file_name_uses_basename() {
        assert_eq!(diagnostic_file_name("data/logs/001-app.log"), "001-app.log");
        assert_eq!(diagnostic_file_name("plain.log"), "plain.log");
    }

    #[test]
    fn completed_execution_failure_keeps_a_visible_evidence_path() {
        use crate::application::{AppError, AppErrorCode, SessionCleanup, StageResult};
        let history = StageResult::Failed(AppError::from_source(
            "execution.history.save",
            AppErrorCode::HistoryWriteFailed,
            "执行历史保存失败",
            std::io::Error::other("fixture history failure"),
        ));
        let message = super::finished_execution_message(
            "data/backups/plan.xlsx",
            &history,
            &StageResult::NotAttempted,
            &SessionCleanup::Completed,
        );

        assert!(message.contains("游戏执行已经结束"));
        assert!(message.contains("禁止直接重试"));
        assert!(message.contains("备份：data/backups/plan.xlsx"));
        assert!(message.contains("执行历史保存失败"));
    }

    #[test]
    fn completed_generation_failure_keeps_the_published_workbook_visible() {
        use crate::application::{AppError, AppErrorCode, SessionCleanup};
        let error = AppError::from_source(
            "game.cleanup",
            AppErrorCode::RuntimeBootstrapFailed,
            "游戏会话清理失败",
            std::io::Error::other("fixture cleanup failure"),
        )
        .with_context("output_path", "data/workbooks/generated.xlsx")
        .with_context("output_package_sha256", "a".repeat(64));

        let failure = super::generation_cleanup_failure(
            "data/workbooks/generated.xlsx",
            SessionCleanup::Failed(error),
        );

        assert!(failure.user_message().contains("工作簿已经生成"));
        assert!(
            failure
                .user_message()
                .contains("data/workbooks/generated.xlsx")
        );
        assert!(failure.user_message().contains("禁止直接重试"));
        assert!(
            failure
                .detail()
                .contains("output_path=data/workbooks/generated.xlsx")
        );
    }

    #[test]
    fn completed_generation_error_returns_an_attention_output() {
        use crate::application::{AppError, AppErrorCode, SessionCleanup};
        let error = AppError::from_source(
            "game.cleanup",
            AppErrorCode::RuntimeBootstrapFailed,
            "游戏会话清理失败",
            std::io::Error::other("fixture cleanup failure"),
        )
        .with_context("output_path", "data/workbooks/generated.xlsx")
        .with_context("output_package_sha256", "a".repeat(64));

        assert!(
            super::generation_error_output(
                "data/workbooks/generated.xlsx",
                SessionCleanup::Failed(error),
            )
            .is_ok()
        );
    }

    #[test]
    fn recovered_shutdown_keeps_generation_attention_and_recovery_detail() {
        use crate::application::{AppError, AppErrorCode, SessionCleanup};
        let error = AppError::from_source(
            "game.cleanup",
            AppErrorCode::RuntimeBootstrapFailed,
            "正常卸载失败",
            std::io::Error::other("固定卸载失败"),
        )
        .with_context("output_path", "data/workbooks/fixture.xlsx");
        let output = super::generation_error_output(
            "data/workbooks/fixture.xlsx",
            SessionCleanup::Recovered(error),
        )
        .unwrap();
        assert!(output.is_attention());
        assert!(output.summary().contains("恢复清理已完成"));
        assert!(!output.summary().contains("清理未完整确认"));
    }
}

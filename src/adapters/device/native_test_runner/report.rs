//! 负责发布 Native 测试输出和结构化运行报告。

#[cfg(target_os = "windows")]
use std::fs;
#[cfg(target_os = "windows")]
use std::io::Write;
#[cfg(target_os = "windows")]
use std::path::Path;
#[cfg(target_os = "windows")]
use std::time::Duration;

#[cfg(target_os = "windows")]
use suzushiro_session_core::SessionId;

use super::MAXIMUM_DIAGNOSTIC_BYTES;
#[cfg(target_os = "windows")]
use super::MAXIMUM_REPORT_BYTES;
#[cfg(target_os = "windows")]
use super::contracts::{
    NativeTestOutputEvidence, NativeTestRunOutcome, NativeTestRunReport, NativeTestRunnerError,
};
#[cfg(target_os = "windows")]
use crate::adapters::json_artifact::{PublishedJson, write_new_pretty_json};
#[cfg(target_os = "windows")]
use crate::adapters::tool_root::ToolRoot;
#[cfg(target_os = "windows")]
use suzushiro_content_digest::sha256_bytes;

#[cfg(target_os = "windows")]
pub(super) fn publish_test_output(
    tool_root: &ToolRoot,
    session_id: SessionId,
    index: usize,
    test_name: &str,
    stream_name: &str,
    text: &str,
) -> Result<NativeTestOutputEvidence, NativeTestRunnerError> {
    let directory_relative = Path::new("data/logs").join(format!("native-tests-{session_id}"));
    tool_root
        .ensure_directory(&directory_relative)
        .map_err(|error| NativeTestRunnerError::Report {
            message: format!("建立 Native 测试输出目录失败: {error}"),
        })?;
    let filename = format!("{index:02}-{test_name}.{stream_name}.txt");
    let relative = directory_relative.join(filename);
    let path =
        tool_root
            .prepare_new_file(&relative)
            .map_err(|error| NativeTestRunnerError::Report {
                message: format!("准备 Native 测试输出文件失败: {error}"),
            })?;
    let bytes = text.as_bytes();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| NativeTestRunnerError::Io {
            path: path.clone(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| NativeTestRunnerError::Io {
            path: path.clone(),
            source,
        })?;
    Ok(NativeTestOutputEvidence {
        path: relative,
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: sha256_bytes(bytes),
    })
}

#[cfg(target_os = "windows")]
pub(super) fn publish_report(
    tool_root: &ToolRoot,
    report: &NativeTestRunReport,
) -> Result<NativeTestRunOutcome, NativeTestRunnerError> {
    let report_name = format!("native-tests-{}.json", report.session_id);
    let target_relative = Path::new("data/logs").join(&report_name);
    let temporary_relative = Path::new("data/logs").join(format!(".{report_name}.tmp"));
    let published: PublishedJson = write_new_pretty_json(
        tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_REPORT_BYTES,
        report,
    )
    .map_err(|error| NativeTestRunnerError::Report {
        message: error.to_string(),
    })?;
    Ok(NativeTestRunOutcome {
        report: report.clone(),
        report_path: published.path().to_path_buf(),
        report_sha256: published.sha256().to_owned(),
        report_size_bytes: published.size_bytes(),
    })
}

#[cfg(target_os = "windows")]
pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn bounded_diagnostic(value: &str) -> String {
    if value.len() <= MAXIMUM_DIAGNOSTIC_BYTES {
        return value.to_owned();
    }
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAXIMUM_DIAGNOSTIC_BYTES)
        .last()
        .unwrap_or_default();
    format!("{}...[diagnostic truncated]", &value[..boundary])
}

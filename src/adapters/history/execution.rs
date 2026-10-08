//! 将每次配装执行结果保存为可追溯、不可覆盖的 JSON 历史记录。

use std::path::Path;

use serde::Serialize;
use thiserror::Error;

use super::super::json_artifact::{JsonArtifactError, PublishedJson, write_new_pretty_json};
use super::super::tool_root::ToolRoot;
use super::{
    HISTORY_DIRECTORY, HistoryMetadataError, current_unix_millis, relative_path_string,
    validate_workbook_name,
};
use crate::adapters::numbered_files::NumberedDirectory;
use crate::application::{
    AppError, AppErrorCode, ExecutionHistoryPort, ExecutionHistoryReport, ExecutionReport,
    ExecutionReportStatus, WorkbookRef,
};

/// 执行历史 JSON 外壳的稳定版本。
pub const EXECUTION_HISTORY_SCHEMA_VERSION: u32 = 1;

const MAXIMUM_EXECUTION_HISTORY_BYTES: u64 = 64 * 1024 * 1024;

/// 将受控工作簿和完整执行报告发布到工具历史目录。
pub(crate) struct JsonExecutionHistoryPort {
    tool_root: ToolRoot,
}

impl JsonExecutionHistoryPort {
    /// 固定受控工具根目录，保存时不接受外部历史路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl ExecutionHistoryPort for JsonExecutionHistoryPort {
    fn save_execution_history(
        &self,
        workbook: &WorkbookRef,
        report: &ExecutionReport,
    ) -> Result<ExecutionHistoryReport, AppError> {
        let workbook_name = workbook
            .relative_path()
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                AppError::from_source(
                    "execution.history.save",
                    AppErrorCode::WorkbookInvalid,
                    "执行历史对应的工作簿名称无效",
                    std::io::Error::other("workbook reference has no UTF-8 file name"),
                )
                .with_context("path", workbook.relative_path().to_string_lossy())
            })?;
        save_execution_history(&self.tool_root, workbook_name, report)
    }
}

/// 执行历史文件的稳定 JSON 外壳；完整逐步证据保留在 `report` 内。
#[derive(Serialize)]
struct ExecutionHistoryDocument<'a> {
    schema_version: u32,
    execution_schema_version: u32,
    plan_schema_version: u32,
    workbook_name: &'a str,
    recorded_at_unix_millis: i64,
    target_fingerprint_sha256: &'a str,
    plan_content_sha256: &'a str,
    report_content_sha256: &'a str,
    report_status: ExecutionReportStatus,
    may_have_writes: bool,
    report: &'a ExecutionReport,
}

/// 执行历史元数据、摘要或原子 JSON 发布失败。
#[derive(Debug, Error)]
enum ExecutionHistoryWriteError {
    #[error("执行历史元数据无效: {0}")]
    Metadata(#[source] HistoryMetadataError),
    #[error("执行历史 JSON 发布失败: {source}")]
    Artifact {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// 将一份完整执行结果以不可覆盖 JSON 文件发布到历史目录。
pub fn save_execution_history(
    tool_root: &ToolRoot,
    workbook_name: &str,
    report: &ExecutionReport,
) -> Result<ExecutionHistoryReport, AppError> {
    let recorded_at_unix_millis = current_unix_millis().map_err(|source| {
        map_execution_history_error(
            tool_root,
            Path::new(HISTORY_DIRECTORY),
            ExecutionHistoryWriteError::Metadata(source),
        )
    })?;
    save_execution_history_at(tool_root, workbook_name, report, recorded_at_unix_millis)
}

fn save_execution_history_at(
    tool_root: &ToolRoot,
    workbook_name: &str,
    report: &ExecutionReport,
    recorded_at_unix_millis: i64,
) -> Result<ExecutionHistoryReport, AppError> {
    validate_workbook_name(workbook_name).map_err(|source| {
        map_execution_history_error(
            tool_root,
            Path::new(HISTORY_DIRECTORY),
            ExecutionHistoryWriteError::Metadata(source),
        )
    })?;
    let map_sequence_error = |source| {
        map_execution_history_error(
            tool_root,
            Path::new(HISTORY_DIRECTORY),
            ExecutionHistoryWriteError::Artifact {
                source: Box::new(source),
            },
        )
    };
    let directory = NumberedDirectory::open(tool_root, Path::new(HISTORY_DIRECTORY))
        .map_err(map_sequence_error)?;
    let target_relative = directory
        .next_path("exec", "json", true)
        .map_err(map_sequence_error)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let document = ExecutionHistoryDocument {
        schema_version: EXECUTION_HISTORY_SCHEMA_VERSION,
        execution_schema_version: report.schema_version(),
        plan_schema_version: report.plan_schema_version(),
        workbook_name,
        recorded_at_unix_millis,
        target_fingerprint_sha256: report.target_identity().fingerprint_sha256(),
        plan_content_sha256: report.plan_hash(),
        report_content_sha256: report.content_sha256(),
        report_status: report.status(),
        may_have_writes: report.may_have_writes(),
        report,
    };
    let published: PublishedJson = write_new_pretty_json(
        tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_EXECUTION_HISTORY_BYTES,
        &document,
    )
    .map_err(|source: JsonArtifactError| {
        map_execution_history_error(
            tool_root,
            &target_relative,
            ExecutionHistoryWriteError::Artifact {
                source: Box::new(source),
            },
        )
    })?;

    Ok(ExecutionHistoryReport::new(
        EXECUTION_HISTORY_SCHEMA_VERSION,
        report.schema_version(),
        report.plan_schema_version(),
        workbook_name.to_owned(),
        recorded_at_unix_millis,
        relative_path_string(&target_relative),
        published.size_bytes(),
        published.sha256().to_owned(),
        report.target_identity().fingerprint_sha256().to_owned(),
        report.plan_hash().to_owned(),
        report.content_sha256().to_owned(),
        report.status(),
        report.may_have_writes(),
    ))
}

fn map_execution_history_error(
    tool_root: &ToolRoot,
    relative_path: &Path,
    source: ExecutionHistoryWriteError,
) -> AppError {
    AppError::from_source(
        "execution.history.save",
        AppErrorCode::HistoryWriteFailed,
        "配装执行记录未能安全发布",
        source,
    )
    .with_context(
        "output_path",
        tool_root.as_path().join(relative_path).to_string_lossy(),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::Value;

    use super::{
        EXECUTION_HISTORY_SCHEMA_VERSION, JsonExecutionHistoryPort, save_execution_history_at,
    };
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::test_support::empty_execution_report;
    use crate::application::{ExecutionHistoryPort, ExecutionReportStatus};
    use suzushiro_content_digest::sha256_bytes;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn publishes_a_versioned_report_without_overwriting_the_workbook() {
        let fixture = TestDirectory::new("publish");
        fs::create_dir_all(fixture.root.join("data/workbooks")).unwrap();
        fs::write(fixture.root.join("data/workbooks/plan.xlsx"), b"workbook").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let report = empty_execution_report();

        let evidence =
            save_execution_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();

        assert_eq!(evidence.message(), "配装执行记录已保存");
        assert_eq!(evidence.schema_version(), EXECUTION_HISTORY_SCHEMA_VERSION);
        assert_eq!(evidence.execution_schema_version(), report.schema_version());
        assert_eq!(evidence.plan_schema_version(), report.plan_schema_version());
        assert_eq!(evidence.workbook_name(), "plan.xlsx");
        assert_eq!(evidence.recorded_at_unix_millis(), 1_700_000_000_123);
        assert_eq!(
            evidence.target_fingerprint_sha256(),
            report.target_identity().fingerprint_sha256()
        );
        assert_eq!(evidence.plan_content_sha256(), report.plan_hash());
        assert_eq!(evidence.report_content_sha256(), report.content_sha256());
        assert_eq!(evidence.report_status(), ExecutionReportStatus::Success);
        assert!(!evidence.may_have_writes());
        assert_eq!(evidence.relative_path(), "data/history/001-exec.json");

        let history_path = fixture.root.join(evidence.relative_path());
        let history_bytes = fs::read(&history_path).unwrap();
        let document: Value = serde_json::from_slice(&history_bytes).unwrap();
        assert_eq!(document["schema_version"], EXECUTION_HISTORY_SCHEMA_VERSION);
        assert_eq!(
            document["execution_schema_version"],
            report.schema_version()
        );
        assert_eq!(
            document["plan_schema_version"],
            report.plan_schema_version()
        );
        assert_eq!(document["workbook_name"], "plan.xlsx");
        assert_eq!(document["recorded_at_unix_millis"], 1_700_000_000_123_i64);
        assert_eq!(
            document["target_fingerprint_sha256"],
            report.target_identity().fingerprint_sha256()
        );
        assert_eq!(document["plan_content_sha256"], report.plan_hash());
        assert_eq!(document["report_content_sha256"], report.content_sha256());
        assert_eq!(document["report_status"], "success");
        assert_eq!(document["may_have_writes"], false);
        assert_eq!(document["report"]["status"], "success");
        assert_eq!(
            document["report"]["content_sha256"],
            report.content_sha256()
        );
        assert_eq!(history_bytes.len() as u64, evidence.size_bytes());
        assert_eq!(evidence.file_sha256(), sha256_bytes(&history_bytes));
        assert_eq!(
            fs::read(fixture.root.join("data/workbooks/plan.xlsx")).unwrap(),
            b"workbook"
        );
        assert!(
            fs::read_dir(fixture.root.join("data/history"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
        );
    }

    #[test]
    fn accepts_only_the_filename_from_a_controlled_workbook_reference() {
        let fixture = TestDirectory::new("port");
        fs::create_dir_all(fixture.root.join("data/workbooks")).unwrap();
        fs::write(fixture.root.join("data/workbooks/plan.xlsx"), b"workbook").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let port = JsonExecutionHistoryPort::new(root);
        let report = empty_execution_report();

        let evidence = port.save_execution_history(&workbook, &report).unwrap();

        assert_eq!(evidence.workbook_name(), "plan.xlsx");
        assert!(evidence.relative_path().starts_with("data/history/"));
        assert_eq!(evidence.report_content_sha256(), report.content_sha256());
    }

    #[test]
    fn appends_repeated_reports_without_replacing_existing_history() {
        let fixture = TestDirectory::new("conflict");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let report = empty_execution_report();
        let first =
            save_execution_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();
        let first_bytes = fs::read(fixture.root.join(first.relative_path())).unwrap();

        let second =
            save_execution_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();
        assert_eq!(first.relative_path(), "data/history/001-exec.json");
        assert_eq!(second.relative_path(), "data/history/002-exec.json");
        assert_eq!(
            fs::read(fixture.root.join(first.relative_path())).unwrap(),
            first_bytes
        );
        let history_entries: Vec<_> = fs::read_dir(fixture.root.join("data/history"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(history_entries.len(), 2);
    }

    #[test]
    fn rejects_a_path_like_workbook_name_before_publishing() {
        let fixture = TestDirectory::new("invalid-name");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let report = empty_execution_report();

        let error = save_execution_history_at(&root, "../plan.xlsx", &report, 1_700_000_000_123)
            .unwrap_err();

        assert_eq!(error.stage(), "execution.history.save");
        assert_eq!(error.code().as_str(), "HISTORY_WRITE_FAILED");
        let expected_output_path = root
            .as_path()
            .join("data/history")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            error.context().get("output_path").map(String::as_str),
            Some(expected_output_path.as_str())
        );
        assert!(!fixture.root.join("data/history").exists());
    }

    struct TestDirectory {
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let root = home
                .join("suzushiro/scratch/azlw-execution-history-tests")
                .join(format!(
                    "{label}-{}",
                    NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

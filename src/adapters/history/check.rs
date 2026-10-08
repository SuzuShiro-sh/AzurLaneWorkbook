//! 将成功的配装检查保存为可追溯、不可覆盖的 JSON 历史记录。

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
    AppError, AppErrorCode, CHECK_HISTORY_SCHEMA_VERSION, CheckHistoryPort, CheckHistoryReport,
    CheckReport, WorkbookRef,
};

const MAXIMUM_CHECK_HISTORY_BYTES: u64 = 4 * 1024 * 1024;

/// 将配装检查结果发布到受控历史目录。
pub(crate) struct JsonCheckHistoryPort {
    tool_root: ToolRoot,
}

impl JsonCheckHistoryPort {
    /// 固定受控工具根目录，保存时不接受外部历史路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl CheckHistoryPort for JsonCheckHistoryPort {
    fn save_check_history(
        &self,
        workbook: &WorkbookRef,
        report: &CheckReport,
    ) -> Result<CheckHistoryReport, AppError> {
        let workbook_name = workbook
            .relative_path()
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid_workbook_reference(workbook))?;
        save_check_history(&self.tool_root, workbook_name, report)
    }
}

/// 检查历史文件的稳定 JSON 外壳；完整计划仍保留在 `report` 内。
#[derive(Serialize)]
struct CheckHistoryDocument<'a> {
    schema_version: u32,
    plan_schema_version: u32,
    workbook_name: &'a str,
    checked_at_unix_millis: i64,
    plan_content_sha256: &'a str,
    report: &'a CheckReport,
}

/// 检查历史路径、时间或原子 JSON 发布失败。
#[derive(Debug, Error)]
enum CheckHistoryWriteError {
    #[error("检查历史元数据无效: {0}")]
    Metadata(#[source] HistoryMetadataError),
    #[error("检查历史 JSON 发布失败: {source}")]
    Artifact {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// 将一份已经通过实时检查的计划以不可覆盖 JSON 文件发布到历史目录。
fn save_check_history(
    tool_root: &ToolRoot,
    workbook_name: &str,
    report: &CheckReport,
) -> Result<CheckHistoryReport, AppError> {
    let checked_at_unix_millis: i64 = current_unix_millis().map_err(|source| {
        map_check_history_error(
            tool_root,
            Path::new(HISTORY_DIRECTORY),
            CheckHistoryWriteError::Metadata(source),
        )
    })?;
    save_check_history_at(tool_root, workbook_name, report, checked_at_unix_millis)
}

fn save_check_history_at(
    tool_root: &ToolRoot,
    workbook_name: &str,
    report: &CheckReport,
    checked_at_unix_millis: i64,
) -> Result<CheckHistoryReport, AppError> {
    validate_workbook_name(workbook_name).map_err(|source| {
        map_check_history_error(
            tool_root,
            Path::new(HISTORY_DIRECTORY),
            CheckHistoryWriteError::Metadata(source),
        )
    })?;
    let map_sequence_error = |source| {
        map_check_history_error(
            tool_root,
            Path::new(HISTORY_DIRECTORY),
            CheckHistoryWriteError::Artifact {
                source: Box::new(source),
            },
        )
    };
    let directory = NumberedDirectory::open(tool_root, Path::new(HISTORY_DIRECTORY))
        .map_err(map_sequence_error)?;
    let target_relative = directory
        .next_path("check", "json", true)
        .map_err(map_sequence_error)?;
    let temporary_relative = target_relative.with_extension("tmp");
    let document = CheckHistoryDocument {
        schema_version: CHECK_HISTORY_SCHEMA_VERSION,
        plan_schema_version: report.plan().schema_version(),
        workbook_name,
        checked_at_unix_millis,
        plan_content_sha256: report.plan().content_sha256(),
        report,
    };
    let published: PublishedJson = write_new_pretty_json(
        tool_root,
        &temporary_relative,
        &target_relative,
        MAXIMUM_CHECK_HISTORY_BYTES,
        &document,
    )
    .map_err(|source: JsonArtifactError| {
        map_check_history_error(
            tool_root,
            &target_relative,
            CheckHistoryWriteError::Artifact {
                source: Box::new(source),
            },
        )
    })?;

    Ok(CheckHistoryReport::new(
        report.plan().schema_version(),
        workbook_name.to_owned(),
        checked_at_unix_millis,
        relative_path_string(&target_relative),
        published.size_bytes(),
        published.sha256().to_owned(),
        report.plan().content_sha256().to_owned(),
    ))
}

fn invalid_workbook_reference(workbook: &WorkbookRef) -> AppError {
    AppError::from_source(
        "check.history.save",
        AppErrorCode::WorkbookInvalid,
        "检查历史对应的工作簿名称无效",
        std::io::Error::other("workbook reference has no UTF-8 file name"),
    )
    .with_context("path", workbook.relative_path().to_string_lossy())
}

fn map_check_history_error(
    tool_root: &ToolRoot,
    relative_path: &Path,
    source: CheckHistoryWriteError,
) -> AppError {
    AppError::from_source(
        "check.history.save",
        AppErrorCode::HistoryWriteFailed,
        "检查历史记录未能安全发布",
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

    use super::{CHECK_HISTORY_SCHEMA_VERSION, save_check_history_at};
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::compile_plan;
    use crate::application::test_support::empty_game_state;
    use crate::domain::DesiredState;
    use suzushiro_content_digest::sha256_bytes;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn publishes_a_versioned_report_without_overwriting_the_workbook() {
        let fixture: TestDirectory = TestDirectory::new("publish");
        fs::create_dir_all(fixture.root.join("data/workbooks")).unwrap();
        fs::write(fixture.root.join("data/workbooks/plan.xlsx"), b"workbook").unwrap();
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let report =
            compile_plan(&empty_game_state(), &DesiredState::new(Vec::new()).unwrap()).unwrap();

        let evidence =
            save_check_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();

        assert_eq!(evidence.message(), "配装计划检查记录已保存");
        assert_eq!(evidence.schema_version(), CHECK_HISTORY_SCHEMA_VERSION);
        assert_eq!(
            evidence.plan_schema_version(),
            report.plan().schema_version()
        );
        assert_eq!(evidence.workbook_name(), "plan.xlsx");
        assert_eq!(evidence.checked_at_unix_millis(), 1_700_000_000_123);
        assert_eq!(
            evidence.plan_content_sha256(),
            report.plan().content_sha256()
        );
        let history_path = fixture.root.join(evidence.relative_path());
        let document: Value = serde_json::from_slice(&fs::read(&history_path).unwrap()).unwrap();
        assert_eq!(document["schema_version"], CHECK_HISTORY_SCHEMA_VERSION);
        assert_eq!(
            document["plan_schema_version"],
            report.plan().schema_version()
        );
        assert_eq!(document["workbook_name"], "plan.xlsx");
        assert_eq!(document["checked_at_unix_millis"], 1_700_000_000_123_i64);
        assert_eq!(
            document["plan_content_sha256"],
            report.plan().content_sha256()
        );
        assert_eq!(document["report"]["message"], "配装计划检查通过");
        assert_eq!(
            fs::metadata(&history_path).unwrap().len(),
            evidence.size_bytes()
        );
        assert_eq!(
            evidence.file_sha256(),
            sha256_bytes(&fs::read(&history_path).unwrap())
        );
        assert_eq!(
            fs::read(fixture.root.join("data/workbooks/plan.xlsx")).unwrap(),
            b"workbook"
        );
    }

    #[test]
    fn appends_repeated_reports_without_replacing_existing_history() {
        let fixture: TestDirectory = TestDirectory::new("conflict");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let report =
            compile_plan(&empty_game_state(), &DesiredState::new(Vec::new()).unwrap()).unwrap();
        let first = save_check_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();
        let first_bytes = fs::read(fixture.root.join(first.relative_path())).unwrap();
        let second = save_check_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();
        assert_eq!(first.relative_path(), "data/history/001-check.json");
        assert_eq!(second.relative_path(), "data/history/002-check.json");
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
    fn shares_numbering_with_other_history_types() {
        let fixture = TestDirectory::new("shared-sequence");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let directory = fixture.root.join("data/history");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("001-probe.json"), b"probe").unwrap();
        fs::write(directory.join("002-equipment.json"), b"equipment").unwrap();
        let report =
            compile_plan(&empty_game_state(), &DesiredState::new(Vec::new()).unwrap()).unwrap();
        let check = save_check_history_at(&root, "plan.xlsx", &report, 1_700_000_000_123).unwrap();
        let execution = crate::adapters::history::execution::save_execution_history(
            &root,
            "plan.xlsx",
            &crate::application::test_support::empty_execution_report(),
        )
        .unwrap();
        assert_eq!(check.relative_path(), "data/history/003-check.json");
        assert_eq!(execution.relative_path(), "data/history/004-exec.json");
        assert_eq!(
            fs::read(directory.join("001-probe.json")).unwrap(),
            b"probe"
        );
        assert_eq!(
            fs::read(directory.join("002-equipment.json")).unwrap(),
            b"equipment"
        );
    }

    #[test]
    fn rejects_a_path_like_workbook_name_before_publishing() {
        let fixture: TestDirectory = TestDirectory::new("invalid-name");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let report =
            compile_plan(&empty_game_state(), &DesiredState::new(Vec::new()).unwrap()).unwrap();

        let error =
            save_check_history_at(&root, "../plan.xlsx", &report, 1_700_000_000_123).unwrap_err();

        assert_eq!(error.code().as_str(), "HISTORY_WRITE_FAILED");
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
                .join("suzushiro/scratch/azlw-check-history-tests")
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

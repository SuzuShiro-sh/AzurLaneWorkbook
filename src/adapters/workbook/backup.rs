//! 在执行游戏写命令前不可覆盖地备份并逐字节核验原数据工作簿。

use std::path::Path;

use thiserror::Error;

use crate::adapters::history::{current_unix_millis, relative_path_string};
use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::application::{
    AppError, AppErrorCode, WorkbookBackupPort, WorkbookBackupReport, WorkbookRef,
};
use suzushiro_content_digest::sha256_bytes;

use super::WorkbookProbeError;
use super::editor::{reject_external_content, reject_macro_content};
use super::package::{
    MAX_RAW_PACKAGE_BYTES, PackageSnapshot, read_bounded_workbook_bytes, write_new_file_bytes,
};

const BACKUP_DIRECTORY: &str = "data/backups";

/// 使用受控工具根目录保存执行前工作簿原始字节。
pub(crate) struct XlsxWorkbookBackupPort {
    tool_root: ToolRoot,
}

impl XlsxWorkbookBackupPort {
    /// 固定工具根目录，调用方不能改变备份输出位置。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl WorkbookBackupPort for XlsxWorkbookBackupPort {
    fn backup_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookBackupReport, AppError> {
        let backed_up_at_unix_millis = current_unix_millis().map_err(|source| {
            map_backup_error(
                &self.tool_root,
                workbook.relative_path(),
                Path::new(BACKUP_DIRECTORY),
                WorkbookBackupError::Clock(source),
            )
        })?;
        backup_workbook_at(&self.tool_root, workbook, backed_up_at_unix_millis)
    }
}

/// 备份路径、源包读取、不可覆盖发布或重读核验没有完整完成。
#[derive(Debug, Error)]
enum WorkbookBackupError {
    #[error("读取或验证源工作簿失败: {0}")]
    SourceWorkbook(#[source] WorkbookProbeError),
    #[error("写入或验证备份工作簿失败: {0}")]
    BackupWorkbook(#[source] WorkbookProbeError),
    #[error("备份路径操作失败: {0}")]
    ToolRoot(#[source] ToolRootError),
    #[error("系统时间不可用: {0}")]
    Clock(#[source] crate::adapters::history::HistoryMetadataError),
    #[error("备份重读内容与源工作簿不一致: expected={expected}, actual={actual}")]
    ContentMismatch { expected: String, actual: String },
    #[error("工作簿备份失败: {operation}; 清理未完成文件同时失败: {cleanup}")]
    OperationAndCleanup {
        operation: Box<WorkbookBackupError>,
        #[source]
        cleanup: ToolRootError,
    },
}

fn backup_workbook_at(
    tool_root: &ToolRoot,
    workbook: &WorkbookRef,
    backed_up_at_unix_millis: i64,
) -> Result<WorkbookBackupReport, AppError> {
    let source_relative = workbook.relative_path();
    let source_path = tool_root.existing_file(source_relative).map_err(|source| {
        map_backup_error(
            tool_root,
            source_relative,
            Path::new(BACKUP_DIRECTORY),
            WorkbookBackupError::ToolRoot(source),
        )
    })?;
    let source_bytes =
        read_bounded_workbook_bytes(&source_path, MAX_RAW_PACKAGE_BYTES, "待备份数据工作簿")
            .map_err(|source| {
                map_backup_error(
                    tool_root,
                    source_relative,
                    Path::new(BACKUP_DIRECTORY),
                    WorkbookBackupError::SourceWorkbook(source.into()),
                )
            })?;
    validate_package_bytes(&source_bytes, &source_path).map_err(|source| {
        map_backup_error(
            tool_root,
            source_relative,
            Path::new(BACKUP_DIRECTORY),
            WorkbookBackupError::SourceWorkbook(source),
        )
    })?;

    let source_sha256 = sha256_bytes(&source_bytes);
    let numbered = crate::adapters::numbered_files::NumberedDirectory::open(
        tool_root,
        Path::new(BACKUP_DIRECTORY),
    )
    .map_err(|source| {
        map_backup_error(
            tool_root,
            source_relative,
            Path::new(BACKUP_DIRECTORY),
            WorkbookBackupError::BackupWorkbook(WorkbookProbeError::Io {
                stage: "分配备份编号",
                path: tool_root.as_path().join(BACKUP_DIRECTORY),
                source,
            }),
        )
    })?;
    let target_relative = numbered
        .next_path("backup", "xlsx", false)
        .map_err(|source| {
            map_backup_error(
                tool_root,
                source_relative,
                Path::new(BACKUP_DIRECTORY),
                WorkbookBackupError::BackupWorkbook(WorkbookProbeError::Io {
                    stage: "分配备份编号",
                    path: tool_root.as_path().join(BACKUP_DIRECTORY),
                    source,
                }),
            )
        })?;
    let temporary_relative = target_relative.with_extension("tmp.xlsx");
    let operation = publish_backup(
        tool_root,
        &temporary_relative,
        &target_relative,
        &source_bytes,
        &source_sha256,
    );
    let backup_sha256 = operation
        .map_err(|source| map_backup_error(tool_root, source_relative, &target_relative, source))?;
    let size_bytes = u64::try_from(source_bytes.len()).unwrap_or(u64::MAX);

    Ok(WorkbookBackupReport::new(
        relative_path_string(source_relative),
        relative_path_string(&target_relative),
        backed_up_at_unix_millis,
        size_bytes,
        source_sha256,
        backup_sha256,
    ))
}

fn publish_backup(
    tool_root: &ToolRoot,
    temporary_relative: &Path,
    target_relative: &Path,
    source_bytes: &[u8],
    source_sha256: &str,
) -> Result<String, WorkbookBackupError> {
    tool_root
        .ensure_directory(Path::new(BACKUP_DIRECTORY))
        .map_err(WorkbookBackupError::ToolRoot)?;
    tool_root
        .prepare_new_file(target_relative)
        .map_err(WorkbookBackupError::ToolRoot)?;
    let temporary_path = tool_root
        .prepare_new_file(temporary_relative)
        .map_err(WorkbookBackupError::ToolRoot)?;
    let temporary_file = write_new_file_bytes(&temporary_path, source_bytes)
        .map_err(|source| WorkbookBackupError::BackupWorkbook(source.into()))?;
    if let Err(source) = verify_backup_bytes(&temporary_path, source_bytes, source_sha256) {
        return Err(cleanup_after_error(
            tool_root,
            &temporary_file,
            temporary_relative,
            source,
        ));
    }
    if let Err(source) =
        tool_root.rename_new_file(&temporary_file, temporary_relative, target_relative)
    {
        return Err(cleanup_after_error(
            tool_root,
            &temporary_file,
            temporary_relative,
            WorkbookBackupError::ToolRoot(source),
        ));
    }

    let final_verification = tool_root
        .existing_file(target_relative)
        .map_err(WorkbookBackupError::ToolRoot)
        .and_then(|target_path| verify_backup_bytes(&target_path, source_bytes, source_sha256));
    match final_verification {
        Ok(sha256) => Ok(sha256),
        Err(operation) => {
            match tool_root.remove_file_if_exists(target_relative, Some(&temporary_file)) {
                Ok(_) => Err(operation),
                Err(cleanup) => Err(WorkbookBackupError::OperationAndCleanup {
                    operation: Box::new(operation),
                    cleanup,
                }),
            }
        }
    }
}

fn verify_backup_bytes(
    path: &Path,
    source_bytes: &[u8],
    expected_sha256: &str,
) -> Result<String, WorkbookBackupError> {
    let backup_bytes = read_bounded_workbook_bytes(path, MAX_RAW_PACKAGE_BYTES, "工作簿备份")
        .map_err(|source| WorkbookBackupError::BackupWorkbook(source.into()))?;
    validate_package_bytes(&backup_bytes, path).map_err(WorkbookBackupError::BackupWorkbook)?;
    let actual_sha256 = sha256_bytes(&backup_bytes);
    if actual_sha256 != expected_sha256 || backup_bytes != source_bytes {
        return Err(WorkbookBackupError::ContentMismatch {
            expected: expected_sha256.to_owned(),
            actual: actual_sha256,
        });
    }
    Ok(actual_sha256)
}

fn validate_package_bytes(bytes: &[u8], path: &Path) -> Result<(), WorkbookProbeError> {
    let package = PackageSnapshot::from_bytes(bytes, path)?;
    reject_external_content(&package)?;
    Ok(reject_macro_content(&package)?)
}

fn cleanup_after_error(
    tool_root: &ToolRoot,
    temporary_file: &std::fs::File,
    temporary_relative: &Path,
    operation: WorkbookBackupError,
) -> WorkbookBackupError {
    match tool_root.remove_file_if_exists(temporary_relative, Some(temporary_file)) {
        Ok(_) => operation,
        Err(cleanup) => WorkbookBackupError::OperationAndCleanup {
            operation: Box::new(operation),
            cleanup,
        },
    }
}

fn map_backup_error(
    tool_root: &ToolRoot,
    source_relative: &Path,
    backup_relative: &Path,
    source: WorkbookBackupError,
) -> AppError {
    let code = match &source {
        WorkbookBackupError::SourceWorkbook(error) if source_workbook_is_locked(error) => {
            AppErrorCode::WorkbookLocked
        }
        WorkbookBackupError::SourceWorkbook(_) => AppErrorCode::WorkbookInvalid,
        _ => AppErrorCode::WorkbookBackupFailed,
    };
    let message = match code {
        AppErrorCode::WorkbookLocked => "数据工作簿正在被占用，执行前备份尚未完成",
        AppErrorCode::WorkbookInvalid => "数据工作簿未通过执行前备份校验",
        _ => "原工作簿未能安全备份，尚未允许执行游戏写命令",
    };
    AppError::from_source("execution.workbook.backup", code, message, source)
        .with_context(
            "source_path",
            tool_root.as_path().join(source_relative).to_string_lossy(),
        )
        .with_context(
            "backup_path",
            tool_root.as_path().join(backup_relative).to_string_lossy(),
        )
}

fn source_workbook_is_locked(error: &WorkbookProbeError) -> bool {
    if matches!(error, WorkbookProbeError::WorkbookLocked { .. }) {
        return true;
    }
    #[cfg(windows)]
    if let WorkbookProbeError::Io { source, .. } = error {
        use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};

        return matches!(
            source.raw_os_error(),
            Some(value)
                if value == ERROR_SHARING_VIOLATION as i32
                    || value == ERROR_LOCK_VIOLATION as i32
        );
    }
    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{XlsxWorkbookBackupPort, backup_workbook_at};
    use crate::adapters::tool_root::ToolRoot;
    use crate::adapters::workbook::create_representative_workbook;
    use crate::application::{AppErrorCode, WorkbookBackupPort};
    use suzushiro_content_digest::sha256_bytes;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn publishes_an_exact_non_overwriting_workbook_backup() {
        let fixture = TestDirectory::new("publish");
        let workbook_path = fixture.root.join("data/workbooks/plan.xlsx");
        fs::create_dir_all(workbook_path.parent().unwrap()).unwrap();
        create_representative_workbook(&workbook_path).unwrap();
        let source_bytes = fs::read(&workbook_path).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();

        let report = backup_workbook_at(&root, &workbook, 1_700_000_000_123).unwrap();

        assert_eq!(report.message(), "原工作簿备份完成");
        assert_eq!(report.source_path(), "data/workbooks/plan.xlsx");
        assert_eq!(report.backed_up_at_unix_millis(), 1_700_000_000_123);
        assert_eq!(report.size_bytes(), source_bytes.len() as u64);
        assert_eq!(report.source_package_sha256(), sha256_bytes(&source_bytes));
        assert_eq!(
            report.backup_package_sha256(),
            report.source_package_sha256()
        );
        assert_eq!(
            fs::read(fixture.root.join(report.backup_path())).unwrap(),
            source_bytes
        );
        assert_eq!(fs::read(&workbook_path).unwrap(), source_bytes);
        assert_no_temporary_files(&fixture.root);
    }

    #[test]
    fn repeated_backups_use_distinct_numbers_and_preserve_the_source() {
        let fixture = TestDirectory::new("conflict");
        let workbook_path = fixture.root.join("data/workbooks/plan.xlsx");
        fs::create_dir_all(workbook_path.parent().unwrap()).unwrap();
        create_representative_workbook(&workbook_path).unwrap();
        let source_bytes = fs::read(&workbook_path).unwrap();
        let source_sha256 = sha256_bytes(&source_bytes);
        let root = ToolRoot::open(&fixture.root).unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();

        let first = backup_workbook_at(&root, &workbook, 1_700_000_000_123).unwrap();
        let backup_bytes = fs::read(fixture.root.join(first.backup_path())).unwrap();
        let second = backup_workbook_at(&root, &workbook, 1_700_000_000_123).unwrap();

        assert_eq!(first.backup_path(), "data/backups/backup-001.xlsx");
        assert_eq!(second.backup_path(), "data/backups/backup-002.xlsx");
        assert_eq!(fs::read(&workbook_path).unwrap(), source_bytes);
        assert_eq!(
            fs::read(fixture.root.join(first.backup_path())).unwrap(),
            backup_bytes
        );
        assert_eq!(first.source_package_sha256(), source_sha256);
        assert_no_temporary_files(&fixture.root);
    }

    #[test]
    fn preserves_a_temporary_file_not_owned_by_this_backup_attempt() {
        let fixture = TestDirectory::new("temporary-conflict");
        let workbook_path = fixture.root.join("data/workbooks/plan.xlsx");
        fs::create_dir_all(workbook_path.parent().unwrap()).unwrap();
        create_representative_workbook(&workbook_path).unwrap();
        let temporary_relative = PathBuf::from("data/backups/backup-001.tmp.xlsx");
        let target_relative = PathBuf::from("data/backups/backup-001.xlsx");
        fs::create_dir_all(fixture.root.join("data/backups")).unwrap();
        fs::write(fixture.root.join(&temporary_relative), b"other-writer").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();

        let error = backup_workbook_at(&root, &workbook, 1_700_000_000_123).unwrap_err();

        assert_eq!(error.code(), AppErrorCode::WorkbookBackupFailed);
        assert_eq!(
            fs::read(fixture.root.join(temporary_relative)).unwrap(),
            b"other-writer"
        );
        assert!(!fixture.root.join(target_relative).exists());
    }

    #[test]
    fn rejects_an_invalid_source_before_creating_the_backup_directory() {
        let fixture = TestDirectory::new("invalid-source");
        let workbook_path = fixture.root.join("data/workbooks/plan.xlsx");
        fs::create_dir_all(workbook_path.parent().unwrap()).unwrap();
        fs::write(&workbook_path, b"not-an-xlsx").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();

        let error = backup_workbook_at(&root, &workbook, 1_700_000_000_123).unwrap_err();

        assert_eq!(error.stage(), "execution.workbook.backup");
        assert_eq!(error.code(), AppErrorCode::WorkbookInvalid);
        assert!(!fixture.root.join("data/backups").exists());
        assert_eq!(fs::read(workbook_path).unwrap(), b"not-an-xlsx");
    }

    #[test]
    fn public_port_uses_the_same_verified_backup_contract() {
        let fixture = TestDirectory::new("port");
        let workbook_path = fixture.root.join("data/workbooks/plan.xlsx");
        fs::create_dir_all(workbook_path.parent().unwrap()).unwrap();
        create_representative_workbook(&workbook_path).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let port = XlsxWorkbookBackupPort::new(root);

        let report = port.backup_workbook(&workbook).unwrap();

        assert!(report.backup_path().starts_with("data/backups/backup-"));
        assert_eq!(
            report.source_package_sha256(),
            report.backup_package_sha256()
        );
    }

    fn assert_no_temporary_files(root: &Path) {
        let backup_directory = root.join("data/backups");
        if !backup_directory.exists() {
            return;
        }
        assert!(fs::read_dir(backup_directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.xlsx")
        }));
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
                .join("suzushiro/scratch/azlw-workbook-backup-tests")
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

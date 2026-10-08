//! 为应用层提供受控工作簿目录和选择能力。

use std::path::Path;

use crate::adapters::tool_root::ToolRoot;
use crate::application::{
    AppError, AppErrorCode, WORKBOOK_DIRECTORY, WorkbookCatalogEntry, WorkbookCatalogPort,
    WorkbookCatalogReport, WorkbookRef,
};

/// 使用固定工具根目录读取和选择 XLSX 工作簿。
pub(crate) struct XlsxWorkbookCatalogPort {
    tool_root: ToolRoot,
}

impl XlsxWorkbookCatalogPort {
    /// 固定工具根目录，目录读取和选择都不接受外部路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl WorkbookCatalogPort for XlsxWorkbookCatalogPort {
    fn list_workbooks(&self) -> Result<WorkbookCatalogReport, AppError> {
        let entries = self
            .tool_root
            .list_workbook_files()
            .map_err(|source| map_catalog_error(&self.tool_root, source))?;
        let workbooks = entries
            .into_iter()
            .map(|(name, size_bytes)| {
                let relative_path = format!("{WORKBOOK_DIRECTORY}/{name}");
                WorkbookCatalogEntry::new(name, relative_path, size_bytes)
            })
            .collect();
        Ok(WorkbookCatalogReport::new(workbooks))
    }

    fn select_workbook(&self, workbook_name: &str) -> Result<WorkbookRef, AppError> {
        self.tool_root
            .existing_workbook(workbook_name)
            .map_err(|source| map_selection_error(&self.tool_root, workbook_name, source))
    }
}

fn map_catalog_error(
    tool_root: &ToolRoot,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    AppError::from_source(
        "workbook.catalog",
        AppErrorCode::WorkbookInvalid,
        "工作簿目录未能安全读取",
        source,
    )
    .with_context(
        "path",
        tool_root
            .as_path()
            .join(Path::new(WORKBOOK_DIRECTORY))
            .to_string_lossy(),
    )
}

fn map_selection_error(
    tool_root: &ToolRoot,
    workbook_name: &str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    AppError::from_source(
        "workbook.select",
        AppErrorCode::WorkbookInvalid,
        "所选工作簿不符合受控路径要求",
        source,
    )
    .with_context(
        "path",
        tool_root
            .as_path()
            .join(Path::new(WORKBOOK_DIRECTORY))
            .to_string_lossy(),
    )
    .with_context("workbook_name", workbook_name)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::XlsxWorkbookCatalogPort;
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::WorkbookCatalogPort;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn lists_and_selects_only_controlled_xlsx_files() {
        let fixture = TestDirectory::new("catalog");
        let workbook_directory = fixture.root.join("data/workbooks");
        fs::create_dir_all(&workbook_directory).unwrap();
        fs::write(workbook_directory.join("zeta.xlsx"), b"123").unwrap();
        fs::write(workbook_directory.join("Alpha.XLSX"), b"12").unwrap();
        fs::write(workbook_directory.join("notes.txt"), b"ignored").unwrap();
        fs::write(workbook_directory.join("~$zeta.xlsx"), b"lock").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let port = XlsxWorkbookCatalogPort::new(root);

        let report = port.list_workbooks().unwrap();

        assert_eq!(report.count(), 2);
        assert_eq!(report.workbooks()[0].name(), "Alpha.XLSX");
        assert_eq!(report.workbooks()[0].size_bytes(), 2);
        assert_eq!(report.workbooks()[1].name(), "zeta.xlsx");
        assert_eq!(report.workbooks()[1].kind(), "workbook");
        assert!(port.select_workbook("~$zeta.xlsx").is_err());
        assert_eq!(
            port.select_workbook("zeta.xlsx").unwrap().relative_path(),
            std::path::Path::new("data/workbooks/zeta.xlsx")
        );
    }

    #[test]
    fn distinguishes_layout_artifacts_and_keeps_them_selectable() {
        let fixture = TestDirectory::new("layout-artifacts");
        let directory = fixture.root.join("data/workbooks");
        fs::create_dir_all(&directory).unwrap();
        for name in ["layout-preview.xlsx", "workbook-layout.updated.xlsx"] {
            fs::write(directory.join(name), b"fixture").unwrap();
        }
        let port = XlsxWorkbookCatalogPort::new(ToolRoot::open(&fixture.root).unwrap());
        let report = port.list_workbooks().unwrap();
        assert_eq!(report.count(), 2);
        assert_eq!(report.workbooks()[0].kind(), "layout_preview");
        assert_eq!(report.workbooks()[1].kind(), "layout_upgrade");
        for entry in report.workbooks() {
            assert!(port.select_workbook(entry.name()).is_ok());
        }
    }

    #[test]
    fn maps_invalid_selection_to_a_stable_application_error() {
        let fixture = TestDirectory::new("selection-error");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let port = XlsxWorkbookCatalogPort::new(root);

        let error = port.select_workbook("../outside.xlsx").unwrap_err();

        assert_eq!(error.code().as_str(), "WORKBOOK_INVALID");
        assert_eq!(error.stage(), "workbook.select");
        assert_eq!(
            error.context().get("workbook_name").map(String::as_str),
            Some("../outside.xlsx")
        );
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
                .join("suzushiro/scratch/azlw-workbook-catalog-tests")
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

//! 将工作簿结果端口适配到受控 XLSX 的摘要绑定原子写回。

pub(in crate::adapters::workbook) mod writer;

use std::path::PathBuf;

use thiserror::Error;

use crate::adapters::history::relative_path_string;
use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::application::{
    AppError, AppErrorCode, ExecutionResultsPort, ExecutionResultsWriteReport, WorkbookLayout,
    WorkbookProjectionRow, WorkbookRef,
};

use super::{
    AtomicExecutionResultsWriteEvidence, ExecutionResultsWriteEvidence, WorkbookProbeError,
};

/// 在固定工具根目录内更新检查和执行结果工作表的正式适配器。
pub(crate) struct XlsxExecutionResultsPort {
    tool_root: ToolRoot,
    acquisition: Option<std::sync::Arc<super::ShipAcquisition>>,
    documents: super::document::WorkbookDocuments,
}

impl XlsxExecutionResultsPort {
    /// 固定受控工具根目录，写回时不接受调用方提供的任意路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self {
            tool_root,
            acquisition: None,
            documents: super::document::WorkbookDocuments::new(),
        }
    }

    pub(crate) fn with_acquisition(
        mut self,
        source: std::sync::Arc<super::ShipAcquisition>,
    ) -> Self {
        self.acquisition = Some(source);
        self
    }

    pub(crate) fn with_documents(mut self, documents: super::document::WorkbookDocuments) -> Self {
        self.documents = documents;
        self
    }
}

impl ExecutionResultsPort for XlsxExecutionResultsPort {
    fn write_check_results(
        &self,
        workbook: &WorkbookRef,
        expected_source_package_sha256: &str,
        layout: &WorkbookLayout,
        result: Result<&crate::application::CheckReport, &AppError>,
        checked_at: Option<i64>,
    ) -> Result<(), AppError> {
        self.documents.release();
        let checked_at = match checked_at {
            Some(value) => value,
            None => crate::adapters::history::current_unix_millis().map_err(|source| {
                AppError::from_source(
                    "check.workbook.writeback",
                    AppErrorCode::WorkbookInvalid,
                    "检查结果时间读取失败",
                    source,
                )
            })?,
        };
        let rows = crate::application::project_check_result_rows(result, checked_at).map_err(
            |source| {
                AppError::from_source(
                    "check.workbook.writeback",
                    AppErrorCode::WorkbookInvalid,
                    "检查结果行生成失败",
                    source,
                )
            },
        )?;
        self.tool_root
            .existing_file(workbook.relative_path())
            .map_err(ExecutionResultsAdapterError::ToolRoot)
            .and_then(|path| {
                writer::write_result_rows_atomically(
                    &path,
                    Some(expected_source_package_sha256),
                    layout,
                    &rows,
                    "check_results",
                )
                .map_err(ExecutionResultsAdapterError::Workbook)
            })
            .map(|_| ())
            .map_err(|source| map_check_results_error(workbook, "check.workbook.writeback", source))
    }

    fn validate_writeback(
        &self,
        workbook: &WorkbookRef,
        expected_source_package_sha256: &str,
        layout: &WorkbookLayout,
        projection: crate::application::WorkbookProjectionV4,
    ) -> Result<(), AppError> {
        let result = (|| {
            let path = self
                .tool_root
                .existing_file(workbook.relative_path())
                .map_err(ExecutionResultsAdapterError::ToolRoot)?;
            let actual = super::document::source_digest(&path, "写回预检源身份")
                .map_err(ExecutionResultsAdapterError::Workbook)?;
            if actual != expected_source_package_sha256 {
                return Err(ExecutionResultsAdapterError::Workbook(
                    super::WorkbookProbeError::Io {
                        stage: "写回预检源身份",
                        path: path.clone(),
                        source: std::io::Error::other(format!(
                            "source changed: expected={expected_source_package_sha256}, actual={actual}"
                        )),
                    },
                ));
            }
            self.documents
                .with_current_index(&path, &actual, |document| {
                    super::projection_writer::technology_views::source_sheet_bindings(
                        document.bytes(),
                        document.package(),
                        layout,
                        &projection,
                    )
                })
                .map_err(ExecutionResultsAdapterError::Workbook)?;
            self.documents.release();
            Ok::<_, ExecutionResultsAdapterError>(())
        })();
        result.map_err(|source| {
            AppError::from_source(
                "execution.workbook.preflight",
                AppErrorCode::WorkbookInvalid,
                "工作簿写回结构预检未通过，尚未执行游戏操作",
                source,
            )
            .with_context("path", workbook.relative_path().to_string_lossy())
        })
    }

    fn write_execution_results(
        &self,
        workbook: &WorkbookRef,
        expected_source_package_sha256: &str,
        layout: &WorkbookLayout,
        rows: &[WorkbookProjectionRow],
        final_projection: Option<crate::application::WorkbookProjectionV4>,
        recorded_at_unix_millis: i64,
    ) -> Result<ExecutionResultsWriteReport, AppError> {
        self.documents.release();
        let final_projection = match (self.acquisition.as_ref(), final_projection) {
            (Some(source), Some(projection)) => Some(
                source
                    .enrich(layout, projection, None, &mut |_| {}, &|| false)?
                    .projection,
            ),
            (_, projection) => projection,
        };
        let relative_path = workbook.relative_path();
        let workbook_path = self
            .tool_root
            .existing_file(relative_path)
            .map_err(ExecutionResultsAdapterError::ToolRoot)
            .and_then(|path| {
                writer::write_execution_snapshot_atomically_if_source_matches(
                    &path,
                    expected_source_package_sha256,
                    layout,
                    rows,
                    final_projection.as_ref(),
                    recorded_at_unix_millis,
                )
                .map_err(ExecutionResultsAdapterError::Workbook)
            })
            .map_err(|source| {
                map_execution_results_error(self.tool_root.as_path().join(relative_path), source)
            })?;
        Ok(report_from_evidence(
            relative_path_string(relative_path),
            workbook_path,
        ))
    }
}

fn map_check_results_error(
    workbook: &WorkbookRef,
    stage: &'static str,
    source: ExecutionResultsAdapterError,
) -> AppError {
    let code = if source.is_workbook_locked() {
        AppErrorCode::WorkbookLocked
    } else {
        AppErrorCode::WorkbookInvalid
    };
    let changed = source
        .source_changed()
        .map(|(expected, actual)| (expected.to_owned(), actual.to_owned()));
    let message = if stage == "check.workbook.prepare" {
        "检查源工作簿读取失败"
    } else {
        "检查结果工作簿写回失败"
    };
    let mut error = AppError::from_source(stage, code, message, source)
        .with_context("path", workbook.relative_path().to_string_lossy());
    if let Some((expected, actual)) = changed {
        error = error
            .with_context("source_changed", "true")
            .with_context("expected", expected)
            .with_context("actual", actual);
    }
    error
}

fn report_from_evidence(
    workbook_path: String,
    evidence: AtomicExecutionResultsWriteEvidence,
) -> ExecutionResultsWriteReport {
    let AtomicExecutionResultsWriteEvidence {
        write,
        replacement_method,
        temporary_file_removed,
    } = evidence;
    let ExecutionResultsWriteEvidence {
        sheet_name,
        worksheet_part,
        table_part,
        row_count,
        source_package_sha256,
        output_package_sha256,
        package,
    } = write;
    ExecutionResultsWriteReport::new(
        workbook_path,
        sheet_name,
        worksheet_part,
        table_part,
        row_count,
        source_package_sha256,
        output_package_sha256,
        package.entry_count,
        package.unchanged_entry_count,
        package.changed_parts,
        replacement_method,
        temporary_file_removed,
    )
}

fn map_execution_results_error(path: PathBuf, source: ExecutionResultsAdapterError) -> AppError {
    let source_changed = source
        .source_changed()
        .map(|(expected, actual)| (expected.to_owned(), actual.to_owned()));
    let code = if source.is_workbook_locked() {
        AppErrorCode::WorkbookLocked
    } else {
        AppErrorCode::ExecutionResultsWriteFailed
    };
    let message = match code {
        AppErrorCode::WorkbookLocked => "数据工作簿正在被占用，执行结果尚未写回",
        _ if source_changed.is_some() => "数据工作簿在执行期间发生变化，执行结果未覆盖新的编辑",
        _ => "执行结果未能在保留工作簿其他内容的前提下安全写回",
    };
    let mut error = AppError::from_source("execution.workbook.writeback", code, message, source)
        .with_context("path", path.to_string_lossy());
    if let Some((expected, actual)) = source_changed {
        error = error
            .with_context("source_changed", "true")
            .with_context("expected", expected)
            .with_context("actual", actual);
    }
    error
}

/// 保留受控路径和工作簿机制的完整底层原因链。
#[derive(Debug, Error)]
enum ExecutionResultsAdapterError {
    #[error("解析受控工作簿路径失败: {0}")]
    ToolRoot(#[source] ToolRootError),
    #[error("结果工作簿写回失败: {0}")]
    Workbook(#[source] WorkbookProbeError),
}

impl ExecutionResultsAdapterError {
    fn is_workbook_locked(&self) -> bool {
        matches!(
            self,
            Self::Workbook(WorkbookProbeError::WorkbookLocked { .. })
        )
    }

    fn source_changed(&self) -> Option<(&str, &str)> {
        match self {
            Self::Workbook(WorkbookProbeError::SourceChanged {
                expected, actual, ..
            }) => Some((expected, actual)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::XlsxExecutionResultsPort;
    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state;
    use crate::adapters::tool_root::ToolRoot;
    use crate::adapters::workbook::projection_writer::build_projection_workbook_bytes;
    use crate::application::test_support::execution_workbook_report_fixture;
    use crate::application::{
        AppErrorCode, ExecutionResultsPort, WorkbookPort, WorkbookProjectionV4,
        project_execution_report_rows, project_game_state_to_workbook,
    };
    use suzushiro_content_digest::sha256_bytes;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn adapts_the_guarded_writer_without_exposing_workbook_types() {
        let fixture = TestDirectory::new("success");
        let root = ToolRoot::open(fixture.path()).unwrap();
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let layout = crate::adapters::workbook::load_workbook_layout(
            &fixture.path().join("workbook-layout.xlsx"),
            &registry,
        )
        .unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let source_bytes = fs::read(fixture.path().join("data/workbooks/plan.xlsx")).unwrap();
        let report = execution_workbook_report_fixture();
        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();
        let port = XlsxExecutionResultsPort::new(root);

        let evidence = port
            .write_execution_results(
                &workbook,
                &sha256_bytes(&source_bytes),
                &layout,
                &rows,
                None,
                1_700_000_000_123,
            )
            .unwrap();

        assert_eq!(evidence.workbook_path(), "data/workbooks/plan.xlsx");
        assert_eq!(evidence.row_count(), rows.len());
        assert_eq!(
            evidence.source_package_sha256(),
            sha256_bytes(&source_bytes)
        );
        assert!(evidence.temporary_file_removed());
        assert!(evidence.changed_parts().len() <= 2);
    }

    #[test]
    fn maps_a_backup_digest_mismatch_to_a_stable_writeback_error() {
        let fixture = TestDirectory::new("source-changed");
        let root = ToolRoot::open(fixture.path()).unwrap();
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let layout = crate::adapters::workbook::load_workbook_layout(
            &fixture.path().join("workbook-layout.xlsx"),
            &registry,
        )
        .unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let report = execution_workbook_report_fixture();
        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();
        let port = XlsxExecutionResultsPort::new(root);

        let error = port
            .write_execution_results(
                &workbook,
                &"0".repeat(64),
                &layout,
                &rows,
                None,
                1_700_000_000_123,
            )
            .unwrap_err();

        assert_eq!(error.code(), AppErrorCode::ExecutionResultsWriteFailed);
        assert_eq!(error.stage(), "execution.workbook.writeback");
        assert_eq!(
            error.context().get("source_changed").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            error.context().get("expected").map(String::as_str),
            Some("0000000000000000000000000000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn check_results_replace_previous_outcome_and_preserve_other_parts() {
        use crate::adapters::workbook::package::PackageSnapshot;
        use crate::application::test_support::empty_game_state;
        use crate::application::{AppError, compile_plan};
        use calamine::{Reader, Xlsx, open_workbook};
        use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;
        let fixture = TestDirectory::new("check-results");
        let root = ToolRoot::open(fixture.path()).unwrap();
        let layout = crate::adapters::workbook::load_workbook_layout(
            &fixture.path().join("workbook-layout.xlsx"),
            &WorkbookProjectionV4::layout_registry().unwrap(),
        )
        .unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let path = fixture.path().join("data/workbooks/plan.xlsx");
        let before = fs::read(&path).unwrap();
        let source = PackageSnapshot::from_bytes(&before, &path).unwrap();
        let worksheet = worksheet_part_name(&source, "检查结果").unwrap();
        let port = XlsxExecutionResultsPort::new(root);
        let hash = suzushiro_content_digest::sha256_bytes(&before);
        let report = compile_plan(
            &empty_game_state(),
            &crate::domain::DesiredState::new(vec![]).unwrap(),
        )
        .unwrap();
        port.write_check_results(
            &workbook,
            &hash,
            &layout,
            Ok(&report),
            Some(1_700_000_000_123),
        )
        .unwrap();
        let after = fs::read(&path).unwrap();
        crate::adapters::workbook::editor::compare_packages(
            &source,
            &PackageSnapshot::from_bytes(&after, &path).unwrap(),
            &[&worksheet],
        )
        .unwrap();
        let mut xlsx: Xlsx<_> = open_workbook(&path).unwrap();
        let range = xlsx.worksheet_range("检查结果").unwrap();
        assert_eq!(
            range.get_value((1, 1)),
            Some(&calamine::Data::String("通过".to_owned()))
        );
        drop(xlsx);
        let error = AppError::from_source(
            "plan.check",
            AppErrorCode::EquipmentStateChanged,
            "装备不足",
            std::io::Error::other("仓库只有8件"),
        )
        .with_context("source", "warehouse:5240")
        .with_context("available", "8")
        .with_context("required", "9");
        let current_bytes = fs::read(&path).unwrap();
        let hash = suzushiro_content_digest::sha256_bytes(&current_bytes);
        port.write_check_results(&workbook, &hash, &layout, Err(&error), None)
            .unwrap();
        let mut xlsx: Xlsx<_> = open_workbook(&path).unwrap();
        let range = xlsx.worksheet_range("检查结果").unwrap();
        assert_eq!(range.height(), 2);
        assert_eq!(
            range.get_value((1, 1)),
            Some(&calamine::Data::String("未通过".to_owned()))
        );
        assert_eq!(
            range.get_value((1, 4)),
            Some(&calamine::Data::String("8".to_owned()))
        );
        assert_eq!(
            range.get_value((1, 5)),
            Some(&calamine::Data::String("9".to_owned()))
        );
        drop(xlsx);
        let current = fs::read(&path).unwrap();
        let rejected = port
            .write_check_results(&workbook, &hash, &layout, Ok(&report), None)
            .unwrap_err();
        assert_eq!(
            rejected.context().get("source_changed").map(String::as_str),
            Some("true")
        );
        assert_eq!(fs::read(&path).unwrap(), current);
    }

    #[test]
    fn writeback_preflight_rejects_a_source_that_differs_from_the_plan_snapshot() {
        let fixture = TestDirectory::new("preflight-source");
        let root = ToolRoot::open(fixture.path()).unwrap();
        let layout = crate::adapters::workbook::load_workbook_layout(
            &fixture.path().join("workbook-layout.xlsx"),
            &WorkbookProjectionV4::layout_registry().unwrap(),
        )
        .unwrap();
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let path = fixture.path().join("data/workbooks/plan.xlsx");
        let bytes = fs::read(&path).unwrap();
        let hash = suzushiro_content_digest::sha256_bytes(&bytes);
        let projection =
            crate::application::project_game_state_to_workbook(&golden_game_state()).unwrap();
        let port = XlsxExecutionResultsPort::new(root);
        let rejected = port
            .validate_writeback(&workbook, &"0".repeat(64), &layout, projection.clone())
            .unwrap_err();
        assert!(rejected.message().contains("尚未执行游戏操作"));
        port.validate_writeback(&workbook, &hash, &layout, projection)
            .unwrap();
    }

    #[test]
    fn plan_read_reuses_the_index_until_the_source_digest_changes() {
        let fixture = TestDirectory::new("shared-document");
        let root = ToolRoot::open(fixture.path()).unwrap();
        let documents = crate::adapters::workbook::WorkbookDocuments::new();
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let workbook_port = crate::adapters::workbook::layout::XlsxWorkbookPort::new(
            root.clone(),
            fixture.path().join("workbook-layout.xlsx"),
            registry,
        )
        .with_documents(documents.clone());
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let inputs = workbook_port.load_plan_inputs(&workbook).unwrap();
        assert!(documents.holds_document(), "计划读取应持有这份工作簿");
        let check_error = crate::application::AppError::from_source(
            "plan.check",
            crate::application::AppErrorCode::WorkbookInvalid,
            "预检失败",
            std::io::Error::other("preflight"),
        );
        let path = fixture.path().join("data/workbooks/plan.xlsx");
        let bytes_before = fs::read(&path).unwrap();
        let port = XlsxExecutionResultsPort::new(root).with_documents(documents.clone());
        let rejected = port.write_check_results(
            &workbook,
            &"0".repeat(64),
            &inputs.layout,
            Err(&check_error),
            Some(1),
        );
        assert!(rejected.is_err(), "源摘要不符时不得覆盖工作簿");
        assert_eq!(fs::read(&path).unwrap(), bytes_before);
        assert!(
            !documents.holds_document(),
            "检查写回应在重新读取前释放预检文档"
        );
        let projection =
            crate::application::project_game_state_to_workbook(&golden_game_state()).unwrap();
        port.validate_writeback(
            &workbook,
            &inputs.source_package_sha256,
            &inputs.layout,
            projection.clone(),
        )
        .unwrap();
        assert!(
            !documents.holds_document(),
            "预检成功后应释放文档，执行写回再读取自己的副本"
        );

        let path = fixture.path().join("data/workbooks/plan.xlsx");
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(&path, bytes).unwrap();
        let error = port
            .validate_writeback(
                &workbook,
                &inputs.source_package_sha256,
                &inputs.layout,
                projection,
            )
            .unwrap_err();
        let mut detail = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            detail.push('\n');
            detail.push_str(&cause.to_string());
            source = cause.source();
        }
        assert!(
            detail.contains("source changed"),
            "摘要变化后不得沿用旧索引: {detail}"
        );
    }

    #[test]
    fn plan_reads_changed_workbook_and_rejects_invalid_layout() {
        use crate::adapters::workbook::package::{PackageSnapshot, rewrite_package_from_bytes};

        let fixture = TestDirectory::new("plan-input-changes");
        let root = ToolRoot::open(fixture.path()).unwrap();
        let documents = crate::adapters::workbook::WorkbookDocuments::new();
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let layout_path = fixture.path().join("workbook-layout.xlsx");
        let port = crate::adapters::workbook::layout::XlsxWorkbookPort::new(
            root.clone(),
            layout_path.clone(),
            registry,
        )
        .with_documents(documents.clone());
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let first = port.load_plan_inputs(&workbook).unwrap();
        let second = port.load_plan_inputs(&workbook).unwrap();
        assert_eq!(first.source_package_sha256, second.source_package_sha256);

        let plan_path = fixture.path().join("data/workbooks/plan.xlsx");
        let original_plan = fs::read(&plan_path).unwrap();
        let dismantle = first
            .layout
            .enum_options()
            .iter()
            .find(|option| {
                option.category_key() == "inventory_operation" && option.stable_value() == "enhance"
            })
            .unwrap()
            .label();
        let inventory_row = warehouse_inventory_row(&plan_path, &first.layout);
        for (field, value) in [
            ("operation", dismantle),
            ("target_enhance_level", "1"),
            ("processing_quantity", "1"),
        ] {
            let column = layout_field_column(&first.layout, "equipment_inventory", field);
            let edited_plan = plan_path.with_extension("edited.xlsx");
            crate::adapters::workbook::edit_text_cell_to_new_file(
                &plan_path,
                &edited_plan,
                "装备总表",
                &format!("{}{inventory_row}", excel_column_name(column)),
                value,
            )
            .unwrap();
            fs::remove_file(&plan_path).unwrap();
            fs::rename(&edited_plan, &plan_path).unwrap();
        }
        let edited_inputs = port.load_plan_inputs(&workbook).unwrap();
        assert_ne!(
            edited_inputs.source_package_sha256,
            first.source_package_sha256
        );
        assert_ne!(edited_inputs.inventory, first.inventory);
        fs::write(&plan_path, &original_plan).unwrap();

        let original_layout = fs::read(&layout_path).unwrap();
        let layout_package = PackageSnapshot::from_bytes(&original_layout, &layout_path).unwrap();
        let app_name = "docProps/app.xml";
        let mut app = layout_package.part(app_name).unwrap().to_vec();
        let marker = b"<Application>";
        let title = app
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("根布局应有应用属性");
        app.insert(title + marker.len(), b' ');
        let edited_layout = rewrite_package_from_bytes(
            &original_layout,
            std::io::Cursor::new(Vec::new()),
            &layout_path,
            &layout_path,
            &std::collections::BTreeMap::from([(app_name.to_owned(), app)]),
            &[],
        )
        .unwrap()
        .into_inner();
        fs::write(&layout_path, &edited_layout).unwrap();
        let changed_layout = port.load_plan_inputs(&workbook).unwrap();
        assert_eq!(
            changed_layout.layout.content_sha256(),
            first.layout.content_sha256()
        );
        fs::write(&layout_path, &original_layout).unwrap();

        let mut broken_layout = original_layout.clone();
        broken_layout[0] = 0;
        fs::write(&layout_path, &broken_layout).unwrap();
        assert!(port.load_plan_inputs(&workbook).is_err());
        fs::write(&layout_path, &original_layout).unwrap();
        let restored = port.load_plan_inputs(&workbook).unwrap();
        assert_eq!(restored.source_package_sha256, first.source_package_sha256);

        let projection =
            crate::application::project_game_state_to_workbook(&golden_game_state()).unwrap();
        let results = XlsxExecutionResultsPort::new(root).with_documents(documents.clone());
        results
            .validate_writeback(
                &workbook,
                &restored.source_package_sha256,
                &restored.layout,
                projection,
            )
            .unwrap();
        assert!(!documents.holds_document());
    }

    fn layout_field_column(
        layout: &crate::application::WorkbookLayout,
        sheet: &str,
        field: &str,
    ) -> usize {
        layout
            .fields()
            .iter()
            .filter(|candidate| {
                candidate.sheet_key() == sheet
                    && candidate.generation() != crate::application::LayoutGenerationMode::Omitted
            })
            .position(|candidate| candidate.stable_key() == field)
            .unwrap()
    }

    fn excel_column_name(mut zero_based: usize) -> String {
        let mut value = String::new();
        zero_based += 1;
        while zero_based > 0 {
            let remainder = (zero_based - 1) % 26;
            value.push(char::from(b'A' + u8::try_from(remainder).unwrap()));
            zero_based = (zero_based - 1) / 26;
        }
        value.chars().rev().collect()
    }

    fn warehouse_inventory_row(
        path: &std::path::Path,
        layout: &crate::application::WorkbookLayout,
    ) -> u32 {
        use calamine::{Data, Reader as _, Xlsx, open_workbook_from_rs};
        let bytes = fs::read(path).unwrap();
        let mut workbook: Xlsx<_> =
            open_workbook_from_rs(std::io::Cursor::new(bytes.as_slice())).unwrap();
        let range = workbook.worksheet_range("装备总表").unwrap();
        let source_column =
            layout_field_column(layout, "equipment_inventory", "source_type") as u32;
        for row in 1..u32::try_from(range.height()).unwrap() {
            if matches!(range.get_value((row, source_column)), Some(Data::String(value)) if value == "仓库")
            {
                return row + 1;
            }
        }
        panic!("测试工作簿缺少仓库库存行");
    }

    struct TestDirectory {
        path: PathBuf,
    }

    /// 显式测量入口。默认测试不进入；设置 AZLW_MEASURE_MODE 后用 --ignored 运行。
    #[test]
    #[ignore = "AZLW_MEASURE_MODE=generate 或 run"]
    fn measure_workbook_handoff_when_requested() {
        match std::env::var("AZLW_MEASURE_MODE").as_deref() {
            Ok("generate") => generate_measure_sample(),
            Ok("run") => run_measure_sample(),
            Ok(other) => panic!("未知 AZLW_MEASURE_MODE: {other}"),
            Err(_) => {}
        }
    }

    fn measure_profile() -> &'static str {
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    }

    /// 阶段结束才输出。wall_ms 是从本次测量起点到阶段结束的墙钟位置，elapsed_ms 只计本阶段。
    fn emit_measure_stage(
        origin: std::time::Instant,
        started: std::time::Instant,
        stage: &str,
        detail: &str,
    ) {
        let wall_ms = origin.elapsed().as_millis();
        let elapsed_ms = started.elapsed().as_millis();
        if detail.is_empty() {
            println!("\nMEASURE stage={stage} wall_ms={wall_ms} elapsed_ms={elapsed_ms}");
        } else {
            println!("\nMEASURE stage={stage} wall_ms={wall_ms} elapsed_ms={elapsed_ms} {detail}");
        }
    }

    fn projection_measure_counts(projection: &WorkbookProjectionV4) -> (usize, usize, usize) {
        let rows: usize = projection
            .sheets()
            .iter()
            .map(|sheet| sheet.rows().len())
            .sum();
        let cells: usize = projection
            .sheets()
            .iter()
            .map(|sheet| sheet.field_keys().len().saturating_mul(sheet.rows().len()))
            .sum();
        let inventory = projection
            .sheet("equipment_inventory")
            .map(|sheet| sheet.rows().len())
            .unwrap_or(0);
        (rows, cells, inventory)
    }

    fn generate_measure_sample() {
        let output =
            PathBuf::from(std::env::var("AZLW_MEASURE_OUTPUT").expect("AZLW_MEASURE_OUTPUT"));
        let sheet_rows: usize = std::env::var("AZLW_MEASURE_SHEET_ROWS")
            .expect("AZLW_MEASURE_SHEET_ROWS")
            .parse()
            .expect("AZLW_MEASURE_SHEET_ROWS 应为整数");
        let origin = std::time::Instant::now();
        let mut started = origin;
        fs::create_dir_all(output.join("data/workbooks")).unwrap();
        let layout_path = output.join("workbook-layout.xlsx");
        crate::adapters::workbook::create_default_layout_workbook(&layout_path).unwrap();
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let layout =
            crate::adapters::workbook::load_workbook_layout(&layout_path, &registry).unwrap();
        emit_measure_stage(origin, started, "layout", "cache=cold");
        started = std::time::Instant::now();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
        let projection = if sheet_rows == 0 {
            projection
        } else {
            projection
                .scale_business_sheet("equipment_inventory", sheet_rows)
                .expect("装备库存行应能按同一结构扩展")
        };
        let (rows, cells, inventory) = projection_measure_counts(&projection);
        emit_measure_stage(
            origin,
            started,
            "projection",
            &format!("rows={rows} cells={cells} inventory_rows={inventory}"),
        );
        started = std::time::Instant::now();
        let plan_path = output.join("data/workbooks/plan.xlsx");
        let build =
            build_projection_workbook_bytes(&plan_path, &layout, &projection, 1_700_000_000_000)
                .unwrap();
        fs::write(&plan_path, &build.bytes).unwrap();
        let bytes = build.bytes.len();
        emit_measure_stage(origin, started, "zip_build", &format!("bytes={bytes}"));
        started = std::time::Instant::now();
        let reopened =
            crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(&plan_path, &layout);
        assert!(reopened.is_ok(), "生成后应能重新读取");
        emit_measure_stage(origin, started, "reopen", "reopened=1");
        println!(
            "\nMEASURE summary profile={} operation_wall_ms={} rows={rows} cells={cells} inventory_rows={inventory} bytes={bytes} reopened=1 cache=cold rpc=not_applicable sync_data=not_applicable readiness=not_applicable static_catalog=not_applicable package_stage=zip_build",
            measure_profile(),
            origin.elapsed().as_millis()
        );
    }

    fn run_measure_sample() {
        let root_path =
            PathBuf::from(std::env::var("AZLW_MEASURE_ROOT").expect("AZLW_MEASURE_ROOT"));
        let sheet_rows: usize = std::env::var("AZLW_MEASURE_SHEET_ROWS")
            .unwrap_or_else(|_| "0".to_owned())
            .parse()
            .expect("AZLW_MEASURE_SHEET_ROWS 应为整数");
        let root = ToolRoot::open(&root_path).unwrap();
        let documents = crate::adapters::workbook::WorkbookDocuments::new();
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let workbook_port = crate::adapters::workbook::layout::XlsxWorkbookPort::new(
            root.clone(),
            root_path.join("workbook-layout.xlsx"),
            registry,
        )
        .with_documents(documents.clone());
        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
        let projection = if sheet_rows == 0 {
            projection
        } else {
            projection
                .scale_business_sheet("equipment_inventory", sheet_rows)
                .unwrap()
        };
        let (rows, cells, inventory) = projection_measure_counts(&projection);
        let plan_path = root_path.join("data/workbooks/plan.xlsx");
        let input_bytes = fs::metadata(&plan_path).unwrap().len();
        let port = XlsxExecutionResultsPort::new(root).with_documents(documents.clone());
        let origin = std::time::Instant::now();
        let mut started = origin;
        let inputs = workbook_port.load_plan_inputs(&workbook).unwrap();
        let held_after_read = documents.holds_document();
        emit_measure_stage(
            origin,
            started,
            "read",
            &format!("bytes={input_bytes} held={held_after_read} cache=miss layout_parse=1"),
        );
        started = std::time::Instant::now();
        let reread = workbook_port.load_plan_inputs(&workbook).unwrap();
        assert_eq!(reread.source_package_sha256, inputs.source_package_sha256);
        emit_measure_stage(
            origin,
            started,
            "read_again",
            "layout_parse=1 package_reparse=1",
        );
        started = std::time::Instant::now();
        port.validate_writeback(
            &workbook,
            &inputs.source_package_sha256,
            &inputs.layout,
            projection.clone(),
        )
        .unwrap();
        let held_after_preflight = documents.holds_document();
        emit_measure_stage(
            origin,
            started,
            "preflight",
            &format!("held={held_after_preflight} cache=released"),
        );
        started = std::time::Instant::now();
        let execution_rows =
            project_execution_report_rows(&execution_workbook_report_fixture(), 1_700_000_001_000)
                .unwrap();
        emit_measure_stage(
            origin,
            started,
            "execution_rows",
            &format!("rows={}", execution_rows.len()),
        );
        started = std::time::Instant::now();
        port.write_execution_results(
            &workbook,
            &inputs.source_package_sha256,
            &inputs.layout,
            &execution_rows,
            Some(projection),
            1_700_000_001_000,
        )
        .unwrap();
        let held_after_write = documents.holds_document();
        emit_measure_stage(
            origin,
            started,
            "write",
            &format!("held={held_after_write} cache=released"),
        );
        started = std::time::Instant::now();
        let reopened = crate::adapters::workbook::reader::load_workbook_plan_from_xlsx(
            &plan_path,
            &inputs.layout,
        );
        assert!(reopened.is_ok(), "写回后应能重新读取");
        emit_measure_stage(origin, started, "reopen", "reopened=1");
        println!(
            "\nMEASURE summary profile={} operation_wall_ms={} rows={rows} cells={cells} inventory_rows={inventory} bytes={input_bytes} held_after_read={held_after_read} held_after_preflight={held_after_preflight} held_after_write={held_after_write} reopened=1 rpc=not_applicable sync_data=not_applicable readiness=not_applicable static_catalog=not_applicable refreshed=dictionaries,equipment_inventory,loadout_plan,resource_recipes,raw_data,execution_results,schema,ship_technology",
            measure_profile(),
            origin.elapsed().as_millis()
        );
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let parent = home.join("suzushiro/scratch/azlw-execution-results-port-tests");
            fs::create_dir_all(&parent).expect("应建立执行结果端口测试根目录");
            let path = parent.join(format!("{label}-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("测试目录不得与残留目录重名");
            fs::create_dir_all(path.join("data/workbooks")).unwrap();
            let layout_path = path.join("workbook-layout.xlsx");
            crate::adapters::workbook::create_default_layout_workbook(&layout_path).unwrap();
            let registry = WorkbookProjectionV4::layout_registry().unwrap();
            let layout =
                crate::adapters::workbook::load_workbook_layout(&layout_path, &registry).unwrap();
            let state = golden_game_state();
            let projection: WorkbookProjectionV4 = project_game_state_to_workbook(&state).unwrap();
            let build = build_projection_workbook_bytes(
                &path.join("data/workbooks/plan.xlsx"),
                &layout,
                &projection,
                1_700_000_000_000,
            )
            .unwrap();
            fs::write(path.join("data/workbooks/plan.xlsx"), build.bytes).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if self.path.exists() {
                fs::remove_dir_all(&self.path).expect("应清理执行结果端口测试目录");
            }
        }
    }
}

//! 在工具根目录内非覆盖生成、验证并排他发布正式数据工作簿。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::application::{
    AcquisitionGenerationState, AcquisitionGenerationSummary, AppError, AppErrorCode,
    OperationProgress, UserPreferences, WORKBOOK_PROJECTION_SCHEMA_VERSION, WorkbookGenerationPort,
    WorkbookGenerationReport, WorkbookLayout, WorkbookProjectionV4,
};
use suzushiro_content_digest::sha256_bytes;

use super::WorkbookProbeError;
use super::package::{read_bounded_workbook_bytes, write_new_file_bytes};
use super::projection_writer::{
    ProjectionWorkbookBuild, ProjectionWorkbookEvidence, build_projection_workbook_bytes,
    verify_projection_workbook, workbook_semantic_sha256,
};

const OUTPUT_DIRECTORY: &str = "data/workbooks";
const DEFAULT_NAME_PREFIX: &str = "fleet-";
const TEMPORARY_NAME_PREFIX: &str = ".azurlane-workbook.";
const MAX_GENERATED_WORKBOOK_BYTES: u64 = 512 * 1024 * 1024;

/// 一次操作交给生成端口的用户偏好。无法读取时保留摘要，生成阶段再返回。
pub(crate) enum OperationPreferences {
    Ready(UserPreferences),
    Unreadable(String),
}

/// 使用受控工具根目录物化并排他发布数据工作簿。
pub(crate) struct XlsxWorkbookGenerationPort {
    tool_root: ToolRoot,
    acquisition: Option<std::sync::Arc<super::ShipAcquisition>>,
    preferences: Option<OperationPreferences>,
}

impl XlsxWorkbookGenerationPort {
    /// 固定工具根目录，调用方只能选择输出文件名，不能改变输出目录。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self {
            tool_root,
            acquisition: None,
            preferences: None,
        }
    }

    pub(crate) fn with_acquisition(
        mut self,
        source: std::sync::Arc<super::ShipAcquisition>,
        preferences: OperationPreferences,
    ) -> Self {
        self.acquisition = Some(source);
        self.preferences = Some(preferences);
        self
    }

    fn operation_preferences(&self) -> Result<UserPreferences, AppError> {
        match self.preferences.as_ref() {
            Some(OperationPreferences::Ready(preferences)) => Ok(*preferences),
            Some(OperationPreferences::Unreadable(summary)) => Err(AppError::from_source(
                "workbook.generate.settings",
                AppErrorCode::SettingsInvalid,
                "读取获取方式设置失败",
                std::io::Error::other(summary.clone()),
            )),
            None => Err(AppError::from_source(
                "workbook.generate.settings",
                AppErrorCode::SettingsInvalid,
                "读取获取方式设置失败",
                std::io::Error::other("生成端口没有本次操作的用户偏好"),
            )),
        }
    }
}

impl WorkbookGenerationPort for XlsxWorkbookGenerationPort {
    fn generate_workbook(
        &self,
        requested_name: Option<&str>,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
    ) -> Result<WorkbookGenerationReport, AppError> {
        self.generate_workbook_with_progress(
            requested_name,
            layout,
            projection,
            &mut |_| {},
            &|| false,
        )
    }

    fn generate_workbook_with_progress(
        &self,
        requested_name: Option<&str>,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<WorkbookGenerationReport, AppError> {
        crate::application::check_generation_cancelled(is_cancelled)?;
        let mut acquisition_policy = None;
        let effective_layout = if self.acquisition.is_some() {
            let preferences = self.operation_preferences()?;
            acquisition_policy = Some(preferences.acquisition_update_policy);
            if !preferences.ship_acquisition_enabled {
                Some(layout.without_ship_acquisition().map_err(|source| {
                    AppError::from_source(
                        "workbook.generate.layout",
                        crate::application::AppErrorCode::WorkbookInvalid,
                        "应用获取方式设置失败",
                        source,
                    )
                })?)
            } else {
                None
            }
        } else {
            None
        };
        let layout = effective_layout.as_ref().unwrap_or(layout);
        let mut enrichment = match self.acquisition.as_ref() {
            Some(source) => source.enrich(
                layout,
                projection,
                acquisition_policy,
                progress,
                is_cancelled,
            )?,
            None => super::ship_acquisition::AcquisitionEnrichment {
                projection,
                values: BTreeMap::new(),
                summary: AcquisitionGenerationSummary::default(),
            },
        };
        if effective_layout.is_some() {
            enrichment.summary.state = AcquisitionGenerationState::Disabled;
        }
        let projection = enrichment.projection;
        progress(OperationProgress::stage("正在校验生成数据与输出目标"));
        crate::application::check_generation_cancelled(is_cancelled)?;
        let semantic_sha256 = workbook_semantic_sha256(layout, &projection).map_err(|error| {
            map_generation_error(
                self.tool_root.as_path().join(OUTPUT_DIRECTORY),
                WorkbookGenerationError::Workbook(error),
            )
        })?;
        let normalized_name = requested_name
            .map(normalize_requested_name)
            .transpose()
            .map_err(|error| {
                map_generation_error(self.tool_root.as_path().join(OUTPUT_DIRECTORY), error)
            })?;
        let numbered = crate::adapters::numbered_files::NumberedDirectory::open(
            &self.tool_root,
            Path::new(OUTPUT_DIRECTORY),
        )
        .map_err(|source| {
            map_generation_error(
                self.tool_root.as_path().join(OUTPUT_DIRECTORY),
                WorkbookGenerationError::Workbook(WorkbookProbeError::Io {
                    stage: "分配工作簿编号",
                    path: self.tool_root.as_path().join(OUTPUT_DIRECTORY),
                    source,
                }),
            )
        })?;
        let output_relative = output_relative_path(
            &self.tool_root,
            &numbered,
            normalized_name.as_deref(),
            &semantic_sha256,
            layout,
        )
        .map_err(|error| {
            map_generation_error(self.tool_root.as_path().join(OUTPUT_DIRECTORY), error)
        })?;
        let output_path = self.tool_root.as_path().join(&output_relative);
        perform_generation(
            &self.tool_root,
            &output_relative,
            layout,
            &projection,
            &semantic_sha256,
            progress,
            is_cancelled,
        )
        .map(|report| report.with_ship_acquisition(enrichment.values, enrichment.summary))
        .map_err(|error| map_generation_error(output_path, error))
    }

    #[cfg(test)]
    fn ship_acquisition_enabled_for_test(&self) -> Result<bool, String> {
        self.operation_preferences()
            .map(|preferences| preferences.ship_acquisition_enabled)
            .map_err(|error| {
                std::error::Error::source(&error)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| error.to_string())
            })
    }
}

/// 复用完全一致的既有目标；否则经临时文件验证后排他发布一个新目标。
fn perform_generation(
    tool_root: &ToolRoot,
    output_relative: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    semantic_sha256: &str,
    progress: &mut dyn FnMut(OperationProgress),
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<WorkbookGenerationReport, WorkbookGenerationError> {
    if is_cancelled() {
        return Err(WorkbookGenerationError::Cancelled);
    }
    tool_root
        .ensure_directory(Path::new(OUTPUT_DIRECTORY))
        .map_err(|source| WorkbookGenerationError::ToolRoot {
            operation: "建立数据工作簿输出目录",
            source,
        })?;

    match tool_root.prepare_new_file(output_relative) {
        Ok(_) => {}
        Err(ToolRootError::PathConflict { .. }) => {
            progress(OperationProgress::stage("正在重读校验已有工作簿"));
            if is_cancelled() {
                return Err(WorkbookGenerationError::Cancelled);
            }
            return reuse_existing(
                tool_root,
                output_relative,
                layout,
                projection,
                semantic_sha256,
            );
        }
        Err(source) => {
            return Err(WorkbookGenerationError::ToolRoot {
                operation: "检查数据工作簿输出目标",
                source,
            });
        }
    }

    let generated_at_unix_millis = current_unix_millis()?;
    let output_path = tool_root.as_path().join(output_relative);
    let build = super::projection_writer::build_projection_workbook_bytes_with_progress(
        &output_path,
        layout,
        projection,
        generated_at_unix_millis,
        progress,
    )
    .map_err(WorkbookGenerationError::Workbook)?;
    if is_cancelled() {
        return Err(WorkbookGenerationError::Cancelled);
    }
    if build.workbook_semantic_sha256 != semantic_sha256 {
        return Err(WorkbookGenerationError::SemanticDigestMismatch {
            expected: semantic_sha256.to_owned(),
            actual: build.workbook_semantic_sha256,
        });
    }
    let expected_package_sha256 = sha256_bytes(&build.bytes);
    let temporary_relative = temporary_workbook_path()?;
    let temporary_path = tool_root
        .prepare_new_file(&temporary_relative)
        .map_err(|source| WorkbookGenerationError::ToolRoot {
            operation: "准备数据工作簿临时文件",
            source,
        })?;
    progress(OperationProgress::stage("正在写出工作簿文件"));
    if is_cancelled() {
        return Err(WorkbookGenerationError::Cancelled);
    }
    let temporary_file = write_new_file_bytes(&temporary_path, &build.bytes)
        .map_err(|source| WorkbookGenerationError::Workbook(source.into()))?;
    progress(OperationProgress::stage("正在重读校验并发布工作簿"));
    let operation = verify_and_publish(
        tool_root,
        &temporary_file,
        &temporary_relative,
        &temporary_path,
        output_relative,
        &expected_package_sha256,
        layout,
        projection,
        is_cancelled,
    );
    match operation {
        Ok(PublishOutcome::Created) => Ok(report_from_build(
            output_relative,
            build,
            layout,
            projection,
            expected_package_sha256,
        )),
        Ok(PublishOutcome::ConcurrentIdentical {
            evidence,
            package_sha256,
        }) => Ok(report_from_evidence(
            output_relative,
            true,
            evidence,
            layout,
            projection,
            semantic_sha256,
            package_sha256,
        )),
        Err(operation) => {
            match tool_root.remove_file_if_exists(&temporary_relative, Some(&temporary_file)) {
                Ok(_) => Err(operation),
                Err(source) => Err(WorkbookGenerationError::Cleanup {
                    operation: Box::new(operation),
                    source,
                }),
            }
        }
    }
}

enum PublishOutcome {
    Created,
    ConcurrentIdentical {
        evidence: ProjectionWorkbookEvidence,
        package_sha256: String,
    },
}

/// 重读临时包并核对已校验缓冲区的字节摘要，再以排他方式发布最终名称。
#[allow(clippy::too_many_arguments)]
fn verify_and_publish(
    tool_root: &ToolRoot,
    temporary_file: &std::fs::File,
    temporary_relative: &Path,
    temporary_path: &Path,
    output_relative: &Path,
    expected_package_sha256: &str,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<PublishOutcome, WorkbookGenerationError> {
    if is_cancelled() {
        return Err(WorkbookGenerationError::Cancelled);
    }
    let temporary_bytes = read_bounded_workbook_bytes(
        temporary_path,
        MAX_GENERATED_WORKBOOK_BYTES,
        "数据工作簿临时文件",
    )
    .map_err(|source| WorkbookGenerationError::Workbook(source.into()))?;
    let actual_package_sha256 = sha256_bytes(&temporary_bytes);
    if actual_package_sha256 != expected_package_sha256 {
        return Err(WorkbookGenerationError::PackageDigestMismatch {
            expected: expected_package_sha256.to_owned(),
            actual: actual_package_sha256,
        });
    }
    if is_cancelled() {
        return Err(WorkbookGenerationError::Cancelled);
    }
    // 发布是提交点，此后保留已发布结果，不再转换为取消。
    match tool_root.rename_new_file(temporary_file, temporary_relative, output_relative) {
        Ok(_) => Ok(PublishOutcome::Created),
        Err(ToolRootError::PathConflict { .. }) => {
            let (evidence, package_sha256) =
                read_and_verify_existing(tool_root, output_relative, layout, projection)?;
            tool_root
                .remove_file_if_exists(temporary_relative, Some(temporary_file))
                .map_err(|source| WorkbookGenerationError::ToolRoot {
                    operation: "清理并发发布临时文件",
                    source,
                })?;
            Ok(PublishOutcome::ConcurrentIdentical {
                evidence,
                package_sha256,
            })
        }
        Err(source) => Err(WorkbookGenerationError::ToolRoot {
            operation: "排他发布数据工作簿",
            source,
        }),
    }
}

/// 将已经通过规范包核验的同语义目标转换为幂等生成报告。
fn reuse_existing(
    tool_root: &ToolRoot,
    output_relative: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    semantic_sha256: &str,
) -> Result<WorkbookGenerationReport, WorkbookGenerationError> {
    let (evidence, package_sha256) =
        read_and_verify_existing(tool_root, output_relative, layout, projection)?;
    Ok(report_from_evidence(
        output_relative,
        true,
        evidence,
        layout,
        projection,
        semantic_sha256,
        package_sha256,
    ))
}

/// 既核对既有文件的语义，也要求其字节等于相同生成时间下的规范输出。
fn read_and_verify_existing(
    tool_root: &ToolRoot,
    output_relative: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<(ProjectionWorkbookEvidence, String), WorkbookGenerationError> {
    let path = tool_root.existing_file(output_relative).map_err(|source| {
        WorkbookGenerationError::ToolRoot {
            operation: "读取既有数据工作簿",
            source,
        }
    })?;
    let bytes = read_bounded_workbook_bytes(&path, MAX_GENERATED_WORKBOOK_BYTES, "既有数据工作簿")
        .map_err(WorkbookProbeError::from)
        .map_err(|source| match source {
            WorkbookProbeError::Io { .. } | WorkbookProbeError::WorkbookLocked { .. } => {
                WorkbookGenerationError::Workbook(source)
            }
            source => WorkbookGenerationError::ExistingContentConflict {
                path: path.clone(),
                source,
            },
        })?;
    let evidence =
        verify_projection_workbook(&path, &bytes, layout, projection).map_err(|source| {
            WorkbookGenerationError::ExistingContentConflict {
                path: path.clone(),
                source,
            }
        })?;
    let expected = build_projection_workbook_bytes(
        &path,
        layout,
        projection,
        evidence.generated_at_unix_millis,
    )
    .map_err(WorkbookGenerationError::Workbook)?;
    if expected.bytes != bytes {
        return Err(WorkbookGenerationError::ExistingContentConflict {
            path,
            source: WorkbookProbeError::WorkbookBuild {
                message: "既有工作簿不是当前布局和投影的规范生成包".to_owned(),
            },
        });
    }
    Ok((evidence, sha256_bytes(&bytes)))
}

/// 将可选用户文件名收敛为固定输出目录下的相对路径。
fn output_relative_path(
    root: &ToolRoot,
    numbered: &crate::adapters::numbered_files::NumberedDirectory,
    requested_name: Option<&str>,
    semantic_sha256: &str,
    layout: &WorkbookLayout,
) -> Result<PathBuf, WorkbookGenerationError> {
    if let Some(name) = requested_name {
        return Ok(Path::new(OUTPUT_DIRECTORY).join(name));
    }
    // 从工作簿内的完整摘要筛选候选；真正复用仍必须通过整包和全部单元格校验。
    let files = root
        .list_workbook_files()
        .map_err(|source| WorkbookGenerationError::ToolRoot {
            operation: "读取已生成工作簿",
            source,
        })?;
    for (name, _) in files {
        let Some(number) = name
            .strip_prefix(DEFAULT_NAME_PREFIX)
            .and_then(|name| name.strip_suffix(".xlsx"))
        else {
            continue;
        };
        if crate::adapters::numbered_files::numbered_name(&format!("{number}-fleet.xlsx")).is_none()
        {
            continue;
        }
        let relative = Path::new(OUTPUT_DIRECTORY).join(name);
        let path =
            root.existing_file(&relative)
                .map_err(|source| WorkbookGenerationError::ToolRoot {
                    operation: "读取工作簿摘要",
                    source,
                })?;
        if super::reader::read_workbook_hash(&path, layout)
            .map_err(WorkbookGenerationError::Workbook)?
            .as_deref()
            == Some(semantic_sha256)
        {
            return Ok(relative);
        }
    }
    numbered
        .next_path("fleet", "xlsx", false)
        .map_err(|source| {
            WorkbookGenerationError::Workbook(WorkbookProbeError::Io {
                stage: "分配工作簿编号",
                path: root.as_path().join(OUTPUT_DIRECTORY),
                source,
            })
        })
}

/// 拒绝路径语义、平台保留名和工具保留名，只接受可移植的单个 XLSX 文件名。
fn normalize_requested_name(name: &str) -> Result<String, WorkbookGenerationError> {
    if name.is_empty() || name.trim() != name {
        return Err(WorkbookGenerationError::InvalidRequestedName {
            name: name.to_owned(),
            message: "文件名不能为空，也不能包含首尾空白".to_owned(),
        });
    }
    if name.contains(['/', '\\', '<', '>', '"', '|', '?', '*'])
        || name.chars().any(char::is_control)
    {
        return Err(WorkbookGenerationError::InvalidRequestedName {
            name: name.to_owned(),
            message: "文件名包含不可移植的路径分隔符、控制字符或 Windows 禁用字符".to_owned(),
        });
    }
    let path = Path::new(name);
    if path.components().count() != 1
        || path.file_name().and_then(|value| value.to_str()) != Some(name)
    {
        return Err(WorkbookGenerationError::InvalidRequestedName {
            name: name.to_owned(),
            message: "只能提供 data/workbooks 内的单个文件名".to_owned(),
        });
    }
    let filename = match path.extension().and_then(|value| value.to_str()) {
        None => format!("{name}.xlsx"),
        Some(extension) if extension.eq_ignore_ascii_case("xlsx") => name.to_owned(),
        Some(_) => {
            return Err(WorkbookGenerationError::InvalidRequestedName {
                name: name.to_owned(),
                message: "数据工作簿必须使用 .xlsx 扩展名".to_owned(),
            });
        }
    };
    let lowercase = filename.to_ascii_lowercase();
    if filename.encode_utf16().count() > 255 {
        return Err(WorkbookGenerationError::InvalidRequestedName {
            name: name.to_owned(),
            message: "文件名超过 255 个 UTF-16 单元".to_owned(),
        });
    }
    if filename.starts_with('.')
        || matches!(
            lowercase.as_str(),
            "layout-preview.xlsx" | "workbook-layout.updated.xlsx" | "workbook-layout.xlsx"
        )
    {
        return Err(WorkbookGenerationError::InvalidRequestedName {
            name: name.to_owned(),
            message: "文件名与工具保留工作簿或临时文件命名冲突".to_owned(),
        });
    }
    crate::adapters::tool_root::validate_portable_component(
        Path::new(&filename),
        std::ffi::OsStr::new(&filename),
    )
    .map_err(|error| WorkbookGenerationError::InvalidRequestedName {
        name: name.to_owned(),
        message: error.to_string(),
    })?;
    Ok(filename)
}

/// 生成同目录排他写入使用的进程和随机数组合临时文件名。
fn temporary_workbook_path() -> Result<PathBuf, WorkbookGenerationError> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random)
        .map_err(WorkbookProbeError::RandomSource)
        .map_err(WorkbookGenerationError::Workbook)?;
    Ok(PathBuf::from(OUTPUT_DIRECTORY).join(format!(
        "{TEMPORARY_NAME_PREFIX}{}.{:016x}.tmp.xlsx",
        std::process::id(),
        u64::from_le_bytes(random)
    )))
}

/// 读取首次生成时间并保证能够写入报告和 schema 的 i64 毫秒字段。
fn current_unix_millis() -> Result<i64, WorkbookGenerationError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(WorkbookGenerationError::Clock)?;
    i64::try_from(duration.as_millis()).map_err(|_| WorkbookGenerationError::ClockOverflow)
}

/// 直接使用本轮构建证据形成新建文件报告，避免重新推导写入计数。
fn report_from_build(
    output_relative: &Path,
    build: ProjectionWorkbookBuild,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    package_sha256: String,
) -> WorkbookGenerationReport {
    WorkbookGenerationReport::new(
        output_relative.to_string_lossy().replace('\\', "/"),
        false,
        build.generated_at_unix_millis,
        layout.schema_version(),
        WORKBOOK_PROJECTION_SCHEMA_VERSION,
        build.generated_sheets,
        build.hidden_sheets,
        build.omitted_sheets,
        build.generated_fields,
        build.hidden_fields,
        build.omitted_fields,
        build.projected_rows,
        build.dictionary_rows,
        build.schema_rows,
        layout.content_sha256().to_owned(),
        projection.content_sha256().to_owned(),
        projection.source().game_state_content_sha256().to_owned(),
        build.workbook_semantic_sha256,
        package_sha256,
    )
    .with_read_scope(projection.source().read_scope())
}

/// 使用重载证据和当前布局重建幂等复用报告中的稳定计数。
#[allow(clippy::too_many_arguments)]
fn report_from_evidence(
    output_relative: &Path,
    reused_existing: bool,
    evidence: ProjectionWorkbookEvidence,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    semantic_sha256: &str,
    package_sha256: String,
) -> WorkbookGenerationReport {
    let outputs = super::projection_writer::technology_views::output_sheets(layout, projection)
        .expect("已验证工作簿的分类布局有效");
    let generated_sheets: Vec<_> = outputs
        .iter()
        .map(|output| output.layout.as_ref())
        .collect();
    let generated_fields = generated_sheets
        .iter()
        .map(|sheet| layout.generated_fields_for_sheet(sheet.stable_key()).len())
        .sum();
    let configured_fields = layout
        .fields()
        .iter()
        .filter(|field| {
            field.generation() != crate::application::LayoutGenerationMode::Omitted
                && layout.sheets().iter().any(|sheet| {
                    sheet.stable_key() == field.sheet_key()
                        && sheet.generation() != crate::application::LayoutGenerationMode::Omitted
                })
        })
        .count();
    WorkbookGenerationReport::new(
        output_relative.to_string_lossy().replace('\\', "/"),
        reused_existing,
        evidence.generated_at_unix_millis,
        layout.schema_version(),
        WORKBOOK_PROJECTION_SCHEMA_VERSION,
        generated_sheets.len(),
        generated_sheets
            .iter()
            .filter(|sheet| sheet.generation() == crate::application::LayoutGenerationMode::Hidden)
            .count(),
        layout
            .sheets()
            .iter()
            .filter(|sheet| sheet.generation() == crate::application::LayoutGenerationMode::Omitted)
            .count(),
        generated_fields,
        generated_sheets
            .iter()
            .flat_map(|sheet| layout.generated_fields_for_sheet(sheet.stable_key()))
            .filter(|field| field.generation() == crate::application::LayoutGenerationMode::Hidden)
            .count(),
        layout.fields().len() - configured_fields,
        evidence.projected_rows,
        evidence.dictionary_rows,
        evidence.schema_rows,
        layout.content_sha256().to_owned(),
        projection.content_sha256().to_owned(),
        projection.source().game_state_content_sha256().to_owned(),
        semantic_sha256.to_owned(),
        package_sha256,
    )
    .with_read_scope(projection.source().read_scope())
}

/// 将内部失败分类映射为稳定应用错误，并附带最终目标路径。
fn map_generation_error(output_path: PathBuf, error: WorkbookGenerationError) -> AppError {
    let code = error.app_error_code();
    AppError::from_source("workbook.generate", code, error.user_message(), error)
        .with_context("output_path", output_path.to_string_lossy())
}

#[derive(Debug, Error)]
enum WorkbookGenerationError {
    #[error("同步并生成已取消，未发布工作簿")]
    Cancelled,
    #[error("请求的数据工作簿文件名 {name:?} 无效: {message}")]
    InvalidRequestedName { name: String, message: String },
    #[error("读取系统生成时间失败: {0}")]
    Clock(#[source] std::time::SystemTimeError),
    #[error("系统生成时间超出 i64 Unix 毫秒范围")]
    ClockOverflow,
    #[error("工作簿语义摘要不一致: expected={expected}, actual={actual}")]
    SemanticDigestMismatch { expected: String, actual: String },
    #[error("工作簿临时文件包摘要不一致: expected={expected}, actual={actual}")]
    PackageDigestMismatch { expected: String, actual: String },
    #[error("既有目标 {path} 与当前布局和投影不一致: {source}")]
    ExistingContentConflict {
        path: PathBuf,
        #[source]
        source: WorkbookProbeError,
    },
    #[error("{operation}失败: {source}")]
    ToolRoot {
        operation: &'static str,
        #[source]
        source: ToolRootError,
    },
    #[error("数据工作簿处理失败: {0}")]
    Workbook(#[source] WorkbookProbeError),
    #[error("原操作失败且临时文件清理失败: operation={operation}; cleanup={source}")]
    Cleanup {
        operation: Box<WorkbookGenerationError>,
        #[source]
        source: ToolRootError,
    },
}

impl WorkbookGenerationError {
    /// 按用户可处理性区分设置、占用、产物和运行环境错误。
    fn app_error_code(&self) -> AppErrorCode {
        match self {
            Self::Cancelled => AppErrorCode::OperationCancelled,
            Self::InvalidRequestedName { .. } => AppErrorCode::SettingsInvalid,
            Self::Clock(_) | Self::ClockOverflow | Self::ToolRoot { .. } | Self::Cleanup { .. } => {
                AppErrorCode::ApplicationInitializationFailed
            }
            Self::Workbook(
                WorkbookProbeError::InvalidPath { .. }
                | WorkbookProbeError::Io { .. }
                | WorkbookProbeError::RandomSource(_)
                | WorkbookProbeError::CleanupFailed { .. },
            ) => AppErrorCode::ApplicationInitializationFailed,
            Self::Workbook(WorkbookProbeError::WorkbookLocked { .. }) => {
                AppErrorCode::WorkbookLocked
            }
            Self::SemanticDigestMismatch { .. }
            | Self::PackageDigestMismatch { .. }
            | Self::ExistingContentConflict { .. }
            | Self::Workbook(_) => AppErrorCode::WorkbookInvalid,
        }
    }

    /// 返回不泄漏内部路径细节的稳定用户提示。
    fn user_message(&self) -> &'static str {
        match self {
            Self::Cancelled => "同步并生成已取消，未发布工作簿",
            Self::InvalidRequestedName { .. } => "数据工作簿文件名不符合固定目录和扩展名规则",
            Self::ExistingContentConflict { .. } => "同名文件已经存在且内容不同，未覆盖既有工作簿",
            Self::Cleanup { .. } | Self::Workbook(WorkbookProbeError::CleanupFailed { .. }) => {
                "数据工作簿生成失败后未能清理临时文件"
            }
            Self::Workbook(WorkbookProbeError::WorkbookLocked { .. }) => {
                "数据工作簿正在被占用，未发布输出"
            }
            Self::Clock(_)
            | Self::ClockOverflow
            | Self::ToolRoot { .. }
            | Self::Workbook(
                WorkbookProbeError::InvalidPath { .. }
                | WorkbookProbeError::Io { .. }
                | WorkbookProbeError::RandomSource(_),
            ) => "数据工作簿输出路径或生成环境不可用",
            Self::SemanticDigestMismatch { .. }
            | Self::PackageDigestMismatch { .. }
            | Self::Workbook(_) => "数据工作簿产物未通过完整验证，未发布输出",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state;
    use crate::adapters::tool_root::ToolRoot;
    use crate::adapters::workbook::load_workbook_layout;
    use crate::application::{
        AppErrorCode, WorkbookGenerationPort, WorkbookLayout, WorkbookProjectionV4,
        project_game_state_to_workbook,
    };
    use suzushiro_content_digest::sha256_bytes;

    use super::{
        TEMPORARY_NAME_PREFIX, WorkbookGenerationError, WorkbookProbeError,
        XlsxWorkbookGenerationPort,
    };

    #[test]
    fn cancellation_before_publish_removes_temporary_workbook() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for stage in [
            "正在校验生成数据与输出目标",
            "正在写出工作簿文件",
            "正在重读校验并发布工作簿",
        ] {
            let fixture = TestDirectory::new("cancel-before-publish");
            fs::create_dir_all(fixture.root.join("data/workbooks")).unwrap();
            let port = XlsxWorkbookGenerationPort::new(ToolRoot::open(&fixture.root).unwrap());
            let cancelled = AtomicBool::new(false);
            let error = port
                .generate_workbook_with_progress(
                    Some("cancelled.xlsx"),
                    &default_layout(),
                    project_game_state_to_workbook(&golden_game_state()).unwrap(),
                    &mut |event| {
                        if event.message == stage {
                            cancelled.store(true, Ordering::Release);
                        }
                    },
                    &|| cancelled.load(Ordering::Acquire),
                )
                .unwrap_err();
            assert!(error.is_cancelled(), "{stage}: {error:?}");
            assert!(!fixture.root.join("data/workbooks/cancelled.xlsx").exists());
            assert_no_temporary_files(&fixture.root);
        }
    }

    #[test]
    fn publishes_once_and_reuses_the_verified_semantic_target() {
        let fixture = TestDirectory::new("idempotent");
        let port = XlsxWorkbookGenerationPort::new(ToolRoot::open(&fixture.root).unwrap());
        let layout = default_layout();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();

        let first = port
            .generate_workbook(Some("my-workbook"), &layout, projection.clone())
            .unwrap();
        let second = port
            .generate_workbook(Some("my-workbook.xlsx"), &layout, projection.clone())
            .unwrap();

        assert!(!first.reused_existing());
        assert!(second.reused_existing());
        assert_eq!(first.output_path(), "data/workbooks/my-workbook.xlsx");
        assert_eq!(first.output_path(), second.output_path());
        assert_eq!(
            first.generated_at_unix_millis(),
            second.generated_at_unix_millis()
        );
        assert_eq!(
            first.output_package_sha256(),
            second.output_package_sha256()
        );
        let published_bytes = fs::read(fixture.root.join(first.output_path())).unwrap();
        assert_eq!(
            first.output_package_sha256(),
            sha256_bytes(&published_bytes)
        );
        assert_no_temporary_files(&fixture.root);
    }

    #[test]
    fn refuses_to_replace_an_existing_different_target() {
        let fixture = TestDirectory::new("conflict");
        let output_directory = fixture.root.join("data/workbooks");
        fs::create_dir_all(&output_directory).unwrap();
        let output = output_directory.join("occupied.xlsx");
        fs::write(&output, b"existing-user-content").unwrap();
        let port = XlsxWorkbookGenerationPort::new(ToolRoot::open(&fixture.root).unwrap());
        let layout = default_layout();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();

        let error = port
            .generate_workbook(Some("occupied.xlsx"), &layout, projection.clone())
            .unwrap_err();

        assert_eq!(error.code(), AppErrorCode::WorkbookInvalid);
        assert_eq!(fs::read(output).unwrap(), b"existing-user-content");
        assert_no_temporary_files(&fixture.root);
    }

    #[test]
    fn default_name_is_numbered_and_identical_content_is_reused() {
        let fixture = TestDirectory::new("default-name");
        let port = XlsxWorkbookGenerationPort::new(ToolRoot::open(&fixture.root).unwrap());
        let layout = default_layout();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();

        let report = port
            .generate_workbook(None, &layout, projection.clone())
            .unwrap();

        assert_eq!(report.output_path(), "data/workbooks/fleet-001.xlsx");
        let repeated = port
            .generate_workbook(None, &layout, projection.clone())
            .unwrap();
        assert!(repeated.reused_existing());
        assert_eq!(repeated.output_path(), report.output_path());
        let changed = crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_unowned_config();
        let changed_projection = project_game_state_to_workbook(&changed).unwrap();
        let next = port
            .generate_workbook(None, &layout, changed_projection)
            .unwrap();
        assert_eq!(next.output_path(), "data/workbooks/fleet-002.xlsx");
        assert!(!next.reused_existing());
        assert_no_temporary_files(&fixture.root);
    }

    #[test]
    fn refuses_a_changed_package_even_when_its_core_cells_still_match() {
        let fixture = TestDirectory::new("changed-package");
        let port = XlsxWorkbookGenerationPort::new(ToolRoot::open(&fixture.root).unwrap());
        let layout = default_layout();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
        let first = port
            .generate_workbook(None, &layout, projection.clone())
            .unwrap();
        let output = fixture.root.join(first.output_path());
        OpenOptions::new()
            .append(true)
            .open(&output)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        let changed = fs::read(&output).unwrap();

        let error = port
            .generate_workbook(None, &layout, projection.clone())
            .unwrap_err();

        assert_eq!(error.code(), AppErrorCode::WorkbookInvalid);
        assert_eq!(fs::read(output).unwrap(), changed);
        assert_no_temporary_files(&fixture.root);
    }

    #[test]
    fn rejects_nonportable_or_reserved_requested_names_before_writing() {
        let fixture = TestDirectory::new("invalid-name");
        let port = XlsxWorkbookGenerationPort::new(ToolRoot::open(&fixture.root).unwrap());
        let layout = default_layout();
        let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();

        for name in [
            "../outside.xlsx",
            "nested/book.xlsx",
            "nested\\book.xlsx",
            "bad?.xlsx",
            "bad:stream.xlsx",
            "CON.xlsx",
            "com1.xlsx",
            "LPT9.xlsx",
            "layout-preview",
        ] {
            let error = port
                .generate_workbook(Some(name), &layout, projection.clone())
                .unwrap_err();
            assert_eq!(error.code(), AppErrorCode::SettingsInvalid, "{name}");
        }
        assert!(!fixture.root.join("data").exists());
    }

    #[test]
    fn classifies_output_cleanup_failures_separately_from_invalid_workbooks() {
        let error = WorkbookGenerationError::Workbook(WorkbookProbeError::CleanupFailed {
            path: PathBuf::from("data/workbooks/.temporary.xlsx"),
            operation: "写入失败".to_owned(),
            source: std::io::Error::other("测试清理失败"),
        });

        assert_eq!(
            error.app_error_code(),
            AppErrorCode::ApplicationInitializationFailed
        );
        assert_eq!(error.user_message(), "数据工作簿生成失败后未能清理临时文件");
    }

    fn default_layout() -> WorkbookLayout {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        load_workbook_layout(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
            &registry,
        )
        .unwrap()
    }

    fn assert_no_temporary_files(root: &Path) {
        let output = root.join("data/workbooks");
        let names: Vec<String> = fs::read_dir(output)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .all(|name| !name.starts_with(TEMPORARY_NAME_PREFIX))
        );
    }

    struct TestDirectory {
        parent: PathBuf,
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let parent = home
                .join("suzushiro/scratch/azlw-workbook-generation-tests")
                .join(format!("{label}-{}", unique_suffix()));
            let root = parent.join("tool-root");
            fs::create_dir_all(&root).expect("应建立独立工具根样本");
            Self { parent, root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.parent) {
                eprintln!("清理工作簿生成测试目录失败: {error}");
            }
        }
    }

    fn unique_suffix() -> String {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).expect("测试需要操作系统随机源");
        format!("{}-{:032x}", std::process::id(), u128::from_le_bytes(bytes))
    }
}

//! 在工具根固定路径写出、重载并原子刷新布局预览工作簿。

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::application::{
    AppError, AppErrorCode, LayoutPreviewPort, LayoutPreviewReport,
    WORKBOOK_PROJECTION_SCHEMA_VERSION, WorkbookLayout,
};
use suzushiro_content_digest::sha256_bytes;

use super::super::WorkbookProbeError;
use super::super::atomic::replace_validated_workbook;
use super::super::package::write_new_file_bytes;

mod render;
use render::{
    LayoutPreviewBuild, build_layout_preview_workbook_bytes, verify_layout_preview_workbook,
};

const OUTPUT_DIRECTORY: &str = "data/workbooks";
const OUTPUT_PREVIEW_PATH: &str = "data/workbooks/layout-preview.xlsx";

/// 使用工具根目录完成固定预览路径的安全刷新。
pub(crate) struct XlsxLayoutPreviewPort {
    tool_root: ToolRoot,
}

impl XlsxLayoutPreviewPort {
    /// 固定受控工具根目录，预览命令不接受外部输出路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl LayoutPreviewPort for XlsxLayoutPreviewPort {
    fn preview_layout(&self, layout: &WorkbookLayout) -> Result<LayoutPreviewReport, AppError> {
        perform_preview(&self.tool_root, layout).map_err(|error| {
            map_preview_error(self.tool_root.as_path().join(OUTPUT_PREVIEW_PATH), error)
        })
    }
}

fn perform_preview(
    tool_root: &ToolRoot,
    layout: &WorkbookLayout,
) -> Result<LayoutPreviewReport, LayoutPreviewError> {
    tool_root
        .ensure_directory(Path::new(OUTPUT_DIRECTORY))
        .map_err(|source| LayoutPreviewError::ToolRoot {
            operation: "建立布局预览输出目录",
            source,
        })?;
    let output_path = tool_root.as_path().join(OUTPUT_PREVIEW_PATH);
    let build = build_layout_preview_workbook_bytes(&output_path, layout)
        .map_err(LayoutPreviewError::Workbook)?;
    let expected_package_sha256 = sha256_bytes(&build.bytes);
    let temporary_relative = temporary_preview_path().map_err(LayoutPreviewError::Workbook)?;
    let temporary_path = tool_root
        .prepare_new_file(&temporary_relative)
        .map_err(|source| LayoutPreviewError::ToolRoot {
            operation: "准备布局预览临时文件",
            source,
        })?;
    let temporary_file = write_new_file_bytes(&temporary_path, &build.bytes)
        .map_err(|source| LayoutPreviewError::Workbook(source.into()))?;

    let publish_result = verify_and_replace(
        &temporary_path,
        &output_path,
        &expected_package_sha256,
        layout,
    );
    if let Err(operation) = publish_result {
        return match tool_root.remove_file_if_exists(&temporary_relative, Some(&temporary_file)) {
            Ok(_) => Err(operation),
            Err(source) => Err(LayoutPreviewError::Cleanup {
                operation: Box::new(operation),
                source,
            }),
        };
    }

    Ok(report_from_build(build, layout, expected_package_sha256))
}

fn verify_and_replace(
    temporary_path: &Path,
    output_path: &Path,
    expected_package_sha256: &str,
    layout: &WorkbookLayout,
) -> Result<(), LayoutPreviewError> {
    let temporary_bytes = std::fs::read(temporary_path).map_err(|source| {
        LayoutPreviewError::Workbook(WorkbookProbeError::Io {
            stage: "重读布局预览临时文件",
            path: temporary_path.to_path_buf(),
            source,
        })
    })?;
    let actual_package_sha256 = sha256_bytes(&temporary_bytes);
    if actual_package_sha256 != expected_package_sha256 {
        return Err(LayoutPreviewError::PackageDigestMismatch {
            expected: expected_package_sha256.to_owned(),
            actual: actual_package_sha256,
        });
    }
    verify_layout_preview_workbook(temporary_path, &temporary_bytes, layout)
        .map_err(LayoutPreviewError::Workbook)?;
    replace_validated_workbook(temporary_path, output_path)
        .map_err(|source| LayoutPreviewError::Workbook(source.into()))
}

fn report_from_build(
    build: LayoutPreviewBuild,
    layout: &WorkbookLayout,
    output_package_sha256: String,
) -> LayoutPreviewReport {
    LayoutPreviewReport::new(
        OUTPUT_PREVIEW_PATH.to_owned(),
        layout.schema_version(),
        WORKBOOK_PROJECTION_SCHEMA_VERSION,
        build.generated_sheets,
        build.hidden_sheets,
        build.omitted_sheets,
        build.generated_fields,
        build.hidden_fields,
        build.omitted_fields,
        build.example_rows,
        layout.content_sha256().to_owned(),
        output_package_sha256,
    )
}

fn temporary_preview_path() -> Result<PathBuf, WorkbookProbeError> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(WorkbookProbeError::RandomSource)?;
    Ok(PathBuf::from(OUTPUT_DIRECTORY).join(format!(
        ".layout-preview.{}.{:016x}.tmp.xlsx",
        std::process::id(),
        u64::from_le_bytes(random)
    )))
}

fn map_preview_error(output_path: PathBuf, error: LayoutPreviewError) -> AppError {
    let code = if error.is_workbook_locked() {
        AppErrorCode::WorkbookLocked
    } else if error.is_output_path_failure() {
        AppErrorCode::ApplicationInitializationFailed
    } else {
        AppErrorCode::WorkbookInvalid
    };
    let message = error.user_message();
    AppError::from_source("workbook.layout.preview", code, message, error)
        .with_context("output_path", output_path.to_string_lossy())
}

#[derive(Debug, Error)]
enum LayoutPreviewError {
    #[error("工作簿预览处理失败: {0}")]
    Workbook(#[source] WorkbookProbeError),
    #[error("布局预览临时文件摘要不一致: expected={expected}, actual={actual}")]
    PackageDigestMismatch { expected: String, actual: String },
    #[error("{operation}失败: {source}")]
    ToolRoot {
        operation: &'static str,
        #[source]
        source: ToolRootError,
    },
    #[error("原操作失败且临时文件清理失败: operation={operation}; cleanup={source}")]
    Cleanup {
        operation: Box<LayoutPreviewError>,
        #[source]
        source: ToolRootError,
    },
}

impl LayoutPreviewError {
    fn is_workbook_locked(&self) -> bool {
        match self {
            Self::Workbook(WorkbookProbeError::WorkbookLocked { .. }) => true,
            Self::Cleanup { operation, .. } => operation.is_workbook_locked(),
            _ => false,
        }
    }

    fn is_output_path_failure(&self) -> bool {
        match self {
            Self::ToolRoot { .. } | Self::Cleanup { .. } => true,
            Self::Workbook(
                WorkbookProbeError::InvalidPath { .. }
                | WorkbookProbeError::Io { .. }
                | WorkbookProbeError::RandomSource(_)
                | WorkbookProbeError::CleanupFailed { .. },
            ) => true,
            Self::Workbook(_) | Self::PackageDigestMismatch { .. } => false,
        }
    }

    fn user_message(&self) -> &'static str {
        match self {
            Self::Workbook(WorkbookProbeError::WorkbookLocked { .. }) => {
                "布局预览工作簿正在被占用，旧文件保持不变"
            }
            Self::Cleanup { .. } => "布局预览失败后未能清理临时文件",
            Self::ToolRoot { .. }
            | Self::Workbook(
                WorkbookProbeError::InvalidPath { .. }
                | WorkbookProbeError::Io { .. }
                | WorkbookProbeError::RandomSource(_)
                | WorkbookProbeError::CleanupFailed { .. },
            ) => "布局预览输出路径不可用，旧文件保持不变",
            Self::Workbook(_) | Self::PackageDigestMismatch { .. } => {
                "布局预览产物未通过完整验证，旧文件保持不变"
            }
        }
    }
}

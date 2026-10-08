//! XLSX 工作簿建立、读取、检查和保真写回适配器。

mod atomic;
mod backup;
mod catalog;
mod document;
pub(crate) use document::WorkbookDocuments;
mod editor;
mod equipment_display;
mod execution_results;
mod generation;
mod layout;
mod opener;
pub(crate) use suzushiro_xlsx_toolkit::package;
pub(in crate::adapters::workbook) mod projection_writer;
mod reader;
mod reference;
mod rendering;
mod sheet_parts;
mod ship_acquisition;
pub(crate) mod ship_wiki;
pub(crate) use ship_acquisition::ShipAcquisition;
pub(crate) use suzushiro_xlsx_toolkit::worksheet_primitives;

use std::path::PathBuf;

use thiserror::Error;

use crate::adapters::tool_root::ToolRoot;
use crate::application::{UserPreferences, WorkbookGenerationPort};

pub use atomic::{AtomicTextCellEditEvidence, edit_text_cell_atomically};
pub(crate) use backup::XlsxWorkbookBackupPort;
pub(crate) use catalog::XlsxWorkbookCatalogPort;
pub use editor::{PackagePreservationEvidence, TextCellEditEvidence, edit_text_cell_to_new_file};
pub(crate) use execution_results::XlsxExecutionResultsPort;
pub use execution_results::writer::{
    AtomicExecutionResultsWriteEvidence, ExecutionResultsWriteEvidence,
    write_execution_results_atomically, write_execution_results_atomically_if_source_matches,
    write_execution_results_to_new_file,
};
pub use layout::load_workbook_layout;
pub(crate) use layout::preview::XlsxLayoutPreviewPort;
pub use layout::template::create_default_layout_workbook;
pub(crate) use layout::{XlsxLayoutUpgradePort, XlsxWorkbookPort};
pub(crate) use opener::{XlsxWorkbookOpenPort, launch_default_application};
pub use reference::fixture::create_representative_workbook;
pub use reference::inspection::{WorkbookFeatureEvidence, inspect_representative_workbook};

/// 为应用组合根建立正式工作簿生成端口，不暴露具体适配器类型。
pub(crate) fn create_workbook_generation_port(
    tool_root: ToolRoot,
    acquisition: std::sync::Arc<ShipAcquisition>,
    preferences: Result<UserPreferences, String>,
) -> Box<dyn WorkbookGenerationPort> {
    let preferences = match preferences {
        Ok(preferences) => generation::OperationPreferences::Ready(preferences),
        Err(summary) => generation::OperationPreferences::Unreadable(summary),
    };
    Box::new(
        generation::XlsxWorkbookGenerationPort::new(tool_root)
            .with_acquisition(acquisition, preferences),
    )
}

/// 工作簿验证的固定失败分类。
#[derive(Debug, Error)]
pub enum WorkbookProbeError {
    /// 输入或输出路径不满足工作簿边界。
    #[error("工作簿路径 {path} 无效: {message}")]
    InvalidPath { path: PathBuf, message: String },
    /// 文件系统操作失败。
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// ZIP 容器无法读取或写出。
    #[error("{stage} 处理 {path} 的 ZIP 容器失败: {source}")]
    Zip {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },
    /// OOXML 部件缺失。
    #[error("工作簿缺少 OOXML 部件 {part}")]
    MissingPart { part: String },
    /// OOXML 或 OPC 结构不满足约束。
    #[error("OOXML 部件 {part} 无效: {message}")]
    InvalidOoxml { part: String, message: String },
    /// 工作簿引用外部资源，定点编辑拒绝继续。
    #[error("关系部件 {part} 的 {relationship_id} 指向外部目标 {target}")]
    ExternalRelationship {
        part: String,
        relationship_id: String,
        target: String,
    },
    /// 工作簿包含当前编辑边界之外的数据连接部件。
    #[error("工作簿包含不支持的外部数据部件 {part}")]
    UnsupportedPart { part: String },
    /// 工作簿被其他程序占用，操作系统拒绝原子替换。
    #[error("工作簿 {path} 正在被占用，请关闭 Excel 后重试: {source}")]
    WorkbookLocked {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 原工作簿在备份后或临时文件验证期间发生变化。
    #[error("工作簿 {path} 已发生变化: expected={expected}, actual={actual}")]
    SourceChanged {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// 操作系统随机源不可用。
    #[error("建立工作簿临时文件名时操作系统随机源不可用: {0}")]
    RandomSource(getrandom::Error),
    /// XLSX 写出后端失败。
    #[error("写出工作簿失败: {source}")]
    XlsxWrite {
        #[source]
        source: rust_xlsxwriter::XlsxError,
    },
    /// 程序内置的布局注册表不满足自身约束。
    #[error("默认布局注册表无效: {source}")]
    LayoutContract {
        #[source]
        source: crate::application::LayoutModelError,
    },
    /// 默认模板缺少已注册项的显示定义。
    #[error("默认布局模板定义无效: {message}")]
    LayoutTemplate { message: String },
    /// 数据工作簿生成所需的布局或内部映射不完整。
    #[error("工作簿生成定义无效: {message}")]
    WorkbookBuild { message: String },
    /// 工作簿语义读取失败。
    #[error("读取工作簿语义失败: {source}")]
    XlsxRead {
        #[source]
        source: calamine::XlsxError,
    },
    /// 代表性工作簿缺少必需能力证据。
    #[error("代表性工作簿缺少特性 {feature}")]
    FeatureMissing { feature: &'static str },
    /// 失败操作留下的目标文件未能清理。
    #[error("清理 {path} 失败，原操作为 {operation}: {source}")]
    CleanupFailed {
        path: PathBuf,
        operation: String,
        #[source]
        source: std::io::Error,
    },
}

impl From<rust_xlsxwriter::XlsxError> for WorkbookProbeError {
    fn from(source: rust_xlsxwriter::XlsxError) -> Self {
        Self::XlsxWrite { source }
    }
}

impl From<suzushiro_xlsx_toolkit::XlsxError> for WorkbookProbeError {
    fn from(error: suzushiro_xlsx_toolkit::XlsxError) -> Self {
        use suzushiro_xlsx_toolkit::XlsxError;
        match error {
            XlsxError::InvalidPath { path, message } => Self::InvalidPath { path, message },
            XlsxError::Io {
                stage,
                path,
                source,
            } => Self::Io {
                stage,
                path,
                source,
            },
            XlsxError::Zip {
                stage,
                path,
                source,
            } => Self::Zip {
                stage,
                path,
                source,
            },
            XlsxError::MissingPart { part } => Self::MissingPart { part },
            XlsxError::InvalidOoxml { part, message } => Self::InvalidOoxml { part, message },
            XlsxError::ExternalRelationship {
                part,
                relationship_id,
                target,
            } => Self::ExternalRelationship {
                part,
                relationship_id,
                target,
            },
            XlsxError::UnsupportedPart { part } => Self::UnsupportedPart { part },
            XlsxError::WorkbookLocked { path, source } => Self::WorkbookLocked { path, source },
            XlsxError::SourceChanged {
                path,
                expected,
                actual,
            } => Self::SourceChanged {
                path,
                expected,
                actual,
            },
            XlsxError::XlsxWrite { source } => Self::XlsxWrite { source },
            XlsxError::XlsxRead { source } => Self::XlsxRead { source },
            XlsxError::CleanupFailed {
                path,
                operation,
                source,
            } => Self::CleanupFailed {
                path,
                operation,
                source,
            },
            XlsxError::FeatureMissing { feature } => Self::FeatureMissing { feature },
            XlsxError::RandomSource(source) => Self::RandomSource(source),
        }
    }
}

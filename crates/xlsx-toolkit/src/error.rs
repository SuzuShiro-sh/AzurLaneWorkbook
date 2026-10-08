use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum XlsxError {
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
    /// 工作簿语义读取失败。
    #[error("读取工作簿语义失败: {source}")]
    XlsxRead {
        #[source]
        source: calamine::XlsxError,
    },
    /// 工作簿缺少调用方要求的结构特性。
    #[error("工作簿缺少特性 {feature}")]
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

impl From<rust_xlsxwriter::XlsxError> for XlsxError {
    fn from(source: rust_xlsxwriter::XlsxError) -> Self {
        Self::XlsxWrite { source }
    }
}

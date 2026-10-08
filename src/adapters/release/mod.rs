//! 解析、建立并验证单目录 Windows 发布清单和完整文件闭包。

pub mod assembly;
mod payload;

pub use payload::ensure_extracted_release;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::application::{AppError, LayoutModelError, WorkbookProjectionV4};

use super::device::parse_adb_revision;
use super::device::profile::{RuntimeProfile, RuntimeProfileError};
use super::device::runtime::EXPECTED_AGENT_VERSION;
use super::settings::{DeviceMode, Settings, SettingsError};
use super::tool_root::{ToolRoot, ToolRootError};
use super::workbook::load_workbook_layout;
use super::{MAXIMUM_AGENT_FILE_BYTES, MAXIMUM_RELEASE_FILE_BYTES};
use suzushiro_content_digest::sha256_file as shared_sha256_file;
use suzushiro_text_format::is_canonical_sha256;

pub(crate) const MANIFEST_RELATIVE_PATH: &str = "manifest.json";
const RELEASE_SCHEMA_VERSION: u32 = 1;
const RELEASE_PRODUCT: &str = "AzurLaneWorkbook";
const RELEASE_TARGET: &str = "x86_64-pc-windows-msvc";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const AGENT_RELATIVE_PATH: &str = "runtime/inject/libazlw-agent-x86_64.so";
const AGENT_VERSION_MARKER_PREFIX: &[u8] = b"AZLW_AGENT_VERSION=";

pub(crate) const RELEASE_FILE_SPECS: &[ReleaseFileSpec<'static>] = &[
    ReleaseFileSpec::new(
        "settings.json",
        "运行设置 schema 1",
        ReleaseFileKind::Configuration,
        "schema-1",
        ReleaseArchitecture::Independent,
        ReleaseFilePolicy::MutableConfig,
        ReleaseFileValidator::SettingsV1,
    ),
    ReleaseFileSpec::new(
        "workbook-layout.xlsx",
        "工作簿能力与布局基线",
        ReleaseFileKind::Workbook,
        env!("CARGO_PKG_VERSION"),
        ReleaseArchitecture::Independent,
        ReleaseFilePolicy::MutableConfig,
        ReleaseFileValidator::WorkbookLayoutV1,
    ),
    ReleaseFileSpec::new(
        "runtime/adb/adb.exe",
        "隔离运行的 Android Debug Bridge 客户端和服务端",
        ReleaseFileKind::Executable,
        "platform-tools",
        ReleaseArchitecture::X86,
        ReleaseFilePolicy::Immutable,
        ReleaseFileValidator::None,
    ),
    ReleaseFileSpec::new(
        "runtime/adb/AdbWinApi.dll",
        "ADB Windows API 运行库",
        ReleaseFileKind::DynamicLibrary,
        "platform-tools",
        ReleaseArchitecture::X86,
        ReleaseFilePolicy::Immutable,
        ReleaseFileValidator::None,
    ),
    ReleaseFileSpec::new(
        "runtime/adb/NOTICE.txt",
        "Android SDK Platform-Tools 第三方声明",
        ReleaseFileKind::License,
        "platform-tools",
        ReleaseArchitecture::Independent,
        ReleaseFilePolicy::Immutable,
        ReleaseFileValidator::None,
    ),
    ReleaseFileSpec::new(
        "runtime/adb/source.properties",
        "Android SDK Platform-Tools 来源版本",
        ReleaseFileKind::Configuration,
        "platform-tools",
        ReleaseArchitecture::Independent,
        ReleaseFilePolicy::Immutable,
        ReleaseFileValidator::None,
    ),
    ReleaseFileSpec::new(
        "runtime/inject/azlw-loader-x86_64",
        "Android x86-64 一次性进程加载器",
        ReleaseFileKind::NativeRuntime,
        env!("CARGO_PKG_VERSION"),
        ReleaseArchitecture::X86_64,
        ReleaseFilePolicy::Immutable,
        ReleaseFileValidator::None,
    ),
    ReleaseFileSpec::new(
        AGENT_RELATIVE_PATH,
        "Android x86-64 专用运行态代理",
        ReleaseFileKind::NativeRuntime,
        EXPECTED_AGENT_VERSION,
        ReleaseArchitecture::X86_64,
        ReleaseFilePolicy::Immutable,
        ReleaseFileValidator::AgentVersion,
    ),
    ReleaseFileSpec::new(
        "runtime/resources/profiles/default.json",
        "目标游戏运行态身份与 Hook 配置",
        ReleaseFileKind::Configuration,
        "profile-schema-1",
        ReleaseArchitecture::Independent,
        ReleaseFilePolicy::VersionedConfig,
        ReleaseFileValidator::RuntimeProfileV1,
    ),
];

/// 单目录发布包的固定产品身份和逐文件声明。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    schema_version: u32,
    product: String,
    product_version: String,
    target: String,
    files: Vec<ReleaseFileEntry>,
}

impl ReleaseManifest {
    /// 建立当前程序版本的 Windows x64 清单，并按可移植路径稳定排序。
    pub fn new(mut files: Vec<ReleaseFileEntry>) -> Self {
        files.sort_by(|left: &ReleaseFileEntry, right: &ReleaseFileEntry| {
            left.path.cmp(&right.path)
        });
        Self {
            schema_version: RELEASE_SCHEMA_VERSION,
            product: RELEASE_PRODUCT.to_owned(),
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
            target: RELEASE_TARGET.to_owned(),
            files,
        }
    }

    /// 编码为带末尾换行的稳定缩进 JSON，供排他发布写入。
    pub fn to_pretty_bytes(&self) -> Result<Vec<u8>, ReleaseError> {
        let mut bytes: Vec<u8> = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// 返回清单声明的程序版本。
    pub fn product_version(&self) -> &str {
        &self.product_version
    }

    /// 返回按路径稳定排序的逐文件声明。
    pub fn files(&self) -> &[ReleaseFileEntry] {
        &self.files
    }
}

/// 发布清单中的单个文件、用途、来源版本、架构和完整性策略。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFileEntry {
    path: String,
    purpose: String,
    kind: ReleaseFileKind,
    version: String,
    architecture: ReleaseArchitecture,
    policy: ReleaseFilePolicy,
    validator: ReleaseFileValidator,
    size_bytes: u64,
    sha256: String,
}

/// 组装发布文件时使用的显式静态描述，不包含从磁盘计算的大小和摘要。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseFileSpec<'a> {
    path: &'a str,
    purpose: &'a str,
    kind: ReleaseFileKind,
    version: &'a str,
    architecture: ReleaseArchitecture,
    policy: ReleaseFilePolicy,
    validator: ReleaseFileValidator,
}

impl<'a> ReleaseFileSpec<'a> {
    /// 建立一个由组装器随后读取和摘要的发布文件描述。
    pub const fn new(
        path: &'a str,
        purpose: &'a str,
        kind: ReleaseFileKind,
        version: &'a str,
        architecture: ReleaseArchitecture,
        policy: ReleaseFilePolicy,
        validator: ReleaseFileValidator,
    ) -> Self {
        Self {
            path,
            purpose,
            kind,
            version,
            architecture,
            policy,
            validator,
        }
    }

    /// 返回最终发布目录中的规范正斜杠路径。
    pub(crate) const fn path(self) -> &'a str {
        self.path
    }

    /// 返回该文件在启动校验中的摘要策略。
    pub(crate) const fn policy(self) -> ReleaseFilePolicy {
        self.policy
    }
}

impl ReleaseFileEntry {
    /// 从工具根目录内既有普通文件建立初始发布声明。
    pub fn from_file(
        tool_root: &ToolRoot,
        specification: ReleaseFileSpec<'_>,
    ) -> Result<Self, ReleaseError> {
        let normalized: PathBuf =
            tool_root.validated_relative_path(Path::new(specification.path))?;
        let path_text: String = portable_path_text(&normalized)?;
        if path_text != specification.path {
            return Err(ReleaseError::InvalidManifest {
                field: format!("files[{}].path", specification.path),
                message: "路径必须使用规范正斜杠表示".to_owned(),
            });
        }
        let path: PathBuf = tool_root.existing_file(&normalized)?;
        let metadata: fs::Metadata = release_file_metadata(&path)?;
        Ok(Self {
            path: path_text,
            purpose: specification.purpose.to_owned(),
            kind: specification.kind,
            version: if specification.path.starts_with("runtime/adb/") {
                read_adb_revision(tool_root)?
            } else {
                specification.version.to_owned()
            },
            architecture: specification.architecture,
            policy: specification.policy,
            validator: specification.validator,
            size_bytes: metadata.len(),
            sha256: file_sha256(&path)?,
        })
    }

    /// 返回使用正斜杠的工具内相对路径。
    pub fn path(&self) -> &str {
        &self.path
    }

    /// 返回文件进入发布包的必要用途。
    pub fn purpose(&self) -> &str {
        &self.purpose
    }

    /// 返回文件的运行角色。
    pub fn kind(&self) -> ReleaseFileKind {
        self.kind
    }

    /// 返回文件来源组件或产品版本。
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 返回文件内容对应的处理器架构。
    pub fn architecture(&self) -> ReleaseArchitecture {
        self.architecture
    }

    /// 返回初始摘要的强制或诊断策略。
    pub fn policy(&self) -> ReleaseFilePolicy {
        self.policy
    }

    /// 返回启动时执行的内容契约。
    pub fn validator(&self) -> ReleaseFileValidator {
        self.validator
    }

    /// 返回组装时记录的初始文件大小。
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回组装时记录的小写 SHA-256。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// 文件在发布闭包中的运行角色。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseFileKind {
    Executable,
    DynamicLibrary,
    NativeRuntime,
    Configuration,
    Workbook,
    License,
}

/// 文件内容对应的处理器架构。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseArchitecture {
    X86,
    X86_64,
    Independent,
}

/// 初始摘要在启动校验中的执行方式。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseFilePolicy {
    Immutable,
    MutableConfig,
    VersionedConfig,
}

/// 可变或版本化配置必须执行的内容契约。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseFileValidator {
    None,
    AgentVersion,
    SettingsV1,
    /// 严格校验当前 v1 布局清单。
    WorkbookLayoutV1,
    RuntimeProfileV1,
}

/// 启动时完成文件集、完整性和配置契约检查后的结构化证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseVerificationReport {
    pub product_version: String,
    pub checked_files: usize,
    pub immutable_files: usize,
    pub modified_configurations: Vec<String>,
}

/// 只读验证发布清单、目录文件闭包、不可变摘要和可变配置契约。
pub fn verify_release(tool_root: &Path) -> Result<ReleaseVerificationReport, ReleaseError> {
    let tool_root: ToolRoot = ToolRoot::open(tool_root)?;
    let manifest_path: PathBuf = tool_root
        .existing_file(Path::new(MANIFEST_RELATIVE_PATH))
        .map_err(|source| {
            release_path_error(
                "release.locate_manifest",
                tool_root.as_path().join(MANIFEST_RELATIVE_PATH),
                source,
            )
        })?;
    let manifest_metadata: fs::Metadata = release_file_metadata(&manifest_path)?;
    if manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ReleaseError::ManifestTooLarge {
            actual: manifest_metadata.len(),
            maximum: MAX_MANIFEST_BYTES,
        });
    }
    let manifest_bytes: Vec<u8> = fs::read(&manifest_path).map_err(|source| ReleaseError::Io {
        stage: "release.read_manifest",
        path: manifest_path,
        source,
    })?;
    let manifest: ReleaseManifest = parse_manifest(&manifest_bytes)?;
    validate_manifest(&tool_root, &manifest)?;

    let declared: BTreeSet<String> = manifest
        .files
        .iter()
        .map(|entry: &ReleaseFileEntry| entry.path.clone())
        .collect();
    let actual: BTreeSet<String> = tool_root
        .list_regular_files(&["data"])
        .map_err(|source| {
            release_path_error(
                "release.enumerate_files",
                tool_root.as_path().to_path_buf(),
                source,
            )
        })?
        .into_iter()
        .map(|path: PathBuf| portable_path_text(&path))
        .collect::<Result<BTreeSet<_>, _>>()?
        .into_iter()
        // 解包和可变配置的持久锁是运行时协调文件，不属于发布载荷。
        .filter(|path: &String| {
            path != MANIFEST_RELATIVE_PATH
                && path != ".workbook-layout.xlsx.write.lock"
                && path != "settings.lock"
                && path != "release.lock"
        })
        .collect();
    if declared != actual {
        return Err(ReleaseError::FileSetMismatch {
            missing: declared.difference(&actual).cloned().collect(),
            unexpected: actual.difference(&declared).cloned().collect(),
        });
    }

    let mut immutable_files: usize = 0;
    let mut modified_configurations: Vec<String> = Vec::new();
    for entry in &manifest.files {
        let path: PathBuf = tool_root
            .existing_file(Path::new(&entry.path))
            .map_err(|source| {
                release_path_error(
                    "release.read_declared_file",
                    tool_root.as_path().join(&entry.path),
                    source,
                )
            })?;
        let metadata: fs::Metadata = release_file_metadata(&path)?;
        let actual_sha256: String = file_sha256(&path)?;
        if entry.policy == ReleaseFilePolicy::Immutable {
            immutable_files += 1;
            if metadata.len() != entry.size_bytes || actual_sha256 != entry.sha256 {
                return Err(ReleaseError::IntegrityMismatch {
                    path: entry.path.clone(),
                    expected_size: entry.size_bytes,
                    actual_size: metadata.len(),
                    expected_sha256: entry.sha256.clone(),
                    actual_sha256,
                });
            }
        } else if metadata.len() != entry.size_bytes || actual_sha256 != entry.sha256 {
            modified_configurations.push(entry.path.clone());
        }
        validate_release_file(&tool_root, &manifest, entry, &path)?;
    }

    Ok(ReleaseVerificationReport {
        product_version: manifest.product_version,
        checked_files: manifest.files.len(),
        immutable_files,
        modified_configurations,
    })
}

/// 发布清单、目录闭包或文件内容没有满足离线启动契约。
#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest.json 为 {actual} 字节，超过 {maximum} 字节上限")]
    ManifestTooLarge { actual: u64, maximum: u64 },
    #[error("manifest.json 字段 {path} 无效: {source}")]
    ManifestJson {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("发布清单字段 {field} 无效: {message}")]
    InvalidManifest { field: String, message: String },
    #[error("发布目录文件集合不一致，缺失={missing:?}，未登记={unexpected:?}")]
    FileSetMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    #[error(
        "不可变文件 {path} 完整性不一致，大小 {actual_size}/{expected_size}，SHA-256 {actual_sha256}/{expected_sha256}"
    )]
    IntegrityMismatch {
        path: String,
        expected_size: u64,
        actual_size: u64,
        expected_sha256: String,
        actual_sha256: String,
    },
    #[error("发布设置校验失败: {0}")]
    Settings(#[from] SettingsError),
    #[error("程序内置工作簿布局注册表无效: {source}")]
    WorkbookLayoutRegistry {
        #[source]
        source: LayoutModelError,
    },
    #[error("发布工作簿布局 {path} 校验失败: {source}")]
    WorkbookLayout {
        path: String,
        #[source]
        source: AppError,
    },
    #[error("发布运行态配置 {path} 校验失败: {source}")]
    RuntimeProfile {
        path: String,
        #[source]
        source: RuntimeProfileError,
    },
    #[error("发布运行态代理 {path} 版本无效，期望 {expected}，实际 {actual}")]
    AgentVersion {
        path: String,
        expected: &'static str,
        actual: String,
    },
    #[error("发布清单 JSON 编码失败: {0}")]
    ManifestEncoding(#[from] serde_json::Error),
}

impl ReleaseError {
    /// 返回 CLI、日志和发布检查脚本可以稳定匹配的错误码。
    pub fn code(&self) -> &'static str {
        match self {
            Self::ToolRoot(_) | Self::WorkbookLayoutRegistry { .. } => {
                "APPLICATION_INITIALIZATION_FAILED"
            }
            Self::Io { .. }
            | Self::ManifestTooLarge { .. }
            | Self::ManifestJson { .. }
            | Self::InvalidManifest { .. }
            | Self::FileSetMismatch { .. }
            | Self::IntegrityMismatch { .. }
            | Self::ManifestEncoding(_) => "MANIFEST_INVALID",
            Self::Settings(_) => "SETTINGS_INVALID",
            Self::WorkbookLayout { source, .. } => source.code().as_str(),
            Self::RuntimeProfile { .. } | Self::AgentVersion { .. } => "RUNTIME_INCOMPATIBLE",
        }
    }

    /// 返回失败所在的稳定发布验证阶段。
    pub fn stage(&self) -> &'static str {
        match self {
            Self::ToolRoot(_) => "release.open_tool_root",
            Self::Io { stage, .. } => stage,
            Self::ManifestTooLarge { .. } => "release.read_manifest",
            Self::ManifestJson { .. } => "release.parse_manifest",
            Self::InvalidManifest { .. } => "release.validate_manifest",
            Self::FileSetMismatch { .. } => "release.compare_files",
            Self::IntegrityMismatch { .. } => "release.verify_integrity",
            Self::Settings(_) => "release.validate_settings",
            Self::WorkbookLayoutRegistry { .. } => "release.validate_workbook_registry",
            Self::WorkbookLayout { .. } => "release.validate_workbook_layout",
            Self::RuntimeProfile { .. } => "release.validate_runtime_profile",
            Self::AgentVersion { .. } => "release.validate_agent_version",
            Self::ManifestEncoding(_) => "release.encode_manifest",
        }
    }

    /// 返回按键排序、可直接展示的定位上下文，不包含完整二进制内容或密钥。
    pub fn context(&self) -> BTreeMap<String, String> {
        let mut context: BTreeMap<String, String> = BTreeMap::new();
        match self {
            Self::ToolRoot(_) => {
                context.insert("component".to_owned(), "tool_root".to_owned());
            }
            Self::Io { stage, path, .. } => {
                context.insert("operation".to_owned(), (*stage).to_owned());
                let display_path: String = if *stage == "release.locate_manifest" {
                    MANIFEST_RELATIVE_PATH.to_owned()
                } else {
                    path.display().to_string()
                };
                context.insert("path".to_owned(), display_path);
                if *stage == "release.locate_manifest" {
                    context.insert("component".to_owned(), "manifest".to_owned());
                }
            }
            Self::ManifestTooLarge { actual, maximum } => {
                context.insert("actual".to_owned(), actual.to_string());
                context.insert("expected".to_owned(), maximum.to_string());
            }
            Self::ManifestJson { path, .. } => {
                context.insert("field".to_owned(), path.clone());
            }
            Self::InvalidManifest { field, .. } => {
                context.insert("field".to_owned(), field.clone());
            }
            Self::FileSetMismatch {
                missing,
                unexpected,
            } => {
                if !missing.is_empty() {
                    context.insert("missing".to_owned(), missing.join(", "));
                }
                if !unexpected.is_empty() {
                    context.insert("unexpected".to_owned(), unexpected.join(", "));
                }
            }
            Self::IntegrityMismatch {
                path,
                expected_size,
                actual_size,
                ..
            } => {
                context.insert("path".to_owned(), path.clone());
                context.insert("expected".to_owned(), expected_size.to_string());
                context.insert("actual".to_owned(), actual_size.to_string());
            }
            Self::Settings(_) => {
                context.insert("component".to_owned(), "settings".to_owned());
            }
            Self::WorkbookLayoutRegistry { .. } => {
                context.insert(
                    "component".to_owned(),
                    "workbook_layout_registry".to_owned(),
                );
            }
            Self::WorkbookLayout { path, source } => {
                context.insert("path".to_owned(), path.clone());
                context.insert("source_code".to_owned(), source.code().as_str().to_owned());
            }
            Self::RuntimeProfile { path, .. } => {
                context.insert("path".to_owned(), path.clone());
            }
            Self::AgentVersion {
                path,
                expected,
                actual,
            } => {
                context.insert("path".to_owned(), path.clone());
                context.insert("expected".to_owned(), (*expected).to_owned());
                context.insert("actual".to_owned(), actual.clone());
            }
            Self::ManifestEncoding(_) => {
                context.insert("component".to_owned(), "manifest".to_owned());
            }
        }
        context
    }
}

fn parse_manifest(bytes: &[u8]) -> Result<ReleaseManifest, ReleaseError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let manifest: ReleaseManifest = serde_path_to_error::deserialize(&mut deserializer).map_err(
        |error: serde_path_to_error::Error<serde_json::Error>| {
            let path: String = normalize_manifest_error_path(error.path().to_string());
            ReleaseError::ManifestJson {
                path,
                source: error.into_inner(),
            }
        },
    )?;
    deserializer
        .end()
        .map_err(|source| ReleaseError::ManifestJson {
            path: "$".to_owned(),
            source,
        })?;
    Ok(manifest)
}

/// 将解析器无法定位字段时的占位路径归一为稳定的 JSON 根路径。
fn normalize_manifest_error_path(path: String) -> String {
    if path == "?" { "$".to_owned() } else { path }
}

/// 来源版本来自随包元数据，依赖锁负责构建版本选择。
fn read_adb_revision(tool_root: &ToolRoot) -> Result<String, ReleaseError> {
    let path = tool_root.existing_file(Path::new("runtime/adb/source.properties"))?;
    let metadata = release_file_metadata(&path)?;
    if metadata.len() > 4096 {
        return Err(ReleaseError::InvalidManifest {
            field: "adb.version".to_owned(),
            message: "ADB 来源元数据超过 4096 字节".to_owned(),
        });
    }
    let properties = fs::read_to_string(&path).map_err(|source| ReleaseError::Io {
        stage: "release.read_adb_properties",
        path,
        source,
    })?;
    parse_adb_revision(&properties).map_err(|error| ReleaseError::InvalidManifest {
        field: "adb.version".to_owned(),
        message: error.to_string(),
    })
}

fn validate_manifest(tool_root: &ToolRoot, manifest: &ReleaseManifest) -> Result<(), ReleaseError> {
    if manifest.schema_version != RELEASE_SCHEMA_VERSION {
        return invalid_manifest(
            "schema_version",
            format!(
                "只支持 {RELEASE_SCHEMA_VERSION}，实际为 {}",
                manifest.schema_version
            ),
        );
    }
    if manifest.product != RELEASE_PRODUCT {
        return invalid_manifest("product", format!("必须是 {RELEASE_PRODUCT}"));
    }
    if manifest.product_version != env!("CARGO_PKG_VERSION") {
        return invalid_manifest(
            "product_version",
            format!("必须与当前程序版本 {} 一致", env!("CARGO_PKG_VERSION")),
        );
    }
    if manifest.target != RELEASE_TARGET {
        return invalid_manifest("target", format!("必须是 {RELEASE_TARGET}"));
    }
    if manifest.files.is_empty() {
        return invalid_manifest("files", "不得为空");
    }

    let mut previous: Option<&str> = None;
    let mut by_path: BTreeMap<&str, &ReleaseFileEntry> = BTreeMap::new();
    let mut case_folded: BTreeSet<String> = BTreeSet::new();
    for (index, entry) in manifest.files.iter().enumerate() {
        validate_entry(tool_root, index, entry)?;
        if previous.is_some_and(|value: &str| value >= entry.path.as_str()) {
            return invalid_manifest(
                format!("files[{index}].path"),
                "文件必须按路径严格递增排序且不得重复",
            );
        }
        previous = Some(&entry.path);
        if !case_folded.insert(entry.path.to_lowercase()) {
            return invalid_manifest(
                format!("files[{index}].path"),
                "Windows 不区分大小写时路径重复",
            );
        }
        by_path.insert(&entry.path, entry);
    }

    let adb_revision = read_adb_revision(tool_root)?;
    for required in RELEASE_FILE_SPECS {
        let entry: &ReleaseFileEntry =
            by_path
                .get(required.path)
                .copied()
                .ok_or_else(|| ReleaseError::InvalidManifest {
                    field: "files".to_owned(),
                    message: format!("缺少必要发布文件 {}", required.path),
                })?;
        if entry.purpose != required.purpose
            || entry.kind != required.kind
            || entry.version
                != if required.path.starts_with("runtime/adb/") {
                    adb_revision.as_str()
                } else {
                    required.version
                }
            || entry.architecture != required.architecture
            || entry.policy != required.policy
            || entry.validator != required.validator
        {
            return invalid_manifest(
                format!("files[{}]", required.path),
                "用途、类型、来源版本、架构或校验策略与固定发布定义不一致",
            );
        }
    }
    let unexpected: Vec<&str> = by_path
        .keys()
        .copied()
        .filter(|path: &&str| {
            !RELEASE_FILE_SPECS
                .iter()
                .any(|required: &ReleaseFileSpec<'_>| required.path == *path)
        })
        .collect();
    if !unexpected.is_empty() {
        return invalid_manifest(
            "files",
            format!("包含未在固定发布闭包定义的文件 {unexpected:?}"),
        );
    }
    Ok(())
}

fn validate_entry(
    tool_root: &ToolRoot,
    index: usize,
    entry: &ReleaseFileEntry,
) -> Result<(), ReleaseError> {
    if entry.path.contains('\\') {
        return invalid_manifest(
            format!("files[{index}].path"),
            "必须使用正斜杠，不能使用反斜杠",
        );
    }
    let normalized: PathBuf = tool_root
        .validated_relative_path(Path::new(&entry.path))
        .map_err(|source| ReleaseError::InvalidManifest {
            field: format!("files[{index}].path"),
            message: source.to_string(),
        })?;
    if portable_path_text(&normalized)? != entry.path {
        return invalid_manifest(format!("files[{index}].path"), "必须是规范的可移植相对路径");
    }
    if entry.path == MANIFEST_RELATIVE_PATH || entry.path.starts_with("data/") {
        return invalid_manifest(
            format!("files[{index}].path"),
            "manifest.json 自身和运行时 data 目录不得进入 files[]",
        );
    }
    validate_text_field(&format!("files[{index}].purpose"), &entry.purpose, 120)?;
    validate_version(&format!("files[{index}].version"), &entry.version)?;
    if entry.size_bytes == 0 || entry.size_bytes > MAXIMUM_RELEASE_FILE_BYTES {
        return invalid_manifest(
            format!("files[{index}].size_bytes"),
            format!("只允许 1 至 {MAXIMUM_RELEASE_FILE_BYTES}"),
        );
    }
    if !is_canonical_sha256(&entry.sha256) {
        return invalid_manifest(
            format!("files[{index}].sha256"),
            "必须是 64 位小写十六进制 SHA-256",
        );
    }
    let valid_architecture: bool = match entry.kind {
        ReleaseFileKind::Executable
        | ReleaseFileKind::DynamicLibrary
        | ReleaseFileKind::NativeRuntime => matches!(
            entry.architecture,
            ReleaseArchitecture::X86 | ReleaseArchitecture::X86_64
        ),
        ReleaseFileKind::Configuration | ReleaseFileKind::Workbook | ReleaseFileKind::License => {
            entry.architecture == ReleaseArchitecture::Independent
        }
    };
    if !valid_architecture {
        return invalid_manifest(
            format!("files[{index}].architecture"),
            format!(
                "文件类型 {:?} 与架构 {:?} 不兼容",
                entry.kind, entry.architecture
            ),
        );
    }
    let valid_contract: bool = matches!(
        (entry.policy, entry.validator),
        (
            ReleaseFilePolicy::Immutable,
            ReleaseFileValidator::None | ReleaseFileValidator::AgentVersion
        ) | (
            ReleaseFilePolicy::MutableConfig,
            ReleaseFileValidator::SettingsV1 | ReleaseFileValidator::WorkbookLayoutV1
        ) | (
            ReleaseFilePolicy::VersionedConfig,
            ReleaseFileValidator::RuntimeProfileV1
        )
    );
    if !valid_contract {
        return invalid_manifest(
            format!("files[{index}].validator"),
            "完整性策略与内容校验器组合无效",
        );
    }
    match entry.validator {
        ReleaseFileValidator::AgentVersion if entry.kind != ReleaseFileKind::NativeRuntime => {
            invalid_manifest(
                format!("files[{index}].kind"),
                "agent_version 只能用于 native_runtime",
            )
        }
        ReleaseFileValidator::SettingsV1 if entry.kind != ReleaseFileKind::Configuration => {
            invalid_manifest(
                format!("files[{index}].kind"),
                "settings_v1 只能用于 configuration",
            )
        }
        ReleaseFileValidator::WorkbookLayoutV1 if entry.kind != ReleaseFileKind::Workbook => {
            invalid_manifest(
                format!("files[{index}].kind"),
                "workbook_layout_v1 只能用于 workbook",
            )
        }
        ReleaseFileValidator::RuntimeProfileV1 if entry.kind != ReleaseFileKind::Configuration => {
            invalid_manifest(
                format!("files[{index}].kind"),
                "runtime_profile_v1 只能用于 configuration",
            )
        }
        _ => Ok(()),
    }
}

fn validate_release_file(
    tool_root: &ToolRoot,
    manifest: &ReleaseManifest,
    entry: &ReleaseFileEntry,
    path: &Path,
) -> Result<(), ReleaseError> {
    match entry.validator {
        ReleaseFileValidator::None => Ok(()),
        ReleaseFileValidator::AgentVersion => {
            if entry.path != AGENT_RELATIVE_PATH {
                return invalid_manifest(
                    format!("files[{}].validator", entry.path),
                    format!("agent_version 只允许固定 {AGENT_RELATIVE_PATH}"),
                );
            }
            validate_agent_version(path)
        }
        ReleaseFileValidator::SettingsV1 => {
            if entry.path != "settings.json" {
                return invalid_manifest(
                    format!("files[{}].validator", entry.path),
                    "settings_v1 只允许固定 settings.json",
                );
            }
            let settings: Settings = Settings::load(tool_root.as_path())?;
            let adb_path: &str = settings.device().adb_path();
            if adb_path.is_empty() {
                return Ok(());
            }
            let normalized: PathBuf = tool_root.validated_relative_path(Path::new(adb_path))?;
            let adb_path: String = portable_path_text(&normalized)?;
            let declared: Option<&ReleaseFileEntry> = manifest
                .files
                .iter()
                .find(|candidate: &&ReleaseFileEntry| candidate.path == adb_path);
            let Some(declared) = declared else {
                if settings.device().mode() == DeviceMode::Auto {
                    return Ok(());
                }
                return Err(ReleaseError::InvalidManifest {
                    field: "settings.device.adb_path".to_owned(),
                    message: format!("{adb_path} 未在发布清单登记"),
                });
            };
            if declared.kind != ReleaseFileKind::Executable
                || declared.policy != ReleaseFilePolicy::Immutable
            {
                return invalid_manifest(
                    "settings.device.adb_path",
                    "自定义 ADB 必须是清单中强制校验的 executable",
                );
            }
            Ok(())
        }
        ReleaseFileValidator::WorkbookLayoutV1 => {
            let registry = WorkbookProjectionV4::layout_registry()
                .map_err(|source| ReleaseError::WorkbookLayoutRegistry { source })?;
            load_workbook_layout(path, &registry)
                .map(|_| ())
                .map_err(|source| ReleaseError::WorkbookLayout {
                    path: entry.path.clone(),
                    source,
                })
        }
        ReleaseFileValidator::RuntimeProfileV1 => {
            RuntimeProfile::load(path)
                .map(|_| ())
                .map_err(|source| ReleaseError::RuntimeProfile {
                    path: entry.path.clone(),
                    source,
                })
        }
    }
}

/// 从有界代理文件中读取唯一的 NUL 结尾版本标记，并与宿主协议门禁保持一致。
fn validate_agent_version(path: &Path) -> Result<(), ReleaseError> {
    let metadata: fs::Metadata = release_file_metadata(path)?;
    if metadata.len() > MAXIMUM_AGENT_FILE_BYTES {
        return Err(ReleaseError::AgentVersion {
            path: AGENT_RELATIVE_PATH.to_owned(),
            expected: EXPECTED_AGENT_VERSION,
            actual: format!("文件超过 {MAXIMUM_AGENT_FILE_BYTES} 字节"),
        });
    }
    let bytes: Vec<u8> = fs::read(path).map_err(|source| ReleaseError::Io {
        stage: "release.read_agent_version",
        path: path.to_path_buf(),
        source,
    })?;
    let markers: Vec<usize> = bytes
        .windows(AGENT_VERSION_MARKER_PREFIX.len())
        .enumerate()
        .filter_map(|(index, value)| (value == AGENT_VERSION_MARKER_PREFIX).then_some(index))
        .collect();
    if markers.len() != 1 {
        return Err(ReleaseError::AgentVersion {
            path: AGENT_RELATIVE_PATH.to_owned(),
            expected: EXPECTED_AGENT_VERSION,
            actual: format!("版本标记数量为 {}", markers.len()),
        });
    }
    let value_start: usize = markers[0] + AGENT_VERSION_MARKER_PREFIX.len();
    let remaining: &[u8] = &bytes[value_start..];
    let Some(value_end) = remaining.iter().take(65).position(|byte: &u8| *byte == 0) else {
        return Err(ReleaseError::AgentVersion {
            path: AGENT_RELATIVE_PATH.to_owned(),
            expected: EXPECTED_AGENT_VERSION,
            actual: "版本标记缺少有界终止符".to_owned(),
        });
    };
    let actual: String = String::from_utf8_lossy(&remaining[..value_end]).into_owned();
    if validate_version("agent.version_marker", &actual).is_err()
        || actual != EXPECTED_AGENT_VERSION
    {
        return Err(ReleaseError::AgentVersion {
            path: AGENT_RELATIVE_PATH.to_owned(),
            expected: EXPECTED_AGENT_VERSION,
            actual,
        });
    }
    Ok(())
}

fn release_file_metadata(path: &Path) -> Result<fs::Metadata, ReleaseError> {
    let metadata: fs::Metadata = fs::metadata(path).map_err(|source| ReleaseError::Io {
        stage: "release.read_file_metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAXIMUM_RELEASE_FILE_BYTES {
        return invalid_manifest(
            path.display().to_string(),
            format!("文件必须为 1 至 {MAXIMUM_RELEASE_FILE_BYTES} 字节的普通文件"),
        );
    }
    Ok(metadata)
}

fn file_sha256(path: &Path) -> Result<String, ReleaseError> {
    shared_sha256_file(path).map_err(|source| ReleaseError::Io {
        stage: "release.hash_file",
        path: path.to_path_buf(),
        source,
    })
}

fn portable_path_text(path: &Path) -> Result<String, ReleaseError> {
    let mut values: Vec<&str> = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return invalid_manifest(path.display().to_string(), "路径包含非普通组件");
        };
        let text: &str = value
            .to_str()
            .ok_or_else(|| ReleaseError::InvalidManifest {
                field: path.display().to_string(),
                message: "路径必须是有效 Unicode".to_owned(),
            })?;
        values.push(text);
    }
    if values.is_empty() {
        return invalid_manifest(path.display().to_string(), "路径不得为空");
    }
    Ok(values.join("/"))
}

fn validate_text_field(field: &str, value: &str, maximum: usize) -> Result<(), ReleaseError> {
    if value.is_empty() || value.chars().count() > maximum || value.chars().any(char::is_control) {
        return invalid_manifest(field, format!("必须是 1 至 {maximum} 个不含控制字符的字符"));
    }
    Ok(())
}

fn validate_version(field: &str, value: &str) -> Result<(), ReleaseError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte: u8| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
        })
    {
        return invalid_manifest(field, "必须是至多 64 字节的稳定版本标识");
    }
    Ok(())
}

fn invalid_manifest<T>(
    field: impl Into<String>,
    message: impl Into<String>,
) -> Result<T, ReleaseError> {
    Err(ReleaseError::InvalidManifest {
        field: field.into(),
        message: message.into(),
    })
}

/// 将清单路径边界错误保留为原因链，同时统一归入发布清单校验阶段。
fn release_path_error(stage: &'static str, path: PathBuf, source: ToolRootError) -> ReleaseError {
    ReleaseError::Io {
        stage,
        path,
        source: std::io::Error::other(source),
    }
}

#[cfg(test)]
mod tests;

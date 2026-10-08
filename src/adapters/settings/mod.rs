//! 严格读取和校验工具根目录中的可移植运行设置。

pub(crate) mod port;
mod save;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::application::{AcquisitionUpdatePolicy, UserPreferences};

use super::file_snapshot::{FileSnapshotError, read_bounded_file_snapshot};
use super::tool_root::{ToolRoot, ToolRootError};

pub(crate) const SETTINGS_RELATIVE_PATH: &str = "settings.json";
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_TIMEOUT_SECONDS: u32 = 3_600;

/// 自动发现或严格手工指定设备目标。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceMode {
    Auto,
    Manual,
}

/// Agent 在目标进程中的可验证映射策略；默认模式保持现有 memfd 行为。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMappingMode {
    #[default]
    Memfd,
    AnonymousRemap,
}

impl AgentMappingMode {
    /// 返回设置、日志与原生收据共用的稳定名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memfd => "memfd",
            Self::AnonymousRemap => "anonymous_remap",
        }
    }

    /// 返回固定二进制启动与卸载结构使用的单字节值。
    pub const fn wire_value(self) -> u8 {
        match self {
            Self::Memfd => 0,
            Self::AnonymousRemap => 1,
        }
    }
}

/// Agent 在目标进程运行期间采用的可见性策略；默认不修改 linker 状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVisibilityMode {
    #[default]
    Normal,
    SolistHidden,
    SolistAndElfHeader,
}

impl AgentVisibilityMode {
    /// 返回设置、日志与原生收据共用的稳定名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::SolistHidden => "solist_hidden",
            Self::SolistAndElfHeader => "solist_and_elf_header",
        }
    }

    /// 返回组合运行策略中的稳定可见性值。
    pub const fn wire_value(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::SolistHidden => 1,
            Self::SolistAndElfHeader => 2,
        }
    }
}

/// 已通过 schema、路径、目标和超时边界校验的运行设置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    device: DeviceSettings,
    runtime: RuntimeSettings,
    preferences: UserPreferences,
    acquisition_request_interval_seconds: u32,
}

impl Settings {
    /// 从工具根目录固定位置读取设置，拒绝链接、超大文件和未知字段。
    pub fn load(tool_root: &Path) -> Result<Self, SettingsError> {
        Ok(SettingsSnapshot::load(tool_root)?.into_settings())
    }

    /// 按受控快照读取设置原文和内容身份，供校验和偏好修改共用。
    fn read_snapshot(tool_root: &ToolRoot) -> Result<(PathBuf, Vec<u8>, String), SettingsError> {
        let relative = Path::new(SETTINGS_RELATIVE_PATH);
        let path = tool_root.existing_file(relative)?;
        let snapshot = read_bounded_file_snapshot(tool_root, relative, MAX_SETTINGS_BYTES)
            .map_err(|error| map_file_snapshot(error, &path))?;
        let (bytes, source_sha256) = snapshot.into_parts();
        Ok((path, bytes, source_sha256))
    }

    /// 从 UTF-8 JSON 读取设置，并保留反序列化失败的完整字段路径。
    pub fn from_slice(tool_root: &ToolRoot, bytes: &[u8]) -> Result<Self, SettingsError> {
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(SettingsError::FileTooLarge {
                path: tool_root.as_path().join(SETTINGS_RELATIVE_PATH),
                actual: bytes.len() as u64,
                maximum: MAX_SETTINGS_BYTES,
            });
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let raw: RawSettings = serde_path_to_error::deserialize(&mut deserializer).map_err(
            |error: serde_path_to_error::Error<serde_json::Error>| {
                let path: String = error.path().to_string();
                SettingsError::Json {
                    path,
                    source: error.into_inner(),
                }
            },
        )?;
        deserializer.end().map_err(|source| SettingsError::Json {
            path: "$".to_owned(),
            source,
        })?;
        if raw.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema {
                actual: raw.schema_version,
            });
        }
        validate_timeout(
            "runtime.connect_timeout_seconds",
            raw.runtime.connect_timeout_seconds,
        )?;
        validate_timeout(
            "runtime.startup_timeout_seconds",
            raw.runtime.startup_timeout_seconds,
        )?;
        validate_optional_adb_path(
            tool_root,
            &raw.device.adb_path,
            raw.device.mode == DeviceMode::Manual,
        )?;
        validate_optional_manager_path(
            &raw.device.manager_path,
            raw.device.mode == DeviceMode::Manual,
        )?;
        if raw.device.mode == DeviceMode::Manual {
            validate_optional_serial(&raw.device.serial)?;
            validate_optional_instance(&raw.device.instance)?;
            validate_optional_package(&raw.device.game_package)?;
            require_manual_value("device.adb_path", &raw.device.adb_path)?;
            require_manual_value("device.serial", &raw.device.serial)?;
            require_manual_value("device.instance", &raw.device.instance)?;
            require_manual_value("device.game_package", &raw.device.game_package)?;
        }

        if !(2..=3600).contains(&raw.workbook.acquisition_request_interval_seconds) {
            return Err(SettingsError::InvalidValue {
                field: "workbook.acquisition_request_interval_seconds",
                message: "请求间隔必须介于 2 与 3600 秒之间".to_owned(),
            });
        }
        Ok(Self {
            acquisition_request_interval_seconds: raw.workbook.acquisition_request_interval_seconds,
            preferences: UserPreferences {
                ship_acquisition_enabled: raw.workbook.ship_acquisition_enabled,
                acquisition_update_policy: raw.workbook.acquisition_update_policy,
                detailed_diagnostics: raw.diagnostics.detailed,
                unload_after_sync: raw.runtime.unload_after_sync,
            },
            device: DeviceSettings {
                mode: raw.device.mode,
                adb_path: raw.device.adb_path,
                manager_path: raw.device.manager_path,
                serial: raw.device.serial,
                instance: raw.device.instance,
                game_package: raw.device.game_package,
            },
            runtime: RuntimeSettings {
                connect_timeout_seconds: raw.runtime.connect_timeout_seconds,
                startup_timeout_seconds: raw.runtime.startup_timeout_seconds,
                agent_mapping_mode: raw.runtime.agent_mapping_mode,
                agent_visibility_mode: raw.runtime.agent_visibility_mode,
            },
        })
    }

    pub(crate) fn acquisition_request_interval_seconds(&self) -> u32 {
        self.acquisition_request_interval_seconds
    }

    /// 返回设备发现模式和可选目标提示。
    pub fn device(&self) -> &DeviceSettings {
        &self.device
    }

    /// 返回有界的连接和启动等待时长。
    pub fn runtime(&self) -> &RuntimeSettings {
        &self.runtime
    }

    /// 返回工作簿、诊断与同步收尾共用的用户偏好。
    pub fn preferences(&self) -> UserPreferences {
        self.preferences
    }
}

/// 一次校验通过的设置，以及该次读取的内容身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsSnapshot {
    settings: Settings,
    source_sha256: String,
}

impl SettingsSnapshot {
    /// 读取并校验设置，身份对应该次通过大小和变化检查的原文。
    pub fn load(tool_root: &Path) -> Result<Self, SettingsError> {
        let tool_root = ToolRoot::open(tool_root)?;
        let (_path, bytes, source_sha256) = Settings::read_snapshot(&tool_root)?;
        let settings = Settings::from_slice(&tool_root, &bytes)?;
        Ok(Self {
            settings,
            source_sha256,
        })
    }

    /// 返回已校验设置。
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// 返回这次校验所依据的原文摘要。
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }

    /// 交出设置，供一次操作分发给各个适配器。
    pub fn into_settings(self) -> Settings {
        self.settings
    }
}

fn map_file_snapshot(error: FileSnapshotError, path: &Path) -> SettingsError {
    match error {
        FileSnapshotError::Path(source) => SettingsError::ToolRoot(source),
        FileSnapshotError::Io { source, .. } => SettingsError::Io {
            stage: "settings.read",
            path: path.to_path_buf(),
            source,
        },
        FileSnapshotError::TooLarge { actual, maximum } => SettingsError::FileTooLarge {
            path: path.to_path_buf(),
            actual,
            maximum,
        },
        FileSnapshotError::Changed => SettingsError::Io {
            stage: "settings.read",
            path: path.to_path_buf(),
            source: std::io::Error::other("文件在读取期间发生变化"),
        },
    }
}

/// 已校验的设备发现设置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSettings {
    mode: DeviceMode,
    adb_path: String,
    manager_path: String,
    serial: String,
    instance: String,
    game_package: String,
}

impl DeviceSettings {
    /// 返回自动发现或严格手工模式。
    pub fn mode(&self) -> DeviceMode {
        self.mode
    }

    /// 返回工具根目录内的可选 ADB 相对路径。
    pub fn adb_path(&self) -> &str {
        &self.adb_path
    }

    /// 返回可选的模拟器管理器绝对路径提示。
    pub fn manager_path(&self) -> &str {
        &self.manager_path
    }

    /// 返回可选的回环设备地址。
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// 返回可选的提供方与实例标识。
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// 返回可选的 Android 游戏包名。
    pub fn game_package(&self) -> &str {
        &self.game_package
    }
}

/// 已校验的运行时等待边界。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSettings {
    connect_timeout_seconds: u32,
    startup_timeout_seconds: u32,
    agent_mapping_mode: AgentMappingMode,
    agent_visibility_mode: AgentVisibilityMode,
}

impl RuntimeSettings {
    /// 返回连接阶段超时秒数。
    pub fn connect_timeout_seconds(&self) -> u32 {
        self.connect_timeout_seconds
    }

    /// 返回启动阶段超时秒数。
    pub fn startup_timeout_seconds(&self) -> u32 {
        self.startup_timeout_seconds
    }

    /// 返回 Agent 映射模式；旧设置缺少字段时稳定回落到 memfd。
    pub fn agent_mapping_mode(&self) -> AgentMappingMode {
        self.agent_mapping_mode
    }

    /// 返回 Agent 可见性策略；旧设置缺少字段时稳定保持 normal。
    pub fn agent_visibility_mode(&self) -> AgentVisibilityMode {
        self.agent_visibility_mode
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    schema_version: u32,
    device: RawDeviceSettings,
    runtime: RawRuntimeSettings,
    #[serde(default)]
    workbook: RawWorkbookSettings,
    #[serde(default)]
    diagnostics: RawDiagnosticsSettings,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawWorkbookSettings {
    #[serde(default)]
    ship_acquisition_enabled: bool,
    #[serde(default)]
    acquisition_update_policy: AcquisitionUpdatePolicy,
    acquisition_request_interval_seconds: u32,
}

impl Default for RawWorkbookSettings {
    fn default() -> Self {
        Self {
            ship_acquisition_enabled: false,
            acquisition_update_policy: AcquisitionUpdatePolicy::default(),
            acquisition_request_interval_seconds: 2,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawDiagnosticsSettings {
    detailed: bool,
}
impl Default for RawDiagnosticsSettings {
    fn default() -> Self {
        Self { detailed: true }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeviceSettings {
    mode: DeviceMode,
    adb_path: String,
    #[serde(default)]
    manager_path: String,
    serial: String,
    instance: String,
    game_package: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimeSettings {
    #[serde(default)]
    unload_after_sync: bool,
    connect_timeout_seconds: u32,
    startup_timeout_seconds: u32,
    #[serde(default)]
    agent_mapping_mode: AgentMappingMode,
    #[serde(default)]
    agent_visibility_mode: AgentVisibilityMode,
}

/// 设置文件无法形成明确、可移植且有界的运行配置。
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("设置已被其他操作修改，请重新打开设置后重试")]
    Changed,
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("设置文件 {path} 为 {actual} 字节，超过 {maximum} 字节上限")]
    FileTooLarge {
        path: PathBuf,
        actual: u64,
        maximum: u64,
    },
    #[error("settings.json 字段 {path} 无效: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("只支持 settings.json schema 1，实际为 {actual}")]
    UnsupportedSchema { actual: u32 },
    #[error("settings.json 字段 {field} 无效: {message}")]
    InvalidValue {
        field: &'static str,
        message: String,
    },
}

impl SettingsError {
    /// 返回不包含绝对路径和底层堆栈的稳定诊断摘要。
    pub fn summary(&self) -> String {
        match self {
            Self::Changed => self.to_string(),
            Self::ToolRoot(_) => "工具根目录路径无效".to_owned(),
            Self::Io { stage, .. } => format!("{stage} 失败"),
            Self::FileTooLarge {
                actual, maximum, ..
            } => format!("文件大小 {actual} 超过上限 {maximum}"),
            Self::Json { path, .. } => format!("字段 {path} 无效"),
            Self::UnsupportedSchema { actual } => format!("schema 版本 {actual} 不受支持"),
            Self::InvalidValue { field, message } => format!("{field}: {message}"),
        }
    }
}

fn tool_root_error_summary(error: &ToolRootError) -> String {
    match error {
        ToolRootError::InvalidRelativePath { .. } => "路径格式或边界无效".to_owned(),
        ToolRootError::UnsafePath { .. } => "路径未通过工具根目录安全校验".to_owned(),
        ToolRootError::PathConflict { .. } => "路径与现有条目冲突".to_owned(),
        ToolRootError::Io { operation, .. } => format!("{operation}失败"),
    }
}

fn validate_timeout(field: &'static str, value: u32) -> Result<(), SettingsError> {
    if !(1..=MAX_TIMEOUT_SECONDS).contains(&value) {
        return Err(SettingsError::InvalidValue {
            field,
            message: format!("只允许 1 至 {MAX_TIMEOUT_SECONDS} 秒，实际为 {value}"),
        });
    }
    Ok(())
}

fn validate_optional_adb_path(
    tool_root: &ToolRoot,
    value: &str,
    require_existing: bool,
) -> Result<(), SettingsError> {
    if value.is_empty() {
        return Ok(());
    }
    let result = if require_existing {
        tool_root.existing_file(Path::new(value)).map(|_| ())
    } else {
        tool_root
            .validated_relative_path(Path::new(value))
            .map(|_| ())
    };
    result.map_err(|error| SettingsError::InvalidValue {
        field: "device.adb_path",
        message: tool_root_error_summary(&error),
    })
}

fn validate_optional_manager_path(
    value: &str,
    require_existing: bool,
) -> Result<(), SettingsError> {
    if value.is_empty() {
        return Ok(());
    }
    let path = Path::new(value);
    let valid_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".exe"));
    if !path.is_absolute() || !valid_name {
        return Err(SettingsError::InvalidValue {
            field: "device.manager_path",
            message: "必须是模拟器管理器 .exe 的绝对路径".to_owned(),
        });
    }
    if require_existing && !path.is_file() {
        return Err(SettingsError::InvalidValue {
            field: "device.manager_path",
            message: "manual 模式指定的模拟器管理器不存在或不是普通文件".to_owned(),
        });
    }
    Ok(())
}

fn validate_optional_serial(value: &str) -> Result<(), SettingsError> {
    if value.is_empty() {
        return Ok(());
    }
    let address: SocketAddr = value.parse().map_err(|_| SettingsError::InvalidValue {
        field: "device.serial",
        message: "必须是 HOST:PORT 格式的回环地址".to_owned(),
    })?;
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(SettingsError::InvalidValue {
            field: "device.serial",
            message: "只允许非零端口的回环地址".to_owned(),
        });
    }
    Ok(())
}

fn validate_optional_instance(value: &str) -> Result<(), SettingsError> {
    if value.is_empty() {
        return Ok(());
    }
    let (provider, index) = value
        .split_once(':')
        .ok_or_else(|| SettingsError::InvalidValue {
            field: "device.instance",
            message: "必须使用 provider:索引 格式".to_owned(),
        })?;
    let provider_valid = !provider.is_empty()
        && provider.len() <= 32
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !provider_valid
        || index.len() > 8
        || index.is_empty()
        || !index.bytes().all(|byte: u8| byte.is_ascii_digit())
    {
        return Err(SettingsError::InvalidValue {
            field: "device.instance",
            message: "必须是 provider:索引，索引至多 8 位".to_owned(),
        });
    }
    Ok(())
}

fn validate_optional_package(value: &str) -> Result<(), SettingsError> {
    if value.is_empty() {
        return Ok(());
    }
    if !suzushiro_text_format::is_ascii_package_name(value) {
        return Err(SettingsError::InvalidValue {
            field: "device.game_package",
            message: "必须是受限的 Android 包名".to_owned(),
        });
    }
    Ok(())
}

fn require_manual_value(field: &'static str, value: &str) -> Result<(), SettingsError> {
    if value.is_empty() {
        return Err(SettingsError::InvalidValue {
            field,
            message: "manual 模式必须提供该字段".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{AgentMappingMode, AgentVisibilityMode, DeviceMode, Settings, SettingsSnapshot};
    use crate::adapters::tool_root::ToolRoot;
    use suzushiro_content_digest::sha256_bytes;

    const AUTO_SETTINGS: &str = r#"{
        "schema_version": 1,
        "device": {
            "mode": "auto",
            "adb_path": "",
            "manager_path": "",
            "serial": "",
            "instance": "",
            "game_package": ""
        },
        "runtime": {
            "connect_timeout_seconds": 30,
            "startup_timeout_seconds": 180
        }
    }"#;

    #[test]
    fn snapshot_identity_matches_the_validated_bytes_and_changes_when_preferences_change() {
        let fixture = TestDirectory::new("snapshot-identity");
        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();

        let snapshot = SettingsSnapshot::load(&fixture.root).unwrap();

        assert_eq!(
            snapshot.source_sha256(),
            sha256_bytes(AUTO_SETTINGS.as_bytes())
        );
        assert!(!snapshot.settings().preferences().ship_acquisition_enabled);
        let first_identity = snapshot.source_sha256().to_owned();
        Settings::save_preferences(
            &fixture.root,
            snapshot.settings().preferences(),
            crate::application::UserPreferences {
                ship_acquisition_enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
        let updated = SettingsSnapshot::load(&fixture.root).unwrap();
        assert_ne!(updated.source_sha256(), first_identity);
        assert!(updated.settings().preferences().ship_acquisition_enabled);
    }

    #[test]
    fn acquisition_request_interval_defaults_and_validates_safe_bounds() {
        let fixture = TestDirectory::new("acquisition-interval");
        let root = ToolRoot::open(&fixture.root).unwrap();
        assert_eq!(
            Settings::from_slice(&root, AUTO_SETTINGS.as_bytes())
                .unwrap()
                .acquisition_request_interval_seconds(),
            2
        );
        for (seconds, valid) in [
            (0, false),
            (1, false),
            (2, true),
            (10, true),
            (3600, true),
            (3601, false),
        ] {
            let mut value: serde_json::Value = serde_json::from_str(AUTO_SETTINGS).unwrap();
            value["workbook"] =
                serde_json::json!({"acquisition_request_interval_seconds": seconds});
            let result = Settings::from_slice(&root, &serde_json::to_vec(&value).unwrap());
            assert_eq!(result.is_ok(), valid, "{seconds}: {result:?}");
        }
    }

    #[test]
    fn acquisition_setting_defaults_off_and_persists_without_changing_other_settings() {
        let fixture = TestDirectory::new("acquisition");
        let path = fixture.root.join("settings.json");
        fs::write(&path, AUTO_SETTINGS).unwrap();
        assert!(
            !Settings::load(&fixture.root)
                .unwrap()
                .preferences()
                .ship_acquisition_enabled
        );
        assert_eq!(
            Settings::load(&fixture.root).unwrap().preferences(),
            crate::application::UserPreferences::default()
        );
        let original: serde_json::Value = serde_json::from_str(AUTO_SETTINGS).unwrap();
        for enabled in [true, false] {
            Settings::save_preferences(
                &fixture.root,
                Settings::load(&fixture.root).unwrap().preferences(),
                crate::application::UserPreferences {
                    ship_acquisition_enabled: enabled,
                    acquisition_update_policy: crate::application::AcquisitionUpdatePolicy::Refresh,
                    detailed_diagnostics: false,
                    unload_after_sync: true,
                },
            )
            .unwrap();
            assert_eq!(
                Settings::load(&fixture.root)
                    .unwrap()
                    .preferences()
                    .ship_acquisition_enabled,
                enabled
            );
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["device"], original["device"]);
            assert_eq!(
                saved["runtime"]["connect_timeout_seconds"],
                original["runtime"]["connect_timeout_seconds"]
            );
            let prefs = Settings::load(&fixture.root).unwrap().preferences();
            assert_eq!(
                prefs.acquisition_update_policy,
                crate::application::AcquisitionUpdatePolicy::Refresh
            );
            assert!(!prefs.detailed_diagnostics);
            assert!(prefs.unload_after_sync);
            assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 2);
        }
        fs::write(&path, b"{invalid}").unwrap();
        assert!(
            Settings::save_preferences(
                &fixture.root,
                crate::application::UserPreferences::default(),
                crate::application::UserPreferences::default()
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"{invalid}");
    }

    #[test]
    fn stale_preferences_are_rejected_without_replacing_the_saved_file() {
        let fixture = TestDirectory::new("stale-preferences");
        let path = fixture.root.join("settings.json");
        fs::write(&path, AUTO_SETTINGS).unwrap();
        let original = Settings::load(&fixture.root).unwrap().preferences();
        let newer = crate::application::UserPreferences {
            detailed_diagnostics: false,
            ..original
        };
        Settings::save_preferences(&fixture.root, original, newer).unwrap();
        let saved = fs::read(&path).unwrap();
        let error = Settings::save_preferences(
            &fixture.root,
            original,
            crate::application::UserPreferences {
                ship_acquisition_enabled: true,
                ..original
            },
        )
        .unwrap_err();
        assert!(matches!(error, super::SettingsError::Changed));
        assert!(error.summary().contains("重新打开设置"));
        assert_eq!(fs::read(&path).unwrap(), saved);
        // 非偏好字段的编辑沿用保存当下的值，不构成偏好冲突。
        let mut current: serde_json::Value = serde_json::from_slice(&saved).unwrap();
        current["runtime"]["connect_timeout_seconds"] = 40.into();
        fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
        Settings::save_preferences(&fixture.root, newer, original).unwrap();
        assert_eq!(
            Settings::load(&fixture.root)
                .unwrap()
                .runtime()
                .connect_timeout_seconds(),
            40
        );
    }

    #[test]
    fn concurrent_preferences_saves_accept_only_one_original_baseline() {
        use std::sync::{Arc, Barrier};
        let fixture = TestDirectory::new("concurrent-preferences");
        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();
        let original = Settings::load(&fixture.root).unwrap().preferences();
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let root = fixture.root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let preferences = crate::application::UserPreferences {
                        ship_acquisition_enabled: true,
                        detailed_diagnostics: index % 2 == 0,
                        unload_after_sync: index % 3 == 0,
                        ..original
                    };
                    barrier.wait();
                    (
                        preferences,
                        Settings::save_preferences(&root, original, preferences),
                    )
                })
            })
            .collect();
        let mut winners = Vec::new();
        for thread in threads {
            let (preferences, result) = thread.join().unwrap();
            match result {
                Ok(()) => winners.push(preferences),
                Err(super::SettingsError::Changed) => {}
                Err(error) => panic!("并发保存失败: {error}"),
            }
        }
        assert_eq!(winners.len(), 1);
        assert_eq!(
            Settings::load(&fixture.root).unwrap().preferences(),
            winners[0]
        );
    }

    #[test]
    fn package_validation_preserves_optional_value_and_error_field() {
        for value in ["", "com.example.app", "Com.Example_1"] {
            super::validate_optional_package(value).unwrap();
        }
        for value in ["single", "a..b", "a.b;id"] {
            let error = super::validate_optional_package(value).unwrap_err();
            assert!(matches!(error, super::SettingsError::InvalidValue {
                field: "device.game_package", ref message
            } if message == "必须是受限的 Android 包名"));
        }
    }

    #[test]
    fn accepts_namespaced_instance_and_rejects_unknown_fields() {
        let fixture = TestDirectory::new("instance-fields");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let valid = AUTO_SETTINGS.replace("\"instance\": \"\"", "\"instance\": \"mumu12:0\"");
        assert_eq!(
            Settings::from_slice(&root, valid.as_bytes())
                .unwrap()
                .device()
                .instance(),
            "mumu12:0"
        );
        let unknown = AUTO_SETTINGS.replace("\"instance\": \"\"", "\"mumu_instance\": \"0\"");
        assert!(Settings::from_slice(&root, unknown.as_bytes()).is_err());
        super::validate_optional_instance("ldplayer:0").unwrap();
        super::validate_optional_instance("mumu12:12").unwrap();
        for invalid in ["0", ":0", "ldplayer:", "ldplayer:1:2", "x;id:0"] {
            assert!(
                super::validate_optional_instance(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn parses_strict_auto_settings() {
        let fixture: TestDirectory = TestDirectory::new("valid");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let settings: Settings = Settings::from_slice(&root, AUTO_SETTINGS.as_bytes()).unwrap();

        assert_eq!(settings.device().mode(), DeviceMode::Auto);
        assert_eq!(settings.runtime().connect_timeout_seconds(), 30);
        assert_eq!(settings.runtime().startup_timeout_seconds(), 180);
        assert_eq!(
            settings.runtime().agent_mapping_mode(),
            AgentMappingMode::Memfd
        );
        assert_eq!(
            settings.runtime().agent_visibility_mode(),
            AgentVisibilityMode::Normal
        );
    }

    #[test]
    fn parses_explicit_anonymous_mapping_mode_and_rejects_unknown_values() {
        let fixture: TestDirectory = TestDirectory::new("mapping-mode");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let anonymous = AUTO_SETTINGS.replace(
            "\"startup_timeout_seconds\": 180",
            "\"startup_timeout_seconds\": 180, \"agent_mapping_mode\": \"anonymous_remap\"",
        );
        let settings: Settings = Settings::from_slice(&root, anonymous.as_bytes()).unwrap();
        assert_eq!(
            settings.runtime().agent_mapping_mode(),
            AgentMappingMode::AnonymousRemap
        );

        let invalid = anonymous.replace("anonymous_remap", "unknown");
        assert!(Settings::from_slice(&root, invalid.as_bytes()).is_err());
    }

    #[test]
    fn parses_explicit_visibility_modes_and_rejects_unknown_values() {
        let fixture: TestDirectory = TestDirectory::new("visibility-mode");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let hidden = AUTO_SETTINGS.replace(
            "\"startup_timeout_seconds\": 180",
            "\"startup_timeout_seconds\": 180, \"agent_visibility_mode\": \"solist_hidden\"",
        );
        let settings: Settings = Settings::from_slice(&root, hidden.as_bytes()).unwrap();
        assert_eq!(
            settings.runtime().agent_visibility_mode(),
            AgentVisibilityMode::SolistHidden
        );

        let strongest = hidden.replace("solist_hidden", "solist_and_elf_header");
        let settings: Settings = Settings::from_slice(&root, strongest.as_bytes()).unwrap();
        assert_eq!(
            settings.runtime().agent_visibility_mode(),
            AgentVisibilityMode::SolistAndElfHeader
        );

        let invalid = hidden.replace("solist_hidden", "unknown");
        assert!(Settings::from_slice(&root, invalid.as_bytes()).is_err());
    }

    #[test]
    fn reports_nested_json_path_and_rejects_unknown_fields() {
        let fixture: TestDirectory = TestDirectory::new("json-path");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let invalid: String = AUTO_SETTINGS.replace(
            "\"connect_timeout_seconds\": 30",
            "\"connect_timeout_seconds\": \"30\"",
        );
        let error: String = Settings::from_slice(&root, invalid.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(error.contains("runtime.connect_timeout_seconds"));

        let unknown: String = AUTO_SETTINGS.replace(
            "\"startup_timeout_seconds\": 180",
            "\"startup_timeout_seconds\": 180, \"fallback\": true",
        );
        assert!(Settings::from_slice(&root, unknown.as_bytes()).is_err());
    }

    #[test]
    fn auto_mode_accepts_missing_safe_adb_hint_but_rejects_unsafe_paths() {
        let fixture: TestDirectory = TestDirectory::new("auto-adb-hint");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let missing: String = AUTO_SETTINGS.replace(
            "\"adb_path\": \"\"",
            "\"adb_path\": \"runtime/removed-adb/adb.exe\"",
        );

        assert!(Settings::from_slice(&root, missing.as_bytes()).is_ok());

        let escaping: String =
            AUTO_SETTINGS.replace("\"adb_path\": \"\"", "\"adb_path\": \"../outside/adb.exe\"");
        assert!(Settings::from_slice(&root, escaping.as_bytes()).is_err());
    }

    #[test]
    fn auto_mode_accepts_stale_absolute_manager_hint_but_rejects_unsafe_paths() {
        let fixture = TestDirectory::new("auto-manager-hint");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let missing = fixture.root.join("removed/MuMuManager.exe");
        let quoted = serde_json::to_string(missing.to_str().unwrap()).unwrap();
        let stale = AUTO_SETTINGS.replace(
            "\"manager_path\": \"\"",
            &format!("\"manager_path\": {quoted}"),
        );
        assert!(Settings::from_slice(&root, stale.as_bytes()).is_ok());

        let relative = AUTO_SETTINGS.replace(
            "\"manager_path\": \"\"",
            "\"manager_path\": \"MuMuManager.exe\"",
        );
        assert!(Settings::from_slice(&root, relative.as_bytes()).is_err());

        let wrong_name = fixture.root.join("removed/manager.txt");
        let quoted = serde_json::to_string(wrong_name.to_str().unwrap()).unwrap();
        let wrong_name = AUTO_SETTINGS.replace(
            "\"manager_path\": \"\"",
            &format!("\"manager_path\": {quoted}"),
        );
        assert!(Settings::from_slice(&root, wrong_name.as_bytes()).is_err());
    }

    #[test]
    fn settings_summary_does_not_expose_tool_root_paths() {
        let fixture: TestDirectory = TestDirectory::new("summary-path");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let escaping: String =
            AUTO_SETTINGS.replace("\"adb_path\": \"\"", "\"adb_path\": \"../outside/adb.exe\"");

        let error = Settings::from_slice(&root, escaping.as_bytes()).unwrap_err();
        let summary = error.summary();

        assert!(summary.contains("device.adb_path"));
        assert!(summary.contains("路径格式或边界无效"));
        assert!(!summary.contains(fixture.root.to_string_lossy().as_ref()));
    }

    #[test]
    fn auto_mode_treats_invalid_target_values_as_stale_hints() {
        let fixture: TestDirectory = TestDirectory::new("auto-target-hints");
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let stale: String = AUTO_SETTINGS
            .replace("\"serial\": \"\"", "\"serial\": \"not-a-loopback-address\"")
            .replace("\"instance\": \"\"", "\"instance\": \"stale\"")
            .replace(
                "\"game_package\": \"\"",
                "\"game_package\": \"not a package\"",
            );

        assert!(Settings::from_slice(&root, stale.as_bytes()).is_ok());
    }

    #[test]
    fn manual_mode_requires_complete_local_target() {
        let fixture: TestDirectory = TestDirectory::new("manual");
        fs::create_dir_all(fixture.root.join("runtime/adb")).unwrap();
        fs::write(fixture.root.join("runtime/adb/adb.exe"), b"fixture").unwrap();
        let root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
        let complete: String = AUTO_SETTINGS
            .replace("\"mode\": \"auto\"", "\"mode\": \"manual\"")
            .replace(
                "\"adb_path\": \"\"",
                "\"adb_path\": \"runtime/adb/adb.exe\"",
            )
            .replace("\"serial\": \"\"", "\"serial\": \"127.0.0.1:16384\"")
            .replace("\"instance\": \"\"", "\"instance\": \"mumu12:0\"")
            .replace(
                "\"game_package\": \"\"",
                "\"game_package\": \"com.bilibili.azurlane\"",
            );
        assert!(Settings::from_slice(&root, complete.as_bytes()).is_ok());

        let incomplete: String =
            complete.replace("\"serial\": \"127.0.0.1:16384\"", "\"serial\": \"\"");
        assert!(Settings::from_slice(&root, incomplete.as_bytes()).is_err());

        fs::remove_file(fixture.root.join("runtime/adb/adb.exe")).unwrap();
        assert!(Settings::from_slice(&root, complete.as_bytes()).is_err());
    }

    struct TestDirectory {
        parent: PathBuf,
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home: PathBuf = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let parent: PathBuf = home
                .join("suzushiro/scratch/azlw-settings-tests")
                .join(format!("{label}-{}", unique_suffix()));
            let root: PathBuf = parent.join("tool-root");
            fs::create_dir_all(&root).unwrap();
            Self { parent, root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }

    fn unique_suffix() -> String {
        let mut bytes: [u8; 16] = [0; 16];
        getrandom::fill(&mut bytes).unwrap();
        format!("{}-{:032x}", std::process::id(), u128::from_le_bytes(bytes))
    }
}

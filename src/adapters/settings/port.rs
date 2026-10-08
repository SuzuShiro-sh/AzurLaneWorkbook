//! 将严格设置加载器接入应用层的脱敏读取端口。

use std::cell::RefCell;

use crate::adapters::settings::{DeviceMode, Settings, SettingsError};
use crate::adapters::tool_root::ToolRoot;
use crate::application::{
    AppError, AppErrorCode, DoctorSettingsCheck, SettingsPort, SettingsSummaryReport,
    UserPreferences,
};

/// 使用固定工具根目录读取运行设置摘要。
pub(crate) struct JsonSettingsPort {
    tool_root: ToolRoot,
    cached: RefCell<Option<Result<Settings, String>>>,
}

impl JsonSettingsPort {
    /// 固定受控工具根目录，不接受外部设置路径。
    #[cfg(test)]
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self {
            tool_root,
            cached: RefCell::new(None),
        }
    }

    /// 使用本次操作已经校验的设置。保存后再读会重新打开文件。
    pub(crate) fn from_snapshot(tool_root: ToolRoot, settings: Settings) -> Self {
        Self {
            tool_root,
            cached: RefCell::new(Some(Ok(settings))),
        }
    }

    /// 保留本次操作已经失败的设置诊断，读取时不再打开文件。
    pub(crate) fn from_read_failure(tool_root: ToolRoot, summary: String) -> Self {
        Self {
            tool_root,
            cached: RefCell::new(Some(Err(summary))),
        }
    }
}

impl SettingsPort for JsonSettingsPort {
    fn read_settings(&self) -> Result<SettingsSummaryReport, AppError> {
        let settings = self.load().map_err(map_settings_error)?;
        Ok(SettingsSummaryReport::new(DoctorSettingsCheck::new(
            device_mode(settings.device().mode()),
            !settings.device().adb_path().is_empty(),
            !settings.device().serial().is_empty(),
            !settings.device().instance().is_empty(),
            !settings.device().game_package().is_empty(),
            settings.runtime().connect_timeout_seconds(),
            settings.runtime().startup_timeout_seconds(),
        )))
    }

    fn read_preferences(&self) -> Result<UserPreferences, AppError> {
        self.load()
            .map(|settings| settings.preferences())
            .map_err(map_settings_error)
    }

    fn save_preferences(
        &self,
        original: UserPreferences,
        preferences: UserPreferences,
    ) -> Result<(), AppError> {
        let saved = Settings::save_preferences(self.tool_root.as_path(), original, preferences)
            .map_err(|source| map_settings_error_at("settings.save", source));
        if saved.is_ok() {
            *self.cached.borrow_mut() = None;
        }
        saved
    }
}

impl JsonSettingsPort {
    fn load(&self) -> Result<Settings, SettingsError> {
        if let Some(cached) = self.cached.borrow().clone() {
            return cached.map_err(|detail| SettingsError::InvalidValue {
                field: "settings.json",
                message: detail,
            });
        }
        let settings = Settings::load(self.tool_root.as_path())?;
        *self.cached.borrow_mut() = Some(Ok(settings.clone()));
        Ok(settings)
    }
}

fn device_mode(mode: DeviceMode) -> &'static str {
    match mode {
        DeviceMode::Auto => "auto",
        DeviceMode::Manual => "manual",
    }
}

fn map_settings_error(source: SettingsError) -> AppError {
    map_settings_error_at("settings.read", source)
}

fn map_settings_error_at(stage: &'static str, source: SettingsError) -> AppError {
    let message = match &source {
        SettingsError::Changed => "设置已被其他操作修改，请重新打开设置后重试",
        SettingsError::Io { .. } | SettingsError::ToolRoot(_) => "设置文件访问失败，请查看具体原因",
        _ => "settings.json 未通过严格校验",
    };
    AppError::from_source(stage, AppErrorCode::SettingsInvalid, message, source)
        .with_context("path", "settings.json")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::JsonSettingsPort;
    use crate::adapters::tool_root::ToolRoot;
    use crate::application::SettingsPort;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn save_error_keeps_the_original_io_source() {
        use std::error::Error;
        let error = super::map_settings_error_at(
            "settings.save",
            crate::adapters::settings::SettingsError::Io {
                stage: "settings.lock",
                path: PathBuf::from("settings.lock"),
                source: std::io::Error::from_raw_os_error(32),
            },
        );
        let settings = error.source().unwrap();
        let cause = settings
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(cause.raw_os_error(), Some(32));
        assert!(settings.to_string().contains("settings.lock"));
        assert!(error.message().contains("访问失败"));
    }

    const AUTO_SETTINGS: &str = r#"{
        "schema_version": 1,
        "device": {
            "mode": "auto",
            "adb_path": "",
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
    fn reads_a_deidentified_settings_summary_through_the_application_port() {
        let fixture = TestDirectory::new("valid");
        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let report = JsonSettingsPort::new(root).read_settings().unwrap();

        assert_eq!(report.message(), "运行设置读取完成");
        assert_eq!(report.settings().device_mode, "auto");
        assert!(!report.settings().adb_path_configured);
        assert_eq!(report.settings().connect_timeout_seconds, 30);
    }

    #[test]
    fn snapshot_preferences_stay_stable_until_this_port_saves() {
        let fixture = TestDirectory::new("snapshot");
        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let settings = crate::adapters::settings::Settings::load(&fixture.root).unwrap();
        let port = JsonSettingsPort::from_snapshot(root, settings);
        fs::write(fixture.root.join("settings.json"), b"{").unwrap();

        let preferences = port.read_preferences().unwrap();
        assert!(preferences.detailed_diagnostics);

        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();
        port.save_preferences(
            preferences,
            crate::application::UserPreferences {
                detailed_diagnostics: false,
                ..crate::application::UserPreferences::default()
            },
        )
        .unwrap();
        let updated = port.read_preferences().unwrap();
        assert!(!updated.detailed_diagnostics);
    }

    #[test]
    fn read_failure_keeps_the_original_summary_when_the_file_changes() {
        let fixture = TestDirectory::new("read-failure");
        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();
        let port = JsonSettingsPort::from_read_failure(root, "字段 $ 无效".to_owned());
        fs::write(fixture.root.join("settings.json"), AUTO_SETTINGS).unwrap();

        let error = port.read_preferences().unwrap_err();
        let source = std::error::Error::source(&error).unwrap();

        assert!(source.to_string().contains("字段 $ 无效"));
    }

    #[test]
    fn maps_invalid_settings_to_a_stable_application_error_without_paths() {
        let fixture = TestDirectory::new("invalid");
        fs::write(fixture.root.join("settings.json"), b"{").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let error = JsonSettingsPort::new(root).read_settings().unwrap_err();

        assert_eq!(error.code().as_str(), "SETTINGS_INVALID");
        assert_eq!(error.stage(), "settings.read");
        assert_eq!(
            error.context().get("path").map(String::as_str),
            Some("settings.json")
        );
        assert!(
            !error
                .to_string()
                .contains(fixture.root.to_string_lossy().as_ref())
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
                .join("suzushiro/scratch/azlw-settings-port-tests")
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

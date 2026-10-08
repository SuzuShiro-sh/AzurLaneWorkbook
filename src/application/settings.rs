//! 定义设置读取后供 CLI、GUI 和 doctor 共用的脱敏摘要。

use serde::{Deserialize, Serialize};

use super::DoctorSettingsCheck;

/// 设置摘要报告的稳定 JSON 外壳版本。
pub const SETTINGS_SUMMARY_SCHEMA_VERSION: u32 = 1;

/// 设置文件严格校验完成后的脱敏摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SettingsSummaryReport {
    message: &'static str,
    schema_version: u32,
    settings: DoctorSettingsCheck,
}

impl SettingsSummaryReport {
    /// 从已严格校验的设置建立不包含路径和目标原文的报告。
    pub(crate) fn new(settings: DoctorSettingsCheck) -> Self {
        Self {
            message: "运行设置读取完成",
            schema_version: SETTINGS_SUMMARY_SCHEMA_VERSION,
            settings,
        }
    }

    /// 返回面向用户的读取结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回设置摘要报告版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回不包含 ADB 路径、serial 和包名原文的设置摘要。
    pub const fn settings(&self) -> &DoctorSettingsCheck {
        &self.settings
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SETTINGS_SUMMARY_SCHEMA_VERSION, SettingsSummaryReport};
    use crate::application::DoctorSettingsCheck;

    #[test]
    fn serializes_only_the_deidentified_settings_summary() {
        let report = SettingsSummaryReport::new(DoctorSettingsCheck::new(
            "auto", true, true, false, true, 30, 180,
        ));

        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "message": "运行设置读取完成",
                "schema_version": SETTINGS_SUMMARY_SCHEMA_VERSION,
                "settings": {
                    "status": "ready",
                    "device_mode": "auto",
                    "adb_path_configured": true,
                    "serial_configured": true,
                    "instance_configured": false,
                    "game_package_configured": true,
                    "connect_timeout_seconds": 30,
                    "startup_timeout_seconds": 180,
                },
            })
        );
    }
}

/// 获取方式在线更新策略。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionUpdatePolicy {
    #[default]
    UseCache,
    Refresh,
}

/// 设置窗口和持久化共用的用户偏好。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserPreferences {
    pub ship_acquisition_enabled: bool,
    pub acquisition_update_policy: AcquisitionUpdatePolicy,
    pub detailed_diagnostics: bool,
    pub unload_after_sync: bool,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            ship_acquisition_enabled: false,
            acquisition_update_policy: AcquisitionUpdatePolicy::UseCache,
            detailed_diagnostics: true,
            unload_after_sync: false,
        }
    }
}

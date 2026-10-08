//! 将模拟器管理器只读实例查询接入应用目录端口。

use crate::adapters::settings::DeviceSettings;
use crate::application::{
    AppError, AppErrorCode, EmulatorInstanceCatalogPort, EmulatorInstanceCatalogReport,
};

use crate::application::EmulatorInstanceCandidate;
use std::path::Path;
use suzushiro_emulator::discovery::{DiscoveryRequest, ResolvedManager, discover_report};
use suzushiro_emulator::{EmulatorError, adapter_for_manager};

/// 实例目录使用的设置片段。已有片段或失败诊断时不再打开设置文件。
enum CatalogSettings {
    Fragment {
        instance: String,
        manager_path: String,
    },
    Unavailable(String),
}

/// 使用设备设置限定管理器安装并标记选中的实例。
pub(crate) struct EmulatorManagerInstanceCatalogPort {
    settings: CatalogSettings,
}

impl EmulatorManagerInstanceCatalogPort {
    /// 使用本次操作已经校验的设备提示，不再读取设置文件。
    pub(crate) fn from_device(device: &DeviceSettings) -> Self {
        Self {
            settings: CatalogSettings::Fragment {
                instance: device.instance().to_owned(),
                manager_path: device.manager_path().to_owned(),
            },
        }
    }

    /// 保留本次操作已经失败的设置诊断，列出实例时不再打开文件。
    pub(crate) fn unavailable(summary: &str) -> Self {
        Self {
            settings: CatalogSettings::Unavailable(summary.to_owned()),
        }
    }

    fn discovery_hints(&self) -> Result<(String, String), AppError> {
        match &self.settings {
            CatalogSettings::Fragment {
                instance,
                manager_path,
            } => Ok((instance.clone(), manager_path.clone())),
            CatalogSettings::Unavailable(summary) => Err(AppError::from_source(
                "emulator.instances.settings",
                AppErrorCode::SettingsInvalid,
                "刷新模拟器实例前 settings.json 未通过严格校验",
                std::io::Error::other(summary.clone()),
            )),
        }
    }
}

impl EmulatorInstanceCatalogPort for EmulatorManagerInstanceCatalogPort {
    fn list_emulator_instances(&self) -> Result<EmulatorInstanceCatalogReport, AppError> {
        let (instance, manager_path) = self.discovery_hints()?;
        read_emulator_instance_catalog(
            Some(instance.as_str()),
            (!manager_path.is_empty()).then(|| std::path::Path::new(manager_path.as_str())),
        )
        .map_err(map_discovery_error)
    }
}

fn map_discovery_error(source: EmulatorError) -> AppError {
    let code = match &source {
        EmulatorError::InvalidOption { .. } => AppErrorCode::SettingsInvalid,
        EmulatorError::Discovery { .. } => AppErrorCode::EmulatorNotFound,
        EmulatorError::AmbiguousManager { .. } => AppErrorCode::EmulatorManagerAmbiguous,
        EmulatorError::Registry { .. } | EmulatorError::ManagerCommand { .. } => {
            AppErrorCode::RuntimeBootstrapFailed
        }
        EmulatorError::Io { .. } => AppErrorCode::ApplicationInitializationFailed,
    };
    AppError::from_source(
        "emulator.instances.catalog",
        code,
        "模拟器实例目录读取失败，请检查模拟器安装和管理器状态",
        source,
    )
    .with_context("operation", "read_only_manager_info")
    .with_context("launch_side_effect", "false")
}

/// 将归一化实例转换成应用目录；不建立 ADB 或游戏会话。
pub(in crate::adapters::device) fn read_emulator_instance_catalog(
    selected_instance: Option<&str>,
    manager_hint: Option<&Path>,
) -> Result<EmulatorInstanceCatalogReport, EmulatorError> {
    let discovery = discover_report(&DiscoveryRequest {
        manager_hint,
        include_running: true,
        strict_hint: manager_hint.is_some(),
    })?;
    Ok(
        catalog_from_managers(discovery.managers, selected_instance)?
            .with_warnings(discovery.warnings),
    )
}

fn catalog_from_managers(
    managers: Vec<ResolvedManager>,
    selected_instance: Option<&str>,
) -> Result<EmulatorInstanceCatalogReport, EmulatorError> {
    let selected_instance = selected_instance
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mut candidates = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for manager in managers {
        let adapter = adapter_for_manager(&manager.executable)?;
        for instance in manager.instances.into_values() {
            let key = format!("{}:{}", adapter.id(), instance.index);
            if !seen.insert(key.clone()) {
                return Err(EmulatorError::AmbiguousManager {
                    message: format!(
                        "多个 {} 安装包含相同实例，请用 manager_path 指定安装",
                        adapter.display_name()
                    ),
                });
            }
            let state = match instance.catalog_state() {
                suzushiro_emulator::TargetState::Ready => {
                    crate::application::EmulatorInstanceState::Ready
                }
                suzushiro_emulator::TargetState::Starting => {
                    crate::application::EmulatorInstanceState::Starting
                }
                suzushiro_emulator::TargetState::Stopped => {
                    crate::application::EmulatorInstanceState::Stopped
                }
                suzushiro_emulator::TargetState::Unavailable => {
                    crate::application::EmulatorInstanceState::Unavailable
                }
            };
            let android_version = instance.android_version_label().to_owned();
            let selected = selected_instance.as_deref() == Some(key.as_str());
            candidates.push(
                EmulatorInstanceCandidate::new(
                    key,
                    format!("{} · {}", adapter.display_name(), instance.name),
                    state,
                    android_version,
                    selected,
                )
                .map_err(|source| EmulatorError::Discovery {
                    message: format!("实例标识无效: {source}"),
                })?,
            );
        }
    }
    Ok(EmulatorInstanceCatalogReport::new(
        selected_instance,
        candidates,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::device::test_support::sample_instances;
    use crate::adapters::settings::Settings;

    #[test]
    fn invalid_bound_manager_preserves_error_without_discovering_other_installations() {
        let root =
            std::env::temp_dir().join(format!("azlw-missing-bound-manager-{}", std::process::id()));
        let manager = root.join("MuMuManager.exe");
        assert!(!root.exists());

        let result = read_emulator_instance_catalog(Some("mumu12:0"), Some(&manager));

        assert!(matches!(
            result,
            Err(EmulatorError::Io {
                stage: "emulator.canonicalize_manager",
                path,
                ..
            }) if path == manager
        ));
    }

    #[test]
    fn provider_catalogs_keep_distinct_ids_and_selected_state() {
        let report = catalog_from_managers(
            vec![
                ResolvedManager {
                    executable: "MuMuManager.exe".into(),
                    instances: sample_instances(),
                },
                ResolvedManager {
                    executable: "ldconsole.exe".into(),
                    instances: sample_instances(),
                },
            ],
            Some("ldplayer:0"),
        )
        .unwrap();
        let value = serde_json::to_value(report).unwrap();
        let candidates = value["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0]["instance_id"], "mumu12:0");
        assert_eq!(candidates[2]["instance_id"], "ldplayer:0");
        assert_eq!(candidates[0]["selected"], false);
        assert_eq!(candidates[2]["selected"], true);
        assert_eq!(candidates[0]["state"], "ready");
        assert_eq!(candidates[1]["state"], "stopped");
    }

    #[test]
    fn duplicate_provider_instances_require_an_installation_hint() {
        let result = catalog_from_managers(
            vec![
                ResolvedManager {
                    executable: "first/MuMuManager.exe".into(),
                    instances: sample_instances(),
                },
                ResolvedManager {
                    executable: "second/MuMuManager.exe".into(),
                    instances: sample_instances(),
                },
            ],
            None,
        );
        assert!(matches!(
            result,
            Err(EmulatorError::AmbiguousManager { .. })
        ));
    }

    #[test]
    fn device_fragment_remains_after_the_settings_file_is_removed() {
        let root = std::env::temp_dir().join(format!(
            "azlw-emulator-settings-fragment-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            include_str!("../../../settings.json"),
        )
        .unwrap();
        let settings = Settings::load(&root).unwrap();
        let port = EmulatorManagerInstanceCatalogPort::from_device(settings.device());
        std::fs::remove_file(root.join("settings.json")).unwrap();

        let (instance, manager_path) = port.discovery_hints().unwrap();

        assert_eq!(instance, settings.device().instance());
        assert_eq!(manager_path, settings.device().manager_path());
        assert!(Settings::load(&root).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unavailable_settings_keep_the_original_diagnostic() {
        let port = EmulatorManagerInstanceCatalogPort::unavailable("字段 $ 无效");
        let error = port.discovery_hints().unwrap_err();
        let source = std::error::Error::source(&error).unwrap();
        assert!(source.to_string().contains("字段 $ 无效"));
    }
}

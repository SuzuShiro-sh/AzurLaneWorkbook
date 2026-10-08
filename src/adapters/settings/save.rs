//! 原子保存用户偏好，保留设备、运行参数及其他现有设置。

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use super::{SETTINGS_RELATIVE_PATH, Settings, SettingsError, ToolRoot};

impl Settings {
    /// 校验编辑基线并原子保存用户偏好，保留其他设置字段。
    pub fn save_preferences(
        tool_root: &Path,
        expected: crate::application::UserPreferences,
        preferences: crate::application::UserPreferences,
    ) -> Result<(), SettingsError> {
        let root = ToolRoot::open(tool_root)?;
        // 在读取基线前取得锁，并保持到原子替换结束，防止并发保存都通过比较。
        let _lock = root
            .lock_file(
                Path::new("settings.lock"),
                std::time::Duration::from_secs(5),
            )
            .map_err(|source| SettingsError::Io {
                stage: "settings.lock",
                path: root.as_path().join("settings.lock"),
                source,
            })?;
        let (path, original, _source_sha256) = Self::read_snapshot(&root)?;
        if Self::from_slice(&root, &original)?.preferences() != expected {
            return Err(SettingsError::Changed);
        }
        let io_error = |source| SettingsError::Io {
            stage: "settings.save",
            path: path.clone(),
            source,
        };
        let mut value: serde_json::Value =
            serde_json::from_slice(&original).map_err(|source| SettingsError::Json {
                path: "$".to_owned(),
                source,
            })?;
        value["workbook"]["ship_acquisition_enabled"] = preferences.ship_acquisition_enabled.into();
        value["workbook"]["acquisition_update_policy"] =
            serde_json::to_value(preferences.acquisition_update_policy).map_err(|source| {
                SettingsError::Json {
                    path: "workbook.acquisition_update_policy".to_owned(),
                    source,
                }
            })?;
        value["runtime"]["unload_after_sync"] = preferences.unload_after_sync.into();
        value["diagnostics"]["detailed"] = preferences.detailed_diagnostics.into();
        let mut bytes =
            serde_json::to_vec_pretty(&value).map_err(|source| SettingsError::Json {
                path: "$".to_owned(),
                source,
            })?;
        bytes.push(b'\n');
        Self::from_slice(&root, &bytes)?;
        let mut token = [0_u8; 8];
        getrandom::fill(&mut token).map_err(|error| io_error(io::Error::other(error)))?;
        let relative = format!(".settings-{:016x}.tmp", u64::from_le_bytes(token));
        let temporary = root.prepare_new_file(Path::new(&relative))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(io_error)?;
        let result = (|| {
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            drop(file);
            root.existing_file(Path::new(SETTINGS_RELATIVE_PATH))?;
            if fs::read(&path).map_err(io_error)? != original {
                return Err(SettingsError::Changed);
            }
            // 同目录重命名原子替换，写入失败时原设置仍保持完整。
            fs::rename(&temporary, &path).map_err(io_error)
        })();
        if result.is_err() {
            fs::remove_file(&temporary).map_err(|cleanup| {
                io_error(io::Error::other(format!(
                    "{}；清理设置临时文件失败：{cleanup}",
                    result.as_ref().unwrap_err()
                )))
            })?;
        }
        result
    }
}

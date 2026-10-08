use super::{EmulatorAdapter, EmulatorError, validate_instance_index};
use std::path::Path;

use super::ldplayer::LdPlayerAdapter;
use super::mumu12::Mumu12Adapter;
pub(super) static MUMU: Mumu12Adapter = Mumu12Adapter;
pub(super) static LDPLAYER: LdPlayerAdapter = LdPlayerAdapter;

/// 新提供方在此注册，发现、选择、启动和运行态传输共用同一契约。
pub static ADAPTERS: &[&dyn EmulatorAdapter] = &[&MUMU, &LDPLAYER];

pub fn adapter_for_manager(manager: &Path) -> Result<&'static dyn EmulatorAdapter, EmulatorError> {
    let name = manager.file_name().and_then(|v| v.to_str()).unwrap_or("");
    ADAPTERS
        .iter()
        .copied()
        .find(|adapter| {
            adapter
                .manager_names()
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
        })
        .ok_or_else(|| EmulatorError::Discovery {
            message: format!("未注册的模拟器管理器: {}", manager.display()),
        })
}

/// 提供方与实例索引共同组成选择标识。
pub fn split_selection(value: &str) -> Result<(&str, &str), EmulatorError> {
    let (provider, index) = value
        .split_once(':')
        .ok_or_else(|| EmulatorError::InvalidOption {
            field: "instance",
            message: "必须使用 provider:索引 格式".to_owned(),
        })?;
    if !ADAPTERS.iter().any(|adapter| adapter.id() == provider) {
        return Err(EmulatorError::InvalidOption {
            field: "instance",
            message: format!("未知模拟器提供方: {provider}"),
        });
    }
    validate_instance_index(index)?;
    Ok((provider, index))
}

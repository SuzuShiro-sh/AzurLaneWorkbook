//! 将探针连接选项转换为模拟器发现请求，并选择唯一安装。
#[cfg(target_os = "windows")]
use super::{PortableMode, PortableProbeError, PortableProbeOptions};
#[cfg(target_os = "windows")]
pub(super) use suzushiro_emulator::discovery::ResolvedManager;
#[cfg(target_os = "windows")]
use suzushiro_emulator::discovery::{DiscoveryRequest, discover_managers as discover};
#[cfg(target_os = "windows")]
use suzushiro_emulator::{adapter_for_manager, split_selection};
#[cfg(target_os = "windows")]
pub(super) fn resolve_manager(
    options: &PortableProbeOptions,
) -> Result<ResolvedManager, PortableProbeError> {
    let mut managers = discover_managers(options)?;
    if let Some(selection) = &options.instance_hint {
        let (provider, _) = split_selection(selection)?;
        managers.retain(|manager| {
            adapter_for_manager(&manager.executable).is_ok_and(|adapter| adapter.id() == provider)
        });
    }
    match managers.len() {
        1 => Ok(managers.remove(0)),
        0 => Err(PortableProbeError::Discovery {
            message: "所选模拟器没有可用安装，请刷新实例目录".to_owned(),
        }),
        _ => Err(PortableProbeError::AmbiguousManager {
            message: "发现多个模拟器安装，请选择带提供方标识的实例或指定管理器路径".to_owned(),
        }),
    }
}
#[cfg(target_os = "windows")]
pub(super) fn discover_managers(
    options: &PortableProbeOptions,
) -> Result<Vec<ResolvedManager>, PortableProbeError> {
    Ok(discover(&DiscoveryRequest {
        manager_hint: options.manager_hint.as_deref(),
        include_running: options.mode == PortableMode::Auto,
        strict_hint: options.manager_hint.is_some() || options.mode == PortableMode::Manual,
    })?)
}

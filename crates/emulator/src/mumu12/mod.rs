//! MuMu12 管理器协议、安装布局和 root shell 适配。

mod instances;
mod nemu;
use super::{EmulatorAdapter, EmulatorError, EmulatorInstance, RootTransport};
use super::{INSTANCE_QUERY_TIMEOUT, InstanceQueryReport};
use crate::command::wrap_remote_command;
use instances::read_instances_with_nemu_fallback;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) struct Mumu12Adapter;

impl EmulatorAdapter for Mumu12Adapter {
    fn id(&self) -> &'static str {
        "mumu12"
    }
    fn display_name(&self) -> &'static str {
        "MuMu12"
    }
    fn manager_names(&self) -> &'static [&'static str] {
        &["MuMuManager.exe"]
    }
    fn process_names(&self) -> &'static [&'static str] {
        &[
            "MuMuManager.exe",
            "MuMuNxMain.exe",
            "MuMuNxDevice.exe",
            "MuMuPlayer.exe",
        ]
    }
    fn matches_uninstall_key(&self, name: &str) -> bool {
        is_mumu_uninstall_name(name)
    }
    fn install_candidates(&self, root: &Path) -> Vec<PathBuf> {
        manager_candidates_from_install_root(root).to_vec()
    }
    fn uninstall_root(&self, executable: &Path) -> Option<PathBuf> {
        if !executable
            .file_name()?
            .eq_ignore_ascii_case("uninstall.exe")
        {
            return None;
        }
        let root = executable.parent()?;
        if let Some(device_root) = root.parent()
            && device_root
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("nx_device"))
        {
            device_root.parent().map(Path::to_path_buf)
        } else {
            Some(root.to_path_buf())
        }
    }
    fn process_candidate(&self, image: &Path) -> Option<PathBuf> {
        manager_candidate_from_process_image(image)
    }
    fn instances(
        &self,
        manager: &Path,
    ) -> Result<BTreeMap<String, EmulatorInstance>, EmulatorError> {
        Ok(self
            .query_instances(manager, INSTANCE_QUERY_TIMEOUT)?
            .instances)
    }
    fn query_instances(
        &self,
        manager: &Path,
        timeout: std::time::Duration,
    ) -> Result<InstanceQueryReport, EmulatorError> {
        read_instances_with_nemu_fallback(manager, timeout)
    }
    fn launch_arguments(&self, index: &str) -> Vec<Vec<String>> {
        instance_launch_argument_sets(index).to_vec()
    }
    fn root_transport(&self) -> RootTransport {
        RootTransport::Manager
    }
    fn root_arguments(&self, index: &str, command: &str) -> Vec<String> {
        root_arguments(index, command)
    }
}

pub(super) fn manager_candidates_from_install_root(install_root: &Path) -> [PathBuf; 2] {
    [
        install_root.join("nx_main").join("MuMuManager.exe"),
        install_root.join("shell").join("MuMuManager.exe"),
    ]
}

pub(super) fn manager_candidate_from_process_image(image: &Path) -> Option<PathBuf> {
    let file_name = image.file_name()?.to_str()?;
    let parent = image.parent()?;
    let parent_name = parent.file_name()?.to_str()?;
    if file_name.eq_ignore_ascii_case("MuMuManager.exe")
        && (parent_name.eq_ignore_ascii_case("nx_main")
            || parent_name.eq_ignore_ascii_case("shell"))
    {
        return Some(image.with_file_name("MuMuManager.exe"));
    }
    if file_name.eq_ignore_ascii_case("MuMuNxMain.exe")
        && parent_name.eq_ignore_ascii_case("nx_main")
    {
        return Some(image.with_file_name("MuMuManager.exe"));
    }
    if file_name.eq_ignore_ascii_case("MuMuPlayer.exe") && parent_name.eq_ignore_ascii_case("shell")
    {
        return Some(image.with_file_name("MuMuManager.exe"));
    }
    if file_name.eq_ignore_ascii_case("MuMuNxDevice.exe") {
        if !parent_name.eq_ignore_ascii_case("shell") {
            return None;
        }
        let engine_root = parent.parent()?;
        let device_root = engine_root.parent()?;
        if !device_root
            .file_name()?
            .to_str()?
            .eq_ignore_ascii_case("nx_device")
        {
            return None;
        }
        let install_root = device_root.parent()?;
        return Some(install_root.join("nx_main").join("MuMuManager.exe"));
    }
    None
}

pub(super) fn is_mumu_uninstall_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "mumuplayerglobal",
        "mumuplayer",
        "mumu player",
        "yxarknights",
    ]
    .iter()
    .any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(['-', ' ']))
    })
}

pub(super) fn instance_launch_argument_sets(instance_index: &str) -> [Vec<String>; 2] {
    [
        vec![
            "control".to_owned(),
            "-v".to_owned(),
            instance_index.to_owned(),
            "launch".to_owned(),
        ],
        vec![
            "api".to_owned(),
            "-v".to_owned(),
            instance_index.to_owned(),
            "launch_player".to_owned(),
        ],
    ]
}

fn root_arguments(instance: &str, command: &str) -> Vec<String> {
    let wrapped = wrap_remote_command(command);
    vec![
        "sh".to_owned(),
        "-v".to_owned(),
        instance.to_owned(),
        "-c".to_owned(),
        wrapped,
    ]
}

#[cfg(test)]
mod command_tests {
    use super::root_arguments;
    #[test]
    fn root_arguments_bind_instance_and_wrap_status() {
        let arguments = root_arguments("1", "id");
        assert_eq!(&arguments[..4], &["sh", "-v", "1", "-c"]);
        assert!(arguments[4].starts_with("( id ) 2>&1;"));
        assert!(arguments[4].ends_with("__SUZUSHIRO_REMOTE_EXIT__=$remote_status"));
    }
}

#[cfg(test)]
mod tests;

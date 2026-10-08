//! 雷电 9 的 list2 协议、代码页与 NAT 监听归属适配。

use super::transport::adb_root_arguments;
use super::{
    EmulatorAdapter, EmulatorError, EmulatorInstance, RootTransport, validate_instance_index,
};
use super::{INSTANCE_QUERY_TIMEOUT, InstanceQueryReport};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use suzushiro_host_command::run_native_with_code_page_timeout;

pub(super) struct LdPlayerAdapter;

impl EmulatorAdapter for LdPlayerAdapter {
    fn id(&self) -> &'static str {
        "ldplayer"
    }
    fn display_name(&self) -> &'static str {
        "雷电"
    }
    fn manager_names(&self) -> &'static [&'static str] {
        &["ldconsole.exe", "dnconsole.exe"]
    }
    fn process_names(&self) -> &'static [&'static str] {
        &["dnplayer.exe", "ldconsole.exe", "dnconsole.exe"]
    }
    fn matches_uninstall_key(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name == "ldplayer"
            || name == "ldplayer9"
            || name.starts_with("ldplayer_")
            || name == "leidian"
            || name == "leidian9"
            || name == "雷电模拟器9"
    }
    fn install_candidates(&self, root: &Path) -> Vec<PathBuf> {
        // 同一安装优先选新版命令入口，避免把兼容别名当成第二套安装。
        let modern = root.join("ldconsole.exe");
        if modern.is_file() {
            vec![modern]
        } else {
            vec![root.join("dnconsole.exe")]
        }
    }
    fn uninstall_root(&self, executable: &Path) -> Option<PathBuf> {
        if executable.file_name()?.eq_ignore_ascii_case("dnuninst.exe") {
            executable.parent().map(Path::to_path_buf)
        } else {
            None
        }
    }
    fn process_candidate(&self, image: &Path) -> Option<PathBuf> {
        let name = image.file_name()?.to_str()?;
        if ["dnplayer.exe", "ldconsole.exe", "dnconsole.exe"]
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n))
        {
            self.install_candidates(image.parent()?).into_iter().next()
        } else {
            None
        }
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
        let executable = super::manager_command::windows_path(manager, "manager")?;
        let output = run_native_with_code_page_timeout(
            &executable,
            &["list2".to_owned()],
            "manager.list2",
            936,
            timeout,
        )
        .map_err(|error| EmulatorError::ManagerCommand {
            stage: "manager.list2",
            message: error.to_string(),
        })?;
        if output.exit_code != 0 || !output.stderr.trim().is_empty() {
            return Err(EmulatorError::ManagerCommand {
                stage: "manager.list2",
                message: format!(
                    "返回 {}，stdout={:?}，stderr={:?}",
                    output.exit_code, output.stdout, output.stderr
                ),
            });
        }
        let mut instances = parse_ldplayer_instances(&output.stdout)?;
        let listeners = super::endpoint::list_tcp_listeners()?;
        for instance in instances
            .values_mut()
            .filter(|instance| instance.is_android_started)
        {
            let port = instance
                .adb_port
                .expect("list2 parser supplies a bounded port");
            let endpoint = super::endpoint::select_owned_loopback(&listeners, port, |pid| {
                super::process_image::query_process_image_path(pid).is_ok_and(|image| {
                    image
                        .file_name()
                        .is_some_and(|name| name.eq_ignore_ascii_case("VBoxNetNAT.exe"))
                        && image
                            .parent()
                            .and_then(|path| path.file_name())
                            .is_some_and(|name| name.eq_ignore_ascii_case("ldplayer9box"))
                })
            });
            instance.adb_host_ip = endpoint.map(|ip| ip.to_string());
        }
        Ok(InstanceQueryReport {
            instances,
            warnings: Vec::new(),
        })
    }
    fn launch_arguments(&self, index: &str) -> Vec<Vec<String>> {
        vec![vec![
            "launch".to_owned(),
            "--index".to_owned(),
            index.to_owned(),
        ]]
    }
    fn root_transport(&self) -> RootTransport {
        RootTransport::AdbSu
    }
    fn root_arguments(&self, _index: &str, command: &str) -> Vec<String> {
        adb_root_arguments(command)
    }
}

/// list2 的前七个字段为稳定实例信息；较新版本在尾部附加宽、高和 DPI。
pub(super) fn parse_ldplayer_instances(
    output: &str,
) -> Result<BTreeMap<String, EmulatorInstance>, EmulatorError> {
    let invalid = |message: String| EmulatorError::ManagerCommand {
        stage: "manager.list2",
        message,
    };
    let mut instances = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<&str> = line.trim_end_matches('\r').split(',').collect();
        if fields.len() != 7 && fields.len() != 10 {
            return Err(invalid(format!("list2 字段数不受支持: {}", fields.len())));
        }
        validate_instance_index(fields[0])?;
        let index: u16 = fields[0]
            .parse()
            .map_err(|_| invalid("实例索引超出端口范围".to_owned()))?;
        let port = index
            .checked_mul(2)
            .and_then(|n| n.checked_add(5555))
            .ok_or_else(|| invalid("实例端口超出范围".to_owned()))?;
        if fields[1].is_empty() {
            return Err(invalid("实例名称为空".to_owned()));
        }
        for field in &fields[2..] {
            field
                .parse::<i64>()
                .map_err(|_| invalid(format!("list2 数值字段无效: {field:?}")))?;
        }
        let ready = match fields[4] {
            "1" => true,
            "0" => false,
            _ => return Err(invalid("Android 状态只允许 0 或 1".to_owned())),
        };
        let process_started = fields[5].parse::<i64>().unwrap() > 0;
        if ready && !process_started {
            return Err(invalid("实例就绪状态与进程状态不一致".to_owned()));
        }
        let instance = EmulatorInstance {
            index: fields[0].to_owned(),
            name: fields[1].to_owned(),
            available: true,
            adb_host_ip: Some("127.0.0.1".to_owned()),
            adb_port: Some(port),
            android_version: None,
            is_process_started: process_started,
            is_android_started: ready,
        };
        if instances.insert(instance.index.clone(), instance).is_some() {
            return Err(invalid("list2 出现重复实例索引".to_owned()));
        }
        if instances.len() > 128 {
            return Err(invalid("实例数量超过 128".to_owned()));
        }
    }
    Ok(instances)
}

#[cfg(test)]
mod tests {
    use super::parse_ldplayer_instances;
    #[test]
    fn ldplayer_catalog_preserves_unicode_and_normalizes_lifecycle() {
        let rows = "0,雷电模拟器,1,2,1,33720,23436,1600,900,240\r\n1,未启动,0,0,0,-1,-1\r\n";
        let parsed = parse_ldplayer_instances(rows).unwrap();
        assert!(parsed["0"].is_ready());
        assert_eq!(parsed["0"].name, "雷电模拟器");
        assert_eq!(parsed["1"].serial().unwrap(), "127.0.0.1:5557");
        assert!(!parsed["1"].is_process_started);
    }
    #[test]
    fn rejects_malformed_or_ambiguous_catalog() {
        for row in [
            "0,x,1,2,2,3,4",
            "0,x,1,2,1,-1,4",
            "0,x,1",
            "32767,x,1,2,1,3,4",
            "0,x,1,2,1,3,4\n0,y,1,2,1,3,4",
        ] {
            assert!(parse_ldplayer_instances(row).is_err(), "{row}");
        }
    }
}

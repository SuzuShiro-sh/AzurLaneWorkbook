use super::{EmulatorError, EmulatorInstance};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// root 命令通过厂商管理器或已绑定实例的隔离 ADB 执行。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootTransport {
    Manager,
    AdbSu,
}

/// 每个提供方只负责厂商差异，不拥有 ADB 服务、游戏进程或运行态资源。
pub trait EmulatorAdapter: Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn manager_names(&self) -> &'static [&'static str];
    fn process_names(&self) -> &'static [&'static str];
    fn matches_uninstall_key(&self, name: &str) -> bool;
    fn install_candidates(&self, root: &Path) -> Vec<PathBuf>;
    fn uninstall_root(&self, executable: &Path) -> Option<PathBuf>;
    fn process_candidate(&self, image: &Path) -> Option<PathBuf>;
    fn instances(
        &self,
        manager: &Path,
    ) -> Result<BTreeMap<String, EmulatorInstance>, EmulatorError>;
    /// 查询实例并保留诊断。内置提供方将预算传至宿主命令及端点探测。
    /// 自定义提供方可覆写此方法；默认实现沿用其已有 instances 行为。
    fn query_instances(
        &self,
        manager: &Path,
        _timeout: std::time::Duration,
    ) -> Result<InstanceQueryReport, EmulatorError> {
        Ok(InstanceQueryReport {
            instances: self.instances(manager)?,
            warnings: Vec::new(),
        })
    }
    fn launch_arguments(&self, index: &str) -> Vec<Vec<String>>;
    fn root_transport(&self) -> RootTransport;
    fn root_arguments(&self, index: &str, command: &str) -> Vec<String>;
}

/// 实例信息及查询成功时仍需报告的降级诊断。
pub struct InstanceQueryReport {
    pub instances: BTreeMap<String, EmulatorInstance>,
    pub warnings: Vec<String>,
}

/// 内置提供方单次实例查询的默认时间预算。
pub const INSTANCE_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

//! MuMu12 实例协议与同安装目录的 .nemu 元数据读取。
use super::super::InstanceQueryReport;
use super::super::manager_command::manager_checked_with_timeout;
use super::super::{EmulatorError, EmulatorInstance, validate_instance_index};
use super::nemu::read_nemu_instances;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// MuMuManager 实例查询响应中用于设备发现和就绪判定的字段。
#[derive(Clone, Debug, Deserialize)]
struct RawManagerInstance {
    #[serde(default)]
    adb_host_ip: Option<String>,
    #[serde(default)]
    adb_port: Option<u16>,
    #[serde(default)]
    android_version: Option<String>,
    error_code: i32,
    index: String,
    #[serde(default)]
    is_android_started: bool,
    #[serde(default)]
    is_process_started: bool,
    name: String,
}

/// Manager JSON 保持首选证据；命令或结构不兼容时才读取同一安装根的 `.nemu`。
pub(super) fn read_instances_with_nemu_fallback(
    manager: &Path,
    timeout: Duration,
) -> Result<InstanceQueryReport, EmulatorError> {
    let deadline = Instant::now() + timeout;
    match read_instances(manager, timeout) {
        Ok(instances) => Ok(InstanceQueryReport {
            instances,
            warnings: Vec::new(),
        }),
        Err(manager_error) => {
            if Instant::now() >= deadline {
                return Err(manager_error);
            }
            let install_root = install_root_from_manager_path(manager)?;
            let nemu_instances = read_nemu_instances(&install_root).map_err(|nemu_error| {
                EmulatorError::Discovery {
                    message: format!(
                        "Manager 实例目录不可用: {manager_error}; .nemu 降级失败: {nemu_error}"
                    ),
                }
            })?;
            nemu_instance_report(nemu_instances, manager_error, deadline)
        }
    }
}

fn nemu_instance_report(
    nemu_instances: BTreeMap<String, super::nemu::NemuInstance>,
    manager_error: EmulatorError,
    deadline: Instant,
) -> Result<InstanceQueryReport, EmulatorError> {
    if Instant::now() >= deadline {
        return Err(EmulatorError::Discovery {
            message: format!("{manager_error}; .nemu 实例查询时间预算已耗尽"),
        });
    }
    let mut instances = BTreeMap::new();
    for (index, instance) in nemu_instances {
        instances.insert(
            index,
            EmulatorInstance {
                adb_host_ip: Some("127.0.0.1".to_owned()),
                adb_port: Some(instance.adb_port),
                android_version: None,
                available: true,
                index: instance.index,
                is_android_started: false,
                is_process_started: false,
                name: instance.name,
            },
        );
    }
    Ok(InstanceQueryReport {
        instances,
        warnings: vec![format!(
            "Manager 查询失败，.nemu 仅提供静态实例配置，运行状态未确认: {manager_error}"
        )],
    })
}

fn install_root_from_manager_path(manager: &Path) -> Result<PathBuf, EmulatorError> {
    let manager_root = manager.parent().ok_or_else(|| EmulatorError::Discovery {
        message: format!("MuMuManager 缺少父目录: {}", manager.display()),
    })?;
    let known_directory = manager_root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("nx_main") || name.eq_ignore_ascii_case("shell")
        });
    if !known_directory {
        return Err(EmulatorError::Discovery {
            message: format!(
                "MuMuManager 不在已知 nx_main/shell 目录: {}",
                manager.display()
            ),
        });
    }
    manager_root
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| EmulatorError::Discovery {
            message: format!("MuMuManager 无法恢复安装根: {}", manager.display()),
        })
}

/// 从所有登记实例 JSON 构造按索引排序的严格候选集合。
fn read_instances(
    manager: &Path,
    timeout: Duration,
) -> Result<BTreeMap<String, EmulatorInstance>, EmulatorError> {
    let output: String = manager_checked_with_timeout(
        manager,
        &["info".to_owned(), "-v".to_owned(), "all".to_owned()],
        "manager.info_all",
        timeout,
    )?;
    parse_instances(&output)
}

/// 解析厂商 info -v all 输出并核对映射键与实例字段一致。
pub(super) fn parse_instances(
    output: &str,
) -> Result<BTreeMap<String, EmulatorInstance>, EmulatorError> {
    let instances: BTreeMap<String, RawManagerInstance> =
        serde_json::from_str(output).map_err(|source| EmulatorError::ManagerCommand {
            stage: "manager.info_all",
            message: format!("实例 JSON 无效: {source}; output={output:?}"),
        })?;
    if instances.is_empty() || instances.len() > 128 {
        return Err(EmulatorError::Discovery {
            message: format!("实例数量必须为 1 至 128，实际为 {}", instances.len()),
        });
    }
    for (key, instance) in &instances {
        validate_instance_index(key)?;
        let valid_name = !instance.name.chars().any(char::is_control);
        let valid_android_version = instance
            .android_version
            .as_deref()
            .is_none_or(|version| !version.chars().any(char::is_control));
        if instance.index != *key || !valid_name || !valid_android_version {
            return Err(EmulatorError::Discovery {
                message: format!(
                    "实例记录无效: key={key}, index={}, error_code={}, name={:?}, android_version={:?}",
                    instance.index, instance.error_code, instance.name, instance.android_version
                ),
            });
        }
    }
    Ok(instances
        .into_iter()
        .map(|(key, raw)| {
            (
                key,
                EmulatorInstance {
                    adb_host_ip: raw.adb_host_ip,
                    adb_port: raw.adb_port,
                    android_version: raw.android_version,
                    available: raw.error_code == 0,
                    index: raw.index,
                    is_android_started: raw.is_android_started,
                    is_process_started: raw.is_process_started,
                    name: raw.name,
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod query_tests {
    use super::*;
    fn metadata() -> BTreeMap<String, super::super::nemu::NemuInstance> {
        BTreeMap::from([(
            "0".into(),
            super::super::nemu::NemuInstance {
                index: "0".into(),
                name: "fixture".into(),
                adb_port: 16384,
            },
        )])
    }
    fn failure() -> EmulatorError {
        EmulatorError::ManagerCommand {
            stage: "manager.info_all",
            message: "fixture failure".into(),
        }
    }
    #[test]
    fn fallback_preserves_static_instance_and_manager_failure() {
        let report = nemu_instance_report(
            metadata(),
            failure(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(report.instances.len(), 1);
        assert!(!report.instances["0"].is_ready());
        assert!(report.warnings[0].contains("fixture failure"));
        assert!(report.warnings[0].contains(".nemu"));
        assert!(report.warnings[0].contains("运行状态未确认"));
    }
    #[test]
    fn exhausted_budget_prevents_static_instance_report() {
        let result = nemu_instance_report(metadata(), failure(), Instant::now());
        assert!(result.err().unwrap().to_string().contains("预算已耗尽"));
    }

    #[test]
    fn unrelated_tcp_listener_does_not_prove_nemu_instance_ready() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut instances = metadata();
        let port = listener.local_addr().unwrap().port();
        instances.get_mut("0").unwrap().adb_port = port;
        let report = nemu_instance_report(
            instances,
            failure(),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();

        assert!(!report.instances["0"].is_ready());
        assert!(!report.instances["0"].is_process_started);
        assert!(!report.instances["0"].is_android_started);
        assert_eq!(report.instances["0"].adb_port, Some(port));
    }
}

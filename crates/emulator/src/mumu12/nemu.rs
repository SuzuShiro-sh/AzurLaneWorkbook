//! 从 MuMu 的只读 `.nemu` 元数据恢复实例索引和主 ADB 端口。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use thiserror::Error;

const MAX_NEMU_BYTES: u64 = 1024 * 1024;
const MAX_NEMU_INSTANCES: usize = 128;
const ADB_GUEST_PORT: u16 = 5555;
const INSTANCE_PREFIXES: [&str; 3] = ["MuMuPlayer-", "MuMuPlayerGlobal-", "YXArkNights-"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NemuInstance {
    pub(super) index: String,
    pub(super) name: String,
    pub(super) adb_port: u16,
}

#[derive(Debug, Error)]
pub(super) enum NemuDiscoveryError {
    #[error("MuMu 实例目录 {path} 读取失败: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("MuMu 实例元数据无效: {message}")]
    Invalid { message: String },
    #[error("MuMu 实例目录中没有可用的 .nemu 元数据: {path}")]
    Empty { path: PathBuf },
}

/// 只遍历`vms/<产品-版本-索引>/<同名>.nemu` 结构，不跟随目录或文件链接。
pub(super) fn read_nemu_instances(
    install_root: &Path,
) -> Result<BTreeMap<String, NemuInstance>, NemuDiscoveryError> {
    let vms_root = install_root.join("vms");
    let entries = fs::read_dir(&vms_root).map_err(|source| NemuDiscoveryError::Io {
        path: vms_root.clone(),
        source,
    })?;
    let mut instances = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| NemuDiscoveryError::Io {
            path: vms_root.clone(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| NemuDiscoveryError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(index) = instance_index_from_name(&name) else {
            continue;
        };
        if instances.len() >= MAX_NEMU_INSTANCES {
            return Err(NemuDiscoveryError::Invalid {
                message: format!("实例数量超过 {MAX_NEMU_INSTANCES}"),
            });
        }
        let nemu_path = entry.path().join(format!("{name}.nemu"));
        let metadata =
            fs::symlink_metadata(&nemu_path).map_err(|source| NemuDiscoveryError::Io {
                path: nemu_path.clone(),
                source,
            })?;
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || !(1..=MAX_NEMU_BYTES).contains(&metadata.len())
        {
            return Err(NemuDiscoveryError::Invalid {
                message: format!(
                    "{} 必须是 1 至 {MAX_NEMU_BYTES} 字节的普通文件",
                    nemu_path.display()
                ),
            });
        }
        let bytes = fs::read(&nemu_path).map_err(|source| NemuDiscoveryError::Io {
            path: nemu_path.clone(),
            source,
        })?;
        let adb_port = parse_nemu_adb_port(&bytes, index.parse().expect("validated index"))?;
        let instance = NemuInstance {
            index: index.clone(),
            name,
            adb_port,
        };
        if instances.insert(index.clone(), instance).is_some() {
            return Err(NemuDiscoveryError::Invalid {
                message: format!("多个实例目录映射到索引 {index}"),
            });
        }
    }
    if instances.is_empty() {
        return Err(NemuDiscoveryError::Empty { path: vms_root });
    }
    Ok(instances)
}

fn instance_index_from_name(name: &str) -> Option<String> {
    let remainder = INSTANCE_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))?;
    let (version, suffix) = remainder.rsplit_once('-')?;
    if version.is_empty() {
        return None;
    }
    if suffix.is_empty() || suffix.len() > 8 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index: u32 = suffix.parse().ok()?;
    (index.to_string() == suffix).then(|| index.to_string())
}

/// MuMu 32 个实例为一组分配端口；配置内主转发缺失时才使用该公式。
fn default_adb_port(index: u32) -> Option<u16> {
    let port = 16_384u32
        .checked_add(index.checked_div(32)?.checked_mul(4)?)?
        .checked_add(index.checked_rem(32)?.checked_mul(32)?)?;
    u16::try_from(port).ok().filter(|port| *port != 0)
}

fn parse_nemu_adb_port(bytes: &[u8], index: u32) -> Result<u16, NemuDiscoveryError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut primary_port = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element))
                if element.name().as_ref().eq_ignore_ascii_case(b"Forwarding") =>
            {
                if let Some(port) = forwarding_adb_port(&reader, &element)?
                    && primary_port
                        .replace(port)
                        .is_some_and(|current| current != port)
                {
                    return Err(NemuDiscoveryError::Invalid {
                        message: "存在冲突的 ADB_PORT 主转发".to_owned(),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(source) => {
                return Err(NemuDiscoveryError::Invalid {
                    message: format!("XML 解析失败: {source}"),
                });
            }
        }
    }
    primary_port
        .or_else(|| default_adb_port(index))
        .ok_or_else(|| NemuDiscoveryError::Invalid {
            message: format!("实例 {index} 的默认 ADB 端口超出范围"),
        })
}

fn forwarding_adb_port(
    reader: &Reader<&[u8]>,
    element: &BytesStart<'_>,
) -> Result<Option<u16>, NemuDiscoveryError> {
    let mut attributes = BTreeMap::new();
    for attribute in element.attributes().with_checks(true) {
        let attribute = attribute.map_err(|source| NemuDiscoveryError::Invalid {
            message: format!("Forwarding 属性无效: {source}"),
        })?;
        let key = std::str::from_utf8(attribute.key.as_ref()).map_err(|_| {
            NemuDiscoveryError::Invalid {
                message: "Forwarding 属性名不是 UTF-8".to_owned(),
            }
        })?;
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|source| NemuDiscoveryError::Invalid {
                message: format!("Forwarding 属性值无效: {source}"),
            })?;
        attributes.insert(key.to_ascii_lowercase(), value.into_owned());
    }
    if !attributes
        .get("name")
        .is_some_and(|name| name.eq_ignore_ascii_case("ADB_PORT"))
    {
        return Ok(None);
    }
    let guest_port = parse_port_attribute(&attributes, "guestport")?;
    if guest_port != ADB_GUEST_PORT {
        return Err(NemuDiscoveryError::Invalid {
            message: format!("ADB_PORT 的 guestport 必须为 {ADB_GUEST_PORT}"),
        });
    }
    let host = attributes
        .get("hostip")
        .map(String::as_str)
        .unwrap_or("0.0.0.0");
    if !matches!(host, "0.0.0.0" | "127.0.0.1") {
        return Err(NemuDiscoveryError::Invalid {
            message: format!("ADB_PORT 的 hostip 不是本机地址: {host:?}"),
        });
    }
    parse_port_attribute(&attributes, "hostport").map(Some)
}

fn parse_port_attribute(
    attributes: &BTreeMap<String, String>,
    name: &'static str,
) -> Result<u16, NemuDiscoveryError> {
    let value = attributes
        .get(name)
        .ok_or_else(|| NemuDiscoveryError::Invalid {
            message: format!("ADB_PORT 缺少 {name}"),
        })?;
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| NemuDiscoveryError::Invalid {
            message: format!("ADB_PORT 的 {name} 不是非零端口: {value:?}"),
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{default_adb_port, parse_nemu_adb_port, read_nemu_instances};

    #[test]
    fn discovers_new_version_instance_metadata() {
        let root = test_root("new-version");
        let directory = root.join("vms/MuMuPlayer-16.2-2");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("MuMuPlayer-16.2-2.nemu"), b"<Nemu/>").unwrap();
        let instances = read_nemu_instances(&root).unwrap();
        assert_eq!(instances["2"].adb_port, 16448);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn primary_adb_record_wins_over_legacy_forwardings() {
        let xml = br#"<Nemu><Forwarding name="ADB_PORT" proto="1" hostip="0.0.0.0" hostport="16384" guestport="5555"/><Forwarding name="ADB_PORT_EX" hostip="127.0.0.1" hostport="5555" guestport="5555"/><Forwarding name="ADB_PORT_OLD" hostip="0.0.0.0" hostport="7555" guestport="5555"/></Nemu>"#;
        assert_eq!(parse_nemu_adb_port(xml, 0).unwrap(), 16_384);
    }

    #[test]
    fn fallback_formula_covers_group_boundary() {
        assert_eq!(default_adb_port(0), Some(16_384));
        assert_eq!(default_adb_port(1), Some(16_416));
        assert_eq!(default_adb_port(31), Some(17_376));
        assert_eq!(default_adb_port(32), Some(16_388));
        assert_eq!(default_adb_port(33), Some(16_420));
        assert_eq!(parse_nemu_adb_port(b"<Nemu/>", 32).unwrap(), 16_388);
    }

    #[test]
    fn rejects_conflicting_or_remote_primary_forwardings() {
        let conflict = br#"<Nemu><Forwarding name="ADB_PORT" hostport="16384" guestport="5555"/><Forwarding name="ADB_PORT" hostport="16416" guestport="5555"/></Nemu>"#;
        assert!(parse_nemu_adb_port(conflict, 0).is_err());
        let remote = br#"<Nemu><Forwarding name="ADB_PORT" hostip="192.0.2.1" hostport="16384" guestport="5555"/></Nemu>"#;
        assert!(parse_nemu_adb_port(remote, 0).is_err());
    }

    #[test]
    fn reads_only_known_instance_directories_and_exact_metadata_name() {
        let root = test_root("catalog");
        let instance = root.join("vms/MuMuPlayerGlobal-15.0-32");
        fs::create_dir_all(&instance).unwrap();
        fs::write(instance.join("MuMuPlayerGlobal-15.0-32.nemu"), b"<Nemu/>").unwrap();
        fs::create_dir_all(root.join("vms/other-0")).unwrap();

        let instances = read_nemu_instances(&root).unwrap();
        assert_eq!(instances.len(), 1);
        let instance = instances.get("32").unwrap();
        assert_eq!(instance.name, "MuMuPlayerGlobal-15.0-32");
        assert_eq!(instance.adb_port, 16_388);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "需要 AZLW_MUMU_INSTALL_ROOT 指向本机 MuMu 安装根"]
    fn installed_mumu_nemu_catalog_matches_real_files() {
        let install_root = std::env::var_os("AZLW_MUMU_INSTALL_ROOT")
            .map(PathBuf::from)
            .expect("需要 AZLW_MUMU_INSTALL_ROOT");
        let instances = read_nemu_instances(&install_root).unwrap();
        assert!(!instances.is_empty());
        assert!(instances.values().all(|instance| instance.adb_port != 0));
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE")
            .join("suzushiro/scratch/emulator-mumu-discovery-tests")
            .join(format!("{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }
}

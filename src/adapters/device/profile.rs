//! 目标包专用运行态 profile 的严格 JSON 解析和契约校验。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use super::bootstrap::{BootstrapEncodingError, RuntimeBootstrapProfile};
use super::runtime::RuntimeAbi;
use suzushiro_text_format::is_lower_hex_with_len;

// 冻结配置档、RPC 版本、文件容量和 Hook 前导长度。
const PROFILE_SCHEMA_VERSION: u32 = 1;
const RUNTIME_PROTOCOL_VERSION: u32 = 1;
/// profile 解析和证据快照共用的最大字节数。
pub(crate) const MAX_PROFILE_BYTES: u64 = 64 * 1024;
const PROLOGUE_SIZE: usize = 14;

/// 已校验的目标模块身份、ABI 和专用 Hook 证据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeProfile {
    profile_id: String,
    abi: RuntimeAbi,
    bootstrap: RuntimeBootstrapProfile,
}

impl RuntimeProfile {
    /// 从固定路径读取 profile，并在解析前限制文件大小。
    pub fn load(path: &Path) -> Result<Self, RuntimeProfileError> {
        let metadata: fs::Metadata =
            fs::metadata(path).map_err(|source| RuntimeProfileError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        if !metadata.is_file() || metadata.len() > MAX_PROFILE_BYTES {
            return Err(RuntimeProfileError::InvalidFile {
                path: path.to_path_buf(),
                maximum: MAX_PROFILE_BYTES,
            });
        }
        let bytes: Vec<u8> = fs::read(path).map_err(|source| RuntimeProfileError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_slice(&bytes)
    }

    /// 从 UTF-8 JSON 解析严格 profile，未知字段不会被忽略。
    pub fn from_slice(bytes: &[u8]) -> Result<Self, RuntimeProfileError> {
        let raw: RawRuntimeProfile = serde_json::from_slice(bytes)?;
        if raw.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(RuntimeProfileError::UnsupportedSchema {
                actual: raw.schema_version,
            });
        }
        if raw.protocol_version != RUNTIME_PROTOCOL_VERSION {
            return Err(RuntimeProfileError::UnsupportedProtocol {
                actual: raw.protocol_version,
            });
        }
        validate_stable_token("profile_id", &raw.profile_id, 64)?;
        validate_shell_token("package_name", &raw.package_name, is_package_character)?;
        validate_shell_token("module_name", &raw.module_name, is_module_character)?;

        let expected_prologue: [u8; PROLOGUE_SIZE] = decode_lower_hex(&raw.expected_prologue_hex)?;
        let bootstrap: RuntimeBootstrapProfile = RuntimeBootstrapProfile::new(
            raw.package_name,
            raw.module_name,
            raw.module_sha256,
            raw.target_symbol_offset,
            expected_prologue,
        )?;

        Ok(Self {
            profile_id: raw.profile_id,
            abi: raw.abi,
            bootstrap,
        })
    }

    /// 将参考配置绑定到本次实际加载的模块，摘要只用于会话内一致性。
    pub(crate) fn bind_module_sha256(&mut self, actual: String) -> Result<(), RuntimeProfileError> {
        self.bootstrap = RuntimeBootstrapProfile::new(
            self.bootstrap.package_name(),
            self.bootstrap.module_name(),
            actual,
            self.bootstrap.target_symbol_offset(),
            *self.bootstrap.expected_prologue(),
        )?;
        Ok(())
    }

    /// 返回用于日志和收据的稳定 profile 标识。
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// 返回 profile 声明的唯一支持 ABI。
    pub fn abi(&self) -> RuntimeAbi {
        self.abi
    }

    /// 返回已经与 JSON schema 一起校验的原生启动字段。
    pub fn bootstrap(&self) -> &RuntimeBootstrapProfile {
        &self.bootstrap
    }
}

/// 尚未完成版本、字符白名单和启动字段校验的 JSON 结构。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimeProfile {
    schema_version: u32,
    profile_id: String,
    protocol_version: u32,
    abi: RuntimeAbi,
    package_name: String,
    module_name: String,
    module_sha256: String,
    target_symbol_offset: u64,
    expected_prologue_hex: String,
}

/// 运行态 profile 无法形成确定的目标身份。
#[derive(Debug, Error)]
pub enum RuntimeProfileError {
    /// profile 文件不存在、不可读或属性读取失败。
    #[error("读取运行态 profile {path} 失败: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// profile 必须是受限大小的普通文件。
    #[error("运行态 profile {path} 必须是至多 {maximum} 字节的普通文件")]
    InvalidFile { path: PathBuf, maximum: u64 },
    /// JSON 结构、字段类型或未知字段不符合 schema。
    #[error("运行态 profile JSON 无效: {0}")]
    Json(#[from] serde_json::Error),
    /// profile schema 与当前宿主不兼容。
    #[error("只支持运行态 profile schema 1，实际为 {actual}")]
    UnsupportedSchema { actual: u32 },
    /// RPC 协议版本与当前宿主不兼容。
    #[error("只支持运行态协议 1，实际为 {actual}")]
    UnsupportedProtocol { actual: u32 },
    /// 稳定标识符为空或包含未登记字符。
    #[error("{field} 必须是 1 至 {maximum} 位的小写稳定标识")]
    InvalidStableToken { field: &'static str, maximum: usize },
    /// 将写入受控 root 命令的身份字段包含歧义字符。
    #[error("{field} 包含设备命令不允许的字符")]
    InvalidShellToken { field: &'static str },
    /// Hook 前导不是 14 字节小写十六进制。
    #[error("expected_prologue_hex 必须是 28 位小写十六进制")]
    InvalidPrologue,
    /// 固定启动字段不满足 loader 契约。
    #[error(transparent)]
    Bootstrap(#[from] BootstrapEncodingError),
}

/// 校验日志和收据可直接使用的小写稳定标识。
fn validate_stable_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), RuntimeProfileError> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte: u8| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(RuntimeProfileError::InvalidStableToken { field, maximum });
    }
    Ok(())
}

/// 校验会进入受控设备命令的字段只含调用方登记字符。
fn validate_shell_token(
    field: &'static str,
    value: &str,
    character_allowed: fn(u8) -> bool,
) -> Result<(), RuntimeProfileError> {
    if value.is_empty() || !value.bytes().all(character_allowed) {
        return Err(RuntimeProfileError::InvalidShellToken { field });
    }
    Ok(())
}

/// 判断字节是否属于当前 Android 包名白名单。
fn is_package_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_')
}

/// 判断字节是否属于共享库文件名白名单。
fn is_module_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+')
}

/// 将固定长度小写十六进制文本解码为数组。
fn decode_lower_hex<const N: usize>(value: &str) -> Result<[u8; N], RuntimeProfileError> {
    if !is_lower_hex_with_len(value, N * 2) {
        return Err(RuntimeProfileError::InvalidPrologue);
    }

    let mut output: [u8; N] = [0; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (decode_nibble(pair[0]) << 4) | decode_nibble(pair[1]);
    }
    Ok(output)
}

/// 解码已通过字符集校验的单个十六进制半字节。
fn decode_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => unreachable!("调用前已校验为小写十六进制"),
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeProfile, RuntimeProfileError};
    use crate::adapters::device::runtime::RuntimeAbi;

    // 所有测试从同一份完整有效配置档定向修改单个边界。
    const VALID_PROFILE: &str = r#"{
        "schema_version": 1,
        "profile_id": "bilibili-cn-x86_64-runtime",
        "protocol_version": 1,
        "abi": "x86_64",
        "package_name": "com.bilibili.azurlane",
        "module_name": "libtolua.so",
        "module_sha256": "fdf3106d1c35ffa6f0c3fb5a4f70e10698f22f8fb94266493eecd2671221126f",
        "target_symbol_offset": 81024,
        "expected_prologue_hex": "555350f30f114c2404f30f110424"
    }"#;

    /// 参考模块变化不阻断实际会话，实际摘要仍须有效。
    #[test]
    fn binds_current_module_without_requiring_reference_digest() {
        let mut profile = RuntimeProfile::from_slice(VALID_PROFILE.as_bytes()).unwrap();
        let offset = profile.bootstrap().target_symbol_offset();
        let prologue = *profile.bootstrap().expected_prologue();
        profile.bind_module_sha256("b".repeat(64)).unwrap();
        assert_eq!(profile.bootstrap().module_sha256(), "b".repeat(64));
        assert_eq!(profile.bootstrap().target_symbol_offset(), offset);
        assert_eq!(*profile.bootstrap().expected_prologue(), prologue);
        assert!(profile.bind_module_sha256("invalid".to_owned()).is_err());
    }

    #[test]
    fn parses_pinned_default_profile() {
        let profile: RuntimeProfile = RuntimeProfile::from_slice(VALID_PROFILE.as_bytes()).unwrap();

        assert_eq!(profile.profile_id(), "bilibili-cn-x86_64-runtime");
        assert_eq!(profile.abi(), RuntimeAbi::X86_64);
        assert_eq!(profile.bootstrap().package_name(), "com.bilibili.azurlane");
        assert_eq!(profile.bootstrap().module_name(), "libtolua.so");
        assert_eq!(profile.bootstrap().target_symbol_offset(), 0x13c80);
    }

    /// 验证严格 schema 和设备命令字符白名单均拒绝歧义输入。
    #[test]
    fn rejects_unknown_fields_and_shell_metacharacters() {
        let unknown: String = VALID_PROFILE.replace(
            "\"schema_version\": 1,",
            "\"schema_version\": 1, \"unexpected\": true,",
        );
        let injected: String =
            VALID_PROFILE.replace("com.bilibili.azurlane", "com.bilibili.azurlane;id");

        assert!(matches!(
            RuntimeProfile::from_slice(unknown.as_bytes()),
            Err(RuntimeProfileError::Json(_))
        ));
        assert!(matches!(
            RuntimeProfile::from_slice(injected.as_bytes()),
            Err(RuntimeProfileError::InvalidShellToken {
                field: "package_name"
            })
        ));
    }

    /// 验证 Hook 前导必须保持固定长度和规范小写编码。
    #[test]
    fn rejects_unpinned_or_uppercase_prologue() {
        let uppercase: String = VALID_PROFILE.replace(
            "555350f30f114c2404f30f110424",
            "555350F30F114C2404F30F110424",
        );

        assert!(matches!(
            RuntimeProfile::from_slice(uppercase.as_bytes()),
            Err(RuntimeProfileError::InvalidPrologue)
        ));
    }
}

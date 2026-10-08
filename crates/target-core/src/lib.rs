//! 定义与目标提供方和业务领域无关的目标标识、生命周期状态与稳定指纹。

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize};
use suzushiro_text_format::is_canonical_sha256;
use thiserror::Error;

/// 通用目标标识允许的最大 UTF-8 字节数。
pub const MAX_TARGET_ID_BYTES: usize = 256;

/// 非空、有界且不包含控制字符的不透明目标标识。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TargetId(String);

impl TargetId {
    /// 校验并建立目标标识；具体 provider 可在此基础上施加更严格格式。
    pub fn new(value: impl Into<String>) -> Result<Self, TargetIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(TargetIdError::Empty);
        }
        if value.len() > MAX_TARGET_ID_BYTES {
            return Err(TargetIdError::TooLong {
                maximum: MAX_TARGET_ID_BYTES,
                actual: value.len(),
            });
        }
        if let Some((index, _)) = value
            .char_indices()
            .find(|(_, character)| character.is_control())
        {
            return Err(TargetIdError::ControlCharacter { index });
        }
        Ok(Self(value))
    }

    /// 返回 provider 赋予目标的原始不透明标识。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费值并返回内部字符串。
    pub fn into_string(self) -> String {
        self.0
    }
}

impl Display for TargetId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for TargetId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for TargetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// 通用目标标识违反基础文本边界。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum TargetIdError {
    /// 标识为空或只有空白字符。
    #[error("目标标识不能为空")]
    Empty,
    /// UTF-8 字节数超过公共上限。
    #[error("目标标识最多允许 {maximum} 字节，实际为 {actual}")]
    TooLong { maximum: usize, actual: usize },
    /// 标识包含不允许进入持久化或日志文本的控制字符。
    #[error("目标标识在字节位置 {index} 包含控制字符")]
    ControlCharacter { index: usize },
}

/// 与具体 provider 无关的目标生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    /// 目标进程尚未启动。
    Stopped,
    /// 目标正在启动，但尚未完全就绪。
    Starting,
    /// 目标已满足 provider 的就绪条件。
    Ready,
    /// 目标存在，但当前不能作为运行目标。
    Unavailable,
}

/// 严格规范化的 SHA-256 目标身份指纹。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TargetFingerprint(String);

impl TargetFingerprint {
    /// 从 64 位小写十六进制 SHA-256 建立目标指纹。
    pub fn new(value: impl Into<String>) -> Result<Self, TargetFingerprintError> {
        let value = value.into();
        if Self::is_canonical(&value) {
            Ok(Self(value))
        } else {
            Err(TargetFingerprintError {
                actual_length: value.len(),
            })
        }
    }

    /// 判断文本是否为唯一规范化的 SHA-256 表示。
    pub fn is_canonical(value: &str) -> bool {
        is_canonical_sha256(value)
    }

    /// 返回规范化的小写十六进制目标指纹。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费值并返回内部字符串。
    pub fn into_string(self) -> String {
        self.0
    }
}

impl Display for TargetFingerprint {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for TargetFingerprint {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for TargetFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// 目标指纹不是规范化的 SHA-256 文本。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("目标指纹必须是 64 位小写十六进制 SHA-256，实际长度为 {actual_length}")]
pub struct TargetFingerprintError {
    actual_length: usize,
}

impl TargetFingerprintError {
    /// 返回输入 UTF-8 字节数，供宿主保留原错误上下文。
    pub const fn actual_length(&self) -> usize {
        self.actual_length
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_TARGET_ID_BYTES, TargetFingerprint, TargetId, TargetIdError, TargetState};

    #[test]
    fn target_id_uses_a_transparent_wire_string() {
        let target = TargetId::new("emulator:0").unwrap();
        let encoded = serde_json::to_string(&target).unwrap();

        assert_eq!(encoded, r#""emulator:0""#);
        assert_eq!(serde_json::from_str::<TargetId>(&encoded).unwrap(), target);
    }

    #[test]
    fn target_id_rejects_empty_long_and_control_text() {
        assert_eq!(TargetId::new("  "), Err(TargetIdError::Empty));
        assert!(matches!(
            TargetId::new("x".repeat(MAX_TARGET_ID_BYTES + 1)),
            Err(TargetIdError::TooLong { .. })
        ));
        assert_eq!(
            TargetId::new("target\n0"),
            Err(TargetIdError::ControlCharacter { index: 6 })
        );
    }

    #[test]
    fn target_states_keep_stable_wire_values() {
        assert_eq!(
            serde_json::to_string(&TargetState::Stopped).unwrap(),
            r#""stopped""#
        );
        assert_eq!(
            serde_json::to_string(&TargetState::Starting).unwrap(),
            r#""starting""#
        );
        assert_eq!(
            serde_json::to_string(&TargetState::Ready).unwrap(),
            r#""ready""#
        );
        assert_eq!(
            serde_json::to_string(&TargetState::Unavailable).unwrap(),
            r#""unavailable""#
        );
    }

    #[test]
    fn target_fingerprint_is_strict_and_transparent() {
        let source = "a".repeat(64);
        let fingerprint = TargetFingerprint::new(source.clone()).unwrap();

        assert_eq!(
            serde_json::to_string(&fingerprint).unwrap(),
            format!(r#""{source}""#)
        );
        assert_eq!(
            serde_json::from_str::<TargetFingerprint>(&format!(r#""{source}""#)).unwrap(),
            fingerprint
        );
        for invalid in [
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            format!("{}\0", "a".repeat(63)),
            format!("{}中", "a".repeat(61)),
        ] {
            assert!(TargetFingerprint::new(invalid).is_err());
        }
        assert!(TargetFingerprint::new(format!("0{}", "a".repeat(63))).is_ok());
    }
}

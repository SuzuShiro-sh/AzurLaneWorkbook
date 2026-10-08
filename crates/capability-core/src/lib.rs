//! 定义命名空间能力状态、证据与业务无关的基础格式校验。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_STABLE_TOKEN_BYTES: usize = 128;
const MAX_EVIDENCE_BYTES: usize = 1024;

/// 稳定能力键到当前状态的确定顺序映射。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesResult {
    /// 带命名空间的稳定能力键到能力状态的映射。
    pub capabilities: BTreeMap<String, CapabilityStatus>,
}

impl CapabilitiesResult {
    /// 校验能力键、原因码和证据的通用格式，不施加业务能力集合或状态策略。
    pub fn validate_basic(&self) -> Result<(), CapabilityValidationError> {
        for (key, status) in &self.capabilities {
            validate_capability_entry(key, status)?;
        }
        Ok(())
    }
}

/// 校验单条能力键、原因码和证据的通用格式。
pub fn validate_capability_entry(
    key: &str,
    status: &CapabilityStatus,
) -> Result<(), CapabilityValidationError> {
    validate_capability_key(key)?;
    status.validate_basic(key)
}

/// 单个稳定能力键在当前会话中的可用性与证据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityStatus {
    /// 当前会话中是否可用。
    pub available: bool,
    /// 可供程序分支判断的稳定原因码。
    pub reason_code: String,
    /// 支持该结论的简短证据。
    pub evidence: Vec<String>,
}

impl CapabilityStatus {
    fn validate_basic(&self, key: &str) -> Result<(), CapabilityValidationError> {
        validate_stable_token("capability.reason_code", &self.reason_code)?;
        if self.evidence.is_empty() {
            return Err(CapabilityValidationError::new(
                "capability_evidence_missing",
                format!("能力 {key} 没有提供证据"),
            ));
        }
        for evidence in &self.evidence {
            validate_non_empty("capability.evidence", evidence, MAX_EVIDENCE_BYTES)?;
        }
        Ok(())
    }
}

/// 能力基础格式无效；宿主可按稳定错误码映射到自己的错误体系。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{code}: {message}")]
pub struct CapabilityValidationError {
    code: &'static str,
    message: String,
}

impl CapabilityValidationError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 返回可供宿主稳定映射的错误码。
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// 返回不包含业务上下文的基础校验说明。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 消费错误并拆出稳定错误码和诊断说明。
    pub fn into_parts(self) -> (&'static str, String) {
        (self.code, self.message)
    }
}

fn validate_capability_key(value: &str) -> Result<(), CapabilityValidationError> {
    validate_stable_token("capability key", value)?;
    if !value.contains('.') {
        return Err(CapabilityValidationError::new(
            "capability_key_invalid",
            format!("能力键 {value} 缺少命名空间"),
        ));
    }
    Ok(())
}

fn validate_stable_token(
    field: &'static str,
    value: &str,
) -> Result<(), CapabilityValidationError> {
    validate_non_empty(field, value, MAX_STABLE_TOKEN_BYTES)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    }) {
        return Err(CapabilityValidationError::new(
            "stable_token_invalid",
            format!("{field}={value} 不是稳定小写标识"),
        ));
    }
    Ok(())
}

fn validate_non_empty(
    field: &'static str,
    value: &str,
    max_length: usize,
) -> Result<(), CapabilityValidationError> {
    if value.trim().is_empty() || value.len() > max_length {
        return Err(CapabilityValidationError::new(
            "text_field_invalid",
            format!("{field} 必须为 1 至 {max_length} 字节的非空文本"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{CapabilitiesResult, CapabilityStatus};

    fn status(available: bool, reason_code: &str) -> CapabilityStatus {
        CapabilityStatus {
            available,
            reason_code: reason_code.to_owned(),
            evidence: vec!["fixture".to_owned()],
        }
    }

    #[test]
    fn preserves_the_wire_model_and_deterministic_key_order() {
        let result = CapabilitiesResult {
            capabilities: BTreeMap::from([
                ("write.apply".to_owned(), status(false, "read_only")),
                ("runtime.health".to_owned(), status(true, "ready")),
            ]),
        };

        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({
                "capabilities": {
                    "runtime.health": {
                        "available": true,
                        "reason_code": "ready",
                        "evidence": ["fixture"],
                    },
                    "write.apply": {
                        "available": false,
                        "reason_code": "read_only",
                        "evidence": ["fixture"],
                    },
                }
            })
        );
        result.validate_basic().unwrap();
    }

    #[test]
    fn rejects_keys_without_a_namespace() {
        let result = CapabilitiesResult {
            capabilities: BTreeMap::from([("health".to_owned(), status(true, "ready"))]),
        };

        assert_eq!(
            result.validate_basic().unwrap_err().code(),
            "capability_key_invalid"
        );
    }

    #[test]
    fn rejects_unstable_reason_codes_and_missing_evidence() {
        let unstable = CapabilitiesResult {
            capabilities: BTreeMap::from([("runtime.health".to_owned(), status(true, "Ready"))]),
        };
        assert_eq!(
            unstable.validate_basic().unwrap_err().code(),
            "stable_token_invalid"
        );

        let mut missing = status(false, "not_ready");
        missing.evidence.clear();
        let missing = CapabilitiesResult {
            capabilities: BTreeMap::from([("runtime.health".to_owned(), missing)]),
        };
        assert_eq!(
            missing.validate_basic().unwrap_err().code(),
            "capability_evidence_missing"
        );
    }
}

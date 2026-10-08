//! 定义与传输、运行平台和业务领域无关的 RPC 信封及会话值对象。

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use suzushiro_text_format::is_lower_hex_with_len;
use thiserror::Error;

/// 会话内使用的 64 位请求标识。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    /// 顺序 RPC 会话通常使用的首个请求标识。
    pub const FIRST: Self = Self(1);

    /// 从原始 64 位值建立请求标识。
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// 返回请求标识的原始 64 位值。
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 返回下一个请求标识；数值耗尽时要求调用方建立新会话。
    pub fn checked_next(self) -> Result<Self, RequestIdError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(RequestIdError::Exhausted)
    }
}

impl Display for RequestId {
    /// 输出固定的十六位小写十六进制表示。
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl Serialize for RequestId {
    /// 按稳定文本格式序列化，避免 JSON 数字精度差异。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RequestId {
    /// 只接受规范化的十六位小写十六进制请求标识。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value: String = String::deserialize(deserializer)?;
        if !is_lower_hex_with_len(&value, 16) {
            return Err(serde::de::Error::custom(
                "request_id 必须是 16 位小写十六进制字符串",
            ));
        }
        u64::from_str_radix(&value, 16)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// 请求号无法继续递增。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RequestIdError {
    /// 当前请求号已经是 64 位无符号整数最大值。
    #[error("请求标识已经耗尽")]
    Exhausted,
}

/// 服务端建议调用方采用的重试范围；传输层不会自动执行重试。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDirective {
    /// 当前逻辑操作不得重试。
    Never,
    /// 可以重试同一逻辑操作，但应分配新的线上请求号。
    SameRequest,
    /// 重新建立连接后才可以重试。
    AfterReconnect,
}

/// 一次服务端错误对当前会话的影响。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEffect {
    /// 当前会话仍可继续使用。
    Unchanged,
    /// 当前连接必须关闭。
    MustClose,
    /// 操作结果未知，不应继续在当前会话执行新操作。
    StateUnknown,
}

/// 携带协议版本、请求号、操作、超时和类型化载荷的 RPC 请求信封。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcRequest<O, P> {
    /// 由具体协议定义并校验的版本号。
    pub protocol_version: u32,
    /// 会话内唯一的请求标识。
    pub request_id: RequestId,
    /// 由具体协议定义的操作。
    pub operation: O,
    /// 由具体协议解释并校验的超时毫秒数。
    pub timeout_ms: u32,
    /// 与操作匹配的请求载荷。
    pub payload: P,
}

impl<O, P> RpcRequest<O, P> {
    /// 构造请求信封；协议版本、超时和业务载荷规则由调用方校验。
    #[must_use]
    pub const fn new(
        protocol_version: u32,
        request_id: RequestId,
        operation: O,
        timeout_ms: u32,
        payload: P,
    ) -> Self {
        Self {
            protocol_version,
            request_id,
            operation,
            timeout_ms,
            payload,
        }
    }
}

/// 使用状态判别字段区分成功结果和结构化错误的 RPC 响应信封。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RpcResponse<R, E> {
    /// 请求成功并返回类型化结果。
    Ok {
        /// 由具体协议定义并校验的版本号。
        protocol_version: u32,
        /// 与请求对应的会话内标识。
        request_id: RequestId,
        /// 与操作匹配的成功结果。
        result: R,
    },
    /// 请求失败并返回结构化错误。
    Error {
        /// 由具体协议定义并校验的版本号。
        protocol_version: u32,
        /// 与请求对应的会话内标识。
        request_id: RequestId,
        /// 由具体协议定义并校验的错误载荷。
        error: E,
    },
}

/// 包含稳定分支字段和泛型诊断详情的 RPC 错误载荷。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError<D> {
    /// 稳定机器错误码。
    pub code: String,
    /// 失败所在阶段。
    pub stage: String,
    /// 面向调用方的错误说明。
    pub message: String,
    /// 调用方可采取的重试动作。
    pub retry: RetryDirective,
    /// 本次失败对当前会话的影响。
    pub session_effect: SessionEffect,
    /// 不参与通用层分支判断的结构化诊断详情。
    pub details: D,
}

impl<D> RpcError<D> {
    /// 构造错误载荷；字段内容及详情结构由具体协议校验。
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        stage: impl Into<String>,
        message: impl Into<String>,
        retry: RetryDirective,
        session_effect: SessionEffect,
        details: D,
    ) -> Self {
        Self {
            code: code.into(),
            stage: stage.into(),
            message: message.into(),
            retry,
            session_effect,
            details,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::{
        RequestId, RequestIdError, RetryDirective, RpcError, RpcRequest, RpcResponse, SessionEffect,
    };

    #[test]
    fn request_id_exposes_raw_value_and_stable_wire_text() {
        let request_id = RequestId::new(0xaf);

        assert_eq!(request_id.get(), 0xaf);
        assert_eq!(request_id.to_string(), "00000000000000af");
        assert_eq!(
            serde_json::to_string(&request_id).unwrap(),
            r#""00000000000000af""#
        );
        assert_eq!(
            serde_json::from_str::<RequestId>(r#""00000000000000af""#).unwrap(),
            request_id
        );
    }

    #[test]
    fn request_id_rejects_noncanonical_wire_text() {
        for invalid in [
            r#""00000000000000AF""#,
            r#""00000000000000ag""#,
            r#""0000000000000af""#,
            r#""000000000000000af""#,
            r#""0000000000000中""#,
        ] {
            let error = serde_json::from_str::<RequestId>(invalid).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("request_id 必须是 16 位小写十六进制字符串")
            );
        }
    }

    #[test]
    fn request_id_progression_reports_exhaustion() {
        assert_eq!(RequestId::FIRST.get(), 1);
        assert_eq!(RequestId::FIRST.checked_next().unwrap().get(), 2);
        assert_eq!(
            RequestId::new(u64::MAX).checked_next(),
            Err(RequestIdError::Exhausted)
        );
    }

    #[test]
    fn retry_directive_has_exact_wire_names() {
        for (value, json) in [
            (RetryDirective::Never, r#""never""#),
            (RetryDirective::SameRequest, r#""same_request""#),
            (RetryDirective::AfterReconnect, r#""after_reconnect""#),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<RetryDirective>(json).unwrap(), value);
        }
        assert!(serde_json::from_str::<RetryDirective>(r#""retry""#).is_err());
    }

    #[test]
    fn session_effect_has_exact_wire_names() {
        for (value, json) in [
            (SessionEffect::Unchanged, r#""unchanged""#),
            (SessionEffect::MustClose, r#""must_close""#),
            (SessionEffect::StateUnknown, r#""state_unknown""#),
        ] {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<SessionEffect>(json).unwrap(), value);
        }
        assert!(serde_json::from_str::<SessionEffect>(r#""closed""#).is_err());
    }

    #[test]
    fn request_envelope_serializes_generic_operation_and_payload() {
        let request = RpcRequest::new(
            7,
            RequestId::FIRST,
            "inspect",
            2_500,
            json!({"resource": "fixture"}),
        );

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "protocol_version": 7,
                "request_id": "0000000000000001",
                "operation": "inspect",
                "timeout_ms": 2_500,
                "payload": {"resource": "fixture"},
            })
        );
    }

    #[test]
    fn response_envelope_preserves_strict_tagged_variants() {
        type FixtureResponse = RpcResponse<Value, RpcError<BTreeMap<String, Value>>>;

        let success: FixtureResponse = serde_json::from_value(json!({
            "status": "ok",
            "protocol_version": 7,
            "request_id": "0000000000000001",
            "result": {"ready": true},
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(success).unwrap(),
            json!({
                "status": "ok",
                "protocol_version": 7,
                "request_id": "0000000000000001",
                "result": {"ready": true},
            })
        );

        let unknown = serde_json::from_value::<FixtureResponse>(json!({
            "status": "ok",
            "protocol_version": 7,
            "request_id": "0000000000000001",
            "result": {},
            "unexpected": true,
        }))
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn error_payload_keeps_required_strict_generic_details() {
        type FixtureError = RpcError<BTreeMap<String, Value>>;

        let error = RpcError::new(
            "fixture_failed",
            "fixture.execute",
            "样本执行失败",
            RetryDirective::SameRequest,
            SessionEffect::Unchanged,
            BTreeMap::from([("reason".to_owned(), json!("fixture"))]),
        );
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "code": "fixture_failed",
                "stage": "fixture.execute",
                "message": "样本执行失败",
                "retry": "same_request",
                "session_effect": "unchanged",
                "details": {"reason": "fixture"},
            })
        );

        let missing_details = serde_json::from_value::<FixtureError>(json!({
            "code": "fixture_failed",
            "stage": "fixture.execute",
            "message": "样本执行失败",
            "retry": "never",
            "session_effect": "must_close",
        }))
        .unwrap_err();
        assert!(
            missing_details
                .to_string()
                .contains("missing field `details`")
        );

        let unknown = serde_json::from_value::<FixtureError>(json!({
            "code": "fixture_failed",
            "stage": "fixture.execute",
            "message": "样本执行失败",
            "retry": "never",
            "session_effect": "must_close",
            "details": {},
            "unexpected": true,
        }))
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown field `unexpected`"));
    }
}

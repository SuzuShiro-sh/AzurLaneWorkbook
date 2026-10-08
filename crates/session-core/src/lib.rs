//! 定义与传输协议、目标平台和业务领域无关的一次性会话凭据。

use std::fmt::{Debug, Display, Formatter, Write};
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::atomic::{Ordering, compiler_fence};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// 128 位会话标识的固定字节数。
pub const SESSION_ID_BYTES: usize = 16;
/// 256 位会话秘密的固定字节数。
pub const SESSION_SECRET_BYTES: usize = 32;

/// 128 位随机会话标识，线上格式固定为 32 位小写十六进制。
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct SessionId([u8; SESSION_ID_BYTES]);

impl SessionId {
    /// 使用操作系统随机源创建新会话标识。
    pub fn generate() -> Result<Self, SessionGenerationError> {
        let mut bytes: [u8; SESSION_ID_BYTES] = [0; SESSION_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(SessionGenerationError::RandomSource)?;
        Ok(Self(bytes))
    }

    /// 将原始字节复制到固定长度缓冲区，供二进制协议编码器使用。
    pub fn copy_bytes_to(&self, destination: &mut [u8; SESSION_ID_BYTES]) {
        destination.copy_from_slice(&self.0);
    }
}

impl Debug for SessionId {
    // 会话标识不是秘密，调试输出复用稳定的线上十六进制格式。
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl Display for SessionId {
    // 会话标识在线上和日志中统一编码为小写十六进制。
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write_lower_hex(formatter, &self.0)
    }
}

impl FromStr for SessionId {
    type Err = SessionParseError;

    // 只接受长度固定的小写十六进制，避免同一标识出现多种表示。
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        decode_lower_hex(value).map(Self)
    }
}

impl Serialize for SessionId {
    // JSON 使用与 Display 相同的稳定文本格式。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    // 反序列化复用严格解析器，不接受宽松 JSON 表示。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(SessionValueVisitor::<Self>(PhantomData))
    }
}

/// 256 位随机会话密钥，调试输出始终脱敏。
#[derive(Clone, Eq, PartialEq)]
pub struct SessionSecret([u8; SESSION_SECRET_BYTES]);

impl SessionSecret {
    /// 使用操作系统随机源创建新会话密钥。
    pub fn generate() -> Result<Self, SessionGenerationError> {
        let mut bytes: [u8; SESSION_SECRET_BYTES] = [0; SESSION_SECRET_BYTES];
        getrandom::fill(&mut bytes).map_err(SessionGenerationError::RandomSource)?;
        Ok(Self(bytes))
    }

    /// 将密钥复制到固定长度缓冲区；调用方使用后应立即清零目标缓冲区。
    pub fn copy_bytes_to(&self, destination: &mut [u8; SESSION_SECRET_BYTES]) {
        destination.copy_from_slice(&self.0);
    }
}

impl Drop for SessionSecret {
    fn drop(&mut self) {
        secure_clear(&mut self.0);
    }
}

impl Debug for SessionSecret {
    // 密钥的调试表示永远不包含原始字节或其编码。
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SessionSecret([REDACTED])")
    }
}

impl FromStr for SessionSecret {
    type Err = SessionParseError;

    // 密钥只接受固定长度的小写十六进制文本。
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        decode_lower_hex(value).map(Self)
    }
}

impl Serialize for SessionSecret {
    // 线上 JSON 需要密钥值，因此显式编码而不借用脱敏的 Debug 实现。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&encode_lower_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for SessionSecret {
    // 反序列化后立即恢复为定长字节，避免保留多余字符串副本。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(SessionValueVisitor::<Self>(PhantomData))
    }
}

struct SessionValueVisitor<T>(PhantomData<T>);

impl<'de, T: FromStr<Err = SessionParseError>> serde::de::Visitor<'de> for SessionValueVisitor<T> {
    type Value = T;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("固定长度的小写十六进制会话凭据")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<T, E> {
        value.parse().map_err(E::custom)
    }

    fn visit_string<E: serde::de::Error>(self, mut value: String) -> Result<T, E> {
        let result = self.visit_str(&value);
        // 接管的字符串可能包含密钥；全部置零后仍是有效 UTF-8。
        secure_clear(unsafe { value.as_bytes_mut() });
        result
    }
}

/// 会话凭据解析失败。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SessionParseError {
    /// 十六进制字符串长度与目标类型不符。
    #[error("会话凭据长度应为 {expected}，实际为 {actual}")]
    InvalidLength { expected: usize, actual: usize },
    /// 字符串包含大写或非十六进制字符。
    #[error("会话凭据第 {index} 位不是小写十六进制字符")]
    InvalidCharacter { index: usize },
}

/// 操作系统随机源不可用。
#[derive(Debug, Error)]
pub enum SessionGenerationError {
    /// 从操作系统读取随机字节失败。
    #[error("操作系统随机源不可用: {0}")]
    RandomSource(getrandom::Error),
}

// 将长度固定的小写十六进制文本解码为定长凭据字节。
fn decode_lower_hex<const N: usize>(value: &str) -> Result<[u8; N], SessionParseError> {
    let expected_length: usize = N * 2;
    if value.len() != expected_length {
        return Err(SessionParseError::InvalidLength {
            expected: expected_length,
            actual: value.len(),
        });
    }

    let mut decoded: [u8; N] = [0; N];
    let bytes: &[u8] = value.as_bytes();
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let high: u8 = decode_nibble(pair[0], index * 2)?;
        let low: u8 = decode_nibble(pair[1], index * 2 + 1)?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

// 逐字节易失写入并设置编译器屏障，保证密钥析构不会被优化移除。
fn secure_clear(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: byte 来自有效的独占可变切片，易失写入不改变别名与生命周期规则。
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

// 解码单个小写十六进制字符，并保留错误字符的位置。
fn decode_nibble(value: u8, index: usize) -> Result<u8, SessionParseError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(SessionParseError::InvalidCharacter { index }),
    }
}

// 直接向格式化器写入小写十六进制，避免为 Display 分配中间字符串。
fn write_lower_hex(formatter: &mut Formatter<'_>, bytes: &[u8]) -> std::fmt::Result {
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";
    for value in bytes {
        formatter.write_char(LOWER_HEX[(value >> 4) as usize] as char)?;
        formatter.write_char(LOWER_HEX[(value & 0x0f) as usize] as char)?;
    }
    Ok(())
}

// 为需要拥有字符串的序列化路径构造小写十六进制文本。
fn encode_lower_hex(bytes: &[u8]) -> String {
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded: String = String::with_capacity(bytes.len() * 2);
    for value in bytes {
        encoded.push(LOWER_HEX[(value >> 4) as usize] as char);
        encoded.push(LOWER_HEX[(value & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{SessionId, SessionParseError, SessionSecret};

    #[test]
    // 验证会话标识在解析、显示和 JSON 序列化之间保持同一格式。
    fn session_id_round_trip_uses_fixed_lower_hex() {
        let source: &str = "00112233445566778899aabbccddeeff";
        let session_id: SessionId = source.parse().expect("固定样本应可解析");

        assert_eq!(session_id.to_string(), source);
        assert_eq!(
            serde_json::to_string(&session_id).unwrap(),
            format!("\"{source}\"")
        );
    }

    #[test]
    // 验证密钥的调试输出不会泄露固定测试样本。
    fn secret_debug_output_never_contains_value() {
        let source: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let secret: SessionSecret = source.parse().expect("固定样本应可解析");

        assert_eq!(format!("{secret:?}"), "SessionSecret([REDACTED])");
        assert!(!format!("{secret:?}").contains(source));
    }

    #[test]
    // 验证解析器拒绝大写字符和非固定长度输入。
    fn parser_rejects_uppercase_and_wrong_length() {
        let uppercase: Result<SessionId, SessionParseError> =
            "00112233445566778899AABBCCDDEEFF".parse();
        let short: Result<SessionId, SessionParseError> = "0011".parse();

        assert_eq!(
            uppercase,
            Err(SessionParseError::InvalidCharacter { index: 20 })
        );
        assert_eq!(
            short,
            Err(SessionParseError::InvalidLength {
                expected: 32,
                actual: 4,
            })
        );
    }

    #[test]
    fn json_deserialization_reuses_the_strict_parser() {
        let uppercase = serde_json::from_str::<SessionId>(r#""00112233445566778899AABBCCDDEEFF""#);
        let short_secret = serde_json::from_str::<SessionSecret>(r#""0011""#);

        assert!(uppercase.unwrap_err().to_string().contains("第 20 位"));
        assert!(
            short_secret
                .unwrap_err()
                .to_string()
                .contains("长度应为 64")
        );
    }

    #[test]
    fn json_reader_and_owned_values_accept_valid_credentials() {
        let id_text = "00112233445566778899aabbccddeeff";
        let secret_text = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let id: SessionId = id_text.parse().unwrap();
        let secret: SessionSecret = secret_text.parse().unwrap();
        let id_json = serde_json::to_vec(&id).unwrap();
        let secret_json = serde_json::to_vec(&secret).unwrap();

        assert_eq!(
            serde_json::from_reader::<_, SessionId>(id_json.as_slice()).unwrap(),
            id
        );
        assert_eq!(
            serde_json::from_value::<SessionId>(serde_json::json!(id_text)).unwrap(),
            id
        );
        assert_eq!(
            serde_json::from_reader::<_, SessionSecret>(secret_json.as_slice()).unwrap(),
            secret
        );
        assert_eq!(
            serde_json::from_value::<SessionSecret>(serde_json::json!(secret_text)).unwrap(),
            secret
        );
    }

    #[test]
    fn json_reader_and_owned_values_reject_invalid_credentials() {
        let uppercase = br#""00112233445566778899AABBCCDDEEFF""#;
        let id_error = serde_json::from_reader::<_, SessionId>(uppercase.as_slice()).unwrap_err();
        let secret_error =
            serde_json::from_value::<SessionSecret>(serde_json::json!("0011")).unwrap_err();

        assert!(id_error.to_string().contains("第 20 位"));
        assert!(secret_error.to_string().contains("长度应为 64"));
    }

    #[test]
    fn copies_exact_protocol_bytes_without_exposing_a_printable_secret() {
        let id: SessionId = "00112233445566778899aabbccddeeff".parse().unwrap();
        let secret: SessionSecret =
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .parse()
                .unwrap();
        let mut id_bytes = [0u8; 16];
        let mut secret_bytes = [0u8; 32];

        id.copy_bytes_to(&mut id_bytes);
        secret.copy_bytes_to(&mut secret_bytes);

        assert_eq!(
            id_bytes,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]
        );
        assert_eq!(secret_bytes, std::array::from_fn(|index| index as u8));
        assert_eq!(format!("{secret:?}"), "SessionSecret([REDACTED])");
    }
}

//! 提供带四字节大端长度前缀和显式载荷上限的同步 JSON 帧编解码。

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// 有界 JSON 帧编码或解码失败。
#[derive(Debug, Error)]
pub enum JsonFrameError {
    /// 读取帧头、读取帧体或写入完整帧时发生 I/O 错误。
    #[error("JSON 帧 I/O 失败: {source}")]
    Io {
        /// 底层同步 I/O 错误。
        #[from]
        source: io::Error,
    },
    /// 待写入的数据无法编码，或已读取的载荷不是目标 JSON 类型。
    #[error("JSON 帧内容无效: {source}")]
    Json {
        /// 底层 JSON 编解码错误。
        #[from]
        source: serde_json::Error,
    },
    /// 本地编码结果或对端声明长度超过调用方给出的载荷上限。
    #[error("JSON 帧为 {actual} 字节，超过上限 {maximum}")]
    FrameTooLarge {
        /// 本地编码后的长度或对端在帧头中声明的长度。
        actual: usize,
        /// 调用方允许的最大 JSON 载荷字节数。
        maximum: usize,
    },
    /// 本地 JSON 载荷长度无法写入四字节帧头。
    #[error("JSON 帧长度 {actual} 无法表示为 32 位无符号整数")]
    LengthOverflow {
        /// 本地编码后的 JSON 载荷字节数。
        actual: usize,
    },
}

/// 四字节大端长度前缀帧的读写失败。JSON 编解码错误不在这里。
#[derive(Debug, Error)]
pub enum LengthPrefixedFrameError {
    /// 读取帧头、读取帧体或写入完整帧时发生 I/O 错误。
    #[error("长度前缀帧 I/O 失败: {source}")]
    Io {
        /// 底层同步 I/O 错误。
        #[source]
        source: io::Error,
    },
    /// 本地载荷或对端声明长度超过调用方给出的上限。
    #[error("长度前缀帧为 {actual} 字节，超过上限 {maximum}")]
    FrameTooLarge {
        /// 本地载荷长度或对端在帧头中声明的长度。
        actual: usize,
        /// 调用方允许的最大载荷字节数。
        maximum: usize,
    },
    /// 本地载荷长度无法写入四字节帧头。
    #[error("长度前缀帧长度 {actual} 无法表示为 32 位无符号整数")]
    LengthOverflow {
        /// 本地载荷字节数。
        actual: usize,
    },
}

impl From<LengthPrefixedFrameError> for JsonFrameError {
    fn from(error: LengthPrefixedFrameError) -> Self {
        match error {
            LengthPrefixedFrameError::Io { source } => Self::Io { source },
            LengthPrefixedFrameError::FrameTooLarge { actual, maximum } => {
                Self::FrameTooLarge { actual, maximum }
            }
            LengthPrefixedFrameError::LengthOverflow { actual } => Self::LengthOverflow { actual },
        }
    }
}

/// 写入四字节大端长度前缀和紧随其后的载荷。超限时不写出任何字节。
pub fn write_length_prefixed_frame<W>(
    writer: &mut W,
    payload: &[u8],
    max_bytes: usize,
) -> Result<(), LengthPrefixedFrameError>
where
    W: Write + ?Sized,
{
    if payload.len() > max_bytes {
        return Err(LengthPrefixedFrameError::FrameTooLarge {
            actual: payload.len(),
            maximum: max_bytes,
        });
    }
    let length: u32 =
        u32::try_from(payload.len()).map_err(|_| LengthPrefixedFrameError::LengthOverflow {
            actual: payload.len(),
        })?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(payload))
        .map_err(|source| LengthPrefixedFrameError::Io { source })
}

/// 读取一个有界长度前缀帧。声明长度超过上限时不分配帧体。
pub fn read_length_prefixed_frame<R>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Vec<u8>, LengthPrefixedFrameError>
where
    R: Read + ?Sized,
{
    let mut header: [u8; 4] = [0; 4];
    reader
        .read_exact(&mut header)
        .map_err(|source| LengthPrefixedFrameError::Io { source })?;
    let length: usize = u32::from_be_bytes(header) as usize;
    if length > max_bytes {
        return Err(LengthPrefixedFrameError::FrameTooLarge {
            actual: length,
            maximum: max_bytes,
        });
    }
    let mut payload: Vec<u8> = vec![0; length];
    reader
        .read_exact(&mut payload)
        .map_err(|source| LengthPrefixedFrameError::Io { source })?;
    Ok(payload)
}

/// 把可序列化值写为四字节大端长度前缀和紧随其后的 JSON 载荷。
pub fn write_json_frame<T, W>(
    writer: &mut W,
    value: &T,
    max_bytes: usize,
) -> Result<(), JsonFrameError>
where
    T: Serialize + ?Sized,
    W: Write + ?Sized,
{
    let payload: Vec<u8> = serde_json::to_vec(value)?;
    write_length_prefixed_frame(writer, &payload, max_bytes)?;
    Ok(())
}

/// 从同步字节流读取一个有界 JSON 帧并解码为目标类型。
pub fn read_json_frame<T, R>(reader: &mut R, max_bytes: usize) -> Result<T, JsonFrameError>
where
    T: DeserializeOwned,
    R: Read + ?Sized,
{
    let payload = read_length_prefixed_frame(reader, max_bytes)?;
    serde_json::from_slice(&payload).map_err(JsonFrameError::from)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, ErrorKind};

    use serde::{Deserialize, Serialize};

    use super::{JsonFrameError, read_json_frame, write_json_frame};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        value: u32,
    }

    #[test]
    fn round_trip_uses_big_endian_length_and_accepts_exact_limit() {
        let mut output: Vec<u8> = Vec::new();
        write_json_frame(&mut output, &Sample { value: 7 }, 11).unwrap();

        assert_eq!(&output[..4], &[0, 0, 0, 11]);
        let decoded: Sample = read_json_frame(&mut Cursor::new(output), 11).unwrap();
        assert_eq!(decoded, Sample { value: 7 });
    }

    #[test]
    fn oversized_encoded_frame_does_not_write_partial_output() {
        let mut output: Vec<u8> = Vec::new();
        let error: JsonFrameError =
            write_json_frame(&mut output, &Sample { value: 7 }, 10).unwrap_err();

        assert!(output.is_empty());
        assert!(matches!(
            error,
            JsonFrameError::FrameTooLarge {
                actual: 11,
                maximum: 10,
            }
        ));
    }

    #[test]
    fn oversized_header_fails_before_payload_read() {
        let input: Vec<u8> = 2048_u32.to_be_bytes().to_vec();
        let error: JsonFrameError =
            read_json_frame::<Sample, _>(&mut Cursor::new(input), 1024).unwrap_err();

        assert!(matches!(
            error,
            JsonFrameError::FrameTooLarge {
                actual: 2048,
                maximum: 1024,
            }
        ));
    }

    #[test]
    fn malformed_payload_is_a_json_error() {
        let mut input: Vec<u8> = 1_u32.to_be_bytes().to_vec();
        input.push(b'{');

        let error: JsonFrameError =
            read_json_frame::<Sample, _>(&mut Cursor::new(input), 1024).unwrap_err();

        assert!(matches!(error, JsonFrameError::Json { .. }));
    }

    #[test]
    fn truncated_header_is_an_io_error() {
        let error: JsonFrameError =
            read_json_frame::<Sample, _>(&mut Cursor::new([0_u8, 0]), 1024).unwrap_err();

        assert!(matches!(
            error,
            JsonFrameError::Io { source } if source.kind() == ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn truncated_payload_is_an_io_error() {
        let mut input: Vec<u8> = 11_u32.to_be_bytes().to_vec();
        input.extend_from_slice(br#"{"value":7}"#);
        input.pop();

        let error: JsonFrameError =
            read_json_frame::<Sample, _>(&mut Cursor::new(input), 1024).unwrap_err();

        assert!(matches!(
            error,
            JsonFrameError::Io { source } if source.kind() == ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn empty_payload_is_invalid_json() {
        let input: Vec<u8> = 0_u32.to_be_bytes().to_vec();
        let error: JsonFrameError =
            read_json_frame::<Sample, _>(&mut Cursor::new(input), 1024).unwrap_err();

        assert!(matches!(error, JsonFrameError::Json { .. }));
    }
}

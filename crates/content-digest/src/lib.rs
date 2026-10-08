//! 提供与路径策略和业务领域无关的 SHA-256 及稳定排序 JSON。

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub use suzushiro_text_format::{is_canonical_sha256, is_lower_hex_with_len};

/// 流式计算普通文件 SHA-256，调用方负责映射阶段和路径上下文。
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file: File = File::open(path)?;
    let mut hasher: Sha256 = Sha256::new();
    let mut buffer: [u8; 64 * 1024] = [0; 64 * 1024];
    loop {
        let count: usize = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(lower_hex(&hasher.finalize()))
}

/// 对递归按对象键排序、保持数组顺序的紧凑 JSON 计算稳定 SHA-256。
pub fn sha256_sorted_json<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let sorted = sort_json_value(serde_json::to_value(value)?);
    let mut writer = DigestWriter::new();
    serde_json::to_writer(&mut writer, &sorted)?;
    Ok(writer.finish())
}

/// 把值递归按对象键排序并编码为紧凑 JSON；数组顺序保持不变。
///
/// 此编码沿用 `serde_json` 的标量格式，不属于 RFC 8785 JCS。
pub fn sorted_json<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(&sort_json_value(serde_json::to_value(value)?))
}

/// 计算内存字节的 SHA-256 并编码为小写十六进制。
pub fn sha256_bytes(bytes: &[u8]) -> String {
    lower_hex(&Sha256::digest(bytes))
}

/// 按 `serde_json` 的紧凑输出顺序编码值并计算 SHA-256。
///
/// 该函数保留结构字段和映射迭代器的输出顺序，不会像 [`sha256_sorted_json`] 一样
/// 排序对象键。调用方必须保证所用类型的序列化顺序本身属于稳定契约。
pub fn sha256_compact_json<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let bytes: Vec<u8> = serde_json::to_vec(value)?;
    Ok(sha256_bytes(&bytes))
}

/// 为任意底层 writer 限制总写入量，并为实际成功写入的字节计算 SHA-256。
///
/// 单次写入若会超过剩余上限，会在调用底层 writer 前完整拒绝，不会留下部分超限内容。
/// 底层 writer 自身允许短写时，只统计和摘要它实际接受的字节。
pub struct BoundedSha256Writer<W> {
    inner: W,
    maximum_bytes: u64,
    written_bytes: u64,
    hasher: Sha256,
    limit_exceeded: bool,
}

impl<W> BoundedSha256Writer<W> {
    /// 包装底层 writer，并设置允许成功写入的总字节数。
    pub fn new(inner: W, maximum_bytes: u64) -> Self {
        Self {
            inner,
            maximum_bytes,
            written_bytes: 0,
            hasher: Sha256::new(),
            limit_exceeded: false,
        }
    }

    /// 返回构造时设置的总字节上限。
    pub const fn maximum_bytes(&self) -> u64 {
        self.maximum_bytes
    }

    /// 返回底层 writer 已成功接受的字节数。
    pub const fn written_bytes(&self) -> u64 {
        self.written_bytes
    }

    /// 返回当前仍可成功写入的字节数。
    pub const fn remaining_bytes(&self) -> u64 {
        self.maximum_bytes.saturating_sub(self.written_bytes)
    }

    /// 报告任意一次写入是否曾因总字节上限被拒绝。
    pub const fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }

    /// 返回当前已成功写入内容的 SHA-256，不结束后续写入。
    pub fn sha256(&self) -> String {
        lower_hex(&self.hasher.clone().finalize())
    }

    /// 借用底层 writer，供调用方执行文件同步等后续操作。
    pub const fn inner(&self) -> &W {
        &self.inner
    }

    /// 释放包装并返回底层 writer。
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for BoundedSha256Writer<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("单次写入长度无法表示为 u64"))?;
        if requested > self.remaining_bytes() {
            self.limit_exceeded = true;
            return Err(io::Error::other(format!(
                "写入内容超过 {} 字节上限",
                self.maximum_bytes
            )));
        }

        let written = self.inner.write(buffer)?;
        if written > buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "底层 writer 返回的写入量超过输入长度",
            ));
        }
        self.hasher.update(&buffer[..written]);
        let written_u64 =
            u64::try_from(written).map_err(|_| io::Error::other("单次写入计数无法表示为 u64"))?;
        self.written_bytes = self
            .written_bytes
            .checked_add(written_u64)
            .ok_or_else(|| io::Error::other("写入计数溢出"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct DigestWriter {
    hasher: Sha256,
}

impl DigestWriter {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> String {
        lower_hex(&self.hasher.finalize())
    }
}

impl Write for DigestWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.hasher.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn sort_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(sort_json_value).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sort_json_value(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}

/// 把摘要字节编码为稳定的小写十六进制。
pub fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output: String = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{self, Write};
    use std::path::PathBuf;

    use serde::Serialize;
    use serde_json::json;

    use super::{
        BoundedSha256Writer, lower_hex, sha256_bytes, sha256_compact_json, sha256_file,
        sha256_sorted_json, sorted_json,
    };

    #[test]
    fn bounded_writer_hashes_only_successfully_written_bytes() {
        let mut writer = BoundedSha256Writer::new(Vec::new(), 5);

        writer.write_all(b"abc").unwrap();
        let error = writer.write_all(b"def").unwrap_err();

        assert_eq!(error.to_string(), "写入内容超过 5 字节上限");
        assert_eq!(writer.maximum_bytes(), 5);
        assert_eq!(writer.written_bytes(), 3);
        assert_eq!(writer.remaining_bytes(), 2);
        assert!(writer.limit_exceeded());
        assert_eq!(writer.sha256(), sha256_bytes(b"abc"));
        assert_eq!(writer.into_inner(), b"abc");
    }

    #[test]
    fn bounded_writer_tracks_short_writes_and_delegates_flush() {
        let mut writer = BoundedSha256Writer::new(ShortWriter::default(), 8);

        assert_eq!(writer.write(b"abcd").unwrap(), 2);
        writer.flush().unwrap();

        assert_eq!(writer.written_bytes(), 2);
        assert_eq!(writer.sha256(), sha256_bytes(b"ab"));
        assert_eq!(writer.inner().bytes, b"ab");
        assert!(writer.inner().flushed);
        let inner = writer.into_inner();
        assert_eq!(inner.bytes, b"ab");
        assert!(inner.flushed);
    }

    #[test]
    fn bounded_writer_rejects_invalid_underlying_write_count() {
        let mut writer = BoundedSha256Writer::new(InvalidCountWriter, 8);

        let error = writer.write(b"abc").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(writer.written_bytes(), 0);
        assert_eq!(writer.sha256(), sha256_bytes(b""));
    }

    #[derive(Default)]
    struct ShortWriter {
        bytes: Vec<u8>,
        flushed: bool,
    }

    impl Write for ShortWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let written = buffer.len().min(2);
            self.bytes.extend_from_slice(&buffer[..written]);
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }

    struct InvalidCountWriter;

    impl Write for InvalidCountWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len().saturating_add(1))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn sorted_json_hash_ignores_object_key_insertion_order() {
        #[derive(Serialize)]
        struct Record {
            second: u32,
            first: u32,
        }

        assert_eq!(
            sha256_sorted_json(&Record {
                second: 2,
                first: 1,
            })
            .unwrap(),
            sha256_sorted_json(&json!({"first": 1, "second": 2})).unwrap()
        );
        assert_eq!(
            sha256_sorted_json(&json!({"outer": {"z": 2, "a": 1}})).unwrap(),
            sha256_sorted_json(&json!({"outer": {"a": 1, "z": 2}})).unwrap()
        );
        let encoded = sorted_json(&json!({"z": 2, "a": {"y": 4, "b": 3}})).unwrap();
        assert_eq!(encoded, r#"{"a":{"b":3,"y":4},"z":2}"#);
        assert_eq!(
            sha256_bytes(encoded.as_bytes()),
            sha256_sorted_json(&json!({
                "a": {"b": 3, "y": 4},
                "z": 2
            }))
            .unwrap()
        );
    }

    #[test]
    fn sorted_json_hash_preserves_array_order() {
        assert_ne!(
            sha256_sorted_json(&json!({"items": [1, 2, 3]})).unwrap(),
            sha256_sorted_json(&json!({"items": [3, 2, 1]})).unwrap()
        );
    }

    #[test]
    fn compact_json_hash_preserves_the_serialized_field_order() {
        #[derive(Serialize)]
        struct Fixture<'a> {
            name: &'a str,
            value: u32,
        }

        assert_eq!(
            sha256_compact_json(&Fixture {
                name: "execution",
                value: 1,
            })
            .unwrap(),
            "b7b1857cde48a163e18a0b0bc0b26ef58e34b126e024cb24a5b7fcabc63b519d"
        );

        #[derive(Serialize)]
        struct ReverseFields {
            second: u32,
            first: u32,
        }
        let value = ReverseFields {
            second: 2,
            first: 1,
        };
        assert_ne!(
            sha256_compact_json(&value).unwrap(),
            sha256_sorted_json(&value).unwrap()
        );
    }

    #[test]
    fn lower_hex_preserves_leading_zeroes_and_letter_case() {
        assert_eq!(lower_hex(&[0x00, 0x0f, 0x10, 0xab, 0xff]), "000f10abff");
    }

    #[test]
    fn preserves_native_max_digits_float_across_json_replay() {
        let encoded = r#"{"value":95.97452000000001}"#;
        let first: serde_json::Value = serde_json::from_str(encoded).unwrap();
        let replayed = serde_json::to_string(&first).unwrap();
        let second: serde_json::Value = serde_json::from_str(&replayed).unwrap();

        assert_eq!(replayed, encoded);
        assert_eq!(serde_json::to_string(&second).unwrap(), encoded);
    }

    /// 共享编码必须与标准空文件摘要完全一致。
    #[test]
    fn hashes_file_as_lower_hex() {
        let home: PathBuf = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let mut random: [u8; 8] = [0; 8];
        getrandom::fill(&mut random).expect("测试需要操作系统随机源");
        let parent: PathBuf = home
            .join("suzushiro/scratch/azlw-digest-tests")
            .join(format!(
                "{}-{:016x}",
                std::process::id(),
                u64::from_le_bytes(random)
            ));
        let path: PathBuf = parent.join("empty");
        fs::create_dir_all(&parent).unwrap();
        fs::write(&path, []).unwrap();

        assert_eq!(
            sha256_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn hashes_files_larger_than_one_read_buffer() {
        let home: PathBuf = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let mut random: [u8; 8] = [0; 8];
        getrandom::fill(&mut random).expect("测试需要操作系统随机源");
        let parent: PathBuf = home
            .join("suzushiro/scratch/azlw-digest-tests")
            .join(format!(
                "{}-{:016x}",
                std::process::id(),
                u64::from_le_bytes(random)
            ));
        let path: PathBuf = parent.join("large");
        let bytes: Vec<u8> = (0..(64 * 1024 + 17))
            .map(|index| (index % 251) as u8)
            .collect();
        fs::create_dir_all(&parent).unwrap();
        fs::write(&path, &bytes).unwrap();

        assert_eq!(sha256_file(&path).unwrap(), sha256_bytes(&bytes));
        fs::remove_dir_all(parent).unwrap();
    }
}

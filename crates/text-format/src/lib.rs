//! 提供与标准库、序列化框架和业务领域无关的严格文本格式谓词。

#![no_std]

/// 判断文本是否恰好由指定字节数的小写十六进制字符组成。
///
/// 该谓词只接受 ASCII `0-9a-f`。`expected_length` 使用 UTF-8 字节数，因此大写、
/// 非 ASCII、控制字符和其他符号都会被拒绝；长度为零时仅空字符串满足契约。
#[must_use]
pub const fn is_lower_hex_with_len(value: &str, expected_length: usize) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != expected_length {
        return false;
    }

    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !((byte >= b'0' && byte <= b'9') || (byte >= b'a' && byte <= b'f')) {
            return false;
        }
        index += 1;
    }
    true
}

/// 判断文本是否为规范的 64 位小写十六进制 SHA-256 表示。
#[must_use]
pub const fn is_canonical_sha256(value: &str) -> bool {
    is_lower_hex_with_len(value, 64)
}

/// 判断包名是否至多 255 字节，由至少两个非空 ASCII 字母、数字或下划线段组成。
///
/// 点号分隔各段；允许大写字母和数字开头，不校验包是否存在。
#[must_use]
pub fn is_ascii_package_name(value: &str) -> bool {
    value.len() <= 255
        && value.contains('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_'))
        && value.split('.').all(|segment| !segment.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{is_ascii_package_name, is_canonical_sha256, is_lower_hex_with_len};

    #[test]
    fn package_name_requires_nonempty_ascii_segments() {
        for value in ["com.example.app", "Com.Example_1", "1._"] {
            assert!(is_ascii_package_name(value), "{value:?}");
        }
        for value in [
            "", "package", ".app", "app.", "a..b", "a-b.c", "a/b.c", "a.b c", "a.b;id", "a.中",
            "a.b\0", "a.b\n",
        ] {
            assert!(!is_ascii_package_name(value), "{value:?}");
        }
    }

    #[test]
    fn package_name_length_is_bounded_in_bytes() {
        let mut bytes = [b'a'; 256];
        bytes[1] = b'.';
        let value = core::str::from_utf8(&bytes).unwrap();
        assert!(is_ascii_package_name(&value[..255]));
        assert!(!is_ascii_package_name(value));
    }

    #[test]
    fn lower_hex_requires_exact_ascii_byte_length_and_lowercase() {
        for (value, length) in [("", 0), ("0", 1), ("001122aabbccddeeff", 18)] {
            assert!(is_lower_hex_with_len(value, length));
        }

        for (value, length) in [
            ("", 1),
            ("0", 0),
            ("A", 1),
            ("g", 1),
            ("\0", 1),
            ("中", 3),
            ("001122aabbccddeef", 18),
            ("001122aabbccddeeff0", 18),
        ] {
            assert!(!is_lower_hex_with_len(value, length));
        }
    }

    #[test]
    fn canonical_sha256_has_one_wire_representation() {
        assert!(is_canonical_sha256(
            "0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));

        for invalid in [
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa中",
        ] {
            assert!(!is_canonical_sha256(invalid));
        }
    }

    #[test]
    fn predicates_are_available_in_const_contexts() {
        const {
            assert!(is_lower_hex_with_len("0000000000000001", 16));
            assert!(is_canonical_sha256(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ));
        }
    }
}

# suzushiro-text-format

`suzushiro-text-format` 提供零依赖的底层文本格式验证原语：

- `is_lower_hex_with_len`：严格验证指定长度的 ASCII 小写十六进制文本（`0-9a-f`）。
- `is_canonical_sha256`：验证 64 位标准 SHA-256 字符串。
- `is_ascii_package_name`：验证至少两段、总长不超过 255 字节的 ASCII 包名格式，允许数字开头，不校验包是否存在。
- `is_lower_hex_with_len` 与 `is_canonical_sha256` 支持 `const fn`，可直接在编译期常数环境中使用。

## 使用示例

```rust
use suzushiro_text_format::{is_canonical_sha256, is_lower_hex_with_len};

const REQUEST_ID_VALID: bool = is_lower_hex_with_len("0000000000000001", 16);
const DIGEST_VALID: bool =
    is_canonical_sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

assert!(REQUEST_ID_VALID);
assert!(DIGEST_VALID);
```

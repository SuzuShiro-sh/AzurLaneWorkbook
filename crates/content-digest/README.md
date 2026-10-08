# suzushiro-content-digest

`suzushiro-content-digest` 提供轻量级、确定性的 SHA-256 内容摘要计算能力：

- **流式有界写入**：`BoundedSha256Writer` 为任意 `Write` 目标提供写入上限保护并同步计算哈希。
- **确定性 JSON 摘要**：
  - `sha256_compact_json`：保留原有字段顺序的紧凑 JSON 摘要。
  - `sha256_sorted_json`：递归对对象键排序后计算摘要，消除字段乱序影响。
- **格式校验**：提供 64 位小写十六进制文本格式检验工具函数。

## 使用示例

```rust
use serde_json::json;
use suzushiro_content_digest::{sha256_bytes, sha256_sorted_json, sorted_json};

let data = json!({"b": 1, "a": 2});
let normalized = sorted_json(&data)?;
assert_eq!(normalized, r#"{"a":2,"b":1}"#);

let digest = sha256_sorted_json(&data)?;
assert_eq!(digest, sha256_bytes(normalized.as_bytes()));
# Ok::<(), Box<dyn std::error::Error>>(())
```

# suzushiro-target-core

`suzushiro-target-core` 提供设备与操作目标的通用标识与状态原语：

- `TargetId`：最多 256 字节的不透明目标标识符（如 `mumu12:0`、`emulator-5554`）。
- `TargetState`：标准生命周期状态枚举（`stopped`、`starting`、`ready`、`unavailable`）。
- `TargetFingerprint`：64 位小写十六进制 SHA-256 目标指纹。

## 使用示例

```rust
use suzushiro_target_core::{TargetFingerprint, TargetId, TargetState};

let target_id = TargetId::new("mumu12:0")?;
let fingerprint = TargetFingerprint::new("a".repeat(64))?;

assert_eq!(target_id.as_str(), "mumu12:0");
assert_eq!(fingerprint.as_str().len(), 64);
assert_eq!(serde_json::to_string(&TargetState::Ready)?, r#""ready""#);
# Ok::<(), Box<dyn std::error::Error>>(())
```

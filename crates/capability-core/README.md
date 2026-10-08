# suzushiro-capability-core

`suzushiro-capability-core` 提供运行时能力目录与健康检查结果数据结构：

- **能力键规范**：1 至 128 字节的点号分割标识符（如 `runtime.health`、`emulator.root`）。
- **可用性与证据链**：每项能力包含布尔值可用状态、原因代码与非空证据描述列表。
- **确定性输出**：基于 `BTreeMap` 保持序列化 JSON 的稳定有序性。

## 使用示例

```rust
use std::collections::BTreeMap;
use suzushiro_capability_core::{CapabilitiesResult, CapabilityStatus};

let capabilities = CapabilitiesResult {
    capabilities: BTreeMap::from([(
        "runtime.health".to_owned(),
        CapabilityStatus {
            available: true,
            reason_code: "ready".to_owned(),
            evidence: vec!["authenticated_session".to_owned()],
        },
    )]),
};

capabilities.validate_basic()?;
let health = &capabilities.capabilities["runtime.health"];
assert!(health.available && health.reason_code == "ready");
# Ok::<(), Box<dyn std::error::Error>>(())
```

# suzushiro-session-core

`suzushiro-session-core` 提供高熵、安全的会话凭据抽象：

- **类型安全**：包含 128 位随机 `SessionId` 与 256 位随机 `SessionSecret`。
- **格式规范**：固定长度小写十六进制表示，支持借用与拥有的 Serde 序列化。
- **内存安全**：`SessionSecret` 实现了脱敏 `Debug` 输出，防止日志意外泄露敏感密钥。

## 使用示例

```rust
use suzushiro_session_core::{
    SESSION_ID_BYTES, SESSION_SECRET_BYTES, SessionId, SessionSecret,
};

let session_id = SessionId::generate()?;
let session_secret = SessionSecret::generate()?;

let mut id_bytes = [0u8; SESSION_ID_BYTES];
let mut secret_bytes = [0u8; SESSION_SECRET_BYTES];

session_id.copy_bytes_to(&mut id_bytes);
session_secret.copy_bytes_to(&mut secret_bytes);

assert_eq!(session_id.to_string().len(), SESSION_ID_BYTES * 2);
assert_eq!(format!("{session_secret:?}"), "SessionSecret([REDACTED])");

// 敏感密钥用后手动清零
secret_bytes.fill(0);
# Ok::<(), Box<dyn std::error::Error>>(())
```

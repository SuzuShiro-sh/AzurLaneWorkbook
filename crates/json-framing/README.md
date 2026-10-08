# suzushiro-json-framing

`suzushiro-json-framing` 为同步 I/O 字节流提供确定性有界 JSON 消息帧编解码：

- **定长帧头**：采用 4 字节大端无符号整数标记载荷长度。
- **边界防溢出**：编码时先计算长度并核验上限，读取时先读帧头再分配缓冲区，避免内存攻击。
- **写入边界**：编码或长度校验失败时不写出任何字节；底层 I/O 写入失败可能留下部分帧。

## 使用示例

```rust
use std::io::Cursor;
use serde::{Deserialize, Serialize};
use suzushiro_json_framing::{read_json_frame, write_json_frame};

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct Message {
    value: u32,
}

let mut buffer = Vec::new();
write_json_frame(&mut buffer, &Message { value: 42 }, 1024)?;

let decoded: Message = read_json_frame(&mut Cursor::new(buffer), 1024)?;
assert_eq!(decoded, Message { value: 42 });
# Ok::<(), Box<dyn std::error::Error>>(())
```

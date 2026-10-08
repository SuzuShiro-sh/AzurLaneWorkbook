# suzushiro-rpc-core

`suzushiro-rpc-core` 提供跨模块与进程通信通用的基础 RPC 信封结构与值对象：

- `RequestId`：64 位请求编号，提供拒绝溢出的 `checked_next` 方法，递增顺序由调用方管理；固定序列化为 16 位小写十六进制字符串（避免前端大整数精度丢失）。
- `RetryDirective`：结构化重试建议枚举（`Never` / `SameRequest` / `AfterReconnect`）。
- `RpcRequest<O, P>`：组合请求操作名、参数负载、超时限制与请求 ID 的通用结构。
- `RpcResponse<R, E>`：统一的标准响应信封与错误结构。

## 使用示例

```rust
use suzushiro_rpc_core::{
    RequestId, RetryDirective, RpcError, RpcRequest, RpcResponse, SessionEffect,
};

let request_id = RequestId::FIRST.checked_next()?;
let request = RpcRequest::new(1, request_id, "ping", 1_000, ());

let response = RpcResponse::<(), ()>::Ok {
    protocol_version: 1,
    request_id,
    result: (),
};

assert_eq!(request_id.to_string(), "0000000000000002");
assert!(matches!(response, RpcResponse::Ok { .. }));
# Ok::<(), Box<dyn std::error::Error>>(())
```

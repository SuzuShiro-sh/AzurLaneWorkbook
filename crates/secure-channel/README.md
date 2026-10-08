# suzushiro-secure-channel

`suzushiro-secure-channel` 提供双向认证加密通道抽象：基于预共享密钥通过挑战-证明握手，随后利用独立的方向密钥与严格递增序号传输 XChaCha20-Poly1305 认证加密帧。

## 握手流程与密钥派生

1. **预共享凭据**：双方持有相同的 16 字节 `session_id` 与 32 字节 `session_secret`。
2. **服务端挑战**：服务端生成 32 字节随机 `server_nonce`，发送 `ServerChallengeFrame`。
3. **客户端验证与回应**：客户端调用 `verify_server_challenge` 核验挑战帧，生成 32 字节 `client_nonce`，发送 `ClientProofFrame` 并派生通道材料 `ChannelKeyMaterial`。
4. **服务端验证并建连**：服务端调用 `verify_client_proof` 校验客户端证明，派生服务端通道材料。
5. **通道建立**：客户端调用 `into_client_channel`，服务端调用 `into_server_channel`，分别获取独立的 `outbound: FrameSealer` 和 `inbound: FrameOpener`。发送与接收序号从 0 开始独立递增。

证明与方向密钥基于 **Keyed BLAKE2b** 派生，各用途先加入 `secure-channel-v1.json` 中定义的独立标签，随后按以下顺序加入输入：

| 派生目标 | 标签后的输入顺序 | 输出长度 |
| --- | --- | --- |
| 服务端证明 | 会话 ID (16B) + 服务端 Nonce (32B) | 32 字节 |
| 客户端证明 | 服务端挑战帧全文 (82B) + 客户端 Nonce (32B) | 32 字节 |
| 双方方向密钥 | 会话 ID + 服务端 Nonce + 客户端 Nonce | 各 32 字节 |
| 双方 Nonce 前缀 | 会话 ID + 服务端 Nonce + 客户端 Nonce | 各 16 字节 |

## V1 二进制帧格式

数据类型及常量规范见 [`secure-channel-v1.json`](secure-channel-v1.json)，序号均采用 8 字节大端无符号整数（Big-Endian `u64`）。

| 帧类型 | 结构组成 | 总长度 |
| --- | --- | --- |
| 服务端挑战帧 | 版本 (1B) + 类型 (1B，值为 1) + 会话 ID (16B) + 服务端 Nonce (32B) + 证明 (32B) | 82 字节 |
| 客户端证明帧 | 版本 (1B) + 类型 (1B，值为 2) + 客户端 Nonce (32B) + 证明 (32B) | 66 字节 |
| 受保护通信帧 | 版本 (1B) + 类型 (1B，值为 3) + 序列号 (8B) + 密文 (NB) + Poly1305 标签 (16B) | 明文长度 + 26 字节 |

受保护帧前 10 字节（版本 + 类型 + 序列号）作为附加认证数据（AAD）。24 字节 AEAD Nonce 由 16 字节方向专属前缀与 8 字节大端序号拼接而成。

## 使用示例

以下固定数组仅用于示例；实际会话凭据须可靠随机生成，每次握手使用新的随机 Nonce。

```rust
use suzushiro_secure_channel::{
    HANDSHAKE_NONCE_BYTES, SECRET_BYTES, SESSION_ID_BYTES, SecureChannelError,
    answer_server_challenge, create_server_challenge, verify_client_proof,
    verify_server_challenge,
};

fn main() -> Result<(), SecureChannelError> {
    let session_id = [1u8; SESSION_ID_BYTES];
    let session_secret = [2u8; SECRET_BYTES];
    let server_nonce = [3u8; HANDSHAKE_NONCE_BYTES];
    let client_nonce = [4u8; HANDSHAKE_NONCE_BYTES];

    // 服务端发送挑战
    let challenge = create_server_challenge(&session_id, &session_secret, &server_nonce);

    // 客户端校验挑战并回应
    let verified = verify_server_challenge(&session_id, &session_secret, challenge.as_bytes())?;
    let (proof, client_keys) = answer_server_challenge(&verified, &session_secret, &client_nonce);

    // 服务端校验证明
    let server_keys = verify_client_proof(&challenge, &session_id, &session_secret, proof.as_bytes())?;

    let mut client = client_keys.into_client_channel();
    let mut server = server_keys.into_server_channel();

    // 双向加密帧收发
    let request_frame = client.outbound.seal(b"ping", 1024)?;
    let request = server.inbound.open(&request_frame, 1024)?;
    assert_eq!(request.as_slice(), b"ping");

    let response_frame = server.outbound.seal(b"pong", 1024)?;
    let response = client.inbound.open(&response_frame, 1024)?;
    assert_eq!(response.as_slice(), b"pong");
    Ok(())
}
```

## 序列号与安全约束

- **单调递增**：序列号仅在成功封装（`seal`）或成功解密认证（`open`）后推进；校验失败时不推进。
- **耗尽保护**：序列号达到 `u64::MAX` 后拒绝再分配，连接不可回绕，必须重新建连握手。
- **内存安全**：派生的方向密钥由 `zeroize::Zeroizing` 托管，在作用域结束时自动清零；调用方的 `session_secret` 及其副本须自行保护和清零。

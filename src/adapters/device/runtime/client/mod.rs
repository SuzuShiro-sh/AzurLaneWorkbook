//! 通过 ADB 动态转发端口访问专用 agent 的同步客户端。

mod catalog_cache;
mod rpc;

use std::fmt::{Display, Formatter};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{Ordering, compiler_fence};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
#[cfg(test)]
use suzushiro_json_framing::{
    JsonFrameError, read_json_frame as read_bounded_json_frame,
    write_json_frame as write_bounded_json_frame,
};
use suzushiro_json_framing::{
    LengthPrefixedFrameError, read_length_prefixed_frame, write_length_prefixed_frame,
};
use suzushiro_secure_channel::{
    CLIENT_PROOF_BYTES, HANDSHAKE_NONCE_BYTES, PROTECTED_OVERHEAD_BYTES, SECRET_BYTES,
    SERVER_CHALLENGE_BYTES, SESSION_ID_BYTES, SecureChannel, SecureChannelError,
    answer_server_challenge, verify_server_challenge,
};
use thiserror::Error;

use super::protocol::{
    AgentError, AgentIdentity, ExpectedAgent, HandshakeResponse, MAX_HANDSHAKE_ATTEMPTS,
    MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_VERSION, RequestId, RpcOperation, RpcRequest,
    RpcResponse, RuntimeProtocolError, SessionEffect, UNAUTHENTICATED_PROBE_CONNECTIONS,
    build_rpc_request, next_request_id, validate_agent_error, validate_protocol_version,
    validate_request_id,
};
use crate::adapters::device::session::{SessionId, SessionSecret};

const HANDSHAKE_RETRY_DELAY: Duration = Duration::from_millis(100);
const RPC_TRANSPORT_MARGIN: Duration = Duration::from_secs(1);

// 当前编排固定执行一次认证前探针；构建期约束防止它与共享连接预算静默漂移。
const _: () = assert!(UNAUTHENTICATED_PROBE_CONNECTIONS == 1);

/// 已完成握手且只允许顺序执行 RPC 的 agent 客户端。
#[derive(Debug)]
pub struct AgentClient {
    catalog_cache: Option<catalog_cache::CatalogCache>,
    stream: TcpStream,
    channel: SecureChannel,
    identity: AgentIdentity,
    session_id: SessionId,
    io_timeout: Duration,
    handshake_attempts: u32,
    next_request_id: RequestId,
    usable: bool,
}

impl AgentClient {
    /// 在认证前发送一个超限帧头，确认服务端拒绝后仍可继续接受正式握手。
    pub(crate) fn probe_oversized_unauthenticated_frame(
        address: SocketAddr,
        session_id: SessionId,
        session_secret: &SessionSecret,
        io_timeout: Duration,
    ) -> Result<(), RuntimeClientError> {
        let mut stream: TcpStream = open_configured_stream(address, io_timeout)?;
        let (session_id_bytes, session_secret_bytes) =
            copy_session_credentials(session_id, session_secret);
        let challenge = read_binary_frame(
            &mut stream,
            SERVER_CHALLENGE_BYTES,
            ClientStage::ReadChallenge,
        )?;
        verify_server_challenge(
            &session_id_bytes,
            session_secret_bytes.as_array(),
            &challenge,
        )
        .map_err(|source| RuntimeClientError::SecureChannel {
            stage: ClientStage::ReadChallenge,
            source,
        })?;
        let oversized_length: u32 = u32::try_from(MAX_REQUEST_BYTES + 1).map_err(|_| {
            RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "probe_frame_length_overflow",
                "诊断帧长度无法表示为 32 位无符号整数",
            ))
        })?;
        stream
            .write_all(&oversized_length.to_be_bytes())
            .map_err(|source| RuntimeClientError::Io {
                stage: ClientStage::WriteRequest,
                source,
            })?;

        let mut response_byte: [u8; 1] = [0; 1];
        match stream.read(&mut response_byte) {
            Ok(0) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                ) =>
            {
                Ok(())
            }
            Ok(_) => Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "oversized_frame_not_rejected",
                "agent 未在收到超限帧头后关闭连接",
            ))),
            Err(source) => Err(RuntimeClientError::Io {
                stage: ClientStage::ReadResponse,
                source,
            }),
        }
    }

    /// 连接 ADB 动态转发端口，配置超时并完成身份握手。
    pub fn connect(
        address: SocketAddr,
        expected: ExpectedAgent,
        session_secret: SessionSecret,
        io_timeout: Duration,
    ) -> Result<Self, RuntimeClientError> {
        if io_timeout.is_zero() {
            return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "io_timeout_invalid",
                "I/O 超时必须大于 0",
            )));
        }

        for attempt in 1..=MAX_HANDSHAKE_ATTEMPTS {
            match Self::connect_once(
                address,
                &expected,
                session_secret.clone(),
                io_timeout,
                attempt,
            ) {
                Ok(client) => return Ok(client),
                Err(error) if error.is_retryable_handshake_transport() => {
                    if attempt == MAX_HANDSHAKE_ATTEMPTS {
                        return Err(RuntimeClientError::HandshakeAttemptsExhausted {
                            attempts: attempt,
                            source: Box::new(error),
                        });
                    }
                    thread::sleep(HANDSHAKE_RETRY_DELAY);
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("固定非零握手次数应在循环内返回")
    }

    /// 建立一次 TCP 连接并完成握手；是否重试由外层依据失败阶段决定。
    fn connect_once(
        address: SocketAddr,
        expected: &ExpectedAgent,
        session_secret: SessionSecret,
        io_timeout: Duration,
        handshake_attempts: u32,
    ) -> Result<Self, RuntimeClientError> {
        let mut stream: TcpStream = open_configured_stream(address, io_timeout)?;
        let (session_id_bytes, session_secret_bytes) =
            copy_session_credentials(expected.session_id(), &session_secret);
        let challenge_frame = read_binary_frame(
            &mut stream,
            SERVER_CHALLENGE_BYTES,
            ClientStage::ReadChallenge,
        )?;
        let challenge = verify_server_challenge(
            &session_id_bytes,
            session_secret_bytes.as_array(),
            &challenge_frame,
        )
        .map_err(|source| RuntimeClientError::SecureChannel {
            stage: ClientStage::ReadChallenge,
            source,
        })?;
        let mut client_nonce = SensitiveBytes {
            bytes: [0; HANDSHAKE_NONCE_BYTES],
        };
        getrandom::fill(&mut client_nonce.bytes)
            .map_err(|source| RuntimeClientError::RandomSource { source })?;
        let (proof, key_material) = answer_server_challenge(
            &challenge,
            session_secret_bytes.as_array(),
            client_nonce.as_array(),
        );
        write_binary_frame(
            &mut stream,
            proof.as_bytes(),
            CLIENT_PROOF_BYTES,
            ClientStage::WriteProof,
        )?;
        let mut channel = key_material.into_client_channel();
        let response: HandshakeResponse = read_protected_json_frame(
            &mut stream,
            &mut channel,
            MAX_RESPONSE_BYTES,
            ClientStage::ReadHandshake,
        )?;
        let identity: AgentIdentity = response.verify(expected)?;

        Ok(Self {
            catalog_cache: None,
            stream,
            channel,
            identity,
            session_id: expected.session_id(),
            io_timeout,
            handshake_attempts,
            next_request_id: RequestId::FIRST,
            usable: true,
        })
    }

    /// 返回握手后固定的 agent 身份。
    pub fn identity(&self) -> &AgentIdentity {
        &self.identity
    }

    /// 返回建立当前认证连接实际使用的有界尝试次数。
    pub fn handshake_attempts(&self) -> u32 {
        self.handshake_attempts
    }

    /// 在已认证连接上验证不会破坏会话的稳定错误行为。
    pub(crate) fn run_live_protocol_probes(
        &mut self,
        oversized_frame_closed_connection: bool,
    ) -> Result<LiveProtocolProbeResult, RuntimeClientError> {
        if !self.usable {
            return Err(RuntimeClientError::SessionUnusable);
        }

        let timeout_request_id: RequestId = self.take_next_request_id()?;
        let timeout_code: String = self.expect_raw_error(
            timeout_request_id,
            json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": timeout_request_id,
                "operation": "health",
                "timeout_ms": 0,
                "payload": {}
            }),
            "timeout_out_of_range",
        )?;

        let unsupported_request_id: RequestId = self.take_next_request_id()?;
        let unsupported_code: String = self.expect_raw_error(
            unsupported_request_id,
            json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": unsupported_request_id,
                "operation": "runtime.unregistered",
                "timeout_ms": 5_000,
                "payload": {}
            }),
            "unsupported_operation",
        )?;

        let duplicate_code: String = self.expect_raw_error(
            RequestId::FIRST,
            json!({
                "protocol_version": PROTOCOL_VERSION,
                "request_id": RequestId::FIRST,
                "operation": "health",
                "timeout_ms": 5_000,
                "payload": {}
            }),
            "request_id_not_increasing",
        )?;

        if !oversized_frame_closed_connection {
            return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "oversized_frame_probe_missing",
                "认证前超限帧拒绝证据缺失",
            )));
        }

        Ok(LiveProtocolProbeResult {
            timeout_code,
            unsupported_code,
            duplicate_code,
            oversized_frame_closed_connection,
        })
    }

    /// 顺序发送一个 RPC；通信或契约状态不确定后会封闭当前会话。
    fn request<P, R, V>(
        &mut self,
        operation: RpcOperation,
        timeout_ms: u32,
        payload: P,
        validate_result: V,
    ) -> Result<R, RuntimeClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
        V: FnOnce(&R) -> Result<(), RuntimeProtocolError>,
    {
        self.request_with_socket_budget(operation, timeout_ms, payload, validate_result, None)
    }

    /// 顺序发送一个 RPC。`socket_budget` 存在时，读写等待不能超过调用方剩余预算。
    fn request_with_socket_budget<P, R, V>(
        &mut self,
        operation: RpcOperation,
        timeout_ms: u32,
        payload: P,
        validate_result: V,
        socket_budget: Option<Duration>,
    ) -> Result<R, RuntimeClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
        V: FnOnce(&R) -> Result<(), RuntimeProtocolError>,
    {
        if !self.usable {
            return Err(RuntimeClientError::SessionUnusable);
        }

        let transport_timeout = transport_timeout(self.io_timeout, timeout_ms, socket_budget);
        if transport_timeout.is_zero() {
            return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "observation_budget_exhausted",
                "剩余观察预算为 0，未发送查询或取消",
            )));
        }

        let request_id: RequestId = self.next_request_id;
        let next_request_id: RequestId = next_request_id(request_id)?;
        let request: RpcRequest<P> = build_rpc_request(request_id, operation, timeout_ms, payload)?;
        let deadline = socket_budget.map(|_| Instant::now() + transport_timeout);
        if deadline.is_none()
            && let Err(source) = self
                .stream
                .set_read_timeout(Some(transport_timeout))
                .and_then(|()| self.stream.set_write_timeout(Some(transport_timeout)))
        {
            self.usable = false;
            return Err(RuntimeClientError::Io {
                stage: ClientStage::ConfigureSocket,
                source,
            });
        }

        if let Err(error) = write_protected_json_frame_until(
            &mut self.stream,
            &mut self.channel,
            &request,
            MAX_REQUEST_BYTES,
            ClientStage::WriteRequest,
            deadline,
        ) {
            self.usable = false;
            return Err(error);
        }
        self.next_request_id = next_request_id;

        let response: RpcResponse<R> = match read_protected_json_frame_until(
            &mut self.stream,
            &mut self.channel,
            MAX_RESPONSE_BYTES,
            ClientStage::ReadResponse,
            deadline,
        ) {
            Ok(response) => response,
            Err(error) => {
                self.usable = false;
                return Err(error);
            }
        };

        match response {
            RpcResponse::Ok {
                protocol_version,
                request_id: actual_request_id,
                result,
            } => {
                if let Err(error) = validate_protocol_version(protocol_version)
                    .and_then(|()| validate_request_id(request_id, actual_request_id))
                    .and_then(|()| validate_result(&result))
                {
                    self.usable = false;
                    return Err(error.into());
                }
                Ok(result)
            }
            RpcResponse::Error {
                protocol_version,
                request_id: actual_request_id,
                error,
            } => {
                if let Err(protocol_error) = validate_protocol_version(protocol_version)
                    .and_then(|()| validate_request_id(request_id, actual_request_id))
                    .and_then(|()| validate_agent_error(&error))
                {
                    self.usable = false;
                    return Err(protocol_error.into());
                }
                if error.session_effect != SessionEffect::Unchanged {
                    self.usable = false;
                }
                Err(RuntimeClientError::Agent { request_id, error })
            }
        }
    }

    /// 取出当前线序请求号并提前推进，防止重试复用旧编号。
    fn take_next_request_id(&mut self) -> Result<RequestId, RuntimeClientError> {
        let request_id: RequestId = self.next_request_id;
        self.next_request_id = next_request_id(request_id)?;
        Ok(request_id)
    }

    /// 发送诊断请求并严格核对错误码及会话未改变声明。
    fn expect_raw_error(
        &mut self,
        request_id: RequestId,
        request: Value,
        expected_code: &str,
    ) -> Result<String, RuntimeClientError> {
        if let Err(error) = write_protected_json_frame(
            &mut self.stream,
            &mut self.channel,
            &request,
            MAX_REQUEST_BYTES,
            ClientStage::WriteRequest,
        ) {
            self.usable = false;
            return Err(error);
        }
        let response: RpcResponse<Value> = match read_protected_json_frame(
            &mut self.stream,
            &mut self.channel,
            MAX_RESPONSE_BYTES,
            ClientStage::ReadResponse,
        ) {
            Ok(response) => response,
            Err(error) => {
                self.usable = false;
                return Err(error);
            }
        };

        match response {
            RpcResponse::Error {
                protocol_version,
                request_id: actual_request_id,
                error,
            } => {
                if let Err(error) = validate_protocol_version(protocol_version)
                    .and_then(|()| validate_request_id(request_id, actual_request_id))
                    .and_then(|()| validate_agent_error(&error))
                {
                    self.usable = false;
                    return Err(error.into());
                }
                if error.code != expected_code {
                    self.usable = false;
                    return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                        "live_probe_error_code_mismatch",
                        format!("诊断请求期望错误码 {expected_code}，实际为 {}", error.code),
                    )));
                }
                if error.session_effect != SessionEffect::Unchanged {
                    self.usable = false;
                    return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                        "live_probe_session_effect_mismatch",
                        "可恢复诊断错误必须保持 session_effect=unchanged",
                    )));
                }
                Ok(error.code)
            }
            RpcResponse::Ok { .. } => {
                self.usable = false;
                Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                    "live_probe_expected_error",
                    format!("诊断请求应返回稳定错误 {expected_code}"),
                )))
            }
        }
    }
}

/// 固定长度密钥副本在所有返回和异常路径上执行不可省略的内存擦除。
struct SensitiveBytes<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> SensitiveBytes<N> {
    fn as_array(&self) -> &[u8; N] {
        &self.bytes
    }
}

impl<const N: usize> Drop for SensitiveBytes<N> {
    fn drop(&mut self) {
        for byte in &mut self.bytes {
            // SAFETY: byte 来自有效的独占可变引用，易失写入不改变别名约束。
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }
}

/// 序列化后的明文缓冲在加密或失败返回后立即执行易失擦除。
struct SensitiveVec {
    bytes: Vec<u8>,
}

impl Drop for SensitiveVec {
    fn drop(&mut self) {
        for byte in &mut self.bytes {
            // SAFETY: byte 来自 Vec 的独占可变借用，易失写入保持分配与别名有效。
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        compiler_fence(Ordering::SeqCst);
    }
}

/// 将会话对象复制为密码协议需要的固定数组，并约束密钥副本生命周期。
fn copy_session_credentials(
    session_id: SessionId,
    session_secret: &SessionSecret,
) -> ([u8; SESSION_ID_BYTES], SensitiveBytes<SECRET_BYTES>) {
    let mut session_id_bytes = [0; SESSION_ID_BYTES];
    session_id.copy_bytes_to(&mut session_id_bytes);
    let mut session_secret_bytes = SensitiveBytes {
        bytes: [0; SECRET_BYTES],
    };
    session_secret.copy_bytes_to(&mut session_secret_bytes.bytes);
    (session_id_bytes, session_secret_bytes)
}

/// 套接字等待取 I/O 超时与请求超时加余量的较大值；调用方给出剩余预算时不能再被放大。
pub(crate) fn transport_timeout(
    io_timeout: Duration,
    timeout_ms: u32,
    socket_budget: Option<Duration>,
) -> Duration {
    let configured = io_timeout
        .max(Duration::from_millis(u64::from(timeout_ms)).saturating_add(RPC_TRANSPORT_MARGIN));
    match socket_budget {
        Some(budget) => configured.min(budget),
        None => configured,
    }
}

/// 建立带有界读写超时的 TCP 连接，供认证和认证前帧探测共用。
fn open_configured_stream(
    address: SocketAddr,
    io_timeout: Duration,
) -> Result<TcpStream, RuntimeClientError> {
    if io_timeout.is_zero() {
        return Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
            "io_timeout_invalid",
            "I/O 超时必须大于 0",
        )));
    }
    let stream: TcpStream = TcpStream::connect_timeout(&address, io_timeout).map_err(|source| {
        RuntimeClientError::Io {
            stage: ClientStage::Connect,
            source,
        }
    })?;
    stream
        .set_read_timeout(Some(io_timeout))
        .map_err(|source| RuntimeClientError::Io {
            stage: ClientStage::ConfigureSocket,
            source,
        })?;
    stream
        .set_write_timeout(Some(io_timeout))
        .map_err(|source| RuntimeClientError::Io {
            stage: ClientStage::ConfigureSocket,
            source,
        })?;
    stream
        .set_nodelay(true)
        .map_err(|source| RuntimeClientError::Io {
            stage: ClientStage::ConfigureSocket,
            source,
        })?;
    Ok(stream)
}

/// 真机协议失败路径的稳定结果；只由运行态验证编排器读取。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct LiveProtocolProbeResult {
    pub timeout_code: String,
    pub unsupported_code: String,
    pub duplicate_code: String,
    pub oversized_frame_closed_connection: bool,
}

/// 按四字节大端长度前缀编码 JSON，并在写入前执行方向上限检查。
#[cfg(test)]
fn write_json_frame<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    max_bytes: usize,
    stage: ClientStage,
) -> Result<(), RuntimeClientError> {
    write_bounded_json_frame(writer, value, max_bytes)
        .map_err(|error: JsonFrameError| map_json_frame_error(error, stage))
}

/// JSON 明文在序列化后立即进入有序认证加密帧，线上不发送可读业务内容。
fn write_protected_json_frame<T: Serialize>(
    writer: &mut impl Write,
    channel: &mut SecureChannel,
    value: &T,
    max_plaintext_bytes: usize,
    stage: ClientStage,
) -> Result<(), RuntimeClientError> {
    let frame = seal_protected_frame(channel, value, max_plaintext_bytes, stage)?;
    write_binary_frame(
        writer,
        &frame,
        maximum_protected_bytes(max_plaintext_bytes)?,
        stage,
    )
}

fn write_protected_json_frame_until<T: Serialize>(
    stream: &mut TcpStream,
    channel: &mut SecureChannel,
    value: &T,
    max_plaintext_bytes: usize,
    stage: ClientStage,
    deadline: Option<Instant>,
) -> Result<(), RuntimeClientError> {
    let frame = seal_protected_frame(channel, value, max_plaintext_bytes, stage)?;
    let maximum_wire_bytes = maximum_protected_bytes(max_plaintext_bytes)?;
    match deadline {
        Some(deadline) => {
            write_binary_frame_until(stream, &frame, maximum_wire_bytes, stage, deadline)
        }
        None => write_binary_frame(stream, &frame, maximum_wire_bytes, stage),
    }
}

fn seal_protected_frame<T: Serialize>(
    channel: &mut SecureChannel,
    value: &T,
    max_plaintext_bytes: usize,
    stage: ClientStage,
) -> Result<Vec<u8>, RuntimeClientError> {
    let plaintext = SensitiveVec {
        bytes: serde_json::to_vec(value)
            .map_err(|source| RuntimeClientError::Json { stage, source })?,
    };
    channel
        .outbound
        .seal(&plaintext.bytes, max_plaintext_bytes)
        .map_err(|source| RuntimeClientError::SecureChannel { stage, source })
}

/// 按既有四字节大端长度前缀写入二进制帧，并在任何写入前核对上限。
fn write_binary_frame(
    writer: &mut impl Write,
    payload: &[u8],
    max_bytes: usize,
    stage: ClientStage,
) -> Result<(), RuntimeClientError> {
    write_length_prefixed_frame(writer, payload, max_bytes)
        .map_err(|error| map_length_prefixed_frame_error(error, stage))
}

/// 读取带四字节大端长度前缀的二进制帧，并在分配前核对方向上限。
fn read_binary_frame(
    reader: &mut impl Read,
    max_bytes: usize,
    stage: ClientStage,
) -> Result<Vec<u8>, RuntimeClientError> {
    read_length_prefixed_frame(reader, max_bytes)
        .map_err(|error| map_length_prefixed_frame_error(error, stage))
}

fn write_binary_frame_until(
    stream: &mut TcpStream,
    payload: &[u8],
    max_bytes: usize,
    stage: ClientStage,
    deadline: Instant,
) -> Result<(), RuntimeClientError> {
    let mut io = DeadlineIo {
        stream,
        stage,
        deadline,
    };
    write_length_prefixed_frame(&mut io, payload, max_bytes)
        .map_err(|error| map_length_prefixed_frame_error(error, stage))
}

fn read_binary_frame_until(
    stream: &mut TcpStream,
    max_bytes: usize,
    stage: ClientStage,
    deadline: Instant,
) -> Result<Vec<u8>, RuntimeClientError> {
    let mut io = DeadlineIo {
        stream,
        stage,
        deadline,
    };
    read_length_prefixed_frame(&mut io, max_bytes)
        .map_err(|error| map_length_prefixed_frame_error(error, stage))
}

/// 在每次实际读写前刷新剩余观察预算，让长度前缀帧沿用同一套读写。
struct DeadlineIo<'a> {
    stream: &'a mut TcpStream,
    stage: ClientStage,
    deadline: Instant,
}

impl DeadlineIo<'_> {
    fn prepare(&mut self) -> std::io::Result<()> {
        apply_remaining_timeout(self.stream, self.stage, self.deadline).map_err(|error| match error
        {
            RuntimeClientError::Io { source, .. } => source,
            other => std::io::Error::other(other.to_string()),
        })
    }
}

impl Write for DeadlineIo<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.prepare()?;
        match self.stream.write(buf) {
            Ok(0) => Err(std::io::Error::new(
                ErrorKind::WriteZero,
                "套接字在观察预算内没有接受数据",
            )),
            Ok(count) => Ok(count),
            Err(source) if source.kind() == ErrorKind::Interrupted => Err(source),
            Err(source) => Err(budgeted_io_error(source, self.deadline)),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for DeadlineIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.prepare()?;
        match self.stream.read(buf) {
            Ok(0) => Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "观察预算内对端已关闭",
            )),
            Ok(count) => Ok(count),
            Err(source) if source.kind() == ErrorKind::Interrupted => Err(source),
            Err(source) => Err(budgeted_io_error(source, self.deadline)),
        }
    }
}

#[cfg(test)]
fn write_all_until(
    stream: &mut TcpStream,
    bytes: &[u8],
    stage: ClientStage,
    deadline: Instant,
) -> Result<(), RuntimeClientError> {
    let mut io = DeadlineIo {
        stream,
        stage,
        deadline,
    };
    io.write_all(bytes)
        .map_err(|source| RuntimeClientError::Io { stage, source })
}

#[cfg(test)]
fn read_exact_until(
    stream: &mut TcpStream,
    bytes: &mut [u8],
    stage: ClientStage,
    deadline: Instant,
) -> Result<(), RuntimeClientError> {
    let mut io = DeadlineIo {
        stream,
        stage,
        deadline,
    };
    io.read_exact(bytes)
        .map_err(|source| RuntimeClientError::Io { stage, source })
}

fn map_length_prefixed_frame_error(
    error: LengthPrefixedFrameError,
    stage: ClientStage,
) -> RuntimeClientError {
    match error {
        LengthPrefixedFrameError::Io { source } => RuntimeClientError::Io { stage, source },
        LengthPrefixedFrameError::FrameTooLarge { actual, maximum } => {
            RuntimeClientError::FrameTooLarge {
                stage,
                actual,
                maximum,
            }
        }
        LengthPrefixedFrameError::LengthOverflow { .. } => {
            RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "frame_length_overflow",
                "二进制帧长度无法表示为 32 位无符号整数",
            ))
        }
    }
}

fn apply_remaining_timeout(
    stream: &TcpStream,
    stage: ClientStage,
    deadline: Instant,
) -> Result<(), RuntimeClientError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
    if millis == 0 {
        return Err(RuntimeClientError::Io {
            stage,
            source: std::io::Error::new(ErrorKind::TimedOut, "剩余观察预算已用尽"),
        });
    }
    let timeout = Duration::from_millis(millis);
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|source| RuntimeClientError::Io { stage, source })
}

fn budgeted_io_error(source: std::io::Error, deadline: Instant) -> std::io::Error {
    if matches!(source.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
        || Instant::now() >= deadline
    {
        return std::io::Error::new(ErrorKind::TimedOut, "剩余观察预算已用尽");
    }
    source
}

/// 先验证有序 AEAD 帧，再对已认证明文执行严格 JSON 反序列化。
fn read_protected_json_frame<T: DeserializeOwned>(
    reader: &mut impl Read,
    channel: &mut SecureChannel,
    max_plaintext_bytes: usize,
    stage: ClientStage,
) -> Result<T, RuntimeClientError> {
    let frame = read_binary_frame(reader, maximum_protected_bytes(max_plaintext_bytes)?, stage)?;
    let plaintext = channel
        .inbound
        .open(&frame, max_plaintext_bytes)
        .map_err(|source| RuntimeClientError::SecureChannel { stage, source })?;
    serde_json::from_slice(&plaintext).map_err(|source| RuntimeClientError::Json { stage, source })
}

fn read_protected_json_frame_until<T: DeserializeOwned>(
    stream: &mut TcpStream,
    channel: &mut SecureChannel,
    max_plaintext_bytes: usize,
    stage: ClientStage,
    deadline: Option<Instant>,
) -> Result<T, RuntimeClientError> {
    let maximum = maximum_protected_bytes(max_plaintext_bytes)?;
    let frame = match deadline {
        Some(deadline) => read_binary_frame_until(stream, maximum, stage, deadline)?,
        None => read_binary_frame(stream, maximum, stage)?,
    };
    let plaintext = channel
        .inbound
        .open(&frame, max_plaintext_bytes)
        .map_err(|source| RuntimeClientError::SecureChannel { stage, source })?;
    serde_json::from_slice(&plaintext).map_err(|source| RuntimeClientError::Json { stage, source })
}

/// 受保护帧只比业务明文多固定协议开销，溢出时在任何 I/O 前失败。
fn maximum_protected_bytes(max_plaintext_bytes: usize) -> Result<usize, RuntimeClientError> {
    max_plaintext_bytes
        .checked_add(PROTECTED_OVERHEAD_BYTES)
        .ok_or_else(|| {
            RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "protected_frame_limit_overflow",
                "受保护帧上限计算溢出",
            ))
        })
}

/// 先验证声明长度再分配帧体，避免对端用超限长度触发大内存分配。
#[cfg(test)]
fn read_json_frame<T: DeserializeOwned>(
    reader: &mut impl Read,
    max_bytes: usize,
    stage: ClientStage,
) -> Result<T, RuntimeClientError> {
    read_bounded_json_frame(reader, max_bytes)
        .map_err(|error: JsonFrameError| map_json_frame_error(error, stage))
}

/// 为通用帧错误补充当前客户端阶段，并保持既有稳定错误码。
#[cfg(test)]
fn map_json_frame_error(error: JsonFrameError, stage: ClientStage) -> RuntimeClientError {
    match error {
        JsonFrameError::Io { source } => RuntimeClientError::Io { stage, source },
        JsonFrameError::Json { source } => RuntimeClientError::Json { stage, source },
        JsonFrameError::FrameTooLarge { actual, maximum } => RuntimeClientError::FrameTooLarge {
            stage,
            actual,
            maximum,
        },
        JsonFrameError::LengthOverflow { .. } => {
            RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "frame_length_overflow",
                "JSON 帧长度无法表示为 32 位无符号整数",
            ))
        }
    }
}

/// 宿主与 agent 通信失败时所处的精确阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientStage {
    /// 建立本地 TCP 连接。
    Connect,
    /// 配置连接超时或禁用 Nagle。
    ConfigureSocket,
    /// 读取并验证服务端挑战。
    ReadChallenge,
    /// 写入客户端挑战证明。
    WriteProof,
    /// 读取并解析握手响应。
    ReadHandshake,
    /// 写入 RPC 请求。
    WriteRequest,
    /// 读取并解析 RPC 响应。
    ReadResponse,
    /// 等待 shutdown 收据后的服务端 EOF。
    ReadShutdownEof,
}

impl Display for ClientStage {
    /// 输出稳定阶段键，供错误分类和上层运行收据记录。
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Connect => "host.connect",
            Self::ConfigureSocket => "host.configure_socket",
            Self::ReadChallenge => "host.read_challenge",
            Self::WriteProof => "host.write_proof",
            Self::ReadHandshake => "host.read_handshake",
            Self::WriteRequest => "host.write_request",
            Self::ReadResponse => "host.read_response",
            Self::ReadShutdownEof => "host.read_shutdown_eof",
        })
    }
}

/// 宿主运行态客户端的完整失败集合。
#[derive(Debug, Error)]
pub enum RuntimeClientError {
    /// 连接或读写系统调用失败。
    #[error("运行态通信在 {stage} 阶段失败: {source}")]
    Io {
        /// 出错阶段。
        stage: ClientStage,
        /// 操作系统错误。
        #[source]
        source: std::io::Error,
    },
    /// 仅握手传输阶段的有界重连已经耗尽。
    #[error("运行态握手在 {attempts} 次连接后仍失败: {source}")]
    HandshakeAttemptsExhausted {
        /// 已执行的固定上限次数。
        attempts: u32,
        /// 最后一次传输错误，保留其精确阶段和系统错误。
        #[source]
        source: Box<RuntimeClientError>,
    },
    /// JSON 编码或严格解码失败。
    #[error("运行态 JSON 在 {stage} 阶段无效: {source}")]
    Json {
        /// 出错阶段。
        stage: ClientStage,
        /// JSON 错误。
        #[source]
        source: serde_json::Error,
    },
    /// 对端声明或本地编码的帧超过冻结上限。
    #[error("运行态帧在 {stage} 阶段为 {actual} 字节，超过上限 {maximum}")]
    FrameTooLarge {
        /// 出错阶段。
        stage: ClientStage,
        /// 实际或声明长度。
        actual: usize,
        /// 该方向允许的最大长度。
        maximum: usize,
    },
    /// 挑战证明或受保护帧违反安全通道契约。
    #[error("安全通道在 {stage} 阶段无效: {source}")]
    SecureChannel {
        /// 出错阶段。
        stage: ClientStage,
        /// 认证或加密协议错误。
        #[source]
        source: SecureChannelError,
    },
    /// 操作系统随机源不可用，禁止退化为可预测随机数。
    #[error("操作系统安全随机源不可用: {source}")]
    RandomSource {
        /// 系统随机源错误。
        #[source]
        source: getrandom::Error,
    },
    /// 数据结构正确但违反运行态契约。
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
    /// agent 明确返回业务或运行态错误。
    #[error("agent 请求 {request_id} 失败: {}: {}", error.code, error.message)]
    Agent {
        /// 对应请求标识。
        request_id: RequestId,
        /// agent 的稳定错误。
        error: AgentError,
    },
    /// 先前通信已令会话状态不可信。
    #[error("当前运行态会话已经不可继续使用")]
    SessionUnusable,
}

impl RuntimeClientError {
    /// 只有发送客户端证明前的 TCP I/O 错误允许有界重连。
    fn is_retryable_handshake_transport(&self) -> bool {
        matches!(
            self,
            Self::Io {
                stage: ClientStage::Connect | ClientStage::ReadChallenge,
                ..
            }
        )
    }

    /// 返回可供测试和上层分类的稳定错误码。
    pub fn code(&self) -> &str {
        match self {
            Self::Io { .. } => "runtime_io_failed",
            Self::HandshakeAttemptsExhausted { .. } => "runtime_handshake_attempts_exhausted",
            Self::Json { .. } => "runtime_json_invalid",
            Self::FrameTooLarge { .. } => "runtime_frame_too_large",
            Self::SecureChannel { .. } => "runtime_secure_channel_invalid",
            Self::RandomSource { .. } => "runtime_secure_random_unavailable",
            Self::Protocol(error) => error.code,
            Self::Agent { error, .. } => &error.code,
            Self::SessionUnusable => "runtime_session_unusable",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    use serde::{Deserialize, Serialize};
    use suzushiro_secure_channel::create_server_challenge;

    use super::{
        AgentClient, ClientStage, MAX_REQUEST_BYTES, RuntimeClientError, read_json_frame,
        write_binary_frame, write_json_frame,
    };
    use crate::adapters::device::session::{SessionId, SessionSecret};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    /// 帧编解码单元测试使用的最小严格 JSON 结构。
    struct Sample {
        value: u32,
    }

    #[test]
    fn socket_budget_limits_transport_timeout() {
        use super::transport_timeout;

        assert_eq!(
            transport_timeout(Duration::from_secs(30), 30_000, None),
            Duration::from_secs(31)
        );
        assert_eq!(
            transport_timeout(
                Duration::from_secs(30),
                30_000,
                Some(Duration::from_millis(200))
            ),
            Duration::from_millis(200)
        );
        assert_eq!(
            transport_timeout(Duration::from_millis(100), 500, None),
            Duration::from_millis(1_500)
        );
    }

    #[test]
    fn stalled_read_stops_at_the_socket_budget() {
        use super::transport_timeout;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(3));
        });
        let mut stream = TcpStream::connect(address).unwrap();
        let timeout = transport_timeout(
            Duration::from_secs(30),
            30_000,
            Some(Duration::from_millis(200)),
        );
        stream.set_read_timeout(Some(timeout)).unwrap();
        stream.set_write_timeout(Some(timeout)).unwrap();
        let started = Instant::now();
        let mut buffer = [0_u8; 1];
        let error = stream.read(&mut buffer).unwrap_err();
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(1000), "{elapsed:?}");
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ),
            "{error:?}"
        );
        server.join().unwrap();
    }

    #[test]
    fn dripped_bytes_cannot_reset_the_request_deadline() {
        use super::read_exact_until;
        use std::io::ErrorKind;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = listener.accept().unwrap().0;
            for _ in 0..24 {
                if stream.write(&[0x11]).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(80));
            }
        });
        let mut stream = TcpStream::connect(address).unwrap();
        let started = Instant::now();
        let error = read_exact_until(
            &mut stream,
            &mut [0; 24],
            ClientStage::ReadResponse,
            started + Duration::from_millis(300),
        )
        .unwrap_err();
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");
        assert!(elapsed > Duration::from_millis(200), "{elapsed:?}");
        match error {
            RuntimeClientError::Io { source, .. } => {
                assert_eq!(source.kind(), ErrorKind::TimedOut, "{source}")
            }
            other => panic!("应在总预算内超时，实际为 {other}"),
        }
        server.join().unwrap();
    }

    #[test]
    fn segmented_frame_completes_when_the_total_stays_inside_the_deadline() {
        use super::read_exact_until;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let payload = b"segmented-frame-ok";
        let server_payload = *payload;
        let server = thread::spawn(move || {
            let mut stream = listener.accept().unwrap().0;
            for byte in server_payload {
                stream.write_all(&[byte]).unwrap();
                thread::sleep(Duration::from_millis(5));
            }
        });
        let mut stream = TcpStream::connect(address).unwrap();
        let mut received = vec![0; payload.len()];
        read_exact_until(
            &mut stream,
            &mut received,
            ClientStage::ReadResponse,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(received, payload);
        server.join().unwrap();
    }

    #[test]
    fn exhausted_deadline_does_not_write() {
        use super::write_all_until;
        use std::io::ErrorKind;
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut stream = listener.accept().unwrap().0;
            stream
                .set_read_timeout(Some(Duration::from_millis(300)))
                .unwrap();
            let mut buffer = [0; 8];
            stream.read(&mut buffer).unwrap_or(0)
        });
        let mut stream = TcpStream::connect(address).unwrap();
        let error = write_all_until(
            &mut stream,
            b"nope",
            ClientStage::WriteRequest,
            Instant::now(),
        )
        .unwrap_err();
        match error {
            RuntimeClientError::Io { source, .. } => {
                assert_eq!(source.kind(), ErrorKind::TimedOut, "{source}")
            }
            other => panic!("零剩余预算不得写入，实际为 {other}"),
        }
        drop(stream);
        assert_eq!(server.join().unwrap(), 0);
    }

    #[test]
    /// 验证帧头使用网络字节序且编码结果可以无损解码。
    fn framing_uses_big_endian_length() {
        let mut output: Vec<u8> = Vec::new();
        write_json_frame(
            &mut output,
            &Sample { value: 7 },
            11,
            ClientStage::WriteRequest,
        )
        .unwrap();

        assert_eq!(&output[..4], &[0, 0, 0, 11]);
        let decoded: Sample =
            read_json_frame(&mut Cursor::new(output), 11, ClientStage::ReadResponse).unwrap();
        assert_eq!(decoded, Sample { value: 7 });
    }

    #[test]
    /// 验证二进制认证帧复用网络字节序长度头且不经过 JSON 编码。
    fn binary_framing_uses_big_endian_length() {
        let mut output: Vec<u8> = Vec::new();
        write_binary_frame(&mut output, &[2, 1, 0xaa], 3, ClientStage::WriteProof).unwrap();

        assert_eq!(output, [0, 0, 0, 3, 2, 1, 0xaa]);
    }

    #[test]
    /// 验证本地 JSON 超过写入上限时不会产生部分帧。
    fn oversized_encoded_frame_does_not_write_partial_output() {
        let mut output: Vec<u8> = Vec::new();
        let error: RuntimeClientError = write_json_frame(
            &mut output,
            &Sample { value: 7 },
            10,
            ClientStage::WriteRequest,
        )
        .unwrap_err();

        assert!(output.is_empty());
        assert!(matches!(
            error,
            RuntimeClientError::FrameTooLarge {
                stage: ClientStage::WriteRequest,
                actual: 11,
                maximum: 10,
            }
        ));
    }

    #[test]
    /// 验证超限帧只读取长度头，不会继续分配或读取帧体。
    fn oversized_header_fails_before_payload_read() {
        let input: Vec<u8> = 2048_u32.to_be_bytes().to_vec();
        let error: RuntimeClientError =
            read_json_frame::<Sample>(&mut Cursor::new(input), 1024, ClientStage::ReadResponse)
                .unwrap_err();

        assert_eq!(error.code(), "runtime_frame_too_large");
    }

    #[test]
    /// 验证完整帧内的畸形 JSON 保持为带读取阶段的 JSON 错误。
    fn malformed_json_keeps_read_stage() {
        let mut input: Vec<u8> = 1_u32.to_be_bytes().to_vec();
        input.push(b'{');

        let error: RuntimeClientError =
            read_json_frame::<Sample>(&mut Cursor::new(input), 1024, ClientStage::ReadResponse)
                .unwrap_err();

        assert!(matches!(
            error,
            RuntimeClientError::Json {
                stage: ClientStage::ReadResponse,
                ..
            }
        ));
    }

    #[test]
    /// 验证截断的长度头保持为带读取阶段的 I/O 错误。
    fn truncated_header_keeps_read_stage() {
        let error: RuntimeClientError =
            read_json_frame::<Sample>(&mut Cursor::new([0_u8, 0]), 1024, ClientStage::ReadResponse)
                .unwrap_err();

        assert!(matches!(
            error,
            RuntimeClientError::Io {
                stage: ClientStage::ReadResponse,
                source,
            } if source.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    /// 验证截断的帧体保持为带读取阶段的 I/O 错误。
    fn truncated_payload_keeps_read_stage() {
        let mut input: Vec<u8> = 11_u32.to_be_bytes().to_vec();
        input.extend_from_slice(br#"{"value":7}"#);
        input.pop();

        let error: RuntimeClientError =
            read_json_frame::<Sample>(&mut Cursor::new(input), 1024, ClientStage::ReadResponse)
                .unwrap_err();

        assert!(matches!(
            error,
            RuntimeClientError::Io {
                stage: ClientStage::ReadResponse,
                source,
            } if source.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    /// 验证零长度帧继续交由严格 JSON 解码并归类为 JSON 错误。
    fn empty_payload_is_invalid_json() {
        let input: Vec<u8> = 0_u32.to_be_bytes().to_vec();
        let error: RuntimeClientError =
            read_json_frame::<Sample>(&mut Cursor::new(input), 1024, ClientStage::ReadResponse)
                .unwrap_err();

        assert!(matches!(
            error,
            RuntimeClientError::Json {
                stage: ClientStage::ReadResponse,
                ..
            }
        ));
    }

    #[test]
    /// 验证认证前超限探测只发送帧头，并以服务端关闭连接作为成功证据。
    fn unauthenticated_oversized_probe_observes_connection_close() {
        let session_id: SessionId = "00112233445566778899aabbccddeeff".parse().unwrap();
        let session_secret: SessionSecret =
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                .parse()
                .unwrap();
        let (session_id_bytes, session_secret_bytes) =
            super::copy_session_credentials(session_id, &session_secret);
        let challenge = create_server_challenge(
            &session_id_bytes,
            session_secret_bytes.as_array(),
            &[0x44; suzushiro_secure_channel::HANDSHAKE_NONCE_BYTES],
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            write_binary_frame(
                &mut stream,
                challenge.as_bytes(),
                suzushiro_secure_channel::SERVER_CHALLENGE_BYTES,
                ClientStage::ReadChallenge,
            )
            .unwrap();
            let mut header = [0_u8; 4];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(
                u32::from_be_bytes(header),
                u32::try_from(MAX_REQUEST_BYTES + 1).unwrap()
            );
        });

        AgentClient::probe_oversized_unauthenticated_frame(
            address,
            session_id,
            &session_secret,
            Duration::from_secs(1),
        )
        .unwrap();
        server.join().unwrap();
    }
}

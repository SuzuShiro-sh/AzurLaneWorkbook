//! 提供跨语言可复用的挑战证明、方向密钥派生和有序认证加密帧。

use blake2::{
    Blake2bMac,
    digest::{KeyInit as BlakeKeyInit, Mac, consts::U16, consts::U32},
};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce, aead::AeadInOut};
use std::fmt::{Debug, Formatter};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

pub const CHANNEL_VERSION: u8 = 1;
pub const SERVER_CHALLENGE_TYPE: u8 = 1;
pub const CLIENT_PROOF_TYPE: u8 = 2;
pub const PROTECTED_FRAME_TYPE: u8 = 3;
pub const SESSION_ID_BYTES: usize = 16;
pub const SECRET_BYTES: usize = 32;
pub const HANDSHAKE_NONCE_BYTES: usize = 32;
pub const PROOF_BYTES: usize = 32;
pub const KEY_BYTES: usize = 32;
pub const NONCE_PREFIX_BYTES: usize = 16;
pub const AEAD_NONCE_BYTES: usize = 24;
pub const AEAD_TAG_BYTES: usize = 16;
pub const SERVER_CHALLENGE_BYTES: usize = 82;
pub const CLIENT_PROOF_BYTES: usize = 66;
pub const PROTECTED_HEADER_BYTES: usize = 10;
pub const PROTECTED_OVERHEAD_BYTES: usize = PROTECTED_HEADER_BYTES + AEAD_TAG_BYTES;

const SERVER_PROOF_LABEL: &[u8] = b"suzushiro.secure-channel.v1/server-proof";
const CLIENT_PROOF_LABEL: &[u8] = b"suzushiro.secure-channel.v1/client-proof";
const HOST_TO_AGENT_KEY_LABEL: &[u8] = b"suzushiro.secure-channel.v1/host-to-agent-key";
const AGENT_TO_HOST_KEY_LABEL: &[u8] = b"suzushiro.secure-channel.v1/agent-to-host-key";
const HOST_TO_AGENT_NONCE_LABEL: &[u8] = b"suzushiro.secure-channel.v1/host-to-agent-nonce";
const AGENT_TO_HOST_NONCE_LABEL: &[u8] = b"suzushiro.secure-channel.v1/agent-to-host-nonce";

const CHALLENGE_SESSION_OFFSET: usize = 2;
const CHALLENGE_NONCE_OFFSET: usize = CHALLENGE_SESSION_OFFSET + SESSION_ID_BYTES;
const CHALLENGE_PROOF_OFFSET: usize = CHALLENGE_NONCE_OFFSET + HANDSHAKE_NONCE_BYTES;
const CLIENT_NONCE_OFFSET: usize = 2;
const CLIENT_PROOF_OFFSET: usize = CLIENT_NONCE_OFFSET + HANDSHAKE_NONCE_BYTES;

const _: () = assert!(CHALLENGE_PROOF_OFFSET + PROOF_BYTES == SERVER_CHALLENGE_BYTES);
const _: () = assert!(CLIENT_PROOF_OFFSET + PROOF_BYTES == CLIENT_PROOF_BYTES);
const _: () = assert!(NONCE_PREFIX_BYTES + 8 == AEAD_NONCE_BYTES);

/// 固定长度服务端挑战；证明用于先确认对端持有会话密钥。
pub struct ServerChallengeFrame {
    bytes: [u8; SERVER_CHALLENGE_BYTES],
}

impl ServerChallengeFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for ServerChallengeFrame {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// 固定长度客户端证明；不包含会话密钥原文。
pub struct ClientProofFrame {
    bytes: [u8; CLIENT_PROOF_BYTES],
}

impl ClientProofFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for ClientProofFrame {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// 已验证挑战中的固定会话上下文。
pub struct VerifiedServerChallenge {
    frame: ServerChallengeFrame,
    session_id: [u8; SESSION_ID_BYTES],
    server_nonce: [u8; HANDSHAKE_NONCE_BYTES],
}

impl VerifiedServerChallenge {
    pub fn as_bytes(&self) -> &[u8] {
        self.frame.as_bytes()
    }
}

/// 双向独立的密钥和 nonce 前缀；密钥在析构时自动清除。
pub struct ChannelKeyMaterial {
    host_to_agent_key: Zeroizing<[u8; KEY_BYTES]>,
    agent_to_host_key: Zeroizing<[u8; KEY_BYTES]>,
    host_to_agent_nonce_prefix: [u8; NONCE_PREFIX_BYTES],
    agent_to_host_nonce_prefix: [u8; NONCE_PREFIX_BYTES],
}

impl ChannelKeyMaterial {
    pub fn host_to_agent_key(&self) -> &[u8; KEY_BYTES] {
        &self.host_to_agent_key
    }

    pub fn agent_to_host_key(&self) -> &[u8; KEY_BYTES] {
        &self.agent_to_host_key
    }

    pub fn host_to_agent_nonce_prefix(&self) -> &[u8; NONCE_PREFIX_BYTES] {
        &self.host_to_agent_nonce_prefix
    }

    pub fn agent_to_host_nonce_prefix(&self) -> &[u8; NONCE_PREFIX_BYTES] {
        &self.agent_to_host_nonce_prefix
    }

    pub fn into_client_channel(self) -> SecureChannel {
        SecureChannel {
            outbound: FrameSealer::new(self.host_to_agent_key, self.host_to_agent_nonce_prefix),
            inbound: FrameOpener::new(self.agent_to_host_key, self.agent_to_host_nonce_prefix),
        }
    }

    pub fn into_server_channel(self) -> SecureChannel {
        SecureChannel {
            outbound: FrameSealer::new(self.agent_to_host_key, self.agent_to_host_nonce_prefix),
            inbound: FrameOpener::new(self.host_to_agent_key, self.host_to_agent_nonce_prefix),
        }
    }
}

/// 一条连接的独立发送和接收状态。
pub struct SecureChannel {
    pub outbound: FrameSealer,
    pub inbound: FrameOpener,
}

impl Debug for SecureChannel {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecureChannel")
            .field("outbound", &"[REDACTED]")
            .field("inbound", &"[REDACTED]")
            .finish()
    }
}

/// 按单调序号生成认证密文帧。
pub struct FrameSealer {
    cipher: XChaCha20Poly1305,
    nonce_prefix: [u8; NONCE_PREFIX_BYTES],
    next_sequence: u64,
}

impl FrameSealer {
    pub fn new(key: Zeroizing<[u8; KEY_BYTES]>, nonce_prefix: [u8; NONCE_PREFIX_BYTES]) -> Self {
        let key_array = Key::try_from(key.as_ref()).expect("固定密钥长度必须为 32 字节");
        let cipher = XChaCha20Poly1305::new(&key_array);
        Self {
            cipher,
            nonce_prefix,
            next_sequence: 0,
        }
    }

    pub fn with_sequence(
        key: Zeroizing<[u8; KEY_BYTES]>,
        nonce_prefix: [u8; NONCE_PREFIX_BYTES],
        next_sequence: u64,
    ) -> Self {
        let mut sealer = Self::new(key, nonce_prefix);
        sealer.next_sequence = next_sequence;
        sealer
    }

    pub fn seal(
        &mut self,
        plaintext: &[u8],
        maximum_plaintext_bytes: usize,
    ) -> Result<Vec<u8>, SecureChannelError> {
        if plaintext.len() > maximum_plaintext_bytes {
            return Err(SecureChannelError::PlaintextTooLarge {
                actual: plaintext.len(),
                maximum: maximum_plaintext_bytes,
            });
        }
        if self.next_sequence == u64::MAX {
            return Err(SecureChannelError::SequenceExhausted);
        }

        let sequence = self.next_sequence;
        let mut frame = Zeroizing::new(Vec::with_capacity(
            PROTECTED_OVERHEAD_BYTES + plaintext.len(),
        ));
        frame.push(CHANNEL_VERSION);
        frame.push(PROTECTED_FRAME_TYPE);
        frame.extend_from_slice(&sequence.to_be_bytes());
        frame.extend_from_slice(plaintext);
        let nonce_bytes = make_nonce(&self.nonce_prefix, sequence);
        let nonce = XNonce::try_from(&nonce_bytes[..]).expect("固定 nonce 长度必须为 24 字节");
        let (header, ciphertext) = frame.split_at_mut(PROTECTED_HEADER_BYTES);
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, header, ciphertext.into())
            .map_err(|_| SecureChannelError::EncryptionFailed)?;
        frame.extend_from_slice(tag.as_slice());
        self.next_sequence += 1;
        Ok(std::mem::take(&mut *frame))
    }
}

/// 严格按预期序号验证并解密认证帧。
pub struct FrameOpener {
    cipher: XChaCha20Poly1305,
    nonce_prefix: [u8; NONCE_PREFIX_BYTES],
    next_sequence: u64,
}

impl FrameOpener {
    pub fn new(key: Zeroizing<[u8; KEY_BYTES]>, nonce_prefix: [u8; NONCE_PREFIX_BYTES]) -> Self {
        let key_array = Key::try_from(key.as_ref()).expect("固定密钥长度必须为 32 字节");
        let cipher = XChaCha20Poly1305::new(&key_array);
        Self {
            cipher,
            nonce_prefix,
            next_sequence: 0,
        }
    }

    pub fn with_sequence(
        key: Zeroizing<[u8; KEY_BYTES]>,
        nonce_prefix: [u8; NONCE_PREFIX_BYTES],
        next_sequence: u64,
    ) -> Self {
        let mut opener = Self::new(key, nonce_prefix);
        opener.next_sequence = next_sequence;
        opener
    }

    pub fn open(
        &mut self,
        frame: &[u8],
        maximum_plaintext_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, SecureChannelError> {
        if frame.len() < PROTECTED_OVERHEAD_BYTES {
            return Err(SecureChannelError::ProtectedFrameTooShort {
                actual: frame.len(),
            });
        }
        let plaintext_bytes = frame.len() - PROTECTED_OVERHEAD_BYTES;
        if plaintext_bytes > maximum_plaintext_bytes {
            return Err(SecureChannelError::PlaintextTooLarge {
                actual: plaintext_bytes,
                maximum: maximum_plaintext_bytes,
            });
        }
        if frame[0] != CHANNEL_VERSION || frame[1] != PROTECTED_FRAME_TYPE {
            return Err(SecureChannelError::ProtectedFrameHeaderInvalid);
        }
        let sequence = u64::from_be_bytes(
            frame[2..PROTECTED_HEADER_BYTES]
                .try_into()
                .expect("固定帧头序号区间长度必须为 8"),
        );
        if sequence != self.next_sequence {
            return Err(SecureChannelError::SequenceMismatch {
                expected: self.next_sequence,
                actual: sequence,
            });
        }
        if self.next_sequence == u64::MAX {
            return Err(SecureChannelError::SequenceExhausted);
        }

        let tag_offset = frame.len() - AEAD_TAG_BYTES;
        let mut plaintext = Zeroizing::new(frame[PROTECTED_HEADER_BYTES..tag_offset].to_vec());
        let tag = Tag::try_from(&frame[tag_offset..]).map_err(|_| {
            SecureChannelError::ProtectedFrameTooShort {
                actual: frame.len(),
            }
        })?;
        let nonce_bytes = make_nonce(&self.nonce_prefix, sequence);
        let nonce = XNonce::try_from(&nonce_bytes[..]).expect("固定 nonce 长度必须为 24 字节");
        self.cipher
            .decrypt_inout_detached(
                &nonce,
                &frame[..PROTECTED_HEADER_BYTES],
                plaintext.as_mut_slice().into(),
                &tag,
            )
            .map_err(|_| SecureChannelError::AuthenticationFailed)?;
        self.next_sequence += 1;
        Ok(plaintext)
    }
}

/// 使用随机服务端 nonce 构造带服务端密钥证明的挑战。
pub fn create_server_challenge(
    session_id: &[u8; SESSION_ID_BYTES],
    session_secret: &[u8; SECRET_BYTES],
    server_nonce: &[u8; HANDSHAKE_NONCE_BYTES],
) -> ServerChallengeFrame {
    let mut bytes = [0; SERVER_CHALLENGE_BYTES];
    bytes[0] = CHANNEL_VERSION;
    bytes[1] = SERVER_CHALLENGE_TYPE;
    bytes[CHALLENGE_SESSION_OFFSET..CHALLENGE_NONCE_OFFSET].copy_from_slice(session_id);
    bytes[CHALLENGE_NONCE_OFFSET..CHALLENGE_PROOF_OFFSET].copy_from_slice(server_nonce);
    let proof = proof_mac(
        session_secret,
        SERVER_PROOF_LABEL,
        &[session_id, server_nonce],
    );
    bytes[CHALLENGE_PROOF_OFFSET..].copy_from_slice(proof.as_ref());
    ServerChallengeFrame { bytes }
}

/// 验证挑战头、会话标识和服务端证明。
pub fn verify_server_challenge(
    expected_session_id: &[u8; SESSION_ID_BYTES],
    session_secret: &[u8; SECRET_BYTES],
    frame: &[u8],
) -> Result<VerifiedServerChallenge, SecureChannelError> {
    if frame.len() != SERVER_CHALLENGE_BYTES {
        return Err(SecureChannelError::ServerChallengeLengthInvalid {
            actual: frame.len(),
        });
    }
    if frame[0] != CHANNEL_VERSION || frame[1] != SERVER_CHALLENGE_TYPE {
        return Err(SecureChannelError::ServerChallengeHeaderInvalid);
    }
    if frame[CHALLENGE_SESSION_OFFSET..CHALLENGE_NONCE_OFFSET] != expected_session_id[..] {
        return Err(SecureChannelError::SessionMismatch);
    }
    let mut server_nonce = [0; HANDSHAKE_NONCE_BYTES];
    server_nonce.copy_from_slice(&frame[CHALLENGE_NONCE_OFFSET..CHALLENGE_PROOF_OFFSET]);
    verify_proof(
        session_secret,
        SERVER_PROOF_LABEL,
        &[expected_session_id, &server_nonce],
        &frame[CHALLENGE_PROOF_OFFSET..],
    )?;
    let mut owned = [0; SERVER_CHALLENGE_BYTES];
    owned.copy_from_slice(frame);
    Ok(VerifiedServerChallenge {
        frame: ServerChallengeFrame { bytes: owned },
        session_id: *expected_session_id,
        server_nonce,
    })
}

/// 回应已验证挑战，并派生当前连接唯一的双向密钥。
pub fn answer_server_challenge(
    challenge: &VerifiedServerChallenge,
    session_secret: &[u8; SECRET_BYTES],
    client_nonce: &[u8; HANDSHAKE_NONCE_BYTES],
) -> (ClientProofFrame, ChannelKeyMaterial) {
    let mut bytes = [0; CLIENT_PROOF_BYTES];
    bytes[0] = CHANNEL_VERSION;
    bytes[1] = CLIENT_PROOF_TYPE;
    bytes[CLIENT_NONCE_OFFSET..CLIENT_PROOF_OFFSET].copy_from_slice(client_nonce);
    let proof = proof_mac(
        session_secret,
        CLIENT_PROOF_LABEL,
        &[challenge.as_bytes(), client_nonce],
    );
    bytes[CLIENT_PROOF_OFFSET..].copy_from_slice(proof.as_ref());
    let keys = derive_keys(
        session_secret,
        &challenge.session_id,
        &challenge.server_nonce,
        client_nonce,
    );
    (ClientProofFrame { bytes }, keys)
}

/// 验证客户端证明，并派生与宿主完全相同的双向密钥。
pub fn verify_client_proof(
    challenge: &ServerChallengeFrame,
    session_id: &[u8; SESSION_ID_BYTES],
    session_secret: &[u8; SECRET_BYTES],
    frame: &[u8],
) -> Result<ChannelKeyMaterial, SecureChannelError> {
    if frame.len() != CLIENT_PROOF_BYTES {
        return Err(SecureChannelError::ClientProofLengthInvalid {
            actual: frame.len(),
        });
    }
    if frame[0] != CHANNEL_VERSION || frame[1] != CLIENT_PROOF_TYPE {
        return Err(SecureChannelError::ClientProofHeaderInvalid);
    }
    let mut client_nonce = [0; HANDSHAKE_NONCE_BYTES];
    client_nonce.copy_from_slice(&frame[CLIENT_NONCE_OFFSET..CLIENT_PROOF_OFFSET]);
    verify_proof(
        session_secret,
        CLIENT_PROOF_LABEL,
        &[challenge.as_bytes(), &client_nonce],
        &frame[CLIENT_PROOF_OFFSET..],
    )?;
    let mut server_nonce = [0; HANDSHAKE_NONCE_BYTES];
    server_nonce
        .copy_from_slice(&challenge.as_bytes()[CHALLENGE_NONCE_OFFSET..CHALLENGE_PROOF_OFFSET]);
    Ok(derive_keys(
        session_secret,
        session_id,
        &server_nonce,
        &client_nonce,
    ))
}

fn derive_keys(
    session_secret: &[u8; SECRET_BYTES],
    session_id: &[u8; SESSION_ID_BYTES],
    server_nonce: &[u8; HANDSHAKE_NONCE_BYTES],
    client_nonce: &[u8; HANDSHAKE_NONCE_BYTES],
) -> ChannelKeyMaterial {
    let context: [&[u8]; 3] = [session_id, server_nonce, client_nonce];
    ChannelKeyMaterial {
        host_to_agent_key: proof_mac(session_secret, HOST_TO_AGENT_KEY_LABEL, &context),
        agent_to_host_key: proof_mac(session_secret, AGENT_TO_HOST_KEY_LABEL, &context),
        host_to_agent_nonce_prefix: nonce_prefix(
            session_secret,
            HOST_TO_AGENT_NONCE_LABEL,
            &context,
        ),
        agent_to_host_nonce_prefix: nonce_prefix(
            session_secret,
            AGENT_TO_HOST_NONCE_LABEL,
            &context,
        ),
    }
}

fn proof_mac(
    key: &[u8; SECRET_BYTES],
    label: &[u8],
    parts: &[&[u8]],
) -> Zeroizing<[u8; PROOF_BYTES]> {
    let mut mac = <Blake2bMac<U32> as BlakeKeyInit>::new_from_slice(key)
        .expect("固定 32 字节会话密钥必须满足 BLAKE2b keyed 模式");
    Mac::update(&mut mac, label);
    for part in parts {
        Mac::update(&mut mac, part);
    }
    let mut output = Zeroizing::new([0; PROOF_BYTES]);
    output.copy_from_slice(mac.finalize().into_bytes().as_slice());
    output
}

fn verify_proof(
    key: &[u8; SECRET_BYTES],
    label: &[u8],
    parts: &[&[u8]],
    proof: &[u8],
) -> Result<(), SecureChannelError> {
    let mut mac = <Blake2bMac<U32> as BlakeKeyInit>::new_from_slice(key)
        .expect("固定 32 字节会话密钥必须满足 BLAKE2b keyed 模式");
    Mac::update(&mut mac, label);
    for part in parts {
        Mac::update(&mut mac, part);
    }
    mac.verify_slice(proof)
        .map_err(|_| SecureChannelError::ProofInvalid)
}

fn nonce_prefix(
    key: &[u8; SECRET_BYTES],
    label: &[u8],
    parts: &[&[u8]],
) -> [u8; NONCE_PREFIX_BYTES] {
    let mut mac = <Blake2bMac<U16> as BlakeKeyInit>::new_from_slice(key)
        .expect("固定 32 字节会话密钥必须满足 BLAKE2b keyed 模式");
    Mac::update(&mut mac, label);
    for part in parts {
        Mac::update(&mut mac, part);
    }
    let mut output = [0; NONCE_PREFIX_BYTES];
    output.copy_from_slice(mac.finalize().into_bytes().as_slice());
    output
}

fn make_nonce(prefix: &[u8; NONCE_PREFIX_BYTES], sequence: u64) -> [u8; AEAD_NONCE_BYTES] {
    let mut nonce = [0; AEAD_NONCE_BYTES];
    nonce[..NONCE_PREFIX_BYTES].copy_from_slice(prefix);
    nonce[NONCE_PREFIX_BYTES..].copy_from_slice(&sequence.to_be_bytes());
    nonce
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SecureChannelError {
    #[error("服务端挑战长度无效: {actual}")]
    ServerChallengeLengthInvalid { actual: usize },
    #[error("服务端挑战版本或类型无效")]
    ServerChallengeHeaderInvalid,
    #[error("服务端挑战会话标识不匹配")]
    SessionMismatch,
    #[error("挑战证明无效")]
    ProofInvalid,
    #[error("客户端证明长度无效: {actual}")]
    ClientProofLengthInvalid { actual: usize },
    #[error("客户端证明版本或类型无效")]
    ClientProofHeaderInvalid,
    #[error("明文长度 {actual} 超过上限 {maximum}")]
    PlaintextTooLarge { actual: usize, maximum: usize },
    #[error("认证加密序号已经耗尽")]
    SequenceExhausted,
    #[error("认证加密失败")]
    EncryptionFailed,
    #[error("受保护帧长度过短: {actual}")]
    ProtectedFrameTooShort { actual: usize },
    #[error("受保护帧版本或类型无效")]
    ProtectedFrameHeaderInvalid,
    #[error("受保护帧序号不匹配: 期望 {expected}，实际 {actual}")]
    SequenceMismatch { expected: u64, actual: u64 },
    #[error("受保护帧认证失败")]
    AuthenticationFailed,
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    const SHARED_CONTRACT: &str = include_str!("../secure-channel-v1.json");

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Contract {
        description: String,
        schema_version: u32,
        constants: Constants,
        labels: Labels,
        fixture: Fixture,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Constants {
        channel_version: u8,
        server_challenge_type: u8,
        client_proof_type: u8,
        protected_frame_type: u8,
        session_id_bytes: usize,
        secret_bytes: usize,
        handshake_nonce_bytes: usize,
        proof_bytes: usize,
        key_bytes: usize,
        nonce_prefix_bytes: usize,
        aead_nonce_bytes: usize,
        aead_tag_bytes: usize,
        server_challenge_bytes: usize,
        client_proof_bytes: usize,
        protected_header_bytes: usize,
        protected_overhead_bytes: usize,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Labels {
        server_proof: String,
        client_proof: String,
        host_to_agent_key: String,
        agent_to_host_key: String,
        host_to_agent_nonce: String,
        agent_to_host_nonce: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fixture {
        session_id_hex: String,
        session_secret_hex: String,
        server_nonce_hex: String,
        client_nonce_hex: String,
        server_challenge_hex: String,
        client_proof_hex: String,
        host_to_agent_key_hex: String,
        agent_to_host_key_hex: String,
        host_to_agent_nonce_prefix_hex: String,
        agent_to_host_nonce_prefix_hex: String,
        protected_sequence: u64,
        protected_plaintext_hex: String,
        protected_frame_hex: String,
    }

    fn fixture_bytes<const N: usize>(start: u8) -> [u8; N] {
        std::array::from_fn(|index| start.wrapping_add(index as u8))
    }

    fn lower_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn decode_hex<const N: usize>(encoded: &str) -> [u8; N] {
        assert_eq!(encoded.len(), N * 2);
        let mut decoded = [0; N];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            let digit = |value: u8| match value {
                b'0'..=b'9' => value - b'0',
                b'a'..=b'f' => value - b'a' + 10,
                _ => panic!("fixture 必须使用小写十六进制"),
            };
            decoded[index] = (digit(pair[0]) << 4) | digit(pair[1]);
        }
        decoded
    }

    fn contract() -> Contract {
        serde_json::from_str(SHARED_CONTRACT).expect("secure-channel 共享契约必须有效")
    }

    #[test]
    fn shared_contract_and_cross_language_fixture_match() {
        let contract = contract();
        assert!(!contract.description.trim().is_empty());
        assert_eq!(contract.schema_version, 1);
        assert_eq!(contract.constants.channel_version, CHANNEL_VERSION);
        assert_eq!(
            contract.constants.server_challenge_type,
            SERVER_CHALLENGE_TYPE
        );
        assert_eq!(contract.constants.client_proof_type, CLIENT_PROOF_TYPE);
        assert_eq!(
            contract.constants.protected_frame_type,
            PROTECTED_FRAME_TYPE
        );
        assert_eq!(contract.constants.session_id_bytes, SESSION_ID_BYTES);
        assert_eq!(contract.constants.secret_bytes, SECRET_BYTES);
        assert_eq!(
            contract.constants.handshake_nonce_bytes,
            HANDSHAKE_NONCE_BYTES
        );
        assert_eq!(contract.constants.proof_bytes, PROOF_BYTES);
        assert_eq!(contract.constants.key_bytes, KEY_BYTES);
        assert_eq!(contract.constants.nonce_prefix_bytes, NONCE_PREFIX_BYTES);
        assert_eq!(contract.constants.aead_nonce_bytes, AEAD_NONCE_BYTES);
        assert_eq!(contract.constants.aead_tag_bytes, AEAD_TAG_BYTES);
        assert_eq!(
            contract.constants.server_challenge_bytes,
            SERVER_CHALLENGE_BYTES
        );
        assert_eq!(contract.constants.client_proof_bytes, CLIENT_PROOF_BYTES);
        assert_eq!(
            contract.constants.protected_header_bytes,
            PROTECTED_HEADER_BYTES
        );
        assert_eq!(
            contract.constants.protected_overhead_bytes,
            PROTECTED_OVERHEAD_BYTES
        );
        assert_eq!(contract.labels.server_proof.as_bytes(), SERVER_PROOF_LABEL);
        assert_eq!(contract.labels.client_proof.as_bytes(), CLIENT_PROOF_LABEL);
        assert_eq!(
            contract.labels.host_to_agent_key.as_bytes(),
            HOST_TO_AGENT_KEY_LABEL
        );
        assert_eq!(
            contract.labels.agent_to_host_key.as_bytes(),
            AGENT_TO_HOST_KEY_LABEL
        );
        assert_eq!(
            contract.labels.host_to_agent_nonce.as_bytes(),
            HOST_TO_AGENT_NONCE_LABEL
        );
        assert_eq!(
            contract.labels.agent_to_host_nonce.as_bytes(),
            AGENT_TO_HOST_NONCE_LABEL
        );

        let session_id = decode_hex::<SESSION_ID_BYTES>(&contract.fixture.session_id_hex);
        let secret = decode_hex::<SECRET_BYTES>(&contract.fixture.session_secret_hex);
        let server_nonce = decode_hex::<HANDSHAKE_NONCE_BYTES>(&contract.fixture.server_nonce_hex);
        let client_nonce = decode_hex::<HANDSHAKE_NONCE_BYTES>(&contract.fixture.client_nonce_hex);
        let challenge = create_server_challenge(&session_id, &secret, &server_nonce);
        assert_eq!(
            lower_hex(challenge.as_bytes()),
            contract.fixture.server_challenge_hex
        );
        let verified = verify_server_challenge(&session_id, &secret, challenge.as_bytes()).unwrap();
        let (proof, keys) = answer_server_challenge(&verified, &secret, &client_nonce);
        assert_eq!(
            lower_hex(proof.as_bytes()),
            contract.fixture.client_proof_hex
        );
        assert_eq!(
            lower_hex(keys.host_to_agent_key()),
            contract.fixture.host_to_agent_key_hex
        );
        assert_eq!(
            lower_hex(keys.agent_to_host_key()),
            contract.fixture.agent_to_host_key_hex
        );
        assert_eq!(
            lower_hex(keys.host_to_agent_nonce_prefix()),
            contract.fixture.host_to_agent_nonce_prefix_hex
        );
        assert_eq!(
            lower_hex(keys.agent_to_host_nonce_prefix()),
            contract.fixture.agent_to_host_nonce_prefix_hex
        );
        let mut sealer = FrameSealer::with_sequence(
            Zeroizing::new(*keys.host_to_agent_key()),
            *keys.host_to_agent_nonce_prefix(),
            contract.fixture.protected_sequence,
        );
        let plaintext = decode_hex::<22>(&contract.fixture.protected_plaintext_hex);
        let protected = sealer.seal(&plaintext, 1024).unwrap();
        assert_eq!(lower_hex(&protected), contract.fixture.protected_frame_hex);
    }

    #[test]
    fn proof_handshake_derives_matching_directional_keys() {
        let session_id = fixture_bytes::<SESSION_ID_BYTES>(0);
        let secret = fixture_bytes::<SECRET_BYTES>(0);
        let server_nonce = fixture_bytes::<HANDSHAKE_NONCE_BYTES>(0x20);
        let client_nonce = fixture_bytes::<HANDSHAKE_NONCE_BYTES>(0x40);
        let challenge = create_server_challenge(&session_id, &secret, &server_nonce);
        let verified = verify_server_challenge(&session_id, &secret, challenge.as_bytes()).unwrap();
        let (proof, client_keys) = answer_server_challenge(&verified, &secret, &client_nonce);
        let server_keys =
            verify_client_proof(&challenge, &session_id, &secret, proof.as_bytes()).unwrap();

        assert_eq!(
            client_keys.host_to_agent_key(),
            server_keys.host_to_agent_key()
        );
        assert_eq!(
            client_keys.agent_to_host_key(),
            server_keys.agent_to_host_key()
        );
        assert_ne!(
            client_keys.host_to_agent_key(),
            client_keys.agent_to_host_key()
        );
        assert_ne!(
            client_keys.host_to_agent_nonce_prefix(),
            client_keys.agent_to_host_nonce_prefix()
        );
    }

    #[test]
    fn ordered_frames_reject_tampering_replay_and_out_of_order_sequence() {
        let secret = fixture_bytes::<SECRET_BYTES>(0);
        let session_id = fixture_bytes::<SESSION_ID_BYTES>(0);
        let server_nonce = fixture_bytes::<HANDSHAKE_NONCE_BYTES>(0x20);
        let client_nonce = fixture_bytes::<HANDSHAKE_NONCE_BYTES>(0x40);
        let challenge = create_server_challenge(&session_id, &secret, &server_nonce);
        let verified = verify_server_challenge(&session_id, &secret, challenge.as_bytes()).unwrap();
        let (_, keys) = answer_server_challenge(&verified, &secret, &client_nonce);
        let mut client = keys.into_client_channel();

        let server_verified =
            verify_server_challenge(&session_id, &secret, challenge.as_bytes()).unwrap();
        let (proof, _) = answer_server_challenge(&server_verified, &secret, &client_nonce);
        let server_keys =
            verify_client_proof(&challenge, &session_id, &secret, proof.as_bytes()).unwrap();
        let mut server = server_keys.into_server_channel();

        let first = client.outbound.seal(b"first", 16).unwrap();
        assert_eq!(
            server.inbound.open(&first, 16).unwrap().as_slice(),
            b"first"
        );
        assert!(matches!(
            server.inbound.open(&first, 16),
            Err(SecureChannelError::SequenceMismatch { .. })
        ));

        let second = client.outbound.seal(b"second", 16).unwrap();
        let mut tampered = second.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            server.inbound.open(&tampered, 16).unwrap_err(),
            SecureChannelError::AuthenticationFailed
        );
        assert_eq!(
            server.inbound.open(&second, 16).unwrap().as_slice(),
            b"second"
        );
    }

    #[test]
    #[ignore = "AZLW_MEASURE_MODE=frame"]
    fn measure_large_frame_when_requested() {
        if std::env::var("AZLW_MEASURE_MODE").ok().as_deref() != Some("frame") {
            return;
        }
        use zeroize::Zeroizing;

        for plaintext_bytes in [64 * 1024_usize, 1024 * 1024, 4 * 1024 * 1024] {
            let mut body = String::with_capacity(plaintext_bytes + 32);
            body.push_str("{\"payload\":\"");
            while body.len() + 2 < plaintext_bytes {
                body.push(char::from(b'a' + (body.len() % 26) as u8));
            }
            body.push_str("\"}");
            let encode_started = std::time::Instant::now();
            let encoded = serde_json::to_vec(&serde_json::json!({ "payload": body })).unwrap();
            let encode_us = encode_started.elapsed().as_micros();
            let decode_started = std::time::Instant::now();
            let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
            let decode_us = decode_started.elapsed().as_micros();
            let copy_started = std::time::Instant::now();
            let copied = encoded.clone();
            let copy_us = copy_started.elapsed().as_micros();
            assert_eq!(copied, encoded);

            let key = Zeroizing::new([7_u8; KEY_BYTES]);
            let mut sealer = FrameSealer::new(key.clone(), [9_u8; NONCE_PREFIX_BYTES]);
            let seal_started = std::time::Instant::now();
            let frame = sealer.seal(&encoded, encoded.len()).unwrap();
            let seal_us = seal_started.elapsed().as_micros();
            let mut opener = FrameOpener::new(key, [9_u8; NONCE_PREFIX_BYTES]);
            let open_started = std::time::Instant::now();
            let opened = opener.open(&frame, encoded.len()).unwrap();
            let open_us = open_started.elapsed().as_micros();
            assert_eq!(opened.as_slice(), encoded.as_slice());
            assert!(decoded.get("payload").is_some());
            println!(
                "\nMEASURE stage=protected_frame plaintext_bytes={} encoded_bytes={} frame_bytes={} json_encode_us={encode_us} json_decode_us={decode_us} copy_us={copy_us} seal_us={seal_us} open_us={open_us}",
                body.len(),
                encoded.len(),
                frame.len()
            );
        }
    }

    #[test]
    fn contract_frame_limits_round_trip_without_changing_the_cipher() {
        use zeroize::Zeroizing;

        const REQUEST_MAX: usize = 65_536;
        const RESPONSE_MAX: usize = 16_777_216;
        let key = Zeroizing::new([7_u8; KEY_BYTES]);
        let nonce = [9_u8; NONCE_PREFIX_BYTES];
        for (limit, size) in [
            (REQUEST_MAX, 0),
            (REQUEST_MAX, 1),
            (REQUEST_MAX, REQUEST_MAX),
            (RESPONSE_MAX, RESPONSE_MAX),
        ] {
            let plaintext = vec![b'a'; size];
            let copy_started = std::time::Instant::now();
            let copied = plaintext.clone();
            let copy_us = copy_started.elapsed().as_micros();
            assert_eq!(copied, plaintext);
            let mut sealer = FrameSealer::new(key.clone(), nonce);
            let seal_started = std::time::Instant::now();
            let frame = sealer.seal(&plaintext, limit).unwrap();
            let seal_us = seal_started.elapsed().as_micros();
            let mut opener = FrameOpener::new(key.clone(), nonce);
            let open_started = std::time::Instant::now();
            let opened = opener.open(&frame, limit).unwrap();
            let open_us = open_started.elapsed().as_micros();
            assert_eq!(opened.as_slice(), plaintext.as_slice());
            assert_eq!(frame.len(), plaintext.len() + PROTECTED_OVERHEAD_BYTES);
            println!(
                "MEASURE stage=contract_frame limit={limit} plaintext_bytes={size} frame_bytes={} copy_us={copy_us} seal_us={seal_us} open_us={open_us}",
                frame.len()
            );
        }
        let mut sealer = FrameSealer::new(key, nonce);
        let too_large = vec![0_u8; REQUEST_MAX + 1];
        assert!(matches!(
            sealer.seal(&too_large, REQUEST_MAX),
            Err(SecureChannelError::PlaintextTooLarge { .. })
        ));
    }
}

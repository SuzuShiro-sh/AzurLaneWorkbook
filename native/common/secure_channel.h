// 声明挑战证明、方向密钥派生和有序 XChaCha20-Poly1305 帧。

#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <span>
#include <vector>

#include "secure_channel_contract.h"

namespace azlw::secure_channel {

using SessionId = std::array<std::uint8_t, secure_channel_contract::kSessionIdBytes>;
using SessionSecret = std::array<std::uint8_t, secure_channel_contract::kSecretBytes>;
using HandshakeNonce = std::array<std::uint8_t, secure_channel_contract::kHandshakeNonceBytes>;
using Key = std::array<std::uint8_t, secure_channel_contract::kKeyBytes>;
using NoncePrefix = std::array<std::uint8_t, secure_channel_contract::kNoncePrefixBytes>;
using ServerChallengeBytes =
    std::array<std::uint8_t, secure_channel_contract::kServerChallengeBytes>;
using ClientProofBytes = std::array<std::uint8_t, secure_channel_contract::kClientProofBytes>;

enum class Error : std::uint8_t {
    Ok = 0,
    ServerChallengeLengthInvalid,
    ServerChallengeHeaderInvalid,
    SessionMismatch,
    ProofInvalid,
    ClientProofLengthInvalid,
    ClientProofHeaderInvalid,
    PlaintextTooLarge,
    SequenceExhausted,
    ProtectedFrameTooShort,
    ProtectedFrameHeaderInvalid,
    SequenceMismatch,
    AuthenticationFailed,
};

class ServerChallenge final {
public:
    ServerChallenge() = default;
    ~ServerChallenge();
    ServerChallenge(const ServerChallenge&) = delete;
    ServerChallenge& operator=(const ServerChallenge&) = delete;
    ServerChallenge(ServerChallenge&& other) noexcept;
    ServerChallenge& operator=(ServerChallenge&& other) noexcept;

    std::span<const std::uint8_t> bytes() const noexcept;

private:
    friend Error create_server_challenge(
        const SessionId&, const SessionSecret&, const HandshakeNonce&, ServerChallenge*) noexcept;
    friend Error verify_client_proof(
        const ServerChallenge&,
        const SessionId&,
        const SessionSecret&,
        std::span<const std::uint8_t>,
        class ChannelKeyMaterial*) noexcept;

    ServerChallengeBytes bytes_{};
};

class VerifiedServerChallenge final {
public:
    VerifiedServerChallenge() = default;
    ~VerifiedServerChallenge();
    VerifiedServerChallenge(const VerifiedServerChallenge&) = delete;
    VerifiedServerChallenge& operator=(const VerifiedServerChallenge&) = delete;
    VerifiedServerChallenge(VerifiedServerChallenge&& other) noexcept;
    VerifiedServerChallenge& operator=(VerifiedServerChallenge&& other) noexcept;

    std::span<const std::uint8_t> bytes() const noexcept;

private:
    friend Error verify_server_challenge(
        const SessionId&,
        const SessionSecret&,
        std::span<const std::uint8_t>,
        VerifiedServerChallenge*) noexcept;
    friend Error answer_server_challenge(
        const VerifiedServerChallenge&,
        const SessionSecret&,
        const HandshakeNonce&,
        ClientProofBytes*,
        class ChannelKeyMaterial*) noexcept;

    ServerChallengeBytes bytes_{};
    SessionId session_id_{};
    HandshakeNonce server_nonce_{};
};

class ChannelKeyMaterial final {
public:
    ChannelKeyMaterial() = default;
    ~ChannelKeyMaterial();
    ChannelKeyMaterial(const ChannelKeyMaterial&) = delete;
    ChannelKeyMaterial& operator=(const ChannelKeyMaterial&) = delete;
    ChannelKeyMaterial(ChannelKeyMaterial&& other) noexcept;
    ChannelKeyMaterial& operator=(ChannelKeyMaterial&& other) noexcept;

    const Key& host_to_agent_key() const noexcept;
    const Key& agent_to_host_key() const noexcept;
    const NoncePrefix& host_to_agent_nonce_prefix() const noexcept;
    const NoncePrefix& agent_to_host_nonce_prefix() const noexcept;

private:
    friend Error answer_server_challenge(
        const VerifiedServerChallenge&,
        const SessionSecret&,
        const HandshakeNonce&,
        ClientProofBytes*,
        ChannelKeyMaterial*) noexcept;
    friend Error verify_client_proof(
        const ServerChallenge&,
        const SessionId&,
        const SessionSecret&,
        std::span<const std::uint8_t>,
        ChannelKeyMaterial*) noexcept;

    Key host_to_agent_key_{};
    Key agent_to_host_key_{};
    NoncePrefix host_to_agent_nonce_prefix_{};
    NoncePrefix agent_to_host_nonce_prefix_{};
};

class FrameSealer final {
public:
    FrameSealer(const Key& key, const NoncePrefix& nonce_prefix, std::uint64_t next_sequence = 0);
    ~FrameSealer();
    FrameSealer(const FrameSealer&) = delete;
    FrameSealer& operator=(const FrameSealer&) = delete;

    Error seal(
        std::span<const std::uint8_t> plaintext,
        std::size_t maximum_plaintext_bytes,
        std::vector<std::uint8_t>* frame) noexcept;

private:
    Key key_{};
    NoncePrefix nonce_prefix_{};
    std::uint64_t next_sequence_ = 0;
};

class FrameOpener final {
public:
    FrameOpener(const Key& key, const NoncePrefix& nonce_prefix, std::uint64_t next_sequence = 0);
    ~FrameOpener();
    FrameOpener(const FrameOpener&) = delete;
    FrameOpener& operator=(const FrameOpener&) = delete;

    Error open(
        std::span<const std::uint8_t> frame,
        std::size_t maximum_plaintext_bytes,
        std::vector<std::uint8_t>* plaintext) noexcept;

private:
    Key key_{};
    NoncePrefix nonce_prefix_{};
    std::uint64_t next_sequence_ = 0;
};

/// 端点角色决定双向密钥与发送、接收方向的唯一映射。
enum class EndpointRole : std::uint8_t {
    Client,
    Server,
};

/// 一条连接的独立双向认证加密状态。
class SecureChannel final {
public:
    SecureChannel(const ChannelKeyMaterial& keys, EndpointRole role);
    SecureChannel(const SecureChannel&) = delete;
    SecureChannel& operator=(const SecureChannel&) = delete;

    FrameSealer& outbound() noexcept;
    FrameOpener& inbound() noexcept;

private:
    FrameSealer outbound_;
    FrameOpener inbound_;
};

Error create_server_challenge(
    const SessionId& session_id,
    const SessionSecret& session_secret,
    const HandshakeNonce& server_nonce,
    ServerChallenge* challenge) noexcept;

Error verify_server_challenge(
    const SessionId& expected_session_id,
    const SessionSecret& session_secret,
    std::span<const std::uint8_t> frame,
    VerifiedServerChallenge* challenge) noexcept;

Error answer_server_challenge(
    const VerifiedServerChallenge& challenge,
    const SessionSecret& session_secret,
    const HandshakeNonce& client_nonce,
    ClientProofBytes* proof,
    ChannelKeyMaterial* keys) noexcept;

Error verify_client_proof(
    const ServerChallenge& challenge,
    const SessionId& session_id,
    const SessionSecret& session_secret,
    std::span<const std::uint8_t> frame,
    ChannelKeyMaterial* keys) noexcept;

}  // namespace azlw::secure_channel

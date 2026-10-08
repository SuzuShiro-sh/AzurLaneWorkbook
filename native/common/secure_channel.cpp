// 实现与 Rust secure-channel crate 逐字节互通的密码学协议封装。

#include "secure_channel.h"

#include <algorithm>
#include <array>
#include <cstring>
#include <initializer_list>
#include <limits>
#include <string_view>

#include <monocypher.h>

namespace azlw::secure_channel {
namespace {

constexpr std::size_t kChallengeSessionOffset = 2;
constexpr std::size_t kChallengeNonceOffset =
    kChallengeSessionOffset + secure_channel_contract::kSessionIdBytes;
constexpr std::size_t kChallengeProofOffset =
    kChallengeNonceOffset + secure_channel_contract::kHandshakeNonceBytes;
constexpr std::size_t kClientNonceOffset = 2;
constexpr std::size_t kClientProofOffset =
    kClientNonceOffset + secure_channel_contract::kHandshakeNonceBytes;

static_assert(
    kChallengeProofOffset + secure_channel_contract::kProofBytes ==
    secure_channel_contract::kServerChallengeBytes);
static_assert(
    kClientProofOffset + secure_channel_contract::kProofBytes ==
    secure_channel_contract::kClientProofBytes);
static_assert(
    secure_channel_contract::kNoncePrefixBytes + sizeof(std::uint64_t) ==
    secure_channel_contract::kAeadNonceBytes);

template <std::size_t OutputSize>
std::array<std::uint8_t, OutputSize> keyed_blake2b(
    const SessionSecret& key,
    std::string_view label,
    std::initializer_list<std::span<const std::uint8_t>> parts) noexcept {
    crypto_blake2b_ctx context{};
    crypto_blake2b_keyed_init(&context, OutputSize, key.data(), key.size());
    crypto_blake2b_update(
        &context,
        reinterpret_cast<const std::uint8_t*>(label.data()),
        label.size());
    for (const std::span<const std::uint8_t> part : parts) {
        crypto_blake2b_update(&context, part.data(), part.size());
    }
    std::array<std::uint8_t, OutputSize> output{};
    crypto_blake2b_final(&context, output.data());
    return output;
}

bool proof_matches(
    const SessionSecret& key,
    std::string_view label,
    std::initializer_list<std::span<const std::uint8_t>> parts,
    std::span<const std::uint8_t> proof) noexcept {
    if (proof.size() != secure_channel_contract::kProofBytes) {
        return false;
    }
    auto expected = keyed_blake2b<secure_channel_contract::kProofBytes>(key, label, parts);
    const int result = crypto_verify32(expected.data(), proof.data());
    crypto_wipe(expected.data(), expected.size());
    return result == 0;
}

void derive_keys(
    const SessionSecret& secret,
    const SessionId& session_id,
    const HandshakeNonce& server_nonce,
    const HandshakeNonce& client_nonce,
    Key* host_to_agent_key,
    Key* agent_to_host_key,
    NoncePrefix* host_to_agent_nonce_prefix,
    NoncePrefix* agent_to_host_nonce_prefix) noexcept {
    *host_to_agent_key = keyed_blake2b<secure_channel_contract::kKeyBytes>(
        secret,
        secure_channel_contract::kHostToAgentKeyLabel,
        {session_id, server_nonce, client_nonce});
    *agent_to_host_key = keyed_blake2b<secure_channel_contract::kKeyBytes>(
        secret,
        secure_channel_contract::kAgentToHostKeyLabel,
        {session_id, server_nonce, client_nonce});
    *host_to_agent_nonce_prefix =
        keyed_blake2b<secure_channel_contract::kNoncePrefixBytes>(
            secret,
            secure_channel_contract::kHostToAgentNonceLabel,
            {session_id, server_nonce, client_nonce});
    *agent_to_host_nonce_prefix =
        keyed_blake2b<secure_channel_contract::kNoncePrefixBytes>(
            secret,
            secure_channel_contract::kAgentToHostNonceLabel,
            {session_id, server_nonce, client_nonce});
}

std::array<std::uint8_t, secure_channel_contract::kAeadNonceBytes> make_nonce(
    const NoncePrefix& prefix,
    std::uint64_t sequence) noexcept {
    std::array<std::uint8_t, secure_channel_contract::kAeadNonceBytes> nonce{};
    std::copy(prefix.begin(), prefix.end(), nonce.begin());
    for (std::size_t index = 0; index < sizeof(sequence); ++index) {
        nonce[secure_channel_contract::kNoncePrefixBytes + index] =
            static_cast<std::uint8_t>(sequence >> ((sizeof(sequence) - index - 1) * 8));
    }
    return nonce;
}

std::uint64_t read_sequence(std::span<const std::uint8_t> frame) noexcept {
    std::uint64_t sequence = 0;
    for (std::size_t index = 2; index < secure_channel_contract::kProtectedHeaderBytes; ++index) {
        sequence = (sequence << 8) | frame[index];
    }
    return sequence;
}

void write_sequence(std::uint64_t sequence, std::vector<std::uint8_t>* frame) {
    for (std::size_t index = 0; index < sizeof(sequence); ++index) {
        (*frame)[2 + index] =
            static_cast<std::uint8_t>(sequence >> ((sizeof(sequence) - index - 1) * 8));
    }
}

template <typename Array>
void move_secret(Array* destination, Array* source) noexcept {
    *destination = *source;
    crypto_wipe(source->data(), source->size());
}

}  // namespace

ServerChallenge::~ServerChallenge() {
    crypto_wipe(bytes_.data(), bytes_.size());
}

ServerChallenge::ServerChallenge(ServerChallenge&& other) noexcept {
    move_secret(&bytes_, &other.bytes_);
}

ServerChallenge& ServerChallenge::operator=(ServerChallenge&& other) noexcept {
    if (this != &other) {
        crypto_wipe(bytes_.data(), bytes_.size());
        move_secret(&bytes_, &other.bytes_);
    }
    return *this;
}

std::span<const std::uint8_t> ServerChallenge::bytes() const noexcept {
    return bytes_;
}

VerifiedServerChallenge::~VerifiedServerChallenge() {
    crypto_wipe(bytes_.data(), bytes_.size());
    crypto_wipe(session_id_.data(), session_id_.size());
    crypto_wipe(server_nonce_.data(), server_nonce_.size());
}

VerifiedServerChallenge::VerifiedServerChallenge(VerifiedServerChallenge&& other) noexcept {
    move_secret(&bytes_, &other.bytes_);
    move_secret(&session_id_, &other.session_id_);
    move_secret(&server_nonce_, &other.server_nonce_);
}

VerifiedServerChallenge& VerifiedServerChallenge::operator=(
    VerifiedServerChallenge&& other) noexcept {
    if (this != &other) {
        crypto_wipe(bytes_.data(), bytes_.size());
        crypto_wipe(session_id_.data(), session_id_.size());
        crypto_wipe(server_nonce_.data(), server_nonce_.size());
        move_secret(&bytes_, &other.bytes_);
        move_secret(&session_id_, &other.session_id_);
        move_secret(&server_nonce_, &other.server_nonce_);
    }
    return *this;
}

std::span<const std::uint8_t> VerifiedServerChallenge::bytes() const noexcept {
    return bytes_;
}

ChannelKeyMaterial::~ChannelKeyMaterial() {
    crypto_wipe(host_to_agent_key_.data(), host_to_agent_key_.size());
    crypto_wipe(agent_to_host_key_.data(), agent_to_host_key_.size());
    crypto_wipe(host_to_agent_nonce_prefix_.data(), host_to_agent_nonce_prefix_.size());
    crypto_wipe(agent_to_host_nonce_prefix_.data(), agent_to_host_nonce_prefix_.size());
}

ChannelKeyMaterial::ChannelKeyMaterial(ChannelKeyMaterial&& other) noexcept {
    move_secret(&host_to_agent_key_, &other.host_to_agent_key_);
    move_secret(&agent_to_host_key_, &other.agent_to_host_key_);
    move_secret(&host_to_agent_nonce_prefix_, &other.host_to_agent_nonce_prefix_);
    move_secret(&agent_to_host_nonce_prefix_, &other.agent_to_host_nonce_prefix_);
}

ChannelKeyMaterial& ChannelKeyMaterial::operator=(ChannelKeyMaterial&& other) noexcept {
    if (this != &other) {
        crypto_wipe(host_to_agent_key_.data(), host_to_agent_key_.size());
        crypto_wipe(agent_to_host_key_.data(), agent_to_host_key_.size());
        crypto_wipe(host_to_agent_nonce_prefix_.data(), host_to_agent_nonce_prefix_.size());
        crypto_wipe(agent_to_host_nonce_prefix_.data(), agent_to_host_nonce_prefix_.size());
        move_secret(&host_to_agent_key_, &other.host_to_agent_key_);
        move_secret(&agent_to_host_key_, &other.agent_to_host_key_);
        move_secret(&host_to_agent_nonce_prefix_, &other.host_to_agent_nonce_prefix_);
        move_secret(&agent_to_host_nonce_prefix_, &other.agent_to_host_nonce_prefix_);
    }
    return *this;
}

const Key& ChannelKeyMaterial::host_to_agent_key() const noexcept {
    return host_to_agent_key_;
}

const Key& ChannelKeyMaterial::agent_to_host_key() const noexcept {
    return agent_to_host_key_;
}

const NoncePrefix& ChannelKeyMaterial::host_to_agent_nonce_prefix() const noexcept {
    return host_to_agent_nonce_prefix_;
}

const NoncePrefix& ChannelKeyMaterial::agent_to_host_nonce_prefix() const noexcept {
    return agent_to_host_nonce_prefix_;
}

FrameSealer::FrameSealer(
    const Key& key,
    const NoncePrefix& nonce_prefix,
    std::uint64_t next_sequence)
    : key_(key), nonce_prefix_(nonce_prefix), next_sequence_(next_sequence) {}

FrameSealer::~FrameSealer() {
    crypto_wipe(key_.data(), key_.size());
    crypto_wipe(nonce_prefix_.data(), nonce_prefix_.size());
}

Error FrameSealer::seal(
    std::span<const std::uint8_t> plaintext,
    std::size_t maximum_plaintext_bytes,
    std::vector<std::uint8_t>* frame) noexcept {
    frame->clear();
    if (plaintext.size() > maximum_plaintext_bytes) {
        return Error::PlaintextTooLarge;
    }
    if (next_sequence_ == std::numeric_limits<std::uint64_t>::max()) {
        return Error::SequenceExhausted;
    }
    try {
        frame->assign(
            secure_channel_contract::kProtectedOverheadBytes + plaintext.size(), 0);
    } catch (...) {
        return Error::PlaintextTooLarge;
    }
    (*frame)[0] = secure_channel_contract::kChannelVersion;
    (*frame)[1] = secure_channel_contract::kProtectedFrameType;
    write_sequence(next_sequence_, frame);
    std::copy(
        plaintext.begin(),
        plaintext.end(),
        frame->begin() + static_cast<std::ptrdiff_t>(secure_channel_contract::kProtectedHeaderBytes));
    const auto nonce = make_nonce(nonce_prefix_, next_sequence_);
    std::uint8_t* ciphertext = frame->data() + secure_channel_contract::kProtectedHeaderBytes;
    std::uint8_t* tag = frame->data() + frame->size() - secure_channel_contract::kAeadTagBytes;
    crypto_aead_lock(
        ciphertext,
        tag,
        key_.data(),
        nonce.data(),
        frame->data(),
        secure_channel_contract::kProtectedHeaderBytes,
        ciphertext,
        plaintext.size());
    ++next_sequence_;
    return Error::Ok;
}

FrameOpener::FrameOpener(
    const Key& key,
    const NoncePrefix& nonce_prefix,
    std::uint64_t next_sequence)
    : key_(key), nonce_prefix_(nonce_prefix), next_sequence_(next_sequence) {}

FrameOpener::~FrameOpener() {
    crypto_wipe(key_.data(), key_.size());
    crypto_wipe(nonce_prefix_.data(), nonce_prefix_.size());
}

SecureChannel::SecureChannel(const ChannelKeyMaterial& keys, EndpointRole role)
    : outbound_(
          role == EndpointRole::Client ? keys.host_to_agent_key() : keys.agent_to_host_key(),
          role == EndpointRole::Client ? keys.host_to_agent_nonce_prefix()
                                       : keys.agent_to_host_nonce_prefix()),
      inbound_(
          role == EndpointRole::Client ? keys.agent_to_host_key() : keys.host_to_agent_key(),
          role == EndpointRole::Client ? keys.agent_to_host_nonce_prefix()
                                       : keys.host_to_agent_nonce_prefix()) {}

FrameSealer& SecureChannel::outbound() noexcept {
    return outbound_;
}

FrameOpener& SecureChannel::inbound() noexcept {
    return inbound_;
}

Error FrameOpener::open(
    std::span<const std::uint8_t> frame,
    std::size_t maximum_plaintext_bytes,
    std::vector<std::uint8_t>* plaintext) noexcept {
    crypto_wipe(plaintext->data(), plaintext->size());
    plaintext->clear();
    if (frame.size() < secure_channel_contract::kProtectedOverheadBytes) {
        return Error::ProtectedFrameTooShort;
    }
    const std::size_t plaintext_size =
        frame.size() - secure_channel_contract::kProtectedOverheadBytes;
    if (plaintext_size > maximum_plaintext_bytes) {
        return Error::PlaintextTooLarge;
    }
    if (frame[0] != secure_channel_contract::kChannelVersion ||
        frame[1] != secure_channel_contract::kProtectedFrameType) {
        return Error::ProtectedFrameHeaderInvalid;
    }
    const std::uint64_t sequence = read_sequence(frame);
    if (sequence != next_sequence_) {
        return Error::SequenceMismatch;
    }
    if (next_sequence_ == std::numeric_limits<std::uint64_t>::max()) {
        return Error::SequenceExhausted;
    }
    try {
        plaintext->assign(
            frame.begin() + static_cast<std::ptrdiff_t>(secure_channel_contract::kProtectedHeaderBytes),
            frame.end() - static_cast<std::ptrdiff_t>(secure_channel_contract::kAeadTagBytes));
    } catch (...) {
        return Error::PlaintextTooLarge;
    }
    const auto nonce = make_nonce(nonce_prefix_, sequence);
    const std::uint8_t* tag = frame.data() + frame.size() - secure_channel_contract::kAeadTagBytes;
    const int result = crypto_aead_unlock(
        plaintext->data(),
        tag,
        key_.data(),
        nonce.data(),
        frame.data(),
        secure_channel_contract::kProtectedHeaderBytes,
        plaintext->data(),
        plaintext->size());
    if (result != 0) {
        crypto_wipe(plaintext->data(), plaintext->size());
        plaintext->clear();
        return Error::AuthenticationFailed;
    }
    ++next_sequence_;
    return Error::Ok;
}

Error create_server_challenge(
    const SessionId& session_id,
    const SessionSecret& session_secret,
    const HandshakeNonce& server_nonce,
    ServerChallenge* challenge) noexcept {
    challenge->bytes_.fill(0);
    challenge->bytes_[0] = secure_channel_contract::kChannelVersion;
    challenge->bytes_[1] = secure_channel_contract::kServerChallengeType;
    std::copy(session_id.begin(), session_id.end(), challenge->bytes_.begin() + kChallengeSessionOffset);
    std::copy(server_nonce.begin(), server_nonce.end(), challenge->bytes_.begin() + kChallengeNonceOffset);
    auto proof = keyed_blake2b<secure_channel_contract::kProofBytes>(
        session_secret,
        secure_channel_contract::kServerProofLabel,
        {session_id, server_nonce});
    std::copy(proof.begin(), proof.end(), challenge->bytes_.begin() + kChallengeProofOffset);
    crypto_wipe(proof.data(), proof.size());
    return Error::Ok;
}

Error verify_server_challenge(
    const SessionId& expected_session_id,
    const SessionSecret& session_secret,
    std::span<const std::uint8_t> frame,
    VerifiedServerChallenge* challenge) noexcept {
    crypto_wipe(challenge->bytes_.data(), challenge->bytes_.size());
    crypto_wipe(challenge->session_id_.data(), challenge->session_id_.size());
    crypto_wipe(challenge->server_nonce_.data(), challenge->server_nonce_.size());
    if (frame.size() != secure_channel_contract::kServerChallengeBytes) {
        return Error::ServerChallengeLengthInvalid;
    }
    if (frame[0] != secure_channel_contract::kChannelVersion ||
        frame[1] != secure_channel_contract::kServerChallengeType) {
        return Error::ServerChallengeHeaderInvalid;
    }
    if (!std::equal(
            expected_session_id.begin(),
            expected_session_id.end(),
            frame.begin() + kChallengeSessionOffset)) {
        return Error::SessionMismatch;
    }
    HandshakeNonce server_nonce{};
    std::copy_n(frame.begin() + kChallengeNonceOffset, server_nonce.size(), server_nonce.begin());
    if (!proof_matches(
            session_secret,
            secure_channel_contract::kServerProofLabel,
            {expected_session_id, server_nonce},
            frame.subspan(kChallengeProofOffset))) {
        crypto_wipe(server_nonce.data(), server_nonce.size());
        return Error::ProofInvalid;
    }
    std::copy(frame.begin(), frame.end(), challenge->bytes_.begin());
    challenge->session_id_ = expected_session_id;
    challenge->server_nonce_ = server_nonce;
    crypto_wipe(server_nonce.data(), server_nonce.size());
    return Error::Ok;
}

Error answer_server_challenge(
    const VerifiedServerChallenge& challenge,
    const SessionSecret& session_secret,
    const HandshakeNonce& client_nonce,
    ClientProofBytes* proof,
    ChannelKeyMaterial* keys) noexcept {
    proof->fill(0);
    (*proof)[0] = secure_channel_contract::kChannelVersion;
    (*proof)[1] = secure_channel_contract::kClientProofType;
    std::copy(client_nonce.begin(), client_nonce.end(), proof->begin() + kClientNonceOffset);
    auto proof_bytes = keyed_blake2b<secure_channel_contract::kProofBytes>(
        session_secret,
        secure_channel_contract::kClientProofLabel,
        {challenge.bytes_, client_nonce});
    std::copy(proof_bytes.begin(), proof_bytes.end(), proof->begin() + kClientProofOffset);
    crypto_wipe(proof_bytes.data(), proof_bytes.size());
    derive_keys(
        session_secret,
        challenge.session_id_,
        challenge.server_nonce_,
        client_nonce,
        &keys->host_to_agent_key_,
        &keys->agent_to_host_key_,
        &keys->host_to_agent_nonce_prefix_,
        &keys->agent_to_host_nonce_prefix_);
    return Error::Ok;
}

Error verify_client_proof(
    const ServerChallenge& challenge,
    const SessionId& session_id,
    const SessionSecret& session_secret,
    std::span<const std::uint8_t> frame,
    ChannelKeyMaterial* keys) noexcept {
    crypto_wipe(keys->host_to_agent_key_.data(), keys->host_to_agent_key_.size());
    crypto_wipe(keys->agent_to_host_key_.data(), keys->agent_to_host_key_.size());
    crypto_wipe(
        keys->host_to_agent_nonce_prefix_.data(),
        keys->host_to_agent_nonce_prefix_.size());
    crypto_wipe(
        keys->agent_to_host_nonce_prefix_.data(),
        keys->agent_to_host_nonce_prefix_.size());
    if (frame.size() != secure_channel_contract::kClientProofBytes) {
        return Error::ClientProofLengthInvalid;
    }
    if (frame[0] != secure_channel_contract::kChannelVersion ||
        frame[1] != secure_channel_contract::kClientProofType) {
        return Error::ClientProofHeaderInvalid;
    }
    HandshakeNonce client_nonce{};
    std::copy_n(frame.begin() + kClientNonceOffset, client_nonce.size(), client_nonce.begin());
    if (!proof_matches(
            session_secret,
            secure_channel_contract::kClientProofLabel,
            {challenge.bytes_, client_nonce},
            frame.subspan(kClientProofOffset))) {
        crypto_wipe(client_nonce.data(), client_nonce.size());
        return Error::ProofInvalid;
    }
    HandshakeNonce server_nonce{};
    std::copy_n(
        challenge.bytes_.begin() + kChallengeNonceOffset,
        server_nonce.size(),
        server_nonce.begin());
    derive_keys(
        session_secret,
        session_id,
        server_nonce,
        client_nonce,
        &keys->host_to_agent_key_,
        &keys->agent_to_host_key_,
        &keys->host_to_agent_nonce_prefix_,
        &keys->agent_to_host_nonce_prefix_);
    crypto_wipe(server_nonce.data(), server_nonce.size());
    crypto_wipe(client_nonce.data(), client_nonce.size());
    return Error::Ok;
}

}  // namespace azlw::secure_channel

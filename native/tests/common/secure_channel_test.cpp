// 验证 Native secure-channel 与 Rust 共享向量、篡改和序号契约互通。

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "secure_channel.h"
#include "secure_channel_contract_data.h"

namespace {

void require(bool condition, const char* message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << std::endl;
        std::exit(1);
    }
}

std::uint8_t nibble(char value) {
    if (value >= '0' && value <= '9') {
        return static_cast<std::uint8_t>(value - '0');
    }
    if (value >= 'a' && value <= 'f') {
        return static_cast<std::uint8_t>(value - 'a' + 10);
    }
    require(false, "fixture must use lower hex");
    return 0;
}

template <std::size_t Size>
std::array<std::uint8_t, Size> decode_array(std::string_view encoded) {
    require(encoded.size() == Size * 2, "fixture hex length mismatch");
    std::array<std::uint8_t, Size> decoded{};
    for (std::size_t index = 0; index < Size; ++index) {
        decoded[index] = static_cast<std::uint8_t>(
            (nibble(encoded[index * 2]) << 4) | nibble(encoded[index * 2 + 1]));
    }
    return decoded;
}

std::vector<std::uint8_t> decode_vector(std::string_view encoded) {
    require(encoded.size() % 2 == 0, "fixture vector hex length mismatch");
    std::vector<std::uint8_t> decoded(encoded.size() / 2);
    for (std::size_t index = 0; index < decoded.size(); ++index) {
        decoded[index] = static_cast<std::uint8_t>(
            (nibble(encoded[index * 2]) << 4) | nibble(encoded[index * 2 + 1]));
    }
    return decoded;
}

}  // namespace

int main() {
    using namespace azlw::secure_channel;
    namespace fixture = azlw::secure_channel_test_contract;

    const SessionId session_id =
        decode_array<azlw::secure_channel_contract::kSessionIdBytes>(fixture::kSessionIdHex);
    const SessionSecret secret =
        decode_array<azlw::secure_channel_contract::kSecretBytes>(fixture::kSessionSecretHex);
    const HandshakeNonce server_nonce =
        decode_array<azlw::secure_channel_contract::kHandshakeNonceBytes>(fixture::kServerNonceHex);
    const HandshakeNonce client_nonce =
        decode_array<azlw::secure_channel_contract::kHandshakeNonceBytes>(fixture::kClientNonceHex);
    const auto expected_challenge =
        decode_array<azlw::secure_channel_contract::kServerChallengeBytes>(
            fixture::kServerChallengeHex);
    const auto expected_proof =
        decode_array<azlw::secure_channel_contract::kClientProofBytes>(fixture::kClientProofHex);

    ServerChallenge challenge;
    require(
        create_server_challenge(session_id, secret, server_nonce, &challenge) == Error::Ok,
        "native server challenge creation failed");
    require(
        std::equal(challenge.bytes().begin(), challenge.bytes().end(), expected_challenge.begin()),
        "native server challenge differs from Rust fixture");

    VerifiedServerChallenge verified;
    require(
        verify_server_challenge(session_id, secret, challenge.bytes(), &verified) == Error::Ok,
        "native server challenge verification failed");
    ClientProofBytes proof{};
    ChannelKeyMaterial client_keys;
    require(
        answer_server_challenge(verified, secret, client_nonce, &proof, &client_keys) == Error::Ok,
        "native client proof creation failed");
    require(proof == expected_proof, "native client proof differs from Rust fixture");
    require(
        client_keys.host_to_agent_key() ==
            decode_array<azlw::secure_channel_contract::kKeyBytes>(
                fixture::kHostToAgentKeyHex),
        "native host-to-agent key differs from Rust fixture");
    require(
        client_keys.agent_to_host_key() ==
            decode_array<azlw::secure_channel_contract::kKeyBytes>(
                fixture::kAgentToHostKeyHex),
        "native agent-to-host key differs from Rust fixture");
    require(
        client_keys.host_to_agent_nonce_prefix() ==
            decode_array<azlw::secure_channel_contract::kNoncePrefixBytes>(
                fixture::kHostToAgentNoncePrefixHex),
        "native host-to-agent nonce differs from Rust fixture");
    require(
        client_keys.agent_to_host_nonce_prefix() ==
            decode_array<azlw::secure_channel_contract::kNoncePrefixBytes>(
                fixture::kAgentToHostNoncePrefixHex),
        "native agent-to-host nonce differs from Rust fixture");

    ChannelKeyMaterial server_keys;
    require(
        verify_client_proof(challenge, session_id, secret, proof, &server_keys) == Error::Ok,
        "native client proof verification failed");
    require(
        server_keys.host_to_agent_key() == client_keys.host_to_agent_key(),
        "native proof sides derive different keys");

    SecureChannel client_channel(client_keys, EndpointRole::Client);
    SecureChannel server_channel(server_keys, EndpointRole::Server);
    const std::vector<std::uint8_t> directional_request = {'r', 'e', 'q'};
    std::vector<std::uint8_t> directional_frame;
    require(
        client_channel.outbound().seal(
            directional_request,
            1024,
            &directional_frame) == Error::Ok,
        "native client direction seal failed");
    std::vector<std::uint8_t> directional_opened;
    require(
        server_channel.inbound().open(
            directional_frame,
            1024,
            &directional_opened) == Error::Ok &&
            directional_opened == directional_request,
        "native server direction open failed");
    const std::vector<std::uint8_t> directional_response = {'r', 'e', 's'};
    require(
        server_channel.outbound().seal(
            directional_response,
            1024,
            &directional_frame) == Error::Ok,
        "native server direction seal failed");
    require(
        client_channel.inbound().open(
            directional_frame,
            1024,
            &directional_opened) == Error::Ok &&
            directional_opened == directional_response,
        "native client direction open failed");

    const std::vector<std::uint8_t> plaintext = decode_vector(fixture::kProtectedPlaintextHex);
    const std::vector<std::uint8_t> expected_frame = decode_vector(fixture::kProtectedFrameHex);
    FrameSealer sealer(
        client_keys.host_to_agent_key(),
        client_keys.host_to_agent_nonce_prefix(),
        fixture::kProtectedSequence);
    std::vector<std::uint8_t> frame;
    require(sealer.seal(plaintext, 1024, &frame) == Error::Ok, "native frame seal failed");
    require(frame == expected_frame, "native protected frame differs from Rust fixture");

    FrameOpener opener(
        server_keys.host_to_agent_key(),
        server_keys.host_to_agent_nonce_prefix(),
        fixture::kProtectedSequence);
    std::vector<std::uint8_t> opened;
    std::vector<std::uint8_t> tampered = frame;
    tampered.back() ^= 1;
    require(
        opener.open(tampered, 1024, &opened) == Error::AuthenticationFailed && opened.empty(),
        "native opener accepted a modified tag");
    require(opener.open(frame, 1024, &opened) == Error::Ok, "native opener rejected Rust fixture");
    require(opened == plaintext, "native opener returned wrong plaintext");
    require(
        opener.open(frame, 1024, &opened) == Error::SequenceMismatch && opened.empty(),
        "native opener accepted a replayed sequence or retained stale plaintext");
    return 0;
}

// 将固定启动配置安全转换为通用挑战证明输入，并清理临时密钥副本。

#include "handshake.h"

#include <algorithm>
#include <string_view>

#include "secure_memory.h"

namespace azlw::agent {
namespace {

void copy_credentials(
    const BootstrapConfigV2& config,
    secure_channel::SessionId* session_id,
    secure_channel::SessionSecret* session_secret) noexcept {
    std::copy_n(config.session_id, session_id->size(), session_id->begin());
    std::copy_n(config.session_secret, session_secret->size(), session_secret->begin());
}

void set_error(std::string* error, std::string_view message) {
    if (error != nullptr) {
        error->assign(message);
    }
}

}  // namespace

bool create_handshake_challenge(
    const BootstrapConfigV2& config,
    const secure_channel::HandshakeNonce& server_nonce,
    secure_channel::ServerChallenge* challenge,
    std::string* error) noexcept {
    secure_channel::SessionId session_id{};
    secure_channel::SessionSecret session_secret{};
    copy_credentials(config, &session_id, &session_secret);
    const secure_channel::Error result = secure_channel::create_server_challenge(
        session_id,
        session_secret,
        server_nonce,
        challenge);
    secure_zero(session_secret.data(), session_secret.size());
    if (result != secure_channel::Error::Ok) {
        set_error(error, "构造服务端挑战失败");
        return false;
    }
    if (error != nullptr) {
        error->clear();
    }
    return true;
}

bool verify_handshake_proof(
    const secure_channel::ServerChallenge& challenge,
    std::span<const std::uint8_t> frame,
    const BootstrapConfigV2& config,
    secure_channel::ChannelKeyMaterial* keys,
    std::string* error) noexcept {
    secure_channel::SessionId session_id{};
    secure_channel::SessionSecret session_secret{};
    copy_credentials(config, &session_id, &session_secret);
    const secure_channel::Error result = secure_channel::verify_client_proof(
        challenge,
        session_id,
        session_secret,
        frame,
        keys);
    secure_zero(session_secret.data(), session_secret.size());
    if (result != secure_channel::Error::Ok) {
        set_error(error, "客户端挑战证明无效");
        return false;
    }
    if (error != nullptr) {
        error->clear();
    }
    return true;
}

}  // namespace azlw::agent

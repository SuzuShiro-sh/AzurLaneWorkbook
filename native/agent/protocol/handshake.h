// 声明启动配置与通用挑战证明协议之间的窄适配层。

#pragma once

#include <cstdint>
#include <span>
#include <string>

#include "bootstrap_config.h"
#include "secure_channel.h"

namespace azlw::agent {

/// 使用启动会话身份和服务端随机数构造不含密钥原文的挑战。
bool create_handshake_challenge(
    const BootstrapConfigV2& config,
    const secure_channel::HandshakeNonce& server_nonce,
    secure_channel::ServerChallenge* challenge,
    std::string* error) noexcept;

/// 验证客户端证明并派生当前连接唯一的双向密钥，失败不改变启动配置。
bool verify_handshake_proof(
    const secure_channel::ServerChallenge& challenge,
    std::span<const std::uint8_t> frame,
    const BootstrapConfigV2& config,
    secure_channel::ChannelKeyMaterial* keys,
    std::string* error) noexcept;

}  // namespace azlw::agent

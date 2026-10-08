// 声明供加载器和 agent 复用的启动配置校验接口。

#pragma once

#include <string>

#include "bootstrap_config.h"

namespace azlw {

/// 校验 loader 与 agent 共同使用的固定启动结构，不读取任何外部状态。
bool validate_bootstrap_config(const BootstrapConfigV2& config, std::string* error);
/// 校验 cleanup loader 使用的固定卸载结构，不读取任何外部状态。
bool validate_unload_config(const AgentUnloadConfigV1& config, std::string* error);

/// 将固定 128 位会话 ID 编码为 32 位小写十六进制。
std::string bootstrap_session_id_hex(const BootstrapConfigV2& config);
/// 将固定 128 位通道 ID 编码为 32 位小写十六进制。
std::string bootstrap_channel_id_hex(const BootstrapConfigV2& config);
/// 将固定 128 位匿名映射 ID 编码为 32 位小写十六进制。
std::string bootstrap_mapping_id_hex(const BootstrapConfigV2& config);
/// 将卸载结构中的固定会话 ID 编码为 32 位小写十六进制。
std::string unload_session_id_hex(const AgentUnloadConfigV1& config);

}  // namespace azlw

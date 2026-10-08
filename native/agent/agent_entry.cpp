// 实现加载器调用的唯一 agent 导出入口，并保证启动配置副本及时擦除。

#include <cstddef>
#include <cstdint>
#include <cstring>

#include "bootstrap_config.h"
#include "runtime/runtime.h"
#include "secure_memory.h"

/// 发布组装器离线核对的唯一版本标记，不增加运行态导出接口。
extern "C" __attribute__((visibility("hidden"), used)) const char azlw_agent_version_marker[] =
    "AZLW_AGENT_VERSION=" AZLW_AGENT_VERSION;

/// 校验固定配置长度，将独立副本交给运行态启动后立即擦除。
extern "C" __attribute__((visibility("default"), used)) std::int32_t azlw_agent_start(
    const void* bootstrap,
    std::size_t size) {
    if (bootstrap == nullptr || size != sizeof(azlw::BootstrapConfigV2)) {
        return static_cast<std::int32_t>(azlw::AgentStartCode::InvalidConfig);
    }

    azlw::BootstrapConfigV2 config{};
    std::memcpy(&config, bootstrap, sizeof(config));
    const azlw::AgentStartCode result = azlw::agent::AgentRuntime::instance().start(config);
    azlw::secure_zero(&config, sizeof(config));
    return static_cast<std::int32_t>(result);
}

/// 由已冻结并完成线程地址核验的 loader 调用，成功后才允许卸载 DSO。
extern "C" __attribute__((visibility("default"), used)) std::int32_t azlw_agent_finalize_shutdown() {
    return static_cast<std::int32_t>(
        azlw::agent::AgentRuntime::instance().finalize_shutdown());
}

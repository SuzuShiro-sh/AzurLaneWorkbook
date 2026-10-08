// 定义加载器唯一允许的 AndKitty 内存注入配置。

#pragma once

#include <cstdint>
#include <string>
#include <utility>

#include <Injector/KittyInjector.hpp>

namespace azlw::loader {

/// 构造加载器唯一允许的 AndKitty 配置，避免磁盘路径暴露给目标 UID。
inline inject_elf_config_t make_injector_config(
    int sdk,
    std::uint32_t timeout_ms,
    std::string memfd_name = {}) {
    inject_elf_config_t config{};
    config.sdk = sdk;
    config.timeout = static_cast<int>(timeout_ms);
    config.rtdl_flags = RTLD_LOCAL | RTLD_NOW;
    config.memfd = true;
    config.memfd_name = std::move(memfd_name);
    return config;
}

}  // namespace azlw::loader

// 声明 Android 运行态使用的系统安全随机字节读取入口。

#pragma once

#include <cstdint>
#include <span>
#include <string>

namespace azlw::agent {

/// 从系统随机设备完整填充缓冲区；任何失败都会擦除已写入字节。
bool fill_secure_random(std::span<std::uint8_t> output, std::string* error) noexcept;

}  // namespace azlw::agent

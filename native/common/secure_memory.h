// 提供不会被编译器优化移除的敏感内存擦除原语。

#pragma once

#include <cstddef>
#include <cstdint>

namespace azlw {

/// 通过 volatile 写入清除密钥，避免编译器把擦除优化掉。
inline void secure_zero(void* memory, std::size_t size) noexcept {
    volatile auto* bytes = static_cast<volatile std::uint8_t*>(memory);
    while (size > 0) {
        *bytes = 0;
        ++bytes;
        --size;
    }
}

}  // namespace azlw

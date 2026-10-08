// 定义固定 64 字节 ELF 头保护、恢复和失败回滚事务。

#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <string>

namespace azlw::loader {

inline constexpr std::size_t kProtectedElfHeaderSize = 64;
using ElfHeaderBytes = std::array<std::uint8_t, kProtectedElfHeaderSize>;

/// ELF 头事务只依赖可故障注入的远程读写和强随机字节来源。
class ElfHeaderProtectionBackend {
public:
    virtual ~ElfHeaderProtectionBackend() = default;

    virtual bool read(std::uintptr_t address, void* output, std::size_t size) = 0;
    virtual bool write(std::uintptr_t address, const void* input, std::size_t size) = 0;
    virtual bool random_bytes(void* output, std::size_t size) = 0;
    [[nodiscard]] virtual std::string last_error() const = 0;
};

/// 卸载时严格匹配的随机化后完整 ELF 头。
struct ElfHeaderProtectionEvidence final {
    ElfHeaderBytes protected_header{};
};

/// 区分当前已保护、已恢复原头和无法证明的现场。
enum class ElfHeaderProtectionState : std::uint8_t {
    Protected,
    Original,
    Unknown,
};

/// 只接受本项目运行时支持的 little-endian ELF64 x86_64 标准文件头。
[[nodiscard]] bool is_supported_elf_header(const ElfHeaderBytes& header) noexcept;

/// 将远程标准 ELF 头替换为随机字节，并在失败时恢复原头。
ElfHeaderProtectionState protect_elf_header(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    const ElfHeaderBytes& expected_original_header,
    ElfHeaderProtectionEvidence* evidence,
    std::string* error);

/// 要求远程字节精确匹配保护证据，再恢复已校验文件提供的原始头。
ElfHeaderProtectionState restore_elf_header(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    const ElfHeaderBytes& original_header,
    const ElfHeaderProtectionEvidence& evidence,
    std::string* error);

}  // namespace azlw::loader

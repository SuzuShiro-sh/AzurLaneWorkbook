// 实现完整 ELF 头随机化、逐字节提交验证与确定性回滚。

#include "elf_header_protection.h"

#include <algorithm>
#include <cstring>

namespace azlw::loader {
namespace {

constexpr std::uint8_t kElfMagic[] = {0x7f, 'E', 'L', 'F'};
constexpr std::size_t kElfClassOffset = 4;
constexpr std::size_t kElfDataOffset = 5;
constexpr std::size_t kElfMachineOffset = 18;
constexpr std::size_t kElfHeaderSizeOffset = 52;
constexpr std::uint16_t kElfMachineX86_64 = 62;

std::uint16_t read_u16_le(const ElfHeaderBytes& bytes, std::size_t offset) noexcept {
    return static_cast<std::uint16_t>(bytes[offset]) |
           static_cast<std::uint16_t>(bytes[offset + 1] << 8);
}

void append_error(std::string* error, const std::string& context, const std::string& detail) {
    if (!error->empty()) {
        *error += "；";
    }
    *error += context + ": " + detail;
}

bool read_header(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    ElfHeaderBytes* output,
    std::string* error) {
    if (!backend->read(address, output->data(), output->size())) {
        *error = "读取远程 ELF 头失败: " + backend->last_error();
        return false;
    }
    return true;
}

bool write_and_verify(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    const ElfHeaderBytes& expected,
    const std::string& label,
    std::string* error) {
    if (!backend->write(address, expected.data(), expected.size())) {
        *error = "写入" + label + "失败: " + backend->last_error();
        return false;
    }
    ElfHeaderBytes actual{};
    if (!read_header(backend, address, &actual, error)) {
        return false;
    }
    if (actual != expected) {
        *error = label + "写后逐字节验证不一致";
        return false;
    }
    return true;
}

bool is_protected_header(const ElfHeaderBytes& header) noexcept {
    const bool all_zero = std::all_of(header.begin(), header.end(), [](std::uint8_t value) {
        return value == 0;
    });
    return !all_zero && !std::equal(std::begin(kElfMagic), std::end(kElfMagic), header.begin());
}

bool rollback(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    const ElfHeaderBytes& expected,
    const std::string& context,
    std::string* error) {
    std::string rollback_error;
    if (!write_and_verify(backend, address, expected, context, &rollback_error)) {
        append_error(error, context + "失败", rollback_error);
        return false;
    }
    return true;
}

}  // namespace

bool is_supported_elf_header(const ElfHeaderBytes& header) noexcept {
    return std::equal(std::begin(kElfMagic), std::end(kElfMagic), header.begin()) &&
           header[kElfClassOffset] == 2 && header[kElfDataOffset] == 1 &&
           read_u16_le(header, kElfMachineOffset) == kElfMachineX86_64 &&
           read_u16_le(header, kElfHeaderSizeOffset) == kProtectedElfHeaderSize;
}

ElfHeaderProtectionState protect_elf_header(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    const ElfHeaderBytes& expected_original_header,
    ElfHeaderProtectionEvidence* evidence,
    std::string* error) {
    if (backend == nullptr || address == 0 || evidence == nullptr || error == nullptr) {
        if (error != nullptr) {
            *error = "ELF 头保护参数无效";
        }
        return ElfHeaderProtectionState::Unknown;
    }
    *evidence = {};
    error->clear();

    ElfHeaderBytes original{};
    if (!read_header(backend, address, &original, error)) {
        return ElfHeaderProtectionState::Unknown;
    }
    if (!is_supported_elf_header(expected_original_header) ||
        original != expected_original_header) {
        *error = "远程 Agent ELF 头与已校验文件不一致";
        return ElfHeaderProtectionState::Original;
    }

    ElfHeaderBytes protected_header{};
    if (!backend->random_bytes(protected_header.data(), protected_header.size())) {
        *error = "生成 ELF 头保护随机字节失败: " + backend->last_error();
        return ElfHeaderProtectionState::Original;
    }
    if (std::equal(std::begin(kElfMagic), std::end(kElfMagic), protected_header.begin())) {
        protected_header[0] ^= 0xff;
    }
    if (std::all_of(
            protected_header.begin(), protected_header.end(), [](std::uint8_t value) {
                return value == 0;
            })) {
        protected_header.back() = 1;
    }
    if (protected_header == expected_original_header ||
        !is_protected_header(protected_header)) {
        *error = "生成的 ELF 头保护字节未改变可识别特征";
        return ElfHeaderProtectionState::Original;
    }

    if (!write_and_verify(backend, address, protected_header, "随机化 ELF 头", error)) {
        return rollback(backend, address, expected_original_header, "回滚原始 ELF 头", error)
                   ? ElfHeaderProtectionState::Original
                   : ElfHeaderProtectionState::Unknown;
    }
    evidence->protected_header = protected_header;
    return ElfHeaderProtectionState::Protected;
}

ElfHeaderProtectionState restore_elf_header(
    ElfHeaderProtectionBackend* backend,
    std::uintptr_t address,
    const ElfHeaderBytes& original_header,
    const ElfHeaderProtectionEvidence& evidence,
    std::string* error) {
    if (backend == nullptr || address == 0 || error == nullptr ||
        !is_supported_elf_header(original_header) ||
        !is_protected_header(evidence.protected_header)) {
        if (error != nullptr) {
            *error = "ELF 头恢复参数或证据无效";
        }
        return ElfHeaderProtectionState::Unknown;
    }
    error->clear();

    ElfHeaderBytes current{};
    if (!read_header(backend, address, &current, error) ||
        current != evidence.protected_header) {
        if (error->empty()) {
            *error = "远程 ELF 头与保护证据不一致";
        }
        return ElfHeaderProtectionState::Unknown;
    }
    if (!write_and_verify(backend, address, original_header, "恢复原始 ELF 头", error)) {
        return rollback(
                   backend,
                   address,
                   evidence.protected_header,
                   "回滚受保护 ELF 头",
                   error)
                   ? ElfHeaderProtectionState::Protected
                   : ElfHeaderProtectionState::Unknown;
    }
    return ElfHeaderProtectionState::Original;
}

}  // namespace azlw::loader

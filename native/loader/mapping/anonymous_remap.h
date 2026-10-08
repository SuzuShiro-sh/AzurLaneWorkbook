// 定义 Agent memfd 段匿名化事务及其可验证结果。

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace azlw::loader {

/// 与具体远程内存库解耦的映射快照。
struct RemoteMapping final {
    std::uintptr_t start = 0;
    std::uintptr_t end = 0;
    std::size_t length = 0;
    int protection = 0;
    bool is_private = false;
    bool is_shared = false;
    std::uintptr_t offset = 0;
    std::string device;
    std::uint64_t inode = 0;
    std::string pathname;
};

/// 远程映射事务只依赖这些可故障注入的原语。
class AnonymousRemapBackend {
public:
    virtual ~AnonymousRemapBackend() = default;

    [[nodiscard]] virtual std::vector<RemoteMapping> mappings() = 0;
    virtual bool read(std::uintptr_t address, void* output, std::size_t size) = 0;
    virtual bool write(std::uintptr_t address, const void* input, std::size_t size) = 0;
    [[nodiscard]] virtual std::uintptr_t map(
        std::uintptr_t address,
        std::size_t size,
        int protection,
        int flags,
        int descriptor,
        std::uintptr_t offset) = 0;
    [[nodiscard]] virtual std::uintptr_t remap(
        std::uintptr_t old_address,
        std::size_t old_size,
        std::size_t new_size,
        int flags,
        std::uintptr_t new_address) = 0;
    virtual bool protect(std::uintptr_t address, std::size_t size, int protection) = 0;
    virtual bool unmap(std::uintptr_t address, std::size_t size) = 0;
    virtual bool close_descriptor(int descriptor) = 0;
    [[nodiscard]] virtual std::string last_error() const = 0;
};

/// 成功事务实际匿名化的段数与字节数。
struct AnonymousRemapEvidence final {
    std::size_t segment_count = 0;
    std::uint64_t byte_count = 0;
};

/// 区分已应用、已证明恢复和需要目标重启的未知现场。
enum class AnonymousRemapResult : std::uint8_t {
    Applied,
    Restored,
    Unknown,
};

/// 将指定加载区间内的精确 memfd 段原址替换为私有匿名段，并在失败时恢复。
AnonymousRemapResult remap_memfd_segments_anonymously(
    AnonymousRemapBackend* backend,
    const std::string& mapping_name,
    std::uintptr_t agent_start,
    std::size_t agent_size,
    int remote_memfd,
    AnonymousRemapEvidence* evidence,
    std::string* error);

}  // namespace azlw::loader

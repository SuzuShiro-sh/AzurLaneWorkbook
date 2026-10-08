// 定义远程内存写入与读回比对的可测试证据边界。

#pragma once

#include <cstddef>
#include <cstdint>
#include <span>
#include <string>

namespace azlw::loader {

/// 抽象远程内存读写，使部分传输和内容不一致可以稳定故障注入。
class RemoteMemoryEvidenceBackend {
public:
    virtual ~RemoteMemoryEvidenceBackend() = default;

    virtual std::size_t read(
        std::uintptr_t address,
        void* output,
        std::size_t size) = 0;
    virtual std::size_t write(
        std::uintptr_t address,
        const void* input,
        std::size_t size) = 0;
};

/// 只有完整读取且内容逐字节一致时，才证明远程区域处于预期状态。
bool verify_remote_memory(
    RemoteMemoryEvidenceBackend* backend,
    std::uintptr_t address,
    std::span<const std::uint8_t> expected,
    std::string* error);

/// 只有完整写入并通过独立读回比对时，才证明远程状态已经改变。
bool write_and_verify_remote_memory(
    RemoteMemoryEvidenceBackend* backend,
    std::uintptr_t address,
    std::span<const std::uint8_t> expected,
    std::string* error);

}  // namespace azlw::loader

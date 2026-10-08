// 实现 AndKitty ptrace 写后端、系统随机源和受控 Agent 文件头读取。

#include "elf_header_protection_kitty.h"

#include <cerrno>
#include <cstring>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <KittyMemoryMgr.hpp>

namespace azlw::loader {
namespace {

class ScopedDescriptor final {
public:
    explicit ScopedDescriptor(int value) : value_(value) {}
    ~ScopedDescriptor() {
        if (value_ >= 0) {
            close(value_);
        }
    }

    ScopedDescriptor(const ScopedDescriptor&) = delete;
    ScopedDescriptor& operator=(const ScopedDescriptor&) = delete;

    [[nodiscard]] int get() const noexcept { return value_; }

private:
    int value_;
};

bool read_exact(int descriptor, void* output, std::size_t size, std::string* error) {
    auto* cursor = static_cast<std::uint8_t*>(output);
    std::size_t total = 0;
    while (total < size) {
        errno = 0;
        const ssize_t count = read(descriptor, cursor + total, size - total);
        if (count > 0) {
            total += static_cast<std::size_t>(count);
            continue;
        }
        if (count < 0 && errno == EINTR) {
            continue;
        }
        *error = count == 0 ? "文件提前结束" : std::strerror(errno);
        return false;
    }
    return true;
}

class KittyElfHeaderProtectionBackend final : public ElfHeaderProtectionBackend {
public:
    explicit KittyElfHeaderProtectionBackend(KittyMemoryMgr* memory) : memory_(memory) {}

    bool read(std::uintptr_t address, void* output, std::size_t size) override {
        if (memory_ == nullptr || output == nullptr || size == 0 ||
            memory_->readMem(address, output, size) != size) {
            last_error_ = "AndKitty 远程 ELF 头读取不完整";
            return false;
        }
        last_error_.clear();
        return true;
    }

    bool write(std::uintptr_t address, const void* input, std::size_t size) override {
        if (memory_ == nullptr || input == nullptr || size == 0 ||
            memory_->trace.pokeMem(address, input, size) != size) {
            last_error_ = "AndKitty ptrace ELF 头写入不完整";
            return false;
        }
        last_error_.clear();
        return true;
    }

    bool random_bytes(void* output, std::size_t size) override {
        if (output == nullptr || size == 0) {
            last_error_ = "随机字节输出无效";
            return false;
        }
        const ScopedDescriptor random(open("/dev/urandom", O_RDONLY | O_CLOEXEC));
        if (random.get() < 0) {
            last_error_ = "打开 /dev/urandom 失败: " + std::string(std::strerror(errno));
            return false;
        }
        if (!read_exact(random.get(), output, size, &last_error_)) {
            last_error_ = "读取 /dev/urandom 失败: " + last_error_;
            return false;
        }
        last_error_.clear();
        return true;
    }

    [[nodiscard]] std::string last_error() const override {
        return last_error_.empty() ? "未知 ELF 头远程操作错误" : last_error_;
    }

private:
    KittyMemoryMgr* memory_;
    std::string last_error_;
};

}  // namespace

ElfHeaderProtectionState protect_kitty_elf_header(
    KittyMemoryMgr* memory,
    std::uintptr_t address,
    const ElfHeaderBytes& expected_original_header,
    ElfHeaderProtectionEvidence* evidence,
    std::string* error) {
    KittyElfHeaderProtectionBackend backend(memory);
    return protect_elf_header(
        &backend, address, expected_original_header, evidence, error);
}

ElfHeaderProtectionState restore_kitty_elf_header(
    KittyMemoryMgr* memory,
    std::uintptr_t address,
    const ElfHeaderBytes& original_header,
    const ElfHeaderProtectionEvidence& evidence,
    std::string* error) {
    KittyElfHeaderProtectionBackend backend(memory);
    return restore_elf_header(&backend, address, original_header, evidence, error);
}

bool read_agent_elf_header(
    const std::string& path,
    ElfHeaderBytes* header,
    std::string* error) {
    if (path.empty() || header == nullptr || error == nullptr) {
        if (error != nullptr) {
            *error = "Agent ELF 头文件参数无效";
        }
        return false;
    }
    const ScopedDescriptor file(open(path.c_str(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW));
    if (file.get() < 0) {
        *error = "打开 Agent 文件失败: " + std::string(std::strerror(errno));
        return false;
    }
    struct stat metadata {};
    if (fstat(file.get(), &metadata) != 0 || !S_ISREG(metadata.st_mode) ||
        metadata.st_size < static_cast<off_t>(header->size())) {
        *error = "Agent 文件不是足够长的普通文件";
        return false;
    }
    if (!read_exact(file.get(), header->data(), header->size(), error)) {
        *error = "读取 Agent ELF 头失败: " + *error;
        return false;
    }
    if (!is_supported_elf_header(*header)) {
        *error = "Agent 文件头不是受支持的 little-endian ELF64 x86_64";
        return false;
    }
    return true;
}

}  // namespace azlw::loader

// 实现 AndKitty 后端转换，并保持匿名化核心可独立故障注入测试。

#include "anonymous_remap_kitty.h"

#include <utility>

#include <KittyMemoryMgr.hpp>

#include "Injector/KittyInjectorSyscall.hpp"

namespace azlw::loader {
namespace {

class KittyAnonymousRemapBackend final : public AnonymousRemapBackend {
public:
    KittyAnonymousRemapBackend(KittyMemoryMgr* memory, KittyRemoteSys* remote_syscall)
        : memory_(memory), remote_syscall_(remote_syscall) {}

    [[nodiscard]] std::vector<RemoteMapping> mappings() override {
        std::vector<RemoteMapping> result;
        if (memory_ == nullptr || !memory_->isMemValid()) {
            last_error_ = "远程内存管理器无效";
            return result;
        }
        const auto process_maps = KittyMemoryEx::getAllMaps(memory_->processID());
        result.reserve(process_maps.size());
        for (const KittyMemoryEx::ProcMap& mapping : process_maps) {
            result.push_back(RemoteMapping{
                .start = mapping.startAddress,
                .end = mapping.endAddress,
                .length = mapping.length,
                .protection = mapping.protection,
                .is_private = mapping.is_private,
                .is_shared = mapping.is_shared,
                .offset = mapping.offset,
                .device = mapping.dev,
                .inode = mapping.inode,
                .pathname = mapping.pathname,
            });
        }
        if (result.empty()) {
            last_error_ = "目标 maps 为空";
        }
        return result;
    }

    bool read(std::uintptr_t address, void* output, std::size_t size) override {
        if (memory_ == nullptr || memory_->readMem(address, output, size) != size) {
            last_error_ = "远程读取未覆盖完整区间";
            return false;
        }
        return true;
    }

    bool write(std::uintptr_t address, const void* input, std::size_t size) override {
        if (memory_ == nullptr ||
            memory_->writeMem(address, const_cast<void*>(input), size) != size) {
            last_error_ = "远程写入未覆盖完整区间";
            return false;
        }
        return true;
    }

    [[nodiscard]] std::uintptr_t map(
        std::uintptr_t address,
        std::size_t size,
        int protection,
        int flags,
        int descriptor,
        std::uintptr_t offset) override {
        if (remote_syscall_ == nullptr) {
            last_error_ = "远程系统调用器无效";
            return 0;
        }
        const std::uintptr_t mapped = remote_syscall_->rmmap(
            address, size, protection, flags, descriptor, offset);
        capture_syscall_error(mapped != 0);
        return mapped;
    }

    [[nodiscard]] std::uintptr_t remap(
        std::uintptr_t old_address,
        std::size_t old_size,
        std::size_t new_size,
        int flags,
        std::uintptr_t new_address) override {
        if (remote_syscall_ == nullptr) {
            last_error_ = "远程系统调用器无效";
            return 0;
        }
        const std::uintptr_t mapped = remote_syscall_->rmremap(
            old_address, old_size, new_size, flags, new_address);
        capture_syscall_error(mapped != 0);
        return mapped;
    }

    bool protect(std::uintptr_t address, std::size_t size, int protection) override {
        if (remote_syscall_ == nullptr) {
            last_error_ = "远程系统调用器无效";
            return false;
        }
        const bool protected_mapping = remote_syscall_->rmprotect(address, size, protection);
        capture_syscall_error(protected_mapping);
        return protected_mapping;
    }

    bool unmap(std::uintptr_t address, std::size_t size) override {
        if (remote_syscall_ == nullptr) {
            last_error_ = "远程系统调用器无效";
            return false;
        }
        const bool unmapped = remote_syscall_->rmunmap(address, size);
        capture_syscall_error(unmapped);
        return unmapped;
    }

    bool close_descriptor(int descriptor) override {
        if (remote_syscall_ == nullptr) {
            last_error_ = "远程系统调用器无效";
            return false;
        }
        const bool closed = remote_syscall_->rclose(descriptor);
        capture_syscall_error(closed);
        return closed;
    }

    [[nodiscard]] std::string last_error() const override {
        return last_error_.empty() ? "未知远程操作错误" : last_error_;
    }

private:
    void capture_syscall_error(bool succeeded) {
        if (succeeded) {
            last_error_.clear();
        } else if (remote_syscall_ != nullptr) {
            last_error_ = remote_syscall_->lastError();
        }
    }

    KittyMemoryMgr* memory_;
    KittyRemoteSys* remote_syscall_;
    std::string last_error_;
};

}  // namespace

AnonymousRemapResult remap_kitty_memfd_anonymously(
    KittyMemoryMgr* memory,
    KittyRemoteSys* remote_syscall,
    const std::string& mapping_name,
    std::uintptr_t agent_start,
    std::size_t agent_size,
    int remote_memfd,
    AnonymousRemapEvidence* evidence,
    std::string* error) {
    KittyAnonymousRemapBackend backend(memory, remote_syscall);
    return remap_memfd_segments_anonymously(
        &backend,
        mapping_name,
        agent_start,
        agent_size,
        remote_memfd,
        evidence,
        error);
}

bool close_kitty_remote_memfd(
    KittyRemoteSys* remote_syscall,
    int remote_memfd,
    std::string* error) {
    if (remote_syscall == nullptr || remote_memfd < 0 || error == nullptr) {
        if (error != nullptr) {
            *error = "远程 memfd 关闭参数无效";
        }
        return false;
    }
    if (!remote_syscall->rclose(remote_memfd)) {
        *error = "关闭远程 memfd 失败: " + remote_syscall->lastError();
        return false;
    }
    return true;
}

}  // namespace azlw::loader

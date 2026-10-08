// 实现固定 14 字节 x86_64 绝对跳转 Hook、trampoline 和完整回滚流程。

#include "hook_x86_64.h"

#include <array>
#include <cerrno>
#include <cstring>
#include <elf.h>
#include <limits>
#include <string>
#include <sys/mman.h>
#include <unistd.h>

namespace azlw::agent {
namespace {

/// RIP 间接绝对跳转指令和内嵌 64 位目标地址的固定总长度。
constexpr std::size_t kAbsoluteJumpSize = 14;

/// 构造不依赖目标距离的 RIP 间接绝对跳转字节。
std::array<std::uint8_t, kAbsoluteJumpSize> absolute_jump(std::uintptr_t destination) {
    std::array<std::uint8_t, kAbsoluteJumpSize> jump{
        0xff, 0x25, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    };
    std::memcpy(jump.data() + 6, &destination, sizeof(destination));
    return jump;
}

/// 将覆盖范围所在 ELF 段权限转换为 `mprotect` 标志。
int segment_protection(const ModuleView& module, std::uintptr_t address, std::size_t size) {
    if (size == 0 || address > std::numeric_limits<std::uintptr_t>::max() - size) {
        return 0;
    }
    const std::uintptr_t end = address + size;
    for (const ModuleSegment& segment : module.segments) {
        if (address < segment.start || end > segment.end) {
            continue;
        }
        int protection = 0;
        if ((segment.flags & PF_R) != 0) {
            protection |= PROT_READ;
        }
        if ((segment.flags & PF_W) != 0) {
            protection |= PROT_WRITE;
        }
        if ((segment.flags & PF_X) != 0) {
            protection |= PROT_EXEC;
        }
        return protection;
    }
    return 0;
}

}  // namespace

// 析构路径尽力恢复目标入口，避免残留指向已释放 trampoline 的跳转。
X64Hook::~X64Hook() {
    std::string ignored;
    uninstall(&ignored);
}

// 先准备只读可执行 trampoline 并发布地址，再短暂开放目标页写权限。
bool X64Hook::install(
    const ModuleView& module,
    std::uintptr_t target,
    std::uintptr_t replacement,
    const std::uint8_t (&expected)[kHookPrologueSize],
    std::atomic<std::uintptr_t>* trampoline_output,
    std::string* error) {
    if (installed_ || target == 0 || replacement == 0 || trampoline_output == nullptr) {
        *error = "Hook 安装参数无效或已经安装";
        return false;
    }
    static_assert(kHookPrologueSize == kAbsoluteJumpSize);
    if (!module.contains_executable(target, kHookPrologueSize)) {
        *error = "Hook 目标不在目标模块的可执行段内";
        return false;
    }
    if (std::memcmp(reinterpret_cast<const void*>(target), expected, kHookPrologueSize) != 0) {
        *error = "Hook 目标 prologue 与 profile 不匹配";
        return false;
    }

    const long page_size_result = sysconf(_SC_PAGESIZE);
    if (page_size_result <= 0) {
        *error = "读取设备页大小失败";
        return false;
    }
    const auto page_size = static_cast<std::uintptr_t>(page_size_result);
    const std::uintptr_t page_start = target & ~(page_size - 1);
    const std::uintptr_t target_end = target + kHookPrologueSize;
    const std::uintptr_t page_end = (target_end + page_size - 1) & ~(page_size - 1);

    target_ = target;
    protected_page_ = page_start;
    protected_size_ = static_cast<std::size_t>(page_end - page_start);
    original_protection_ = segment_protection(module, target, kHookPrologueSize);
    if ((original_protection_ & PROT_EXEC) == 0) {
        *error = "Hook 目标页缺少执行权限";
        target_ = 0;
        return false;
    }
    std::memcpy(original_.data(), reinterpret_cast<const void*>(target), original_.size());

    trampoline_size_ = static_cast<std::size_t>(page_size);
    trampoline_ = mmap(
        nullptr,
        trampoline_size_,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0);
    if (trampoline_ == MAP_FAILED) {
        trampoline_ = nullptr;
        *error = "分配 Hook trampoline 失败: " + std::string(std::strerror(errno));
        target_ = 0;
        return false;
    }
    auto* trampoline_bytes = static_cast<std::uint8_t*>(trampoline_);
    std::memcpy(trampoline_bytes, original_.data(), original_.size());
    const auto return_jump = absolute_jump(target + kHookPrologueSize);
    std::memcpy(trampoline_bytes + kHookPrologueSize, return_jump.data(), return_jump.size());
    __builtin___clear_cache(
        reinterpret_cast<char*>(trampoline_),
        reinterpret_cast<char*>(trampoline_) + kHookPrologueSize + return_jump.size());
    if (mprotect(trampoline_, trampoline_size_, PROT_READ | PROT_EXEC) != 0) {
        *error = "设置 Hook trampoline 执行权限失败: " + std::string(std::strerror(errno));
        munmap(trampoline_, trampoline_size_);
        trampoline_ = nullptr;
        target_ = 0;
        return false;
    }

    trampoline_output_ = trampoline_output;
    trampoline_output_->store(reinterpret_cast<std::uintptr_t>(trampoline_), std::memory_order_release);
    if (!set_target_protection(original_protection_ | PROT_WRITE, error)) {
        trampoline_output_->store(0, std::memory_order_release);
        munmap(trampoline_, trampoline_size_);
        trampoline_ = nullptr;
        target_ = 0;
        return false;
    }

    const auto entry_jump = absolute_jump(replacement);
    std::memcpy(reinterpret_cast<void*>(target_), entry_jump.data(), entry_jump.size());
    __builtin___clear_cache(
        reinterpret_cast<char*>(target_),
        reinterpret_cast<char*>(target_ + kHookPrologueSize));
    if (!set_target_protection(original_protection_, error)) {
        std::memcpy(reinterpret_cast<void*>(target_), original_.data(), original_.size());
        __builtin___clear_cache(
            reinterpret_cast<char*>(target_),
            reinterpret_cast<char*>(target_ + kHookPrologueSize));
        set_target_protection(original_protection_, error);
        trampoline_output_->store(0, std::memory_order_release);
        munmap(trampoline_, trampoline_size_);
        trampoline_ = nullptr;
        target_ = 0;
        return false;
    }
    installed_ = true;
    return true;
}

HookUninstallStatus X64Hook::uninstall_frozen() noexcept {
    if (!installed_) {
        if (trampoline_ != nullptr) {
            if (munmap(trampoline_, trampoline_size_) != 0) {
                return HookUninstallStatus::TrampolineReleaseFailed;
            }
            trampoline_ = nullptr;
            trampoline_size_ = 0;
        }
        return HookUninstallStatus::Ok;
    }
    if (!set_target_protection_raw(original_protection_ | PROT_WRITE)) {
        return HookUninstallStatus::TargetWriteProtectionFailed;
    }
    std::memcpy(reinterpret_cast<void*>(target_), original_.data(), original_.size());
    __builtin___clear_cache(
        reinterpret_cast<char*>(target_),
        reinterpret_cast<char*>(target_ + kHookPrologueSize));
    if (!set_target_protection_raw(original_protection_)) {
        return HookUninstallStatus::TargetProtectionRestoreFailed;
    }
    if (trampoline_output_ != nullptr) {
        trampoline_output_->store(0, std::memory_order_release);
    }
    if (trampoline_ != nullptr && munmap(trampoline_, trampoline_size_) != 0) {
        return HookUninstallStatus::TrampolineReleaseFailed;
    }
    trampoline_ = nullptr;
    trampoline_size_ = 0;
    target_ = 0;
    trampoline_output_ = nullptr;
    installed_ = false;
    return HookUninstallStatus::Ok;
}

// 先执行与冻结终结相同的无分配事务，再在安全调用环境中补充具体诊断。
bool X64Hook::uninstall(std::string* error) {
    switch (uninstall_frozen()) {
        case HookUninstallStatus::Ok:
            return true;
        case HookUninstallStatus::TargetWriteProtectionFailed:
            *error = "开放 Hook 目标页写权限失败: " + std::string(std::strerror(errno));
            return false;
        case HookUninstallStatus::TargetProtectionRestoreFailed:
            *error = "恢复 Hook 目标页权限失败: " + std::string(std::strerror(errno));
            return false;
        case HookUninstallStatus::TrampolineReleaseFailed:
            *error = "释放 Hook trampoline 失败: " + std::string(std::strerror(errno));
            return false;
    }
    *error = "Hook 卸载返回未知状态";
    return false;
}

// 返回完整安装事务是否已提交。
bool X64Hook::installed() const noexcept {
    return installed_;
}

// 地址只在 Hook 完整安装期间有效，供宿主生成严格卸载收据。
std::uintptr_t X64Hook::target() const noexcept {
    return installed_ ? target_ : 0;
}

std::uintptr_t X64Hook::trampoline_start() const noexcept {
    return installed_ ? reinterpret_cast<std::uintptr_t>(trampoline_) : 0;
}

std::size_t X64Hook::trampoline_size() const noexcept {
    return installed_ ? trampoline_size_ : 0;
}

// 权限范围覆盖所有被替换字节，即使前导横跨页面边界。
bool X64Hook::set_target_protection(int protection, std::string* error) const {
    if (!set_target_protection_raw(protection)) {
        *error = "修改 Hook 目标页权限失败: " + std::string(std::strerror(errno));
        return false;
    }
    return true;
}

bool X64Hook::set_target_protection_raw(int protection) const noexcept {
    return mprotect(reinterpret_cast<void*>(protected_page_), protected_size_, protection) == 0;
}

}  // namespace azlw::agent

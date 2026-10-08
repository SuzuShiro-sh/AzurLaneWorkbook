// 声明仅适用于已验证 x86_64 函数前导字节的可回滚绝对跳转 Hook。

#pragma once

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <string>

#include "bootstrap_config.h"
#include "lua/module_view.h"

namespace azlw::agent {

/// 冻结终结路径使用的无分配 Hook 卸载结果。
enum class HookUninstallStatus : std::uint8_t {
    Ok,
    TargetWriteProtectionFailed,
    TargetProtectionRestoreFailed,
    TrampolineReleaseFailed,
};

/// 固定 14 字节绝对跳转 Hook；仅服务已验证的 tolua_update profile。
class X64Hook final {
public:
    /// 创建尚未安装的 Hook 对象。
    X64Hook() = default;
    /// 尝试还原目标字节并释放 trampoline。
    ~X64Hook();

    /// Hook 持有目标页和 trampoline 所有权，因此禁止复制。
    X64Hook(const X64Hook&) = delete;
    X64Hook& operator=(const X64Hook&) = delete;

    /// 核对前导字节后安装固定跳转，并原子发布原函数 trampoline。
    bool install(
        const ModuleView& module,
        std::uintptr_t target,
        std::uintptr_t replacement,
        const std::uint8_t (&expected)[kHookPrologueSize],
        std::atomic<std::uintptr_t>* trampoline_output,
        std::string* error);
    /// 还原原始前导字节、清空已发布地址并释放 trampoline。
    bool uninstall(std::string* error);
    /// 在线程全部冻结时执行同一卸载事务，不分配内存也不生成诊断文本。
    HookUninstallStatus uninstall_frozen() noexcept;
    /// 返回目标入口当前是否已由本对象接管。
    bool installed() const noexcept;
    /// 返回当前 Hook 目标入口，未安装时为 0。
    std::uintptr_t target() const noexcept;
    /// 返回匿名 trampoline 起始地址，未安装时为 0。
    std::uintptr_t trampoline_start() const noexcept;
    /// 返回匿名 trampoline 完整映射长度，未安装时为 0。
    std::size_t trampoline_size() const noexcept;

private:
    /// 修改覆盖目标字节所涉及的完整页面权限范围。
    bool set_target_protection(int protection, std::string* error) const;
    /// 冻结终结路径直接调用 `mprotect`，失败时只保留稳定状态码和 `errno`。
    bool set_target_protection_raw(int protection) const noexcept;

    std::uintptr_t target_ = 0;
    void* trampoline_ = nullptr;
    std::size_t trampoline_size_ = 0;
    std::uintptr_t protected_page_ = 0;
    std::size_t protected_size_ = 0;
    int original_protection_ = 0;
    std::array<std::uint8_t, kHookPrologueSize> original_{};
    std::atomic<std::uintptr_t>* trampoline_output_ = nullptr;
    bool installed_ = false;
};

}  // namespace azlw::agent

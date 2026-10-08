// 声明为加载器远程函数调用提供独立匿名栈的作用域事务。

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

#include <sys/user.h>

class KittyMemoryMgr;
class KittyRemoteSys;

namespace azlw::loader {

/// 分配远程匿名栈，并在释放前精确恢复目标主线程通用寄存器。
class RemoteCallStack final {
public:
    /// 当前对象只允许承载一次远程调用事务。
    RemoteCallStack() = default;

    RemoteCallStack(const RemoteCallStack&) = delete;
    RemoteCallStack& operator=(const RemoteCallStack&) = delete;

    /// 保存当前寄存器、分配远程栈并把 RSP 切换到栈顶。
    bool prepare(
        KittyMemoryMgr* memory,
        KittyRemoteSys* remote_sys,
        std::size_t stack_bytes,
        std::string* error);
    /// 恢复原寄存器、解除远程映射，再消除 munmap 的栈对齐副作用。
    bool restore_and_release(std::string* error);
    /// 返回远程函数调用应观察到的 16 字节对齐栈顶。
    std::uintptr_t stack_pointer() const noexcept;
    /// 判断失败路径是否仍在目标中留下未释放的匿名映射。
    bool has_remote_mapping() const noexcept;

private:
    KittyMemoryMgr* memory_ = nullptr;
    KittyRemoteSys* remote_sys_ = nullptr;
    std::uintptr_t mapping_ = 0;
    std::size_t mapping_size_ = 0;
    user_regs_struct original_registers_{};
    bool original_registers_captured_ = false;
};

}  // namespace azlw::loader

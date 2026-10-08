// 实现远程匿名栈的分配、精确寄存器恢复和确定性解除映射。

#include "remote_call_stack.h"

#include <limits>

#include <sys/mman.h>

#include <KittyMemoryMgr.hpp>

#include "Injector/KittyInjectorSyscall.hpp"

namespace azlw::loader {
namespace {

/// 保留首要错误并追加清理阶段诊断。
void append_error(std::string* error, const std::string& detail) {
    if (!error->empty()) {
        *error += "；";
    }
    *error += detail;
}

}  // namespace

bool RemoteCallStack::prepare(
    KittyMemoryMgr* memory,
    KittyRemoteSys* remote_sys,
    std::size_t stack_bytes,
    std::string* error) {
    if (memory == nullptr || remote_sys == nullptr || !memory->isMemValid() || stack_bytes == 0 ||
        memory_ != nullptr || mapping_ != 0 || original_registers_captured_) {
        *error = "远程调用栈参数或对象状态无效";
        return false;
    }
    if (!memory->trace.getRegs(&original_registers_)) {
        *error = "保存远程调用前主线程寄存器失败";
        return false;
    }
    original_registers_captured_ = true;
    memory_ = memory;
    remote_sys_ = remote_sys;

    mapping_ = remote_sys_->rmmap(
        0,
        stack_bytes,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0);
    mapping_size_ = mapping_ == 0 ? 0 : stack_bytes;
    const bool restored_after_mmap = memory_->trace.setRegs(&original_registers_);
    if (mapping_ == 0 || !restored_after_mmap) {
        *error = mapping_ == 0 ? "分配远程调用栈失败"
                               : "分配远程调用栈后恢复主线程寄存器失败";
        return false;
    }
    const auto fail_and_release = [this, error](const std::string& primary) {
        *error = primary;
        std::string cleanup_error;
        if (!restore_and_release(&cleanup_error)) {
            append_error(error, "回收远程调用栈失败: " + cleanup_error);
        }
        return false;
    };
    if (mapping_ > std::numeric_limits<std::uintptr_t>::max() - mapping_size_) {
        return fail_and_release("远程调用栈地址范围溢出");
    }
    const std::uintptr_t mapping_end = mapping_ + mapping_size_;
    const KittyMemoryEx::ProcMap map =
        KittyMemoryEx::getAddressMap(memory_->processID(), mapping_);
    if (!map.isValid() || !map.readable || !map.writeable || map.startAddress > mapping_ ||
        map.endAddress < mapping_end) {
        return fail_and_release("远程调用栈映射属性不完整");
    }

    user_regs_struct call_registers = original_registers_;
    call_registers.rsp = mapping_end & ~std::uintptr_t{0xF};
    if (!memory_->trace.setRegs(&call_registers)) {
        return fail_and_release("切换到远程调用栈失败");
    }
    return true;
}

bool RemoteCallStack::restore_and_release(std::string* error) {
    if (memory_ == nullptr || remote_sys_ == nullptr || mapping_ == 0 || mapping_size_ == 0 ||
        !original_registers_captured_) {
        *error = "远程调用栈尚未完整准备";
        return false;
    }
    if (!memory_->trace.setRegs(&original_registers_)) {
        *error = "解除远程调用栈前恢复主线程寄存器失败";
        return false;
    }

    const std::uintptr_t released_mapping = mapping_;
    const std::size_t released_size = mapping_size_;
    const bool unmapped = remote_sys_->rmunmap(released_mapping, released_size);
    const bool restored_after_munmap = memory_->trace.setRegs(&original_registers_);
    bool mapping_absent = false;
    if (unmapped) {
        mapping_absent =
            !KittyMemoryEx::getAddressMap(memory_->processID(), released_mapping).isValid();
        if (mapping_absent) {
            mapping_ = 0;
            mapping_size_ = 0;
        }
    }
    if (!unmapped) {
        *error = "解除远程调用栈映射失败";
    }
    if (!restored_after_munmap) {
        append_error(error, "munmap 后恢复主线程寄存器失败");
    }
    if (!unmapped || !restored_after_munmap) {
        return false;
    }
    if (!mapping_absent) {
        *error = "远程调用栈在 munmap 后仍存在映射";
        return false;
    }

    original_registers_captured_ = false;
    memory_ = nullptr;
    remote_sys_ = nullptr;
    return true;
}

std::uintptr_t RemoteCallStack::stack_pointer() const noexcept {
    if (mapping_ == 0 || mapping_size_ == 0 ||
        mapping_ > std::numeric_limits<std::uintptr_t>::max() - mapping_size_) {
        return 0;
    }
    return (mapping_ + mapping_size_) & ~std::uintptr_t{0xF};
}

bool RemoteCallStack::has_remote_mapping() const noexcept {
    return mapping_ != 0 && mapping_size_ != 0;
}

}  // namespace azlw::loader

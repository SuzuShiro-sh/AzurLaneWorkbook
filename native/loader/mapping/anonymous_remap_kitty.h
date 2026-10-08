// 连接匿名重映射事务与 AndKitty 远程内存、系统调用能力。

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

#include "anonymous_remap.h"

class KittyMemoryMgr;
class KittyRemoteSys;

namespace azlw::loader {

/// 使用目标进程当前 maps 与远程系统调用完成 Agent 匿名重映射。
AnonymousRemapResult remap_kitty_memfd_anonymously(
    KittyMemoryMgr* memory,
    KittyRemoteSys* remote_syscall,
    const std::string& mapping_name,
    std::uintptr_t agent_start,
    std::size_t agent_size,
    int remote_memfd,
    AnonymousRemapEvidence* evidence,
    std::string* error);

/// 在目标仍冻结时归还仅供 dlopen 使用的远程 memfd 描述符。
bool close_kitty_remote_memfd(
    KittyRemoteSys* remote_syscall,
    int remote_memfd,
    std::string* error);

}  // namespace azlw::loader

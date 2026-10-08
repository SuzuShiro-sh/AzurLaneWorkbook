// 连接 solist 可回滚事务与 AndKitty linker 扫描、远程内存能力。

#pragma once

#include <cstdint>
#include <string>

#include "solist_visibility.h"

class KittyMemoryMgr;

namespace azlw::loader {

/// 从当前 linker 尾部隐藏指定原生 soinfo。
SolistVisibilityState hide_kitty_solisted_tail(
    KittyMemoryMgr* memory,
    std::uintptr_t soinfo_address,
    SolistVisibilityEvidence* evidence,
    std::string* error);

/// 将隐藏 soinfo 接到当前 linker 尾部，供系统 dlclose 正常删除。
SolistVisibilityState restore_kitty_solisted_tail(
    KittyMemoryMgr* memory,
    const SolistVisibilityEvidence& evidence,
    std::string* error);

}  // namespace azlw::loader

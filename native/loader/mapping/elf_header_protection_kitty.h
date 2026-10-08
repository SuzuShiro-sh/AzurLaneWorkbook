// 连接 ELF 头保护事务与 AndKitty ptrace 远程内存及本地 Agent 文件。

#pragma once

#include <cstdint>
#include <string>

#include "elf_header_protection.h"

class KittyMemoryMgr;

namespace azlw::loader {

/// 使用系统强随机字节保护远程 Agent 的完整 ELF 头。
ElfHeaderProtectionState protect_kitty_elf_header(
    KittyMemoryMgr* memory,
    std::uintptr_t address,
    const ElfHeaderBytes& expected_original_header,
    ElfHeaderProtectionEvidence* evidence,
    std::string* error);

/// 在卸载前恢复由已校验 Agent 文件提供的原始 ELF 头。
ElfHeaderProtectionState restore_kitty_elf_header(
    KittyMemoryMgr* memory,
    std::uintptr_t address,
    const ElfHeaderBytes& original_header,
    const ElfHeaderProtectionEvidence& evidence,
    std::string* error);

/// 精确读取普通文件开头的标准 ELF64 x86_64 文件头。
bool read_agent_elf_header(
    const std::string& path,
    ElfHeaderBytes* header,
    std::string* error);

}  // namespace azlw::loader

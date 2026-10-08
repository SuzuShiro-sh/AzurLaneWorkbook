// 定义 Android linker solist 尾节点隐藏与恢复的可回滚事务。

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace azlw::loader {

/// 只保留验证单向链表所需的稳定节点字段。
struct SolistNode final {
    std::uintptr_t address = 0;
    std::uintptr_t next = 0;
};

/// 一次冻结时刻的 linker 链表、尾指针地址及 soinfo 布局。
struct SolistSnapshot final {
    std::vector<SolistNode> nodes;
    std::uintptr_t tail = 0;
    std::uintptr_t tail_pointer_address = 0;
    std::uintptr_t next_offset = 0;
};

/// 事务核心只依赖可故障注入的快照与指针读写能力。
class SolistVisibilityBackend {
public:
    virtual ~SolistVisibilityBackend() = default;

    virtual bool snapshot(SolistSnapshot* output) = 0;
    virtual bool read_pointer(std::uintptr_t address, std::uintptr_t* output) = 0;
    virtual bool write_pointer(std::uintptr_t address, std::uintptr_t value) = 0;
    [[nodiscard]] virtual std::string last_error() const = 0;
};

/// 成功隐藏后写入卸载凭据的最小稳定身份。
struct SolistVisibilityEvidence final {
    std::uintptr_t soinfo_address = 0;
};

/// 区分已隐藏、已接回和无法证明的链表现场。
enum class SolistVisibilityState : std::uint8_t {
    Hidden,
    Linked,
    Unknown,
};

/// 仅摘除当前尾节点，并在任一步失败时恢复原始链表。
SolistVisibilityState hide_solisted_tail(
    SolistVisibilityBackend* backend,
    std::uintptr_t soinfo_address,
    SolistVisibilityEvidence* evidence,
    std::string* error);

/// 将隐藏节点接到冻结时的当前尾部，允许隐藏期间链表继续增长。
SolistVisibilityState restore_solisted_tail(
    SolistVisibilityBackend* backend,
    const SolistVisibilityEvidence& evidence,
    std::string* error);

}  // namespace azlw::loader

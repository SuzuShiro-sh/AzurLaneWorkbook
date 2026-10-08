// 实现 linker solist 尾节点修改、提交验证与确定性回滚。

#include "solist_visibility.h"

#include <algorithm>
#include <limits>
#include <string>

namespace azlw::loader {
namespace {

void append_error(std::string* error, const std::string& context, const std::string& detail) {
    if (!error->empty()) {
        *error += "；";
    }
    *error += context + ": " + detail;
}

bool pointer_slot(
    std::uintptr_t node,
    std::uintptr_t offset,
    std::uintptr_t* slot,
    std::string* error) {
    if (node == 0 || offset > std::numeric_limits<std::uintptr_t>::max() - node) {
        *error = "soinfo next 指针地址溢出";
        return false;
    }
    *slot = node + offset;
    return true;
}

bool validate_snapshot(const SolistSnapshot& snapshot, std::string* error) {
    if (snapshot.nodes.empty() || snapshot.tail == 0 ||
        snapshot.tail_pointer_address == 0) {
        *error = "linker solist 快照缺少节点或尾指针";
        return false;
    }
    for (std::size_t index = 0; index < snapshot.nodes.size(); ++index) {
        const SolistNode& node = snapshot.nodes[index];
        if (node.address == 0) {
            *error = "linker solist 包含空节点";
            return false;
        }
        const auto duplicate = std::find_if(
            snapshot.nodes.begin(),
            snapshot.nodes.begin() + static_cast<std::ptrdiff_t>(index),
            [&node](const SolistNode& candidate) {
                return candidate.address == node.address;
            });
        if (duplicate != snapshot.nodes.begin() + static_cast<std::ptrdiff_t>(index)) {
            *error = "linker solist 包含重复节点或环";
            return false;
        }
        const std::uintptr_t expected_next =
            index + 1 < snapshot.nodes.size() ? snapshot.nodes[index + 1].address : 0;
        if (node.next != expected_next) {
            *error = "linker solist 节点顺序与 next 指针不一致";
            return false;
        }
    }
    if (snapshot.nodes.back().address != snapshot.tail) {
        *error = "linker solist 尾指针与最后节点不一致";
        return false;
    }
    return true;
}

bool snapshot_checked(
    SolistVisibilityBackend* backend,
    SolistSnapshot* snapshot,
    std::string* error) {
    if (!backend->snapshot(snapshot)) {
        *error = "读取 linker solist 失败: " + backend->last_error();
        return false;
    }
    return validate_snapshot(*snapshot, error);
}

bool read_expected(
    SolistVisibilityBackend* backend,
    std::uintptr_t address,
    std::uintptr_t expected,
    const std::string& label,
    std::string* error) {
    std::uintptr_t actual = 0;
    if (!backend->read_pointer(address, &actual)) {
        *error = "读取" + label + "失败: " + backend->last_error();
        return false;
    }
    if (actual != expected) {
        *error = label + "不匹配，expected=" + std::to_string(expected) +
                 ", actual=" + std::to_string(actual);
        return false;
    }
    return true;
}

bool write_and_verify(
    SolistVisibilityBackend* backend,
    std::uintptr_t address,
    std::uintptr_t value,
    const std::string& label,
    std::string* error) {
    if (!backend->write_pointer(address, value)) {
        *error = "写入" + label + "失败: " + backend->last_error();
        return false;
    }
    return read_expected(backend, address, value, label + "写后值", error);
}

bool verify_hidden_snapshot(
    SolistVisibilityBackend* backend,
    std::uintptr_t soinfo_address,
    std::uintptr_t expected_tail,
    std::string* error) {
    SolistSnapshot snapshot;
    if (!snapshot_checked(backend, &snapshot, error)) {
        return false;
    }
    const bool absent = std::none_of(
        snapshot.nodes.begin(), snapshot.nodes.end(), [soinfo_address](const SolistNode& node) {
            return node.address == soinfo_address;
        });
    if (!absent || snapshot.tail != expected_tail) {
        *error = "摘链后的 soinfo 仍可见或 linker 尾节点不匹配";
        return false;
    }
    return true;
}

bool verify_linked_snapshot(
    SolistVisibilityBackend* backend,
    std::uintptr_t soinfo_address,
    std::string* error) {
    SolistSnapshot snapshot;
    if (!snapshot_checked(backend, &snapshot, error)) {
        return false;
    }
    const std::size_t count = static_cast<std::size_t>(std::count_if(
        snapshot.nodes.begin(), snapshot.nodes.end(), [soinfo_address](const SolistNode& node) {
            return node.address == soinfo_address;
        }));
    if (count != 1 || snapshot.tail != soinfo_address ||
        snapshot.nodes.back().next != 0) {
        *error = "恢复后的 soinfo 未唯一接回 linker 尾部";
        return false;
    }
    return true;
}

bool rollback_hide(
    SolistVisibilityBackend* backend,
    const SolistSnapshot& original,
    std::uintptr_t predecessor_next_slot,
    std::uintptr_t soinfo_address,
    std::string* error) {
    std::string rollback_error;
    const bool predecessor_restored = write_and_verify(
        backend,
        predecessor_next_slot,
        soinfo_address,
        "回滚前驱 next",
        &rollback_error);
    const bool tail_restored = write_and_verify(
        backend,
        original.tail_pointer_address,
        soinfo_address,
        "回滚 linker 尾指针",
        &rollback_error);
    const bool chain_restored = predecessor_restored && tail_restored &&
                                verify_linked_snapshot(backend, soinfo_address, &rollback_error);
    if (!chain_restored) {
        append_error(error, "solist 隐藏回滚失败", rollback_error);
    }
    return chain_restored;
}

bool rollback_restore(
    SolistVisibilityBackend* backend,
    const SolistSnapshot& hidden,
    std::uintptr_t current_tail_next_slot,
    std::uintptr_t soinfo_address,
    std::string* error) {
    std::string rollback_error;
    const bool tail_restored = write_and_verify(
        backend,
        hidden.tail_pointer_address,
        hidden.tail,
        "回滚 linker 尾指针",
        &rollback_error);
    const bool next_restored = write_and_verify(
        backend,
        current_tail_next_slot,
        0,
        "回滚当前尾节点 next",
        &rollback_error);
    const bool chain_restored = tail_restored && next_restored &&
                                verify_hidden_snapshot(
                                    backend, soinfo_address, hidden.tail, &rollback_error);
    if (!chain_restored) {
        append_error(error, "solist 恢复回滚失败", rollback_error);
    }
    return chain_restored;
}

}  // namespace

SolistVisibilityState hide_solisted_tail(
    SolistVisibilityBackend* backend,
    std::uintptr_t soinfo_address,
    SolistVisibilityEvidence* evidence,
    std::string* error) {
    if (backend == nullptr || soinfo_address == 0 || evidence == nullptr || error == nullptr) {
        if (error != nullptr) {
            *error = "solist 隐藏参数无效";
        }
        return SolistVisibilityState::Linked;
    }
    *evidence = {};
    error->clear();

    SolistSnapshot original;
    if (!snapshot_checked(backend, &original, error)) {
        return SolistVisibilityState::Linked;
    }
    if (original.nodes.size() < 2 || original.tail != soinfo_address ||
        original.nodes.back().address != soinfo_address || original.nodes.back().next != 0) {
        *error = "只允许隐藏 linker solist 当前非头尾节点";
        return SolistVisibilityState::Linked;
    }
    const std::size_t occurrences = static_cast<std::size_t>(std::count_if(
        original.nodes.begin(), original.nodes.end(), [soinfo_address](const SolistNode& node) {
            return node.address == soinfo_address;
        }));
    if (occurrences != 1) {
        *error = "待隐藏 soinfo 未在 linker solist 中唯一出现";
        return SolistVisibilityState::Linked;
    }

    const SolistNode& predecessor = original.nodes[original.nodes.size() - 2];
    std::uintptr_t predecessor_next_slot = 0;
    if (!pointer_slot(
            predecessor.address, original.next_offset, &predecessor_next_slot, error) ||
        !read_expected(
            backend,
            predecessor_next_slot,
            soinfo_address,
            "隐藏前前驱 next",
            error) ||
        !read_expected(
            backend,
            original.tail_pointer_address,
            soinfo_address,
            "隐藏前 linker 尾指针",
            error)) {
        return SolistVisibilityState::Linked;
    }

    if (!write_and_verify(
            backend, predecessor_next_slot, 0, "前驱 next", error) ||
        !write_and_verify(
            backend,
            original.tail_pointer_address,
            predecessor.address,
            "linker 尾指针",
            error) ||
        !verify_hidden_snapshot(backend, soinfo_address, predecessor.address, error)) {
        return rollback_hide(
                   backend,
                   original,
                   predecessor_next_slot,
                   soinfo_address,
                   error)
                   ? SolistVisibilityState::Linked
                   : SolistVisibilityState::Unknown;
    }

    evidence->soinfo_address = soinfo_address;
    return SolistVisibilityState::Hidden;
}

SolistVisibilityState restore_solisted_tail(
    SolistVisibilityBackend* backend,
    const SolistVisibilityEvidence& evidence,
    std::string* error) {
    if (backend == nullptr || evidence.soinfo_address == 0 || error == nullptr) {
        if (error != nullptr) {
            *error = "solist 恢复参数无效";
        }
        return SolistVisibilityState::Hidden;
    }
    error->clear();

    SolistSnapshot hidden;
    if (!snapshot_checked(backend, &hidden, error)) {
        return SolistVisibilityState::Hidden;
    }
    if (std::any_of(
            hidden.nodes.begin(),
            hidden.nodes.end(),
            [&evidence](const SolistNode& node) {
                return node.address == evidence.soinfo_address;
            })) {
        *error = "待恢复 soinfo 已出现在 linker solist 中";
        return SolistVisibilityState::Hidden;
    }

    std::uintptr_t current_tail_next_slot = 0;
    std::uintptr_t hidden_soinfo_next_slot = 0;
    if (!pointer_slot(
            hidden.tail, hidden.next_offset, &current_tail_next_slot, error) ||
        !pointer_slot(
            evidence.soinfo_address,
            hidden.next_offset,
            &hidden_soinfo_next_slot,
            error) ||
        !read_expected(
            backend,
            current_tail_next_slot,
            0,
            "恢复前当前尾节点 next",
            error) ||
        !read_expected(
            backend,
            hidden_soinfo_next_slot,
            0,
            "恢复前隐藏 soinfo next",
            error) ||
        !read_expected(
            backend,
            hidden.tail_pointer_address,
            hidden.tail,
            "恢复前 linker 尾指针",
            error)) {
        return SolistVisibilityState::Hidden;
    }

    if (!write_and_verify(
            backend,
            current_tail_next_slot,
            evidence.soinfo_address,
            "当前尾节点 next",
            error) ||
        !write_and_verify(
            backend,
            hidden.tail_pointer_address,
            evidence.soinfo_address,
            "linker 尾指针",
            error) ||
        !verify_linked_snapshot(backend, evidence.soinfo_address, error)) {
        return rollback_restore(
                   backend,
                   hidden,
                   current_tail_next_slot,
                   evidence.soinfo_address,
                   error)
                   ? SolistVisibilityState::Hidden
                   : SolistVisibilityState::Unknown;
    }

    return SolistVisibilityState::Linked;
}

}  // namespace azlw::loader

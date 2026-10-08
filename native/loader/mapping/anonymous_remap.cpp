// 实现带原址回滚、内容复核和远程描述符收口的 Agent 匿名重映射事务。

#include "anonymous_remap.h"

#include <algorithm>
#include <limits>
#include <ranges>
#include <string_view>
#include <sys/mman.h>
#include <utility>

namespace azlw::loader {
namespace {

constexpr std::uint64_t kMaximumAgentMappingBytes = 256ULL * 1024ULL * 1024ULL;

struct SegmentTransaction final {
    RemoteMapping original;
    std::vector<std::uint8_t> bytes;
    std::uintptr_t anonymous_staging = 0;
    std::uintptr_t memfd_backup = 0;
    bool original_moved = false;
    bool anonymous_installed = false;
};

std::string_view normalize_mapping_name(std::string_view pathname) {
    constexpr std::string_view kDeletedSuffix = " (deleted)";
    if (pathname.ends_with(kDeletedSuffix)) {
        pathname.remove_suffix(kDeletedSuffix.size());
    }
    return pathname;
}

bool same_mapping_name(std::string_view left, std::string_view right) {
    return normalize_mapping_name(left) == normalize_mapping_name(right);
}

bool private_anonymous_mapping(const RemoteMapping& mapping) {
    return mapping.is_private && !mapping.is_shared && mapping.device == "00:00" &&
           mapping.inode == 0 && mapping.pathname.empty();
}

void append_error(std::string* error, std::string detail) {
    if (!error->empty()) {
        *error += "；";
    }
    *error += std::move(detail);
}

bool unmap_if_present(
    AnonymousRemapBackend* backend,
    std::uintptr_t* address,
    std::size_t size,
    std::string* error) {
    if (*address == 0) {
        return true;
    }
    if (!backend->unmap(*address, size)) {
        append_error(
            error,
            "回收远程临时映射失败: " + backend->last_error());
        return false;
    }
    *address = 0;
    return true;
}

bool restore_from_descriptor(
    AnonymousRemapBackend* backend,
    SegmentTransaction* segment,
    int remote_memfd,
    std::string* error) {
    const int preparation_protection = segment->bytes.empty() ? segment->original.protection
                                                               : PROT_READ | PROT_WRITE;
    const std::uintptr_t restored = backend->map(
        segment->original.start,
        segment->original.length,
        preparation_protection,
        MAP_FIXED | MAP_PRIVATE,
        remote_memfd,
        segment->original.offset);
    if (restored != segment->original.start) {
        append_error(
            error,
            "从远程 memfd 恢复原映射失败: " + backend->last_error());
        return false;
    }
    if (!segment->bytes.empty() &&
        !backend->write(restored, segment->bytes.data(), segment->bytes.size())) {
        append_error(error, "恢复原映射内容失败: " + backend->last_error());
        return false;
    }
    if (preparation_protection != segment->original.protection &&
        !backend->protect(restored, segment->original.length, segment->original.protection)) {
        append_error(error, "恢复原映射权限失败: " + backend->last_error());
        return false;
    }
    segment->anonymous_installed = false;
    segment->original_moved = false;
    return true;
}

bool rollback_segments(
    AnonymousRemapBackend* backend,
    std::vector<SegmentTransaction>* segments,
    int remote_memfd,
    std::string* error) {
    bool restored = true;
    for (auto iterator = segments->rbegin(); iterator != segments->rend(); ++iterator) {
        SegmentTransaction& segment = *iterator;
        if (segment.original_moved || segment.anonymous_installed) {
            if (segment.memfd_backup != 0) {
                const std::uintptr_t result = backend->remap(
                    segment.memfd_backup,
                    segment.original.length,
                    segment.original.length,
                    MREMAP_MAYMOVE | MREMAP_FIXED,
                    segment.original.start);
                if (result != segment.original.start) {
                    append_error(error, "原址恢复 memfd 段失败: " + backend->last_error());
                    restored = false;
                } else {
                    segment.memfd_backup = 0;
                    segment.original_moved = false;
                    segment.anonymous_installed = false;
                }
            } else if (!restore_from_descriptor(backend, &segment, remote_memfd, error)) {
                restored = false;
            }
        }
        if (!unmap_if_present(
                backend,
                &segment.anonymous_staging,
                segment.original.length,
                error)) {
            restored = false;
        }
        if (!segment.original_moved &&
            !unmap_if_present(
                backend,
                &segment.memfd_backup,
                segment.original.length,
                error)) {
            restored = false;
        }
    }
    return restored;
}

bool close_remote_descriptor(
    AnonymousRemapBackend* backend,
    int descriptor,
    std::string* error) {
    if (backend->close_descriptor(descriptor)) {
        return true;
    }
    append_error(error, "关闭远程 memfd 失败: " + backend->last_error());
    return false;
}

bool validate_anonymous_result(
    AnonymousRemapBackend* backend,
    const std::vector<SegmentTransaction>& segments,
    std::string* error) {
    const std::vector<RemoteMapping> mappings = backend->mappings();
    if (mappings.empty()) {
        append_error(error, "匿名重映射后无法读取目标 maps");
        return false;
    }
    for (const SegmentTransaction& segment : segments) {
        const auto covering = std::find_if(
            mappings.begin(),
            mappings.end(),
            [&segment](const RemoteMapping& mapping) {
                return mapping.start <= segment.original.start &&
                       mapping.end >= segment.original.end &&
                       mapping.protection == segment.original.protection &&
                       private_anonymous_mapping(mapping);
            });
        if (covering == mappings.end()) {
            append_error(error, "匿名段地址、权限或身份复核失败");
            return false;
        }
        if (!segment.bytes.empty()) {
            std::vector<std::uint8_t> actual(segment.bytes.size());
            if (!backend->read(segment.original.start, actual.data(), actual.size()) ||
                actual != segment.bytes) {
                append_error(error, "匿名段内容复核失败");
                return false;
            }
        }
    }
    return true;
}

bool validate_memfd_restored(
    AnonymousRemapBackend* backend,
    const std::string& mapping_name,
    const std::vector<SegmentTransaction>& segments,
    std::string* error) {
    const std::vector<RemoteMapping> mappings = backend->mappings();
    for (const SegmentTransaction& segment : segments) {
        const auto restored = std::find_if(
            mappings.begin(),
            mappings.end(),
            [&segment, &mapping_name](const RemoteMapping& mapping) {
                if (mapping.start > segment.original.start ||
                    mapping.end < segment.original.end ||
                    mapping.protection != segment.original.protection ||
                    !same_mapping_name(mapping.pathname, mapping_name)) {
                    return false;
                }
                const std::uintptr_t delta = segment.original.start - mapping.start;
                return mapping.offset <= std::numeric_limits<std::uintptr_t>::max() - delta &&
                       mapping.offset + delta == segment.original.offset;
            });
        if (restored == mappings.end()) {
            append_error(error, "回滚后原 memfd 映射身份复核失败");
            return false;
        }
    }
    return true;
}

bool rollback_preserving_descriptor(
    AnonymousRemapBackend* backend,
    const std::string& mapping_name,
    std::vector<SegmentTransaction>* segments,
    int remote_memfd,
    std::string* error) {
    if (rollback_segments(backend, segments, remote_memfd, error) &&
        validate_memfd_restored(backend, mapping_name, *segments, error)) {
        append_error(error, "匿名化已回滚，远程 memfd 保留供目标重启清理");
        return true;
    }
    append_error(error, "回滚未得到完整证明，保留远程 memfd 供目标重启清理");
    return false;
}

}  // namespace

AnonymousRemapResult remap_memfd_segments_anonymously(
    AnonymousRemapBackend* backend,
    const std::string& mapping_name,
    std::uintptr_t agent_start,
    std::size_t agent_size,
    int remote_memfd,
    AnonymousRemapEvidence* evidence,
    std::string* error) {
    if (backend == nullptr || evidence == nullptr || error == nullptr || mapping_name.empty() ||
        agent_start == 0 || agent_size == 0 || remote_memfd < 0 ||
        agent_start > std::numeric_limits<std::uintptr_t>::max() - agent_size) {
        if (error != nullptr) {
            *error = "匿名重映射参数无效";
        }
        return AnonymousRemapResult::Unknown;
    }
    *evidence = {};
    error->clear();
    const std::uintptr_t agent_end = agent_start + agent_size;
    std::vector<RemoteMapping> selected;
    for (const RemoteMapping& mapping : backend->mappings()) {
        if (!same_mapping_name(mapping.pathname, mapping_name)) {
            continue;
        }
        if (mapping.start < agent_start || mapping.end > agent_end || !mapping.is_private ||
            mapping.is_shared || mapping.start >= mapping.end ||
            mapping.length != mapping.end - mapping.start) {
            *error = "Agent memfd 映射超出加载区间或属性无效";
            return AnonymousRemapResult::Restored;
        }
        selected.push_back(mapping);
    }
    if (selected.empty()) {
        *error = "没有找到需要匿名化的 Agent memfd 映射";
        return AnonymousRemapResult::Restored;
    }
    std::sort(
        selected.begin(),
        selected.end(),
        [](const RemoteMapping& left, const RemoteMapping& right) {
            return left.start < right.start;
        });
    std::uint64_t total_bytes = 0;
    std::uintptr_t previous_end = 0;
    for (const RemoteMapping& mapping : selected) {
        if (previous_end > mapping.start || mapping.length > kMaximumAgentMappingBytes ||
            total_bytes > kMaximumAgentMappingBytes - mapping.length) {
            *error = "Agent memfd 映射重叠或超过 256 MiB 上限";
            return AnonymousRemapResult::Restored;
        }
        previous_end = mapping.end;
        total_bytes += mapping.length;
    }

    std::vector<SegmentTransaction> segments;
    segments.reserve(selected.size());
    for (RemoteMapping& mapping : selected) {
        SegmentTransaction segment{};
        segment.original = std::move(mapping);
        if (segment.original.protection != PROT_NONE) {
            segment.bytes.resize(segment.original.length);
            if (!backend->read(
                    segment.original.start,
                    segment.bytes.data(),
                    segment.bytes.size())) {
                append_error(error, "备份 Agent 段内容失败: " + backend->last_error());
                const bool restored = rollback_preserving_descriptor(
                    backend, mapping_name, &segments, remote_memfd, error);
                return restored ? AnonymousRemapResult::Restored
                                : AnonymousRemapResult::Unknown;
            }
        }
        const int staging_protection =
            segment.bytes.empty() ? PROT_NONE : PROT_READ | PROT_WRITE;
        segment.anonymous_staging = backend->map(
            0,
            segment.original.length,
            staging_protection,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0);
        if (segment.anonymous_staging == 0) {
            append_error(error, "创建匿名候选段失败: " + backend->last_error());
            segments.push_back(std::move(segment));
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        if (!segment.bytes.empty() &&
            !backend->write(
                segment.anonymous_staging,
                segment.bytes.data(),
                segment.bytes.size())) {
            append_error(error, "写入匿名候选段失败: " + backend->last_error());
            segments.push_back(std::move(segment));
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        if (staging_protection != segment.original.protection &&
            !backend->protect(
                segment.anonymous_staging,
                segment.original.length,
                segment.original.protection)) {
            append_error(error, "设置匿名候选段权限失败: " + backend->last_error());
            segments.push_back(std::move(segment));
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        segment.memfd_backup = backend->map(
            0,
            segment.original.length,
            PROT_NONE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0);
        if (segment.memfd_backup == 0) {
            append_error(error, "预留 memfd 回滚地址失败: " + backend->last_error());
            segments.push_back(std::move(segment));
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        segments.push_back(std::move(segment));
    }

    for (SegmentTransaction& segment : segments) {
        const std::uintptr_t backup = backend->remap(
            segment.original.start,
            segment.original.length,
            segment.original.length,
            MREMAP_MAYMOVE | MREMAP_FIXED,
            segment.memfd_backup);
        if (backup != segment.memfd_backup) {
            append_error(error, "移动原 memfd 段到回滚区失败: " + backend->last_error());
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        segment.original_moved = true;
        const std::uintptr_t installed = backend->remap(
            segment.anonymous_staging,
            segment.original.length,
            segment.original.length,
            MREMAP_MAYMOVE | MREMAP_FIXED,
            segment.original.start);
        if (installed != segment.original.start) {
            append_error(error, "安装原址匿名段失败: " + backend->last_error());
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        segment.anonymous_staging = 0;
        segment.anonymous_installed = true;
    }

    if (!validate_anonymous_result(backend, segments, error)) {
        const bool restored = rollback_preserving_descriptor(
            backend, mapping_name, &segments, remote_memfd, error);
        return restored ? AnonymousRemapResult::Restored
                        : AnonymousRemapResult::Unknown;
    }

    for (SegmentTransaction& segment : segments) {
        if (!unmap_if_present(
                backend,
                &segment.memfd_backup,
                segment.original.length,
                error)) {
            const bool restored = rollback_preserving_descriptor(
                backend, mapping_name, &segments, remote_memfd, error);
            return restored ? AnonymousRemapResult::Restored
                            : AnonymousRemapResult::Unknown;
        }
        segment.original_moved = false;
    }
    const bool memfd_mapping_remains =
        std::ranges::any_of(backend->mappings(), [&mapping_name](const RemoteMapping& mapping) {
            return same_mapping_name(mapping.pathname, mapping_name);
        });
    if (memfd_mapping_remains) {
        append_error(error, "提交后仍存在 Agent memfd 映射");
        const bool restored = rollback_preserving_descriptor(
            backend, mapping_name, &segments, remote_memfd, error);
        return restored ? AnonymousRemapResult::Restored
                        : AnonymousRemapResult::Unknown;
    }
    if (!close_remote_descriptor(backend, remote_memfd, error)) {
        const bool restored = rollback_preserving_descriptor(
            backend, mapping_name, &segments, remote_memfd, error);
        return restored ? AnonymousRemapResult::Restored
                        : AnonymousRemapResult::Unknown;
    }

    evidence->segment_count = segments.size();
    evidence->byte_count = total_bytes;
    return AnonymousRemapResult::Applied;
}

}  // namespace azlw::loader

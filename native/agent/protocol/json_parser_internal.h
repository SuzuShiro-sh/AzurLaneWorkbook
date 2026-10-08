// 声明请求解析共享的文档视图与字段遍历原语，文档所有权保留在顶层解析调用中。

#pragma once

#include <algorithm>
#include <charconv>
#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

// 共享 token 布局；jsmn 的解析实现仅在顶层请求解析文件内编译。
#define JSMN_PARENT_LINKS
#define JSMN_HEADER
#include <jsmn.h>
#undef JSMN_HEADER

#include "protocol_types.h"

namespace azlw::agent::json_parser_internal {

/// 保留原始文本及指向其区间的 jsmn token 集合。
struct ParsedJson final {
    std::string_view source;
    std::vector<jsmntok_t> tokens;
};

/// 从原始文档取得 token 对应的零拷贝文本视图。
inline std::string_view token_text(const ParsedJson& parsed, const jsmntok_t& token) {
    return parsed.source.substr(
        static_cast<std::size_t>(token.start),
        static_cast<std::size_t>(token.end - token.start));
}

/// 返回当前 token 子树之后的首个 token 下标。
inline int subtree_end(const ParsedJson& parsed, int token_index) {
    const int boundary = parsed.tokens[static_cast<std::size_t>(token_index)].end;
    int cursor = token_index + 1;
    while (cursor < static_cast<int>(parsed.tokens.size()) &&
           parsed.tokens[static_cast<std::size_t>(cursor)].start < boundary) {
        ++cursor;
    }
    return cursor;
}

/// 按原始顺序遍历对象字段，使调用方能够识别未知键和重复键。
template <typename Visitor>
bool visit_object(const ParsedJson& parsed, int object_index, Visitor visitor, std::string* error) {
    const jsmntok_t& object = parsed.tokens[static_cast<std::size_t>(object_index)];
    if (object.type != JSMN_OBJECT) {
        *error = "字段必须是 JSON 对象";
        return false;
    }

    int cursor = object_index + 1;
    while (cursor < static_cast<int>(parsed.tokens.size()) &&
           parsed.tokens[static_cast<std::size_t>(cursor)].start < object.end) {
        const jsmntok_t& key = parsed.tokens[static_cast<std::size_t>(cursor)];
        if (key.type != JSMN_STRING || cursor + 1 >= static_cast<int>(parsed.tokens.size())) {
            *error = "JSON 对象字段结构无效";
            return false;
        }
        const int value_index = cursor + 1;
        if (!visitor(token_text(parsed, key), value_index)) {
            return false;
        }
        cursor = subtree_end(parsed, value_index);
    }
    return true;
}

/// 按原始顺序遍历数组的直接元素，嵌套对象由调用方继续严格解析。
template <typename Visitor>
bool visit_array(const ParsedJson& parsed, int array_index, Visitor visitor, std::string* error) {
    const jsmntok_t& array = parsed.tokens[static_cast<std::size_t>(array_index)];
    if (array.type != JSMN_ARRAY) {
        *error = "字段必须是 JSON 数组";
        return false;
    }
    std::size_t element_index = 0;
    int cursor = array_index + 1;
    while (cursor < static_cast<int>(parsed.tokens.size()) &&
           parsed.tokens[static_cast<std::size_t>(cursor)].start < array.end) {
        if (!visitor(element_index, cursor)) {
            return false;
        }
        ++element_index;
        cursor = subtree_end(parsed, cursor);
    }
    return true;
}

/// 只接受规范十进制无符号整数，并同时执行调用方上限检查。
inline bool parse_unsigned(
    const ParsedJson& parsed,
    int token_index,
    std::uint64_t maximum,
    std::uint64_t* value) {
    const jsmntok_t& token = parsed.tokens[static_cast<std::size_t>(token_index)];
    if (token.type != JSMN_PRIMITIVE) {
        return false;
    }
    const std::string_view text = token_text(parsed, token);
    if (text.empty() || (text.size() > 1 && text.front() == '0') ||
        !std::all_of(text.begin(), text.end(), [](char current) {
            return current >= '0' && current <= '9';
        })) {
        return false;
    }
    std::uint64_t parsed_value = 0;
    const auto result = std::from_chars(text.data(), text.data() + text.size(), parsed_value);
    if (result.ec != std::errc{} || result.ptr != text.data() + text.size() || parsed_value > maximum) {
        return false;
    }
    *value = parsed_value;
    return true;
}

bool parse_execute_equipment_command_payload(
    const ParsedJson& parsed,
    int payload_index,
    EquipmentCommand* command,
    AgentError* error);

bool parse_equipment_command_lookup_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::string* command_id,
    AgentError* error);

}  // namespace azlw::agent::json_parser_internal

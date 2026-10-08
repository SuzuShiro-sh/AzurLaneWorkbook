// 实现有界 Lua 配置值读取、循环识别和确定性表结构规范化。

#include "lua_value.h"

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "lua_reader.h"

namespace azlw::agent {
namespace {

/// Lua 5.1 类型编号转换为稳定文本，供不支持的值和键进入诊断结构。
std::string lua_type_name(int type) {
    switch (type) {
        case kLuaTypeNil:
            return "nil";
        case kLuaTypeBoolean:
            return "boolean";
        case kLuaTypeLightUserData:
            return "lightuserdata";
        case kLuaTypeNumber:
            return "number";
        case kLuaTypeString:
            return "string";
        case kLuaTypeTable:
            return "table";
        case kLuaTypeFunction:
            return "function";
        case kLuaTypeUserData:
            return "userdata";
        case kLuaTypeThread:
            return "thread";
        default:
            return "type_" + std::to_string(type);
    }
}

/// 路径只用于诊断，不作为可重新执行的 Lua 表达式。
std::string child_path(std::string_view path, const LuaValueKey& key) {
    std::string result(path);
    result.push_back('[');
    if (key.kind == LuaValueKey::Kind::Number) {
        result += std::to_string(key.number);
    } else if (key.kind == LuaValueKey::Kind::String) {
        result += key.text;
    } else {
        result += '<' + key.lua_type + '>';
    }
    result.push_back(']');
    return result;
}

LuaValueKey number_key(double number) {
    LuaValueKey key;
    key.kind = LuaValueKey::Kind::Number;
    key.number = number;
    return key;
}

LuaValueKey string_key(std::string text) {
    LuaValueKey key;
    key.kind = LuaValueKey::Kind::String;
    key.text = std::move(text);
    return key;
}

LuaValueKey unsupported_key(std::string lua_type) {
    LuaValueKey key;
    key.kind = LuaValueKey::Kind::Unsupported;
    key.lua_type = std::move(lua_type);
    return key;
}

LuaValue scalar_value(LuaValueKind kind) {
    LuaValue value;
    value.kind = kind;
    return value;
}

LuaValue boolean_value(bool boolean) {
    LuaValue value = scalar_value(LuaValueKind::Boolean);
    value.boolean = boolean;
    return value;
}

LuaValue number_value(double number) {
    LuaValue value = scalar_value(LuaValueKind::Number);
    value.number = number;
    return value;
}

LuaValue text_value(LuaValueKind kind, std::string text) {
    LuaValue value = scalar_value(kind);
    value.text = std::move(text);
    return value;
}

LuaValue truncated_table(std::string reason) {
    LuaValue value = scalar_value(LuaValueKind::Table);
    value.truncated = true;
    value.reason = std::move(reason);
    return value;
}

/// 读取表键时只允许有限数字和 UTF-8 字符串直接参与排序。
LuaValueKey read_key(
    const LuaApi& api,
    lua_State* state,
    int index,
    const LuaValueLimits& limits,
    std::string_view path,
    std::vector<std::string>* errors) {
    const int type = api.type(state, index);
    if (type == kLuaTypeNumber) {
        const double number = api.to_number(state, index);
        if (std::isfinite(number)) {
            return number_key(number);
        }
        errors->push_back(std::string(path) + ": 表键不是有限数值");
        return unsupported_key("number");
    }
    if (type == kLuaTypeString) {
        std::string text_error;
        std::optional<std::string> text = read_lua_text(
            api,
            state,
            index,
            limits.maximum_string_bytes,
            true,
            &text_error);
        if (text.has_value()) {
            return string_key(std::move(*text));
        }
        errors->push_back(std::string(path) + ": 表键无效: " + text_error);
        return unsupported_key("string");
    }
    const std::string name = lua_type_name(type);
    errors->push_back(std::string(path) + ": 不支持的表键类型 " + name);
    return unsupported_key(name);
}

/// 混合表必须在同一运行与不同运行间保持完全一致的键顺序。
bool key_less(const LuaValueKey& left, const LuaValueKey& right) {
    if (left.kind != right.kind) {
        return left.kind < right.kind;
    }
    switch (left.kind) {
        case LuaValueKey::Kind::Number:
            return left.number < right.number;
        case LuaValueKey::Kind::String:
            return left.text < right.text;
        case LuaValueKey::Kind::Unsupported:
            return left.lua_type < right.lua_type;
    }
    return false;
}

/// 根据完整键集合选择 JSON 数组、对象或显式混合表表示。
void normalize_table(LuaValue* value) {
    std::vector<std::size_t> order(value->keys.size());
    for (std::size_t index = 0; index < order.size(); ++index) {
        order[index] = index;
    }
    std::stable_sort(order.begin(), order.end(), [value](std::size_t left, std::size_t right) {
        return key_less(value->keys[left], value->keys[right]);
    });

    std::vector<LuaValueKey> sorted_keys;
    std::vector<LuaValue> sorted_values;
    sorted_keys.reserve(order.size());
    sorted_values.reserve(order.size());
    for (const std::size_t index : order) {
        sorted_keys.push_back(std::move(value->keys[index]));
        sorted_values.push_back(std::move(value->values[index]));
    }
    value->keys = std::move(sorted_keys);
    value->values = std::move(sorted_values);

    bool all_strings = true;
    bool contiguous_numbers = !value->truncated;
    for (std::size_t index = 0; index < value->keys.size(); ++index) {
        const LuaValueKey& key = value->keys[index];
        all_strings = all_strings && key.kind == LuaValueKey::Kind::String;
        contiguous_numbers = contiguous_numbers && key.kind == LuaValueKey::Kind::Number &&
                             key.number == static_cast<double>(index + 1);
    }
    if (contiguous_numbers) {
        value->kind = LuaValueKind::Array;
        value->keys.clear();
    } else if (all_strings) {
        value->kind = LuaValueKind::Object;
    } else {
        value->kind = LuaValueKind::Table;
    }
}

/// 递归上下文只保存当前祖先表指针，兄弟分支共享同一表仍可各自展开。
struct ReadContext final {
    const LuaApi& api;
    lua_State* state;
    const LuaValueLimits& limits;
    std::vector<const void*> ancestors;
    std::vector<std::string> errors;
};

LuaValue read_value(
    ReadContext* context,
    int index,
    std::string_view path,
    std::size_t depth) {
    const int type = context->api.type(context->state, index);
    if (type == kLuaTypeNil) {
        return scalar_value(LuaValueKind::Null);
    }
    if (type == kLuaTypeBoolean) {
        return boolean_value(context->api.to_boolean(context->state, index) != 0);
    }
    if (type == kLuaTypeNumber) {
        const double number = context->api.to_number(context->state, index);
        if (std::isfinite(number)) {
            return number_value(number);
        }
        context->errors.push_back(std::string(path) + ": 数值不是有限值");
        return text_value(LuaValueKind::Unsupported, "number");
    }
    if (type == kLuaTypeString) {
        std::string text_error;
        std::optional<std::string> text = read_lua_text(
            context->api,
            context->state,
            index,
            context->limits.maximum_string_bytes,
            true,
            &text_error);
        if (text.has_value()) {
            return text_value(LuaValueKind::String, std::move(*text));
        }
        context->errors.push_back(std::string(path) + ": 字符串无效: " + text_error);
        return text_value(LuaValueKind::Unsupported, "string");
    }
    if (type != kLuaTypeTable) {
        const std::string name = lua_type_name(type);
        context->errors.push_back(std::string(path) + ": 不支持的 Lua 类型 " + name);
        return text_value(LuaValueKind::Unsupported, name);
    }
    if (depth == 0) {
        context->errors.push_back(std::string(path) + ": 已达到最大递归深度");
        return truncated_table("max_depth");
    }

    context->api.ensure_stack(context->state, 4);
    const int original_top = context->api.get_top(context->state);
    const int table_index = context->api.stable_stack_index(context->state, index);
    const void* identity = context->api.to_pointer(context->state, table_index);
    if (identity != nullptr &&
        std::find(context->ancestors.begin(), context->ancestors.end(), identity) !=
            context->ancestors.end()) {
        context->errors.push_back(std::string(path) + ": 检测到循环表引用");
        return truncated_table("cycle");
    }
    if (identity != nullptr) {
        context->ancestors.push_back(identity);
    }

    LuaValue value = scalar_value(LuaValueKind::Table);
    std::size_t visited_entries = 0;
    context->api.push_nil(context->state);
    while (context->api.next(context->state, table_index) != 0) {
        if (visited_entries >= context->limits.maximum_entries) {
            value.truncated = true;
            value.reason = "max_entries";
            context->errors.push_back(
                std::string(path) + ": 表条目数超过 " +
                std::to_string(context->limits.maximum_entries));
            context->api.set_top(context->state, original_top);
            break;
        }
        ++visited_entries;
        LuaValueKey key = read_key(
            context->api,
            context->state,
            -2,
            context->limits,
            path,
            &context->errors);
        if (key.kind == LuaValueKey::Kind::Unsupported) {
            value.truncated = true;
            if (value.reason.empty()) {
                value.reason = "unsupported_key";
            }
            context->api.set_top(
                context->state,
                context->api.get_top(context->state) - 1);
            continue;
        }
        value.values.push_back(read_value(
            context,
            -1,
            child_path(path, key),
            depth - 1));
        value.keys.push_back(std::move(key));
        context->api.set_top(context->state, context->api.get_top(context->state) - 1);
    }
    context->api.set_top(context->state, original_top);
    if (identity != nullptr) {
        context->ancestors.pop_back();
    }
    normalize_table(&value);
    return value;
}

}  // namespace

LuaValueResult read_lua_value(
    const LuaApi& api,
    lua_State* state,
    int index,
    const LuaValueLimits& limits) {
    LuaValueResult result;
    if (!api.ready() || state == nullptr) {
        result.read_errors.push_back("Lua 状态或必需函数表尚未就绪");
        return result;
    }
    if (limits.maximum_depth == 0 || limits.maximum_entries == 0 ||
        limits.maximum_string_bytes == 0) {
        result.read_errors.push_back("Lua 配置值读取限制必须全部大于零");
        return result;
    }
    const LuaStackGuard stack_guard(api, state);
    ReadContext context{api, state, limits, {}, {}};
    result.value = read_value(&context, index, "$", limits.maximum_depth);
    result.read_errors = std::move(context.errors);
    result.complete = result.read_errors.empty();
    return result;
}

LuaValue merge_lua_objects(const LuaValue& base, const LuaValue& override_value) {
    std::map<std::string, LuaValue> merged;
    const auto copy_fields = [&merged](const LuaValue& source) {
        if (source.kind != LuaValueKind::Object || source.keys.size() != source.values.size()) {
            return;
        }
        for (std::size_t index = 0; index < source.keys.size(); ++index) {
            const LuaValueKey& key = source.keys[index];
            if (key.kind == LuaValueKey::Kind::String) {
                merged[key.text] = source.values[index];
            }
        }
    };
    copy_fields(base);
    copy_fields(override_value);

    LuaValue result;
    result.kind = LuaValueKind::Object;
    result.truncated = base.truncated || override_value.truncated;
    if (result.truncated) {
        result.reason = !override_value.reason.empty() ? override_value.reason : base.reason;
    }
    result.keys.reserve(merged.size());
    result.values.reserve(merged.size());
    for (auto& [key, value] : merged) {
        result.keys.push_back(LuaValueKey{
            .kind = LuaValueKey::Kind::String,
            .number = 0.0,
            .text = key,
            .lua_type = {},
        });
        result.values.push_back(std::move(value));
    }
    return result;
}

}  // namespace azlw::agent

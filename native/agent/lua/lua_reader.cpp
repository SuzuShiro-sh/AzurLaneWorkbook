// 实现运行态快照共享的 Lua 栈恢复、严格标量读取和错误分类。

#include "lua_reader.h"

#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <utility>

namespace azlw::agent {
namespace {

/// Lua number 以双精度表示时仍能精确承载的最大非负整数和文本上限。
constexpr std::uint64_t kMaximumExactLuaInteger = 9'007'199'254'740'991ULL;
constexpr std::size_t kMaximumLuaTextBytes = 512;
constexpr std::string_view kM02InitializationSource = "Support/Helpers/M02:0";
constexpr std::string_view kM02InitializationFailure =
    "attempt to index field 'm02' (a nil value)";

/// 拒绝截断、过长编码、代理区和超出 Unicode 范围的 UTF-8。
bool valid_utf8(std::string_view value) {
    std::size_t index = 0;
    while (index < value.size()) {
        const auto first = static_cast<unsigned char>(value[index]);
        std::size_t continuation = 0;
        std::uint32_t codepoint = 0;
        if (first <= 0x7f) {
            continuation = 0;
            codepoint = first;
        } else if ((first & 0xe0) == 0xc0) {
            continuation = 1;
            codepoint = first & 0x1f;
        } else if ((first & 0xf0) == 0xe0) {
            continuation = 2;
            codepoint = first & 0x0f;
        } else if ((first & 0xf8) == 0xf0) {
            continuation = 3;
            codepoint = first & 0x07;
        } else {
            return false;
        }
        if (index + continuation >= value.size()) {
            return false;
        }
        for (std::size_t offset = 1; offset <= continuation; ++offset) {
            const auto current = static_cast<unsigned char>(value[index + offset]);
            if ((current & 0xc0) != 0x80) {
                return false;
            }
            codepoint = (codepoint << 6) | (current & 0x3f);
        }
        if ((continuation == 1 && codepoint < 0x80) ||
            (continuation == 2 && codepoint < 0x800) ||
            (continuation == 3 && codepoint < 0x10000) ||
            codepoint > 0x10ffff || (codepoint >= 0xd800 && codepoint <= 0xdfff)) {
            return false;
        }
        index += continuation + 1;
    }
    return true;
}

/// 读取 Lua 错误对象并为字段或方法调用补充上下文。
std::string call_failure_detail(
    const LuaApi& api,
    lua_State* state,
    std::string message) {
    std::string detail_error;
    const std::optional<std::string> detail =
        read_lua_text(api, state, -1, 4096, true, &detail_error);
    if (detail.has_value() && !detail->empty()) {
        message += ": " + *detail;
    }
    return message;
}

/// 按固定类型压入函数参数；StackValue 必须由调用方预先转换成稳定索引。
void push_call_argument(const LuaApi& api, lua_State* state, const LuaCallArgument& argument) {
    switch (argument.kind) {
        case LuaCallArgument::Kind::Nil:
            api.push_nil(state);
            break;
        case LuaCallArgument::Kind::Boolean:
            api.push_boolean(state, argument.boolean_value ? 1 : 0);
            break;
        case LuaCallArgument::Kind::Number:
            api.push_number(state, argument.number_value);
            break;
        case LuaCallArgument::Kind::String:
            api.push_string(state, argument.string_value);
            break;
        case LuaCallArgument::Kind::StackValue:
            api.push_value(state, argument.stack_index);
            break;
    }
}

/// 调用已经位于栈上的函数，统一追加参数并捕获 Lua 错误。
bool call_function(
    const LuaApi& api,
    lua_State* state,
    std::span<const LuaCallArgument> arguments,
    int leading_argument_count,
    std::string context,
    std::string* error) {
    api.ensure_stack(state, static_cast<int>(arguments.size()) + 4);
    for (const LuaCallArgument& argument : arguments) {
        push_call_argument(api, state, argument);
    }
    const int argument_count = leading_argument_count + static_cast<int>(arguments.size());
    if (api.protected_call(state, argument_count, 1, 0) != 0) {
        *error = call_failure_detail(api, state, std::move(context));
        return false;
    }
    return true;
}

}  // namespace

LuaCallArgument LuaCallArgument::nil() {
    return LuaCallArgument{.kind = Kind::Nil};
}

LuaCallArgument LuaCallArgument::boolean(bool value) {
    return LuaCallArgument{.kind = Kind::Boolean, .boolean_value = value};
}

LuaCallArgument LuaCallArgument::number(double value) {
    return LuaCallArgument{.kind = Kind::Number, .number_value = value};
}

LuaCallArgument LuaCallArgument::string(const char* value) {
    return LuaCallArgument{.kind = Kind::String, .string_value = value};
}

LuaCallArgument LuaCallArgument::stack_value(int stable_index) {
    return LuaCallArgument{.kind = Kind::StackValue, .stack_index = stable_index};
}

LuaStackGuard::LuaStackGuard(const LuaApi& api, lua_State* state)
    : api_(api), state_(state), top_(api.get_top(state)) {}

LuaStackGuard::~LuaStackGuard() {
    api_.set_top(state_, top_);
}

int LuaStackGuard::top() const noexcept {
    return top_;
}

AgentError make_lua_error(std::string code, std::string message, std::string retry) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.lua",
        .message = std::move(message),
        .retry = std::move(retry),
        .session_effect = "unchanged",
    };
}

std::string lua_failure_detail(
    const LuaApi& api,
    lua_State* state,
    std::string message) {
    return call_failure_detail(api, state, std::move(message));
}

bool is_lua_proxy_initialization_pending(std::string_view detail) {
    return detail.find(kM02InitializationSource) != std::string_view::npos &&
           detail.find(kM02InitializationFailure) != std::string_view::npos;
}

bool push_lua_proxy(
    const LuaApi& api,
    lua_State* state,
    std::string_view proxy_name,
    std::string invalid_code,
    AgentError* error) {
    api.push_string(state, "getProxy");
    api.raw_get(state, kLuaGlobalsIndex);
    if (api.type(state, -1) != kLuaTypeFunction) {
        *error = make_lua_error(
            "lua_get_proxy_missing",
            "Lua 全局表尚未提供 getProxy 函数",
            "same_request");
        return false;
    }

    const std::string proxy(proxy_name);
    api.push_string(state, proxy.c_str());
    api.raw_get(state, kLuaGlobalsIndex);
    if (api.protected_call(state, 1, 1, 0) != 0) {
        std::string detail_error;
        const std::optional<std::string> detail =
            read_lua_string(api, state, -1, &detail_error);
        const std::string retry =
            detail.has_value() && is_lua_proxy_initialization_pending(*detail)
                ? "same_request"
                : "never";
        *error = make_lua_error(
            "lua_get_proxy_failed",
            detail.has_value() ? "读取 " + proxy + " 失败: " + *detail
                               : "读取 " + proxy + " 失败",
            retry);
        return false;
    }

    const int proxy_type = api.type(state, -1);
    if (proxy_type != kLuaTypeTable && proxy_type != kLuaTypeUserData) {
        *error = make_lua_error(
            std::move(invalid_code),
            "getProxy(" + proxy + ") 尚未返回 table 或 userdata",
            "same_request");
        return false;
    }
    return true;
}

bool push_lua_global_table(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::string* error) {
    api.push_string(state, table_name);
    api.raw_get(state, kLuaGlobalsIndex);
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "Lua 全局表缺少 " + std::string(table_name);
        return false;
    }
    return true;
}

bool push_lua_pg_config_table(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "pg", error)) {
        api.set_top(state, top);
        return false;
    }
    const int pg_index = api.get_top(state);
    if (!push_lua_table_field(api, state, pg_index, table_name, error)) {
        *error = "读取 pg." + std::string(table_name) + " 失败: " + *error;
        api.set_top(state, top);
        return false;
    }
    return true;
}

bool push_lua_table_field(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    const char* field_name,
    std::string* error) {
    const int top = api.get_top(state);
    const int stable_table = api.stable_stack_index(state, table_index);
    if (api.type(state, stable_table) != kLuaTypeTable) {
        *error = "字段 " + std::string(field_name) + " 的父值不是 table";
        return false;
    }
    if (api.get_field_protected(state, stable_table, field_name) != 0) {
        *error = lua_failure_detail(
            api,
            state,
            "读取 table 字段 " + std::string(field_name) + " 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "table 字段 " + std::string(field_name) + " 不是 table";
        api.set_top(state, top);
        return false;
    }
    return true;
}

bool push_lua_materialized_table(
    const LuaApi& api,
    lua_State* state,
    int source_index,
    std::span<const char* const> fields,
    std::string* error) {
    const int top = api.get_top(state);
    const int stable_source = api.stable_stack_index(state, source_index);
    if (api.type(state, stable_source) != kLuaTypeTable) {
        *error = "待物化配置不是 table";
        return false;
    }
    struct Materialize final {
        std::span<const char* const> fields;
        const char* current_field = nullptr;
    };
    Materialize materialize{fields};
    const int status = api.run_protected(
        state,
        [](const LuaApi& inner, lua_State* lua, void* data) {
            auto* input = static_cast<Materialize*>(data);
            inner.create_table_unchecked(lua, 0, static_cast<int>(input->fields.size()));
            for (const char* field : input->fields) {
                input->current_field = field;
                inner.push_string_unchecked(lua, field);
                inner.get_table(lua, 1);
                inner.set_field_unchecked(lua, 2, field);
            }
            return 1;
        }, &materialize, {&stable_source, 1});
    if (status != 0) {
        const std::string context = materialize.current_field == nullptr
            ? "创建配置物化表失败"
            : "物化配置字段 " + std::string(materialize.current_field) + " 失败";
        *error = lua_failure_detail(api, state, context);
        api.set_top(state, top);
        return false;
    }
    return true;
}

namespace {

std::optional<std::string> read_lua_static_name_with_argument(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    const char* function_name,
    const LuaCallArgument& argument,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, table_name, error)) {
        api.set_top(state, top);
        return std::nullopt;
    }
    const int table_index = api.get_top(state);
    const std::array arguments{argument};
    if (!push_lua_table_function_result(
            api,
            state,
            table_index,
            function_name,
            arguments,
            error)) {
        api.set_top(state, top);
        return std::nullopt;
    }
    std::optional<std::string> result = read_lua_text(api, state, -1, 512, false, error);
    api.set_top(state, top);
    if (!result.has_value()) {
        *error = std::string(table_name) + "." + function_name + " 返回值无效: " + *error;
    }
    return result;
}

}  // namespace

std::optional<std::string> read_lua_static_name(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    const char* function_name,
    std::uint64_t identifier,
    std::string* error) {
    return read_lua_static_name_with_argument(
        api,
        state,
        table_name,
        function_name,
        LuaCallArgument::number(static_cast<double>(identifier)),
        error);
}

std::optional<std::string> read_lua_static_name(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    const char* function_name,
    const char* key,
    std::string* error) {
    return read_lua_static_name_with_argument(
        api, state, table_name, function_name, LuaCallArgument::string(key), error);
}

std::optional<std::string> read_lua_pg_config_text(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::uint64_t identifier,
    const char* field_name,
    std::string* error) {
    const int top = api.get_top(state);
    if (identifier > kMaximumExactLuaInteger) {
        *error = "pg 配置 ID 超出 Lua number 精确整数范围";
        return std::nullopt;
    }
    const std::string row_path =
        "pg." + std::string(table_name) + "[" + std::to_string(identifier) + "]";
    if (!push_lua_global_table(api, state, "pg", error)) {
        api.set_top(state, top);
        return std::nullopt;
    }
    const int pg_index = api.get_top(state);
    if (!push_lua_table_field(api, state, pg_index, table_name, error)) {
        *error = "读取 pg." + std::string(table_name) + " 失败: " + *error;
        api.set_top(state, top);
        return std::nullopt;
    }
    const int config_index = api.get_top(state);
    if (api.get_number_index_protected(
            state, config_index, static_cast<double>(identifier)) != 0) {
        *error = lua_failure_detail(api, state, "读取 " + row_path + " 失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = row_path + " 不是 table";
        api.set_top(state, top);
        return std::nullopt;
    }
    std::optional<std::string> result =
        read_lua_text_field(api, state, -1, field_name, 512, false, error);
    api.set_top(state, top);
    if (!result.has_value()) {
        *error = row_path + "." + field_name + " 无效: " + *error;
    }
    return result;
}

std::optional<std::string> read_lua_string(
    const LuaApi& api,
    lua_State* state,
    int index,
    std::string* error) {
    return read_lua_text(api, state, index, kMaximumLuaTextBytes, false, error);
}

std::optional<std::string> read_lua_text(
    const LuaApi& api,
    lua_State* state,
    int index,
    std::size_t maximum_bytes,
    bool allow_empty,
    std::string* error) {
    if (api.type(state, index) != kLuaTypeString) {
        *error = "Lua 值不是字符串";
        return std::nullopt;
    }
    std::size_t length = 0;
    const char* value = api.to_string(state, index, &length);
    if (value == nullptr) {
        *error = "Lua 值不是字符串";
        return std::nullopt;
    }
    if ((!allow_empty && length == 0) || length > maximum_bytes) {
        *error = allow_empty
                     ? "Lua 字符串长度超过 " + std::to_string(maximum_bytes) + " 字节"
                     : "Lua 字符串长度超出 1 至 " + std::to_string(maximum_bytes) + " 字节";
        return std::nullopt;
    }
    const std::string result(value, length);
    if (!valid_utf8(result)) {
        *error = "Lua 字符串不是有效 UTF-8";
        return std::nullopt;
    }
    return result;
}

LuaNumber read_lua_number(const LuaApi& api, lua_State* state, int index) {
    if (api.type(state, index) == kLuaTypeNil) {
        return LuaNumber{.status = LuaNumberStatus::Missing};
    }
    if (api.type(state, index) != kLuaTypeNumber) {
        return LuaNumber{.status = LuaNumberStatus::Invalid};
    }
    const double value = api.to_number(state, index);
    if (!std::isfinite(value) || value < 0 || value > static_cast<double>(kMaximumExactLuaInteger) ||
        std::floor(value) != value) {
        return LuaNumber{.status = LuaNumberStatus::Invalid};
    }
    return LuaNumber{
        .status = LuaNumberStatus::Present,
        .value = static_cast<std::uint64_t>(value),
    };
}

std::optional<double> read_lua_nonnegative_real(
    const LuaApi& api,
    lua_State* state,
    int index) {
    if (api.type(state, index) != kLuaTypeNumber) {
        return std::nullopt;
    }
    const double value = api.to_number(state, index);
    if (!std::isfinite(value) || value < 0.0) {
        return std::nullopt;
    }
    return value;
}

std::optional<bool> read_lua_boolean_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name,
    std::string* error) {
    const int top = api.get_top(state);
    if (api.get_field_protected(state, object_index, field_name) != 0) {
        *error = call_failure_detail(
            api,
            state,
            "读取字段 " + std::string(field_name) + " 失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeBoolean) {
        api.set_top(state, top);
        *error = "字段 " + std::string(field_name) + " 不是布尔值";
        return std::nullopt;
    }
    const bool result = api.to_boolean(state, -1) != 0;
    api.set_top(state, top);
    return result;
}

std::optional<std::string> read_lua_text_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name,
    std::size_t maximum_bytes,
    bool allow_empty,
    std::string* error) {
    const int top = api.get_top(state);
    if (api.get_field_protected(state, object_index, field_name) != 0) {
        *error = call_failure_detail(
            api,
            state,
            "读取字段 " + std::string(field_name) + " 失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    std::optional<std::string> result =
        read_lua_text(api, state, -1, maximum_bytes, allow_empty, error);
    api.set_top(state, top);
    if (!result.has_value()) {
        *error = "字段 " + std::string(field_name) + " 无效: " + *error;
    }
    return result;
}

LuaNumber read_lua_number_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name) {
    const int top = api.get_top(state);
    if (api.get_field_protected(state, object_index, field_name) != 0) {
        api.set_top(state, top);
        return LuaNumber{.status = LuaNumberStatus::Invalid};
    }
    const LuaNumber result = read_lua_number(api, state, -1);
    api.set_top(state, top);
    return result;
}

LuaNumber read_raw_lua_number_field(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    const char* field_name) {
    const int top = api.get_top(state);
    api.push_string(state, field_name);
    api.raw_get(state, table_index);
    const LuaNumber result = read_lua_number(api, state, -1);
    api.set_top(state, top);
    return result;
}

bool push_lua_method_result(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error) {
    const int top = api.get_top(state);
    const int stable_object_index = api.stable_stack_index(state, object_index);
    if (api.get_field_protected(state, stable_object_index, method_name) != 0) {
        *error = call_failure_detail(
            api,
            state,
            "读取方法 " + std::string(method_name) + " 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeFunction) {
        api.set_top(state, top);
        *error = "对象缺少方法 " + std::string(method_name);
        return false;
    }

    api.push_value(state, stable_object_index);
    if (!call_function(
            api,
            state,
            arguments,
            1,
            "调用方法 " + std::string(method_name) + " 失败",
            error)) {
        api.set_top(state, top);
        return false;
    }
    return true;
}

bool push_lua_table_function_result(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    const char* function_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error) {
    const int top = api.get_top(state);
    const int stable_table_index = api.stable_stack_index(state, table_index);
    if (api.get_field_protected(state, stable_table_index, function_name) != 0) {
        *error = call_failure_detail(
            api,
            state,
            "读取函数 " + std::string(function_name) + " 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeFunction) {
        api.set_top(state, top);
        *error = "表缺少函数 " + std::string(function_name);
        return false;
    }
    if (!call_function(
            api,
            state,
            arguments,
            0,
            "调用函数 " + std::string(function_name) + " 失败",
            error)) {
        api.set_top(state, top);
        return false;
    }
    return true;
}

std::optional<std::uint64_t> read_lua_number_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_method_result(
            api,
            state,
            object_index,
            method_name,
            arguments,
            error)) {
        return std::nullopt;
    }
    const LuaNumber result = read_lua_number(api, state, -1);
    api.set_top(state, top);
    if (result.status != LuaNumberStatus::Present) {
        *error = "方法 " + std::string(method_name) + " 未返回精确非负整数";
        return std::nullopt;
    }
    return result.value;
}

std::optional<bool> read_lua_boolean_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_method_result(
            api,
            state,
            object_index,
            method_name,
            arguments,
            error)) {
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeBoolean) {
        api.set_top(state, top);
        *error = "方法 " + std::string(method_name) + " 未返回布尔值";
        return std::nullopt;
    }
    const bool result = api.to_boolean(state, -1) != 0;
    api.set_top(state, top);
    return result;
}

std::optional<std::string> read_lua_text_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::size_t maximum_bytes,
    bool allow_empty,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_method_result(
            api,
            state,
            object_index,
            method_name,
            arguments,
            error)) {
        return std::nullopt;
    }
    std::optional<std::string> result =
        read_lua_text(api, state, -1, maximum_bytes, allow_empty, error);
    api.set_top(state, top);
    if (!result.has_value()) {
        *error = "方法 " + std::string(method_name) + " 返回值无效: " + *error;
    }
    return result;
}

}  // namespace azlw::agent

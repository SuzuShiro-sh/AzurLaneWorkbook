// 声明有界 Lua 配置值、确定性表键和递归读取结果。

#pragma once

#include <cstddef>
#include <string>
#include <vector>

#include "lua_api.h"

namespace azlw::agent {

/// JSON 可以直接表达或需要显式表结构保存的 Lua 值类型。
enum class LuaValueKind {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
    Table,
    Unsupported,
};

/// 混合表键保留原始 Lua 类型，避免数字键与同文字符串键发生覆盖。
struct LuaValueKey final {
    enum class Kind { Number, String, Unsupported };

    Kind kind = Kind::Unsupported;
    double number = 0.0;
    std::string text;
    std::string lua_type;
};

/// 递归配置值；对象和混合表通过与 values 等长的 keys 保存键。
struct LuaValue final {
    LuaValueKind kind = LuaValueKind::Null;
    bool boolean = false;
    double number = 0.0;
    std::string text;
    std::vector<LuaValueKey> keys;
    std::vector<LuaValue> values;
    bool truncated = false;
    std::string reason;
};

/// 读取限制作用于每张表和单个字符串，递归深度同时提供循环兜底。
struct LuaValueLimits final {
    std::size_t maximum_depth = 12;
    std::size_t maximum_entries = 4'096;
    std::size_t maximum_string_bytes = 64 * 1024;
};

/// 原始值即使局部不完整也会返回，错误列表解释所有截断或不支持类型。
struct LuaValueResult final {
    LuaValue value;
    bool complete = false;
    std::vector<std::string> read_errors;
};

/// 在不改变调用方 Lua 栈的前提下读取一个配置值，并规范化表的输出顺序。
LuaValueResult read_lua_value(
    const LuaApi& api,
    lua_State* state,
    int index,
    const LuaValueLimits& limits = {});

/// 合并两个字符串键对象；后一个对象覆盖同名字段，结果按字段名稳定排序。
LuaValue merge_lua_objects(const LuaValue& base, const LuaValue& override_value);

}  // namespace azlw::agent

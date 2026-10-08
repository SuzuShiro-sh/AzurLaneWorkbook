// 声明运行态快照共享的 Lua 栈、标量读取和稳定错误能力。

#pragma once

#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>

#include "lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 记录进入作用域时的 Lua 栈顶，并在退出时恢复。
class LuaStackGuard final {
public:
    /// 捕获当前 Lua 栈顶。
    LuaStackGuard(const LuaApi& api, lua_State* state);
    /// 清理作用域内压入的全部临时值。
    ~LuaStackGuard();

    LuaStackGuard(const LuaStackGuard&) = delete;
    LuaStackGuard& operator=(const LuaStackGuard&) = delete;

    /// 返回进入作用域时的栈顶位置。
    int top() const noexcept;

private:
    const LuaApi& api_;
    lua_State* state_;
    int top_;
};

/// 区分 Lua 数值缺失、有效和类型或范围无效。
enum class LuaNumberStatus { Present, Missing, Invalid };

/// 保存 Lua 数值的分类结果及规范化整数值。
struct LuaNumber final {
    LuaNumberStatus status = LuaNumberStatus::Missing;
    std::uint64_t value = 0;
};

/// Lua 函数调用允许的固定参数类型，不提供任意脚本执行入口。
struct LuaCallArgument final {
    enum class Kind { Nil, Boolean, Number, String, StackValue };

    Kind kind = Kind::Nil;
    bool boolean_value = false;
    double number_value = 0.0;
    const char* string_value = nullptr;
    int stack_index = 0;

    static LuaCallArgument nil();
    static LuaCallArgument boolean(bool value);
    static LuaCallArgument number(double value);
    static LuaCallArgument string(const char* value);
    static LuaCallArgument stack_value(int stable_index);
};

/// 构造不会改变会话状态的稳定 Lua 读取错误。
AgentError make_lua_error(
    std::string code,
    std::string message,
    std::string retry = "never");

/// 读取栈顶 Lua 错误对象，并为调用方上下文补充可定位详情。
std::string lua_failure_detail(
    const LuaApi& api,
    lua_State* state,
    std::string message);

/// 判断 getProxy 失败是否精确匹配已知标题页初始化状态。
bool is_lua_proxy_initialization_pending(std::string_view detail);

/// 调用全局 `getProxy` 并把经过类型校验的代理对象留在栈顶。
bool push_lua_proxy(
    const LuaApi& api,
    lua_State* state,
    std::string_view proxy_name,
    std::string invalid_code,
    AgentError* error);

/// 从 Lua 全局环境取得指定 table，并把它留在栈顶。
bool push_lua_global_table(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::string* error);

/// 取得 `pg.<table_name>` 并把配置表留在栈顶，供静态配置读取器共用。
bool push_lua_pg_config_table(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::string* error);

/// 从已知 table 受保护地取得子 table，并把父表和子表都保留在栈上。
bool push_lua_table_field(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    const char* field_name,
    std::string* error);

/// 把带 `__index` 的配置页按固定字段复制为普通 table，避免遍历或改写来源表。
bool push_lua_materialized_table(
    const LuaApi& api,
    lua_State* state,
    int source_index,
    std::span<const char* const> fields,
    std::string* error);

/// 调用客户端静态名称函数，例如 `ShipType.Type2Name(id)`。
std::optional<std::string> read_lua_static_name(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    const char* function_name,
    std::uint64_t identifier,
    std::string* error);

/// 调用以稳定字符串键为参数的客户端静态名称函数，例如 `AttributeType.Type2Name(key)`。
std::optional<std::string> read_lua_static_name(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    const char* function_name,
    const char* key,
    std::string* error);

/// 从 `pg.<table_name>[identifier].<field_name>` 读取当前客户端配置文本。
std::optional<std::string> read_lua_pg_config_text(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::uint64_t identifier,
    const char* field_name,
    std::string* error);

/// 读取长度受限的非空 UTF-8 Lua 字符串。
std::optional<std::string> read_lua_string(
    const LuaApi& api,
    lua_State* state,
    int index,
    std::string* error);

/// 读取有明确长度上限的 UTF-8 文本；说明和效果可按调用方约束允许空串。
std::optional<std::string> read_lua_text(
    const LuaApi& api,
    lua_State* state,
    int index,
    std::size_t maximum_bytes,
    bool allow_empty,
    std::string* error);

/// 将指定栈位置分类为缺失、有效精确整数或无效数值。
LuaNumber read_lua_number(const LuaApi& api, lua_State* state, int index);

/// 读取非负有限 Lua number，保留航速等合法小数。
std::optional<double> read_lua_nonnegative_real(
    const LuaApi& api,
    lua_State* state,
    int index);

/// 受保护地读取对象布尔字段，拒绝数字和 nil 的隐式转换。
std::optional<bool> read_lua_boolean_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name,
    std::string* error);

/// 受保护地读取对象 UTF-8 文本字段，并应用调用方给出的长度与空值约束。
std::optional<std::string> read_lua_text_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name,
    std::size_t maximum_bytes,
    bool allow_empty,
    std::string* error);

/// 受保护地读取对象数值字段，并恢复临时压栈内容。
LuaNumber read_lua_number_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name);

/// 直接读取可信配置表中的数值字段，不触发 `__index` 元方法。
LuaNumber read_raw_lua_number_field(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    const char* field_name);

/// 调用对象方法并把唯一返回值留在栈顶；失败时恢复调用前栈顶。
bool push_lua_method_result(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error);

/// 调用表上的普通函数，不隐式传入表本身；失败时恢复调用前栈顶。
bool push_lua_table_function_result(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    const char* function_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error);

/// 调用返回精确非负整数的对象方法。
std::optional<std::uint64_t> read_lua_number_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error);

/// 调用返回严格布尔值的对象方法。
std::optional<bool> read_lua_boolean_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::string* error);

/// 调用返回长度受限 UTF-8 文本的对象方法。
std::optional<std::string> read_lua_text_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::size_t maximum_bytes,
    bool allow_empty,
    std::string* error);

}  // namespace azlw::agent

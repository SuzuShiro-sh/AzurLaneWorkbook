// 声明从已验证目标模块解析的最小 Lua C API 函数表。

#pragma once

#include <cstddef>
#include <string>
#include <span>

#include "module_view.h"

struct lua_State;

namespace azlw::agent {

/// 当前 Lua 运行时使用的伪索引和类型标识。
inline constexpr int kLuaGlobalsIndex = -10'002;
inline constexpr int kLuaTypeNil = 0;
inline constexpr int kLuaTypeBoolean = 1;
inline constexpr int kLuaTypeLightUserData = 2;
inline constexpr int kLuaTypeNumber = 3;
inline constexpr int kLuaTypeString = 4;
inline constexpr int kLuaTypeTable = 5;
inline constexpr int kLuaTypeFunction = 6;
inline constexpr int kLuaTypeUserData = 7;
inline constexpr int kLuaTypeThread = 8;

/// 背包只读采集所需的 Lua 导出函数集合。
struct LuaApi final {
    using CFunction = int (*)(lua_State* state);
    using GetTop = int (*)(lua_State* state);
    using SetTop = void (*)(lua_State* state, int index);
    using Type = int (*)(lua_State* state, int index);
    using ToBoolean = int (*)(lua_State* state, int index);
    using GetTable = void (*)(lua_State* state, int index);
    using RawGet = void (*)(lua_State* state, int index);
    using RawSetI = void (*)(lua_State* state, int index, int array_index);
    using CreateTable = void (*)(lua_State* state, int array_size, int record_size);
    using SetField = void (*)(lua_State* state, int index, const char* key);
    using ObjectLength = std::size_t (*)(lua_State* state, int index);
    using PushCClosure = void (*)(lua_State* state, CFunction function, int upvalues);
    using PushBoolean = void (*)(lua_State* state, int value);
    using PushString = void (*)(lua_State* state, const char* value);
    using PushNumber = void (*)(lua_State* state, double value);
    using PushValue = void (*)(lua_State* state, int index);
    using PushNil = void (*)(lua_State* state);
    using Next = int (*)(lua_State* state, int index);
    using ToString = const char* (*)(lua_State* state, int index, std::size_t* length);
    using ToNumber = double (*)(lua_State* state, int index);
    using ToPointer = const void* (*)(lua_State* state, int index);
    using ProtectedCall = int (*)(lua_State* state, int arguments, int results, int error_function);
    using ProtectedCCall = int (*)(lua_State*, CFunction, void*);
    using PushLightUserData = void (*)(lua_State*, void*);
    using RawSet = void (*)(lua_State*, int);
    using CheckStack = int (*)(lua_State*, int);
    using GarbageCollect = int (*)(lua_State*, int, int);
    using Operation = int (*)(const LuaApi&, lua_State*, void*);

    GetTop get_top = nullptr;
    SetTop set_top = nullptr;
    Type type = nullptr;
    ToBoolean to_boolean = nullptr;
    GetTable get_table = nullptr;
    RawGet raw_get = nullptr;
    RawSetI raw_set_i_unchecked = nullptr;
    CreateTable create_table_unchecked = nullptr;
    SetField set_field_unchecked = nullptr;
    ObjectLength object_length = nullptr;
    PushCClosure push_c_closure = nullptr;
    PushBoolean push_boolean = nullptr;
    PushString push_string_unchecked = nullptr;
    PushNumber push_number = nullptr;
    PushValue push_value = nullptr;
    PushNil push_nil = nullptr;
    Next next = nullptr;
    ToString to_string = nullptr;
    ToNumber to_number = nullptr;
    ToPointer to_pointer = nullptr;
    ProtectedCall protected_call = nullptr;
    ProtectedCCall protected_c_call = nullptr;
    PushLightUserData push_light_userdata = nullptr;
    RawSet raw_set = nullptr;
    CheckStack check_stack = nullptr;
    GarbageCollect garbage_collect = nullptr;

    /// 解析全部必需导出，并确认地址属于目标模块映射。
    bool resolve(const ModuleView& module, std::string* error);
    /// 确认函数表没有缺失入口。
    bool ready() const;

    /// 将普通相对索引转换为后续压栈操作仍可使用的稳定索引。
    int stable_stack_index(lua_State* state, int index) const;

    /// 回调仅持有平凡局部变量；Lua 错误不得跨越 C++ 容器或 RAII 对象。
    /// 输入索引复制为回调参数；成功留下一个结果，失败留下原始错误对象。
    int run_protected(lua_State* state, Operation operation, void* data,
                      std::span<const int> indices = {}) const;

    // 分配失败先退出 Lua 保护调用，再抛出可正常析构 C++ 对象的异常。
    void create_table(lua_State* state, int array_size, int record_size) const;
    void set_field(lua_State* state, int index, const char* key) const;
    void raw_set_i(lua_State* state, int index, int array_index) const;
    void push_string(lua_State* state, const char* value) const;
    void ensure_stack(lua_State* state, int slots) const;
    /// 返回 Lua 堆的 KiB 数，用于估算本帧采集产生的临时分配。
    int memory_kib(lua_State* state) const;
    /// 按本帧净分配量推进增量回收，不修改游戏的回收参数。
    bool collect_allocations_since(lua_State* state, int before_kib, std::string* error) const;

    /// 在 Lua pcall 边界内读取字段。成功或失败时分别把结果或 Lua 错误对象留在栈顶。
    int get_field_protected(lua_State* state, int object_index, const char* key) const;
    /// 在 Lua pcall 边界内按数值键索引表，支持带 `__index` 的分页配置表。
    int get_number_index_protected(lua_State* state, int object_index, double key) const;
};

}  // namespace azlw::agent

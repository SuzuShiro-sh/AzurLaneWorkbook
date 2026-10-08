// 实现最小 Lua C API 的动态解析，并约束所有入口属于目标模块。

#include "lua_api.h"

#include <cstdint>
#include <cstring>
#include <string>
#include <stdexcept>
#include <array>
#include <algorithm>

namespace azlw::agent {
namespace {

constexpr int kLuaRegistryIndex = -10'000;

struct ProtectedContext final {
    const LuaApi* api;
    LuaApi::Operation operation;
    void* data;
    int stack_slots;
    bool stack_ready = false;
    bool closure_stored = false;
};

thread_local ProtectedContext* active_context = nullptr;

int dispatch_operation(lua_State* state) {
    return active_context->operation(*active_context->api, state, active_context->data);
}

// cpcall 连自身闭包的分配也保护；注册表仅在本次调用中暂存闭包。
int prepare_operation(lua_State* state) {
    ProtectedContext* context = active_context;
    const LuaApi& api = *context->api;
    context->stack_ready = api.check_stack(state, context->stack_slots) != 0;
    if (!context->stack_ready) {
        return 0;
    }
    api.push_light_userdata(state, context);
    api.push_c_closure(state, dispatch_operation, 0);
    api.raw_set(state, kLuaRegistryIndex);
    context->closure_stored = true;
    return 0;
}

struct Lookup final {
    const char* text;
    double number;
};

int lookup_operation(const LuaApi& api, lua_State* state, void* data) {
    const auto* lookup = static_cast<const Lookup*>(data);
    if (lookup->text != nullptr) {
        api.push_string_unchecked(state, lookup->text);
    } else {
        api.push_number(state, lookup->number);
    }
    api.get_table(state, 1);
    return 1;
}

/// 从模块文件动态符号表解析单个入口，并复核运行时地址归属。
template <typename Function>
bool resolve_function(
    const ModuleView& module,
    const char* name,
    Function* output,
    std::string* error) {
    std::uintptr_t address = 0;
    if (!resolve_exported_function(module, name, &address, error)) {
        return false;
    }
    static_assert(sizeof(Function) == sizeof(address));
    std::memcpy(output, &address, sizeof(address));
    return true;
}

}  // namespace

// 全部入口采用短路解析，首个缺失或越界符号会保留精确名称。
bool LuaApi::resolve(const ModuleView& module, std::string* error) {
    return resolve_function(module, "lua_gettop", &get_top, error) &&
           resolve_function(module, "lua_settop", &set_top, error) &&
           resolve_function(module, "lua_type", &type, error) &&
           resolve_function(module, "lua_toboolean", &to_boolean, error) &&
           resolve_function(module, "lua_gettable", &get_table, error) &&
           resolve_function(module, "lua_rawget", &raw_get, error) &&
           resolve_function(module, "lua_rawseti", &raw_set_i_unchecked, error) &&
           resolve_function(module, "lua_createtable", &create_table_unchecked, error) &&
           resolve_function(module, "lua_setfield", &set_field_unchecked, error) &&
           resolve_function(module, "lua_objlen", &object_length, error) &&
           resolve_function(module, "lua_pushcclosure", &push_c_closure, error) &&
           resolve_function(module, "lua_pushboolean", &push_boolean, error) &&
           resolve_function(module, "lua_pushstring", &push_string_unchecked, error) &&
           resolve_function(module, "lua_pushnumber", &push_number, error) &&
           resolve_function(module, "lua_pushvalue", &push_value, error) &&
           resolve_function(module, "lua_pushnil", &push_nil, error) &&
           resolve_function(module, "lua_next", &next, error) &&
           resolve_function(module, "lua_tolstring", &to_string, error) &&
           resolve_function(module, "lua_tonumber", &to_number, error) &&
           resolve_function(module, "lua_topointer", &to_pointer, error) &&
           resolve_function(module, "lua_pcall", &protected_call, error) &&
           resolve_function(module, "lua_cpcall", &protected_c_call, error) &&
           resolve_function(module, "lua_pushlightuserdata", &push_light_userdata, error) &&
           resolve_function(module, "lua_rawset", &raw_set, error) &&
           resolve_function(module, "lua_checkstack", &check_stack, error) &&
           resolve_function(module, "lua_gc", &garbage_collect, error);
}

// 调用背包读取前必须确认函数表完整，禁止部分能力降级。
bool LuaApi::ready() const {
    return get_top != nullptr && set_top != nullptr && type != nullptr && to_boolean != nullptr &&
           get_table != nullptr && raw_get != nullptr && raw_set_i_unchecked != nullptr &&
           create_table_unchecked != nullptr &&
           set_field_unchecked != nullptr && object_length != nullptr && push_c_closure != nullptr &&
           push_boolean != nullptr && push_string_unchecked != nullptr && push_number != nullptr &&
           push_value != nullptr &&
           push_nil != nullptr && next != nullptr && to_string != nullptr && to_number != nullptr &&
           to_pointer != nullptr && protected_call != nullptr && protected_c_call != nullptr &&
           push_light_userdata != nullptr && raw_set != nullptr && check_stack != nullptr &&
           garbage_collect != nullptr;
}

// Lua 5.1 没有导出 lua_absindex；伪索引和正索引本身不随压栈变化。
int LuaApi::stable_stack_index(lua_State* state, int index) const {
    constexpr int kFirstPseudoIndex = -10'000;
    if (index > 0 || index <= kFirstPseudoIndex) {
        return index;
    }
    return get_top(state) + index + 1;
}

int LuaApi::run_protected(
    lua_State* state, Operation operation, void* data, std::span<const int> indices) const {
    const int original_top = get_top(state);
    ProtectedContext context{this, operation, data, static_cast<int>(indices.size()) + 8};
    ProtectedContext* previous = active_context;
    active_context = &context;
    int status = protected_c_call(state, prepare_operation, nullptr);
    if (status != 0 && context.closure_stored) {
        // cpcall 返回前的 GC 也可能失败；保留栈顶原始错误并移除已存入的闭包。
        check_stack(state, 2);
        push_light_userdata(state, &context);
        push_nil(state);
        raw_set(state, kLuaRegistryIndex);
    }
    if (status == 0 && context.stack_ready) {
        // 内层已扩容，此处只更新调用者帧的 top 上界，不再分配内存。
        check_stack(state, static_cast<int>(indices.size()) + 4);
        push_light_userdata(state, &context);
        raw_get(state, kLuaRegistryIndex);
        push_light_userdata(state, &context);
        push_nil(state);
        raw_set(state, kLuaRegistryIndex);
        for (const int index : indices) {
            push_value(state, index > 0 || index <= kLuaRegistryIndex
                                  ? index : original_top + index + 1);
        }
        status = protected_call(state, static_cast<int>(indices.size()), 1, 0);
    } else if (status == 0) {
        active_context = previous;
        throw std::runtime_error("Lua 栈容量不足");
    }
    active_context = previous;
    return status;
}

namespace {

void require_operation(const LuaApi& api, lua_State* state, int status,
                       int original_top, const char* operation) {
    if (status == 0) {
        return;
    }
    std::string detail(operation);
    if (api.type(state, -1) == kLuaTypeString) {
        std::size_t length = 0;
        const char* message = api.to_string(state, -1, &length);
        if (message != nullptr) {
            detail += ": ";
            detail.append(message, length);
        }
    }
    api.set_top(state, original_top);
    throw std::runtime_error(detail);
}

}  // namespace

void LuaApi::create_table(lua_State* state, int array_size, int record_size) const {
    struct Sizes { int array; int record; } sizes{array_size, record_size};
    const int top = get_top(state);
    const int status = run_protected(state,
        [](const LuaApi& api, lua_State* lua, void* data) {
            const auto* sizes = static_cast<const Sizes*>(data);
            api.create_table_unchecked(lua, sizes->array, sizes->record);
            return 1;
        }, &sizes);
    require_operation(*this, state, status, top, "lua_createtable 失败");
}

void LuaApi::push_string(lua_State* state, const char* value) const {
    const int top = get_top(state);
    const char* input = value;
    const int status = run_protected(state,
        [](const LuaApi& api, lua_State* lua, void* data) {
            api.push_string_unchecked(lua, *static_cast<const char**>(data));
            return 1;
        }, &input);
    require_operation(*this, state, status, top, "lua_pushstring 失败");
}

void LuaApi::set_field(lua_State* state, int index, const char* key) const {
    const int top = get_top(state);
    const std::array inputs{stable_stack_index(state, index), top};
    const char* input = key;
    const int status = run_protected(state,
        [](const LuaApi& api, lua_State* lua, void* data) {
            api.set_field_unchecked(lua, 1, *static_cast<const char**>(data));
            return 0;
        }, &input, inputs);
    require_operation(*this, state, status, top, "lua_setfield 失败");
    set_top(state, top - 1);
}

void LuaApi::raw_set_i(lua_State* state, int index, int array_index) const {
    const int top = get_top(state);
    const std::array inputs{stable_stack_index(state, index), top};
    const int status = run_protected(state,
        [](const LuaApi& api, lua_State* lua, void* data) {
            api.raw_set_i_unchecked(lua, 1, *static_cast<int*>(data));
            return 0;
        }, &array_index, inputs);
    require_operation(*this, state, status, top, "lua_rawseti 失败");
    set_top(state, top - 1);
}

void LuaApi::ensure_stack(lua_State* state, int slots) const {
    const int top = get_top(state);
    bool ready = false;
    struct Request { int slots; bool* ready; } request{slots, &ready};
    const int status = run_protected(state,
        [](const LuaApi& api, lua_State* lua, void* data) {
            const auto* request = static_cast<const Request*>(data);
            *request->ready = api.check_stack(lua, request->slots) != 0;
            return 0;
        }, &request);
    require_operation(*this, state, status, top, "lua_checkstack 失败");
    set_top(state, top);
    if (!ready || check_stack(state, slots) == 0) {
        throw std::runtime_error("Lua 栈容量不足");
    }
}

int LuaApi::memory_kib(lua_State* state) const {
    constexpr int kLuaGcCount = 3;
    return garbage_collect(state, kLuaGcCount, 0);
}

bool LuaApi::collect_allocations_since(lua_State* state, int before_kib, std::string* error) const {
    int allocated_kib = std::max(0, memory_kib(state) - before_kib);
    if (allocated_kib == 0) {
        return true;
    }
    const int top = get_top(state);
    constexpr int kLuaGcIsRunning = 9;
    constexpr int kLuaGcStop = 0;
    const bool was_stopped = garbage_collect(state, kLuaGcIsRunning, 0) == 0;
    try {
        const int status = run_protected(state,
            [](const LuaApi& api, lua_State* lua, void* data) {
                constexpr int kLuaGcStep = 5;
                api.garbage_collect(lua, kLuaGcStep, *static_cast<int*>(data));
                return 0;
            }, &allocated_kib);
        // LuaJIT 的 STEP 会重设回收阈值，必须保留游戏原有的自动回收暂停状态。
        if (was_stopped) {
            garbage_collect(state, kLuaGcStop, 0);
        }
        require_operation(*this, state, status, top, "Lua 增量回收失败");
        set_top(state, top);
        return true;
    } catch (const std::exception& exception) {
        if (was_stopped) {
            garbage_collect(state, kLuaGcStop, 0);
        }
        set_top(state, top);
        *error = exception.what();
        return false;
    }
}

int LuaApi::get_field_protected(lua_State* state, int object_index, const char* key) const {
    Lookup lookup{key, 0};
    return run_protected(state, lookup_operation, &lookup, {&object_index, 1});
}

int LuaApi::get_number_index_protected(lua_State* state, int object_index, double key) const {
    Lookup lookup{nullptr, key};
    return run_protected(state, lookup_operation, &lookup, {&object_index, 1});
}

}  // namespace azlw::agent

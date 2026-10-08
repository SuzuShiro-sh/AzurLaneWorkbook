// 验证 BagProxy 冷启动错误分类和背包响应的确定性顺序。

#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <optional>
#include <string>

#include "snapshots/bag_snapshot.h"

namespace {

/// 保存当前用例注入的 Lua 错误文本。
std::string fixture_error_detail;
bool fixture_call_failed = false;
azlw::agent::LuaApi::CFunction fixture_callback = nullptr;

/// 断言测试条件成立；失败时输出稳定原因并结束进程。
void require(bool condition, const char* message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << '\n';
        std::exit(1);
    }
}

/// 返回空栈，供快照入口建立恢复边界。
int fixture_get_top(lua_State*) { return 0; }

/// 接受栈恢复操作；夹具不保存真实 Lua 栈。
void fixture_set_top(lua_State*, int) {}

/// 把 getProxy 全局值固定报告为函数。
int fixture_type(lua_State*, int) {
    return fixture_call_failed ? azlw::agent::kLuaTypeString : azlw::agent::kLuaTypeFunction;
}

/// 提供完整函数表所需的固定布尔转换结果。
int fixture_to_boolean(lua_State*, int) { return 0; }

/// 提供完整函数表所需的空表读取入口。
void fixture_get_table(lua_State*, int) {}

/// 接受 getProxy 与参数的原始全局读取操作。
void fixture_raw_get(lua_State*, int) {}

/// 提供完整函数表所需的空数值数组写入入口。
void fixture_raw_set_i(lua_State*, int, int) {}

/// 提供完整函数表所需的空 table 构造入口。
void fixture_create_table(lua_State*, int, int) {}

/// 提供完整函数表所需的空字段写入入口。
void fixture_set_field(lua_State*, int, const char*) {}

/// 当前错误分类用例不读取表长度。
std::size_t fixture_object_length(lua_State*, int) { return 0; }

/// 提供完整函数表所需的空闭包压栈入口。
void fixture_push_c_closure(lua_State*, azlw::agent::LuaApi::CFunction function, int) {
    fixture_callback = function;
}

/// 提供完整函数表所需的空布尔压栈入口。
void fixture_push_boolean(lua_State*, int) {}

/// 返回原始字符串地址，模拟 Lua 的字符串压栈结果。
void fixture_push_string(lua_State*, const char*) {}

/// 提供完整函数表所需的空数值压栈入口。
void fixture_push_number(lua_State*, double) {}

/// 提供完整函数表所需的空值复制入口。
void fixture_push_value(lua_State*, int) {}

/// 提供完整函数表所需的空 nil 压栈入口。
void fixture_push_nil(lua_State*) {}

/// 固定结束表遍历；当前用例不会进入背包数据分支。
int fixture_next(lua_State*, int) { return 0; }

/// 返回当前用例的错误文本及准确字节长度。
const char* fixture_to_string(lua_State*, int, std::size_t* length) {
    *length = fixture_error_detail.size();
    return fixture_error_detail.data();
}

/// 提供完整函数表所需的固定数值转换结果。
double fixture_to_number(lua_State*, int) { return 0.0; }

/// 当前错误分类用例不读取表身份。
const void* fixture_to_pointer(lua_State*, int) { return nullptr; }

/// 固定返回 Lua 调用失败，使快照只进入 getProxy 错误分类分支。
int fixture_protected_call(lua_State* state, int arguments, int, int) {
    if (arguments == 0) {
        fixture_callback(state);
        return 0;
    }
    fixture_call_failed = true;
    return 1;
}

int fixture_protected_c_call(lua_State* state, azlw::agent::LuaApi::CFunction function, void*) {
    function(state);
    return 0;
}
void fixture_push_light_userdata(lua_State*, void*) {}
void fixture_raw_set(lua_State*, int) {}
int fixture_check_stack(lua_State*, int) { return 1; }

/// 构造只走 getProxy 失败分支的完整 Lua API 函数表。
azlw::agent::LuaApi make_fixture_api() {
    return azlw::agent::LuaApi{
        .get_top = fixture_get_top,
        .set_top = fixture_set_top,
        .type = fixture_type,
        .to_boolean = fixture_to_boolean,
        .get_table = fixture_get_table,
        .raw_get = fixture_raw_get,
        .raw_set_i_unchecked = fixture_raw_set_i,
        .create_table_unchecked = fixture_create_table,
        .set_field_unchecked = fixture_set_field,
        .object_length = fixture_object_length,
        .push_c_closure = fixture_push_c_closure,
        .push_boolean = fixture_push_boolean,
        .push_string_unchecked = fixture_push_string,
        .push_number = fixture_push_number,
        .push_value = fixture_push_value,
        .push_nil = fixture_push_nil,
        .next = fixture_next,
        .to_string = fixture_to_string,
        .to_number = fixture_to_number,
        .to_pointer = fixture_to_pointer,
        .protected_call = fixture_protected_call,
        .protected_c_call = fixture_protected_c_call,
        .push_light_userdata = fixture_push_light_userdata,
        .raw_set = fixture_raw_set,
        .check_stack = fixture_check_stack,
        .garbage_collect = [](lua_State*, int, int) { return 0; },
    };
}

/// 执行一次受控 getProxy 失败，并核对 agent 给出的重试指令。
void require_retry_directive(const std::string& detail, const std::string& expected_retry) {
    fixture_error_detail = detail;
    fixture_call_failed = false;
    const azlw::agent::LuaApi api = make_fixture_api();
    lua_State* const state = reinterpret_cast<lua_State*>(static_cast<std::uintptr_t>(1));
    const azlw::agent::SnapshotExecution execution = azlw::agent::snapshot_bag(api, state, 1);
    require(!execution.success, "getProxy fixture unexpectedly succeeded");
    require(execution.error.code == "lua_get_proxy_failed", "unexpected getProxy error code");
    require(execution.error.session_effect == "unchanged", "getProxy failure changed session state");
    require(execution.error.retry == expected_retry, "unexpected getProxy retry directive");
}

/// 乱序 Lua 遍历结果必须整理为稳定条目和诊断顺序。
void require_canonical_order() {
    azlw::agent::BagSnapshot snapshot;
    snapshot.items = {
        azlw::agent::BagItem{
            .item_id = 30,
            .quantity = 1,
            .resolved_name = "thirty",
            .compose_recipe = std::nullopt,
        },
        azlw::agent::BagItem{
            .item_id = 10,
            .quantity = 1,
            .resolved_name = "ten",
            .compose_recipe = std::nullopt,
        },
        azlw::agent::BagItem{
            .item_id = 20,
            .quantity = 1,
            .resolved_name = "twenty",
            .compose_recipe = std::nullopt,
        },
    };
    snapshot.read_errors = {
        azlw::agent::ReadError{.item_id = 20, .code = "z", .message = "second"},
        azlw::agent::ReadError{.item_id = std::nullopt, .code = "root", .message = "first"},
        azlw::agent::ReadError{.item_id = 20, .code = "a", .message = "third"},
        azlw::agent::ReadError{.item_id = 10, .code = "item", .message = "fourth"},
    };

    azlw::agent::canonicalize_bag_snapshot(&snapshot);

    require(snapshot.items[0].item_id == 10, "first bag item must have the smallest id");
    require(snapshot.items[1].item_id == 20, "second bag item order mismatch");
    require(snapshot.items[2].item_id == 30, "third bag item order mismatch");
    require(!snapshot.read_errors[0].item_id.has_value(), "root diagnostic must sort first");
    require(snapshot.read_errors[1].item_id == 10, "item diagnostic id order mismatch");
    require(snapshot.read_errors[2].code == "a", "diagnostic code tie-breaker mismatch");
    require(snapshot.read_errors[3].code == "z", "diagnostic code order mismatch");
}

}  // namespace

/// 覆盖精确冷启动状态、单特征误报和未知 Lua 异常。
int main() {
    require_retry_directive(
        "Support/Helpers/M02:0: attempt to index field 'm02' (a nil value)",
        "same_request");
    require_retry_directive("Support/Helpers/M02:0: unrelated failure", "never");
    require_retry_directive("Other/Module:0: attempt to index field 'm02' (a nil value)", "never");
    require_retry_directive("unknown getProxy failure", "never");
    require_canonical_order();

    std::cout << "PASS bag_snapshot_test\n";
    return 0;
}

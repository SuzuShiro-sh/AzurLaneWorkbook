// 用离线目录索引核对装备配置和合成配方的跨帧批次。

#include <algorithm>
#include <cstdint>
#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <stdexcept>
#include <utility>
#include <vector>

#include "snapshots/equipment_config_snapshot.h"

namespace {

using azlw::agent::ComposeRecipeBatchProgress;
using azlw::agent::EquipmentBatchStep;
using azlw::agent::EquipmentConfigBatchProgress;
using azlw::agent::LuaApi;
using azlw::agent::kMaximumEquipmentFrameSize;
using azlw::agent::kMaximumEquipmentPageSize;

enum class ValueKind { Nil, Number, String, Function, Table };

struct FakeTable;

struct FakeValue {
    ValueKind kind = ValueKind::Nil;
    double number = 0.0;
    std::string text;
    LuaApi::CFunction function = nullptr;
    std::shared_ptr<FakeTable> table;

    static FakeValue nil() { return {}; }
    static FakeValue number_value(double value) {
        FakeValue result;
        result.kind = ValueKind::Number;
        result.number = value;
        return result;
    }
    static FakeValue string_value(std::string value) {
        FakeValue result;
        result.kind = ValueKind::String;
        result.text = std::move(value);
        return result;
    }
    static FakeValue function_value(LuaApi::CFunction value) {
        FakeValue result;
        result.kind = ValueKind::Function;
        result.function = value;
        return result;
    }
    static FakeValue table_value(std::shared_ptr<FakeTable> value) {
        FakeValue result;
        result.kind = ValueKind::Table;
        result.table = std::move(value);
        return result;
    }
};

struct FakeTable {
    std::map<std::string, FakeValue> fields;
    std::map<std::uint64_t, FakeValue> numeric;
};

struct FakeState {
    std::vector<FakeValue> stack;
    std::map<std::string, FakeValue> registry;
    int protected_depth = 0;
    std::string fault_operation;
    bool stack_available = true;
    std::map<std::string, FakeValue> globals;
};

void require(bool condition, const std::string& message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << '\n';
        std::exit(1);
    }
}

FakeState& fixture(lua_State* state) { return *reinterpret_cast<FakeState*>(state); }

FakeValue* stack_value(FakeState& state, int index) {
    if (index > 0) {
        const auto offset = static_cast<std::size_t>(index - 1);
        return offset < state.stack.size() ? &state.stack[offset] : nullptr;
    }
    if (index < 0 && index > azlw::agent::kLuaGlobalsIndex) {
        const auto offset = static_cast<std::ptrdiff_t>(state.stack.size()) + index;
        return offset >= 0 && static_cast<std::size_t>(offset) < state.stack.size()
                   ? &state.stack[static_cast<std::size_t>(offset)]
                   : nullptr;
    }
    return nullptr;
}

int fake_get_top(lua_State* state) { return static_cast<int>(fixture(state).stack.size()); }

void fake_set_top(lua_State* state, int index) {
    FakeState& current = fixture(state);
    int target = index;
    if (index < 0 && index > azlw::agent::kLuaGlobalsIndex) {
        target = static_cast<int>(current.stack.size()) + index + 1;
    }
    current.stack.resize(static_cast<std::size_t>(std::max(target, 0)));
}

int fake_type(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    if (value == nullptr || value->kind == ValueKind::Nil) {
        return azlw::agent::kLuaTypeNil;
    }
    switch (value->kind) {
        case ValueKind::Number:
            return azlw::agent::kLuaTypeNumber;
        case ValueKind::String:
            return azlw::agent::kLuaTypeString;
        case ValueKind::Function:
            return azlw::agent::kLuaTypeFunction;
        case ValueKind::Table:
            return azlw::agent::kLuaTypeTable;
        case ValueKind::Nil:
            return azlw::agent::kLuaTypeNil;
    }
    return azlw::agent::kLuaTypeNil;
}

int fake_to_boolean(lua_State*, int) { return 0; }
double fake_to_number(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    return value != nullptr && value->kind == ValueKind::Number ? value->number : 0.0;
}
const char* fake_to_string(lua_State* state, int index, std::size_t* length) {
    const FakeValue* value = stack_value(fixture(state), index);
    if (value == nullptr || value->kind != ValueKind::String) {
        *length = 0;
        return nullptr;
    }
    *length = value->text.size();
    return value->text.c_str();
}
const void* fake_to_pointer(lua_State*, int) { return nullptr; }

void fake_push_value(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    fixture(state).stack.push_back(value == nullptr ? FakeValue::nil() : *value);
}
void fake_push_string(lua_State* state, const char* value) {
    fixture(state).stack.push_back(FakeValue::string_value(value == nullptr ? "" : value));

}
void fake_push_boolean(lua_State* state, int) {
    fixture(state).stack.push_back(FakeValue::nil());
}
void fake_push_number(lua_State* state, double value) {
    fixture(state).stack.push_back(FakeValue::number_value(value));
}
void fake_push_nil(lua_State* state) { fixture(state).stack.push_back(FakeValue::nil()); }

void lookup(FakeState& current, const FakeValue* object, const FakeValue& key) {
    FakeValue result = FakeValue::nil();
    if (object != nullptr && object->kind == ValueKind::Table && object->table != nullptr) {
        if (key.kind == ValueKind::String) {
            const auto found = object->table->fields.find(key.text);
            if (found != object->table->fields.end()) {
                result = found->second;
            }
        } else if (key.kind == ValueKind::Number) {
            const auto found = object->table->numeric.find(static_cast<std::uint64_t>(key.number));
            if (found != object->table->numeric.end()) {
                result = found->second;
            }
        }
    }
    current.stack.push_back(std::move(result));
}

void fake_get_table(lua_State* state, int index) {
    FakeState& current = fixture(state);
    const FakeValue key = current.stack.back();
    current.stack.pop_back();
    lookup(current, stack_value(current, index), key);
}

void fake_raw_get(lua_State* state, int index) {
    if (index == -10'000) {
        auto& current = fixture(state);
        const std::string key = current.stack.back().text;
        current.stack.back() = current.registry.at(key);
        return;
    }
    FakeState& current = fixture(state);
    const FakeValue key = current.stack.back();
    current.stack.pop_back();
    if (index == azlw::agent::kLuaGlobalsIndex && key.kind == ValueKind::String) {
        const auto found = current.globals.find(key.text);
        current.stack.push_back(found == current.globals.end() ? FakeValue::nil() : found->second);
        return;
    }
    lookup(current, stack_value(current, index), key);
}

void fake_raw_set_i(lua_State*, int, int) {}
void fake_create_table(lua_State* state, int, int) {
    fixture(state).stack.push_back(FakeValue::table_value(std::make_shared<FakeTable>()));
}
void fake_set_field(lua_State*, int, const char*) {}
std::size_t fake_object_length(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    return value != nullptr && value->kind == ValueKind::Table && value->table != nullptr
               ? value->table->numeric.size()
               : 0;
}
void fake_push_c_closure(lua_State* state, LuaApi::CFunction function, int) {
    fixture(state).stack.push_back(FakeValue::function_value(function));
}
int fake_next(lua_State*, int) { return 0; }

int fake_protected_call(lua_State* state, int arguments, int, int) {
    auto& current = fixture(state);
    const auto function_position = current.stack.size() - static_cast<std::size_t>(arguments + 1);
    const auto function = current.stack[function_position].function;
    std::vector<FakeValue> prefix(current.stack.begin(), current.stack.begin() + function_position);
    current.stack.erase(current.stack.begin(), current.stack.begin() + function_position + 1);
    ++current.protected_depth;
    try {
        const int count = function(state);
        const FakeValue result = count > 0 ? current.stack.back() : FakeValue::nil();
        current.stack = std::move(prefix);
        current.stack.push_back(result);
        --current.protected_depth;
        return 0;
    } catch (const std::runtime_error& error) {
        current.stack = std::move(prefix);
        current.stack.push_back(FakeValue::string_value(error.what()));
        --current.protected_depth;
        return 2;
    }
}

int fake_protected_c_call(lua_State* state, LuaApi::CFunction function, void*) {
    auto& current = fixture(state);
    auto prefix = std::move(current.stack);
    current.stack = {FakeValue::nil()};
    ++current.protected_depth;
    try {
        function(state);
        current.stack = std::move(prefix);
        --current.protected_depth;
        return 0;
    } catch (const std::runtime_error& error) {
        current.stack = std::move(prefix);
        current.stack.push_back(FakeValue::string_value(error.what()));
        --current.protected_depth;
        return 2;
    }
}

void fake_push_light_userdata(lua_State* state, void* value) {
    fixture(state).stack.push_back(FakeValue::string_value(
        std::to_string(reinterpret_cast<std::uintptr_t>(value))));
}

void fake_raw_set(lua_State* state, int index) {
    auto& current = fixture(state);
    require(index == -10'000, "unexpected registry index");
    const FakeValue value = current.stack.back();
    current.stack.pop_back();
    const auto key = current.stack.back().text;
    current.stack.pop_back();
    if (value.kind == ValueKind::Nil) {
        current.registry.erase(key);
    } else {
        current.registry[key] = value;
    }
}

int fake_check_stack(lua_State* state, int) { return fixture(state).stack_available ? 1 : 0; }

LuaApi make_api() {
    LuaApi api;
    api.get_top = fake_get_top;
    api.set_top = fake_set_top;
    api.type = fake_type;
    api.to_boolean = fake_to_boolean;
    api.get_table = fake_get_table;
    api.raw_get = fake_raw_get;
    api.raw_set_i_unchecked = fake_raw_set_i;
    api.create_table_unchecked = fake_create_table;
    api.set_field_unchecked = fake_set_field;
    api.object_length = fake_object_length;
    api.push_c_closure = fake_push_c_closure;
    api.push_boolean = fake_push_boolean;
    api.push_string_unchecked = fake_push_string;
    api.push_number = fake_push_number;
    api.push_value = fake_push_value;
    api.push_nil = fake_push_nil;
    api.next = fake_next;
    api.to_string = fake_to_string;
    api.to_number = fake_to_number;
    api.to_pointer = fake_to_pointer;
    api.protected_call = fake_protected_call;
    api.protected_c_call = fake_protected_c_call;
    api.push_light_userdata = fake_push_light_userdata;
    api.raw_set = fake_raw_set;
    api.check_stack = fake_check_stack;
    api.garbage_collect = [](lua_State*, int, int) { return 0; };
    return api;
}

FakeState catalog_with_zero_ids(const char* table_name, std::uint32_t count) {
    FakeState state;
    auto all = std::make_shared<FakeTable>();
    for (std::uint32_t index = 1; index <= count; ++index) {
        all->numeric.emplace(index, FakeValue::number_value(0));
    }
    auto table = std::make_shared<FakeTable>();
    table->fields.emplace("all", FakeValue::table_value(all));
    auto pg = std::make_shared<FakeTable>();
    pg->fields.emplace(table_name, FakeValue::table_value(table));
    state.globals.emplace("pg", FakeValue::table_value(pg));
    return state;
}

void test_equipment_batch_crosses_the_frame_boundary() {
    require(kMaximumEquipmentFrameSize == 250, "装备单帧容量应为 250");
    require(kMaximumEquipmentPageSize == 1000, "装备单次响应页容量应为 1000");
    FakeState state = catalog_with_zero_ids("equip_data_template", 300);
    EquipmentConfigBatchProgress progress;
    progress.start_index = 0;
    progress.page_size = 300;
    progress.module_sha256 = std::string(64, 'a');
    const LuaApi api = make_api();
    auto* lua = reinterpret_cast<lua_State*>(&state);
    require(
        azlw::agent::advance_equipment_config_batch(api, lua, &progress) ==
            EquipmentBatchStep::Continue,
        "300 条装备目录的第一帧应续办");
    require(progress.cursor == 250, "装备第一帧应停在帧容量");
    require(progress.outcome.page.read_errors.size() == 250, "无效索引应记在第一帧");
    require(
        azlw::agent::advance_equipment_config_batch(api, lua, &progress) ==
            EquipmentBatchStep::Finished,
        "装备第二帧应结束批次");
    require(progress.cursor == 300, "装备批次应覆盖 300 条");
    require(progress.outcome.page.read_errors.size() == 300, "两帧错误应合并");
    require(!progress.outcome.page.next_index.has_value(), "读完装备目录后不应再给下一页");
}

void test_recipe_batch_uses_the_same_frame_budget() {
    FakeState state = catalog_with_zero_ids("compose_data_template", 300);
    ComposeRecipeBatchProgress progress;
    progress.start_index = 0;
    progress.page_size = 300;
    progress.module_sha256 = std::string(64, 'a');
    const LuaApi api = make_api();
    auto* lua = reinterpret_cast<lua_State*>(&state);
    require(
        azlw::agent::advance_compose_recipe_batch(api, lua, &progress) ==
            EquipmentBatchStep::Continue,
        "300 条配方的第一帧应续办");
    require(progress.cursor == kMaximumEquipmentFrameSize, "配方第一帧应停在帧容量");
    require(
        azlw::agent::advance_compose_recipe_batch(api, lua, &progress) ==
            EquipmentBatchStep::Finished,
        "配方第二帧应结束批次");
    require(progress.outcome.page.read_errors.size() == 300, "配方两帧错误应合并");
}

}  // namespace

int main() {
    {
        FakeState state = catalog_with_zero_ids("equip_data_template", 0);
        // all 缺失也必须支持显式 ID；不存在的记录不尝试构造 Equipment。
        auto table = state.globals.at("pg").table->fields.at("equip_data_template").table;
        table->fields.erase("all");
        EquipmentConfigBatchProgress progress;
        progress.ids = {100, 200};
        progress.module_sha256 = std::string(64, 'a');
        require(azlw::agent::advance_equipment_config_batch(make_api(), reinterpret_cast<lua_State*>(&state), &progress) == EquipmentBatchStep::Finished, "ID 批次应单帧结束");
        require(progress.outcome.success && progress.outcome.page.complete, "没有 all 时应成功查询显式 ID");
        require(progress.outcome.page.missing_ids == progress.ids, "缺失 ID 必须完整返回");
        require(progress.outcome.page.configs.empty() && state.stack.empty(), "查询后应恢复 Lua 栈");
        table->numeric[100] = FakeValue::table_value(std::make_shared<FakeTable>());
        EquipmentConfigBatchProgress malformed;
        malformed.ids = {100, 200};
        malformed.module_sha256 = progress.module_sha256;
        azlw::agent::advance_equipment_config_batch(make_api(), reinterpret_cast<lua_State*>(&state), &malformed);
        require(malformed.outcome.success && !malformed.outcome.page.complete, "已存在但解析失败的配置应返回不完整记录");
        require(malformed.outcome.page.configs.size() == 1 && malformed.outcome.page.configs[0].config_id == 100, "解析失败必须保留配置 ID");
        require(malformed.outcome.page.missing_ids == std::vector<std::uint64_t>{200}, "解析失败不得作为不存在返回");

    }

    test_equipment_batch_crosses_the_frame_boundary();
    test_recipe_batch_uses_the_same_frame_budget();
    std::cout << "PASS equipment_batch_test\n";
    return 0;
}

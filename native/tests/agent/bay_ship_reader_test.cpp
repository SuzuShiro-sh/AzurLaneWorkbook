// 用离线 Lua 表核对账号前窗口的船坞分页：空表、页容量、累计上限、无效键和锚点丢失。

#include <algorithm>
#include <cstdint>
#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <stdexcept>
#include <utility>
#include <vector>

#include "snapshots/bay_ship_reader.h"
#include "snapshots/owned_state_snapshot.h"

namespace {

using azlw::agent::AccountBeforeExecution;
using azlw::agent::AccountBeforePhase;
using azlw::agent::AccountBeforeProgress;
using azlw::agent::AccountBeforeStep;
using azlw::agent::LuaApi;
using azlw::agent::kMaximumDockPageSize;

enum class ValueKind { Nil, Boolean, Number, String, Function, Table };

struct FakeTable;

struct FakeValue final {
    ValueKind kind = ValueKind::Nil;
    bool boolean = false;
    double number = 0.0;
    std::string text;
    LuaApi::CFunction function = nullptr;
    std::shared_ptr<FakeTable> table;

    static FakeValue nil() { return {}; }

    static FakeValue boolean_value(bool value) {
        FakeValue result;
        result.kind = ValueKind::Boolean;
        result.boolean = value;
        return result;
    }

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

struct FakeTable final {
    std::map<std::string, FakeValue> fields;
    std::map<std::uint64_t, FakeValue> numeric;
    std::vector<std::pair<FakeValue, FakeValue>> entries;
};

struct FakeState final {
    std::vector<FakeValue> stack;
    std::map<std::string, FakeValue> registry;
    int protected_depth = 0;
    std::string fault_operation;
    bool stack_available = true;
    std::map<std::string, FakeValue> globals;
    std::shared_ptr<FakeTable> dock;
    std::shared_ptr<FakeTable> warehouse;
    bool resources_only = false;
    bool capacity_invalid = false;
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
        const std::size_t offset = static_cast<std::size_t>(index - 1);
        return offset < state.stack.size() ? &state.stack[offset] : nullptr;
    }
    if (index < 0 && index > azlw::agent::kLuaGlobalsIndex) {
        const std::ptrdiff_t offset = static_cast<std::ptrdiff_t>(state.stack.size()) + index;
        return offset >= 0 && static_cast<std::size_t>(offset) < state.stack.size()
                   ? &state.stack[static_cast<std::size_t>(offset)]
                   : nullptr;
    }
    return nullptr;
}

bool same_key(const FakeValue& left, const FakeValue& right) {
    if (left.kind != right.kind) {
        return false;
    }
    if (left.kind == ValueKind::Number) {
        return left.number == right.number;
    }
    if (left.kind == ValueKind::String) {
        return left.text == right.text;
    }
    return left.kind == ValueKind::Nil;
}

std::uint64_t number_key(double value) { return static_cast<std::uint64_t>(value); }

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
    if (value == nullptr) {
        return azlw::agent::kLuaTypeNil;
    }
    switch (value->kind) {
        case ValueKind::Nil:
            return azlw::agent::kLuaTypeNil;
        case ValueKind::Boolean:
            return azlw::agent::kLuaTypeBoolean;
        case ValueKind::Number:
            return azlw::agent::kLuaTypeNumber;
        case ValueKind::String:
            return azlw::agent::kLuaTypeString;
        case ValueKind::Function:
            return azlw::agent::kLuaTypeFunction;
        case ValueKind::Table:
            return azlw::agent::kLuaTypeTable;
    }
    return azlw::agent::kLuaTypeNil;
}

int fake_to_boolean(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    return value != nullptr && value->kind == ValueKind::Boolean && value->boolean ? 1 : 0;
}

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
    return value->text.data();
}

const void* fake_to_pointer(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    return value != nullptr && value->kind == ValueKind::Table ? value->table.get() : nullptr;
}

void fake_push_value(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    fixture(state).stack.push_back(value == nullptr ? FakeValue::nil() : *value);
}

void fake_push_string(lua_State* state, const char* value) {
    fixture(state).stack.push_back(FakeValue::string_value(value == nullptr ? "" : value));

}

void fake_push_boolean(lua_State* state, int value) {
    fixture(state).stack.push_back(FakeValue::boolean_value(value != 0));
}

void fake_push_number(lua_State* state, double value) {
    fixture(state).stack.push_back(FakeValue::number_value(value));
}

void fake_push_nil(lua_State* state) { fixture(state).stack.push_back(FakeValue::nil()); }

void fake_get_table(lua_State* state, int index) {
    FakeState& current = fixture(state);
    const FakeValue* object = stack_value(current, index);
    const FakeValue key = current.stack.back();
    current.stack.pop_back();
    FakeValue result = FakeValue::nil();
    if (object != nullptr && object->kind == ValueKind::Table && object->table != nullptr) {
        if (key.kind == ValueKind::String) {
            const auto found = object->table->fields.find(key.text);
            if (found != object->table->fields.end()) {
                result = found->second;
            }
        } else if (key.kind == ValueKind::Number) {
            const auto found = object->table->numeric.find(number_key(key.number));
            if (found != object->table->numeric.end()) {
                result = found->second;
            }
        }
    }
    current.stack.push_back(std::move(result));
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
    FakeValue result = FakeValue::nil();
    if (index == azlw::agent::kLuaGlobalsIndex && key.kind == ValueKind::String) {
        const auto found = current.globals.find(key.text);
        if (found != current.globals.end()) {
            result = found->second;
        }
    } else if (const FakeValue* object = stack_value(current, index);
               object != nullptr && object->kind == ValueKind::Table && object->table != nullptr) {
        if (key.kind == ValueKind::String) {
            const auto found = object->table->fields.find(key.text);
            if (found != object->table->fields.end()) {
                result = found->second;
            }
        } else if (key.kind == ValueKind::Number) {
            const auto found = object->table->numeric.find(number_key(key.number));
            if (found != object->table->numeric.end()) {
                result = found->second;
            }
        }
    }
    current.stack.push_back(std::move(result));
}

void fake_raw_set_i(lua_State*, int, int) {}

void fake_create_table(lua_State* state, int, int) {
    fixture(state).stack.push_back(FakeValue::table_value(std::make_shared<FakeTable>()));
}

void fake_set_field(lua_State* state, int index, const char* key) {
    FakeState& current = fixture(state);
    const FakeValue value = current.stack.back();
    current.stack.pop_back();
    FakeValue* object = stack_value(current, index);
    if (object == nullptr || object->kind != ValueKind::Table || object->table == nullptr ||
        key == nullptr) {
        return;
    }
    if (value.kind == ValueKind::Nil) {
        object->table->fields.erase(key);
        return;
    }
    object->table->fields[key] = value;
}

std::size_t fake_object_length(lua_State*, int) { return 0; }

void fake_push_c_closure(lua_State* state, LuaApi::CFunction function, int) {
    fixture(state).stack.push_back(FakeValue::function_value(function));
}

int fake_next(lua_State* state, int index) {
    FakeState& current = fixture(state);
    const FakeValue* object = stack_value(current, index);
    if (object == nullptr || object->kind != ValueKind::Table || object->table == nullptr) {
        return 0;
    }
    const FakeValue key = current.stack.back();
    std::size_t next = 0;
    if (key.kind != ValueKind::Nil) {
        const auto found = std::find_if(
            object->table->entries.begin(),
            object->table->entries.end(),
            [&key](const auto& entry) { return same_key(entry.first, key); });
        if (found == object->table->entries.end()) {
            current.stack.pop_back();
            return 0;
        }
        next = static_cast<std::size_t>(found - object->table->entries.begin()) + 1;
    }
    if (next >= object->table->entries.size()) {
        current.stack.pop_back();
        return 0;
    }
    current.stack.back() = object->table->entries[next].first;
    current.stack.push_back(object->table->entries[next].second);
    return 1;
}

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

void add_number_entry(FakeTable& table, std::uint64_t key, FakeValue value);

int return_true(lua_State* state) {
    fixture(state).stack.push_back(FakeValue::boolean_value(true));
    return 1;
}

int return_false(lua_State* state) {
    fixture(state).stack.push_back(FakeValue::boolean_value(false));
    return 1;
}

FakeValue empty_fleet_data() {
    auto fleet = std::make_shared<FakeTable>();
    fleet->fields["id"] = FakeValue::number_value(1);
    fleet->fields["name"] = FakeValue::string_value("fixture");
    fleet->fields["isRegularFleet"] = FakeValue::function_value(return_true);
    fleet->fields["isSubmarineFleet"] = FakeValue::function_value(return_false);
    fleet->fields["isPVPFleet"] = FakeValue::function_value(return_false);
    fleet->fields["mainShips"] = FakeValue::table_value(std::make_shared<FakeTable>());
    fleet->fields["vanguardShips"] = FakeValue::table_value(std::make_shared<FakeTable>());
    fleet->fields["subShips"] = FakeValue::table_value(std::make_shared<FakeTable>());
    auto data = std::make_shared<FakeTable>();
    add_number_entry(*data, 1, FakeValue::table_value(std::move(fleet)));
    return FakeValue::table_value(std::move(data));
}

int fake_capacity(lua_State* state) {
    fixture(state).stack.push_back(fixture(state).capacity_invalid ? FakeValue::nil() : FakeValue::number_value(7));
    return 1;
}
int fake_equipment_limit(lua_State* state) {
    fixture(state).stack.push_back(FakeValue::number_value(300));
    return 1;
}
int fake_player_data(lua_State* state) {
    auto data = std::make_shared<FakeTable>();
    data->fields["gold"] = FakeValue::number_value(1234);
    data->fields["getMaxEquipmentBag"] = FakeValue::function_value(fake_equipment_limit);
    fixture(state).stack.push_back(FakeValue::table_value(data));
    return 1;
}
int fake_get_proxy(lua_State* state) {
    FakeState& current = fixture(state);
    const FakeValue name = current.stack.empty() ? FakeValue::nil() : current.stack.back();
    std::shared_ptr<FakeTable> proxy = std::make_shared<FakeTable>();
    if (name.kind == ValueKind::String && name.text == "BayProxy") {
        require(!current.resources_only, "资源读取不得访问船坞");
        proxy->fields["data"] = FakeValue::table_value(current.dock);
    } else if (name.kind == ValueKind::String && name.text == "EquipmentProxy") {
        proxy->fields["getCapacity"] = FakeValue::function_value(fake_capacity);
        auto data = std::make_shared<FakeTable>();
        data->fields["equipments"] = FakeValue::table_value(current.warehouse);
        proxy->fields["data"] = FakeValue::table_value(data);
    } else if (name.kind == ValueKind::String && name.text == "PlayerProxy") {
        proxy->fields["getData"] = FakeValue::function_value(fake_player_data);
    } else if (name.kind == ValueKind::String && name.text == "FleetProxy") {
        proxy->fields["data"] = empty_fleet_data();
    }
    current.stack.push_back(FakeValue::table_value(std::move(proxy)));
    return 1;
}

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

void add_number_entry(FakeTable& table, std::uint64_t key, FakeValue value) {
    table.numeric[key] = value;
    table.entries.emplace_back(FakeValue::number_value(static_cast<double>(key)), std::move(value));
}

void add_string_entry(FakeTable& table, std::string key, FakeValue value) {
    table.entries.emplace_back(FakeValue::string_value(std::move(key)), std::move(value));
}

FakeValue ship_record(std::uint64_t ship_id) {
    auto ship = std::make_shared<FakeTable>();
    ship->fields["id"] = FakeValue::number_value(static_cast<double>(ship_id));
    ship->fields["configId"] = FakeValue::number_value(1);
    ship->fields["level"] = FakeValue::number_value(1);
    ship->fields["exp"] = FakeValue::number_value(0);
    ship->fields["intimacy"] = FakeValue::number_value(0);
    ship->fields["energy"] = FakeValue::number_value(0);
    ship->fields["proficiency"] = FakeValue::number_value(0);
    ship->fields["skills"] = FakeValue::table_value(std::make_shared<FakeTable>());
    ship->fields["equipments"] = FakeValue::table_value(std::make_shared<FakeTable>());
    return FakeValue::table_value(std::move(ship));
}

void add_ships(FakeTable& dock, std::uint64_t first, std::uint64_t count) {
    for (std::uint64_t offset = 0; offset < count; ++offset) {
        const std::uint64_t ship_id = first + offset;
        add_number_entry(dock, ship_id, ship_record(ship_id));
    }
}

FakeState make_state() {
    FakeState state;
    state.dock = std::make_shared<FakeTable>();
    state.warehouse = std::make_shared<FakeTable>();
    state.globals["EquipmentProxy"] = FakeValue::string_value("EquipmentProxy");
    state.globals["getProxy"] = FakeValue::function_value(fake_get_proxy);
    state.globals["BayProxy"] = FakeValue::string_value("BayProxy");
    state.globals["FleetProxy"] = FakeValue::string_value("FleetProxy");
    return state;
}

struct DockReport {
    std::uint32_t frames = 0;
    std::uint32_t cursor = 0;
    std::size_t ships = 0;
    bool truncated = false;
    bool dock_finished = false;
    bool failed = false;
    std::string fail_code;
    std::vector<std::string> error_codes;
    std::vector<std::uint64_t> ship_ids;
};

bool contains_code(const DockReport& report, const std::string& code) {
    return std::find(report.error_codes.begin(), report.error_codes.end(), code) !=
           report.error_codes.end();
}

DockReport read_dock(FakeState& state, std::uint32_t max_ships) {
    AccountBeforeProgress progress;
    progress.max_ships = max_ships;
    progress.max_equipments = 1;
    progress.max_items = 1;
    progress.module_sha256 = std::string(64, 'a');
    AccountBeforeExecution execution;
    const LuaApi api = make_api();
    DockReport report;
    for (std::uint32_t guard = 0; guard < 80; ++guard) {
        const AccountBeforeStep step = azlw::agent::advance_account_before(
            api, reinterpret_cast<lua_State*>(&state), &progress, &execution);
        if (step == AccountBeforeStep::Finished) {
            report.failed = true;
            report.fail_code = execution.error.code;
            break;
        }
        if (progress.phase != AccountBeforePhase::Dock) {
            report.dock_finished = true;
            break;
        }
    }
    report.frames = progress.dock_frames;
    report.cursor = progress.dock_cursor;
    report.ships = progress.owned.dock.ships.size();
    report.truncated = progress.owned.dock.truncated;
    report.error_codes.reserve(progress.owned.dock.read_errors.size());
    for (const auto& error : progress.owned.dock.read_errors) {
        report.error_codes.push_back(error.code);
    }
    report.ship_ids.reserve(progress.owned.dock.ships.size());
    for (const auto& ship : progress.owned.dock.ships) {
        report.ship_ids.push_back(ship.ship_id);
    }
    return report;
}

void expect_page(
    const DockReport& report,
    const std::string& label,
    std::uint32_t frames,
    std::uint32_t cursor,
    std::size_t ships,
    bool truncated) {
    require(report.dock_finished, label + " 应在船坞结束后停止，失败码 " + report.fail_code);
    require(!report.failed, label + " 不应在船坞阶段失败，失败码 " + report.fail_code);
    require(report.frames == frames, label + " 帧数不符");
    require(report.cursor == cursor, label + " 游标不符");
    require(report.ships == ships, label + " 舰船数不符");
    require(report.truncated == truncated, label + " 截断标记不符");
}

void test_empty() {
    FakeState state = make_state();
    const DockReport report = read_dock(state, 33);
    expect_page(report, "空船坞", 1, 0, 0, false);
}

void test_exact_page() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, kMaximumDockPageSize);
    const DockReport report = read_dock(state, 2000);
    expect_page(report, "正好一页", 1, kMaximumDockPageSize, kMaximumDockPageSize, false);
}

void test_one_past_page() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, kMaximumDockPageSize + 1);
    const DockReport report = read_dock(state, 2000);
    expect_page(
        report,
        "多出一艘",
        2,
        kMaximumDockPageSize + 1,
        kMaximumDockPageSize + 1,
        false);
}

void test_requested_cap() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, 40);
    const DockReport report = read_dock(state, 33);
    expect_page(report, "上限 33", 2, 33, 33, true);
    require(report.ship_ids.front() == 1 && report.ship_ids.back() == 33, "上限 33 的舰船范围不符");
}

void test_cap_plus_one_on_page_boundary() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, kMaximumDockPageSize + 1);
    const DockReport report = read_dock(state, kMaximumDockPageSize);
    expect_page(report, "页边界上限", 1, kMaximumDockPageSize, kMaximumDockPageSize, true);
}

void test_snapshot_limit() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, 2000);
    const DockReport report = read_dock(state, 2000);
    expect_page(report, "2000 艘", 63, 2000, 2000, false);

    FakeState overflow = make_state();
    add_ships(*overflow.dock, 1, 2001);
    const DockReport limited = read_dock(overflow, 2000);
    expect_page(limited, "2001 艘", 63, 2000, 2000, true);
}

void test_invalid_key_page_stops() {
    FakeState state = make_state();
    for (int index = 0; index < static_cast<int>(kMaximumDockPageSize); ++index) {
        add_string_entry(*state.dock, "bad-" + std::to_string(index), ship_record(1));
    }
    add_ships(*state.dock, 100, 5);
    const DockReport report = read_dock(state, 2000);
    expect_page(report, "连续无效键", 1, kMaximumDockPageSize, 0, true);
    require(contains_code(report, "ship_id_invalid"), "连续无效键应记录身份错误");
    require(
        std::find(report.ship_ids.begin(), report.ship_ids.end(), 100) == report.ship_ids.end(),
        "无效键页之后的舰船不应被读取");
}

void test_anchor_removed() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, 40);
    AccountBeforeProgress progress;
    progress.max_ships = 2000;
    progress.max_equipments = 1;
    progress.max_items = 1;
    progress.module_sha256 = std::string(64, 'a');
    AccountBeforeExecution execution;
    const LuaApi api = make_api();
    const AccountBeforeStep first = azlw::agent::advance_account_before(
        api, reinterpret_cast<lua_State*>(&state), &progress, &execution);
    require(first == AccountBeforeStep::Continue, "第一页应继续");
    require(progress.phase == AccountBeforePhase::Dock, "第一页后仍应停留在船坞");
    require(progress.dock_cursor == kMaximumDockPageSize, "第一页游标应为页容量");
    state.dock->numeric.erase(kMaximumDockPageSize);
    state.dock->entries.erase(
        std::remove_if(
            state.dock->entries.begin(),
            state.dock->entries.end(),
            [](const auto& entry) {
                return entry.first.kind == ValueKind::Number &&
                       entry.first.number == static_cast<double>(kMaximumDockPageSize);
            }),
        state.dock->entries.end());
    const AccountBeforeStep second = azlw::agent::advance_account_before(
        api, reinterpret_cast<lua_State*>(&state), &progress, &execution);
    require(second == AccountBeforeStep::Continue, "锚点丢失后应结束船坞而不是失败退出");
    require(progress.phase == AccountBeforePhase::Warehouse, "锚点丢失后不应再读船坞");
    require(progress.dock_frames == 2, "锚点丢失仍消耗当前帧");
    require(progress.dock_cursor == kMaximumDockPageSize, "锚点丢失不应继续累计后续舰船");
    require(progress.owned.dock.ships.size() == kMaximumDockPageSize, "已读舰船应保持第一页");
    const bool recorded = std::any_of(
        progress.owned.dock.read_errors.begin(),
        progress.owned.dock.read_errors.end(),
        [](const auto& error) { return error.code == "ship_page_anchor_missing"; });
    require(recorded, "锚点丢失应记录 ship_page_anchor_missing");
}

void test_table_replaced_between_frames() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, 40);
    AccountBeforeProgress progress;
    progress.max_ships = 2000;
    progress.max_equipments = 1;
    progress.max_items = 1;
    progress.module_sha256 = std::string(64, 'a');
    AccountBeforeExecution execution;
    const LuaApi api = make_api();
    require(
        azlw::agent::advance_account_before(
            api, reinterpret_cast<lua_State*>(&state), &progress, &execution) ==
            AccountBeforeStep::Continue,
        "换表前第一页应继续");
    auto replaced = std::make_shared<FakeTable>();
    add_ships(*replaced, 500, 8);
    state.dock = std::move(replaced);
    const AccountBeforeStep second = azlw::agent::advance_account_before(
        api, reinterpret_cast<lua_State*>(&state), &progress, &execution);
    require(second == AccountBeforeStep::Continue, "整表替换后应结束船坞");
    require(progress.phase == AccountBeforePhase::Warehouse, "丢失旧锚点后不应扫描新表");
    require(progress.owned.dock.ships.size() == kMaximumDockPageSize, "新表中的舰船不应被补读");
    const bool recorded = std::any_of(
        progress.owned.dock.read_errors.begin(),
        progress.owned.dock.read_errors.end(),
        [](const auto& error) { return error.code == "ship_page_anchor_missing"; });
    require(recorded, "整表替换应记录锚点丢失");
}

void test_owned_query_selects_ids_and_fields() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, 80);
    auto& selected = state.dock->numeric.at(7).table->fields;
    selected["name"] = FakeValue::string_value("测试舰船");
    // 未请求的养成、技能、槽位和编队全部故意缺失，基础查询仍须成功。
    selected.erase("exp"); selected.erase("skills"); selected.erase("equipments");
    state.globals.erase("FleetProxy");
    // 无关实例身份畸形不能影响直接 ID 查询。
    state.dock->numeric.at(1).table->fields.erase("id");
    azlw::agent::OwnedQueryProgress progress;
    progress.query = {.kind = "ships", .ids = {7, 900}, .fields = {}};
    azlw::agent::OwnedQueryExecution execution;
    const auto api = make_api();
    require(azlw::agent::advance_owned_query(api, reinterpret_cast<lua_State*>(&state), &progress, &execution), "两个显式 ID 应在一帧结束");
    require(execution.success, "基础查询应跳过未请求的字段");
    require(execution.ships.size() == 1 && execution.ships[0].ship.ship_id == 7, "只能返回选中的实例");
    require(execution.ships[0].name == "测试舰船", "应读取舰船名称");
    require(execution.missing_ids == std::vector<std::uint64_t>{900}, "应明确返回缺失 ID");
    require(state.stack.empty(), "查询必须恢复 Lua 栈");

    progress = {};
    progress.query = {.kind = "ships", .ids = {7}, .fields = {"skills"}};
    execution = {};
    require(azlw::agent::advance_owned_query(api, reinterpret_cast<lua_State*>(&state), &progress, &execution), "字段错误应结束查询");
    require(!execution.success && !execution.error.code.empty(), "显式请求缺失技能必须保留读取错误");
}

void test_owned_equipment_query_merges_locations() {
    FakeState state = make_state();
    add_ships(*state.dock, 7, 1);
    state.dock->numeric.at(7).table->fields.erase("configId");
    state.dock->numeric.at(7).table->fields.erase("level");
    auto equipment = [](std::uint64_t id, std::uint64_t config, std::uint64_t count) {
        auto object = std::make_shared<FakeTable>();
        object->fields["id"] = FakeValue::number_value(id);
        object->fields["configId"] = FakeValue::number_value(config);
        object->fields["count"] = FakeValue::number_value(count);
        return FakeValue::table_value(object);
    };
    add_number_entry(*state.warehouse, 1000, equipment(1000, 100, 3));
    add_number_entry(*state.warehouse, 1001, equipment(1001, 100, 2));
    auto& slots = *state.dock->numeric.at(7).table->fields["equipments"].table;
    add_number_entry(slots, 1, equipment(700, 100, 0));
    add_number_entry(slots, 2, equipment(701, 200, 0));
    azlw::agent::OwnedQueryProgress progress;
    progress.query = {.kind = "equipment", .ids = {100, 200, 999}, .fields = {"warehouse_quantity", "equipped"}};
    azlw::agent::OwnedQueryExecution execution;
    const auto api = make_api();
    require(!azlw::agent::advance_owned_query(api, reinterpret_cast<lua_State*>(&state), &progress, &execution), "仓库完成后应在下一帧读取槽位");
    require(azlw::agent::advance_owned_query(api, reinterpret_cast<lua_State*>(&state), &progress, &execution), "槽位读取完成应结束查询");
    require(execution.success && execution.equipment.size() == 2, "应按配置 ID 合并仓库与装载装备");
    require(execution.equipment[0].warehouse_quantity == 5, "同配置仓库数量应累加");
    require(execution.equipment[0].equipped.size() == 1 && execution.equipment[0].equipped[0].equipment_id == 700, "应返回实际装载实例");
    require(execution.equipment[1].warehouse_quantity == 0 && execution.equipment[1].equipped[0].slot_index == 2, "仅装载的配置也必须返回");
    require(execution.missing_ids == std::vector<std::uint64_t>{999}, "缺失配置应明确列出");
    require(state.stack.empty(), "装备查询必须恢复 Lua 栈");
}

void test_owned_query_all_is_paged() {
    FakeState state = make_state();
    add_ships(*state.dock, 1, 80);
    azlw::agent::OwnedQueryProgress progress;
    progress.query = {.kind = "ships", .ids = {}, .fields = {"level"}};
    azlw::agent::OwnedQueryExecution execution;
    const auto api = make_api();
    require(!azlw::agent::advance_owned_query(api, reinterpret_cast<lua_State*>(&state), &progress, &execution), "全船坞第一页应续办");
    require(execution.ships.size() == kMaximumDockPageSize, "一帧不能读取超过页容量");
    std::uint32_t frames = 1;
    while (!azlw::agent::advance_owned_query(api, reinterpret_cast<lua_State*>(&state), &progress, &execution)) {
        require(++frames < 10, "全船坞查询应有限结束");
    }
    require(execution.success && execution.ships.size() == 80, "跨帧查询应覆盖全部实例");
    require(state.stack.empty(), "跨帧查询必须恢复 Lua 栈");
}

void test_account_before_readiness_follows_completed_details() {
    using azlw::agent::AccountBeforeReadiness;
    using azlw::agent::account_before_readiness;
    AccountBeforeExecution failed;
    failed.owned.bag.complete = true;
    failed.owned.complete = true;
    failed.details.complete = true;
    const AccountBeforeReadiness failed_ready = account_before_readiness(failed);
    require(!failed_ready.bag, "失败不能发布背包就绪");
    require(!failed_ready.owned_state, "失败不能发布养成就绪");
    require(!failed_ready.ship_details, "失败不能发布舰船详情就绪");

    AccountBeforeExecution partial;
    partial.success = true;
    partial.owned.bag.complete = true;
    partial.owned.complete = true;
    const AccountBeforeReadiness partial_ready = account_before_readiness(partial);
    require(partial_ready.bag && partial_ready.owned_state, "完整养成应发布背包和养成就绪");
    require(!partial_ready.ship_details, "详情不完整不能发布舰船详情就绪");

    partial.details.complete = true;
    const AccountBeforeReadiness complete_ready = account_before_readiness(partial);
    require(complete_ready.ship_details, "成功且详情完整应发布舰船详情就绪");
}

}  // namespace

int main() {
    {
        auto state = make_state();
        state.resources_only = true;
        state.globals["PlayerProxy"] = FakeValue::string_value("PlayerProxy");
        const auto result = azlw::agent::snapshot_resources(make_api(), reinterpret_cast<lua_State*>(&state));
        require(result.success && result.player.gold == 1234 && result.player.equipment_capacity == 7 && result.player.equipment_limit == 300, "资源应只通过容量与玩家代理读取");
        require(state.stack.empty(), "资源读取必须恢复栈");
        state.capacity_invalid = true;
        const auto failed = azlw::agent::snapshot_resources(make_api(), reinterpret_cast<lua_State*>(&state));
        require(!failed.success && failed.error.code == "lua_equipment_capacity_invalid", "容量失败应保留错误原因");
        require(state.stack.empty(), "资源读取失败必须恢复栈");
    }

    require(kMaximumDockPageSize == 32, "船坞页容量应为契约中的 32");
    test_owned_equipment_query_merges_locations();
    test_owned_query_selects_ids_and_fields();
    test_owned_query_all_is_paged();
    test_empty();
    test_exact_page();
    test_one_past_page();
    test_requested_cap();
    test_cap_plus_one_on_page_boundary();
    test_snapshot_limit();
    test_invalid_key_page_stops();
    test_anchor_removed();
    test_table_replaced_between_frames();
    test_account_before_readiness_follows_completed_details();
    return 0;
}

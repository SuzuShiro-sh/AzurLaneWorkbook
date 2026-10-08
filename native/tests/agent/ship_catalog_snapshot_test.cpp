// 验证 confNEO 空代理经 pg.base 物理行、延迟索引和 base 继承后的完整物化。

#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <stdexcept>
#include <utility>
#include <vector>

#include "snapshots/ship_catalog_snapshot.h"

namespace {

using azlw::agent::LuaApi;
using azlw::agent::LuaValue;
using azlw::agent::LuaValueKind;
using azlw::agent::ShipCatalogPageExecution;

enum class ValueKind { Nil, Number, String, Function, Table };

struct FakeTable;

struct FakeValue final {
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

struct FakeTable final {
    std::map<std::string, FakeValue> fields;
    std::map<std::uint64_t, FakeValue> numeric;
    std::vector<std::pair<FakeValue, FakeValue>> entries;
    std::shared_ptr<FakeTable> inherited;
    std::vector<std::string> hidden_fields;
    bool config_proxy = false;
};

struct FakeState final {
    std::vector<FakeValue> stack;
    std::map<std::string, FakeValue> registry;
    int protected_depth = 0;
    std::string fault_operation;
    bool stack_available = true;
    int heap_kib = 0;
    int collected_kib = 0;
    bool gc_running = true;
    std::map<std::string, FakeValue> globals;
    std::map<std::uint64_t, std::shared_ptr<FakeTable>> proxy_rows;
    std::size_t lazy_proxy_reads = 0;
    std::size_t next_calls = 0;
};

void require(bool condition, const std::string& message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << '\n';
        std::exit(1);
    }
}

FakeState& fixture(lua_State* state) { return *reinterpret_cast<FakeState*>(state); }

void allocation_point(lua_State* state, const char* operation) {
    auto& current = fixture(state);
    require(current.protected_depth > 0, std::string(operation) + " outside protected call");
    if (current.fault_operation == operation) {
        current.fault_operation.clear();
        throw std::runtime_error(std::string("fixture GC failure: ") + operation);
    }
}


const FakeValue* stack_value(const FakeState& state, int index) {
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

FakeValue* mutable_stack_value(FakeState& state, int index) {
    return const_cast<FakeValue*>(stack_value(state, index));
}

bool same_key(const FakeValue& left, const FakeValue& right) {
    if (left.kind != right.kind) {
        return false;
    }
    if (left.kind == ValueKind::Nil) {
        return true;
    }
    if (left.kind == ValueKind::Number) {
        return left.number == right.number;
    }
    return left.kind == ValueKind::String && left.text == right.text;
}

std::uint64_t number_key(const FakeValue& value) {
    if (value.kind != ValueKind::Number || !std::isfinite(value.number) || value.number < 0 ||
        std::floor(value.number) != value.number) {
        return 0;
    }
    return static_cast<std::uint64_t>(value.number);
}

FakeValue lookup_field(const std::shared_ptr<FakeTable>& table, const std::string& key) {
    if (table == nullptr ||
        std::find(table->hidden_fields.begin(), table->hidden_fields.end(), key) !=
            table->hidden_fields.end()) {
        return FakeValue::nil();
    }
    const auto found = table->fields.find(key);
    if (found != table->fields.end()) {
        return found->second;
    }
    return table->inherited == nullptr ? FakeValue::nil() : lookup_field(table->inherited, key);
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
        case ValueKind::Nil:
            return azlw::agent::kLuaTypeNil;
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
    allocation_point(state, "fake_push_string");
    fixture(state).stack.push_back(FakeValue::string_value(value == nullptr ? "" : value));

}

void fake_push_boolean(lua_State*, int) {}

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
            result = lookup_field(object->table, key.text);
        } else if (key.kind == ValueKind::Number) {
            const std::uint64_t numeric_key = number_key(key);
            if (object->table->config_proxy) {
                ++current.lazy_proxy_reads;
                const auto found = current.proxy_rows.find(numeric_key);
                if (found != current.proxy_rows.end()) {
                    result = FakeValue::table_value(found->second);
                }
            } else if (const auto found = object->table->numeric.find(numeric_key);
                       found != object->table->numeric.end()) {
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
            const auto found = object->table->numeric.find(number_key(key));
            if (found != object->table->numeric.end()) {
                result = found->second;
            }
        }
    }
    current.stack.push_back(std::move(result));
}

void fake_raw_set_i(lua_State*, int, int) {}

void fake_create_table(lua_State* state, int, int) {
    allocation_point(state, "fake_create_table");
    fixture(state).stack.push_back(FakeValue::table_value(std::make_shared<FakeTable>()));
}

void fake_set_field(lua_State* state, int index, const char* key) {
    allocation_point(state, "fake_set_field");
    FakeState& current = fixture(state);
    const FakeValue value = current.stack.back();
    current.stack.pop_back();
    FakeValue* object = mutable_stack_value(current, index);
    if (object == nullptr || object->kind != ValueKind::Table || object->table == nullptr) {
        return;
    }
    auto& entries = object->table->entries;
    entries.erase(std::remove_if(entries.begin(),
                                 entries.end(),
                                 [key](const auto& entry) {
                                     return entry.first.kind == ValueKind::String &&
                                            entry.first.text == key;
                                 }),
                  entries.end());
    object->table->fields.erase(key);
    if (value.kind != ValueKind::Nil) {
        object->table->fields[key] = value;
        entries.emplace_back(FakeValue::string_value(key), value);
    }
}

std::size_t fake_object_length(lua_State* state, int index) {
    const FakeValue* value = stack_value(fixture(state), index);
    return value != nullptr && value->kind == ValueKind::Table && value->table != nullptr
               ? value->table->numeric.size()
               : 0;
}

void fake_push_c_closure(lua_State* state, LuaApi::CFunction function, int) {
    allocation_point(state, "fake_push_c_closure");
    fixture(state).stack.push_back(FakeValue::function_value(function));
}

int fake_next(lua_State* state, int index) {
    FakeState& current = fixture(state);
    current.next_calls += 1;
    const FakeValue* object = stack_value(current, index);
    if (object == nullptr || object->kind != ValueKind::Table || object->table == nullptr) {
        return 0;
    }
    const FakeValue key = current.stack.back();
    std::size_t next = 0;
    if (key.kind != ValueKind::Nil) {
        const auto found =
            std::find_if(object->table->entries.begin(),
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

LuaApi make_api() {
    return LuaApi{
        .get_top = fake_get_top,
        .set_top = fake_set_top,
        .type = fake_type,
        .to_boolean = fake_to_boolean,
        .get_table = fake_get_table,
        .raw_get = fake_raw_get,
        .raw_set_i_unchecked = fake_raw_set_i,
        .create_table_unchecked = fake_create_table,
        .set_field_unchecked = fake_set_field,
        .object_length = fake_object_length,
        .push_c_closure = fake_push_c_closure,
        .push_boolean = fake_push_boolean,
        .push_string_unchecked = fake_push_string,
        .push_number = fake_push_number,
        .push_value = fake_push_value,
        .push_nil = fake_push_nil,
        .next = fake_next,
        .to_string = fake_to_string,
        .to_number = fake_to_number,
        .to_pointer = fake_to_pointer,
        .protected_call = fake_protected_call,
        .protected_c_call = fake_protected_c_call,
        .push_light_userdata = fake_push_light_userdata,
        .raw_set = fake_raw_set,
        .check_stack = fake_check_stack,
        .garbage_collect = [](lua_State* state, int operation, int amount) {
            if (operation == 3) return fixture(state).heap_kib;
            if (operation == 9) return static_cast<int>(fixture(state).gc_running);
            if (operation == 0) {
                fixture(state).gc_running = false;
                return 0;
            }
            require(operation == 5, "采集只能读取内存或推进增量回收");
            fixture(state).gc_running = true;
            allocation_point(state, "fake_gc_step");
            fixture(state).collected_kib += amount;
            return 0;
        },
    };
}

void add_field(const std::shared_ptr<FakeTable>& table, const std::string& key, FakeValue value) {
    table->fields[key] = value;
    table->entries.emplace_back(FakeValue::string_value(key), std::move(value));
}

void add_numeric(const std::shared_ptr<FakeTable>& table, std::uint64_t key, FakeValue value) {
    table->numeric[key] = value;
    table->entries.emplace_back(FakeValue::number_value(static_cast<double>(key)),
                                std::move(value));
}

struct CatalogFixture final {
    FakeState state;
    std::shared_ptr<FakeTable> config = std::make_shared<FakeTable>();
    std::shared_ptr<FakeTable> physical = std::make_shared<FakeTable>();
    std::shared_ptr<FakeTable> base = std::make_shared<FakeTable>();

    CatalogFixture() {
        config->config_proxy = true;
        config->fields["__name"] = FakeValue::string_value("fixture_source");
        auto all = std::make_shared<FakeTable>();
        config->fields["all"] = FakeValue::table_value(all);

        base->fields["fixture_source"] = FakeValue::table_value(physical);
        auto pg = std::make_shared<FakeTable>();
        pg->fields["ship_data_group"] = FakeValue::table_value(config);
        pg->fields["base"] = FakeValue::table_value(base);
        state.globals["pg"] = FakeValue::table_value(pg);
    }

    void add_id(std::uint64_t id) {
        auto all = config->fields.at("all").table;
        add_numeric(all, all->numeric.size() + 1, FakeValue::number_value(id));
    }

    std::shared_ptr<FakeTable> add_row(std::uint64_t id) {
        auto row = std::make_shared<FakeTable>();
        add_numeric(physical, id, FakeValue::table_value(row));
        return row;
    }

    ShipCatalogPageExecution read() {
        return azlw::agent::snapshot_ship_catalog(make_api(),
                                                  reinterpret_cast<lua_State*>(&state),
                                                  "ship_data_group",
                                                  0,
                                                  32,
                                                  std::string(64, 'a'));
    }
};

const LuaValue& field(const LuaValue& object, const std::string& key) {
    for (std::size_t index = 0; index < object.keys.size(); ++index) {
        if (object.keys[index].text == key) {
            return object.values[index];
        }
    }
    require(false, "materialized object is missing field " + key);
    return object;
}

void test_empty_proxy_materializes_physical_base_chain() {
    CatalogFixture fixture;
    fixture.add_id(101);
    const auto base_row = fixture.add_row(100);
    add_field(base_row, "id", FakeValue::number_value(100));
    add_field(base_row, "base", FakeValue::number_value(0));
    add_field(base_row, "nation", FakeValue::number_value(1));
    add_field(base_row, "rarity", FakeValue::number_value(4));
    const auto child_row = fixture.add_row(101);
    add_field(child_row, "id", FakeValue::number_value(101));
    add_field(child_row, "base", FakeValue::number_value(100));
    add_field(child_row, "name", FakeValue::string_value("child"));

    auto base_proxy = std::make_shared<FakeTable>();
    base_proxy->fields = base_row->fields;
    auto child_proxy = std::make_shared<FakeTable>();
    child_proxy->fields = child_row->fields;
    child_proxy->inherited = base_proxy;
    fixture.state.proxy_rows[100] = base_proxy;
    fixture.state.proxy_rows[101] = child_proxy;

    const ShipCatalogPageExecution execution = fixture.read();
    require(execution.success, "valid lazy catalog fixture returned an operation error");
    require(execution.page.complete, "valid lazy catalog page was incomplete");
    require(execution.page.records.size() == 1, "valid lazy catalog page lost its record");
    require(fixture.config->numeric.empty(), "fixture config proxy unexpectedly stored raw rows");
    require(fixture.state.lazy_proxy_reads >= 3, "catalog did not trigger lazy proxy reads");
    const LuaValue& raw = execution.page.records.front().raw;
    require(raw.kind == LuaValueKind::Object && raw.keys.size() == 5,
            "materialized inherited row did not contain the physical field union");
    require(field(raw, "id").number == 101 && field(raw, "base").number == 100,
            "child fields did not override inherited values");
    require(field(raw, "nation").number == 1 && field(raw, "rarity").number == 4,
            "base-only fields were not inherited");
    require(field(raw, "name").text == "child", "child string field was not retained");
    require(fixture.state.stack.empty(), "successful snapshot did not restore the Lua stack");
}

void test_base_cycle_is_reported_as_incomplete() {
    CatalogFixture fixture;
    fixture.add_id(200);
    const auto first = fixture.add_row(200);
    add_field(first, "id", FakeValue::number_value(200));
    add_field(first, "base", FakeValue::number_value(201));
    const auto second = fixture.add_row(201);
    add_field(second, "id", FakeValue::number_value(201));
    add_field(second, "base", FakeValue::number_value(200));
    fixture.state.proxy_rows[200] = std::make_shared<FakeTable>();
    fixture.state.proxy_rows[201] = std::make_shared<FakeTable>();

    const ShipCatalogPageExecution execution = fixture.read();
    require(execution.success && !execution.page.complete,
            "base cycle did not produce a valid incomplete page");
    require(execution.page.records.empty() && execution.page.read_errors.size() == 1,
            "base cycle did not remain localized to its catalog record");
    require(execution.page.read_errors.front().message.find("循环") != std::string::npos,
            "base cycle diagnostic was not specific");
}

void test_missing_physical_row_is_reported_as_incomplete() {
    CatalogFixture fixture;
    fixture.add_id(300);
    fixture.state.proxy_rows[300] = std::make_shared<FakeTable>();

    const ShipCatalogPageExecution execution = fixture.read();
    require(execution.success && !execution.page.complete,
            "missing physical row did not produce a valid incomplete page");
    require(execution.page.read_errors.size() == 1 &&
                execution.page.read_errors.front().message.find("物理来源") != std::string::npos,
            "missing physical row diagnostic was not specific");
}

void test_missing_proxy_field_is_reported_as_incomplete() {
    CatalogFixture fixture;
    fixture.add_id(400);
    const auto row = fixture.add_row(400);
    add_field(row, "id", FakeValue::number_value(400));
    add_field(row, "base", FakeValue::number_value(0));
    add_field(row, "required", FakeValue::string_value("present in physical row"));
    auto proxy = std::make_shared<FakeTable>();
    proxy->fields = row->fields;
    proxy->hidden_fields.push_back("required");
    fixture.state.proxy_rows[400] = proxy;

    const ShipCatalogPageExecution execution = fixture.read();
    require(execution.success && !execution.page.complete,
            "missing proxy field did not produce a valid incomplete page");
    require(execution.page.read_errors.size() == 1 &&
                execution.page.read_errors.front().message.find("字段数量") != std::string::npos,
            "missing proxy field diagnostic was not specific");
}

void test_invalid_source_metadata_reports_bounded_scalar_details() {
    CatalogFixture fixture;
    fixture.config->fields["__name"] = FakeValue::number_value(42);

    const ShipCatalogPageExecution execution = fixture.read();
    require(!execution.success, "invalid source metadata unexpectedly succeeded");
    const std::string& message = execution.error.message;
    require(message.find("table_key=ship_data_group") != std::string::npos,
            "source metadata diagnostic omitted table_key");
    require(message.find("__sub__{type=nil}") != std::string::npos,
            "source metadata diagnostic omitted raw __sub__ type");
    require(message.find("__name{type=number,value=42.000000}") != std::string::npos,
            "source metadata diagnostic omitted bounded scalar value");
    require(message.find("first_all_id") == std::string::npos,
            "row shape diagnostic ran when source metadata was not both nil");
    require(fixture.state.stack.empty(), "metadata failure did not restore the Lua stack");
}

void test_nil_source_metadata_uses_same_name_base_source() {
    CatalogFixture fixture;
    fixture.config->fields.erase("__name");
    fixture.add_id(501);

    auto base_table = std::make_shared<FakeTable>();
    auto base_row = std::make_shared<FakeTable>();
    add_field(base_row, "id", FakeValue::number_value(501));
    add_field(base_row, "base", FakeValue::number_value(0));
    add_field(base_row, "name", FakeValue::string_value("diagnostic row"));
    add_numeric(base_table, 501, FakeValue::table_value(base_row));
    fixture.base->fields["ship_data_group"] = FakeValue::table_value(base_table);

    auto proxy_row = std::make_shared<FakeTable>();
    proxy_row->fields = base_row->fields;
    fixture.state.proxy_rows[501] = proxy_row;

    const ShipCatalogPageExecution execution = fixture.read();
    require(execution.success && execution.page.complete,
            "same-name base source did not materialize a complete page");
    require(execution.page.records.size() == 1 &&
                execution.page.records.front().raw.keys.size() == 3,
            "same-name base source did not supply its complete field set");
    require(field(execution.page.records.front().raw, "name").text == "diagnostic row",
            "same-name base field was not read through the protected proxy");
    require(fixture.state.stack.empty(), "same-name source read did not restore the Lua stack");
}

void test_nil_source_metadata_without_same_name_base_fails() {
    CatalogFixture fixture;
    fixture.config->fields.erase("__name");
    fixture.add_id(502);
    fixture.state.proxy_rows[502] = std::make_shared<FakeTable>();

    const ShipCatalogPageExecution execution = fixture.read();
    require(!execution.success && execution.error.code == "ship_catalog_source_invalid",
            "missing same-name base source did not fail source validation");
    require(execution.error.message.find("pg.base.ship_data_group{type=nil}") !=
                std::string::npos,
            "missing same-name base source was not diagnosed precisely");
    require(fixture.state.stack.empty(), "missing source failure did not restore the Lua stack");
}

void test_partial_raw_proxy_uses_complete_same_name_base_fields() {
    CatalogFixture fixture;
    fixture.config->fields.erase("__name");
    fixture.add_id(503);

    auto base_table = std::make_shared<FakeTable>();
    auto base_row = std::make_shared<FakeTable>();
    add_field(base_row, "id", FakeValue::number_value(503));
    add_field(base_row, "base", FakeValue::number_value(0));
    for (std::uint64_t index = 0; index < 38; ++index) {
        add_field(base_row,
                  "field_" + std::to_string(index),
                  FakeValue::number_value(static_cast<double>(index)));
    }
    add_numeric(base_table, 503, FakeValue::table_value(base_row));
    fixture.base->fields["ship_data_group"] = FakeValue::table_value(base_table);

    auto proxy_row = std::make_shared<FakeTable>();
    add_field(proxy_row, "id", FakeValue::number_value(503));
    proxy_row->inherited = base_row;
    add_numeric(fixture.config, 503, FakeValue::table_value(proxy_row));
    fixture.state.proxy_rows[503] = proxy_row;

    const ShipCatalogPageExecution execution = fixture.read();
    require(execution.success && execution.page.complete,
            "partial raw proxy prevented protected materialization");
    require(proxy_row->entries.size() == 1,
            "partial raw proxy fixture did not retain the one-field boundary");
    require(execution.page.records.size() == 1 &&
                execution.page.records.front().raw.keys.size() == 40,
            "materialization used the partial raw proxy instead of the complete base fields");
    require(field(execution.page.records.front().raw, "field_37").number == 37,
            "complete same-name base field set was not read through the protected proxy");
    require(fixture.state.stack.empty(), "partial proxy read did not restore the Lua stack");
}

int fake_collection_proxy(lua_State* state) {
    fixture(state).stack.push_back(fixture(state).globals.at("collection_fixture"));
    return 1;
}

void test_collection_history_paging_and_missing_values() {
    CatalogFixture fixture;
    auto groups = std::make_shared<FakeTable>();
    auto proxy = std::make_shared<FakeTable>();
    add_field(proxy,"shipGroups",FakeValue::table_value(groups));
    fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
    fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
    fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
    auto read = [&](std::uint32_t start) {
        return azlw::agent::snapshot_ship_catalog(make_api(),reinterpret_cast<lua_State*>(&fixture.state),"collection_ship_group",start,1,std::string(64,'a'));
    };
    const auto empty = read(0);
    require(empty.success && empty.page.complete && empty.page.total_count == 0,"empty collection must be a complete history");
    std::shared_ptr<FakeTable> first;
    for (const std::uint64_t id : {200u,100u}) {
        auto row = std::make_shared<FakeTable>();
        add_field(row,"id",FakeValue::number_value(id));
        add_field(row,"star",FakeValue::number_value(5));
        add_field(row,"maxLV",FakeValue::number_value(120));
        add_field(row,"unrelated",FakeValue::string_value("not part of technology"));
        add_numeric(groups,id,FakeValue::table_value(row));
        if (id == 100) { first = row; }
    }
    const auto page = read(0);
    require(page.success && page.page.complete && page.page.records.size() == 1 && page.page.records[0].id == 100 && page.page.next_index == 1,"collection page must follow sorted group identifiers");
    require(page.page.records[0].raw.keys.size() == 3 && field(page.page.records[0].raw,"maxLV").number == 120,"collection must expose only history fields");
    const auto tail = read(1);
    require(tail.success && tail.page.records[0].id == 200 && !tail.page.next_index.has_value(),"collection tail pagination invalid");
    first->fields.erase("maxLV");
    require(!read(0).success,"missing historical level must not become zero");
    require(fixture.state.stack.empty(),"collection snapshot must restore stack on success and failure");
    fixture.state.globals.erase("getProxy");
    require(!read(0).success,"unavailable collection proxy must not become empty history");
}

void test_catalog_batch_crosses_frames_without_exceeding_the_frame_budget() {
    require(azlw::agent::kMaximumShipCatalogFrameSize == 32, "单帧容量应为 32");
    require(azlw::agent::kMaximumShipCatalogPageSize == 128, "单次响应页容量应为 128");
    CatalogFixture fixture;
    auto groups = std::make_shared<FakeTable>();
    auto proxy = std::make_shared<FakeTable>();
    add_field(proxy, "shipGroups", FakeValue::table_value(groups));
    fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
    fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
    fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
    for (std::uint64_t id = 1; id <= 40; ++id) {
        auto row = std::make_shared<FakeTable>();
        add_field(row, "id", FakeValue::number_value(id));
        add_field(row, "star", FakeValue::number_value(1));
        add_field(row, "maxLV", FakeValue::number_value(120));
        add_numeric(groups, id, FakeValue::table_value(row));
    }
    azlw::agent::ShipCatalogBatchProgress progress;
    progress.table_key = "collection_ship_group";
    progress.start_index = 0;
    progress.page_size = 40;
    progress.module_sha256 = std::string(64, 'a');
    progress.cursor = 0;
    const azlw::agent::LuaApi api = make_api();
    auto* state = reinterpret_cast<lua_State*>(&fixture.state);
    fixture.state.next_calls = 0;
    const azlw::agent::ShipCatalogBatchStep first =
        azlw::agent::advance_ship_catalog_batch(api, state, &progress);
    require(fixture.state.next_calls <= azlw::agent::kMaximumShipCatalogFrameSize,
            "第一帧键遍历超过帧容量");
    require(fixture.state.next_calls == 32, "第一帧应只前进 32 个键");
    require(
        first == azlw::agent::ShipCatalogBatchStep::Continue,
        "40 条目录的第一帧应续办，实际成功=" +
            std::string(progress.outcome.success ? "1" : "0") + " 游标=" +
            std::to_string(progress.cursor) + " 记录=" +
            std::to_string(progress.outcome.page.records.size()) + " 总数=" +
            std::to_string(progress.outcome.page.total_count) + " 错误=" +
            progress.outcome.error.code + " " + progress.outcome.error.message);
    require(progress.outcome.page.records.empty(), "键尚未遍历完时不能物化");
    std::size_t frames = 1;
    while (azlw::agent::advance_ship_catalog_batch(api, state, &progress) ==
           azlw::agent::ShipCatalogBatchStep::Continue) {
        frames += 1;
        require(frames < 8, "40 条目录的帧数失控");
    }
    frames += 1;
    require(progress.cursor == 40, "批次应覆盖全部 40 条");
    require(progress.outcome.page.records.size() == 40, "多帧结果应合并为 40 条");
    require(progress.outcome.page.records.front().id == 1, "结果应按组标识排序");
    require(!progress.outcome.page.next_index.has_value(), "读完表后不应再给下一页");
    require(progress.outcome.success && progress.outcome.page.complete, "完整批次应成功");
    require(fixture.state.stack.empty(), "跨帧读取后应恢复 Lua 栈");
}

void add_collection_row(const std::shared_ptr<FakeTable>& groups, std::uint64_t id) {
    auto row = std::make_shared<FakeTable>();
    add_field(row, "id", FakeValue::number_value(id));
    add_field(row, "star", FakeValue::number_value(1));
    add_field(row, "maxLV", FakeValue::number_value(120));
    add_numeric(groups, id, FakeValue::table_value(row));
}

void test_collection_index_is_reused_and_content_change_fails() {
    CatalogFixture fixture;
    auto groups = std::make_shared<FakeTable>();
    auto proxy = std::make_shared<FakeTable>();
    add_field(proxy, "shipGroups", FakeValue::table_value(groups));
    fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
    fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
    fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
    for (std::uint64_t id = 1; id <= 64; ++id) {
        add_collection_row(groups, id);
    }
    auto index = std::make_shared<azlw::agent::CollectionReadIndex>();
    azlw::agent::ShipCatalogBatchProgress first;
    first.table_key = "collection_ship_group";
    first.page_size = 32;
    first.module_sha256 = std::string(64, 'f');
    first.collection_index = index;
    const azlw::agent::LuaApi api = make_api();
    auto* state = reinterpret_cast<lua_State*>(&fixture.state);
    azlw::agent::ShipCatalogBatchStep step = azlw::agent::ShipCatalogBatchStep::Continue;
    for (int frame = 0; frame < 64 && step == azlw::agent::ShipCatalogBatchStep::Continue; ++frame) {
        step = azlw::agent::advance_ship_catalog_batch(api, state, &first);
    }
    require(step == azlw::agent::ShipCatalogBatchStep::Finished && first.outcome.success,
            "第一页应完成并留下索引");
    require(index->ready && index->identifiers.size() == 64, "索引应留给后续页");
    for (auto& entry : groups->entries) {
        if (entry.second.kind == ValueKind::Table && entry.second.table != nullptr) {
            entry.second.table->fields["maxLV"] = FakeValue::number_value(121);
        }
    }
    fixture.state.next_calls = 0;
    azlw::agent::ShipCatalogBatchProgress second;
    second.table_key = "collection_ship_group";
    second.start_index = 32;
    second.page_size = 32;
    second.cursor = 32;
    second.module_sha256 = first.module_sha256;
    second.collection_index = index;
    step = azlw::agent::ShipCatalogBatchStep::Continue;
    for (int frame = 0; frame < 64 && step == azlw::agent::ShipCatalogBatchStep::Continue; ++frame) {
        step = azlw::agent::advance_ship_catalog_batch(api, state, &second);
    }
    require(!second.outcome.success, "已物化记录的等级变化不能组成完整结果");
    require(second.outcome.error.code == "collection_content_changed", second.outcome.error.code);
}

void test_collection_frames_do_not_rescan_the_table() {
    CatalogFixture fixture;
    auto groups = std::make_shared<FakeTable>();
    auto proxy = std::make_shared<FakeTable>();
    add_field(proxy, "shipGroups", FakeValue::table_value(groups));
    fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
    fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
    fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
    constexpr std::uint64_t count = 64;
    for (std::uint64_t id = 1; id <= count; ++id) {
        add_collection_row(groups, id);
    }
    azlw::agent::ShipCatalogBatchProgress progress;
    progress.table_key = "collection_ship_group";
    progress.start_index = 0;
    progress.page_size = 64;
    progress.module_sha256 = std::string(64, 'b');
    progress.cursor = 0;
    const azlw::agent::LuaApi api = make_api();
    auto* state = reinterpret_cast<lua_State*>(&fixture.state);
    fixture.state.next_calls = 0;
    require(azlw::agent::advance_ship_catalog_batch(api, state, &progress) ==
                azlw::agent::ShipCatalogBatchStep::Continue,
            "图鉴第一帧应继续");
    require(fixture.state.next_calls == 32, "第一帧不能扫描整表");
    fixture.state.next_calls = 0;
    require(azlw::agent::advance_ship_catalog_batch(api, state, &progress) ==
                azlw::agent::ShipCatalogBatchStep::Continue,
            "图鉴第二帧应继续");
    require(fixture.state.next_calls == 32, "第二帧不能从头重扫");
    require(progress.outcome.page.records.empty(), "两帧只完成键遍历时还不能物化");

    auto replaced = std::make_shared<FakeTable>();
    for (std::uint64_t id = 1; id <= count; ++id) {
        add_collection_row(replaced, id + 1000);
    }
    proxy->fields["shipGroups"] = FakeValue::table_value(replaced);
    require(azlw::agent::advance_ship_catalog_batch(api, state, &progress) ==
                azlw::agent::ShipCatalogBatchStep::Finished,
            "替换图鉴表后应停止");
    require(!progress.outcome.success, "同数量的新表不能沿用旧游标");
    require(progress.outcome.error.code == "collection_groups_replaced",
            progress.outcome.error.code);

    azlw::agent::ShipCatalogBatchProgress other;
    other.table_key = "collection_ship_group";
    other.start_index = 0;
    other.page_size = 1;
    other.cursor = 0;
    other.module_sha256 = std::string(64, 'c');
    azlw::agent::ShipCatalogBatchStep other_step = azlw::agent::ShipCatalogBatchStep::Continue;
    for (int frame = 0; frame < 8 && other_step == azlw::agent::ShipCatalogBatchStep::Continue;
         ++frame) {
        other_step = azlw::agent::advance_ship_catalog_batch(api, state, &other);
    }
    require(other_step == azlw::agent::ShipCatalogBatchStep::Finished,
            "新请求应重新读取替换后的图鉴");
    require(other.outcome.success && other.outcome.page.total_count == count, "总数相同");
    require(other.outcome.page.records.size() == 1 &&
                other.outcome.page.records.front().id == 1001,
            "同数量换标识后应读到新标识");

    replaced->numeric.at(1001).table->fields["id"] = FakeValue::number_value(1);
    azlw::agent::ShipCatalogBatchProgress mismatch;
    mismatch.table_key = "collection_ship_group";
    mismatch.start_index = 0;
    mismatch.page_size = 1;
    mismatch.cursor = 0;
    mismatch.module_sha256 = std::string(64, 'e');
    azlw::agent::ShipCatalogBatchStep mismatch_step = azlw::agent::ShipCatalogBatchStep::Continue;
    for (int frame = 0; frame < 8 && mismatch_step == azlw::agent::ShipCatalogBatchStep::Continue;
         ++frame) {
        mismatch_step = azlw::agent::advance_ship_catalog_batch(api, state, &mismatch);
    }
    require(mismatch_step == azlw::agent::ShipCatalogBatchStep::Finished, "标识替换应结束请求");
    require(!mismatch.outcome.success, "组内标识变化不能当成原记录");
    require(mismatch.outcome.error.code == "collection_group_field_invalid",
            mismatch.outcome.error.code);

    azlw::agent::ShipCatalogBatchProgress changed;
    changed.table_key = "collection_ship_group";
    changed.start_index = 0;
    changed.page_size = 64;
    changed.cursor = 80;
    changed.module_sha256 = std::string(64, 'd');
    require(azlw::agent::advance_ship_catalog_batch(api, state, &changed) ==
                azlw::agent::ShipCatalogBatchStep::Finished,
            "失效游标应立即结束");
    require(!changed.outcome.success, "越过页容量的游标不能继续");
}

void test_four_thousand_collection_keys_are_scanned_once() {
    CatalogFixture fixture;
    auto groups = std::make_shared<FakeTable>();
    auto proxy = std::make_shared<FakeTable>();
    add_field(proxy, "shipGroups", FakeValue::table_value(groups));
    fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
    fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
    fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
    constexpr std::uint64_t count = 4000;
    for (std::uint64_t id = 1; id <= count; ++id) {
        add_collection_row(groups, id);
    }
    auto index = std::make_shared<azlw::agent::CollectionReadIndex>();
    azlw::agent::ShipCatalogBatchProgress first;
    first.table_key = "collection_ship_group";
    first.page_size = 128;
    first.module_sha256 = std::string(64, '9');
    first.collection_index = index;
    const azlw::agent::LuaApi api = make_api();
    auto* state = reinterpret_cast<lua_State*>(&fixture.state);
    fixture.state.next_calls = 0;
    std::size_t max_next = 0;
    int frames = 0;
    auto step = azlw::agent::ShipCatalogBatchStep::Continue;
    while (step == azlw::agent::ShipCatalogBatchStep::Continue) {
        const std::size_t before = fixture.state.next_calls;
        step = azlw::agent::advance_ship_catalog_batch(api, state, &first);
        max_next = std::max(max_next, fixture.state.next_calls - before);
        frames += 1;
        require(frames < 20000, "4000 条分页帧数失控");
    }
    require(first.outcome.success && first.outcome.page.records.size() == 128, "第一页应物化 128 条");
    require(fixture.state.next_calls == count + 1, "整表只应遍历一轮并多一次结束探测");
    require(max_next <= azlw::agent::kMaximumShipCatalogFrameSize, "单帧 lua_next 超过 32");
    require(index->ready, "第一页应留下索引");
    const std::size_t first_next = fixture.state.next_calls;
    fixture.state.next_calls = 0;
    azlw::agent::ShipCatalogBatchProgress second;
    second.table_key = "collection_ship_group";
    second.start_index = 128;
    second.cursor = 128;
    second.page_size = 128;
    second.module_sha256 = first.module_sha256;
    second.collection_index = index;
    step = azlw::agent::ShipCatalogBatchStep::Continue;
    int second_frames = 0;
    while (step == azlw::agent::ShipCatalogBatchStep::Continue) {
        step = azlw::agent::advance_ship_catalog_batch(api, state, &second);
        second_frames += 1;
        require(second_frames < 20000, "续页核对没有结束");
    }
    const std::size_t second_next = fixture.state.next_calls;
    require(second_next == count + 1, "续页应再核对一整轮键，而不是每页从头建索引");
    require(second.outcome.success && second.outcome.page.records.size() == 128, "续页应物化下一段");
    require(first.collection_identifiers.empty(), "排序后的键应交给共享索引，不在进度里再留一份");
    std::cout << "SHAPE=4000 first_next=" << first_next << " max_next_per_frame=" << max_next
              << " first_frames=" << frames << " second_next=" << second_next
              << " second_frames=" << second_frames << "\n";
}

bool finish_collection_page(CatalogFixture& fixture,
                            azlw::agent::ShipCatalogBatchProgress& progress,
                            std::uint32_t* max_heap_steps) {
    const azlw::agent::LuaApi api = make_api();
    auto* state = reinterpret_cast<lua_State*>(&fixture.state);
    auto step = azlw::agent::ShipCatalogBatchStep::Continue;
    int frames = 0;
    while (step == azlw::agent::ShipCatalogBatchStep::Continue) {
        step = azlw::agent::advance_ship_catalog_batch(api, state, &progress);
        if (max_heap_steps != nullptr) {
            *max_heap_steps = std::max(*max_heap_steps, progress.collection_heap_ops);
        }
        frames += 1;
        if (frames >= 20000) {
            return false;
        }
    }
    return true;
}

void test_collection_orders_keep_heap_steps_inside_the_frame() {
    constexpr std::uint64_t count = 4000;
    std::vector<std::uint64_t> ascending(count);
    std::vector<std::uint64_t> descending(count);
    std::vector<std::uint64_t> shuffled(count);
    for (std::uint64_t index = 0; index < count; ++index) {
        ascending[static_cast<std::size_t>(index)] = index + 1;
        descending[static_cast<std::size_t>(index)] = count - index;
        shuffled[static_cast<std::size_t>(index)] = index + 1;
    }
    std::uint64_t mixer = 0x9E3779B97F4A7C15ULL;
    for (std::uint64_t index = count; index > 1; --index) {
        mixer = mixer * 6364136223846793005ULL + 1;
        std::swap(shuffled[static_cast<std::size_t>(index - 1)],
                  shuffled[static_cast<std::size_t>(mixer % index)]);
    }
    const std::array<std::pair<const char*, const std::vector<std::uint64_t>*>, 3> orders{{
        {"asc", &ascending},
        {"desc", &descending},
        {"shuffle", &shuffled},
    }};
    for (const auto& [label, ids] : orders) {
        CatalogFixture fixture;
        auto groups = std::make_shared<FakeTable>();
        auto proxy = std::make_shared<FakeTable>();
        add_field(proxy, "shipGroups", FakeValue::table_value(groups));
        fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
        fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
        fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
        for (const std::uint64_t id : *ids) {
            add_collection_row(groups, id);
        }
        azlw::agent::ShipCatalogBatchProgress progress;
        progress.table_key = "collection_ship_group";
        progress.page_size = 128;
        progress.module_sha256 = std::string(64, '7');
        progress.collection_index = std::make_shared<azlw::agent::CollectionReadIndex>();
        std::uint32_t max_heap_steps = 0;
        require(finish_collection_page(fixture, progress, &max_heap_steps), "排序分页没有结束");
        require(max_heap_steps <= azlw::agent::kMaximumShipCatalogFrameSize,
                std::string(label) + " 单帧堆操作超过 32，实际 " + std::to_string(max_heap_steps));
        require(progress.outcome.success && progress.outcome.page.records.size() == 128,
                std::string(label) + " 第一页应成功");
        require(progress.outcome.page.records.front().id == 1, std::string(label) + " 结果应按标识升序");
        require(progress.collection_identifiers.empty(), std::string(label) + " 不应保留索引副本");
        require(progress.collection_index->identifiers.size() == count, std::string(label) + " 索引应包含整表");
        std::cout << "ORDER=" << label << " heap_steps_per_frame=" << max_heap_steps
                  << " next_calls=" << fixture.state.next_calls << "\n";
    }
}

void test_collection_continuation_rejects_table_changes() {
    const auto run = [](const char* label, const auto& mutate) {
        CatalogFixture fixture;
        auto groups = std::make_shared<FakeTable>();
        auto proxy = std::make_shared<FakeTable>();
        add_field(proxy, "shipGroups", FakeValue::table_value(groups));
        fixture.state.globals["collection_fixture"] = FakeValue::table_value(proxy);
        fixture.state.globals["CollectionProxy"] = FakeValue::table_value(proxy);
        fixture.state.globals["getProxy"] = FakeValue::function_value(fake_collection_proxy);
        for (std::uint64_t id = 1; id <= 64; ++id) {
            add_collection_row(groups, id);
        }
        auto index = std::make_shared<azlw::agent::CollectionReadIndex>();
        azlw::agent::ShipCatalogBatchProgress first;
        first.table_key = "collection_ship_group";
        first.page_size = 32;
        first.module_sha256 = std::string(64, '6');
        first.collection_index = index;
        require(finish_collection_page(fixture, first, nullptr), "一致性第一页没有结束");
        require(first.outcome.success && index->fields_captured && !index->verified, "第一页应留下未核对字段");
        mutate(groups);
        azlw::agent::ShipCatalogBatchProgress second;
        second.table_key = "collection_ship_group";
        second.start_index = 32;
        second.cursor = 32;
        second.page_size = 32;
        second.module_sha256 = first.module_sha256;
        second.collection_index = index;
        require(finish_collection_page(fixture, second, nullptr), "一致性续页没有结束");
        require(!second.outcome.success && !second.outcome.page.complete, std::string(label) + " 不能返回完整结果");
        require(second.outcome.error.code == "collection_content_changed",
                std::string(label) + " " + second.outcome.error.code);
        require(!index->ready, std::string(label) + " 变化后索引应失效");
        std::cout << label << " success=" << second.outcome.success
                  << " complete=" << second.outcome.page.complete
                  << " code=" << second.outcome.error.code << "\n";
    };
    run("NON_WITNESS", [](const std::shared_ptr<FakeTable>& groups) {
        for (std::uint64_t id = 2; id <= 64; ++id) {
            groups->numeric.at(id).table->fields["maxLV"] = FakeValue::number_value(121);
        }
    });
    run("STAR", [](const std::shared_ptr<FakeTable>& groups) {
        for (std::uint64_t id = 1; id <= 64; ++id) {
            groups->numeric.at(id).table->fields["star"] = FakeValue::number_value(2);
        }
    });
    run("ADDED_ROW", [](const std::shared_ptr<FakeTable>& groups) { add_collection_row(groups, 65); });
}

}  // namespace

int main() {
    {
        const LuaApi api = make_api();
        FakeState fixture_state;
        auto* state = reinterpret_cast<lua_State*>(&fixture_state);
        for (const char* fault : {"fake_push_c_closure", "fake_create_table", "fake_push_string"}) {
            fixture_state.fault_operation = fault;
            bool failed = false;
            try {
                if (std::string(fault) == "fake_push_string") {
                    api.push_string(state, "value");
                } else {
                    api.create_table(state, 0, 1);
                }
            } catch (const std::runtime_error& error) {
                failed = std::string(error.what()).find("fixture GC failure") != std::string::npos;
            }
            require(failed, "protected allocation lost original failure");
            require(fixture_state.stack.empty(), "allocation failure leaked stack values");
            require(fixture_state.registry.empty(), "allocation failure retained agent closure");
        }
        api.create_table(state, 0, 1);
        api.push_number(state, 7);
        fixture_state.fault_operation = "fake_set_field";
        try {
            api.set_field(state, 1, "value");
            require(false, "setfield failure was ignored");
        } catch (const std::runtime_error& error) {
            require(std::string(error.what()).find("fixture GC failure") != std::string::npos,
                    "setfield lost original error");
        }
        require(api.get_top(state) == 2, "setfield failure changed input stack");
        require(fixture_state.registry.empty(), "setfield retained closure");
        api.set_top(state, 0);
        fixture_state.heap_kib = 120;
        std::string collection_error;
        require(api.collect_allocations_since(state, 100, &collection_error), "增量回收失败");
        require(fixture_state.collected_kib == 20, "回收量必须跟随本帧净分配");
        require(api.collect_allocations_since(state, 150, &collection_error), "堆缩小不应失败");
        require(fixture_state.collected_kib == 20, "堆缩小时不应追加回收");
        require(fixture_state.gc_running, "自动回收启用状态不应改变");
        fixture_state.gc_running = false;
        require(api.collect_allocations_since(state, 100, &collection_error), "暂停状态下增量回收失败");
        require(!fixture_state.gc_running, "增量回收必须保留自动回收暂停状态");
        fixture_state.fault_operation = "fake_gc_step";
        require(!api.collect_allocations_since(state, 100, &collection_error), "回收异常不能被忽略");
        require(collection_error.find("fixture GC failure") != std::string::npos, "回收错误应保留原因");
        require(!fixture_state.gc_running, "回收异常必须保留自动回收暂停状态");
        require(fixture_state.stack.empty() && fixture_state.registry.empty(), "回收必须释放栈和闭包");
        fixture_state.stack_available = false;
        bool stack_exhausted = false;
        try {
            api.ensure_stack(state, 32);
        } catch (const std::runtime_error& error) {
            stack_exhausted = std::string(error.what()) == "Lua 栈容量不足";
        }
        require(stack_exhausted, "stack exhaustion was ignored");
        require(fixture_state.stack.empty(), "stack exhaustion changed stack");
        require(fixture_state.registry.empty(), "stack exhaustion retained closure");
    }

    test_catalog_batch_crosses_frames_without_exceeding_the_frame_budget();
    test_collection_frames_do_not_rescan_the_table();
    test_collection_index_is_reused_and_content_change_fails();
    test_four_thousand_collection_keys_are_scanned_once();
    test_collection_orders_keep_heap_steps_inside_the_frame();
    test_collection_continuation_rejects_table_changes();
    test_collection_history_paging_and_missing_values();
    test_empty_proxy_materializes_physical_base_chain();
    test_base_cycle_is_reported_as_incomplete();
    test_missing_physical_row_is_reported_as_incomplete();
    test_missing_proxy_field_is_reported_as_incomplete();
    test_invalid_source_metadata_reports_bounded_scalar_details();
    test_nil_source_metadata_uses_same_name_base_source();
    test_nil_source_metadata_without_same_name_base_fails();
    test_partial_raw_proxy_uses_complete_same_name_base_fields();
    std::cout << "PASS ship_catalog_snapshot_test\n";
    return 0;
}

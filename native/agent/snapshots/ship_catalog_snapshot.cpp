// 实现 confNEO 配置代理的全字段物化与舰船静态目录分页读取。

#include "ship_catalog_snapshot.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <exception>
#include <limits>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "lua/lua_reader.h"
#include "protocol/json_codec.h"

namespace azlw::agent {
namespace {

constexpr std::size_t kMaximumConfigBaseDepth = 32;
constexpr std::size_t kMaximumConfigFieldCount = 4'096;
constexpr std::size_t kMaximumDiagnosticScalarBytes = 64;
constexpr auto kLuaTypeLabels = std::to_array<std::string_view>({
    "nil",
    "boolean",
    "lightuserdata",
    "number",
    "string",
    "table",
    "function",
    "userdata",
    "thread",
});

struct LuaValueDiagnostic final {
    int type = kLuaTypeNil;
    std::string description;
};

bool push_raw_field(const LuaApi& api, lua_State* state, int table_index, const char* field_name);

std::string lua_type_label(int type) {
    if (type >= 0 && static_cast<std::size_t>(type) < kLuaTypeLabels.size()) {
        return std::string(kLuaTypeLabels[static_cast<std::size_t>(type)]);
    }
    return "type_" + std::to_string(type);
}

LuaValueDiagnostic describe_lua_value(const LuaApi& api, lua_State* state, int value_index) {
    LuaValueDiagnostic diagnostic;
    diagnostic.type = api.type(state, value_index);
    diagnostic.description = "type=" + lua_type_label(diagnostic.type);
    if (diagnostic.type == kLuaTypeBoolean) {
        diagnostic.description +=
            api.to_boolean(state, value_index) == 0 ? ",value=false" : ",value=true";
    } else if (diagnostic.type == kLuaTypeNumber) {
        const double value = api.to_number(state, value_index);
        diagnostic.description +=
            std::isfinite(value) ? ",value=" + std::to_string(value) : ",value=non_finite";
    } else if (diagnostic.type == kLuaTypeString) {
        std::string text_error;
        const std::optional<std::string> text = read_lua_text(
            api, state, value_index, kMaximumDiagnosticScalarBytes, true, &text_error);
        if (text.has_value()) {
            JsonWriter encoded;
            encoded.string(*text);
            diagnostic.description += ",value=" + encoded.take();
        } else {
            diagnostic.description += ",value_unavailable=" + text_error;
        }
    }
    return diagnostic;
}

LuaValueDiagnostic
describe_raw_field(const LuaApi& api, lua_State* state, int table_index, const char* field_name) {
    const int top = api.get_top(state);
    push_raw_field(api, state, table_index, field_name);
    LuaValueDiagnostic diagnostic = describe_lua_value(api, state, -1);
    api.set_top(state, top);
    return diagnostic;
}

std::string describe_lua_shape(const LuaApi& api, lua_State* state, int value_index) {
    const LuaValueDiagnostic diagnostic = describe_lua_value(api, state, value_index);
    std::string result = diagnostic.description;
    if (diagnostic.type != kLuaTypeTable) {
        return result;
    }

    const int top = api.get_top(state);
    const int stable_table = api.stable_stack_index(state, value_index);
    std::size_t fields = 0;
    bool bounded = false;
    // 诊断只统计 raw 键数量；达到配置字段上限即停止，不读取键和值的内容。
    api.push_nil(state);
    while (api.next(state, stable_table) != 0) {
        ++fields;
        api.set_top(state, api.get_top(state) - 1);
        if (fields >= kMaximumConfigFieldCount) {
            bounded = true;
            break;
        }
    }
    api.set_top(state, top);
    result += bounded ? ",fields_at_least=" : ",fields=";
    result += std::to_string(fields);
    return result;
}

void append_pg_base_diagnostic(const LuaApi& api,
                               lua_State* state,
                               std::string_view table_key,
                               std::string* diagnostic) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_global_table(api, state, "pg", &error)) {
        *diagnostic += "; pg.base." + std::string(table_key) + "{unavailable=" + error + "}";
        api.set_top(state, top);
        return;
    }
    const int pg_index = api.get_top(state);
    push_raw_field(api, state, pg_index, "base");
    if (api.type(state, -1) != kLuaTypeTable) {
        *diagnostic += "; pg.base{" + describe_lua_value(api, state, -1).description + "}";
        api.set_top(state, top);
        return;
    }
    const int base_index = api.get_top(state);
    push_raw_field(api, state, base_index, std::string(table_key).c_str());
    *diagnostic += "; pg.base." + std::string(table_key) + "{" +
                   describe_lua_value(api, state, -1).description + "}";
    api.set_top(state, top);
}

bool has_pg_base_source_table(const LuaApi& api,
                              lua_State* state,
                              std::string_view table_key) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_global_table(api, state, "pg", &error)) {
        api.set_top(state, top);
        return false;
    }
    const int pg_index = api.get_top(state);
    push_raw_field(api, state, pg_index, "base");
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        return false;
    }
    const int base_index = api.get_top(state);
    push_raw_field(api, state, base_index, std::string(table_key).c_str());
    const bool present = api.type(state, -1) == kLuaTypeTable;
    api.set_top(state, top);
    return present;
}

void append_first_row_diagnostic(const LuaApi& api,
                                 lua_State* state,
                                 int config_table_index,
                                 std::string_view table_key,
                                 std::string* diagnostic) {
    // 仅在来源元数据均为 nil 的失败路径触发首行代理，再观察两个固定 raw 位置。
    const int top = api.get_top(state);
    const int stable_config = api.stable_stack_index(state, config_table_index);
    if (api.get_field_protected(state, stable_config, "all") != 0) {
        *diagnostic +=
            "; first_all_id{unavailable=" + lua_failure_detail(api, state, "读取 all 失败") + "}";
        api.set_top(state, top);
        return;
    }
    if (api.type(state, -1) != kLuaTypeTable || api.object_length(state, -1) == 0) {
        *diagnostic += "; first_all_id{unavailable=all 不是非空 table}";
        api.set_top(state, top);
        return;
    }
    const int all_index = api.get_top(state);
    if (api.get_number_index_protected(state, all_index, 1.0) != 0) {
        *diagnostic +=
            "; first_all_id{unavailable=" + lua_failure_detail(api, state, "读取 all[1] 失败") +
            "}";
        api.set_top(state, top);
        return;
    }
    const LuaNumber first_id = read_lua_number(api, state, -1);
    api.set_top(state, api.get_top(state) - 1);
    if (first_id.status != LuaNumberStatus::Present) {
        *diagnostic += "; first_all_id{unavailable=all[1] 不是非负精确整数}";
        api.set_top(state, top);
        return;
    }
    *diagnostic += "; first_all_id=" + std::to_string(first_id.value);

    if (api.get_number_index_protected(state, stable_config, static_cast<double>(first_id.value)) !=
        0) {
        *diagnostic +=
            "; protected_pg_row{unavailable=" + lua_failure_detail(api, state, "触发配置代理失败") +
            "}";
    }
    api.set_top(state, all_index);

    api.push_number(state, static_cast<double>(first_id.value));
    api.raw_get(state, stable_config);
    *diagnostic += "; raw_pg_row{" + describe_lua_shape(api, state, -1) + "}";
    api.set_top(state, all_index);

    std::string error;
    if (!push_lua_global_table(api, state, "pg", &error)) {
        *diagnostic += "; raw_pg_base_row{unavailable=" + error + "}";
        api.set_top(state, top);
        return;
    }
    const int pg_index = api.get_top(state);
    push_raw_field(api, state, pg_index, "base");
    if (api.type(state, -1) != kLuaTypeTable) {
        *diagnostic += "; raw_pg_base_row{unavailable=pg.base 不是 table}";
        api.set_top(state, top);
        return;
    }
    const int base_index = api.get_top(state);
    push_raw_field(api, state, base_index, std::string(table_key).c_str());
    if (api.type(state, -1) != kLuaTypeTable) {
        *diagnostic +=
            "; raw_pg_base_row{unavailable=pg.base." + std::string(table_key) + " 不是 table}";
        api.set_top(state, top);
        return;
    }
    const int base_table_index = api.get_top(state);
    api.push_number(state, static_cast<double>(first_id.value));
    api.raw_get(state, base_table_index);
    *diagnostic += "; raw_pg_base_row{" + describe_lua_shape(api, state, -1) + "}";
    api.set_top(state, top);
}

bool push_raw_field(const LuaApi& api, lua_State* state, int table_index, const char* field_name) {
    const int stable_table = api.stable_stack_index(state, table_index);
    api.push_string(state, field_name);
    api.raw_get(state, stable_table);
    return true;
}

bool read_source_names(const LuaApi& api,
                       lua_State* state,
                       int config_table_index,
                       std::string_view table_key,
                       std::vector<std::string>* sources,
                       std::string* error) {
    const int top = api.get_top(state);
    const int stable_table = api.stable_stack_index(state, config_table_index);
    const auto finish_error = [&](std::string message) {
        const LuaValueDiagnostic sub_diagnostic =
            describe_raw_field(api, state, stable_table, "__sub__");
        const LuaValueDiagnostic name_diagnostic =
            describe_raw_field(api, state, stable_table, "__name");
        message += "; table_key=" + std::string(table_key) + "; __sub__{" +
                   sub_diagnostic.description + "}; __name{" + name_diagnostic.description + "}";
        if (sub_diagnostic.type == kLuaTypeNil && name_diagnostic.type == kLuaTypeNil) {
            append_pg_base_diagnostic(api, state, table_key, &message);
            append_first_row_diagnostic(api, state, stable_table, table_key, &message);
        }
        *error = std::move(message);
        api.set_top(state, top);
        return false;
    };
    push_raw_field(api, state, stable_table, "__sub__");
    if (api.type(state, -1) == kLuaTypeTable) {
        const int sub_table = api.get_top(state);
        const std::size_t length = api.object_length(state, sub_table);
        if (length == 0 || length > 256) {
            return finish_error("配置表 __sub__ 数量必须位于 1 至 256");
        }
        sources->reserve(length);
        for (std::size_t index = 0; index < length; ++index) {
            if (api.get_number_index_protected(state, sub_table, static_cast<double>(index + 1U)) !=
                0) {
                return finish_error(lua_failure_detail(api, state, "读取配置表 __sub__ 失败"));
            }
            std::string text_error;
            std::optional<std::string> source =
                read_lua_text(api, state, -1, 128, false, &text_error);
            api.set_top(state, api.get_top(state) - 1);
            if (!source.has_value()) {
                return finish_error("配置表 __sub__ 包含无效来源名: " + text_error);
            }
            sources->push_back(std::move(*source));
        }
        api.set_top(state, top);
        return true;
    }
    api.set_top(state, top);

    push_raw_field(api, state, stable_table, "__name");
    std::string text_error;
    std::optional<std::string> source = read_lua_text(api, state, -1, 128, false, &text_error);
    api.set_top(state, top);
    if (!source.has_value()) {
        const LuaValueDiagnostic sub_diagnostic =
            describe_raw_field(api, state, stable_table, "__sub__");
        const LuaValueDiagnostic name_diagnostic =
            describe_raw_field(api, state, stable_table, "__name");
        if (sub_diagnostic.type == kLuaTypeNil && name_diagnostic.type == kLuaTypeNil &&
            has_pg_base_source_table(api, state, table_key)) {
            sources->emplace_back(table_key);
            return true;
        }
        return finish_error("配置表缺少有效 __name/__sub__: " + text_error);
    }
    sources->push_back(std::move(*source));
    return true;
}

bool push_physical_row(const LuaApi& api,
                       lua_State* state,
                       const std::vector<std::string>& sources,
                       std::uint64_t id,
                       std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "pg", error)) {
        api.set_top(state, top);
        return false;
    }
    const int pg_index = api.get_top(state);
    if (!push_lua_table_field(api, state, pg_index, "base", error)) {
        *error = "读取 pg.base 失败: " + *error;
        api.set_top(state, top);
        return false;
    }
    const int base_index = api.get_top(state);
    std::optional<std::string> matched_source;
    for (const std::string& source : sources) {
        const int source_top = api.get_top(state);
        if (!push_lua_table_field(api, state, base_index, source.c_str(), error)) {
            *error = "读取 pg.base." + source + " 失败: " + *error;
            api.set_top(state, top);
            return false;
        }
        const int source_index = api.get_top(state);
        if (api.get_number_index_protected(state, source_index, static_cast<double>(id)) != 0) {
            *error = lua_failure_detail(
                api, state, "读取 pg.base." + source + "[" + std::to_string(id) + "] 失败");
            api.set_top(state, top);
            return false;
        }
        if (api.type(state, -1) == kLuaTypeTable) {
            if (matched_source.has_value()) {
                *error = "配置 ID " + std::to_string(id) + " 同时存在于多个物理来源";
                api.set_top(state, top);
                return false;
            }
            matched_source = source;
        }
        api.set_top(state, source_top);
    }
    if (!matched_source.has_value()) {
        *error = "配置 ID " + std::to_string(id) + " 未出现在 pg.base 物理来源中";
        api.set_top(state, top);
        return false;
    }
    if (!push_lua_table_field(api, state, base_index, matched_source->c_str(), error)) {
        *error = "重新读取 pg.base." + *matched_source + " 失败: " + *error;
        api.set_top(state, top);
        return false;
    }
    const int source_index = api.get_top(state);
    if (api.get_number_index_protected(state, source_index, static_cast<double>(id)) != 0) {
        *error = lua_failure_detail(api, state, "重新读取配置物理行失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "配置物理行在重复读取时不再是 table";
        api.set_top(state, top);
        return false;
    }
    return true;
}

bool collect_physical_fields(const LuaApi& api,
                             lua_State* state,
                             int row_index,
                             std::set<std::string>* fields,
                             std::string* error) {
    const int top = api.get_top(state);
    const int stable_row = api.stable_stack_index(state, row_index);
    api.push_nil(state);
    while (api.next(state, stable_row) != 0) {
        std::string text_error;
        std::optional<std::string> field = read_lua_text(api, state, -2, 128, false, &text_error);
        if (!field.has_value()) {
            *error = "配置物理行包含非字符串或无效字段名: " + text_error;
            api.set_top(state, top);
            return false;
        }
        fields->insert(std::move(*field));
        if (fields->size() > kMaximumConfigFieldCount) {
            *error = "配置物理字段总数超过 4096";
            api.set_top(state, top);
            return false;
        }
        api.set_top(state, api.get_top(state) - 1);
    }
    api.set_top(state, top);
    return true;
}

bool collect_materialized_fields(const LuaApi& api,
                                 lua_State* state,
                                 int config_table_index,
                                 const std::vector<std::string>& sources,
                                 std::uint64_t id,
                                 std::set<std::uint64_t>* ancestors,
                                 std::size_t remaining_depth,
                                 std::set<std::string>* fields,
                                 std::string* error) {
    if (remaining_depth == 0) {
        *error = "配置 base 链超过 32 层";
        return false;
    }
    if (!ancestors->insert(id).second) {
        *error = "配置 base 链存在循环";
        return false;
    }
    const int top = api.get_top(state);
    const int stable_table = api.stable_stack_index(state, config_table_index);
    if (api.get_number_index_protected(state, stable_table, static_cast<double>(id)) != 0) {
        *error = lua_failure_detail(api, state, "触发配置代理 " + std::to_string(id) + " 失败");
        api.set_top(state, top);
        ancestors->erase(id);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "配置代理 " + std::to_string(id) + " 不是 table";
        api.set_top(state, top);
        ancestors->erase(id);
        return false;
    }
    api.set_top(state, top);

    if (!push_physical_row(api, state, sources, id, error)) {
        api.set_top(state, top);
        ancestors->erase(id);
        return false;
    }
    const int physical_row = api.get_top(state);
    if (!collect_physical_fields(api, state, physical_row, fields, error)) {
        api.set_top(state, top);
        ancestors->erase(id);
        return false;
    }
    const LuaNumber base = read_raw_lua_number_field(api, state, physical_row, "base");
    api.set_top(state, top);
    if (base.status == LuaNumberStatus::Invalid) {
        *error = "配置 " + std::to_string(id) + " 的 raw base 不是非负精确整数";
        ancestors->erase(id);
        return false;
    }
    bool complete = true;
    if (base.status == LuaNumberStatus::Present && base.value == id) {
        *error = "配置 base 链存在自循环";
        ancestors->erase(id);
        return false;
    }
    if (base.status == LuaNumberStatus::Present && base.value != 0) {
        complete = collect_materialized_fields(api,
                                               state,
                                               stable_table,
                                               sources,
                                               base.value,
                                               ancestors,
                                               remaining_depth - 1,
                                               fields,
                                               error);
    }
    ancestors->erase(id);
    return complete;
}

bool materialize_record(const LuaApi& api,
                        lua_State* state,
                        int config_table_index,
                        const std::vector<std::string>& sources,
                        std::uint64_t id,
                        LuaValue* raw,
                        std::string* error) {
    const int top = api.get_top(state);
    const int stable_table = api.stable_stack_index(state, config_table_index);
    std::set<std::string> fields;
    std::set<std::uint64_t> ancestors;
    if (!collect_materialized_fields(api,
                                     state,
                                     stable_table,
                                     sources,
                                     id,
                                     &ancestors,
                                     kMaximumConfigBaseDepth,
                                     &fields,
                                     error)) {
        api.set_top(state, top);
        return false;
    }
    if (fields.empty()) {
        *error = "配置物理字段集合为空";
        return false;
    }
    if (api.get_number_index_protected(state, stable_table, static_cast<double>(id)) != 0) {
        *error = lua_failure_detail(api, state, "重新读取配置代理失败");
        api.set_top(state, top);
        return false;
    }
    const int proxy_index = api.get_top(state);
    std::vector<const char*> field_names;
    field_names.reserve(fields.size());
    for (const std::string& field : fields) {
        field_names.push_back(field.c_str());
    }
    if (!push_lua_materialized_table(api, state, proxy_index, field_names, error)) {
        api.set_top(state, top);
        return false;
    }
    LuaValueResult value = read_lua_value(api, state, -1);
    api.set_top(state, top);
    if (!value.complete) {
        *error = "物化配置值不完整";
        if (!value.read_errors.empty()) {
            *error += ": " + value.read_errors.front();
        }
        return false;
    }
    if (value.value.kind != LuaValueKind::Object || value.value.keys.size() != fields.size()) {
        *error = "物化配置字段数量与物理字段集合不一致";
        return false;
    }
    *raw = std::move(value.value);
    return true;
}

bool read_catalog_id(const LuaApi& api,
                     lua_State* state,
                     int all_index,
                     std::uint32_t zero_based_index,
                     std::uint64_t* id,
                     std::string* error) {
    const int top = api.get_top(state);
    if (api.get_number_index_protected(
            state, all_index, static_cast<double>(zero_based_index + 1U)) != 0) {
        *error = lua_failure_detail(api, state, "读取舰船静态目录索引失败");
        api.set_top(state, top);
        return false;
    }
    const LuaNumber value = read_lua_number(api, state, -1);
    api.set_top(state, top);
    if (value.status != LuaNumberStatus::Present || value.value == 0) {
        *error = "舰船静态目录索引不是正整数";
        return false;
    }
    *id = value.value;
    return true;
}

// 图鉴键不是数组下标。请求自己记住数值键，下一帧从该键继续，不能每帧重走整张表。
bool push_collection_groups(const LuaApi& api, lua_State* state, ShipCatalogPageExecution* result) {
    if (!push_lua_proxy(api, state, "CollectionProxy", "collection_proxy_invalid", &result->error)) {
        return false;
    }
    if (api.get_field_protected(state, api.get_top(state), "shipGroups") != 0) {
        result->error = make_lua_error(
            "collection_groups_failed", lua_failure_detail(api, state, "图鉴记录读取失败"));
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        result->error =
            make_lua_error("collection_groups_missing", "图鉴记录不是table", "same_request");
        return false;
    }
    return true;
}

bool scan_collection_keys(const LuaApi& api,
                          lua_State* state,
                          ShipCatalogBatchProgress* progress,
                          std::uint32_t* budget) {
    if (!push_collection_groups(api, state, &progress->outcome)) {
        return false;
    }
    const int groups = api.get_top(state);
    const void* identity = api.to_pointer(state, groups);
    if (!progress->collection_scan_started) {
        progress->collection_groups = identity;
        progress->collection_scan_started = true;
    } else if (identity != progress->collection_groups) {
        progress->outcome.error = make_lua_error(
            "collection_groups_replaced", "图鉴表在分页期间被替换，游标已失效");
        return false;
    }
    if (progress->collection_has_cursor_key) {
        api.push_number(state, static_cast<double>(progress->collection_cursor_key));
    } else {
        api.push_nil(state);
    }
    while (*budget > 0) {
        if (api.next(state, groups) == 0) {
            progress->collection_scan_complete = true;
            break;
        }
        *budget -= 1;
        const LuaNumber id = read_lua_number(api, state, -2);
        if (id.status != LuaNumberStatus::Present || id.value == 0 ||
            progress->collection_identifiers.size() >= kMaximumShipCatalogItems) {
            progress->outcome.error =
                make_lua_error("collection_group_id_invalid", "图鉴组标识或条目数无效");
            return false;
        }
        progress->collection_identifiers.push_back(id.value);
        progress->collection_cursor_key = id.value;
        progress->collection_has_cursor_key = true;
        api.set_top(state, api.get_top(state) - 1);
    }
    return true;
}

bool materialize_collection_records(const LuaApi& api,
                                    lua_State* state,
                                    ShipCatalogBatchProgress* progress,
                                    std::uint32_t budget) {
    if (!push_collection_groups(api, state, &progress->outcome)) {
        return false;
    }
    const int groups = api.get_top(state);
    if (api.to_pointer(state, groups) != progress->collection_groups) {
        progress->outcome.error = make_lua_error(
            "collection_groups_replaced", "图鉴表在分页期间被替换，游标已失效");
        return false;
    }
    const std::uint32_t total = progress->outcome.page.total_count;
    const std::uint32_t end = std::min(total, progress->start_index + progress->page_size);
    constexpr auto fields = std::to_array<const char*>({"id", "maxLV", "star"});
    while (budget > 0 && progress->cursor < end) {
        budget -= 1;
        const std::uint64_t identifier =
            (progress->collection_using_shared && progress->collection_index)
                ? progress->collection_index->identifiers[progress->cursor]
                : progress->collection_identifiers[progress->cursor];
        const int top = api.get_top(state);
        if (api.get_number_index_protected(state, groups, static_cast<double>(identifier)) != 0) {
            progress->outcome.error = make_lua_error(
                "collection_group_failed", lua_failure_detail(api, state, "图鉴组读取失败"));
            return false;
        }
        const int item = api.get_top(state);
        LuaValue raw;
        raw.kind = LuaValueKind::Object;
        for (const char* field : fields) {
            if (api.get_field_protected(state, item, field) != 0) {
                progress->outcome.error = make_lua_error(
                    "collection_group_field_failed",
                    lua_failure_detail(api, state, "图鉴字段读取失败"));
                return false;
            }
            const LuaNumber value = read_lua_number(api, state, -1);
            api.set_top(state, api.get_top(state) - 1);
            if (value.status != LuaNumberStatus::Present ||
                (std::string_view(field) == "id" && value.value != identifier)) {
                progress->outcome.error = make_lua_error(
                    "collection_group_field_invalid", "图鉴字段不是有效非负整数，或组标识不一致");
                return false;
            }
            LuaValueKey key;
            key.kind = LuaValueKey::Kind::String;
            key.text = field;
            raw.keys.push_back(std::move(key));
            LuaValue number;
            number.kind = LuaValueKind::Number;
            number.number = static_cast<double>(value.value);
            raw.values.push_back(std::move(number));
        }
        progress->outcome.page.records.push_back(
            ShipCatalogRecord{.id = identifier, .raw = std::move(raw)});
        api.set_top(state, top);
        progress->cursor += 1;
    }
    if (progress->cursor < total) {
        progress->outcome.page.next_index = progress->cursor;
    } else {
        progress->outcome.page.next_index.reset();
    }
    progress->outcome.page.complete = progress->cursor == end;
    return true;
}

ShipCatalogBatchStep advance_collection_batch(const LuaApi& api,
                                              lua_State* state,
                                              ShipCatalogBatchProgress* progress) {
    LuaStackGuard stack_guard(api, state);
    auto fail = [&](AgentError error) {
        progress->outcome.success = false;
        progress->outcome.page.complete = false;
        progress->outcome.error = std::move(error);
        return ShipCatalogBatchStep::Finished;
    };
    if (!api.ready() || state == nullptr) {
        return fail(make_lua_error(
            "ship_catalog_lua_not_ready", "Lua 状态或必需函数表尚未就绪", "same_request"));
    }
    if (!progress->started) {
        progress->outcome.success = true;
        progress->outcome.page.table_key = progress->table_key;
        progress->outcome.page.module_sha256 = progress->module_sha256;
        progress->outcome.page.start_index = progress->start_index;
        progress->outcome.page.complete = false;
        progress->started = true;
    }
    std::uint32_t budget = kMaximumShipCatalogFrameSize;
    progress->collection_heap_ops = 0;
    if (progress->collection_index && progress->collection_index->ready &&
        !progress->collection_scan_started) {
        progress->collection_using_shared = true;
        progress->collection_adopted = true;
        progress->collection_groups = progress->collection_index->groups;
        progress->collection_scan_started = true;
        progress->collection_scan_complete = true;
        progress->collection_sorted = true;
        progress->outcome.page.total_count =
            static_cast<std::uint32_t>(progress->collection_index->identifiers.size());
        if (progress->start_index > progress->outcome.page.total_count) {
            return fail(make_lua_error(
                "ship_catalog_page_start_out_of_range", "图鉴分页起点超过条目数"));
        }
        progress->cursor = progress->start_index;
    }
    if (!progress->collection_scan_complete) {
        if (!scan_collection_keys(api, state, progress, &budget)) {
            return fail(std::move(progress->outcome.error));
        }
        if (!progress->collection_scan_complete) {
            return ShipCatalogBatchStep::Continue;
        }
        if (progress->collection_identifiers.size() > std::numeric_limits<std::uint32_t>::max()) {
            return fail(make_lua_error("collection_group_id_invalid", "图鉴条目数超出范围"));
        }
    }
    if (!progress->collection_sorted) {
        auto& identifiers = progress->collection_identifiers;
        if (!progress->collection_heap_ready) {
            while (budget > 0 && progress->collection_sorted_count < identifiers.size()) {
                std::push_heap(
                    identifiers.begin(),
                    identifiers.begin() +
                        static_cast<std::ptrdiff_t>(progress->collection_sorted_count + 1));
                progress->collection_sorted_count += 1;
                progress->collection_heap_ops += 1;
                budget -= 1;
            }
            if (progress->collection_sorted_count < identifiers.size()) {
                return ShipCatalogBatchStep::Continue;
            }
            progress->collection_heap_ready = true;
        }
        if (progress->collection_heap_ready) {
            while (budget > 0 && progress->collection_sorted_count > 0) {
                std::pop_heap(
                    identifiers.begin(),
                    identifiers.begin() + static_cast<std::ptrdiff_t>(progress->collection_sorted_count));
                progress->collection_sorted_count -= 1;
                progress->collection_heap_ops += 1;
                budget -= 1;
            }
            if (progress->collection_sorted_count > 0) {
                return ShipCatalogBatchStep::Continue;
            }
            progress->collection_sorted = true;
            progress->outcome.page.total_count = static_cast<std::uint32_t>(identifiers.size());
            if (progress->start_index > progress->outcome.page.total_count) {
                return fail(make_lua_error(
                    "ship_catalog_page_start_out_of_range", "图鉴分页起点超过条目数"));
            }
            progress->cursor = progress->start_index;
            if (progress->collection_index) {
                progress->collection_index->identifiers.swap(identifiers);
                progress->collection_using_shared = true;
                progress->collection_index->groups = progress->collection_groups;
                progress->collection_index->ready = true;
            }
            if (budget == 0) {
                return ShipCatalogBatchStep::Continue;
            }
        }
        if (!progress->collection_sorted) {
            return ShipCatalogBatchStep::Continue;
        }
    }
    if (progress->collection_index && progress->collection_index->ready &&
        !progress->collection_index->fields_captured && !progress->collection_adopted) {
        auto& index = *progress->collection_index;
        if (index.levels.empty()) {
            index.levels.assign(index.identifiers.size(), 0);
            index.stars.assign(index.identifiers.size(), 0);
        }
        if (!push_collection_groups(api, state, &progress->outcome)) {
            return fail(std::move(progress->outcome.error));
        }
        const int groups = api.get_top(state);
        while (budget > 0 && index.capture_rows < index.identifiers.size()) {
            const std::uint64_t identifier = index.identifiers[index.capture_rows];
            if (api.get_number_index_protected(state, groups, static_cast<double>(identifier)) !=
                0) {
                return fail(make_lua_error(
                    "collection_group_failed", lua_failure_detail(api, state, "图鉴组读取失败")));
            }
            const int item = api.get_top(state);
            if (api.get_field_protected(state, item, "maxLV") != 0 ||
                api.type(state, -1) != kLuaTypeNumber) {
                return fail(make_lua_error("collection_group_field_failed", "图鉴等级读取失败"));
            }
            index.levels[index.capture_rows] = static_cast<std::uint64_t>(api.to_number(state, -1));
            api.set_top(state, api.get_top(state) - 1);
            if (api.get_field_protected(state, item, "star") != 0 ||
                api.type(state, -1) != kLuaTypeNumber) {
                return fail(make_lua_error("collection_group_field_failed", "图鉴星级读取失败"));
            }
            index.stars[index.capture_rows] = static_cast<std::uint64_t>(api.to_number(state, -1));
            api.set_top(state, groups);
            index.capture_rows += 1;
            budget -= 1;
        }
        if (index.capture_rows < index.identifiers.size()) {
            return ShipCatalogBatchStep::Continue;
        }
        index.fields_captured = true;
    }
    if (progress->collection_adopted && progress->collection_index &&
        progress->collection_index->ready && !progress->collection_index->verified) {
        auto& index = *progress->collection_index;
        if (!push_collection_groups(api, state, &progress->outcome)) {
            return fail(std::move(progress->outcome.error));
        }
        const int groups = api.get_top(state);
        if (api.to_pointer(state, groups) != index.groups) {
            index.ready = false;
            return fail(make_lua_error(
                "collection_groups_replaced", "图鉴表在分页期间被替换，游标已失效"));
        }
        if (!index.verify_count_done) {
            if (index.verify_has_key) {
                api.push_number(state, static_cast<double>(index.verify_key));
            } else {
                api.push_nil(state);
            }
            while (budget > 0) {
                if (api.next(state, groups) == 0) {
                    index.verify_count_done = true;
                    break;
                }
                budget -= 1;
                const LuaNumber key = read_lua_number(api, state, -2);
                if (key.status != LuaNumberStatus::Present || key.value == 0) {
                    index.ready = false;
                    return fail(make_lua_error("collection_group_id_invalid", "图鉴组标识无效"));
                }
                index.verify_count += 1;
                index.verify_key = key.value;
                index.verify_has_key = true;
                api.set_top(state, api.get_top(state) - 1);
            }
            if (!index.verify_count_done) {
                return ShipCatalogBatchStep::Continue;
            }
            if (index.verify_count != index.identifiers.size()) {
                index.ready = false;
                return fail(make_lua_error(
                    "collection_content_changed", "图鉴条目数量在同一次读取中发生变化"));
            }
        }
        while (budget > 0 && index.verify_rows < index.identifiers.size()) {
            const std::uint64_t identifier = index.identifiers[index.verify_rows];
            if (api.get_number_index_protected(state, groups, static_cast<double>(identifier)) !=
                0) {
                index.ready = false;
                return fail(make_lua_error(
                    "collection_group_failed", lua_failure_detail(api, state, "图鉴组读取失败")));
            }
            const int item = api.get_top(state);
            if (api.get_field_protected(state, item, "maxLV") != 0) {
                index.ready = false;
                return fail(make_lua_error(
                    "collection_group_field_failed",
                    lua_failure_detail(api, state, "图鉴字段读取失败")));
            }
            const LuaNumber level = read_lua_number(api, state, -1);
            api.set_top(state, api.get_top(state) - 1);
            if (api.get_field_protected(state, item, "star") != 0) {
                index.ready = false;
                return fail(make_lua_error(
                    "collection_group_field_failed",
                    lua_failure_detail(api, state, "图鉴字段读取失败")));
            }
            const LuaNumber star = read_lua_number(api, state, -1);
            api.set_top(state, api.get_top(state) - 1);
            if (level.status != LuaNumberStatus::Present || star.status != LuaNumberStatus::Present ||
                index.verify_rows >= index.levels.size() ||
                level.value != index.levels[index.verify_rows] ||
                star.value != index.stars[index.verify_rows]) {
                index.ready = false;
                return fail(make_lua_error(
                    "collection_content_changed", "图鉴字段在同一次读取中发生变化"));
            }
            api.set_top(state, groups);
            index.verify_rows += 1;
            budget -= 1;
        }
        if (index.verify_rows < index.identifiers.size()) {
            return ShipCatalogBatchStep::Continue;
        }
        index.verified = true;
    }
    if (progress->collection_has_witness) {
        if (budget == 0) {
            return ShipCatalogBatchStep::Continue;
        }
        budget -= 1;
        if (!push_collection_groups(api, state, &progress->outcome)) {
            return fail(std::move(progress->outcome.error));
        }
        const int groups = api.get_top(state);
        if (api.to_pointer(state, groups) != progress->collection_groups) {
            if (progress->collection_index != nullptr) {
                progress->collection_index->ready = false;
            }
            return fail(make_lua_error(
                "collection_groups_replaced", "图鉴表在分页期间被替换，游标已失效"));
        }
        if (api.get_number_index_protected(
                state, groups, static_cast<double>(progress->collection_witness_id)) != 0) {
            return fail(make_lua_error(
                "collection_group_failed", lua_failure_detail(api, state, "图鉴组读取失败")));
        }
        const int item = api.get_top(state);
        if (api.get_field_protected(state, item, "maxLV") != 0) {
            return fail(make_lua_error(
                "collection_group_field_failed",
                lua_failure_detail(api, state, "图鉴字段读取失败")));
        }
        const LuaNumber level = read_lua_number(api, state, -1);
        api.set_top(state, api.get_top(state) - 1);
        if (level.status != LuaNumberStatus::Present ||
            level.value != progress->collection_witness_level) {
            if (progress->collection_index != nullptr) {
                progress->collection_index->ready = false;
            }
            return fail(make_lua_error(
                "collection_content_changed", "图鉴内容在同一次读取中发生变化"));
        }
    }
    if (!materialize_collection_records(api, state, progress, budget)) {
        return fail(std::move(progress->outcome.error));
    }
    if (!progress->collection_has_witness && !progress->outcome.page.records.empty()) {
        const ShipCatalogRecord& first = progress->outcome.page.records.front();
        progress->collection_witness_id = first.id;
        for (std::size_t index = 0; index < first.raw.keys.size(); ++index) {
            if (first.raw.keys[index].text == "maxLV") {
                progress->collection_witness_level =
                    static_cast<std::uint64_t>(first.raw.values[index].number);
            }
            if (first.raw.keys[index].text == "star") {
                progress->collection_witness_star =
                    static_cast<std::uint64_t>(first.raw.values[index].number);
            }
        }
        progress->collection_has_witness = true;
        if (progress->collection_index != nullptr) {
            progress->collection_index->has_witness = true;
            progress->collection_index->witness_id = progress->collection_witness_id;
            progress->collection_index->witness_level = progress->collection_witness_level;
            progress->collection_index->witness_star = progress->collection_witness_star;
        }
    }
    const std::uint32_t end =
        std::min(progress->outcome.page.total_count, progress->start_index + progress->page_size);
    if (progress->cursor < end) {
        return ShipCatalogBatchStep::Continue;
    }
    progress->outcome.success = true;
    progress->outcome.page.complete = true;
    return ShipCatalogBatchStep::Finished;
}

}  // namespace

ShipCatalogBatchStep advance_ship_catalog_batch(
    const LuaApi& api,
    lua_State* state,
    ShipCatalogBatchProgress* progress) noexcept {
    auto fail = [&](AgentError error) {
        progress->outcome.success = false;
        progress->outcome.error = std::move(error);
        return ShipCatalogBatchStep::Finished;
    };
    if (progress == nullptr || progress->page_size == 0 ||
        progress->page_size > kMaximumShipCatalogPageSize ||
        progress->cursor < progress->start_index ||
        progress->cursor - progress->start_index > progress->page_size) {
        return fail(make_lua_error(
            "ship_catalog_page_size_out_of_range",
            "舰船静态目录批次超出共享页容量"));
    }
    const std::uint32_t filled = progress->cursor - progress->start_index;
    if (filled == progress->page_size) {
        return ShipCatalogBatchStep::Finished;
    }
    if (progress->table_key == "collection_ship_group") {
        try {
            return advance_collection_batch(api, state, progress);
        } catch (const std::exception& exception) {
            progress->outcome.page.complete = false;
            return fail(make_lua_error("ship_catalog_exception", exception.what()));
        }
    }
    const std::uint32_t frame_size =
        std::min(kMaximumShipCatalogFrameSize, progress->page_size - filled);
    ShipCatalogPageExecution frame = snapshot_ship_catalog(
        api,
        state,
        progress->table_key,
        progress->cursor,
        frame_size,
        progress->module_sha256);
    if (!frame.success) {
        progress->outcome = std::move(frame);
        return ShipCatalogBatchStep::Finished;
    }
    if (frame.page.start_index != progress->cursor) {
        return fail(make_lua_error(
            "ship_catalog_cursor_invalid",
            "舰船静态目录帧起点与批次游标不一致"));
    }
    const std::uint32_t frame_consumed = static_cast<std::uint32_t>(
        frame.page.records.size() + frame.page.read_errors.size());
    if (!progress->started) {
        progress->outcome = std::move(frame);
        progress->outcome.page.start_index = progress->start_index;
        progress->started = true;
    } else if (frame.page.total_count != progress->outcome.page.total_count ||
               frame.page.table_key != progress->outcome.page.table_key) {
        return fail(make_lua_error(
            "ship_catalog_size_invalid",
            "舰船静态目录分页期间表身份或总数发生变化"));
    } else {
        ShipCatalogPage& page = progress->outcome.page;
        page.records.insert(
            page.records.end(),
            std::make_move_iterator(frame.page.records.begin()),
            std::make_move_iterator(frame.page.records.end()));
        page.read_errors.insert(
            page.read_errors.end(),
            std::make_move_iterator(frame.page.read_errors.begin()),
            std::make_move_iterator(frame.page.read_errors.end()));
        page.next_index = frame.page.next_index;
        page.complete = page.complete && frame.page.complete;
        page.module_sha256 = std::move(frame.page.module_sha256);
    }
    const std::uint32_t next_cursor = progress->cursor + frame_consumed;
    if (progress->outcome.page.next_index.has_value() &&
        *progress->outcome.page.next_index != next_cursor) {
        return fail(make_lua_error(
            "ship_catalog_cursor_invalid",
            "舰船静态目录帧没有按已消费条目前进"));
    }
    progress->cursor = next_cursor;
    const std::uint32_t consumed = progress->cursor - progress->start_index;
    if (progress->outcome.page.next_index.has_value() && consumed < progress->page_size) {
        if (*progress->outcome.page.next_index <= progress->start_index ||
            *progress->outcome.page.next_index != progress->cursor) {
            return fail(make_lua_error(
                "ship_catalog_cursor_invalid",
                "舰船静态目录批次游标没有前进"));
        }
        return ShipCatalogBatchStep::Continue;
    }
    if (consumed > progress->page_size) {
        return fail(make_lua_error(
            "ship_catalog_page_size_out_of_range",
            "舰船静态目录批次读取超过请求页容量"));
    }
    if (progress->cursor < progress->outcome.page.total_count) {
        progress->outcome.page.next_index = progress->cursor;
    } else {
        progress->outcome.page.next_index.reset();
    }
    return ShipCatalogBatchStep::Finished;
}

ShipCatalogPageExecution snapshot_ship_catalog(const LuaApi& api,
                                               lua_State* state,
                                               std::string_view table_key,
                                               std::uint32_t start_index,
                                               std::uint32_t page_size,
                                               std::string_view module_sha256) noexcept {
    ShipCatalogPageExecution execution;
    if (!api.ready() || state == nullptr) {
        execution.error = make_lua_error(
            "ship_catalog_lua_not_ready", "Lua 状态或必需函数表尚未就绪", "same_request");
        return execution;
    }
    LuaStackGuard stack_guard(api, state);
    try {
        if (!is_supported_ship_catalog_table(table_key)) {
            execution.error = make_lua_error("ship_catalog_table_unsupported",
                                             "table_key 不属于舰船静态目录白名单");
            return execution;
        }
        if (page_size == 0 || page_size > kMaximumShipCatalogFrameSize) {
            execution.error = make_lua_error("ship_catalog_page_size_out_of_range",
                                             "page_size 超出舰船静态目录单帧容量");
            return execution;
        }
        execution.page.table_key.assign(table_key);
        execution.page.module_sha256.assign(module_sha256);
        execution.page.start_index = start_index;
        if (table_key == "collection_ship_group") {
            ShipCatalogBatchProgress progress;
            progress.table_key.assign(table_key);
            progress.start_index = start_index;
            progress.page_size = page_size;
            progress.cursor = start_index;
            progress.module_sha256.assign(module_sha256);
            advance_ship_catalog_batch(api, state, &progress);
            return progress.outcome;
        }


        std::string error;
        const std::string table_name(table_key);
        if (!push_lua_pg_config_table(api, state, table_name.c_str(), &error)) {
            execution.error =
                make_lua_error("ship_catalog_missing", std::move(error), "same_request");
            return execution;
        }
        const int config_table = api.get_top(state);
        std::vector<std::string> sources;
        if (!read_source_names(api, state, config_table, table_key, &sources, &error)) {
            execution.error = make_lua_error("ship_catalog_source_invalid", std::move(error));
            return execution;
        }
        if (api.get_field_protected(state, config_table, "all") != 0) {
            execution.error =
                make_lua_error("ship_catalog_index_failed",
                               lua_failure_detail(api, state, "读取舰船静态目录 all 失败"));
            return execution;
        }
        if (api.type(state, -1) != kLuaTypeTable) {
            execution.error =
                make_lua_error("ship_catalog_index_invalid", "舰船静态目录 all 不是 table");
            return execution;
        }
        const int all_index = api.get_top(state);
        const std::size_t length = api.object_length(state, all_index);
        if (length == 0 || length > kMaximumShipCatalogItems ||
            length > std::numeric_limits<std::uint32_t>::max()) {
            execution.error =
                make_lua_error("ship_catalog_size_invalid", "舰船静态目录条目数不符合共享构建契约");
            return execution;
        }
        execution.page.total_count = static_cast<std::uint32_t>(length);
        if (start_index > execution.page.total_count) {
            execution.error = make_lua_error("ship_catalog_page_start_out_of_range",
                                             "start_index 不得大于舰船静态目录总数");
            return execution;
        }

        const std::uint32_t end_index = std::min(
            execution.page.total_count, static_cast<std::uint32_t>(start_index + page_size));
        execution.page.records.reserve(end_index - start_index);
        for (std::uint32_t index = start_index; index < end_index; ++index) {
            std::uint64_t id = 0;
            if (!read_catalog_id(api, state, all_index, index, &id, &error)) {
                execution.page.read_errors.push_back(ShipCatalogReadError{
                    .catalog_index = index,
                    .id = std::nullopt,
                    .code = "ship_catalog_id_invalid",
                    .message = std::move(error),
                });
                continue;
            }
            LuaValue raw;
            if (!materialize_record(api, state, config_table, sources, id, &raw, &error)) {
                execution.page.read_errors.push_back(ShipCatalogReadError{
                    .catalog_index = index,
                    .id = id,
                    .code = "ship_catalog_record_incomplete",
                    .message = std::move(error),
                });
                continue;
            }
            execution.page.records.push_back(ShipCatalogRecord{
                .id = id,
                .raw = std::move(raw),
            });
        }
        if (end_index < execution.page.total_count) {
            execution.page.next_index = end_index;
        }
        execution.page.complete = execution.page.read_errors.empty() &&
                                  execution.page.records.size() == end_index - start_index;
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error =
            make_lua_error("ship_catalog_exception",
                           "舰船静态目录分页读取发生本地异常: " + std::string(exception.what()));
        return execution;
    } catch (...) {
        execution.error =
            make_lua_error("ship_catalog_exception", "舰船静态目录分页读取发生未知本地异常");
        return execution;
    }
}

}  // namespace azlw::agent

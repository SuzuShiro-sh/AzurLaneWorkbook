// 实现装备静态配置和合成配方的有界分页读取与完整性诊断。

#include "equipment_config_snapshot.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <exception>
#include <limits>
#include <optional>
#include <set>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "lua/lua_reader.h"

namespace azlw::agent {
namespace {

constexpr std::size_t kMaximumConfigBaseDepth = 32;

constexpr auto kEquipmentStatisticsFields = std::to_array<const char*>({
    "ammo",
    "ammo_icon",
    "ammo_info",
    "anti_siren",
    "attribute_1",
    "attribute_2",
    "attribute_3",
    "base",
    "damage",
    "descrip",
    "equip_info",
    "equip_parameters",
    "hidden_skill_id",
    "icon",
    "id",
    "label",
    "name",
    "nationality",
    "part_main",
    "part_sub",
    "property_rate",
    "rarity",
    "skill_id",
    "speciality",
    "tech",
    "torpedo_ammo",
    "type",
    "value_1",
    "value_2",
    "value_3",
    "weapon_id",
});

constexpr auto kEquipmentTemplateFields = std::to_array<const char*>({
    "base",
    "destory_gold",
    "destory_item",
    "equip_limit",
    "group",
    "id",
    "important",
    "level",
    "next",
    "prev",
    "restore_gold",
    "restore_item",
    "ship_type_forbidden",
    "trans_use_gold",
    "trans_use_item",
    "type",
    "upgrade_formula_id",
});

constexpr auto kEquipmentConfigFields = std::to_array<const char*>({
    "ammo",
    "ammo_icon",
    "ammo_info",
    "anti_siren",
    "attribute_1",
    "attribute_2",
    "attribute_3",
    "base",
    "damage",
    "descrip",
    "equip_info",
    "equip_parameters",
    "hidden_skill_id",
    "icon",
    "id",
    "label",
    "name",
    "nationality",
    "part_main",
    "part_sub",
    "property_rate",
    "rarity",
    "skill_id",
    "speciality",
    "tech",
    "torpedo_ammo",
    "type",
    "value_1",
    "value_2",
    "value_3",
    "weapon_id",
    "destory_gold",
    "destory_item",
    "equip_limit",
    "group",
    "important",
    "level",
    "next",
    "prev",
    "restore_gold",
    "restore_item",
    "ship_type_forbidden",
    "trans_use_gold",
    "trans_use_item",
    "upgrade_formula_id",
});

/// Lua number 必须能无损表示正整数配置标识。
std::optional<std::uint64_t> positive_integer(const LuaValue& value) {
    if (value.kind != LuaValueKind::Number || !std::isfinite(value.number) || value.number <= 0.0 ||
        std::floor(value.number) != value.number ||
        value.number > static_cast<double>(9'007'199'254'740'991ULL)) {
        return std::nullopt;
    }
    return static_cast<std::uint64_t>(value.number);
}

/// 从字符串键对象中取得可空正整数；零值明确表示没有父配置。
std::optional<std::uint64_t> object_link(const LuaValue& value, std::string_view key) {
    if (value.kind != LuaValueKind::Object || value.keys.size() != value.values.size()) {
        return std::nullopt;
    }
    for (std::size_t index = 0; index < value.keys.size(); ++index) {
        if (value.keys[index].kind == LuaValueKey::Kind::String && value.keys[index].text == key) {
            if (value.values[index].kind == LuaValueKind::Number && value.values[index].number == 0.0) {
                return std::uint64_t{0};
            }
            return positive_integer(value.values[index]);
        }
    }
    return std::nullopt;
}

void append_value_errors(
    std::string_view source,
    const LuaValueResult& result,
    std::vector<std::string>* errors) {
    for (const std::string& error : result.read_errors) {
        errors->push_back(std::string(source) + ": " + error);
    }
}

/// 读取配置当前页并递归合并 `base` 链；来源表和当前页都不被修改。
std::optional<LuaValue> read_merged_pg_config(
    const LuaApi& api,
    lua_State* state,
    int table_index,
    std::uint64_t config_id,
    std::span<const char* const> fields,
    std::string_view source_name,
    std::set<std::uint64_t>* ancestors,
    std::size_t remaining_depth,
    std::vector<std::string>* errors) {
    if (remaining_depth == 0) {
        errors->push_back(std::string(source_name) + ": base 链超过 32 层");
        return std::nullopt;
    }
    if (!ancestors->insert(config_id).second) {
        errors->push_back(std::string(source_name) + ": base 链存在循环");
        return std::nullopt;
    }

    const int top = api.get_top(state);
    const int stable_table = api.stable_stack_index(state, table_index);
    if (api.get_number_index_protected(state, stable_table, static_cast<double>(config_id)) != 0) {
        errors->push_back(lua_failure_detail(
            api,
            state,
            std::string(source_name) + "[" + std::to_string(config_id) + "] 读取失败"));
        api.set_top(state, top);
        ancestors->erase(config_id);
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        errors->push_back(
            std::string(source_name) + "[" + std::to_string(config_id) + "] 不是 table");
        api.set_top(state, top);
        ancestors->erase(config_id);
        return std::nullopt;
    }
    const int row_index = api.get_top(state);
    std::string materialize_error;
    if (!push_lua_materialized_table(api, state, row_index, fields, &materialize_error)) {
        errors->push_back(std::string(source_name) + ": " + materialize_error);
        api.set_top(state, top);
        ancestors->erase(config_id);
        return std::nullopt;
    }
    LuaValueResult current_result = read_lua_value(api, state, -1);
    append_value_errors(source_name, current_result, errors);
    LuaValue current = std::move(current_result.value);
    api.set_top(state, top);

    LuaValue merged;
    merged.kind = LuaValueKind::Object;
    const std::optional<std::uint64_t> base_id = object_link(current, "base");
    if (base_id.has_value() && *base_id > 0 && *base_id != config_id) {
        const std::optional<LuaValue> inherited = read_merged_pg_config(
            api,
            state,
            stable_table,
            *base_id,
            fields,
            source_name,
            ancestors,
            remaining_depth - 1,
            errors);
        if (inherited.has_value()) {
            merged = *inherited;
        }
    }
    ancestors->erase(config_id);
    return merge_lua_objects(merged, current);
}

std::optional<LuaValue> read_pg_source(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    std::uint64_t config_id,
    std::span<const char* const> fields,
    std::vector<std::string>* errors) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_pg_config_table(api, state, table_name, &error)) {
        errors->push_back(std::move(error));
        api.set_top(state, top);
        return std::nullopt;
    }
    const int table_index = api.get_top(state);
    std::set<std::uint64_t> ancestors;
    std::optional<LuaValue> value = read_merged_pg_config(
        api,
        state,
        table_index,
        config_id,
        fields,
        table_name,
        &ancestors,
        kMaximumConfigBaseDepth,
        errors);
    api.set_top(state, top);
    return value;
}

bool push_equipment(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t config_id,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "Equipment", error)) {
        api.set_top(state, top);
        return false;
    }
    const int equipment_table = api.get_top(state);
    api.create_table(state, 0, 1);
    const int constructor_argument = api.get_top(state);
    api.push_number(state, static_cast<double>(config_id));
    api.set_field(state, constructor_argument, "id");
    const std::array arguments{LuaCallArgument::stack_value(constructor_argument)};
    if (!push_lua_table_function_result(
            api, state, equipment_table, "New", arguments, error)) {
        api.set_top(state, top);
        return false;
    }
    const int result_type = api.type(state, -1);
    if (result_type != kLuaTypeTable && result_type != kLuaTypeUserData) {
        *error = "Equipment.New 未返回 table 或 userdata";
        api.set_top(state, top);
        return false;
    }
    return true;
}

bool read_materialized_method_value(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const char* const> fields,
    LuaValue* output,
    std::vector<std::string>* errors) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_method_result(api, state, object_index, method_name, {}, &error)) {
        errors->push_back(std::string(method_name) + ": " + error);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable ||
        !push_lua_materialized_table(api, state, -1, fields, &error)) {
        api.set_top(state, top);
        errors->push_back(
            std::string(method_name) + ": " +
            (error.empty() ? "未返回 table" : error));
        return false;
    }
    LuaValueResult result = read_lua_value(api, state, -1);
    append_value_errors(method_name, result, errors);
    *output = std::move(result.value);
    api.set_top(state, top);
    return result.complete;
}

bool read_method_value(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    LuaValue* output,
    std::vector<std::string>* errors) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_method_result(api, state, object_index, method_name, {}, &error)) {
        errors->push_back(std::string(method_name) + ": " + error);
        return false;
    }
    LuaValueResult result = read_lua_value(api, state, -1);
    append_value_errors(method_name, result, errors);
    *output = std::move(result.value);
    api.set_top(state, top);
    return result.complete;
}

bool read_root_config_id(
    const LuaApi& api,
    lua_State* state,
    int equipment_index,
    std::optional<std::uint64_t>* output,
    std::vector<std::string>* errors) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_method_result(api, state, equipment_index, "GetRootEquipment", {}, &error)) {
        errors->push_back("GetRootEquipment: " + error);
        return false;
    }
    const int root_type = api.type(state, -1);
    if (root_type != kLuaTypeTable && root_type != kLuaTypeUserData) {
        api.set_top(state, top);
        errors->push_back("GetRootEquipment: 未返回 table 或 userdata");
        return false;
    }
    LuaNumber root = read_lua_number_field(api, state, -1, "configId");
    if (root.status != LuaNumberStatus::Present || root.value == 0) {
        root = read_lua_number_field(api, state, -1, "id");
    }
    api.set_top(state, top);
    if (root.status != LuaNumberStatus::Present || root.value == 0) {
        errors->push_back("GetRootEquipment: 根配置 ID 缺失或无效");
        return false;
    }
    *output = root.value;
    return true;
}

bool collect_weapon_ids(
    const LuaValue& value,
    std::vector<std::uint64_t>* output,
    std::vector<std::string>* errors) {
    if (value.kind != LuaValueKind::Array) {
        errors->push_back("GetWeaponID: 返回值不是连续数组");
        return false;
    }
    std::set<std::uint64_t> seen;
    for (const LuaValue& item : value.values) {
        const std::optional<std::uint64_t> identifier = positive_integer(item);
        if (!identifier.has_value() || !seen.insert(*identifier).second) {
            errors->push_back("GetWeaponID: 包含无效或重复的武器 ID");
            return false;
        }
        output->push_back(*identifier);
    }
    std::sort(output->begin(), output->end());
    return true;
}

bool read_optional_number_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::optional<double>* output,
    std::vector<std::string>* errors) {
    const int top = api.get_top(state);
    std::string error;
    if (!push_lua_method_result(api, state, object_index, method_name, {}, &error)) {
        errors->push_back(std::string(method_name) + ": " + error);
        return false;
    }
    if (api.type(state, -1) == kLuaTypeNil) {
        api.set_top(state, top);
        *output = std::nullopt;
        return true;
    }
    const std::optional<double> value = read_lua_nonnegative_real(api, state, -1);
    api.set_top(state, top);
    if (!value.has_value()) {
        errors->push_back(std::string(method_name) + ": 未返回可空非负有限数值");
        return false;
    }
    *output = *value;
    return true;
}

EquipmentConfigRecord read_equipment_config(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t config_id) {
    EquipmentConfigRecord record;
    record.config_id = config_id;
    const int top = api.get_top(state);

    const std::optional<LuaValue> statistics = read_pg_source(
        api,
        state,
        "equip_data_statistics",
        config_id,
        kEquipmentStatisticsFields,
        &record.read_errors);
    const std::optional<LuaValue> equipment_template = read_pg_source(
        api,
        state,
        "equip_data_template",
        config_id,
        kEquipmentTemplateFields,
        &record.read_errors);

    std::string error;
    if (!push_equipment(api, state, config_id, &error)) {
        record.read_errors.push_back("Equipment.New: " + error);
        api.set_top(state, top);
        return record;
    }
    const int equipment_index = api.get_top(state);
    LuaValue runtime_config;
    const bool runtime_complete = read_materialized_method_value(
        api,
        state,
        equipment_index,
        "getConfigTable",
        kEquipmentConfigFields,
        &runtime_config,
        &record.read_errors);

    LuaValue merged;
    merged.kind = LuaValueKind::Object;
    if (statistics.has_value()) {
        merged = merge_lua_objects(merged, *statistics);
    }
    if (equipment_template.has_value()) {
        merged = merge_lua_objects(merged, *equipment_template);
    }
    record.raw_config = merge_lua_objects(merged, runtime_config);

    bool methods_complete = read_root_config_id(
        api, state, equipment_index, &record.root_config_id, &record.read_errors);
    methods_complete = read_method_value(
                           api,
                           state,
                           equipment_index,
                           "GetAttributes",
                           &record.attributes,
                           &record.read_errors) &&
                       methods_complete;
    methods_complete = read_method_value(
                           api,
                           state,
                           equipment_index,
                           "GetPropertiesInfo",
                           &record.properties,
                           &record.read_errors) &&
                       methods_complete;
    methods_complete = read_method_value(
                           api,
                           state,
                           equipment_index,
                           "GetSkill",
                           &record.skill,
                           &record.read_errors) &&
                       methods_complete;
    methods_complete = read_method_value(
                           api,
                           state,
                           equipment_index,
                           "GetPropertyRate",
                           &record.property_rate,
                           &record.read_errors) &&
                       methods_complete;

    LuaValue weapon_ids;
    const bool weapon_value_complete = read_method_value(
        api,
        state,
        equipment_index,
        "GetWeaponID",
        &weapon_ids,
        &record.read_errors);
    methods_complete = collect_weapon_ids(
                           weapon_ids, &record.weapon_ids, &record.read_errors) &&
                       weapon_value_complete && methods_complete;

    const std::optional<std::uint64_t> gear_score =
        read_lua_number_method(api, state, equipment_index, "GetGearScore", {}, &error);
    if (!gear_score.has_value()) {
        record.read_errors.push_back("GetGearScore: " + error);
        methods_complete = false;
    } else {
        record.gear_score = *gear_score;
    }
    methods_complete = read_optional_number_method(
                           api,
                           state,
                           equipment_index,
                           "GetAntiSirenPower",
                           &record.anti_siren_power,
                           &record.read_errors) &&
                       methods_complete;

    const std::optional<bool> is_device =
        read_lua_boolean_method(api, state, equipment_index, "isDevice", {}, &error);
    if (!is_device.has_value()) {
        record.read_errors.push_back("isDevice: " + error);
        methods_complete = false;
    } else {
        record.is_device = *is_device;
    }
    const std::optional<bool> is_aircraft =
        read_lua_boolean_method(api, state, equipment_index, "isAircraft", {}, &error);
    if (!is_aircraft.has_value()) {
        record.read_errors.push_back("isAircraft: " + error);
        methods_complete = false;
    } else {
        record.is_aircraft = *is_aircraft;
    }

    record.complete = statistics.has_value() && equipment_template.has_value() &&
                      runtime_complete && methods_complete && record.read_errors.empty();
    api.set_top(state, top);
    return record;
}

bool read_catalog(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    int* table_index,
    int* all_index,
    std::uint32_t* total_count,
    AgentError* error) {
    std::string detail;
    if (!push_lua_pg_config_table(api, state, table_name, &detail)) {
        *error = make_lua_error("equipment_catalog_missing", std::move(detail), "same_request");
        return false;
    }
    *table_index = api.get_top(state);
    if (api.get_field_protected(state, *table_index, "all") != 0) {
        detail = lua_failure_detail(api, state, "读取配置目录 all 失败");
        *error = make_lua_error("equipment_catalog_index_failed", std::move(detail));
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = make_lua_error("equipment_catalog_index_invalid", "配置目录 all 不是 table");
        return false;
    }
    *all_index = api.get_top(state);
    const std::size_t length = api.object_length(state, *all_index);
    if (length == 0 || length > kMaximumEquipmentCatalogItems ||
        length > std::numeric_limits<std::uint32_t>::max()) {
        *error = make_lua_error(
            "equipment_catalog_size_invalid",
            "配置目录条目数必须位于 1 至 100000");
        return false;
    }
    *total_count = static_cast<std::uint32_t>(length);
    return true;
}

std::optional<std::uint64_t> read_catalog_id(
    const LuaApi& api,
    lua_State* state,
    int all_index,
    std::uint32_t zero_based_index,
    std::string* error) {
    const int top = api.get_top(state);
    if (api.get_number_index_protected(
            state, all_index, static_cast<double>(zero_based_index + 1U)) != 0) {
        *error = lua_failure_detail(api, state, "读取配置目录索引失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    const LuaNumber identifier = read_lua_number(api, state, -1);
    api.set_top(state, top);
    if (identifier.status != LuaNumberStatus::Present || identifier.value == 0) {
        *error = "配置目录索引不是正整数";
        return std::nullopt;
    }
    return identifier.value;
}

bool read_required_recipe_field(
    const LuaApi& api,
    lua_State* state,
    int row_index,
    const char* field,
    bool positive,
    std::uint64_t* output,
    std::string* error) {
    const LuaNumber value = read_lua_number_field(api, state, row_index, field);
    if (value.status != LuaNumberStatus::Present || (positive && value.value == 0)) {
        *error = std::string(field) +
                 (positive ? " 缺失或不是正整数" : " 缺失或不是非负整数");
        return false;
    }
    *output = value.value;
    return true;
}

}  // namespace

EquipmentConfigPageExecution snapshot_equipment_configs(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::string_view module_sha256) noexcept {
    EquipmentConfigPageExecution execution;
    LuaStackGuard stack_guard(api, state);
    try {
        if (page_size == 0 || page_size > kMaximumEquipmentFrameSize) {
            execution.error = make_lua_error(
                "equipment_page_size_out_of_range",
                "单帧 page_size 只允许 1 至 " + std::to_string(kMaximumEquipmentFrameSize));
            return execution;
        }
        execution.page.source.module_sha256.assign(module_sha256);
        execution.page.start_index = start_index;
        int table_index = 0;
        int all_index = 0;
        if (!read_catalog(
                api,
                state,
                "equip_data_template",
                &table_index,
                &all_index,
                &execution.page.total_count,
                &execution.error)) {
            return execution;
        }
        (void)table_index;
        if (start_index > execution.page.total_count) {
            execution.error = make_lua_error(
                "equipment_page_start_out_of_range",
                "start_index 不得大于装备目录总数");
            return execution;
        }
        const std::uint32_t end_index = std::min(
            execution.page.total_count,
            static_cast<std::uint32_t>(start_index + page_size));
        execution.page.configs.reserve(end_index - start_index);
        for (std::uint32_t index = start_index; index < end_index; ++index) {
            std::string error;
            const std::optional<std::uint64_t> config_id =
                read_catalog_id(api, state, all_index, index, &error);
            if (!config_id.has_value()) {
                execution.page.read_errors.push_back(EquipmentConfigReadError{
                    .catalog_index = index,
                    .config_id = std::nullopt,
                    .code = "equipment_catalog_id_invalid",
                    .message = std::move(error),
                });
                continue;
            }
            execution.page.configs.push_back(read_equipment_config(api, state, *config_id));
        }
        if (end_index < execution.page.total_count) {
            execution.page.next_index = end_index;
        }
        execution.page.complete = execution.page.read_errors.empty() &&
                                  std::all_of(
                                      execution.page.configs.begin(),
                                      execution.page.configs.end(),
                                      [](const EquipmentConfigRecord& record) {
                                          return record.complete;
                                      });
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = make_lua_error(
            "equipment_config_exception",
            "装备配置分页读取发生本地异常: " + std::string(exception.what()));
        return execution;
    } catch (...) {
        execution.error = make_lua_error(
            "equipment_config_exception", "装备配置分页读取发生未知本地异常");
        return execution;
    }
}

ComposeRecipePageExecution snapshot_compose_recipes(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::string_view module_sha256) noexcept {
    ComposeRecipePageExecution execution;
    LuaStackGuard stack_guard(api, state);
    try {
        if (page_size == 0 || page_size > kMaximumEquipmentFrameSize) {
            execution.error = make_lua_error(
                "compose_page_size_out_of_range",
                "单帧 page_size 只允许 1 至 " + std::to_string(kMaximumEquipmentFrameSize));
            return execution;
        }
        execution.page.source.module_sha256.assign(module_sha256);
        execution.page.start_index = start_index;
        int table_index = 0;
        int all_index = 0;
        if (!read_catalog(
                api,
                state,
                "compose_data_template",
                &table_index,
                &all_index,
                &execution.page.total_count,
                &execution.error)) {
            return execution;
        }
        if (start_index > execution.page.total_count) {
            execution.error = make_lua_error(
                "compose_page_start_out_of_range",
                "start_index 不得大于合成配方总数");
            return execution;
        }
        const std::uint32_t end_index = std::min(
            execution.page.total_count,
            static_cast<std::uint32_t>(start_index + page_size));
        execution.page.recipes.reserve(end_index - start_index);
        for (std::uint32_t index = start_index; index < end_index; ++index) {
            std::string error;
            const std::optional<std::uint64_t> recipe_id =
                read_catalog_id(api, state, all_index, index, &error);
            if (!recipe_id.has_value()) {
                execution.page.read_errors.push_back(ComposeRecipeReadError{
                    .catalog_index = index,
                    .recipe_id = std::nullopt,
                    .code = "compose_catalog_id_invalid",
                    .message = std::move(error),
                });
                continue;
            }

            const int top = api.get_top(state);
            if (api.get_number_index_protected(
                    state, table_index, static_cast<double>(*recipe_id)) != 0) {
                error = lua_failure_detail(api, state, "读取合成配方失败");
            } else if (api.type(state, -1) != kLuaTypeTable) {
                error = "合成配方不是 table";
            } else {
                const int row_index = api.get_top(state);
                EquipmentComposeRecipe recipe;
                if (read_required_recipe_field(
                        api, state, row_index, "id", true, &recipe.recipe_id, &error) &&
                    read_required_recipe_field(
                        api,
                        state,
                        row_index,
                        "material_id",
                        true,
                        &recipe.material_id,
                        &error) &&
                    read_required_recipe_field(
                        api,
                        state,
                        row_index,
                        "material_num",
                        true,
                        &recipe.material_count,
                        &error) &&
                    read_required_recipe_field(
                        api, state, row_index, "gold_num", false, &recipe.gold, &error) &&
                    read_required_recipe_field(
                        api, state, row_index, "equip_id", true, &recipe.equipment_id, &error) &&
                    recipe.recipe_id == *recipe_id) {
                    execution.page.recipes.push_back(std::move(recipe));
                } else if (error.empty()) {
                    error = "配方 id 与目录 id 不一致";
                }
            }
            api.set_top(state, top);
            if (!error.empty()) {
                execution.page.read_errors.push_back(ComposeRecipeReadError{
                    .catalog_index = index,
                    .recipe_id = recipe_id,
                    .code = "compose_recipe_invalid",
                    .message = std::move(error),
                });
            }
        }
        if (end_index < execution.page.total_count) {
            execution.page.next_index = end_index;
        }
        execution.page.complete = execution.page.read_errors.empty();
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = make_lua_error(
            "compose_recipe_exception",
            "合成配方分页读取发生本地异常: " + std::string(exception.what()));
        return execution;
    } catch (...) {
        execution.error = make_lua_error(
            "compose_recipe_exception", "合成配方分页读取发生未知本地异常");
        return execution;
    }
}

template <typename Execution, typename Page, typename Record, typename ReadError>
EquipmentBatchStep advance_static_catalog_batch(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t start_index,
    std::uint32_t page_size,
    const std::string& module_sha256,
    std::uint32_t* cursor,
    bool* started,
    Execution* outcome,
    std::vector<Record> Page::* records,
    std::vector<ReadError> Page::* errors,
    Execution (*read_frame)(
        const LuaApi&,
        lua_State*,
        std::uint32_t,
        std::uint32_t,
        std::string_view)) noexcept {
    auto fail = [&](AgentError error) {
        outcome->success = false;
        outcome->error = std::move(error);
        return EquipmentBatchStep::Finished;
    };
    if (cursor == nullptr || started == nullptr || outcome == nullptr || page_size == 0 ||
        page_size > kMaximumEquipmentPageSize || *cursor < start_index ||
        *cursor - start_index > page_size) {
        return fail(make_lua_error(
            "equipment_page_size_out_of_range", "装备静态目录批次超出共享页容量"));
    }
    const std::uint32_t filled = *cursor - start_index;
    if (filled == page_size) {
        return EquipmentBatchStep::Finished;
    }
    const std::uint32_t frame_size = std::min(kMaximumEquipmentFrameSize, page_size - filled);
    Execution frame = read_frame(api, state, *cursor, frame_size, module_sha256);
    if (!frame.success) {
        *outcome = std::move(frame);
        return EquipmentBatchStep::Finished;
    }
    if (frame.page.start_index != *cursor) {
        return fail(make_lua_error(
            "equipment_page_start_out_of_range", "装备静态目录帧起点与批次游标不一致"));
    }
    const std::uint32_t frame_consumed = static_cast<std::uint32_t>(
        (frame.page.*records).size() + (frame.page.*errors).size());
    if (!*started) {
        *outcome = std::move(frame);
        outcome->page.start_index = start_index;
        *started = true;
    } else if (frame.page.total_count != outcome->page.total_count) {
        return fail(make_lua_error(
            "equipment_catalog_size_invalid", "装备静态目录分页期间总数发生变化"));
    } else {
        (outcome->page.*records)
            .insert(
                (outcome->page.*records).end(),
                std::make_move_iterator((frame.page.*records).begin()),
                std::make_move_iterator((frame.page.*records).end()));
        (outcome->page.*errors)
            .insert(
                (outcome->page.*errors).end(),
                std::make_move_iterator((frame.page.*errors).begin()),
                std::make_move_iterator((frame.page.*errors).end()));
        outcome->page.next_index = frame.page.next_index;
        outcome->page.complete = outcome->page.complete && frame.page.complete;
        outcome->page.source = std::move(frame.page.source);
    }
    const std::uint32_t next_cursor = *cursor + frame_consumed;
    if (outcome->page.next_index.has_value() && *outcome->page.next_index != next_cursor) {
        return fail(make_lua_error(
            "equipment_page_start_out_of_range", "装备静态目录帧没有按已消费条目前进"));
    }
    *cursor = next_cursor;
    const std::uint32_t consumed = *cursor - start_index;
    if (outcome->page.next_index.has_value() && consumed < page_size) {
        return EquipmentBatchStep::Continue;
    }
    if (consumed > page_size) {
        return fail(make_lua_error(
            "equipment_page_size_out_of_range", "装备静态目录批次读取超过请求页容量"));
    }
    if (*cursor < outcome->page.total_count) {
        outcome->page.next_index = *cursor;
    } else {
        outcome->page.next_index.reset();
    }
    return EquipmentBatchStep::Finished;
}

EquipmentBatchStep advance_equipment_config_batch(
    const LuaApi& api,
    lua_State* state,
    EquipmentConfigBatchProgress* progress) noexcept {
    if (!progress->ids.empty()) {
        auto& execution = progress->outcome;
        auto& page = execution.page;
        page.selected_ids = true;
        page.source.module_sha256 = progress->module_sha256;
        if (!api.ready() || state == nullptr || progress->ids.size() > kMaximumEquipmentFrameSize) {
            execution.error = make_lua_error("equipment_config_batch_invalid", "装备 ID 批次或 Lua 状态无效");
            return EquipmentBatchStep::Finished;
        }
        LuaStackGuard stack(api, state);
        try {
            std::string detail;
            if (!push_lua_pg_config_table(api, state, "equip_data_template", &detail)) {
                execution.error = make_lua_error("equipment_config_table_invalid", std::move(detail));
                return EquipmentBatchStep::Finished;
            }
            const int table = api.get_top(state);
            for (const auto id : progress->ids) {
                if (api.get_number_index_protected(state, table, static_cast<double>(id)) != 0) {
                    execution.error = make_lua_error("equipment_config_lookup_failed", lua_failure_detail(api, state, "读取装备配置失败"));
                    return EquipmentBatchStep::Finished;
                }
                const bool missing = api.type(state, -1) == kLuaTypeNil;
                api.set_top(state, table);
                if (missing) page.missing_ids.push_back(id);
                else page.configs.push_back(read_equipment_config(api, state, id));
            }
            page.complete = std::all_of(page.configs.begin(), page.configs.end(), [](const auto& item) { return item.complete; });
            execution.success = true;
        } catch (const std::exception& error) {
            execution.error = make_lua_error("equipment_config_exception", error.what());
        } catch (...) {
            execution.error = make_lua_error("equipment_config_exception", "装备配置批次发生未知异常");
        }
        return EquipmentBatchStep::Finished;
    }
    return advance_static_catalog_batch(
        api,
        state,
        progress->start_index,
        progress->page_size,
        progress->module_sha256,
        &progress->cursor,
        &progress->started,
        &progress->outcome,
        &EquipmentConfigPage::configs,
        &EquipmentConfigPage::read_errors,
        snapshot_equipment_configs);
}

EquipmentBatchStep advance_compose_recipe_batch(
    const LuaApi& api,
    lua_State* state,
    ComposeRecipeBatchProgress* progress) noexcept {
    return advance_static_catalog_batch(
        api,
        state,
        progress->start_index,
        progress->page_size,
        progress->module_sha256,
        &progress->cursor,
        &progress->started,
        &progress->outcome,
        &ComposeRecipePage::recipes,
        &ComposeRecipePage::read_errors,
        snapshot_compose_recipes);
}

}  // namespace azlw::agent

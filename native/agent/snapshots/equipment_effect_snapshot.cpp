// 实现装备武器参数与技能效果的有界批量读取和逐来源诊断。

#include "equipment_effect_snapshot.h"

#include <array>
#include <exception>
#include <iterator>
#include <span>
#include <string>
#include <utility>

#include "lua/lua_reader.h"

namespace azlw::agent {
namespace {

constexpr auto kEquipmentWeaponFields = std::to_array<const char*>({
    "action_index",
    "aim_type",
    "angle",
    "attack_attribute",
    "attack_attribute_ratio",
    "auto_aftercast",
    "axis_angle",
    "barrage_ID",
    "base",
    "bullet_ID",
    "charge_param",
    "corrected",
    "damage",
    "effect_move",
    "expose",
    "fire_fx",
    "fire_fx_loop_type",
    "fire_sfx",
    "id",
    "initial_over_heat",
    "min_range",
    "oxy_type",
    "precast_param",
    "queue",
    "range",
    "recover_time",
    "reload_max",
    "search_condition",
    "search_type",
    "shakescreen",
    "spawn_bound",
    "suppress",
    "torpedo_ammo",
    "type",
});

constexpr auto kSkillEffectDisplayFields = std::to_array<const char*>({
    "id",
    "name",
    "desc",
    "desc_get",
    "system_transform",
});

AgentError snapshot_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.lua",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

bool push_pg_table(
    const LuaApi& api,
    lua_State* state,
    const char* field_name,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "pg", error)) {
        api.set_top(state, top);
        return false;
    }
    if (!push_lua_table_field(api, state, -1, field_name, error)) {
        *error = "读取 pg." + std::string(field_name) + " 失败: " + *error;
        api.set_top(state, top);
        return false;
    }
    return true;
}

EquipmentWeaponDetail read_weapon(
    const LuaApi& api,
    lua_State* state,
    int weapon_table_index,
    std::uint64_t weapon_id) {
    const int top = api.get_top(state);
    EquipmentWeaponDetail detail;
    detail.weapon_id = weapon_id;
    const int stable_table = api.stable_stack_index(state, weapon_table_index);
    if (api.get_number_index_protected(state, stable_table, static_cast<double>(weapon_id)) != 0) {
        detail.read_errors.push_back(lua_failure_detail(
            api,
            state,
            "读取 pg.weapon_property[" + std::to_string(weapon_id) + "] 失败"));
        api.set_top(state, top);
        return detail;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        detail.read_errors.push_back(
            "pg.weapon_property[" + std::to_string(weapon_id) + "] 不是 table");
        api.set_top(state, top);
        return detail;
    }
    LuaValueResult physical = read_lua_value(api, state, -1);
    std::string error;
    if (!push_lua_materialized_table(api, state, -1, kEquipmentWeaponFields, &error)) {
        detail.raw = std::move(physical.value);
        detail.read_errors = std::move(physical.read_errors);
        detail.read_errors.push_back("物化武器继承字段失败: " + error);
        api.set_top(state, top);
        return detail;
    }
    LuaValueResult materialized = read_lua_value(api, state, -1);
    detail.raw = merge_lua_objects(physical.value, materialized.value);
    detail.read_errors = std::move(physical.read_errors);
    detail.read_errors.insert(
        detail.read_errors.end(),
        std::make_move_iterator(materialized.read_errors.begin()),
        std::make_move_iterator(materialized.read_errors.end()));
    detail.complete = physical.complete && materialized.complete;
    api.set_top(state, top);
    return detail;
}

SkillEffectSource read_display_source(
    const LuaApi& api,
    lua_State* state,
    int display_table_index,
    std::uint64_t skill_id) {
    const int top = api.get_top(state);
    SkillEffectSource source;
    const int stable_table = api.stable_stack_index(state, display_table_index);
    if (api.get_number_index_protected(state, stable_table, static_cast<double>(skill_id)) != 0) {
        source.error = lua_failure_detail(
            api,
            state,
            "读取 pg.skill_data_template[" + std::to_string(skill_id) + "] 失败");
        api.set_top(state, top);
        return source;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        source.error =
            "pg.skill_data_template[" + std::to_string(skill_id) + "] 不是 table";
        api.set_top(state, top);
        return source;
    }
    std::string error;
    if (!push_lua_materialized_table(
            api,
            state,
            -1,
            kSkillEffectDisplayFields,
            &error)) {
        source.error = std::move(error);
        api.set_top(state, top);
        return source;
    }
    source.available = true;
    LuaValueResult value = read_lua_value(api, state, -1);
    source.value = std::move(value.value);
    source.read_errors = std::move(value.read_errors);
    source.complete = value.complete;
    api.set_top(state, top);
    return source;
}

bool push_battle_data_function(
    const LuaApi& api,
    lua_State* state,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "ys", error) ||
        !push_lua_table_field(api, state, -1, "Battle", error) ||
        !push_lua_table_field(api, state, -1, "BattleDataFunction", error)) {
        api.set_top(state, top);
        return false;
    }
    return true;
}

SkillEffectSource read_battle_source(
    const LuaApi& api,
    lua_State* state,
    int battle_data_function_index,
    const char* function_name,
    const SkillEffectQuery& query) {
    const int top = api.get_top(state);
    SkillEffectSource source;
    const std::array arguments{
        LuaCallArgument::number(static_cast<double>(query.skill_id)),
        LuaCallArgument::number(static_cast<double>(query.level)),
    };
    std::string error;
    if (!push_lua_table_function_result(
            api,
            state,
            battle_data_function_index,
            function_name,
            arguments,
            &error)) {
        source.error = std::move(error);
        api.set_top(state, top);
        return source;
    }
    source.available = true;
    LuaValueResult value = read_lua_value(api, state, -1);
    source.value = std::move(value.value);
    source.read_errors = std::move(value.read_errors);
    source.complete = value.complete;
    api.set_top(state, top);
    return source;
}

SkillEffectDetail read_skill(
    const LuaApi& api,
    lua_State* state,
    int display_table_index,
    int battle_data_function_index,
    const SkillEffectQuery& query) {
    SkillEffectDetail detail;
    detail.skill_id = query.skill_id;
    detail.level = query.level;
    detail.display = read_display_source(api, state, display_table_index, query.skill_id);
    detail.battle_skill = read_battle_source(
        api,
        state,
        battle_data_function_index,
        "GetSkillTemplate",
        query);
    detail.battle_buff = read_battle_source(
        api,
        state,
        battle_data_function_index,
        "GetBuffTemplate",
        query);
    const bool has_battle_source = detail.battle_skill.available || detail.battle_buff.available;
    const bool available_sources_complete =
        (!detail.battle_skill.available || detail.battle_skill.complete) &&
        (!detail.battle_buff.available || detail.battle_buff.complete);
    detail.complete = detail.display.complete && has_battle_source && available_sources_complete;
    return detail;
}

}  // namespace

EquipmentWeaponBatchExecution snapshot_equipment_weapons(
    const LuaApi& api,
    lua_State* state,
    std::span<const std::uint64_t> weapon_ids,
    std::string_view module_sha256) noexcept {
    EquipmentWeaponBatchExecution execution;
    execution.batch.source.module_sha256 = module_sha256;
    try {
        if (!api.ready() || state == nullptr) {
            execution.error = snapshot_error(
                "lua_state_unavailable",
                "Lua 状态或必需函数表尚未就绪");
            return execution;
        }
        LuaStackGuard stack_guard(api, state);
        std::string error;
        if (!push_pg_table(api, state, "weapon_property", &error)) {
            execution.error = snapshot_error("weapon_property_unavailable", std::move(error));
            return execution;
        }
        const int weapon_table_index = api.get_top(state);
        execution.batch.weapons.reserve(weapon_ids.size());
        for (const std::uint64_t weapon_id : weapon_ids) {
            execution.batch.weapons.push_back(
                read_weapon(api, state, weapon_table_index, weapon_id));
        }
        execution.batch.complete = !execution.batch.weapons.empty();
        for (const EquipmentWeaponDetail& detail : execution.batch.weapons) {
            execution.batch.complete = execution.batch.complete && detail.complete;
        }
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = snapshot_error(
            "equipment_weapon_exception",
            "读取装备武器参数时发生异常: " + std::string(exception.what()));
        return execution;
    } catch (...) {
        execution.error = snapshot_error(
            "equipment_weapon_exception",
            "读取装备武器参数时发生未知异常");
        return execution;
    }
}

SkillEffectBatchExecution snapshot_skill_effects(
    const LuaApi& api,
    lua_State* state,
    std::span<const SkillEffectQuery> skills,
    std::string_view module_sha256) noexcept {
    SkillEffectBatchExecution execution;
    execution.batch.source.module_sha256 = module_sha256;
    try {
        if (!api.ready() || state == nullptr) {
            execution.error = snapshot_error(
                "lua_state_unavailable",
                "Lua 状态或必需函数表尚未就绪");
            return execution;
        }
        LuaStackGuard stack_guard(api, state);
        std::string error;
        if (!push_pg_table(api, state, "skill_data_template", &error)) {
            execution.error = snapshot_error("skill_display_unavailable", std::move(error));
            return execution;
        }
        const int display_table_index = api.get_top(state);
        if (!push_battle_data_function(api, state, &error)) {
            execution.error = snapshot_error("battle_data_function_unavailable", std::move(error));
            return execution;
        }
        const int battle_data_function_index = api.get_top(state);
        execution.batch.skills.reserve(skills.size());
        for (const SkillEffectQuery& query : skills) {
            execution.batch.skills.push_back(read_skill(
                api,
                state,
                display_table_index,
                battle_data_function_index,
                query));
        }
        execution.batch.complete = !execution.batch.skills.empty();
        for (const SkillEffectDetail& detail : execution.batch.skills) {
            execution.batch.complete = execution.batch.complete && detail.complete;
        }
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = snapshot_error(
            "skill_effect_exception",
            "读取技能效果证据时发生异常: " + std::string(exception.what()));
        return execution;
    } catch (...) {
        execution.error = snapshot_error(
            "skill_effect_exception",
            "读取技能效果证据时发生未知异常");
        return execution;
    }
}

}  // namespace azlw::agent

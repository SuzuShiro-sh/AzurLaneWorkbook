// 实现舰船自身技能的确定性枚举、身份校验和 Lua 栈恢复。

#include "ship_skill_reader.h"

#include <cstdint>
#include <limits>
#include <optional>
#include <unordered_set>

#include "lua/lua_reader.h"

namespace azlw::agent {

bool visit_owned_ship_skills(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    const ShipSkillVisitor& visitor,
    ShipSkillVisitError* error) {
    const int top = api.get_top(state);
    if (api.get_field_protected(state, ship_index, "skills") != 0) {
        *error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_skills_lookup_failed",
            .message = lua_failure_detail(api, state, "读取舰船 skills 字段失败"),
        };
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_skills_invalid",
            .message = "舰船 skills 字段不是 table",
        };
        return false;
    }

    const int skills_index = api.get_top(state);
    std::unordered_set<std::uint64_t> skill_ids;
    skill_ids.reserve(kMaximumShipSkillCount);
    std::uint32_t visited = 0;
    api.push_nil(state);
    while (api.next(state, skills_index) != 0) {
        const int skill_value_index = api.get_top(state);
        const LuaNumber key_id = read_lua_number(api, state, -2);
        const std::optional<std::uint64_t> known_skill_id =
            key_id.status == LuaNumberStatus::Present && key_id.value > 0
                ? std::optional<std::uint64_t>(key_id.value)
                : std::nullopt;
        if (visited >= kMaximumShipSkillCount) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = known_skill_id,
                .code = "ship_skill_limit_exceeded",
                .message = "舰船自身技能数量超过协议安全上限",
            };
            return false;
        }
        ++visited;
        if (!known_skill_id.has_value()) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = std::nullopt,
                .code = "ship_skill_id_invalid",
                .message = "舰船 skills 的键不是正整数",
            };
            return false;
        }
        if (api.type(state, skill_value_index) != kLuaTypeTable) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = known_skill_id,
                .code = "ship_skill_invalid",
                .message = "舰船技能值不是 table",
            };
            return false;
        }

        const LuaNumber object_id =
            read_lua_number_field(api, state, skill_value_index, "id");
        if (object_id.status != LuaNumberStatus::Present || object_id.value == 0) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = known_skill_id,
                .code = "ship_skill_id_invalid",
                .message = "舰船技能 id 缺失或不是正整数",
            };
            return false;
        }
        if (object_id.value != *known_skill_id) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = object_id.value,
                .code = "ship_skill_id_mismatch",
                .message = "舰船 skills 的键与技能 id 不一致",
            };
            return false;
        }
        if (!skill_ids.insert(object_id.value).second) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = object_id.value,
                .code = "duplicate_ship_skill_id",
                .message = "舰船包含重复 skill_id",
            };
            return false;
        }

        const LuaNumber level =
            read_lua_number_field(api, state, skill_value_index, "level");
        if (level.status != LuaNumberStatus::Present || level.value == 0 ||
            level.value > std::numeric_limits<std::uint32_t>::max()) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = object_id.value,
                .code = "ship_skill_level_invalid",
                .message = "舰船技能 level 缺失、为零或超出 u32",
            };
            return false;
        }
        const LuaNumber experience =
            read_lua_number_field(api, state, skill_value_index, "exp");
        if (experience.status != LuaNumberStatus::Present) {
            api.set_top(state, top);
            *error = ShipSkillVisitError{
                .skill_id = object_id.value,
                .code = "ship_skill_experience_invalid",
                .message = "舰船技能 exp 缺失或不是非负整数",
            };
            return false;
        }

        const OwnedShipSkill skill{
            .skill_id = object_id.value,
            .level = static_cast<std::uint32_t>(level.value),
            .experience = experience.value,
        };
        ShipSkillVisitError visitor_error;
        if (!visitor(skill_value_index, skill, &visitor_error)) {
            api.set_top(state, top);
            if (!visitor_error.skill_id.has_value()) {
                visitor_error.skill_id = skill.skill_id;
            }
            *error = std::move(visitor_error);
            return false;
        }
        // `lua_next` 继续迭代时必须只保留当前键，访问者的临时值一并清理。
        api.set_top(state, skill_value_index - 1);
    }
    api.set_top(state, top);
    return true;
}

}  // namespace azlw::agent

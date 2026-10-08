// 声明运行态快照共用的舰船自身技能枚举与原始进度校验入口。

#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 定位技能表结构、身份或访问者转换失败的稳定诊断。
struct ShipSkillVisitError final {
    std::optional<std::uint64_t> skill_id;
    std::string code;
    std::string message;
};

/// 访问已经通过键、对象 ID、等级、经验和唯一性校验的自身技能。
using ShipSkillVisitor = std::function<bool(
    int object_index,
    const OwnedShipSkill& skill,
    ShipSkillVisitError* error)>;

/// 只遍历 `Ship.skills`，拒绝装备技能、触发技能及无法稳定定位的条目。
bool visit_owned_ship_skills(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    const ShipSkillVisitor& visitor,
    ShipSkillVisitError* error);

}  // namespace azlw::agent

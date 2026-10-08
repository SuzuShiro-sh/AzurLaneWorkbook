// 声明从 FleetProxy 持久编队建立舰船成员关系索引的只读入口。

#pragma once

#include <cstdint>
#include <unordered_map>
#include <vector>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

using ShipFleetMembershipIndex =
    std::unordered_map<std::uint64_t, std::vector<ShipFleetMembership>>;

/// 读取普通、潜艇和演习编队；活动与挑战临时编队不在 FleetProxy.data 中。
bool read_persistent_ship_fleet_memberships(
    const LuaApi& api,
    lua_State* state,
    ShipFleetMembershipIndex* memberships,
    AgentError* error);

}  // namespace azlw::agent

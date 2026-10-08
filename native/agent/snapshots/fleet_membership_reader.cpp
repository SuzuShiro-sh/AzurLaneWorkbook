// 实现 FleetProxy 持久编队的严格读取、成员校验和确定性排序。

#include "fleet_membership_reader.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <limits>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <unordered_set>
#include <utility>

#include "lua/lua_reader.h"

namespace azlw::agent {
namespace {

struct TeamField final {
    const char* lua_name;
    const char* protocol_name;
};

constexpr std::array kTeamFields{
    TeamField{"mainShips", "main"},
    TeamField{"vanguardShips", "vanguard"},
    TeamField{"subShips", "submarine"},
};

bool fail(
    AgentError* error,
    std::string code,
    std::string message,
    std::string retry = "never") {
    *error = make_lua_error(std::move(code), std::move(message), std::move(retry));
    return false;
}

std::optional<std::string> read_fleet_display_name(
    const LuaApi& api,
    lua_State* state,
    int fleet_index,
    std::string* error) {
    const std::optional<std::string> custom_name =
        read_lua_text_field(api, state, fleet_index, "name", 512, true, error);
    if (!custom_name.has_value()) {
        return std::nullopt;
    }
    if (!custom_name->empty()) {
        return custom_name;
    }

    const int top = api.get_top(state);
    if (api.get_field_protected(state, fleet_index, "defaultName") != 0) {
        *error = lua_failure_detail(api, state, "读取编队 defaultName 失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    if (api.type(state, -1) == kLuaTypeNil) {
        api.set_top(state, top);
        return std::string{};
    }
    std::optional<std::string> default_name =
        read_lua_text(api, state, -1, 512, false, error);
    api.set_top(state, top);
    if (!default_name.has_value()) {
        *error = "编队 defaultName 无效: " + *error;
    }
    return default_name;
}

std::optional<std::string> read_fleet_kind(
    const LuaApi& api,
    lua_State* state,
    int fleet_index,
    std::string* error) {
    const std::optional<bool> regular =
        read_lua_boolean_method(api, state, fleet_index, "isRegularFleet", {}, error);
    const std::optional<bool> submarine = regular.has_value()
                                                ? read_lua_boolean_method(
                                                      api,
                                                      state,
                                                      fleet_index,
                                                      "isSubmarineFleet",
                                                      {},
                                                      error)
                                                : std::nullopt;
    const std::optional<bool> exercise = submarine.has_value()
                                               ? read_lua_boolean_method(
                                                     api,
                                                     state,
                                                     fleet_index,
                                                     "isPVPFleet",
                                                     {},
                                                     error)
                                               : std::nullopt;
    if (!regular.has_value() || !submarine.has_value() || !exercise.has_value()) {
        return std::nullopt;
    }
    if (*exercise && !*regular && !*submarine) {
        return std::string{"exercise"};
    }
    if (!*exercise && *regular && *submarine) {
        return std::string{"submarine"};
    }
    if (!*exercise && *regular && !*submarine) {
        return std::string{"regular"};
    }
    *error = "编队类型方法返回了相互矛盾或未知的组合";
    return std::nullopt;
}

bool read_team_members(
    const LuaApi& api,
    lua_State* state,
    int fleet_index,
    std::uint32_t fleet_id,
    const std::optional<std::string>& display_name,
    std::string_view kind,
    const TeamField& team,
    std::unordered_set<std::uint64_t>* fleet_ship_ids,
    ShipFleetMembershipIndex* memberships,
    std::string* error) {
    const int top = api.get_top(state);
    if (api.get_field_protected(state, fleet_index, team.lua_name) != 0) {
        *error = lua_failure_detail(
            api, state, "读取编队 " + std::string(team.lua_name) + " 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "编队 " + std::string(team.lua_name) + " 不是 table";
        api.set_top(state, top);
        return false;
    }
    const int team_index = api.get_top(state);
    const std::size_t count = api.object_length(state, team_index);
    if (count > kMaximumFleetTeamShipCount) {
        *error = "编队 " + std::string(team.lua_name) + " 超过三个位置";
        api.set_top(state, top);
        return false;
    }
    const bool submarine_team = std::string_view(team.protocol_name) == "submarine";
    const bool submarine_fleet = kind == "submarine";
    if (count > 0 && submarine_team != submarine_fleet) {
        *error = "编队类型与非空队伍 " + std::string(team.protocol_name) + " 不一致";
        api.set_top(state, top);
        return false;
    }

    for (std::size_t offset = 0; offset < count; ++offset) {
        const std::uint32_t position = static_cast<std::uint32_t>(offset + 1);
        if (api.get_number_index_protected(
                state, team_index, static_cast<double>(position)) != 0) {
            *error = lua_failure_detail(api, state, "读取编队位置失败");
            api.set_top(state, top);
            return false;
        }
        const LuaNumber ship_id = read_lua_number(api, state, -1);
        api.set_top(state, team_index);
        if (ship_id.status != LuaNumberStatus::Present || ship_id.value == 0) {
            *error = "编队成员 ship_id 缺失或不是正整数";
            api.set_top(state, top);
            return false;
        }
        if (!fleet_ship_ids->insert(ship_id.value).second) {
            *error = "同一编队重复引用 ship_id=" + std::to_string(ship_id.value);
            api.set_top(state, top);
            return false;
        }
        auto& ship_memberships = (*memberships)[ship_id.value];
        if (ship_memberships.size() >= kMaximumShipFleetMembershipCount) {
            *error = "单艘舰船的持久编队关系超过协议上限";
            api.set_top(state, top);
            return false;
        }
        ship_memberships.push_back(ShipFleetMembership{
            .fleet_id = fleet_id,
            .display_name = display_name,
            .kind = std::string(kind),
            .team = team.protocol_name,
            .position = position,
        });
    }
    api.set_top(state, top);
    return true;
}

}  // namespace

bool read_persistent_ship_fleet_memberships(
    const LuaApi& api,
    lua_State* state,
    ShipFleetMembershipIndex* memberships,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    if (!push_lua_proxy(api, state, "FleetProxy", "lua_fleet_proxy_invalid", error)) {
        return false;
    }
    const int proxy_index = api.get_top(state);
    if (api.get_field_protected(state, proxy_index, "data") != 0) {
        return fail(
            error,
            "lua_fleet_data_lookup_failed",
            lua_failure_detail(api, state, "读取 FleetProxy.data 失败"),
            "same_request");
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        return fail(
            error,
            "lua_fleet_data_invalid",
            "FleetProxy.data 尚未成为 Lua table",
            "same_request");
    }
    const int data_index = api.get_top(state);

    std::unordered_set<std::uint32_t> fleet_ids;
    std::uint32_t visited = 0;
    api.push_nil(state);
    while (api.next(state, data_index) != 0) {
        if (visited >= kMaximumPersistentFleetCount) {
            return fail(
                error,
                "lua_fleet_limit_exceeded",
                "FleetProxy.data 超过持久编队数量上限");
        }
        ++visited;
        const int fleet_index = api.get_top(state);
        const int fleet_type = api.type(state, fleet_index);
        if (fleet_type != kLuaTypeTable && fleet_type != kLuaTypeUserData) {
            return fail(error, "lua_fleet_entry_invalid", "持久编队值不是 table 或 userdata");
        }

        const LuaNumber key_id = read_lua_number(api, state, -2);
        const LuaNumber object_id = read_lua_number_field(api, state, fleet_index, "id");
        if (key_id.status != LuaNumberStatus::Present || key_id.value == 0 ||
            object_id.status != LuaNumberStatus::Present || object_id.value == 0 ||
            key_id.value != object_id.value ||
            object_id.value > std::numeric_limits<std::uint32_t>::max()) {
            return fail(
                error,
                "lua_fleet_id_invalid",
                "持久编队键与对象 id 必须是相同的 u32 正整数");
        }
        const std::uint32_t fleet_id = static_cast<std::uint32_t>(object_id.value);
        if (!fleet_ids.insert(fleet_id).second) {
            return fail(error, "lua_fleet_id_duplicate", "FleetProxy.data 包含重复编队 id");
        }

        std::string read_error;
        const std::optional<std::string> display_name =
            read_fleet_display_name(api, state, fleet_index, &read_error);
        if (!display_name.has_value()) {
            return fail(error, "lua_fleet_name_invalid", std::move(read_error));
        }
        const std::optional<std::string> kind =
            read_fleet_kind(api, state, fleet_index, &read_error);
        if (!kind.has_value()) {
            return fail(error, "lua_fleet_kind_invalid", std::move(read_error));
        }

        std::unordered_set<std::uint64_t> fleet_ship_ids;
        for (const TeamField& team : kTeamFields) {
            if (!read_team_members(
                    api,
                    state,
                    fleet_index,
                    fleet_id,
                    display_name->empty()
                        ? std::optional<std::string>{}
                        : std::optional<std::string>{*display_name},
                    *kind,
                    team,
                    &fleet_ship_ids,
                    memberships,
                    &read_error)) {
                return fail(error, "lua_fleet_members_invalid", std::move(read_error));
            }
        }
        api.set_top(state, fleet_index - 1);
    }
    if (visited == 0) {
        return fail(
            error,
            "lua_fleet_data_empty",
            "FleetProxy.data 尚未加载持久编队",
            "same_request");
    }

    for (auto& [ship_id, ship_memberships] : *memberships) {
        static_cast<void>(ship_id);
        std::sort(
            ship_memberships.begin(),
            ship_memberships.end(),
            [](const ShipFleetMembership& left, const ShipFleetMembership& right) {
                return std::tie(left.fleet_id, left.team, left.position) <
                       std::tie(right.fleet_id, right.team, right.position);
            });
    }
    return true;
}

}  // namespace azlw::agent

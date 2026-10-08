// 声明舰船养成、自身技能、装备、背包和玩家资源的一致运行态采集入口。

#pragma once

#include <cstdint>
#include <algorithm>
#include <string>

#include <unordered_set>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"
#include "snapshots/ship_details_snapshot.h"

namespace azlw::agent {

/// 持有对象查询的单船条目，按选择器携带养成、名称和可选详情。
struct OwnedQueryShip final {
    OwnedShip ship;
    std::string name;
    std::optional<ShipDetail> details;
};
struct OwnedQueryEquipmentLocation final {
    std::uint64_t ship_id = 0;
    std::uint32_t slot_index = 0;
    std::uint64_t equipment_id = 0;
};
struct OwnedQueryEquipment final {
    EquipmentSnapshot equipment;
    std::uint64_t warehouse_quantity = 0;
    std::vector<OwnedQueryEquipmentLocation> equipped;
};
struct OwnedQueryExecution final {
    bool success = false;
    OwnedQuery query;
    std::vector<OwnedQueryShip> ships;
    std::vector<OwnedQueryEquipment> equipment;
    std::vector<std::uint64_t> missing_ids;
    AgentError error;
};
struct OwnedQueryProgress final {
    OwnedQuery query;
    std::uint32_t cursor = 0;
    std::uint64_t resume_key = 0;
    bool warehouse_finished = false;
    std::unordered_set<std::uint64_t> seen_ids;
};
/// 判断规范化后的字段选择；身份字段始终存在。
inline bool owned_query_has_field(const OwnedQuery& query, std::string_view field) {
    if (field == "config_id" || (query.kind == "ships" && field == "ship_id")) return true;
    if (query.fields.empty()) {
        return query.kind == "ships" ? field == "name" || field == "level"
                                     : field == "enhance_level" || field == "warehouse_quantity" || field == "equipped";
    }
    return std::find(query.fields.begin(), query.fields.end(), field) != query.fields.end();
}
/// 返回 true 表示整次查询结束；每帧最多读取现有船坞页容量个对象。
bool advance_owned_query(const LuaApi& api, lua_State* state,
                         OwnedQueryProgress* progress, OwnedQueryExecution* execution) noexcept;

/// 玩家物资与装备容量读取的成功数据或稳定错误。
struct ResourcesExecution final {
    bool success = false;
    PlayerResources player;
    AgentError error;
};
ResourcesExecution snapshot_resources(const LuaApi& api, lua_State* state) noexcept;

/// 同时承载一次完整运行态快照的成功数据或稳定错误。
struct OwnedStateExecution final {
    bool success = false;
    OwnedStateSnapshot snapshot;
    AgentError error;
};

/// 从游戏装备对象读取运行态标识、配置标识和用户可见强化等级。
bool read_equipment_snapshot(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    EquipmentSnapshot* equipment,
    std::string* error,
    bool include_enhance_level = true);

/// 账号前窗口跨帧续办时的阶段。船坞按页推进，仓库和背包各占后续帧。
enum class AccountBeforePhase : std::uint8_t { Dock, Warehouse, Bag };

/// 同一次账号前窗口在帧之间保留的船坞游标和已读快照。
struct AccountBeforeProgress final {
    std::uint32_t max_ships = 0;
    std::uint32_t max_equipments = 0;
    std::uint32_t max_items = 0;
    std::string module_sha256;
    std::uint32_t dock_cursor = 0;
    std::uint64_t dock_resume_key = 0;
    std::unordered_set<std::uint64_t> seen_ship_ids;
    std::uint32_t dock_frames = 0;
    AccountBeforePhase phase = AccountBeforePhase::Dock;
    OwnedStateSnapshot owned;
    ShipDetailsSnapshot details;
};

/// 账号前窗口的最终结果。dock_frames 只计船坞分页，不含仓库和背包帧。
struct AccountBeforeExecution final {
    bool success = false;
    OwnedStateSnapshot owned;
    ShipDetailsSnapshot details;
    std::uint32_t dock_frames = 0;
    AgentError error;
};

enum class AccountBeforeStep : std::uint8_t { Continue, Finished };

/// 一次已完成的联合快照可以锁存的读取就绪。失败或不完整保持 false。
struct AccountBeforeReadiness final {
    bool bag = false;
    bool owned_state = false;
    bool ship_details = false;
};

inline AccountBeforeReadiness account_before_readiness(
    const AccountBeforeExecution& outcome) noexcept {
    AccountBeforeReadiness readiness;
    if (!outcome.success) {
        return readiness;
    }
    readiness.bag = outcome.owned.bag.complete;
    readiness.owned_state = outcome.owned.complete;
    readiness.ship_details = outcome.details.complete;
    return readiness;
}

/// 推进一帧。返回 Continue 时进度已保存，调用方不得结束任务。
AccountBeforeStep advance_account_before(
    const LuaApi& api,
    lua_State* state,
    AccountBeforeProgress* progress,
    AccountBeforeExecution* execution) noexcept;

/// 必须由 `tolua_update` 所在线程调用，所有数据在同一次主线程停顿内读取。
OwnedStateExecution snapshot_owned_state(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    std::uint32_t max_equipments,
    std::uint32_t max_items) noexcept;

}  // namespace azlw::agent

// 实现在同一次游戏主线程停顿内读取舰船养成、装备、背包和玩家资源。

#include "owned_state_snapshot.h"

#include <exception>
#include <algorithm>
#include <array>
#include <cstdint>
#include <limits>
#include <optional>
#include <string>
#include <unordered_set>
#include <utility>

#include "bag_snapshot.h"
#include "bay_ship_reader.h"
#include "fleet_membership_reader.h"
#include "ship_details_snapshot.h"
#include "lua/lua_reader.h"
#include "ship_skill_reader.h"

namespace azlw::agent {

/// 从装备对象读取配置标识和强化等级；强化等级按用户可见的 `+N` 表示。
bool read_equipment_snapshot(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    EquipmentSnapshot* equipment,
    std::string* error,
    bool include_enhance_level) {
    const int value_type = api.type(state, object_index);
    if (value_type != kLuaTypeTable && value_type != kLuaTypeUserData) {
        *error = "装备值不是 table 或 userdata";
        return false;
    }

    const LuaNumber equipment_id = read_lua_number_field(api, state, object_index, "id");
    if (equipment_id.status != LuaNumberStatus::Present || equipment_id.value == 0) {
        *error = "装备 id 缺失或不是正整数";
        return false;
    }
    const LuaNumber config_id = read_lua_number_field(api, state, object_index, "configId");
    if (config_id.status == LuaNumberStatus::Invalid ||
        (config_id.status == LuaNumberStatus::Present && config_id.value == 0)) {
        *error = "装备 configId 不是正整数";
        return false;
    }

    equipment->equipment_id = equipment_id.value;
    equipment->config_id = config_id.status == LuaNumberStatus::Present ? config_id.value : equipment_id.value;
    if (!include_enhance_level) return true;

    const std::array arguments{LuaCallArgument::string("level")};
    const std::optional<std::uint64_t> config_level = read_lua_number_method(
        api, state, object_index, "getConfig", arguments, error);
    if (!config_level.has_value() || *config_level == 0 ||
        *config_level - 1 > std::numeric_limits<std::uint32_t>::max()) {
        if (config_level.has_value()) {
            *error = "装备配置 level 超出可表示范围";
        }
        return false;
    }

    equipment->enhance_level = static_cast<std::uint32_t>(*config_level - 1);
    return true;
}

namespace {

/// 读取受保护字段访问留下的 Lua 错误，避免逐项诊断丢失根因。
std::string field_failure_detail(
    const LuaApi& api,
    lua_State* state,
    std::string message) {
    std::string detail_error;
    const std::optional<std::string> detail =
        read_lua_text(api, state, -1, 4096, true, &detail_error);
    if (detail.has_value() && !detail->empty()) {
        message += ": " + *detail;
    }
    return message;
}

/// 追加单艘舰船或槽位读取错误。
void add_ship_error(
    DockSnapshot* snapshot,
    std::optional<std::uint64_t> ship_id,
    std::optional<std::uint32_t> slot_index,
    std::string code,
    std::string message,
    std::optional<std::uint64_t> skill_id = std::nullopt) {
    snapshot->read_errors.push_back(ShipReadError{
        .ship_id = ship_id,
        .skill_id = skill_id,
        .slot_index = slot_index,
        .code = std::move(code),
        .message = std::move(message),
    });
}

/// 读取舰船必需的精确整数；`positive` 用于标识和等级等不可为零的字段。
bool read_required_ship_number(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    const char* field_name,
    bool positive,
    std::uint64_t* value,
    std::string* error) {
    const LuaNumber field = read_lua_number_field(api, state, ship_index, field_name);
    if (field.status != LuaNumberStatus::Present || (positive && field.value == 0)) {
        *error = "舰船 " + std::string(field_name) +
                 (positive ? " 缺失或不是正整数" : " 缺失或不是非负整数");
        return false;
    }
    *value = field.value;
    return true;
}

/// 复用共享技能身份校验，并把具体诊断映射到船坞错误契约。
bool read_ship_skills(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    std::uint64_t ship_id,
    std::vector<OwnedShipSkill>* skills,
    DockSnapshot* snapshot) {
    ShipSkillVisitError visit_error;
    if (!visit_owned_ship_skills(
            api,
            state,
            ship_index,
            [&](int, const OwnedShipSkill& skill, ShipSkillVisitError*) {
                skills->push_back(skill);
                return true;
            },
            &visit_error)) {
        add_ship_error(
            snapshot,
            ship_id,
            std::nullopt,
            std::move(visit_error.code),
            std::move(visit_error.message),
            visit_error.skill_id);
        return false;
    }
    std::sort(
        skills->begin(),
        skills->end(),
        [](const OwnedShipSkill& left, const OwnedShipSkill& right) {
            return left.skill_id < right.skill_id;
        });
    return true;
}

/// 读取单艘舰船的养成、技能和固定五槽；任一字段畸形时不返回部分舰船。
bool read_ship(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    std::uint64_t ship_id,
    OwnedShip* ship,
    DockSnapshot* snapshot,
    const OwnedQuery* query = nullptr) {
    const auto selected = [&](std::string_view field) {
        // 装备查询仅借用槽位读取，不要求舰船配置或养成字段。
        if (query && query->kind == "equipment" && field == "config_id") return false;
        return !query || owned_query_has_field(*query, field);
    };
    const int top = api.get_top(state);
    OwnedShip candidate{
        .ship_id = ship_id,
        .config_id = 0,
        .level = 0,
        .experience_in_level = 0,
        .intimacy_raw = 0,
        .energy = 0,
        .proficiency = 0,
        .fleet_memberships = {},
        .skills = {},
        .slots = {},
    };
    std::uint64_t level = 0;
    std::string field_error;
    if ((selected("config_id") && !read_required_ship_number(
            api, state, object_index, "configId", true, &candidate.config_id, &field_error)) ||
        (selected("level") && !read_required_ship_number(
            api, state, object_index, "level", true, &level, &field_error)) ||
        (selected("experience_in_level") && !read_required_ship_number(
            api, state, object_index, "exp", false, &candidate.experience_in_level, &field_error)) ||
        (selected("intimacy_raw") && !read_required_ship_number(
            api, state, object_index, "intimacy", false, &candidate.intimacy_raw, &field_error)) ||
        (selected("energy") && !read_required_ship_number(
            api, state, object_index, "energy", false, &candidate.energy, &field_error)) ||
        (selected("proficiency") && !read_required_ship_number(
            api, state, object_index, "proficiency", false, &candidate.proficiency, &field_error))) {
        add_ship_error(
            snapshot,
            ship_id,
            std::nullopt,
            "ship_growth_invalid",
            std::move(field_error));
        api.set_top(state, top);
        return false;
    }
    if (level > std::numeric_limits<std::uint32_t>::max()) {
        add_ship_error(
            snapshot,
            ship_id,
            std::nullopt,
            "ship_level_invalid",
            "舰船 level 超出 u32");
        api.set_top(state, top);
        return false;
    }
    candidate.level = static_cast<std::uint32_t>(level);
    candidate.skills.reserve(4);
    if (selected("skills") && !read_ship_skills(
            api,
            state,
            object_index,
            ship_id,
            &candidate.skills,
            snapshot)) {
        api.set_top(state, top);
        return false;
    }

    if (!selected("slots")) {
        *ship = std::move(candidate);
        return true;
    }

    if (api.get_field_protected(state, object_index, "equipments") != 0) {
        add_ship_error(
            snapshot,
            ship_id,
            std::nullopt,
            "ship_equipments_lookup_failed",
            field_failure_detail(api, state, "读取舰船 equipments 字段失败"));
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        add_ship_error(
            snapshot,
            ship_id,
            std::nullopt,
            "ship_equipments_invalid",
            "舰船 equipments 字段不是 table");
        return false;
    }
    const int equipments_index = api.get_top(state);

    candidate.slots.reserve(kShipEquipmentSlotCount);
    for (std::uint32_t slot_index = 1; slot_index <= kShipEquipmentSlotCount; ++slot_index) {
        api.push_number(state, static_cast<double>(slot_index));
        api.raw_get(state, equipments_index);
        ShipEquipmentSlot slot{
            .slot_index = slot_index,
            .equipment = std::nullopt,
        };
        const int equipment_type = api.type(state, -1);
        const bool empty_slot = equipment_type == kLuaTypeNil ||
                                (equipment_type == kLuaTypeBoolean &&
                                 api.to_boolean(state, -1) == 0);
        if (!empty_slot) {
            if (equipment_type == kLuaTypeBoolean) {
                api.set_top(state, top);
                add_ship_error(
                    snapshot,
                    ship_id,
                    slot_index,
                    "ship_equipment_boolean_invalid",
                    "装备槽布尔哨兵必须为 false");
                return false;
            }
            EquipmentSnapshot equipment;
            std::string equipment_error;
            const bool equipment_query = query && query->kind == "equipment";
            if (!read_equipment_snapshot(api, state, -1, &equipment, &equipment_error,
                                         !equipment_query)) {
                api.set_top(state, top);
                add_ship_error(
                    snapshot,
                    ship_id,
                    slot_index,
                    "ship_equipment_invalid",
                    std::move(equipment_error));
                return false;
            }
            const bool requested_equipment = !equipment_query || query->ids.empty() ||
                std::find(query->ids.begin(), query->ids.end(), equipment.config_id) != query->ids.end();
            if (equipment_query && requested_equipment && selected("enhance_level") &&
                !read_equipment_snapshot(api, state, -1, &equipment, &equipment_error)) {
                api.set_top(state, top);
                add_ship_error(snapshot, ship_id, slot_index, "ship_equipment_invalid", std::move(equipment_error));
                return false;
            }
            if (requested_equipment) slot.equipment = equipment;
        }
        candidate.slots.push_back(std::move(slot));
        api.set_top(state, equipments_index);
    }
    api.set_top(state, top);
    *ship = std::move(candidate);
    return true;
}

/// 读取 BayProxy.data，并按舰船实例 ID 排序形成确定性船坞快照。
bool read_dock(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    DockSnapshot* snapshot,
    AgentError* error) {
    ShipFleetMembershipIndex fleet_memberships;
    if (!read_persistent_ship_fleet_memberships(
            api, state, &fleet_memberships, error)) {
        return false;
    }

    BayShipVisitResult visit_result;
    if (!visit_bay_ships(
            api,
            state,
            max_ships,
            [&](int object_index, std::uint64_t ship_id) {
                OwnedShip ship;
                const bool read_ok = read_ship(api, state, object_index, ship_id, &ship, snapshot);
                if (auto memberships = fleet_memberships.find(ship_id);
                    memberships != fleet_memberships.end()) {
                    if (read_ok) {
                        ship.fleet_memberships = std::move(memberships->second);
                    }
                    fleet_memberships.erase(memberships);
                }
                if (read_ok) {
                    snapshot->ships.push_back(std::move(ship));
                }
            },
            &visit_result,
            error)) {
        return false;
    }
    snapshot->truncated = visit_result.truncated;
    for (BayShipVisitError& visit_error : visit_result.read_errors) {
        add_ship_error(
            snapshot,
            visit_error.ship_id,
            std::nullopt,
            std::move(visit_error.code),
            std::move(visit_error.message));
    }
    if (!visit_result.truncated) {
        for (const auto& [ship_id, memberships] : fleet_memberships) {
            static_cast<void>(memberships);
            add_ship_error(
                snapshot,
                ship_id,
                std::nullopt,
                "fleet_ship_missing",
                "持久编队引用的舰船不在 BayProxy.data 中");
        }
    }

    std::sort(
        snapshot->ships.begin(),
        snapshot->ships.end(),
        [](const OwnedShip& left, const OwnedShip& right) { return left.ship_id < right.ship_id; });
    snapshot->complete = !snapshot->truncated && snapshot->read_errors.empty();
    return true;
}

/// 追加单个装备仓库条目的读取错误。
void add_equipment_error(
    WarehouseSnapshot* snapshot,
    std::optional<std::uint64_t> equipment_id,
    std::string code,
    std::string message) {
    snapshot->read_errors.push_back(EquipmentReadError{
        .equipment_id = equipment_id,
        .code = std::move(code),
        .message = std::move(message),
    });
}

/// 读取 EquipmentProxy 的聚合装备表和服务自身报告的已用容量。
bool read_warehouse(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_equipments,
    WarehouseSnapshot* snapshot,
    std::uint64_t* equipment_capacity,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    if (!push_lua_proxy(
            api,
            state,
            "EquipmentProxy",
            "lua_equipment_proxy_invalid",
            error)) {
        return false;
    }
    const int proxy_index = api.get_top(state);
    std::string method_error;
    const std::optional<std::uint64_t> reported_capacity = read_lua_number_method(
        api, state, proxy_index, "getCapacity", {}, &method_error);
    if (!reported_capacity.has_value()) {
        *error = make_lua_error("lua_equipment_capacity_invalid", std::move(method_error));
        return false;
    }

    if (api.get_field_protected(state, proxy_index, "data") != 0) {
        *error = make_lua_error(
            "lua_equipment_data_lookup_failed",
            field_failure_detail(api, state, "读取 EquipmentProxy.data 失败"));
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = make_lua_error(
            "lua_equipment_data_invalid",
            "EquipmentProxy.data 尚未成为 Lua table",
            "same_request");
        return false;
    }
    const int data_index = api.get_top(state);
    if (api.get_field_protected(state, data_index, "equipments") != 0) {
        *error = make_lua_error(
            "lua_equipments_lookup_failed",
            field_failure_detail(api, state, "读取 EquipmentProxy.data.equipments 失败"));
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = make_lua_error(
            "lua_equipments_invalid",
            "EquipmentProxy.data.equipments 尚未成为 Lua table",
            "same_request");
        return false;
    }
    const int equipments_index = api.get_top(state);

    std::unordered_set<std::uint64_t> equipment_ids;
    equipment_ids.reserve(max_equipments);
    std::uint64_t computed_capacity = 0;
    bool capacity_overflow = false;
    std::uint32_t visited = 0;
    api.push_nil(state);
    while (api.next(state, equipments_index) != 0) {
        if (visited >= max_equipments) {
            snapshot->truncated = true;
            break;
        }
        ++visited;
        const int object_index = api.get_top(state);
        const LuaNumber key_id = read_lua_number(api, state, -2);
        EquipmentSnapshot equipment;
        std::string equipment_error;
        if (!read_equipment_snapshot(
                api,
                state,
                object_index,
                &equipment,
                &equipment_error)) {
            const std::optional<std::uint64_t> known_id =
                key_id.status == LuaNumberStatus::Present && key_id.value > 0
                    ? std::optional<std::uint64_t>(key_id.value)
                    : std::nullopt;
            add_equipment_error(
                snapshot,
                known_id,
                "warehouse_equipment_invalid",
                std::move(equipment_error));
        } else if (key_id.status != LuaNumberStatus::Present || key_id.value == 0) {
            add_equipment_error(
                snapshot,
                equipment.equipment_id,
                "warehouse_key_invalid",
                "装备仓库键不是正整数");
        } else if (key_id.value != equipment.equipment_id) {
            add_equipment_error(
                snapshot,
                equipment.equipment_id,
                "warehouse_equipment_id_mismatch",
                "装备仓库键与装备 id 不一致");
        } else if (!equipment_ids.insert(equipment.equipment_id).second) {
            add_equipment_error(
                snapshot,
                equipment.equipment_id,
                "duplicate_equipment_id",
                "装备仓库包含重复 equipment_id");
        } else {
            const LuaNumber quantity =
                read_lua_number_field(api, state, object_index, "count");
            if (quantity.status != LuaNumberStatus::Present || quantity.value == 0) {
                add_equipment_error(
                    snapshot,
                    equipment.equipment_id,
                    "warehouse_quantity_invalid",
                    "装备仓库 count 缺失或不是正整数");
            } else {
                if (computed_capacity > std::numeric_limits<std::uint64_t>::max() - quantity.value) {
                    capacity_overflow = true;
                } else {
                    computed_capacity += quantity.value;
                }
                snapshot->items.push_back(WarehouseEquipment{
                    .equipment = equipment,
                    .quantity = quantity.value,
                });
            }
        }
        api.set_top(state, object_index - 1);
    }

    std::sort(
        snapshot->items.begin(),
        snapshot->items.end(),
        [](const WarehouseEquipment& left, const WarehouseEquipment& right) {
            if (left.equipment.equipment_id != right.equipment.equipment_id) {
                return left.equipment.equipment_id < right.equipment.equipment_id;
            }
            return left.equipment.config_id < right.equipment.config_id;
        });
    if (capacity_overflow) {
        add_equipment_error(
            snapshot,
            std::nullopt,
            "equipment_capacity_mismatch",
            "装备仓库数量求和溢出");
    } else if (!snapshot->truncated && snapshot->read_errors.empty() &&
               computed_capacity != *reported_capacity) {
        // 只有完整枚举得到的数量才能和代理报告的全仓库容量直接比较。
        add_equipment_error(
            snapshot,
            std::nullopt,
            "equipment_capacity_mismatch",
            "装备条目数量之和与 EquipmentProxy.getCapacity() 不一致");
    }
    snapshot->complete = !snapshot->truncated && snapshot->read_errors.empty();
    *equipment_capacity = *reported_capacity;
    return true;
}

/// 读取 PlayerProxy 当前物资和装备仓库上限。
bool read_player_resources(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t equipment_capacity,
    PlayerResources* player,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    if (!push_lua_proxy(api, state, "PlayerProxy", "lua_player_proxy_invalid", error)) {
        return false;
    }
    const int proxy_index = api.get_top(state);
    std::string method_error;
    if (!push_lua_method_result(api, state, proxy_index, "getData", {}, &method_error)) {
        *error = make_lua_error("lua_player_data_failed", std::move(method_error));
        return false;
    }
    const int data_type = api.type(state, -1);
    if (data_type != kLuaTypeTable && data_type != kLuaTypeUserData) {
        *error = make_lua_error(
            "lua_player_data_invalid",
            "PlayerProxy.getData() 尚未返回 table 或 userdata",
            "same_request");
        return false;
    }
    const int data_index = api.get_top(state);
    const LuaNumber gold = read_lua_number_field(api, state, data_index, "gold");
    if (gold.status != LuaNumberStatus::Present) {
        *error = make_lua_error("lua_player_gold_invalid", "PlayerProxy.data.gold 不是非负整数");
        return false;
    }
    const std::optional<std::uint64_t> equipment_limit = read_lua_number_method(
        api, state, data_index, "getMaxEquipmentBag", {}, &method_error);
    if (!equipment_limit.has_value()) {
        *error = make_lua_error("lua_equipment_limit_invalid", std::move(method_error));
        return false;
    }

    *player = PlayerResources{
        .gold = gold.value,
        .equipment_capacity = equipment_capacity,
        .equipment_limit = *equipment_limit,
    };
    return true;
}

}  // namespace

ResourcesExecution snapshot_resources(const LuaApi& api, lua_State* state) noexcept {
    ResourcesExecution execution;
    if (!api.ready() || state == nullptr) {
        execution.error = make_lua_error("lua_resources_arguments_invalid", "Lua API 或状态无效");
        return execution;
    }
    try {
        LuaStackGuard stack(api, state);
        if (!push_lua_proxy(api, state, "EquipmentProxy", "lua_equipment_proxy_invalid", &execution.error)) return execution;
        std::string detail;
        const auto capacity = read_lua_number_method(api, state, api.get_top(state), "getCapacity", {}, &detail);
        if (!capacity.has_value()) {
            execution.error = make_lua_error("lua_equipment_capacity_invalid", std::move(detail));
            return execution;
        }
        execution.success = read_player_resources(api, state, *capacity, &execution.player, &execution.error);
    } catch (const std::exception& error) {
        execution.error = make_lua_error("lua_resources_exception", error.what());
    } catch (...) {
        execution.error = make_lua_error("lua_resources_exception", "玩家资源读取发生未知异常");
    }
    return execution;
}

OwnedStateExecution snapshot_owned_state(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    std::uint32_t max_equipments,
    std::uint32_t max_items) noexcept {
    OwnedStateExecution execution;
    if (!api.ready() || state == nullptr || max_ships == 0 ||
        max_ships > kMaximumSnapshotItems || max_equipments == 0 ||
        max_equipments > kMaximumSnapshotItems || max_items == 0 ||
        max_items > kMaximumSnapshotItems) {
        execution.error = make_lua_error(
            "lua_owned_state_arguments_invalid",
            "Lua API、状态或运行态条目上限无效");
        return execution;
    }

    try {
        LuaStackGuard stack(api, state);
        if (!read_dock(
                api,
                state,
                max_ships,
                &execution.snapshot.dock,
                &execution.error)) {
            return execution;
        }

        std::uint64_t equipment_capacity = 0;
        if (!read_warehouse(
                api,
                state,
                max_equipments,
                &execution.snapshot.warehouse,
                &equipment_capacity,
                &execution.error)) {
            return execution;
        }
        if (!read_player_resources(
                api,
                state,
                equipment_capacity,
                &execution.snapshot.player,
                &execution.error)) {
            return execution;
        }

        SnapshotExecution bag_execution = snapshot_bag(api, state, max_items);
        if (!bag_execution.success) {
            execution.error = std::move(bag_execution.error);
            return execution;
        }
        execution.snapshot.bag = std::move(bag_execution.snapshot);
        execution.snapshot.complete = execution.snapshot.dock.complete &&
                                      execution.snapshot.warehouse.complete &&
                                      execution.snapshot.bag.complete;
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = make_lua_error("lua_owned_state_exception", exception.what());
        return execution;
    } catch (...) {
        execution.error = make_lua_error(
            "lua_owned_state_exception",
            "完整运行态快照发生未预期的本地异常");
        return execution;
    }
}

namespace {

void add_detail_read_error(
    ShipDetailsSnapshot* snapshot,
    std::optional<std::uint64_t> ship_id,
    std::optional<std::uint64_t> skill_id,
    std::string code,
    std::string message) {
    snapshot->read_errors.push_back(ShipDetailReadError{
        .ship_id = ship_id,
        .skill_id = skill_id,
        .code = std::move(code),
        .message = std::move(message),
    });
}

bool read_account_dock_page(
    const LuaApi& api,
    lua_State* state,
    AccountBeforeProgress* progress,
    AgentError* error) {
    BayShipVisitResult visit_result;
    const std::uint64_t resume_before = progress->dock_resume_key;
    const std::uint32_t remaining = progress->max_ships > progress->dock_cursor
                                        ? progress->max_ships - progress->dock_cursor
                                        : 0;
    if (remaining == 0) {
        progress->owned.dock.truncated = true;
        progress->details.truncated = true;
        visit_result.more = false;
    } else if (!visit_bay_ships(
            api,
            state,
            remaining,
            [&](int object_index, std::uint64_t ship_id) {
                const bool duplicate = !progress->seen_ship_ids.insert(ship_id).second;
                if (duplicate) {
                    add_ship_error(
                        &progress->owned.dock,
                        ship_id,
                        std::nullopt,
                        "duplicate_ship_id",
                        "船坞数据包含重复 ship_id");
                    add_detail_read_error(
                        &progress->details,
                        ship_id,
                        std::nullopt,
                        "duplicate_ship_id",
                        "船坞数据包含重复 ship_id");
                    return;
                }
                OwnedShip ship;
                if (read_ship(api, state, object_index, ship_id, &ship, &progress->owned.dock)) {
                    progress->owned.dock.ships.push_back(std::move(ship));
                }
                ShipDetail detail;
                ShipSkillVisitError detail_error;
                if (read_ship_detail(
                        api, state, object_index, ship_id, &detail, &detail_error)) {
                    progress->details.ships.push_back(std::move(detail));
                } else {
                    add_detail_read_error(
                        &progress->details,
                        ship_id,
                        detail_error.skill_id,
                        std::move(detail_error.code),
                        std::move(detail_error.message));
                }
            },
            &visit_result,
            error,
            0,
            kMaximumDockPageSize,
            progress->dock_resume_key)) {
        return false;
    }
    if (visit_result.more &&
        (!visit_result.last_key.has_value() || *visit_result.last_key == resume_before)) {
        visit_result.more = false;
        progress->owned.dock.truncated = true;
        progress->details.truncated = true;
    }
    progress->dock_cursor += visit_result.consumed;
    if (visit_result.last_key.has_value()) {
        progress->dock_resume_key = *visit_result.last_key;
    }
    progress->dock_frames += 1;
    progress->owned.dock.truncated = progress->owned.dock.truncated || visit_result.truncated;
    progress->details.truncated = progress->details.truncated || visit_result.truncated;
    for (BayShipVisitError& visit_error : visit_result.read_errors) {
        add_ship_error(
            &progress->owned.dock,
            visit_error.ship_id,
            std::nullopt,
            visit_error.code,
            visit_error.message);
        add_detail_read_error(
            &progress->details,
            visit_error.ship_id,
            std::nullopt,
            std::move(visit_error.code),
            std::move(visit_error.message));
    }
    if (visit_result.more && visit_result.consumed == 0) {
        visit_result.more = false;
        progress->owned.dock.truncated = true;
        progress->details.truncated = true;
    }
    if (visit_result.more) {
        return true;
    }

    ShipFleetMembershipIndex fleet_memberships;
    if (!read_persistent_ship_fleet_memberships(api, state, &fleet_memberships, error)) {
        return false;
    }
    for (OwnedShip& ship : progress->owned.dock.ships) {
        if (auto memberships = fleet_memberships.find(ship.ship_id);
            memberships != fleet_memberships.end()) {
            ship.fleet_memberships = std::move(memberships->second);
            fleet_memberships.erase(memberships);
        }
    }
    if (!progress->owned.dock.truncated) {
        for (const auto& [ship_id, memberships] : fleet_memberships) {
            static_cast<void>(memberships);
            add_ship_error(
                &progress->owned.dock,
                ship_id,
                std::nullopt,
                "fleet_ship_missing",
                "持久编队引用的舰船不在 BayProxy.data 中");
        }
    }
    std::sort(
        progress->owned.dock.ships.begin(),
        progress->owned.dock.ships.end(),
        [](const OwnedShip& left, const OwnedShip& right) { return left.ship_id < right.ship_id; });
    std::sort(
        progress->details.ships.begin(),
        progress->details.ships.end(),
        [](const ShipDetail& left, const ShipDetail& right) {
            return left.ship_id < right.ship_id;
        });
    progress->owned.dock.complete =
        !progress->owned.dock.truncated && progress->owned.dock.read_errors.empty();
    progress->details.complete =
        !progress->details.truncated && progress->details.read_errors.empty();
    progress->phase = AccountBeforePhase::Warehouse;
    return true;
}

}  // namespace

AccountBeforeStep advance_account_before(
    const LuaApi& api,
    lua_State* state,
    AccountBeforeProgress* progress,
    AccountBeforeExecution* execution) noexcept {
    auto fail = [&](AgentError agent_error) {
        execution->error = std::move(agent_error);
        execution->dock_frames = progress->dock_frames;
        return AccountBeforeStep::Finished;
    };
    if (!api.ready() || state == nullptr || progress->max_ships == 0 ||
        progress->max_ships > kMaximumSnapshotItems || progress->max_equipments == 0 ||
        progress->max_equipments > kMaximumSnapshotItems || progress->max_items == 0 ||
        progress->max_items > kMaximumSnapshotItems) {
        return fail(make_lua_error(
            "lua_owned_state_arguments_invalid",
            "Lua API、状态或运行态条目上限无效"));
    }
    try {
        LuaStackGuard stack(api, state);
        if (progress->details.source.module_sha256.empty()) {
            progress->details.source.module_sha256 = progress->module_sha256;
        }
        if (progress->phase == AccountBeforePhase::Dock) {
            if (!read_account_dock_page(api, state, progress, &execution->error)) {
                execution->dock_frames = progress->dock_frames;
                return AccountBeforeStep::Finished;
            }
            return AccountBeforeStep::Continue;
        }
        if (progress->phase == AccountBeforePhase::Warehouse) {
            std::uint64_t equipment_capacity = 0;
            if (!read_warehouse(
                    api,
                    state,
                    progress->max_equipments,
                    &progress->owned.warehouse,
                    &equipment_capacity,
                    &execution->error) ||
                !read_player_resources(
                    api,
                    state,
                    equipment_capacity,
                    &progress->owned.player,
                    &execution->error)) {
                execution->dock_frames = progress->dock_frames;
                return AccountBeforeStep::Finished;
            }
            progress->phase = AccountBeforePhase::Bag;
            return AccountBeforeStep::Continue;
        }
        SnapshotExecution bag_execution = snapshot_bag(api, state, progress->max_items);
        if (!bag_execution.success) {
            return fail(std::move(bag_execution.error));
        }
        progress->owned.bag = std::move(bag_execution.snapshot);
        progress->owned.complete = progress->owned.dock.complete &&
                                   progress->owned.warehouse.complete &&
                                   progress->owned.bag.complete;
        execution->success = true;
        execution->owned = std::move(progress->owned);
        execution->details = std::move(progress->details);
        execution->dock_frames = progress->dock_frames;
        return AccountBeforeStep::Finished;
    } catch (const std::exception& exception) {
        return fail(make_lua_error("lua_owned_state_exception", exception.what()));
    } catch (...) {
        return fail(make_lua_error(
            "lua_owned_state_exception",
            "账号前窗口快照发生未预期的本地异常"));
    }
}



bool advance_owned_query(const LuaApi& api, lua_State* state,
                         OwnedQueryProgress* progress, OwnedQueryExecution* execution) noexcept {
    auto fail = [&](std::string code, std::string message) {
        execution->error = make_lua_error(std::move(code), std::move(message));
        return true;
    };
    if (!api.ready() || !state) return fail("owned_query_arguments_invalid", "Lua API 或状态无效");
    try {
        LuaStackGuard stack(api, state);
        const OwnedQuery& query = progress->query;
        execution->query = query;
        auto wanted = [&](std::uint64_t id) {
            return query.ids.empty() || std::find(query.ids.begin(), query.ids.end(), id) != query.ids.end();
        };
        auto equipment_entry = [&](const EquipmentSnapshot& equipment) -> OwnedQueryEquipment* {
            auto found = std::find_if(execution->equipment.begin(), execution->equipment.end(),
                [&](const auto& item) { return item.equipment.config_id == equipment.config_id; });
            if (found != execution->equipment.end()) return &*found;
            if (execution->equipment.size() >= kMaximumSnapshotItems) return nullptr;
            execution->equipment.push_back(OwnedQueryEquipment{.equipment = equipment, .warehouse_quantity = 0, .equipped = {}});
            return &execution->equipment.back();
        };
        // 仓库不读取容量和玩家资源；逐页保留原始键，避免每帧从头扫描。
        if (query.kind == "equipment" && !progress->warehouse_finished) {
            if (!push_lua_proxy(api, state, "EquipmentProxy", "lua_equipment_proxy_invalid", &execution->error)) return true;
            if (api.get_field_protected(state, -1, "data") != 0 || api.type(state, -1) != kLuaTypeTable ||
                api.get_field_protected(state, -1, "equipments") != 0 || api.type(state, -1) != kLuaTypeTable) {
                return fail("lua_equipments_invalid", lua_failure_detail(api, state, "读取装备仓库失败"));
            }
            const int table = api.get_top(state);
            if (progress->resume_key) {
                api.push_number(state, static_cast<double>(progress->resume_key));
                api.push_value(state, -1); api.raw_get(state, table);
                if (api.type(state, -1) == kLuaTypeNil) return fail("query_anchor_missing", "装备查询续办键已消失");
                api.set_top(state, api.get_top(state) - 1);
            } else api.push_nil(state);
            std::uint32_t consumed = 0;
            while (api.next(state, table) != 0) {
                if (progress->cursor >= kMaximumSnapshotItems) return fail("owned_query_limit", "装备仓库超过查询容量");
                if (consumed >= kMaximumDockPageSize) return false;
                ++consumed; ++progress->cursor;
                const int object = api.get_top(state);
                const LuaNumber key = read_lua_number(api, state, -2);
                const LuaNumber id = read_lua_number_field(api, state, object, "id");
                const LuaNumber config = read_lua_number_field(api, state, object, "configId");
                if (key.status != LuaNumberStatus::Present || key.value == 0 ||
                    id.status != LuaNumberStatus::Present || id.value != key.value ||
                    config.status == LuaNumberStatus::Invalid ||
                    (config.status == LuaNumberStatus::Present && config.value == 0)) {
                    return fail("warehouse_identity_invalid", "装备仓库身份字段无效或与键不符");
                }
                progress->resume_key = key.value;
                const std::uint64_t config_id = config.status == LuaNumberStatus::Present ? config.value : id.value;
                if (wanted(config_id)) {
                    EquipmentSnapshot equipment{.equipment_id = id.value, .config_id = config_id, .enhance_level = 0};
                    std::string reason;
                    if (owned_query_has_field(query, "enhance_level") &&
                        !read_equipment_snapshot(api, state, object, &equipment, &reason)) return fail("equipment_invalid", reason);
                    auto* item = equipment_entry(equipment);
                    if (!item) return fail("owned_query_limit", "持有装备超过查询容量");
                    if (owned_query_has_field(query, "warehouse_quantity")) {
                        const LuaNumber quantity = read_lua_number_field(api, state, object, "count");
                        if (quantity.status != LuaNumberStatus::Present || quantity.value == 0 ||
                            item->warehouse_quantity > std::numeric_limits<std::uint64_t>::max() - quantity.value)
                            return fail("warehouse_quantity_invalid", "仓库装备数量无效或求和溢出");
                        item->warehouse_quantity += quantity.value;
                    }
                }
                api.set_top(state, object - 1);
            }
            progress->warehouse_finished = true;
            progress->cursor = 0;
            progress->resume_key = 0;
            return false;
        }
        ShipFleetMembershipIndex memberships;
        if (query.kind == "ships" && owned_query_has_field(query, "fleet_memberships") &&
            !read_persistent_ship_fleet_memberships(api, state, &memberships, &execution->error)) return true;
        std::vector<std::uint64_t> selected;
        const bool direct = query.kind == "ships" && !query.ids.empty();
        if (direct) {
            const auto end = std::min<std::size_t>(query.ids.size(), progress->cursor + kMaximumDockPageSize);
            selected.assign(query.ids.begin() + progress->cursor, query.ids.begin() + end);
        }
        bool failed = false;
        BayShipVisitResult visited;
        if (!visit_bay_ships(api, state, kMaximumSnapshotItems - progress->cursor,
            [&](int object, std::uint64_t id) {
                if (failed) return;
                if (!progress->seen_ids.insert(id).second) {
                    fail("query_ship_repeated", "跨帧查询遇到重复舰船实例"); failed = true; return;
                }
                DockSnapshot diagnostics;
                OwnedQueryShip entry;
                // 装备存在性也包含已装载装备；只请求槽位，不读取舰船养成、技能和详情。
                OwnedQuery slots_query{.kind = "equipment", .ids = query.ids, .fields = {"slots"}};
                if (owned_query_has_field(query, "enhance_level")) slots_query.fields.push_back("enhance_level");
                if (!read_ship(api, state, object, id, &entry.ship, &diagnostics,
                               query.kind == "ships" ? &query : &slots_query)) {
                    const auto& error = diagnostics.read_errors.front();
                    fail(error.code, "ship_id=" + std::to_string(id) + ": " + error.message); failed = true; return;
                }
                if (query.kind == "equipment") {
                    for (const auto& slot : entry.ship.slots) {
                        if (!slot.equipment || !wanted(slot.equipment->config_id)) continue;
                        auto* item = equipment_entry(*slot.equipment);
                        if (!item) { fail("owned_query_limit", "持有装备超过查询容量"); failed = true; return; }
                        if (owned_query_has_field(query, "equipped")) item->equipped.push_back({id, slot.slot_index, slot.equipment->equipment_id});
                    }
                    return;
                }
                if (owned_query_has_field(query, "name")) {
                    std::string reason;
                    auto name = read_lua_text_field(api, state, object, "name", 512, false, &reason);
                    if (!name) { fail("ship_name_invalid", reason); failed = true; return; }
                    entry.name = std::move(*name);
                }
                if (owned_query_has_field(query, "fleet_memberships")) {
                    if (auto found = memberships.find(id); found != memberships.end()) entry.ship.fleet_memberships = found->second;
                }
                if (owned_query_has_field(query, "details")) {
                    ShipDetail detail;
                    ShipSkillVisitError error;
                    if (!read_ship_detail(api, state, object, id, &detail, &error)) {
                        fail(error.code, "ship_id=" + std::to_string(id) + ": " + error.message); failed = true; return;
                    }
                    entry.details = std::move(detail);
                }
                execution->ships.push_back(std::move(entry));
            }, &visited, &execution->error, 0, kMaximumDockPageSize, progress->resume_key,
            direct ? &selected : nullptr)) return true;
        if (failed) return true;
        if (!visited.read_errors.empty()) return fail(visited.read_errors.front().code, visited.read_errors.front().message);
        if (visited.truncated) return fail("owned_query_limit", "船坞超过查询容量");
        progress->cursor += direct ? static_cast<std::uint32_t>(selected.size()) : visited.consumed;
        if (visited.last_key) progress->resume_key = *visited.last_key;
        if (direct ? progress->cursor < query.ids.size() : visited.more) return false;
        for (const auto id : query.ids) {
            const bool found = query.kind == "ships"
                ? std::any_of(execution->ships.begin(), execution->ships.end(), [&](const auto& item) { return item.ship.ship_id == id; })
                : std::any_of(execution->equipment.begin(), execution->equipment.end(), [&](const auto& item) { return item.equipment.config_id == id; });
            if (!found) execution->missing_ids.push_back(id);
        }
        std::sort(execution->ships.begin(), execution->ships.end(), [](const auto& a, const auto& b) { return a.ship.ship_id < b.ship.ship_id; });
        std::sort(execution->equipment.begin(), execution->equipment.end(), [](const auto& a, const auto& b) { return a.equipment.config_id < b.equipment.config_id; });
        execution->success = true;
        return true;
    } catch (const std::exception& error) {
        return fail("owned_query_exception", error.what());
    } catch (...) {
        return fail("owned_query_exception", "持有对象查询发生未预期的本地异常");
    }
}

}  // namespace azlw::agent

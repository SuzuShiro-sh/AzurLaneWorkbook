// 在游戏主线程读取装备命令需要的槽位、仓库、背包和容量。

#include "equipment_command_detail.h"

#include <array>
#include <exception>
#include <optional>
#include <span>
#include <string>
#include <utility>

#include "lua/lua_reader.h"
#include "snapshots/owned_state_snapshot.h"

namespace azlw::agent {
namespace {

/// 调用返回可空装备对象的方法，并严格解释 false/nil 空槽哨兵。
bool read_optional_equipment_method(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    std::optional<EquipmentSnapshot>* equipment,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_method_result(
            api,
            state,
            object_index,
            method_name,
            arguments,
            error)) {
        return false;
    }
    const int value_type = api.type(state, -1);
    if (value_type == kLuaTypeNil ||
        (value_type == kLuaTypeBoolean && api.to_boolean(state, -1) == 0)) {
        equipment->reset();
        api.set_top(state, top);
        return true;
    }
    if (value_type == kLuaTypeBoolean) {
        *error = std::string(method_name) + " 返回了无效的 true 装备哨兵";
        api.set_top(state, top);
        return false;
    }
    EquipmentSnapshot snapshot;
    if (!read_equipment_snapshot(api, state, -1, &snapshot, error)) {
        api.set_top(state, top);
        return false;
    }
    *equipment = snapshot;
    api.set_top(state, top);
    return true;
}

/// 按聚合装备 ID 读取仓库对象；数量为零时对象必须已经从表中移除。
bool read_warehouse_entry(
    const LuaApi& api,
    lua_State* state,
    int proxy_index,
    std::uint64_t equipment_id,
    EquipmentWarehouseEntry* entry,
    std::string* error) {
    const int top = api.get_top(state);
    const std::array arguments{
        LuaCallArgument::number(static_cast<double>(equipment_id)),
    };
    if (!push_lua_method_result(
            api,
            state,
            proxy_index,
            "getEquipmentById",
            arguments,
            error)) {
        return false;
    }
    if (api.type(state, -1) == kLuaTypeNil) {
        *entry = EquipmentWarehouseEntry{};
        api.set_top(state, top);
        return true;
    }
    EquipmentSnapshot equipment;
    if (!read_equipment_snapshot(api, state, -1, &equipment, error)) {
        api.set_top(state, top);
        return false;
    }
    const LuaNumber quantity = read_lua_number_field(api, state, -1, "count");
    if (quantity.status != LuaNumberStatus::Present || quantity.value == 0) {
        *error = "EquipmentProxy.getEquipmentById 返回的 count 不是正整数";
        api.set_top(state, top);
        return false;
    }
    *entry = EquipmentWarehouseEntry{
        .equipment = equipment,
        .quantity = quantity.value,
    };
    api.set_top(state, top);
    return true;
}

/// 按配置 ID 枚举仓库聚合表；目标配置可能尚不存在对应运行态装备 ID。
bool read_warehouse_entry_by_config(
    const LuaApi& api,
    lua_State* state,
    int proxy_index,
    std::uint64_t config_id,
    EquipmentWarehouseEntry* entry,
    std::string* error) {
    const int top = api.get_top(state);
    if (api.get_field_protected(state, proxy_index, "data") != 0) {
        *error = lua_failure_detail(api, state, "读取 EquipmentProxy.data 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "EquipmentProxy.data 尚未成为表";
        api.set_top(state, top);
        return false;
    }
    const int data_index = api.get_top(state);
    if (api.get_field_protected(state, data_index, "equipments") != 0) {
        *error = lua_failure_detail(api, state, "读取 EquipmentProxy.data.equipments 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "EquipmentProxy.data.equipments 尚未成为表";
        api.set_top(state, top);
        return false;
    }
    const int equipments_index = api.get_top(state);
    EquipmentWarehouseEntry found;
    bool matched = false;
    api.push_nil(state);
    while (api.next(state, equipments_index) != 0) {
        const int object_index = api.get_top(state);
        EquipmentSnapshot equipment;
        std::string equipment_error;
        if (!read_equipment_snapshot(
                api,
                state,
                object_index,
                &equipment,
                &equipment_error)) {
            *error = "读取仓库装备失败: " + equipment_error;
            api.set_top(state, top);
            return false;
        }
        if (equipment.config_id == config_id) {
            if (matched) {
                *error = "EquipmentProxy 中同一配置出现多个聚合对象";
                api.set_top(state, top);
                return false;
            }
            const LuaNumber quantity = read_lua_number_field(api, state, object_index, "count");
            if (quantity.status != LuaNumberStatus::Present || quantity.value == 0) {
                *error = "目标配置仓库对象的 count 不是正整数";
                api.set_top(state, top);
                return false;
            }
            found = EquipmentWarehouseEntry{
                .equipment = equipment,
                .quantity = quantity.value,
            };
            matched = true;
        }
        api.set_top(state, object_index - 1);
    }
    *entry = found;
    api.set_top(state, top);
    return true;
}

/// 只读取当前命令材料的数量，避免无关背包条目影响局部前态。
bool read_bag_item_quantity(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t item_id,
    std::uint64_t* quantity,
    std::string* error) {
    LuaStackGuard stack(api, state);
    AgentError proxy_error;
    if (!push_lua_proxy(api, state, "BagProxy", "lua_bag_proxy_invalid", &proxy_error)) {
        *error = proxy_error.message;
        return false;
    }
    const int proxy_index = api.get_top(state);
    if (api.get_field_protected(state, proxy_index, "data") != 0) {
        *error = lua_failure_detail(api, state, "读取 BagProxy.data 失败");
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, api.get_top(state) - 1);
        if (!push_lua_method_result(api, state, proxy_index, "getRawData", {}, error)) {
            return false;
        }
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = "BagProxy 数据尚未成为表";
        return false;
    }
    const int bag_index = api.get_top(state);
    api.push_number(state, static_cast<double>(item_id));
    api.raw_get(state, bag_index);
    const int value_type = api.type(state, -1);
    if (value_type == kLuaTypeNil) {
        *quantity = 0;
        return true;
    }
    const LuaNumber value =
        value_type == kLuaTypeTable || value_type == kLuaTypeUserData
            ? read_lua_number_field(api, state, -1, "count")
            : read_lua_number(api, state, -1);
    if (value.status != LuaNumberStatus::Present) {
        *error = "BagProxy 中装备命令材料数量不是非负整数";
        return false;
    }
    *quantity = value.value;
    return true;
}

}  // namespace

/// 取得目标舰船对象并留在栈顶。
bool push_target_ship(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t ship_id,
    AgentError* error) {
    if (!push_lua_proxy(api, state, "BayProxy", "lua_bay_proxy_invalid", error)) {
        return false;
    }
    const int proxy_index = api.get_top(state);
    const std::array arguments{
        LuaCallArgument::number(static_cast<double>(ship_id)),
    };
    std::string method_error;
    if (!push_lua_method_result(
            api,
            state,
            proxy_index,
            "getShipById",
            arguments,
            &method_error)) {
        *error = make_lua_error("lua_equipment_command_ship_failed", std::move(method_error));
        return false;
    }
    const int ship_type = api.type(state, -1);
    if (ship_type != kLuaTypeTable && ship_type != kLuaTypeUserData) {
        *error = command_error(
            "equipment_command_ship_missing",
            "设备端前检未找到目标舰船实例");
        return false;
    }
    return true;
}


EquipmentCommandStateRead read_equipment_command_state(
    const LuaApi& api,
    lua_State* state,
    const EquipmentCommand& command) noexcept {
    EquipmentCommandStateRead result;
    if (!api.ready() || state == nullptr) {
        result.error = command_error(
            "equipment_command_arguments_invalid",
            "Lua API、状态或装备命令目标无效");
        return result;
    }
    if (const auto* action = std::get_if<DismantleEquipmentCommandAction>(&command.action);
        action != nullptr && !dismantle_equipment_command_action_is_valid(*action)) {
        result.error = command_error(
            "equipment_command_arguments_invalid",
            "拆解命令的来源、数量、强化等级或仓库容量前置条件无效");
        return result;
    }
    if (const auto* action = std::get_if<ComposeEquipmentCommandAction>(&command.action);
        action != nullptr && !compose_equipment_command_action_is_valid(*action)) {
        result.error = command_error(
            "equipment_command_arguments_invalid",
            "合成命令的配方、资源或仓库容量前置条件无效");
        return result;
    }
    if (const auto* action =
            std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action);
        action != nullptr && !enhance_warehouse_equipment_command_action_is_valid(*action)) {
        result.error = command_error(
            "equipment_command_arguments_invalid",
            "仓库强化命令的来源、目标、资源或容量前置条件无效");
        return result;
    }
    if (const auto* action = std::get_if<EnhanceShipEquipmentCommandAction>(&command.action);
        action != nullptr && !enhance_ship_equipment_command_action_is_valid(*action)) {
        result.error = command_error(
            "equipment_command_arguments_invalid",
            "舰上强化命令的槽位、目标、资源或容量前置条件无效");
        return result;
    }

    try {
        LuaStackGuard stack(api, state);
        std::string method_error;
        const auto read_ship_slot = [&](std::uint64_t ship_id, std::uint32_t slot_index) {
            if (ship_id == 0 || slot_index == 0 || slot_index > kShipEquipmentSlotCount) {
                result.error = command_error(
                    "equipment_command_arguments_invalid",
                    "装备命令的舰船或槽位无效");
                return false;
            }
            if (!push_target_ship(api, state, ship_id, &result.error)) {
                return false;
            }
            const int ship_index = api.get_top(state);
            const std::array slot_arguments{
                LuaCallArgument::number(static_cast<double>(slot_index)),
            };
            if (!read_optional_equipment_method(
                    api,
                    state,
                    ship_index,
                    "getEquip",
                    slot_arguments,
                    &result.state.target_slot,
                    &method_error)) {
                result.error = make_lua_error(
                    "lua_equipment_command_slot_failed",
                    std::move(method_error));
                return false;
            }
            const std::optional<std::uint64_t> equipment_skin_id = read_lua_number_method(
                api,
                state,
                ship_index,
                "getEquipSkin",
                slot_arguments,
                &method_error);
            if (!equipment_skin_id.has_value()) {
                result.error = make_lua_error(
                    "lua_equipment_command_skin_failed",
                    std::move(method_error));
                return false;
            }
            result.state.equipment_skin_id = *equipment_skin_id;
            return true;
        };
        if (const auto* action =
                std::get_if<UnequipEquipmentCommandAction>(&command.action)) {
            if (!read_ship_slot(action->ship_id, action->slot_index)) {
                return result;
            }
        } else if (const auto* action =
                       std::get_if<EquipEquipmentCommandAction>(&command.action)) {
            if (!read_ship_slot(action->ship_id, action->slot_index)) {
                return result;
            }
        } else if (const auto* action =
                       std::get_if<EnhanceShipEquipmentCommandAction>(&command.action)) {
            if (!read_ship_slot(action->ship_id, action->slot_index)) {
                return result;
            }
        }

        if (!push_lua_proxy(
                api,
                state,
                "EquipmentProxy",
                "lua_equipment_proxy_invalid",
                &result.error)) {
            return result;
        }
        const int equipment_proxy_index = api.get_top(state);
        const EquipmentSnapshot* source_before = nullptr;
        const EquipmentSnapshot* target_before = nullptr;
        if (const auto* action = std::get_if<UnequipEquipmentCommandAction>(&command.action)) {
            target_before = &action->target_before;
        } else if (const auto* action =
                       std::get_if<EquipEquipmentCommandAction>(&command.action)) {
            source_before = &action->source_before;
            if (action->target_before.has_value()) {
                target_before = &*action->target_before;
            }
        } else if (const auto* action =
                       std::get_if<DismantleEquipmentCommandAction>(&command.action)) {
            source_before = &action->source_before;
        } else if (const auto* action =
                       std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action)) {
            source_before = &action->source_before;
        }
        if (source_before != nullptr &&
            !read_warehouse_entry(
                api,
                state,
                equipment_proxy_index,
                source_before->equipment_id,
                &result.state.source_warehouse,
                &method_error)) {
            result.error = make_lua_error(
                "lua_equipment_command_source_failed",
                std::move(method_error));
            return result;
        }
        if (const auto* action = std::get_if<ComposeEquipmentCommandAction>(&command.action)) {
            if (!read_warehouse_entry_by_config(
                    api,
                    state,
                    equipment_proxy_index,
                    action->output_config_id,
                    &result.state.compose_output_warehouse,
                    &method_error)) {
                result.error = make_lua_error(
                    "lua_equipment_command_compose_output_failed",
                    std::move(method_error));
                return result;
            }
        } else if (const auto* action =
                       std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action)) {
            if (!read_warehouse_entry_by_config(
                    api,
                    state,
                    equipment_proxy_index,
                    action->target_config_id,
                    &result.state.target_warehouse,
                    &method_error)) {
                result.error = make_lua_error(
                    "lua_equipment_command_target_failed",
                    std::move(method_error));
                return result;
            }
        }
        if (target_before != nullptr &&
            !read_warehouse_entry(
                api,
                state,
                equipment_proxy_index,
                target_before->equipment_id,
                &result.state.target_warehouse,
                &method_error)) {
            result.error = make_lua_error(
                "lua_equipment_command_target_failed",
                std::move(method_error));
            return result;
        }
        const std::optional<std::uint64_t> equipment_capacity = read_lua_number_method(
            api,
            state,
            equipment_proxy_index,
            "getCapacity",
            {},
            &method_error);
        if (!equipment_capacity.has_value()) {
            result.error = make_lua_error(
                "lua_equipment_command_capacity_failed",
                std::move(method_error));
            return result;
        }
        result.state.equipment_capacity = *equipment_capacity;

        if (!push_lua_proxy(
                api,
                state,
                "PlayerProxy",
                "lua_player_proxy_invalid",
                &result.error)) {
            return result;
        }
        const int player_proxy_index = api.get_top(state);
        if (!push_lua_method_result(
                api,
                state,
                player_proxy_index,
                "getData",
                {},
                &method_error)) {
            result.error = make_lua_error(
                "lua_equipment_command_player_failed",
                std::move(method_error));
            return result;
        }
        const int player_type = api.type(state, -1);
        if (player_type != kLuaTypeTable && player_type != kLuaTypeUserData) {
            result.error = make_lua_error(
                "lua_equipment_command_player_invalid",
                "PlayerProxy.getData() 未返回玩家对象");
            return result;
        }
        const int player_index = api.get_top(state);
        if (const auto* action = std::get_if<ComposeEquipmentCommandAction>(&command.action)) {
            const LuaNumber gold = read_lua_number_field(api, state, player_index, "gold");
            if (gold.status != LuaNumberStatus::Present) {
                result.error = make_lua_error(
                    "lua_equipment_command_gold_failed",
                    "PlayerProxy.data.gold 不是非负整数");
                return result;
            }
            result.state.gold = gold.value;
            if (!read_bag_item_quantity(
                    api,
                    state,
                    action->material_id,
                    &result.state.compose_material_quantity,
                    &method_error)) {
                result.error = make_lua_error(
                    "lua_equipment_command_material_failed",
                    std::move(method_error));
                return result;
            }
        } else {
            const std::vector<EquipmentCommandMaterialCost>* materials = nullptr;
            if (const auto* action =
                    std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action)) {
                materials = &action->materials;
            } else if (const auto* action =
                           std::get_if<EnhanceShipEquipmentCommandAction>(&command.action)) {
                materials = &action->materials;
            }
            if (materials != nullptr) {
                const LuaNumber gold = read_lua_number_field(api, state, player_index, "gold");
                if (gold.status != LuaNumberStatus::Present) {
                    result.error = make_lua_error(
                        "lua_equipment_command_gold_failed",
                        "PlayerProxy.data.gold 不是非负整数");
                    return result;
                }
                result.state.gold = gold.value;
                result.state.enhance_materials.reserve(materials->size());
                for (const EquipmentCommandMaterialCost& material : *materials) {
                    std::uint64_t quantity = 0;
                    if (!read_bag_item_quantity(
                            api,
                            state,
                            material.item_id,
                            &quantity,
                            &method_error)) {
                        result.error = make_lua_error(
                            "lua_equipment_command_material_failed",
                            std::move(method_error));
                        return result;
                    }
                    result.state.enhance_materials.push_back(
                        EquipmentCommandMaterialState{
                            .item_id = material.item_id,
                            .quantity = quantity,
                        });
                }
            }
        }
        const std::optional<std::uint64_t> equipment_limit = read_lua_number_method(
            api,
            state,
            player_index,
            "getMaxEquipmentBag",
            {},
            &method_error);
        if (!equipment_limit.has_value()) {
            result.error = make_lua_error(
                "lua_equipment_command_limit_failed",
                std::move(method_error));
            return result;
        }
        result.state.equipment_limit = *equipment_limit;
        result.success = true;
        return result;
    } catch (const std::exception& exception) {
        result.error = command_error(
            "equipment_command_state_exception",
            "读取装备命令局部状态失败: " + std::string(exception.what()));
        return result;
    } catch (...) {
        result.error = command_error(
            "equipment_command_state_exception",
            "读取装备命令局部状态时发生未知本地异常");
        return result;
    }
}
}  // namespace azlw::agent

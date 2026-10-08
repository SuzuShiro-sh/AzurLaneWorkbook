// 实现装备命令的动作前检和官方通知派发。

#include "equipment_command.h"
#include "equipment_command_detail.h"

#include <algorithm>
#include <array>
#include <exception>
#include <optional>
#include <string>
#include <type_traits>
#include <utility>

#include "lua/lua_reader.h"
#include "snapshots/owned_state_snapshot.h"

namespace azlw::agent {
namespace {

constexpr std::uint64_t kDismantleConfirmationRarity = 4;
// 配置 ID 末尾区段是工具附加的保守保护边界，并与 Rust 计划前检保持一致。
constexpr std::uint64_t kProtectedVariantRemainder = 10;
constexpr std::uint64_t kProtectedVariantModulus = 20;


/// 构造通知调用已经开始、设备状态不再确定的诊断。
AgentError uncertain_dispatch_error(std::string message) {
    return AgentError{
        .code = "equipment_command_dispatch_uncertain",
        .stage = "agent.equipment_command",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "state_unknown",
    };
}

/// 在前态完全相等后再次调用客户端兼容性判断，读取其第一个布尔结果。
bool source_can_equip(
    const LuaApi& api,
    lua_State* state,
    const EquipEquipmentCommandAction& action,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    if (!push_target_ship(api, state, action.ship_id, error)) {
        return false;
    }
    const int ship_index = api.get_top(state);
    if (!push_lua_proxy(
            api,
            state,
            "EquipmentProxy",
            "lua_equipment_proxy_invalid",
            error)) {
        return false;
    }
    const int equipment_proxy_index = api.get_top(state);
    const EquipmentSnapshot& source = action.source_before;
    const std::array source_arguments{
        LuaCallArgument::number(static_cast<double>(source.equipment_id)),
    };
    std::string method_error;
    if (!push_lua_method_result(
            api,
            state,
            equipment_proxy_index,
            "getEquipmentById",
            source_arguments,
            &method_error)) {
        *error = make_lua_error("lua_equipment_command_source_failed", std::move(method_error));
        return false;
    }
    const int source_type = api.type(state, -1);
    if (source_type != kLuaTypeTable && source_type != kLuaTypeUserData) {
        *error = command_error(
            "equipment_command_precondition_changed",
            "兼容性检查前仓库来源装备已经消失");
        return false;
    }
    const int source_index = api.get_top(state);
    const std::array compatibility_arguments{
        LuaCallArgument::stack_value(source_index),
        LuaCallArgument::number(static_cast<double>(action.slot_index)),
    };
    const std::optional<bool> compatible = read_lua_boolean_method(
        api,
        state,
        ship_index,
        "canEquipAtPos",
        compatibility_arguments,
        &method_error);
    if (!compatible.has_value()) {
        *error = make_lua_error(
            "lua_equipment_command_compatibility_failed",
            std::move(method_error));
        return false;
    }
    if (!*compatible) {
        *error = command_error(
            "equipment_command_incompatible",
            "目标舰船当前不允许把来源装备放入指定槽位");
        return false;
    }
    return true;
}

/// 在派发前按当前装备对象复核自动拆解安全边界，禁止触发二级密码或确认路径。
bool source_can_dismantle(
    const LuaApi& api,
    lua_State* state,
    const DismantleEquipmentCommandAction& action,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    if (action.source_before.enhance_level != 0) {
        *error = command_error(
            "equipment_command_dismantle_protected",
            "设备端拒绝拆解已强化装备");
        return false;
    }
    if (!push_lua_proxy(
            api,
            state,
            "EquipmentProxy",
            "lua_equipment_proxy_invalid",
            error)) {
        return false;
    }
    const int equipment_proxy_index = api.get_top(state);
    const std::array source_arguments{
        LuaCallArgument::number(static_cast<double>(action.source_before.equipment_id)),
    };
    std::string method_error;
    if (!push_lua_method_result(
            api,
            state,
            equipment_proxy_index,
            "getEquipmentById",
            source_arguments,
            &method_error)) {
        *error = make_lua_error(
            "lua_equipment_command_source_failed",
            std::move(method_error));
        return false;
    }
    const int source_type = api.type(state, -1);
    if (source_type != kLuaTypeTable && source_type != kLuaTypeUserData) {
        *error = command_error(
            "equipment_command_precondition_changed",
            "拆解安全检查前仓库来源装备已经消失");
        return false;
    }
    const int source_index = api.get_top(state);
    const std::optional<bool> important = read_lua_boolean_method(
        api,
        state,
        source_index,
        "isImportance",
        {},
        &method_error);
    if (!important.has_value()) {
        *error = make_lua_error(
            "lua_equipment_command_importance_failed",
            std::move(method_error));
        return false;
    }
    const std::array rarity_arguments{LuaCallArgument::string("rarity")};
    const std::optional<std::uint64_t> rarity = read_lua_number_method(
        api,
        state,
        source_index,
        "getConfig",
        rarity_arguments,
        &method_error);
    if (!rarity.has_value()) {
        *error = make_lua_error(
            "lua_equipment_command_rarity_failed",
            std::move(method_error));
        return false;
    }
    const std::array config_id_arguments{LuaCallArgument::string("id")};
    const std::optional<std::uint64_t> config_id = read_lua_number_method(
        api,
        state,
        source_index,
        "getConfig",
        config_id_arguments,
        &method_error);
    if (!config_id.has_value()) {
        *error = make_lua_error(
            "lua_equipment_command_config_id_failed",
            std::move(method_error));
        return false;
    }
    if (*important || *rarity >= kDismantleConfirmationRarity ||
        *config_id % kProtectedVariantModulus >= kProtectedVariantRemainder) {
        *error = command_error(
            "equipment_command_dismantle_protected",
            "设备端拒绝拆解重要、高稀有度或受保护变体装备");
        return false;
    }
    return true;
}

/// 在派发前复核客户端静态配方，防止宿主前态与当前表内容发生漂移。
bool source_can_compose(
    const LuaApi& api,
    lua_State* state,
    const ComposeEquipmentCommandAction& action,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    if (!compose_equipment_command_action_is_valid(action)) {
        *error = command_error(
            "equipment_command_arguments_invalid",
            "合成命令的配方、资源或容量前置条件无效");
        return false;
    }
    std::string lookup_error;
    if (!push_lua_global_table(api, state, "pg", &lookup_error)) {
        *error = make_lua_error("lua_compose_recipe_lookup_failed", std::move(lookup_error));
        return false;
    }
    const int pg_index = api.get_top(state);
    if (!push_lua_table_field(
            api, state, pg_index, "compose_data_template", &lookup_error)) {
        *error = command_error(
            "equipment_command_compose_recipe_changed",
            "客户端合成配方表尚未就绪");
        return false;
    }
    const int recipes_index = api.get_top(state);
    if (api.get_number_index_protected(
            state, recipes_index, static_cast<double>(action.recipe_id)) != 0) {
        *error = make_lua_error(
            "lua_compose_recipe_lookup_failed",
            lua_failure_detail(api, state, "读取合成配方失败"));
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = command_error(
            "equipment_command_compose_recipe_changed",
            "设备端前检未找到计划绑定的合成配方");
        return false;
    }
    const int recipe_index = api.get_top(state);
    const LuaNumber recipe_id = read_lua_number_field(api, state, recipe_index, "id");
    const LuaNumber material_id = read_lua_number_field(api, state, recipe_index, "material_id");
    const LuaNumber material_count =
        read_lua_number_field(api, state, recipe_index, "material_num");
    const LuaNumber gold = read_lua_number_field(api, state, recipe_index, "gold_num");
    const LuaNumber output_config = read_lua_number_field(api, state, recipe_index, "equip_id");
    if (recipe_id.status != LuaNumberStatus::Present ||
        material_id.status != LuaNumberStatus::Present ||
        material_count.status != LuaNumberStatus::Present ||
        gold.status != LuaNumberStatus::Present ||
        output_config.status != LuaNumberStatus::Present || recipe_id.value != action.recipe_id ||
        material_id.value != action.material_id ||
        material_count.value != action.material_quantity_per_unit ||
        gold.value != action.gold_per_unit || output_config.value != action.output_config_id) {
        *error = command_error(
            "equipment_command_compose_recipe_changed",
            "客户端当前合成配方与命令绑定的材料、物资或产物不一致");
        return false;
    }
    return true;
}

/// 通过客户端构造器创建一个目标配置的 Equipment 对象并留在栈顶。
bool push_new_equipment(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t config_id,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "Equipment", error)) {
        return false;
    }
    const int equipment_table = api.get_top(state);
    api.create_table(state, 0, 1);
    const int constructor_argument = api.get_top(state);
    api.push_number(state, static_cast<double>(config_id));
    api.set_field(state, constructor_argument, "id");
    const std::array arguments{LuaCallArgument::stack_value(constructor_argument)};
    if (!push_lua_table_function_result(
            api,
            state,
            equipment_table,
            "New",
            arguments,
            error)) {
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

/// 取得仓库或舰船槽位中的强化来源装备并留在栈顶。
template <typename Action>
bool push_enhance_source(
    const LuaApi& api,
    lua_State* state,
    const Action& action,
    AgentError* error) {
    std::string method_error;
    if constexpr (std::is_same_v<Action, EnhanceWarehouseEquipmentCommandAction>) {
        if (!push_lua_proxy(
                api,
                state,
                "EquipmentProxy",
                "lua_equipment_proxy_invalid",
                error)) {
            return false;
        }
        const int proxy_index = api.get_top(state);
        const std::array arguments{
            LuaCallArgument::number(static_cast<double>(action.source_before.equipment_id)),
        };
        if (!push_lua_method_result(
                api,
                state,
                proxy_index,
                "getEquipmentById",
                arguments,
                &method_error)) {
            *error = make_lua_error(
                "lua_equipment_command_source_failed",
                std::move(method_error));
            return false;
        }
    } else {
        if (!push_target_ship(api, state, action.ship_id, error)) {
            return false;
        }
        const int ship_index = api.get_top(state);
        const std::array arguments{
            LuaCallArgument::number(static_cast<double>(action.slot_index)),
        };
        if (!push_lua_method_result(
                api,
                state,
                ship_index,
                "getEquip",
                arguments,
                &method_error)) {
            *error = make_lua_error(
                "lua_equipment_command_source_failed",
                std::move(method_error));
            return false;
        }
    }
    const int source_type = api.type(state, -1);
    if (source_type != kLuaTypeTable && source_type != kLuaTypeUserData) {
        *error = command_error(
            "equipment_command_precondition_changed",
            "强化派发前来源装备已经消失");
        return false;
    }
    return true;
}

/// 从当前装备配置读取全部单级材料成本，排序后与宿主冻结的成本比较。
template <typename Action>
bool enhance_material_costs_match(
    const LuaApi& api,
    lua_State* state,
    int source_index,
    const Action& action,
    AgentError* error) {
    const int top = api.get_top(state);
    const std::array arguments{LuaCallArgument::string("trans_use_item")};
    std::string method_error;
    if (!push_lua_method_result(
            api,
            state,
            source_index,
            "getConfig",
            arguments,
            &method_error)) {
        *error = make_lua_error(
            "lua_equipment_command_enhance_cost_failed",
            std::move(method_error));
        return false;
    }
    if (api.type(state, -1) == kLuaTypeNil && action.materials.empty()) {
        api.set_top(state, top);
        return true;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = command_error(
            "equipment_command_enhance_cost_changed",
            "客户端当前强化材料成本不是数组");
        api.set_top(state, top);
        return false;
    }
    const int materials_index = api.get_top(state);
    struct IndexedMaterial final {
        std::uint64_t index;
        std::uint64_t item_id;
        std::uint64_t cost;
    };
    std::vector<IndexedMaterial> indexed;
    api.push_nil(state);
    while (api.next(state, materials_index) != 0) {
        const int value_index = api.get_top(state);
        const LuaNumber array_index = read_lua_number(api, state, value_index - 1);
        if (array_index.status != LuaNumberStatus::Present || array_index.value == 0 ||
            array_index.value > kMaximumEnhanceMaterialCount ||
            indexed.size() >= kMaximumEnhanceMaterialCount ||
            api.type(state, value_index) != kLuaTypeTable ||
            api.object_length(state, value_index) != 2) {
            *error = command_error(
                "equipment_command_enhance_cost_changed",
                "客户端当前强化材料成本必须是最多 64 项的连续二元数组");
            api.set_top(state, top);
            return false;
        }
        const int entry_index = value_index;
        api.push_number(state, 1.0);
        api.raw_get(state, entry_index);
        const LuaNumber item_id = read_lua_number(api, state, -1);
        api.set_top(state, entry_index);
        api.push_number(state, 2.0);
        api.raw_get(state, entry_index);
        const LuaNumber cost = read_lua_number(api, state, -1);
        api.set_top(state, entry_index);
        if (item_id.status != LuaNumberStatus::Present || item_id.value == 0 ||
            cost.status != LuaNumberStatus::Present || cost.value == 0) {
            *error = command_error(
                "equipment_command_enhance_cost_changed",
                "客户端当前强化材料 ID 或数量无效");
            api.set_top(state, top);
            return false;
        }
        indexed.push_back(IndexedMaterial{
            .index = array_index.value,
            .item_id = item_id.value,
            .cost = cost.value,
        });
        api.set_top(state, value_index - 1);
    }
    api.set_top(state, top);
    std::sort(
        indexed.begin(),
        indexed.end(),
        [](const IndexedMaterial& left, const IndexedMaterial& right) {
            return left.index < right.index;
        });
    std::vector<std::pair<std::uint64_t, std::uint64_t>> actual;
    actual.reserve(indexed.size());
    for (std::size_t index = 0; index < indexed.size(); ++index) {
        if (indexed[index].index != index + 1) {
            *error = command_error(
                "equipment_command_enhance_cost_changed",
                "客户端当前强化材料成本数组存在稀疏或非连续索引");
            return false;
        }
        actual.emplace_back(indexed[index].item_id, indexed[index].cost);
    }
    std::sort(actual.begin(), actual.end());
    if (std::adjacent_find(
            actual.begin(),
            actual.end(),
            [](const auto& left, const auto& right) { return left.first == right.first; }) !=
        actual.end()) {
        *error = command_error(
            "equipment_command_enhance_cost_changed",
            "客户端当前强化材料存在重复物品 ID");
        return false;
    }
    if (actual.size() != action.materials.size()) {
        *error = command_error(
            "equipment_command_enhance_cost_changed",
            "客户端当前强化材料种类与计划不一致");
        return false;
    }
    for (std::size_t index = 0; index < actual.size(); ++index) {
        if (actual[index].first != action.materials[index].item_id ||
            actual[index].second != action.materials[index].cost) {
            *error = command_error(
                "equipment_command_enhance_cost_changed",
                "客户端当前强化材料 ID 或数量与计划不一致");
            return false;
        }
    }
    return true;
}

/// 派发前复核相邻配置、展示等级和完整单级资源成本。
template <typename Action>
bool source_can_enhance(
    const LuaApi& api,
    lua_State* state,
    const Action& action,
    AgentError* error) {
    LuaStackGuard stack(api, state);
    const bool valid = [&]() {
        if constexpr (std::is_same_v<Action, EnhanceWarehouseEquipmentCommandAction>) {
            return enhance_warehouse_equipment_command_action_is_valid(action);
        } else {
            return enhance_ship_equipment_command_action_is_valid(action);
        }
    }();
    if (!valid) {
        *error = command_error(
            "equipment_command_arguments_invalid",
            "强化命令的来源、目标、资源或容量前置条件无效");
        return false;
    }
    if (!push_enhance_source(api, state, action, error)) {
        return false;
    }
    const int source_index = api.get_top(state);
    std::string method_error;
    const std::array next_arguments{LuaCallArgument::string("next")};
    const std::optional<std::uint64_t> next_config_id = read_lua_number_method(
        api,
        state,
        source_index,
        "getConfig",
        next_arguments,
        &method_error);
    if (!next_config_id.has_value()) {
        *error = make_lua_error(
            "lua_equipment_command_enhance_target_failed",
            std::move(method_error));
        return false;
    }
    const std::array gold_arguments{LuaCallArgument::string("trans_use_gold")};
    const std::optional<std::uint64_t> gold_cost = read_lua_number_method(
        api,
        state,
        source_index,
        "getConfig",
        gold_arguments,
        &method_error);
    if (!gold_cost.has_value()) {
        *error = make_lua_error(
            "lua_equipment_command_enhance_cost_failed",
            std::move(method_error));
        return false;
    }
    if (*next_config_id != action.target_config_id || *gold_cost != action.gold_cost ||
        !enhance_material_costs_match(api, state, source_index, action, error)) {
        if (error->code.empty()) {
            *error = command_error(
                "equipment_command_enhance_config_changed",
                "客户端当前下一强化配置或物资成本与计划不一致");
        }
        return false;
    }
    if (!push_new_equipment(api, state, action.target_config_id, &method_error)) {
        *error = make_lua_error(
            "lua_equipment_command_enhance_target_failed",
            std::move(method_error));
        return false;
    }
    EquipmentSnapshot target;
    if (!read_equipment_snapshot(api, state, -1, &target, &method_error)) {
        *error = make_lua_error(
            "lua_equipment_command_enhance_target_failed",
            std::move(method_error));
        return false;
    }
    if (target.config_id != action.target_config_id ||
        target.enhance_level != action.target_enhance_level) {
        *error = command_error(
            "equipment_command_enhance_config_changed",
            "客户端构造的下一强化配置或展示等级与计划不一致");
        return false;
    }
    return true;
}

/// 构造官方强化通知使用的材料 Drop 数组并写入 body.materials。
template <typename Action>
bool set_enhance_materials(
    const LuaApi& api,
    lua_State* state,
    int body_index,
    const Action& action,
    std::string* error) {
    api.push_string(state, "DROP_TYPE_ITEM");
    api.raw_get(state, kLuaGlobalsIndex);
    const LuaNumber drop_type = read_lua_number(api, state, -1);
    api.set_top(state, body_index);
    if (drop_type.status != LuaNumberStatus::Present || drop_type.value == 0) {
        *error = "Lua 全局 DROP_TYPE_ITEM 不是正整数";
        return false;
    }
    if (!push_lua_global_table(api, state, "Drop", error)) {
        return false;
    }
    const int drop_table_index = api.get_top(state);
    api.create_table(state, static_cast<int>(action.materials.size()), 0);
    const int materials_index = api.get_top(state);
    for (std::size_t index = 0; index < action.materials.size(); ++index) {
        const EquipmentCommandMaterialCost& material = action.materials[index];
        api.create_table(state, 0, 3);
        const int argument_index = api.get_top(state);
        api.push_number(state, static_cast<double>(drop_type.value));
        api.set_field(state, argument_index, "type");
        api.push_number(state, static_cast<double>(material.item_id));
        api.set_field(state, argument_index, "id");
        api.push_number(state, static_cast<double>(material.cost));
        api.set_field(state, argument_index, "count");
        const std::array arguments{LuaCallArgument::stack_value(argument_index)};
        if (!push_lua_table_function_result(
                api,
                state,
                drop_table_index,
                "New",
                arguments,
                error)) {
            api.set_top(state, body_index);
            return false;
        }
        const int drop_type_result = api.type(state, -1);
        if (drop_type_result != kLuaTypeTable && drop_type_result != kLuaTypeUserData) {
            *error = "Drop.New 未返回 table 或 userdata";
            api.set_top(state, body_index);
            return false;
        }
        api.raw_set_i(state, materials_index, static_cast<int>(index + 1));
        api.set_top(state, materials_index);
    }
    api.set_field(state, body_index, "materials");
    api.set_top(state, body_index);
    return true;
}

/// 构造仓库或舰上单级强化的官方通知 body。
template <typename Action>
bool push_enhance_notification_body(
    const LuaApi& api,
    lua_State* state,
    const Action& action,
    std::string* error) {
    api.create_table(state, 0, 6);
    const int body_index = api.get_top(state);
    if constexpr (std::is_same_v<Action, EnhanceWarehouseEquipmentCommandAction>) {
        api.push_number(state, static_cast<double>(action.source_before.equipment_id));
        api.set_field(state, body_index, "equipmentId");
    } else {
        api.push_number(state, static_cast<double>(action.ship_id));
        api.set_field(state, body_index, "shipId");
        api.push_number(state, static_cast<double>(action.slot_index));
        api.set_field(state, body_index, "pos");
    }
    if (!set_enhance_materials(api, state, body_index, action, error)) {
        return false;
    }
    api.push_number(state, static_cast<double>(action.gold_cost));
    api.set_field(state, body_index, "consume");
    if (!push_new_equipment(api, state, action.target_config_id, error)) {
        api.set_top(state, body_index);
        return false;
    }
    api.set_field(state, body_index, "target");
    api.set_top(state, body_index);
    return true;
}

/// 调用游戏自己的通知命令；body 不携带 Agent C 回调，保证卸载后无悬空闭包。
bool send_equipment_notification(
    const LuaApi& api,
    lua_State* state,
    const EquipmentCommand& command,
    EquipmentCommandDispatchGate* dispatch_gate,
    EquipmentCommandDeadline deadline,
    bool* call_started,
    std::string* error) {
    LuaStackGuard stack(api, state);
    if (!push_lua_global_table(api, state, "pg", error)) {
        return false;
    }
    const int pg_index = api.get_top(state);
    if (api.get_field_protected(state, pg_index, "m02") != 0) {
        *error = lua_failure_detail(api, state, "读取 pg.m02 失败");
        return false;
    }
    const int m02_type = api.type(state, -1);
    if (m02_type != kLuaTypeTable && m02_type != kLuaTypeUserData) {
        *error = "pg.m02 尚未成为通知分发对象";
        return false;
    }
    const int m02_index = api.get_top(state);

    if (!push_lua_global_table(api, state, "GAME", error)) {
        return false;
    }
    const int game_index = api.get_top(state);
    const EquipmentCommandActionKind action_kind =
        equipment_command_action_kind(command.action);
    const char* event_field = nullptr;
    switch (action_kind) {
        case EquipmentCommandActionKind::Equip:
            event_field = "EQUIP_TO_SHIP";
            break;
        case EquipmentCommandActionKind::Unequip:
            event_field = "UNEQUIP_FROM_SHIP";
            break;
        case EquipmentCommandActionKind::Dismantle:
            event_field = "DESTROY_EQUIPMENTS";
            break;
        case EquipmentCommandActionKind::Compose:
            event_field = "COMPOSITE_EQUIPMENT";
            break;
        case EquipmentCommandActionKind::EnhanceWarehouse:
        case EquipmentCommandActionKind::EnhanceShip:
            event_field = "UPGRADE_EQUIPMENTS";
            break;
    }
    if (api.get_field_protected(state, game_index, event_field) != 0) {
        *error = lua_failure_detail(
            api,
            state,
            "读取 GAME." + std::string(event_field) + " 失败");
        return false;
    }
    std::string text_error;
    const std::optional<std::string> event =
        read_lua_text(api, state, -1, 256, false, &text_error);
    if (!event.has_value()) {
        *error = "GAME." + std::string(event_field) + " 不是有效通知名称: " + text_error;
        return false;
    }
    api.set_top(state, m02_index);

    if (const auto* action = std::get_if<ComposeEquipmentCommandAction>(&command.action)) {
        api.create_table(state, 0, 2);
        const int body_index = api.get_top(state);
        api.push_number(state, static_cast<double>(action->recipe_id));
        api.set_field(state, body_index, "id");
        api.push_number(state, static_cast<double>(action->compose_quantity));
        api.set_field(state, body_index, "count");
    } else if (const auto* action =
                   std::get_if<DismantleEquipmentCommandAction>(&command.action)) {
        api.create_table(state, 0, 1);
        const int body_index = api.get_top(state);
        api.create_table(state, 1, 0);
        const int equipments_index = api.get_top(state);
        api.create_table(state, 2, 0);
        const int entry_index = api.get_top(state);
        api.push_number(state, static_cast<double>(action->source_before.equipment_id));
        api.raw_set_i(state, entry_index, 1);
        api.push_number(state, static_cast<double>(action->dismantle_quantity));
        api.raw_set_i(state, entry_index, 2);
        api.raw_set_i(state, equipments_index, 1);
        api.set_field(state, body_index, "equipments");
    } else if (const auto* action =
                   std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action)) {
        if (!push_enhance_notification_body(api, state, *action, error)) {
            return false;
        }
    } else if (const auto* action =
                   std::get_if<EnhanceShipEquipmentCommandAction>(&command.action)) {
        if (!push_enhance_notification_body(api, state, *action, error)) {
            return false;
        }
    } else {
        const auto* equip = std::get_if<EquipEquipmentCommandAction>(&command.action);
        const auto* unequip = std::get_if<UnequipEquipmentCommandAction>(&command.action);
        if (equip == nullptr && unequip == nullptr) {
            *error = "装备命令动作没有对应的通知 body";
            return false;
        }
        const std::uint64_t ship_id = equip != nullptr ? equip->ship_id : unequip->ship_id;
        const std::uint32_t slot_index =
            equip != nullptr ? equip->slot_index : unequip->slot_index;
        api.create_table(state, 0, action_kind == EquipmentCommandActionKind::Equip ? 3 : 2);
        const int body_index = api.get_top(state);
        api.push_number(state, static_cast<double>(ship_id));
        api.set_field(state, body_index, "shipId");
        api.push_number(state, static_cast<double>(slot_index));
        api.set_field(state, body_index, "pos");
        if (equip != nullptr) {
            api.push_number(state, static_cast<double>(equip->source_before.equipment_id));
            api.set_field(state, body_index, "equipmentId");
        }
    }
    const int body_index = api.get_top(state);

    if (api.get_field_protected(state, m02_index, "sendNotification") != 0) {
        *error = lua_failure_detail(api, state, "读取 pg.m02.sendNotification 失败");
        return false;
    }
    if (api.type(state, -1) != kLuaTypeFunction) {
        *error = "pg.m02 缺少 sendNotification 方法";
        return false;
    }
    api.push_value(state, m02_index);
    api.push_string(state, event->c_str());
    api.push_value(state, body_index);
    if (!begin_equipment_command_dispatch(
            dispatch_gate,
            deadline)) {
        const EquipmentCommandDispatchGateState gate_state =
            dispatch_gate->load(std::memory_order_acquire);
        *call_started = gate_state == EquipmentCommandDispatchGateState::Claimed ||
                        gate_state == EquipmentCommandDispatchGateState::Started;
        *error = gate_state == EquipmentCommandDispatchGateState::Canceled
                     ? "装备命令已在通知调用前取消"
                     : "装备命令派发门已经被其他调用占用";
        return false;
    }
    *call_started = true;
    if (api.protected_call(state, 3, 0, 0) != 0) {
        *error = lua_failure_detail(api, state, "调用 pg.m02.sendNotification 失败");
        return false;
    }
    return true;
}


}  // namespace

bool begin_equipment_command_dispatch(
    EquipmentCommandDispatchGate* gate,
    EquipmentCommandDeadline deadline) noexcept {
    if (gate == nullptr) {
        return false;
    }
    EquipmentCommandDispatchGateState expected =
        EquipmentCommandDispatchGateState::Pending;
    if (!gate->compare_exchange_strong(
            expected,
            EquipmentCommandDispatchGateState::Claimed,
            std::memory_order_acq_rel,
            std::memory_order_acquire)) {
        return false;
    }
    if (EquipmentCommandClock::now() >= deadline) {
        expected = EquipmentCommandDispatchGateState::Claimed;
        (void)gate->compare_exchange_strong(
            expected,
            EquipmentCommandDispatchGateState::Canceled,
            std::memory_order_acq_rel,
            std::memory_order_acquire);
        return false;
    }
    expected = EquipmentCommandDispatchGateState::Claimed;
    return gate->compare_exchange_strong(
        expected,
        EquipmentCommandDispatchGateState::Started,
        std::memory_order_acq_rel,
        std::memory_order_acquire);
}

EquipmentCommandDispatchCancelResult cancel_equipment_command_dispatch(
    EquipmentCommandDispatchGate* gate) noexcept {
    if (gate == nullptr) {
        return EquipmentCommandDispatchCancelResult::CallMayHaveStarted;
    }
    EquipmentCommandDispatchGateState observed = gate->load(std::memory_order_acquire);
    while (observed == EquipmentCommandDispatchGateState::Pending ||
           observed == EquipmentCommandDispatchGateState::Claimed) {
        if (gate->compare_exchange_weak(
                observed,
                EquipmentCommandDispatchGateState::Canceled,
                std::memory_order_acq_rel,
                std::memory_order_acquire)) {
            return EquipmentCommandDispatchCancelResult::CanceledBeforeCall;
        }
    }
    return observed == EquipmentCommandDispatchGateState::Canceled
               ? EquipmentCommandDispatchCancelResult::CanceledBeforeCall
               : EquipmentCommandDispatchCancelResult::CallMayHaveStarted;
}



EquipmentCommandDispatchExecution dispatch_equipment_command(
    const LuaApi& api,
    lua_State* state,
    const EquipmentCommand& command,
    EquipmentCommandDispatchGate* dispatch_gate,
    EquipmentCommandDeadline deadline) noexcept {
    EquipmentCommandDispatchExecution result;
    bool call_started = false;
    if (dispatch_gate == nullptr) {
        result.error = command_error(
            "equipment_command_arguments_invalid",
            "装备命令缺少派发门");
        return result;
    }
    try {
        const EquipmentCommandStateRead state_read =
            read_equipment_command_state(api, state, command);
        if (!state_read.success) {
            result.error = state_read.error;
            return result;
        }
        if (classify_equipment_command_state(command, state_read.state) !=
            EquipmentCommandStateMatch::Before) {
            result.error = command_error(
                "equipment_command_precondition_changed",
                "设备端局部状态与命令绑定的发送前状态不一致");
            return result;
        }
        if (const auto* action = std::get_if<EquipEquipmentCommandAction>(&command.action)) {
            if (!source_can_equip(api, state, *action, &result.error)) {
                return result;
            }
        } else if (const auto* action =
                       std::get_if<DismantleEquipmentCommandAction>(&command.action)) {
            if (!source_can_dismantle(api, state, *action, &result.error)) {
                return result;
            }
        } else if (const auto* action =
                       std::get_if<ComposeEquipmentCommandAction>(&command.action)) {
            if (!source_can_compose(api, state, *action, &result.error)) {
                return result;
            }
        } else if (const auto* action =
                       std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action)) {
            if (!source_can_enhance(api, state, *action, &result.error)) {
                return result;
            }
        } else if (const auto* action =
                       std::get_if<EnhanceShipEquipmentCommandAction>(&command.action)) {
            if (!source_can_enhance(api, state, *action, &result.error)) {
                return result;
            }
        }

        std::string dispatch_error;
        if (!send_equipment_notification(
                api,
                state,
                command,
                dispatch_gate,
                deadline,
                &call_started,
                &dispatch_error)) {
            result.write_dispatched = call_started;
            result.error = call_started
                               ? uncertain_dispatch_error(std::move(dispatch_error))
                               : command_error(
                                     "equipment_command_dispatch_failed",
                                     std::move(dispatch_error));
            return result;
        }
        result.write_dispatched = true;
        return result;
    } catch (const std::exception& exception) {
        result.write_dispatched = call_started;
        result.error = call_started
                           ? uncertain_dispatch_error(exception.what())
                           : command_error(
                                 "equipment_command_dispatch_exception",
                                 "派发装备命令失败: " + std::string(exception.what()));
        return result;
    } catch (...) {
        result.write_dispatched = call_started;
        result.error = call_started
                           ? uncertain_dispatch_error("派发装备命令时发生未知本地异常")
                           : command_error(
                                 "equipment_command_dispatch_exception",
                                 "派发装备命令时发生未知本地异常");
        return result;
    }
}

}  // namespace azlw::agent

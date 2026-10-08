// 根据一次局部状态判断装备命令仍是前态、已到后态，或出现其他变化。

#include "equipment_command.h"

#include <optional>
#include <type_traits>

namespace azlw::agent {
namespace {

/// 数量为零时仓库对象必须为空，否则对象身份和数量都必须匹配。
bool warehouse_entry_matches(
    const EquipmentWarehouseEntry& actual,
    const std::optional<EquipmentSnapshot>& expected,
    std::uint64_t expected_quantity) {
    if (expected_quantity == 0) {
        return actual.quantity == 0 && !actual.equipment.has_value();
    }
    return actual.quantity == expected_quantity && actual.equipment == expected;
}

/// 根据数量生成仓库应保留的对象身份。
std::optional<EquipmentSnapshot> equipment_when_present(
    const std::optional<EquipmentSnapshot>& equipment,
    std::uint64_t quantity) {
    return quantity == 0 ? std::nullopt : equipment;
}

/// 合成新建仓库对象时运行态 ID 未知，但配置、强化等级和数量必须精确匹配。
bool compose_output_matches_after(
    const EquipmentWarehouseEntry& actual,
    const ComposeEquipmentCommandAction& action) {
    const std::uint64_t expected_quantity =
        action.output_quantity_before + action.compose_quantity;
    if (actual.quantity != expected_quantity || !actual.equipment.has_value()) {
        return false;
    }
    if (action.output_before.has_value()) {
        return actual.equipment == action.output_before;
    }
    return actual.equipment->equipment_id != 0 &&
           actual.equipment->config_id == action.output_config_id &&
           actual.equipment->enhance_level == 0;
}

/// 强化新建目标聚合时运行态 ID 未知，但配置、等级和数量必须精确匹配。
bool enhance_target_matches_after(
    const EquipmentWarehouseEntry& actual,
    const EnhanceWarehouseEquipmentCommandAction& action) {
    const std::uint64_t expected_quantity = action.target_warehouse_quantity_before + 1;
    if (actual.quantity != expected_quantity || !actual.equipment.has_value()) {
        return false;
    }
    if (action.target_before.has_value()) {
        return actual.equipment == action.target_before;
    }
    return actual.equipment->equipment_id != 0 &&
           actual.equipment->config_id == action.target_config_id &&
           actual.equipment->enhance_level == action.target_enhance_level;
}

/// 核对单级强化前后全部材料数量和物资，拒绝遗漏或额外的局部资源。
template <typename Action>
bool enhance_resources_match(
    const EquipmentCommandLocalState& state,
    const Action& action,
    bool after) {
    if (state.enhance_materials.size() != action.materials.size()) {
        return false;
    }
    for (std::size_t index = 0; index < action.materials.size(); ++index) {
        const EquipmentCommandMaterialCost& expected = action.materials[index];
        const EquipmentCommandMaterialState& actual = state.enhance_materials[index];
        const std::uint64_t expected_quantity =
            after ? expected.quantity_before - expected.cost : expected.quantity_before;
        if (actual.item_id != expected.item_id || actual.quantity != expected_quantity) {
            return false;
        }
    }
    return state.gold == (after ? action.gold_before - action.gold_cost : action.gold_before);
}

/// 强化后的舰槽对象运行态 ID 可变化，但配置和展示等级必须精确达到单级目标。
template <typename Action>
bool enhance_ship_target_matches(
    const std::optional<EquipmentSnapshot>& actual,
    const Action& action) {
    return actual.has_value() && actual->equipment_id != 0 &&
           actual->config_id == action.target_config_id &&
           actual->enhance_level == action.target_enhance_level;
}

/// 核对命令载荷描述的完整发送前局部状态。
bool matches_before(
    const EquipmentCommand& command,
    const EquipmentCommandLocalState& state) {
    return std::visit(
        [&](const auto& action) {
            using Action = std::decay_t<decltype(action)>;
            if (state.equipment_capacity != action.equipment_capacity_before ||
                state.equipment_limit != action.equipment_limit_before) {
                return false;
            }
            if constexpr (std::is_same_v<Action, UnequipEquipmentCommandAction>) {
                return state.target_slot ==
                           std::optional<EquipmentSnapshot>{action.target_before} &&
                       state.equipment_skin_id == 0 &&
                       warehouse_entry_matches(
                           state.target_warehouse,
                           equipment_when_present(
                               action.target_before,
                               action.target_warehouse_quantity_before),
                           action.target_warehouse_quantity_before);
            } else if constexpr (std::is_same_v<Action, EquipEquipmentCommandAction>) {
                return state.target_slot == action.target_before &&
                       state.equipment_skin_id == 0 &&
                       warehouse_entry_matches(
                           state.source_warehouse,
                           action.source_before,
                           action.source_quantity_before) &&
                       warehouse_entry_matches(
                           state.target_warehouse,
                           equipment_when_present(
                               action.target_before,
                               action.target_warehouse_quantity_before),
                           action.target_warehouse_quantity_before);
            } else if constexpr (std::is_same_v<Action, DismantleEquipmentCommandAction>) {
                return !state.target_slot.has_value() && state.equipment_skin_id == 0 &&
                       warehouse_entry_matches(
                           state.source_warehouse,
                           action.source_before,
                           action.source_quantity_before) &&
                       warehouse_entry_matches(state.target_warehouse, std::nullopt, 0);
            } else if constexpr (std::is_same_v<Action, ComposeEquipmentCommandAction>) {
                return !state.target_slot.has_value() && state.equipment_skin_id == 0 &&
                       warehouse_entry_matches(
                           state.compose_output_warehouse,
                           action.output_before,
                           action.output_quantity_before) &&
                       state.compose_material_quantity == action.material_quantity_before &&
                       state.gold == action.gold_before;
            } else if constexpr (
                std::is_same_v<Action, EnhanceWarehouseEquipmentCommandAction>) {
                return !state.target_slot.has_value() && state.equipment_skin_id == 0 &&
                       warehouse_entry_matches(
                           state.source_warehouse,
                           action.source_before,
                           action.source_quantity_before) &&
                       warehouse_entry_matches(
                           state.target_warehouse,
                           action.target_before,
                           action.target_warehouse_quantity_before) &&
                       enhance_resources_match(state, action, false);
            } else {
                return state.target_slot ==
                           std::optional<EquipmentSnapshot>{action.source_before} &&
                       state.equipment_skin_id == 0 &&
                       warehouse_entry_matches(state.source_warehouse, std::nullopt, 0) &&
                       warehouse_entry_matches(state.target_warehouse, std::nullopt, 0) &&
                       enhance_resources_match(state, action, false);
            }
        },
        command.action);
}

/// 核对游戏成功回调完成后必须同时成立的槽位、仓库和容量变化。
bool matches_after(
    const EquipmentCommand& command,
    const EquipmentCommandLocalState& state) {
    return std::visit(
        [&](const auto& action) {
            using Action = std::decay_t<decltype(action)>;
            if (state.equipment_skin_id != 0 ||
                state.equipment_limit != action.equipment_limit_before) {
                return false;
            }
            if constexpr (std::is_same_v<Action, UnequipEquipmentCommandAction>) {
                const std::uint64_t target_quantity =
                    action.target_warehouse_quantity_before + 1;
                return !state.target_slot.has_value() &&
                       state.equipment_capacity == action.equipment_capacity_before + 1 &&
                       warehouse_entry_matches(
                           state.target_warehouse,
                           action.target_before,
                           target_quantity);
            } else if constexpr (std::is_same_v<Action, EquipEquipmentCommandAction>) {
                const std::uint64_t source_quantity = action.source_quantity_before - 1;
                const std::uint64_t expected_capacity = action.target_before.has_value()
                                                            ? action.equipment_capacity_before
                                                            : action.equipment_capacity_before - 1;
                if (state.target_slot !=
                        std::optional<EquipmentSnapshot>{action.source_before} ||
                    state.equipment_capacity != expected_capacity ||
                    !warehouse_entry_matches(
                        state.source_warehouse,
                        equipment_when_present(action.source_before, source_quantity),
                        source_quantity)) {
                    return false;
                }
                if (!action.target_before.has_value()) {
                    return warehouse_entry_matches(state.target_warehouse, std::nullopt, 0);
                }
                const std::uint64_t target_quantity =
                    action.target_warehouse_quantity_before + 1;
                return warehouse_entry_matches(
                    state.target_warehouse,
                    action.target_before,
                    target_quantity);
            } else if constexpr (std::is_same_v<Action, DismantleEquipmentCommandAction>) {
                const std::uint64_t source_quantity =
                    action.source_quantity_before - action.dismantle_quantity;
                return !state.target_slot.has_value() &&
                       state.equipment_capacity ==
                           action.equipment_capacity_before - action.dismantle_quantity &&
                       warehouse_entry_matches(
                           state.source_warehouse,
                           equipment_when_present(action.source_before, source_quantity),
                           source_quantity) &&
                       warehouse_entry_matches(state.target_warehouse, std::nullopt, 0);
            } else if constexpr (std::is_same_v<Action, ComposeEquipmentCommandAction>) {
                const std::uint64_t material_cost =
                    action.material_quantity_per_unit * action.compose_quantity;
                const std::uint64_t gold_cost = action.gold_per_unit * action.compose_quantity;
                return !state.target_slot.has_value() &&
                       state.equipment_capacity ==
                           action.equipment_capacity_before + action.compose_quantity &&
                       compose_output_matches_after(state.compose_output_warehouse, action) &&
                       state.compose_material_quantity ==
                           action.material_quantity_before - material_cost &&
                       state.gold == action.gold_before - gold_cost;
            } else if constexpr (
                std::is_same_v<Action, EnhanceWarehouseEquipmentCommandAction>) {
                const std::uint64_t source_quantity = action.source_quantity_before - 1;
                return !state.target_slot.has_value() &&
                       state.equipment_capacity == action.equipment_capacity_before &&
                       warehouse_entry_matches(
                           state.source_warehouse,
                           equipment_when_present(action.source_before, source_quantity),
                           source_quantity) &&
                       enhance_target_matches_after(state.target_warehouse, action) &&
                       enhance_resources_match(state, action, true);
            } else {
                return state.equipment_capacity == action.equipment_capacity_before &&
                       enhance_ship_target_matches(state.target_slot, action) &&
                       warehouse_entry_matches(state.source_warehouse, std::nullopt, 0) &&
                       warehouse_entry_matches(state.target_warehouse, std::nullopt, 0) &&
                       enhance_resources_match(state, action, true);
            }
        },
        command.action);
}

}  // namespace

EquipmentCommandStateMatch classify_equipment_command_state(
    const EquipmentCommand& command,
    const EquipmentCommandLocalState& state) noexcept {
    if (const auto* action = std::get_if<DismantleEquipmentCommandAction>(&command.action);
        action != nullptr && !dismantle_equipment_command_action_is_valid(*action)) {
        return EquipmentCommandStateMatch::Mismatch;
    }
    if (const auto* action = std::get_if<ComposeEquipmentCommandAction>(&command.action);
        action != nullptr && !compose_equipment_command_action_is_valid(*action)) {
        return EquipmentCommandStateMatch::Mismatch;
    }
    if (const auto* action =
            std::get_if<EnhanceWarehouseEquipmentCommandAction>(&command.action);
        action != nullptr && !enhance_warehouse_equipment_command_action_is_valid(*action)) {
        return EquipmentCommandStateMatch::Mismatch;
    }
    if (const auto* action = std::get_if<EnhanceShipEquipmentCommandAction>(&command.action);
        action != nullptr && !enhance_ship_equipment_command_action_is_valid(*action)) {
        return EquipmentCommandStateMatch::Mismatch;
    }
    if (matches_after(command, state)) {
        return EquipmentCommandStateMatch::After;
    }
    if (matches_before(command, state)) {
        return EquipmentCommandStateMatch::Before;
    }
    return EquipmentCommandStateMatch::Mismatch;
}
}  // namespace azlw::agent

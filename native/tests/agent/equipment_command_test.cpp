// 验证装备命令对完整前态、预期后态和异常局部变化的确定分类。

#include <cstdlib>
#include <iostream>
#include <optional>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "commands/equipment_command.h"

namespace {

using azlw::agent::EquipmentCommand;
using azlw::agent::EquipmentCommandMaterialCost;
using azlw::agent::EquipmentCommandMaterialState;
using azlw::agent::ComposeEquipmentCommandAction;
using azlw::agent::DismantleEquipmentCommandAction;
using azlw::agent::EquipmentCommandDispatchCancelResult;
using azlw::agent::EquipmentCommandClock;
using azlw::agent::EquipmentCommandDispatchGate;
using azlw::agent::EquipmentCommandDispatchGateState;
using azlw::agent::EquipmentCommandLocalState;
using azlw::agent::EquipmentCommandStateMatch;
using azlw::agent::EquipmentSnapshot;
using azlw::agent::EquipmentWarehouseEntry;
using azlw::agent::EquipEquipmentCommandAction;
using azlw::agent::EnhanceShipEquipmentCommandAction;
using azlw::agent::EnhanceWarehouseEquipmentCommandAction;
using azlw::agent::UnequipEquipmentCommandAction;
using azlw::agent::begin_equipment_command_dispatch;
using azlw::agent::cancel_equipment_command_dispatch;
using azlw::agent::classify_equipment_command_state;
using azlw::agent::compose_equipment_command_action_is_valid;
using azlw::agent::dismantle_equipment_command_action_is_valid;
using azlw::agent::enhance_ship_equipment_command_action_is_valid;
using azlw::agent::enhance_warehouse_equipment_command_action_is_valid;

/// 断言条件成立；失败时输出具体原因并结束测试。
void require(bool condition, const std::string& message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << '\n';
        std::exit(1);
    }
}

/// 构造协议测试使用的聚合装备身份。
EquipmentSnapshot equipment(std::uint64_t equipment_id, std::uint32_t enhance_level) {
    return EquipmentSnapshot{
        .equipment_id = equipment_id,
        .config_id = equipment_id,
        .enhance_level = enhance_level,
    };
}

/// 构造仓库中存在的单个聚合条目。
EquipmentWarehouseEntry warehouse(EquipmentSnapshot value, std::uint64_t quantity) {
    return EquipmentWarehouseEntry{
        .equipment = value,
        .quantity = quantity,
    };
}

}  // namespace

int main() {
    EquipmentCommandDispatchGate canceled_gate{
        EquipmentCommandDispatchGateState::Pending};
    require(
        cancel_equipment_command_dispatch(&canceled_gate) ==
                EquipmentCommandDispatchCancelResult::CanceledBeforeCall &&
            !begin_equipment_command_dispatch(
                &canceled_gate,
                EquipmentCommandClock::time_point::max()),
        "timeout cancellation must prevent a later notification call");

    EquipmentCommandDispatchGate started_gate{
        EquipmentCommandDispatchGateState::Pending};
    require(
        begin_equipment_command_dispatch(
            &started_gate,
            EquipmentCommandClock::time_point::max()) &&
            cancel_equipment_command_dispatch(&started_gate) ==
                EquipmentCommandDispatchCancelResult::CallMayHaveStarted,
        "a started notification call must never be reported as canceled");

    EquipmentCommandDispatchGate claimed_gate{
        EquipmentCommandDispatchGateState::Claimed};
    require(
        cancel_equipment_command_dispatch(&claimed_gate) ==
                EquipmentCommandDispatchCancelResult::CanceledBeforeCall &&
            claimed_gate.load() == EquipmentCommandDispatchGateState::Canceled,
        "shutdown must cancel a claimed gate before the notification call starts");

    EquipmentCommandDispatchGate expired_gate{
        EquipmentCommandDispatchGateState::Pending};
    require(
        !begin_equipment_command_dispatch(
                &expired_gate,
                EquipmentCommandClock::time_point::min()) &&
            expired_gate.load() == EquipmentCommandDispatchGateState::Canceled,
        "the original absolute deadline must prevent a late notification call");

    for (int iteration = 0; iteration < 1'000; ++iteration) {
        EquipmentCommandDispatchGate racing_gate{
            EquipmentCommandDispatchGateState::Pending};
        std::atomic<unsigned int> ready{0};
        std::atomic<bool> start{false};
        bool dispatch_started = false;
        EquipmentCommandDispatchCancelResult cancel_result =
            EquipmentCommandDispatchCancelResult::CallMayHaveStarted;
        std::thread dispatch_thread([&]() {
            ready.fetch_add(1, std::memory_order_release);
            while (!start.load(std::memory_order_acquire)) {
                std::this_thread::yield();
            }
            dispatch_started = begin_equipment_command_dispatch(
                &racing_gate,
                EquipmentCommandClock::time_point::max());
        });
        std::thread cancel_thread([&]() {
            ready.fetch_add(1, std::memory_order_release);
            while (!start.load(std::memory_order_acquire)) {
                std::this_thread::yield();
            }
            cancel_result = cancel_equipment_command_dispatch(&racing_gate);
        });
        while (ready.load(std::memory_order_acquire) != 2) {
            std::this_thread::yield();
        }
        start.store(true, std::memory_order_release);
        dispatch_thread.join();
        cancel_thread.join();

        const EquipmentCommandDispatchGateState final_state =
            racing_gate.load(std::memory_order_acquire);
        require(
            (dispatch_started &&
             cancel_result ==
                 EquipmentCommandDispatchCancelResult::CallMayHaveStarted &&
             final_state == EquipmentCommandDispatchGateState::Started) ||
                (!dispatch_started &&
                 cancel_result ==
                     EquipmentCommandDispatchCancelResult::CanceledBeforeCall &&
                 final_state == EquipmentCommandDispatchGateState::Canceled),
            "dispatch and cancel must resolve to one coherent atomic winner");
    }

    const EquipmentSnapshot target = equipment(500, 10);
    const EquipmentSnapshot source = equipment(600, 6);

    EquipmentCommand unequip;
    unequip.action = UnequipEquipmentCommandAction{
        .ship_id = 9'001,
        .slot_index = 1,
        .target_before = target,
        .target_warehouse_quantity_before = 2,
        .equipment_capacity_before = 10,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState unequip_before{
        .target_slot = target,
        .equipment_skin_id = 0,
        .source_warehouse = {},
        .target_warehouse = warehouse(target, 2),
        .equipment_capacity = 10,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {},
        .gold = 0,
    };
    EquipmentCommandLocalState unequip_after = unequip_before;
    unequip_after.target_slot.reset();
    unequip_after.target_warehouse = warehouse(target, 3);
    unequip_after.equipment_capacity = 11;
    require(
        classify_equipment_command_state(unequip, unequip_before) ==
            EquipmentCommandStateMatch::Before,
        "unequip pre-state must remain distinguishable");
    require(
        classify_equipment_command_state(unequip, unequip_after) ==
            EquipmentCommandStateMatch::After,
        "unequip post-state must require slot, quantity, and capacity changes");
    unequip_after.equipment_skin_id = 7;
    require(
        classify_equipment_command_state(unequip, unequip_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "equipment skin changes must not be accepted as success");

    EquipmentCommand equip_empty;
    equip_empty.action = EquipEquipmentCommandAction{
        .ship_id = 9'001,
        .slot_index = 2,
        .target_before = std::nullopt,
        .source_before = source,
        .source_quantity_before = 3,
        .target_warehouse_quantity_before = 0,
        .equipment_capacity_before = 11,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState equip_empty_before{
        .target_slot = std::nullopt,
        .equipment_skin_id = 0,
        .source_warehouse = warehouse(source, 3),
        .target_warehouse = {},
        .equipment_capacity = 11,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {},
        .gold = 0,
    };
    EquipmentCommandLocalState equip_empty_after = equip_empty_before;
    equip_empty_after.target_slot = source;
    equip_empty_after.source_warehouse = warehouse(source, 2);
    equip_empty_after.equipment_capacity = 10;
    require(
        classify_equipment_command_state(equip_empty, equip_empty_before) ==
            EquipmentCommandStateMatch::Before,
        "empty-slot equip pre-state must match");
    require(
        classify_equipment_command_state(equip_empty, equip_empty_after) ==
            EquipmentCommandStateMatch::After,
        "empty-slot equip must consume one warehouse capacity unit");

    EquipmentCommand equip_replace;
    equip_replace.action = EquipEquipmentCommandAction{
        .ship_id = 9'001,
        .slot_index = 3,
        .target_before = target,
        .source_before = source,
        .source_quantity_before = 3,
        .target_warehouse_quantity_before = 2,
        .equipment_capacity_before = 11,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState equip_replace_before{
        .target_slot = target,
        .equipment_skin_id = 0,
        .source_warehouse = warehouse(source, 3),
        .target_warehouse = warehouse(target, 2),
        .equipment_capacity = 11,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {},
        .gold = 0,
    };
    EquipmentCommandLocalState equip_replace_after = equip_replace_before;
    equip_replace_after.target_slot = source;
    equip_replace_after.source_warehouse = warehouse(source, 2);
    equip_replace_after.target_warehouse = warehouse(target, 3);
    require(
        classify_equipment_command_state(equip_replace, equip_replace_after) ==
            EquipmentCommandStateMatch::After,
        "replacement equip must preserve capacity and update both aggregates");
    equip_replace_after.target_warehouse.quantity = 4;
    require(
        classify_equipment_command_state(equip_replace, equip_replace_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "unrelated warehouse changes must make the result uncertain");

    const EquipmentSnapshot disposable = equipment(700, 0);
    EquipmentCommand dismantle;
    dismantle.action = DismantleEquipmentCommandAction{
        .source_before = disposable,
        .source_quantity_before = 7,
        .dismantle_quantity = 1,
        .equipment_capacity_before = 11,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState dismantle_before{
        .target_slot = std::nullopt,
        .equipment_skin_id = 0,
        .source_warehouse = warehouse(disposable, 7),
        .target_warehouse = {},
        .equipment_capacity = 11,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {},
        .gold = 0,
    };
    EquipmentCommandLocalState dismantle_after = dismantle_before;
    dismantle_after.source_warehouse = warehouse(disposable, 6);
    dismantle_after.equipment_capacity = 10;
    require(
        classify_equipment_command_state(dismantle, dismantle_before) ==
            EquipmentCommandStateMatch::Before,
        "dismantle pre-state must match the source aggregate and capacity");
    require(
        classify_equipment_command_state(dismantle, dismantle_after) ==
            EquipmentCommandStateMatch::After,
        "dismantle post-state must consume the exact quantity and capacity");
    dismantle_after.source_warehouse.quantity = 5;
    require(
        classify_equipment_command_state(dismantle, dismantle_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "partial source changes must not prove dismantle success");
    dismantle_after = dismantle_before;
    dismantle_after.source_warehouse = warehouse(disposable, 6);
    dismantle_after.equipment_capacity = 11;
    require(
        classify_equipment_command_state(dismantle, dismantle_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "dismantle must observe the matching warehouse capacity decrease");
    dismantle_after = dismantle_before;
    dismantle_after.source_warehouse = warehouse(disposable, 6);
    dismantle_after.equipment_capacity = 10;
    dismantle_after.target_slot = target;
    require(
        classify_equipment_command_state(dismantle, dismantle_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "dismantle must reject unrelated slot changes");

    auto invalid_dismantle = std::get<DismantleEquipmentCommandAction>(dismantle.action);
    invalid_dismantle.dismantle_quantity = 0;
    dismantle.action = invalid_dismantle;
    require(
        !dismantle_equipment_command_action_is_valid(invalid_dismantle) &&
            classify_equipment_command_state(dismantle, dismantle_before) ==
                EquipmentCommandStateMatch::Mismatch,
        "zero dismantle quantity must be rejected before state classification");
    invalid_dismantle.dismantle_quantity = 8;
    dismantle.action = invalid_dismantle;
    require(
        !dismantle_equipment_command_action_is_valid(invalid_dismantle) &&
            classify_equipment_command_state(dismantle, dismantle_before) ==
                EquipmentCommandStateMatch::Mismatch,
        "dismantle quantity above the source aggregate must be rejected");
    invalid_dismantle.dismantle_quantity = 1;
    invalid_dismantle.equipment_capacity_before = 6;
    dismantle.action = invalid_dismantle;
    require(
        !dismantle_equipment_command_action_is_valid(invalid_dismantle) &&
            classify_equipment_command_state(dismantle, dismantle_before) ==
                EquipmentCommandStateMatch::Mismatch,
        "source quantity above warehouse capacity must be rejected");

    EquipmentCommand dismantle_all;
    dismantle_all.action = DismantleEquipmentCommandAction{
        .source_before = disposable,
        .source_quantity_before = 2,
        .dismantle_quantity = 2,
        .equipment_capacity_before = 2,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState dismantle_all_after{
        .target_slot = std::nullopt,
        .equipment_skin_id = 0,
        .source_warehouse = {},
        .target_warehouse = {},
        .equipment_capacity = 0,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {},
        .gold = 0,
    };
    require(
        classify_equipment_command_state(dismantle_all, dismantle_all_after) ==
            EquipmentCommandStateMatch::After,
        "dismantling the whole aggregate must require the warehouse object to disappear");

    const EquipmentSnapshot compose_output{
        .equipment_id = 800,
        .config_id = 1'000,
        .enhance_level = 0,
    };
    EquipmentCommand compose;
    compose.action = ComposeEquipmentCommandAction{
        .recipe_id = 2,
        .compose_quantity = 2,
        .output_config_id = 1'000,
        .output_before = compose_output,
        .output_quantity_before = 3,
        .material_id = 20'001,
        .material_quantity_before = 20,
        .material_quantity_per_unit = 5,
        .gold_before = 1'000,
        .gold_per_unit = 100,
        .equipment_capacity_before = 10,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState compose_before{
        .target_slot = std::nullopt,
        .equipment_skin_id = 0,
        .source_warehouse = {},
        .target_warehouse = {},
        .equipment_capacity = 10,
        .equipment_limit = 300,
        .compose_output_warehouse = warehouse(compose_output, 3),
        .compose_material_quantity = 20,
        .enhance_materials = {},
        .gold = 1'000,
    };
    EquipmentCommandLocalState compose_after = compose_before;
    compose_after.compose_output_warehouse = warehouse(compose_output, 5);
    compose_after.compose_material_quantity = 10;
    compose_after.gold = 800;
    compose_after.equipment_capacity = 12;
    require(
        classify_equipment_command_state(compose, compose_before) ==
            EquipmentCommandStateMatch::Before,
        "compose pre-state must bind output, material, gold, and capacity");
    require(
        classify_equipment_command_state(compose, compose_after) ==
            EquipmentCommandStateMatch::After,
        "compose post-state must require all resource and output deltas");
    compose_after.gold = 801;
    require(
        classify_equipment_command_state(compose, compose_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "compose must reject a partial gold delta");

    auto invalid_compose = std::get<ComposeEquipmentCommandAction>(compose.action);
    invalid_compose.material_quantity_before = 9;
    compose.action = invalid_compose;
    require(
        !compose_equipment_command_action_is_valid(invalid_compose) &&
            classify_equipment_command_state(compose, compose_before) ==
                EquipmentCommandStateMatch::Mismatch,
        "compose must reject insufficient bound material before classification");

    EquipmentCommand compose_new;
    auto compose_new_action = std::get<ComposeEquipmentCommandAction>(compose.action);
    compose_new_action.output_before.reset();
    compose_new_action.output_quantity_before = 0;
    compose_new_action.material_quantity_before = 20;
    compose_new.action = compose_new_action;
    EquipmentCommandLocalState compose_new_before = compose_before;
    compose_new_before.compose_output_warehouse = {};
    EquipmentCommandLocalState compose_new_after = compose_new_before;
    compose_new_after.compose_output_warehouse = warehouse(compose_output, 2);
    compose_new_after.compose_material_quantity = 10;
    compose_new_after.gold = 800;
    compose_new_after.equipment_capacity = 12;
    require(
        classify_equipment_command_state(compose_new, compose_new_before) ==
                EquipmentCommandStateMatch::Before &&
            classify_equipment_command_state(compose_new, compose_new_after) ==
                EquipmentCommandStateMatch::After,
        "compose must accept a newly created aggregate with an unknown runtime equipment id");
    compose_new_after.compose_output_warehouse.equipment->config_id = 1'001;
    require(
        classify_equipment_command_state(compose_new, compose_new_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "compose must reject a newly created aggregate with the wrong output config");

    const EquipmentSnapshot enhance_source{
        .equipment_id = 900,
        .config_id = 1'000,
        .enhance_level = 0,
    };
    const EquipmentSnapshot enhance_target{
        .equipment_id = 901,
        .config_id = 1'001,
        .enhance_level = 1,
    };
    const std::vector<EquipmentCommandMaterialCost> enhance_costs{
        EquipmentCommandMaterialCost{.item_id = 17'001, .quantity_before = 10, .cost = 2},
        EquipmentCommandMaterialCost{.item_id = 17'002, .quantity_before = 4, .cost = 1},
    };
    EquipmentCommand enhance_warehouse;
    enhance_warehouse.action = EnhanceWarehouseEquipmentCommandAction{
        .source_before = enhance_source,
        .source_quantity_before = 3,
        .target_config_id = 1'001,
        .target_enhance_level = 1,
        .target_before = enhance_target,
        .target_warehouse_quantity_before = 2,
        .materials = enhance_costs,
        .gold_before = 1'000,
        .gold_cost = 20,
        .equipment_capacity_before = 5,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState enhance_warehouse_before{
        .target_slot = std::nullopt,
        .equipment_skin_id = 0,
        .source_warehouse = warehouse(enhance_source, 3),
        .target_warehouse = warehouse(enhance_target, 2),
        .equipment_capacity = 5,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {
            EquipmentCommandMaterialState{.item_id = 17'001, .quantity = 10},
            EquipmentCommandMaterialState{.item_id = 17'002, .quantity = 4},
        },
        .gold = 1'000,
    };
    EquipmentCommandLocalState enhance_warehouse_after = enhance_warehouse_before;
    enhance_warehouse_after.source_warehouse = warehouse(enhance_source, 2);
    enhance_warehouse_after.target_warehouse = warehouse(enhance_target, 3);
    enhance_warehouse_after.enhance_materials[0].quantity = 8;
    enhance_warehouse_after.enhance_materials[1].quantity = 3;
    enhance_warehouse_after.gold = 980;
    require(
        enhance_warehouse_equipment_command_action_is_valid(
            std::get<EnhanceWarehouseEquipmentCommandAction>(enhance_warehouse.action)) &&
            classify_equipment_command_state(
                enhance_warehouse,
                enhance_warehouse_before) == EquipmentCommandStateMatch::Before,
        "warehouse enhance pre-state must bind source, target, resources, and capacity");
    require(
        classify_equipment_command_state(enhance_warehouse, enhance_warehouse_after) ==
            EquipmentCommandStateMatch::After,
        "warehouse enhance post-state must move one item and consume exact resources");
    enhance_warehouse_after.enhance_materials[1].quantity = 4;
    require(
        classify_equipment_command_state(enhance_warehouse, enhance_warehouse_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "warehouse enhance must reject a partial material delta");

    EquipmentCommand enhance_new_target;
    auto enhance_new_target_action =
        std::get<EnhanceWarehouseEquipmentCommandAction>(enhance_warehouse.action);
    enhance_new_target_action.target_before.reset();
    enhance_new_target_action.target_warehouse_quantity_before = 0;
    enhance_new_target_action.equipment_capacity_before = 3;
    enhance_new_target.action = enhance_new_target_action;
    EquipmentCommandLocalState enhance_new_target_before = enhance_warehouse_before;
    enhance_new_target_before.target_warehouse = {};
    enhance_new_target_before.equipment_capacity = 3;
    EquipmentCommandLocalState enhance_new_target_after = enhance_new_target_before;
    enhance_new_target_after.source_warehouse = warehouse(enhance_source, 2);
    enhance_new_target_after.target_warehouse = warehouse(
        EquipmentSnapshot{
            .equipment_id = 902,
            .config_id = 1'001,
            .enhance_level = 1,
        },
        1);
    enhance_new_target_after.enhance_materials[0].quantity = 8;
    enhance_new_target_after.enhance_materials[1].quantity = 3;
    enhance_new_target_after.gold = 980;
    require(
        classify_equipment_command_state(enhance_new_target, enhance_new_target_before) ==
                EquipmentCommandStateMatch::Before &&
            classify_equipment_command_state(enhance_new_target, enhance_new_target_after) ==
                EquipmentCommandStateMatch::After,
        "warehouse enhance must accept a newly created target aggregate by config and level");

    auto invalid_enhance = enhance_new_target_action;
    std::swap(invalid_enhance.materials[0], invalid_enhance.materials[1]);
    enhance_new_target.action = invalid_enhance;
    require(
        !enhance_warehouse_equipment_command_action_is_valid(invalid_enhance) &&
            classify_equipment_command_state(enhance_new_target, enhance_new_target_before) ==
                EquipmentCommandStateMatch::Mismatch,
        "warehouse enhance must reject unordered material preconditions");

    EquipmentCommand enhance_ship;
    enhance_ship.action = EnhanceShipEquipmentCommandAction{
        .ship_id = 9'001,
        .slot_index = 1,
        .source_before = enhance_source,
        .target_config_id = 1'001,
        .target_enhance_level = 1,
        .materials = enhance_costs,
        .gold_before = 1'000,
        .gold_cost = 20,
        .equipment_capacity_before = 5,
        .equipment_limit_before = 300,
    };
    const EquipmentCommandLocalState enhance_ship_before{
        .target_slot = enhance_source,
        .equipment_skin_id = 0,
        .source_warehouse = {},
        .target_warehouse = {},
        .equipment_capacity = 5,
        .equipment_limit = 300,
        .compose_output_warehouse = {},
        .compose_material_quantity = 0,
        .enhance_materials = {
            EquipmentCommandMaterialState{.item_id = 17'001, .quantity = 10},
            EquipmentCommandMaterialState{.item_id = 17'002, .quantity = 4},
        },
        .gold = 1'000,
    };
    EquipmentCommandLocalState enhance_ship_after = enhance_ship_before;
    enhance_ship_after.target_slot = EquipmentSnapshot{
        .equipment_id = 903,
        .config_id = 1'001,
        .enhance_level = 1,
    };
    enhance_ship_after.enhance_materials[0].quantity = 8;
    enhance_ship_after.enhance_materials[1].quantity = 3;
    enhance_ship_after.gold = 980;
    require(
        enhance_ship_equipment_command_action_is_valid(
            std::get<EnhanceShipEquipmentCommandAction>(enhance_ship.action)) &&
            classify_equipment_command_state(enhance_ship, enhance_ship_before) ==
                EquipmentCommandStateMatch::Before,
        "ship enhance pre-state must bind the slot and exact resources");
    require(
        classify_equipment_command_state(enhance_ship, enhance_ship_after) ==
            EquipmentCommandStateMatch::After,
        "ship enhance post-state must reach the adjacent config without changing capacity");
    enhance_ship_after.target_slot->enhance_level = 2;
    require(
        classify_equipment_command_state(enhance_ship, enhance_ship_after) ==
            EquipmentCommandStateMatch::Mismatch,
        "ship enhance must reject an unexpected target level");

    std::cout << "PASS equipment_command_test\n";
    return 0;
}

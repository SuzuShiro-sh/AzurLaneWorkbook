// 声明装备穿脱、拆解、合成与强化命令的局部前检、通知派发和后态判定。

#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <optional>
#include <vector>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 仓库中单个聚合装备 ID 的对象身份和当前数量。
struct EquipmentWarehouseEntry final {
    std::optional<EquipmentSnapshot> equipment;
    std::uint64_t quantity = 0;

    bool operator==(const EquipmentWarehouseEntry&) const = default;
};

/// 强化命令局部状态中一种背包材料的当前数量。
struct EquipmentCommandMaterialState final {
    std::uint64_t item_id = 0;
    std::uint64_t quantity = 0;

    bool operator==(const EquipmentCommandMaterialState&) const = default;
};

/// 与一条装备命令有关的最小游戏状态，全部在同一个主线程停顿内读取。
struct EquipmentCommandLocalState final {
    std::optional<EquipmentSnapshot> target_slot;
    std::uint64_t equipment_skin_id = 0;
    EquipmentWarehouseEntry source_warehouse;
    EquipmentWarehouseEntry target_warehouse;
    std::uint64_t equipment_capacity = 0;
    std::uint64_t equipment_limit = 0;
    EquipmentWarehouseEntry compose_output_warehouse;
    std::uint64_t compose_material_quantity = 0;
    std::vector<EquipmentCommandMaterialState> enhance_materials;
    std::uint64_t gold = 0;

    bool operator==(const EquipmentCommandLocalState&) const = default;
};

/// 局部状态相对命令的确定分类；只有完整预期后态才能证明成功。
enum class EquipmentCommandStateMatch { Before, After, Mismatch };

/// 在线程超时与游戏主线程调用之间冻结唯一派发结果。
enum class EquipmentCommandDispatchGateState : std::uint8_t {
    Pending,
    Claimed,
    Started,
    Canceled,
};

using EquipmentCommandDispatchGate = std::atomic<EquipmentCommandDispatchGateState>;
using EquipmentCommandClock = std::chrono::steady_clock;
using EquipmentCommandDeadline = EquipmentCommandClock::time_point;

/// 超时取消只能证明调用前取消，或确认调用边界可能已经跨过。
enum class EquipmentCommandDispatchCancelResult {
    CanceledBeforeCall,
    CallMayHaveStarted,
};

/// 局部状态读取的成功数据或稳定错误。
struct EquipmentCommandStateRead final {
    bool success = false;
    EquipmentCommandLocalState state;
    AgentError error;
};

/// 官方通知调用结果；调用一旦开始，后续错误也必须视为可能已经写入。
struct EquipmentCommandDispatchExecution final {
    bool write_dispatched = false;
    AgentError error;
};

/// 在同一绝对截止时间内原子取得派发权；取消或到期先发生时返回 false。
bool begin_equipment_command_dispatch(
    EquipmentCommandDispatchGate* gate,
    EquipmentCommandDeadline deadline) noexcept;

/// 超时或关闭时尝试阻止尚未开始的调用，并返回可安全采用的最强结论。
EquipmentCommandDispatchCancelResult cancel_equipment_command_dispatch(
    EquipmentCommandDispatchGate* gate) noexcept;

/// 读取目标槽、相关仓库条目和容量，禁止从非游戏主线程调用。
EquipmentCommandStateRead read_equipment_command_state(
    const LuaApi& api,
    lua_State* state,
    const EquipmentCommand& command) noexcept;

/// 判断完整局部状态仍是发送前状态、已成为预期后态或出现其他变化。
EquipmentCommandStateMatch classify_equipment_command_state(
    const EquipmentCommand& command,
    const EquipmentCommandLocalState& state) noexcept;

/// 原子复核前态和兼容性后，经游戏自己的通知命令派发一次写入。
EquipmentCommandDispatchExecution dispatch_equipment_command(
    const LuaApi& api,
    lua_State* state,
    const EquipmentCommand& command,
    EquipmentCommandDispatchGate* dispatch_gate,
    EquipmentCommandDeadline deadline) noexcept;

}  // namespace azlw::agent

// 声明主线程任务：每种任务只携带自己的请求和结果。

#pragma once

#include <condition_variable>
#include <cstdint>
#include <memory>
#include <mutex>
#include <type_traits>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "commands/equipment_command.h"
#include "commands/equipment_command_ledger.h"
#include "protocol/protocol_types.h"
#include "snapshots/bag_snapshot.h"
#include "snapshots/equipment_config_snapshot.h"
#include "snapshots/equipment_effect_snapshot.h"
#include "snapshots/equipment_reference_snapshot.h"
#include "snapshots/owned_state_snapshot.h"
#include "snapshots/ship_catalog_snapshot.h"
#include "snapshots/ship_details_snapshot.h"

namespace azlw::agent {

/// RPC 线程与游戏主线程之间单个任务的生命周期。
enum class TaskState : std::uint8_t { Pending, Running, Completed, Canceled };

/// 只读取玩家物资和装备容量。
struct ResourcesLuaTask {
    ResourcesExecution outcome;
};

/// 只读取背包。
struct BagLuaTask {
    std::uint32_t max_items = 0;
    SnapshotExecution outcome;
};

/// 账号前窗口：同一船坞遍历读取养成和详情，并按页跨帧续办。
struct AccountBeforeLuaTask {
    AccountBeforeProgress progress;
    AccountBeforeExecution outcome;
};

struct OwnedQueryLuaTask {
    OwnedQueryProgress progress;
    OwnedQueryExecution outcome;
};

/// 在同一次主线程停顿内读取运行态。
struct OwnedStateLuaTask {
    std::uint32_t max_ships = 0;
    std::uint32_t max_equipments = 0;
    std::uint32_t max_items = 0;
    OwnedStateExecution outcome;
};

/// 读取当前客户端舰船详情。
struct ShipDetailsLuaTask {
    std::uint32_t max_ships = 0;
    std::string module_sha256;
    ShipDetailsExecution outcome;
};

/// 读取一批舰船静态配置。单帧不超过帧容量，整批不超过页容量。
struct ShipCatalogLuaTask {
    ShipCatalogBatchProgress progress;
    ShipCatalogPageExecution outcome;
};

/// 读取一批装备静态配置。单帧不超过帧容量，整批不超过页容量。
struct EquipmentConfigLuaTask {
    EquipmentConfigBatchProgress progress;
    EquipmentConfigPageExecution outcome;
};

/// 读取一批静态合成配方。单帧不超过帧容量，整批不超过页容量。
struct ComposeRecipeLuaTask {
    ComposeRecipeBatchProgress progress;
    ComposeRecipePageExecution outcome;
};

/// 按显式武器 ID 读取参数。
struct EquipmentWeaponLuaTask {
    std::vector<std::uint64_t> weapon_ids;
    std::string module_sha256;
    EquipmentWeaponBatchExecution outcome;
};

/// 按显式技能等级读取效果。
struct SkillEffectLuaTask {
    std::vector<SkillEffectQuery> skills;
    std::string module_sha256;
    SkillEffectBatchExecution outcome;
};

/// 解析装备类型、阵营、舰种和属性名称。
struct EquipmentReferenceNameLuaTask {
    std::vector<std::uint64_t> equipment_type_ids;
    std::vector<std::uint64_t> nation_ids;
    std::vector<std::uint64_t> ship_type_ids;
    std::vector<std::string> attribute_keys;
    std::string module_sha256;
    EquipmentReferenceNameBatchExecution outcome;
};

/// 派发一条已登记的装备命令。派发门不进入命令账本。
struct EquipmentCommandLuaTask {
    EquipmentCommandHandle command;
    /// 派发门单独持有，避免原子状态使具体任务无法进入 variant。
    std::shared_ptr<EquipmentCommandDispatchGate> dispatch_gate = std::make_shared<EquipmentCommandDispatchGate>(
        EquipmentCommandDispatchGateState::Pending);
    EquipmentCommandDispatchExecution outcome;
};

using LuaTaskBody = std::variant<
    ResourcesLuaTask,
    BagLuaTask,
    OwnedStateLuaTask,
    AccountBeforeLuaTask,
    OwnedQueryLuaTask,
    ShipDetailsLuaTask,
    ShipCatalogLuaTask,
    EquipmentConfigLuaTask,
    ComposeRecipeLuaTask,
    EquipmentWeaponLuaTask,
    SkillEffectLuaTask,
    EquipmentReferenceNameLuaTask,
    EquipmentCommandLuaTask>;

/// 队列可见的任务外壳。具体请求和结果只放在 body 里。
struct LuaTask {
    explicit LuaTask(LuaTaskBody requested_body) : body(std::move(requested_body)) {}

    std::mutex mutex;
    std::condition_variable condition;
    TaskState state = TaskState::Pending;
    LuaTaskBody body;
};

/// 超时诊断使用的稳定操作名。新增任务时只在这里补对应名称。
inline std::string_view lua_task_operation_name(const LuaTaskBody& body) {
    return std::visit(
        [](const auto& task) -> std::string_view {
            using Task = std::decay_t<decltype(task)>;
            namespace contract = azlw::runtime_rpc_contract;
            if constexpr (std::is_same_v<Task, ResourcesLuaTask>) {
                return contract::kRpcOperationSnapshotResources;
            } else if constexpr (std::is_same_v<Task, BagLuaTask>) {
                return contract::kRpcOperationSnapshotBag;
            } else if constexpr (std::is_same_v<Task, OwnedStateLuaTask>) {
                return contract::kRpcOperationSnapshotOwnedState;
            } else if constexpr (std::is_same_v<Task, OwnedQueryLuaTask>) {
                return contract::kRpcOperationQueryOwned;
            } else if constexpr (std::is_same_v<Task, AccountBeforeLuaTask>) {
                return contract::kRpcOperationSnapshotAccountBefore;
            } else if constexpr (std::is_same_v<Task, ShipDetailsLuaTask>) {
                return contract::kRpcOperationSnapshotShipDetails;
            } else if constexpr (std::is_same_v<Task, ShipCatalogLuaTask>) {
                return contract::kRpcOperationSnapshotShipCatalog;
            } else if constexpr (std::is_same_v<Task, EquipmentConfigLuaTask>) {
                return contract::kRpcOperationSnapshotEquipmentConfigs;
            } else if constexpr (std::is_same_v<Task, ComposeRecipeLuaTask>) {
                return contract::kRpcOperationSnapshotComposeRecipes;
            } else if constexpr (std::is_same_v<Task, EquipmentWeaponLuaTask>) {
                return contract::kRpcOperationSnapshotEquipmentWeapons;
            } else if constexpr (std::is_same_v<Task, SkillEffectLuaTask>) {
                return contract::kRpcOperationSnapshotSkillEffects;
            } else if constexpr (std::is_same_v<Task, EquipmentReferenceNameLuaTask>) {
                return contract::kRpcOperationSnapshotEquipmentReferenceNames;
            } else {
                return contract::kRpcOperationExecuteEquipmentCommand;
            }
        },
        body);
}

}  // namespace azlw::agent

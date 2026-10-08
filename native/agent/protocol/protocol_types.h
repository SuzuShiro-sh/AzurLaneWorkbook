// 定义 agent 内部使用的 RPC 请求、身份、错误和运行态快照数据结构。

#pragma once

#include <cstddef>
#include <cstdint>
#include <limits>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include "runtime_rpc_contract.h"

namespace azlw::agent {

#ifndef AZLW_AGENT_VERSION
#error "AZLW_AGENT_VERSION must be provided by the native build"
#endif

/// 冻结协议版本、帧容量、快照容量和 agent 版本。
inline constexpr std::uint32_t kProtocolVersion = runtime_rpc_contract::kProtocolVersion;
inline constexpr std::size_t kMaximumRequestBytes = runtime_rpc_contract::kMaximumRequestBytes;
inline constexpr std::size_t kMaximumResponseBytes = runtime_rpc_contract::kMaximumResponseBytes;
inline constexpr std::uint32_t kMaximumSnapshotItems =
    runtime_rpc_contract::kMaximumSnapshotItems;
inline constexpr std::uint32_t kMaximumEquipmentPageSize =
    runtime_rpc_contract::kMaximumEquipmentPageSize;
inline constexpr std::uint32_t kMaximumEquipmentFrameSize =
    runtime_rpc_contract::kMaximumEquipmentFrameSize;
inline constexpr std::uint32_t kMaximumEquipmentCatalogItems =
    runtime_rpc_contract::kMaximumEquipmentCatalogItems;
inline constexpr std::uint32_t kMaximumShipCatalogPageSize =
    runtime_rpc_contract::kMaximumShipCatalogPageSize;
inline constexpr std::uint32_t kMaximumShipCatalogFrameSize =
    runtime_rpc_contract::kMaximumShipCatalogFrameSize;
inline constexpr std::uint32_t kMaximumDockPageSize =
    runtime_rpc_contract::kMaximumDockPageSize;
inline constexpr std::uint32_t kMaximumShipCatalogItems =
    runtime_rpc_contract::kMaximumShipCatalogItems;
inline constexpr std::uint32_t kMaximumEquipmentWeaponBatchSize =
    runtime_rpc_contract::kMaximumEquipmentWeaponBatchSize;
inline constexpr std::uint32_t kMaximumSkillEffectBatchSize =
    runtime_rpc_contract::kMaximumSkillEffectBatchSize;
inline constexpr std::uint32_t kMaximumEquipmentReferenceBatchSize =
    runtime_rpc_contract::kMaximumEquipmentReferenceBatchSize;
inline constexpr std::uint32_t kShipEquipmentSlotCount =
    runtime_rpc_contract::kShipEquipmentSlotCount;
inline constexpr std::uint32_t kMaximumShipSlotEquipmentTypeCount =
    runtime_rpc_contract::kMaximumShipSlotEquipmentTypeCount;
inline constexpr std::uint32_t kMaximumShipSkillCount =
    runtime_rpc_contract::kMaximumShipSkillCount;
inline constexpr std::uint32_t kMaximumPersistentFleetCount = 32;
inline constexpr std::uint32_t kMaximumFleetTeamShipCount =
    runtime_rpc_contract::kMaximumFleetTeamShipCount;
inline constexpr std::uint32_t kMaximumShipFleetMembershipCount =
    runtime_rpc_contract::kMaximumShipFleetMembershipCount;
inline constexpr std::uint32_t kMaximumEnhanceMaterialCount =
    runtime_rpc_contract::kMaximumEnhanceMaterialCount;
inline constexpr const char* kAgentVersion = AZLW_AGENT_VERSION;

/// 已登记的 RPC 操作；未知名称保留为可响应的 Unsupported。
enum class Operation {
    Health,
    Capabilities,
    SnapshotBag,
    SnapshotResources,
    SnapshotOwnedState,
    QueryOwned,
    SnapshotShipDetails,
    SnapshotAccountBefore,
    SnapshotShipCatalog,
    SnapshotEquipmentConfigs,
    SnapshotComposeRecipes,
    SnapshotEquipmentWeapons,
    SnapshotSkillEffects,
    SnapshotEquipmentReferenceNames,
    ExecuteEquipmentCommand,
    QueryEquipmentCommand,
    CancelEquipmentCommand,
    Shutdown,
    Unsupported,
};

/// 技能效果证据按技能标识和生效等级唯一定位。
struct SkillEffectQuery final {
    std::uint64_t skill_id = 0;
    std::uint32_t level = 0;
};

/// 装备对象同时保留运行态 ID 与配置 ID，避免把聚合仓库条目误称为实例。
struct EquipmentSnapshot final {
    std::uint64_t equipment_id = 0;
    std::uint64_t config_id = 0;
    std::uint32_t enhance_level = 0;

    bool operator==(const EquipmentSnapshot&) const = default;
};

/// 装备命令当前支持的官方客户端动作。
enum class EquipmentCommandActionKind {
    Equip,
    Unequip,
    Dismantle,
    Compose,
    EnhanceWarehouse,
    EnhanceShip,
};

/// 卸下动作只绑定目标舰船槽位及该装备的仓库聚合前态。
struct UnequipEquipmentCommandAction final {
    std::uint64_t ship_id = 0;
    std::uint32_t slot_index = 0;
    EquipmentSnapshot target_before;
    std::uint64_t target_warehouse_quantity_before = 0;
    std::uint64_t equipment_capacity_before = 0;
    std::uint64_t equipment_limit_before = 0;

    bool operator==(const UnequipEquipmentCommandAction&) const = default;
};

/// 装上动作绑定目标槽、仓库来源和可能被替换装备的完整局部前态。
struct EquipEquipmentCommandAction final {
    std::uint64_t ship_id = 0;
    std::uint32_t slot_index = 0;
    std::optional<EquipmentSnapshot> target_before;
    EquipmentSnapshot source_before;
    std::uint64_t source_quantity_before = 0;
    std::uint64_t target_warehouse_quantity_before = 0;
    std::uint64_t equipment_capacity_before = 0;
    std::uint64_t equipment_limit_before = 0;

    bool operator==(const EquipEquipmentCommandAction&) const = default;
};

/// 拆解动作只绑定仓库来源、拆解数量和仓库容量前态。
struct DismantleEquipmentCommandAction final {
    EquipmentSnapshot source_before;
    std::uint64_t source_quantity_before = 0;
    std::uint64_t dismantle_quantity = 0;
    std::uint64_t equipment_capacity_before = 0;
    std::uint64_t equipment_limit_before = 0;

    bool operator==(const DismantleEquipmentCommandAction&) const = default;
};

/// 合成动作绑定配方、产物、材料、物资和仓库容量的同一时刻前态。
struct ComposeEquipmentCommandAction final {
    std::uint64_t recipe_id = 0;
    std::uint64_t compose_quantity = 0;
    std::uint64_t output_config_id = 0;
    std::optional<EquipmentSnapshot> output_before;
    std::uint64_t output_quantity_before = 0;
    std::uint64_t material_id = 0;
    std::uint64_t material_quantity_before = 0;
    std::uint64_t material_quantity_per_unit = 0;
    std::uint64_t gold_before = 0;
    std::uint64_t gold_per_unit = 0;
    std::uint64_t equipment_capacity_before = 0;
    std::uint64_t equipment_limit_before = 0;

    bool operator==(const ComposeEquipmentCommandAction&) const = default;
};

/// 单级强化消耗的一种背包材料及其发送前数量。
struct EquipmentCommandMaterialCost final {
    std::uint64_t item_id = 0;
    std::uint64_t quantity_before = 0;
    std::uint64_t cost = 0;

    bool operator==(const EquipmentCommandMaterialCost&) const = default;
};

/// 仓库单级强化绑定来源、目标聚合、资源和容量的同一时刻前态。
struct EnhanceWarehouseEquipmentCommandAction final {
    EquipmentSnapshot source_before;
    std::uint64_t source_quantity_before = 0;
    std::uint64_t target_config_id = 0;
    std::uint32_t target_enhance_level = 0;
    std::optional<EquipmentSnapshot> target_before;
    std::uint64_t target_warehouse_quantity_before = 0;
    std::vector<EquipmentCommandMaterialCost> materials;
    std::uint64_t gold_before = 0;
    std::uint64_t gold_cost = 0;
    std::uint64_t equipment_capacity_before = 0;
    std::uint64_t equipment_limit_before = 0;

    bool operator==(const EnhanceWarehouseEquipmentCommandAction&) const = default;
};

/// 舰上单级强化绑定槽位装备、资源和容量的同一时刻前态。
struct EnhanceShipEquipmentCommandAction final {
    std::uint64_t ship_id = 0;
    std::uint32_t slot_index = 0;
    EquipmentSnapshot source_before;
    std::uint64_t target_config_id = 0;
    std::uint32_t target_enhance_level = 0;
    std::vector<EquipmentCommandMaterialCost> materials;
    std::uint64_t gold_before = 0;
    std::uint64_t gold_cost = 0;
    std::uint64_t equipment_capacity_before = 0;
    std::uint64_t equipment_limit_before = 0;

    bool operator==(const EnhanceShipEquipmentCommandAction&) const = default;
};

/// 校验拆解动作的身份、数量和容量不变量，供协议入口与内部调用共同复用。
inline bool dismantle_equipment_command_action_is_valid(
    const DismantleEquipmentCommandAction& action) noexcept {
    return action.source_before.equipment_id != 0 && action.source_before.config_id != 0 &&
           action.source_before.enhance_level == 0 && action.source_quantity_before != 0 &&
           action.dismantle_quantity != 0 &&
           action.dismantle_quantity <= action.source_quantity_before &&
           action.source_quantity_before <= action.equipment_capacity_before &&
           action.equipment_limit_before != 0 &&
           action.equipment_capacity_before <= action.equipment_limit_before;
}

/// 校验合成动作的配方、资源乘积和可观察产物不变量。
inline bool compose_equipment_command_action_is_valid(
    const ComposeEquipmentCommandAction& action) noexcept {
    const bool output_before_valid =
        action.output_before.has_value()
            ? action.output_quantity_before != 0 &&
                  action.output_before->equipment_id != 0 &&
                  action.output_before->config_id == action.output_config_id &&
                  action.output_before->enhance_level == 0
            : action.output_quantity_before == 0;
    return action.recipe_id != 0 && action.compose_quantity != 0 &&
           action.output_config_id != 0 && output_before_valid && action.material_id != 0 &&
           action.material_quantity_per_unit != 0 && action.equipment_limit_before != 0 &&
           action.equipment_capacity_before <= action.equipment_limit_before &&
           action.output_quantity_before <= action.equipment_capacity_before &&
           action.compose_quantity <=
               action.equipment_limit_before - action.equipment_capacity_before &&
           action.material_quantity_per_unit <=
               action.material_quantity_before / action.compose_quantity &&
           (action.gold_per_unit == 0 ||
            action.gold_per_unit <= action.gold_before / action.compose_quantity);
}

/// 校验两种强化位置共用的单级配置跃迁、资源和容量前态。
template <typename Action>
inline bool enhance_equipment_command_common_is_valid(const Action& action) noexcept {
    if (action.source_before.equipment_id == 0 || action.source_before.config_id == 0 ||
        action.target_config_id == 0 ||
        action.source_before.config_id == action.target_config_id ||
        action.source_before.enhance_level == std::numeric_limits<std::uint32_t>::max() ||
        action.target_enhance_level != action.source_before.enhance_level + 1 ||
        action.gold_cost > action.gold_before || action.equipment_limit_before == 0 ||
        action.equipment_capacity_before > action.equipment_limit_before) {
        return false;
    }
    if (action.materials.size() > kMaximumEnhanceMaterialCount) {
        return false;
    }
    std::uint64_t previous_item_id = 0;
    for (const EquipmentCommandMaterialCost& material : action.materials) {
        if (material.item_id == 0 || material.item_id <= previous_item_id || material.cost == 0 ||
            material.cost > material.quantity_before) {
            return false;
        }
        previous_item_id = material.item_id;
    }
    return true;
}

/// 校验仓库强化的来源、目标聚合和单级资源不变量。
inline bool enhance_warehouse_equipment_command_action_is_valid(
    const EnhanceWarehouseEquipmentCommandAction& action) noexcept {
    const bool target_before_valid = action.target_before.has_value()
                                         ? action.target_warehouse_quantity_before != 0 &&
                                               action.target_before->equipment_id != 0 &&
                                               action.target_before->config_id ==
                                                   action.target_config_id &&
                                               action.target_before->enhance_level ==
                                                   action.target_enhance_level &&
                                               action.target_before->equipment_id !=
                                                   action.source_before.equipment_id
                                         : action.target_warehouse_quantity_before == 0;
    return enhance_equipment_command_common_is_valid(action) && target_before_valid &&
           action.source_quantity_before != 0 &&
           action.source_quantity_before <= action.equipment_capacity_before &&
           action.target_warehouse_quantity_before <=
               action.equipment_capacity_before - action.source_quantity_before;
}

/// 校验舰上强化的槽位、单级配置跃迁和资源不变量。
inline bool enhance_ship_equipment_command_action_is_valid(
    const EnhanceShipEquipmentCommandAction& action) noexcept {
    return enhance_equipment_command_common_is_valid(action) && action.ship_id != 0 &&
           action.slot_index >= 1 && action.slot_index <= kShipEquipmentSlotCount;
}

using EquipmentCommandAction = std::variant<
    UnequipEquipmentCommandAction,
    EquipEquipmentCommandAction,
    DismantleEquipmentCommandAction,
    ComposeEquipmentCommandAction,
    EnhanceWarehouseEquipmentCommandAction,
    EnhanceShipEquipmentCommandAction>;

/// 返回强类型动作对应的稳定命令类别。
inline EquipmentCommandActionKind equipment_command_action_kind(
    const EquipmentCommandAction& action) noexcept {
    if (std::holds_alternative<EquipEquipmentCommandAction>(action)) {
        return EquipmentCommandActionKind::Equip;
    }
    if (std::holds_alternative<DismantleEquipmentCommandAction>(action)) {
        return EquipmentCommandActionKind::Dismantle;
    }
    if (std::holds_alternative<ComposeEquipmentCommandAction>(action)) {
        return EquipmentCommandActionKind::Compose;
    }
    if (std::holds_alternative<EnhanceWarehouseEquipmentCommandAction>(action)) {
        return EquipmentCommandActionKind::EnhanceWarehouse;
    }
    if (std::holds_alternative<EnhanceShipEquipmentCommandAction>(action)) {
        return EquipmentCommandActionKind::EnhanceShip;
    }
    return EquipmentCommandActionKind::Unequip;
}

/// 绑定完整宿主前态摘要和设备端局部原子前置条件的单条装备命令。
struct EquipmentCommand final {
    std::uint32_t schema_version = 0;
    std::string command_id;
    std::string target_fingerprint_sha256;
    std::string plan_hash;
    std::uint32_t sequence = 0;
    std::string pre_state_content_sha256;
    EquipmentCommandAction action = UnequipEquipmentCommandAction{};

    bool operator==(const EquipmentCommand&) const = default;
};

/// 写入请求对原命令的可确认状态，不把通知派发成功冒充游戏写入成功。
enum class EquipmentCommandStatus { Success, Failed, Unknown };

/// 原命令当前拥有的最强设备端证据。
enum class EquipmentCommandPhase { Observing, Succeeded, Failed, Uncertain };

/// execute、query 和 cancel 共用的原命令状态收据。
struct EquipmentCommandReceipt final {
    std::uint32_t schema_version = 1;
    std::string command_id;
    EquipmentCommandStatus status = EquipmentCommandStatus::Unknown;
    EquipmentCommandPhase phase = EquipmentCommandPhase::Observing;
    bool write_dispatched = false;
    bool cancel_requested = false;
    std::uint32_t observation_count = 0;
    std::optional<std::string> error_code;
    std::optional<std::string> message;
};

/// 健康检查没有业务参数；顶层 timeout_ms 只做协议校验。
struct HealthRpcRequest final {};

/// 能力查询没有业务参数。
struct CapabilitiesRpcRequest final {};

/// 背包快照只携带本次读取上限和运行态超时。
struct SnapshotBagRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::uint32_t max_items = 0;
};

/// 玩家资源读取只需要主线程任务超时。
struct SnapshotResourcesRpcRequest final {
    std::uint32_t timeout_ms = 0;
};

/// 完整运行态快照分别限制舰船、仓库装备和背包条目。
struct SnapshotOwnedStateRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::uint32_t max_ships = 0;
    std::uint32_t max_equipments = 0;
    std::uint32_t max_items = 0;
};

/// 按实例或配置标识查询持有对象；空标识列表枚举全部，空字段列表采用默认字段。
struct OwnedQuery final {
    std::string kind;
    std::vector<std::uint64_t> ids;
    std::vector<std::string> fields;
};
struct QueryOwnedRpcRequest final {
    std::uint32_t timeout_ms = 0;
    OwnedQuery query;
};

/// 舰船详情快照只限制本次舰船数量。
struct SnapshotShipDetailsRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::uint32_t max_ships = 0;
};

/// 账号前窗口一次读取船坞养成和舰船详情，上限与完整运行态相同。
struct SnapshotAccountBeforeRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::uint32_t max_ships = 0;
    std::uint32_t max_equipments = 0;
    std::uint32_t max_items = 0;
};

/// 舰船静态目录页绑定白名单表和分页。
struct SnapshotShipCatalogRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::string table_key;
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
};

/// 装备配置请求选择目录分页或显式 ID 批次。
struct SnapshotEquipmentConfigsRpcRequest final {
    // 空数组表示目录分页；显式选择必须非空。
    std::vector<std::uint64_t> ids;
    std::uint32_t timeout_ms = 0;
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
};

/// 合成配方页只携带分页。
struct SnapshotComposeRecipesRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
};

/// 武器参数批次只携带升序武器 ID。
struct SnapshotEquipmentWeaponsRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::vector<std::uint64_t> weapon_ids;
};

/// 技能效果批次只携带技能和等级。
struct SnapshotSkillEffectsRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::vector<SkillEffectQuery> skills;
};

/// 装备引用名称批次按四个命名空间携带键。
struct SnapshotEquipmentReferenceNamesRpcRequest final {
    std::uint32_t timeout_ms = 0;
    std::vector<std::uint64_t> equipment_type_ids;
    std::vector<std::uint64_t> nation_ids;
    std::vector<std::uint64_t> ship_type_ids;
    std::vector<std::string> attribute_keys;
};

/// 执行请求携带完整命令和本次运行态超时。
struct ExecuteEquipmentCommandRpcRequest final {
    std::uint32_t timeout_ms = 0;
    EquipmentCommand command;
};

/// 查询只按原命令标识读取已有收据。
struct QueryEquipmentCommandRpcRequest final {
    std::string command_id;
};

/// 取消只按原命令标识阻止尚未开始的写入。
struct CancelEquipmentCommandRpcRequest final {
    std::string command_id;
};

/// 关闭准备只携带排空超时。
struct ShutdownRpcRequest final {
    std::uint32_t timeout_ms = 0;
};

/// 未知操作保留原始名称，以便返回可定位的 unsupported_operation。
struct UnsupportedRpcRequest final {
    std::string raw_operation;
};

/// 已解析请求的具体载荷；公共信封不存放其他操作的默认字段。
using RpcOperationBody = std::variant<
    HealthRpcRequest,
    CapabilitiesRpcRequest,
    SnapshotBagRpcRequest,
    SnapshotResourcesRpcRequest,
    SnapshotOwnedStateRpcRequest,
    QueryOwnedRpcRequest,
    SnapshotShipDetailsRpcRequest,
    SnapshotAccountBeforeRpcRequest,
    SnapshotShipCatalogRpcRequest,
    SnapshotEquipmentConfigsRpcRequest,
    SnapshotComposeRecipesRpcRequest,
    SnapshotEquipmentWeaponsRpcRequest,
    SnapshotSkillEffectsRpcRequest,
    SnapshotEquipmentReferenceNamesRpcRequest,
    ExecuteEquipmentCommandRpcRequest,
    QueryEquipmentCommandRpcRequest,
    CancelEquipmentCommandRpcRequest,
    ShutdownRpcRequest,
    UnsupportedRpcRequest>;

/// 严格解析并规范化后的 RPC 请求。
struct RpcRequest final {
    std::string request_id;
    std::uint64_t request_number = 0;
    RpcOperationBody body{UnsupportedRpcRequest{}};
};

/// 从具体载荷得到稳定操作分类。未知名称仍是 Unsupported。
[[nodiscard]] inline Operation operation_of(const RpcRequest& request) noexcept {
    if (std::holds_alternative<HealthRpcRequest>(request.body)) {
        return Operation::Health;
    }
    if (std::holds_alternative<CapabilitiesRpcRequest>(request.body)) {
        return Operation::Capabilities;
    }
    if (std::holds_alternative<SnapshotBagRpcRequest>(request.body)) {
        return Operation::SnapshotBag;
    }
    if (std::holds_alternative<QueryOwnedRpcRequest>(request.body)) {
        return Operation::QueryOwned;
    }
    if (std::holds_alternative<SnapshotResourcesRpcRequest>(request.body)) {
        return Operation::SnapshotResources;
    }
    if (std::holds_alternative<SnapshotOwnedStateRpcRequest>(request.body)) {
        return Operation::SnapshotOwnedState;
    }
    if (std::holds_alternative<SnapshotShipDetailsRpcRequest>(request.body)) {
        return Operation::SnapshotShipDetails;
    }
    if (std::holds_alternative<SnapshotAccountBeforeRpcRequest>(request.body)) {
        return Operation::SnapshotAccountBefore;
    }
    if (std::holds_alternative<SnapshotShipCatalogRpcRequest>(request.body)) {
        return Operation::SnapshotShipCatalog;
    }
    if (std::holds_alternative<SnapshotEquipmentConfigsRpcRequest>(request.body)) {
        return Operation::SnapshotEquipmentConfigs;
    }
    if (std::holds_alternative<SnapshotComposeRecipesRpcRequest>(request.body)) {
        return Operation::SnapshotComposeRecipes;
    }
    if (std::holds_alternative<SnapshotEquipmentWeaponsRpcRequest>(request.body)) {
        return Operation::SnapshotEquipmentWeapons;
    }
    if (std::holds_alternative<SnapshotSkillEffectsRpcRequest>(request.body)) {
        return Operation::SnapshotSkillEffects;
    }
    if (std::holds_alternative<SnapshotEquipmentReferenceNamesRpcRequest>(request.body)) {
        return Operation::SnapshotEquipmentReferenceNames;
    }
    if (std::holds_alternative<ExecuteEquipmentCommandRpcRequest>(request.body)) {
        return Operation::ExecuteEquipmentCommand;
    }
    if (std::holds_alternative<QueryEquipmentCommandRpcRequest>(request.body)) {
        return Operation::QueryEquipmentCommand;
    }
    if (std::holds_alternative<CancelEquipmentCommandRpcRequest>(request.body)) {
        return Operation::CancelEquipmentCommand;
    }
    if (std::holds_alternative<ShutdownRpcRequest>(request.body)) {
        return Operation::Shutdown;
    }
    return Operation::Unsupported;
}

/// 可在线上稳定分类的 agent 错误。
struct AgentError final {
    std::string code;
    std::string stage;
    std::string message;
    std::string retry = "never";
    std::string session_effect = "unchanged";
};

/// 与当前一次性会话和目标进程绑定的 agent 身份。
struct AgentIdentity final {
    std::string session_id;
    std::int32_t process_id = 0;
    std::string package_name;
};

/// Agent 排空任务后交给宿主的严格卸载前收据。
struct ShutdownPreparation final {
    bool success = false;
    AgentError error;
    std::int32_t worker_tid = 0;
    std::uint64_t worker_start_time = 0;
    std::uint64_t hook_target = 0;
    std::uint64_t trampoline_start = 0;
    std::uint64_t trampoline_size = 0;
};

/// 背包物品的可选合成配方。
struct ComposeRecipe final {
    std::uint64_t recipe_id = 0;
    std::uint64_t material_id = 0;
    std::uint64_t material_count = 0;
    std::uint64_t gold = 0;
    std::optional<std::uint64_t> equipment_id;
    std::optional<std::uint64_t> max_count;
};

/// 已完整读取并通过边界校验的背包条目。
struct BagItem final {
    std::uint64_t item_id = 0;
    std::uint64_t quantity = 0;
    std::string resolved_name;
    std::optional<ComposeRecipe> compose_recipe;
};

/// 单个原始条目未能可靠转换时的定位信息。
struct ReadError final {
    std::optional<std::uint64_t> item_id;
    std::string code;
    std::string message;
};

/// 带完整性声明和逐条错误的背包快照。
struct BagSnapshot final {
    bool complete = false;
    bool truncated = false;
    std::vector<BagItem> items;
    std::vector<ReadError> read_errors;
};

/// 舰船固定槽位及其可空装备。
struct ShipEquipmentSlot final {
    std::uint32_t slot_index = 0;
    std::optional<EquipmentSnapshot> equipment;
};

/// 舰船自身技能的运行态学习进度，不包含装备或触发技能。
struct OwnedShipSkill final {
    std::uint64_t skill_id = 0;
    std::uint32_t level = 0;
    std::uint64_t experience = 0;
};

/// 一艘舰船在持久编队中的队伍和位置。
struct ShipFleetMembership final {
    std::uint32_t fleet_id = 0;
    std::optional<std::string> display_name;
    std::string kind;
    std::string team;
    std::uint32_t position = 0;
};

/// 舰船实例的稳定养成字段、持久编队、已学习技能和固定五个装备槽位。
struct OwnedShip final {
    std::uint64_t ship_id = 0;
    std::uint64_t config_id = 0;
    std::uint32_t level = 0;
    std::uint64_t experience_in_level = 0;
    std::uint64_t intimacy_raw = 0;
    std::uint64_t energy = 0;
    std::uint64_t proficiency = 0;
    std::vector<ShipFleetMembership> fleet_memberships;
    std::vector<OwnedShipSkill> skills;
    std::vector<ShipEquipmentSlot> slots;
};

/// 单艘舰船或单个槽位读取失败时的定位信息。
struct ShipReadError final {
    std::optional<std::uint64_t> ship_id;
    std::optional<std::uint64_t> skill_id;
    std::optional<std::uint32_t> slot_index;
    std::string code;
    std::string message;
};

/// 带完整性声明和逐项错误的船坞快照。
struct DockSnapshot final {
    bool complete = false;
    bool truncated = false;
    std::vector<OwnedShip> ships;
    std::vector<ShipReadError> read_errors;
};

/// 仓库中按装备配置聚合计数的条目。
struct WarehouseEquipment final {
    EquipmentSnapshot equipment;
    std::uint64_t quantity = 0;
};

/// 单个仓库条目读取失败时的定位信息。
struct EquipmentReadError final {
    std::optional<std::uint64_t> equipment_id;
    std::string code;
    std::string message;
};

/// 带完整性声明和逐项错误的装备仓库快照。
struct WarehouseSnapshot final {
    bool complete = false;
    bool truncated = false;
    std::vector<WarehouseEquipment> items;
    std::vector<EquipmentReadError> read_errors;
};

/// 影响配装、合成和强化决策的玩家资源与仓库容量。
struct PlayerResources final {
    std::uint64_t gold = 0;
    std::uint64_t equipment_capacity = 0;
    std::uint64_t equipment_limit = 0;
};

/// 在同一次游戏主线程停顿内取得的完整运行态视图。
struct OwnedStateSnapshot final {
    bool complete = false;
    DockSnapshot dock;
    WarehouseSnapshot warehouse;
    BagSnapshot bag;
    PlayerResources player;
};

/// 受当前 loader 验证的 Lua 模块身份；它只证明运行目标，不冒充完整数据版本。
struct ShipDetailSource final {
    std::string module_sha256;
};

/// 舰船面板需要的绝对属性阶段；差值由宿主映射器计算并校验。
struct ShipAttributeSet final {
    double durability = 0.0;
    double cannon = 0.0;
    double torpedo = 0.0;
    double anti_aircraft = 0.0;
    double air = 0.0;
    double reload = 0.0;
    double hit = 0.0;
    double dodge = 0.0;
    double anti_sub = 0.0;
    double luck = 0.0;
    double speed = 0.0;
};

/// 舰种、装甲、阵营和稀有度等客户端展示分类。
struct ShipClassification final {
    std::uint64_t group_id = 0;
    std::uint64_t ship_type_id = 0;
    std::string ship_type_name;
    std::uint64_t armor_type_id = 0;
    std::string armor_type_name;
    std::uint64_t nation_id = 0;
    std::string nation_name;
    std::uint32_t rarity = 0;
    std::uint32_t star = 0;
    std::uint32_t max_star = 0;
    std::uint64_t skin_id = 0;
};

/// 当前舰船实例上的已学习技能及客户端解析后的展示配置。
struct ShipSkillDetail final {
    std::uint64_t skill_id = 0;
    std::uint64_t effective_skill_id = 0;
    std::string name;
    std::uint32_t level = 0;
    std::uint32_t max_level = 0;
    std::uint64_t experience = 0;
    std::uint64_t next_level_experience = 0;
    std::string description_template;
    std::string current_effect;
};

/// 舰船单个装备槽位及客户端配置允许的装备类型集合。
struct ShipEquipmentSlotRule final {
    std::uint32_t slot_index = 0;
    std::vector<std::uint64_t> allowed_equipment_type_ids;
};

/// 单艘舰船由客户端方法解析出的分类、成长、展示和属性阶段。
struct ShipDetail final {
    std::uint64_t ship_id = 0;
    std::uint64_t config_id = 0;
    std::string name;
    std::uint32_t level = 0;
    std::uint32_t max_level = 0;
    std::uint64_t experience_in_level = 0;
    std::uint64_t total_experience = 0;
    std::uint64_t next_level_experience = 0;
    std::uint64_t intimacy_raw = 0;
    std::uint64_t intimacy_maximum = 0;
    std::uint64_t intimacy_stage_id = 0;
    std::string intimacy_stage_description;
    bool proposed = false;
    std::uint64_t propose_time = 0;
    std::uint64_t create_time = 0;
    std::uint64_t combat_power = 0;
    bool locked = false;
    std::uint64_t oil_cost_start = 0;
    std::uint64_t oil_cost_end = 0;
    std::uint64_t oil_cost_total = 0;
    ShipClassification classification;
    ShipAttributeSet base_attributes;
    ShipAttributeSet equipment_applied_attributes;
    ShipAttributeSet effective_attributes;
    std::vector<ShipEquipmentSlotRule> slot_rules;
    std::vector<ShipSkillDetail> skills;
};

/// 单艘舰船或技能详情读取失败时的稳定定位信息。
struct ShipDetailReadError final {
    std::optional<std::uint64_t> ship_id;
    std::optional<std::uint64_t> skill_id;
    std::string code;
    std::string message;
};

/// 受限数量、确定性排序并携带当前模块身份的舰船详情快照。
struct ShipDetailsSnapshot final {
    bool complete = false;
    bool truncated = false;
    ShipDetailSource source;
    std::vector<ShipDetail> ships;
    std::vector<ShipDetailReadError> read_errors;
};

}  // namespace azlw::agent

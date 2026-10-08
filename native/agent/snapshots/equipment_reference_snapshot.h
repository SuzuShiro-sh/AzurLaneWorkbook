// 声明装备分类与属性显示名称的有界批量只读契约。

#pragma once

#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "equipment_config_snapshot.h"
#include "lua/lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 数值分类标识对应的当前客户端显示名称或明确读取错误。
struct EquipmentReferenceName final {
    std::uint64_t identifier = 0;
    std::optional<std::string> name;
    std::optional<std::string> error;
};

/// 属性稳定键对应的当前客户端显示名称或明确读取错误。
struct EquipmentAttributeName final {
    std::string key;
    std::optional<std::string> name;
    std::optional<std::string> error;
};

/// 四个名称命名空间分别保持请求顺序，避免宿主依赖 JSON 对象键顺序。
struct EquipmentReferenceNameBatch final {
    bool complete = false;
    EquipmentConfigSource source;
    std::vector<EquipmentReferenceName> equipment_types;
    std::vector<EquipmentReferenceName> nations;
    std::vector<EquipmentReferenceName> ship_types;
    std::vector<EquipmentAttributeName> attributes;
};

struct EquipmentReferenceNameBatchExecution final {
    bool success = false;
    EquipmentReferenceNameBatch batch;
    AgentError error;
};

/// 必须由 `tolua_update` 所在线程调用，按显式去重键解析四类显示名称。
EquipmentReferenceNameBatchExecution snapshot_equipment_reference_names(
    const LuaApi& api,
    lua_State* state,
    std::span<const std::uint64_t> equipment_type_ids,
    std::span<const std::uint64_t> nation_ids,
    std::span<const std::uint64_t> ship_type_ids,
    std::span<const std::string> attribute_keys,
    std::string_view module_sha256) noexcept;

}  // namespace azlw::agent

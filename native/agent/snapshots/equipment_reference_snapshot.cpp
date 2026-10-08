// 实现装备分类与属性显示名称的确定性批量解析。

#include "equipment_reference_snapshot.h"

#include <exception>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>

#include "lua/lua_reader.h"

namespace azlw::agent {
namespace {

/// 单条名称失败保留在记录内，使宿主能报告准确命名空间和键。
EquipmentReferenceName read_numeric_name(
    const LuaApi& api,
    lua_State* state,
    const char* table_name,
    const char* function_name,
    std::uint64_t identifier) {
    EquipmentReferenceName record;
    record.identifier = identifier;
    std::string error;
    record.name = read_lua_static_name(
        api, state, table_name, function_name, identifier, &error);
    if (!record.name.has_value()) {
        record.error = std::move(error);
    }
    return record;
}

/// 这些内部舰种不属于玩家舰种表，但装备限制会引用其稳定数值标识。
bool has_enemy_ship_type_name(std::uint64_t identifier) {
    return identifier == 14 || identifier == 15 || identifier == 16;
}

/// 玩家舰种表不含三个内部类型，仅对这些标识读取同一客户端的敌方舰种配置。
EquipmentReferenceName read_ship_type_name(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t identifier) {
    EquipmentReferenceName record =
        read_numeric_name(api, state, "ShipType", "Type2Name", identifier);
    if (record.name.has_value() || !has_enemy_ship_type_name(identifier)) {
        return record;
    }

    const std::string primary_error = record.error.value_or("ShipType.Type2Name 未返回名称");
    std::string fallback_error;
    record.name = read_lua_pg_config_text(
        api,
        state,
        "enemy_data_by_type",
        identifier,
        "type_name",
        &fallback_error);
    if (record.name.has_value()) {
        record.error.reset();
    } else {
        record.error = primary_error + "; 敌方舰种配置回退失败: " + fallback_error;
    }
    return record;
}

EquipmentAttributeName read_attribute_name(
    const LuaApi& api,
    lua_State* state,
    const std::string& key) {
    EquipmentAttributeName record;
    record.key = key;
    std::string error;
    record.name = read_lua_static_name(
        api, state, "AttributeType", "Type2Name", key.c_str(), &error);
    if (!record.name.has_value()) {
        record.error = std::move(error);
    }
    return record;
}

template <typename Record>
bool records_complete(std::span<const Record> records) {
    for (const Record& record : records) {
        if (!record.name.has_value() || record.error.has_value()) {
            return false;
        }
    }
    return true;
}

AgentError snapshot_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.lua",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

}  // namespace

EquipmentReferenceNameBatchExecution snapshot_equipment_reference_names(
    const LuaApi& api,
    lua_State* state,
    std::span<const std::uint64_t> equipment_type_ids,
    std::span<const std::uint64_t> nation_ids,
    std::span<const std::uint64_t> ship_type_ids,
    std::span<const std::string> attribute_keys,
    std::string_view module_sha256) noexcept {
    EquipmentReferenceNameBatchExecution execution;
    execution.batch.source.module_sha256.assign(module_sha256);
    const LuaStackGuard guard(api, state);
    try {
        execution.batch.equipment_types.reserve(equipment_type_ids.size());
        for (const std::uint64_t identifier : equipment_type_ids) {
            execution.batch.equipment_types.push_back(
                read_numeric_name(api, state, "EquipType", "Type2Name", identifier));
        }
        execution.batch.nations.reserve(nation_ids.size());
        for (const std::uint64_t identifier : nation_ids) {
            execution.batch.nations.push_back(
                read_numeric_name(api, state, "Nation", "Nation2Name", identifier));
        }
        execution.batch.ship_types.reserve(ship_type_ids.size());
        for (const std::uint64_t identifier : ship_type_ids) {
            execution.batch.ship_types.push_back(
                read_ship_type_name(api, state, identifier));
        }
        execution.batch.attributes.reserve(attribute_keys.size());
        for (const std::string& key : attribute_keys) {
            execution.batch.attributes.push_back(read_attribute_name(api, state, key));
        }

        const bool has_requested_name = !equipment_type_ids.empty() || !nation_ids.empty() ||
                                        !ship_type_ids.empty() || !attribute_keys.empty();
        execution.batch.complete =
            has_requested_name &&
            records_complete<EquipmentReferenceName>(execution.batch.equipment_types) &&
            records_complete<EquipmentReferenceName>(execution.batch.nations) &&
            records_complete<EquipmentReferenceName>(execution.batch.ship_types) &&
            records_complete<EquipmentAttributeName>(execution.batch.attributes);
        execution.success = true;
    } catch (const std::exception& error) {
        execution.error = snapshot_error(
            "equipment_reference_exception",
            std::string("解析装备引用名称时发生异常: ") + error.what());
    } catch (...) {
        execution.error = snapshot_error(
            "equipment_reference_exception", "解析装备引用名称时发生未知异常");
    }
    return execution;
}

}  // namespace azlw::agent

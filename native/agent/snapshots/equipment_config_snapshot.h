// 声明装备静态配置与合成配方的分页只读采集契约。

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "lua/lua_api.h"
#include "lua/lua_value.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 静态配置来源绑定当前已验证的 Lua 模块，分页之间可据此拒绝混用数据。
struct EquipmentConfigSource final {
    std::string module_sha256;
};

/// 客户端 Equipment 对象解析出的完整配置和派生信息。
struct EquipmentConfigRecord final {
    std::uint64_t config_id = 0;
    std::optional<std::uint64_t> root_config_id;
    LuaValue raw_config;
    LuaValue attributes;
    LuaValue properties;
    LuaValue skill;
    LuaValue property_rate;
    std::vector<std::uint64_t> weapon_ids;
    std::optional<std::uint64_t> gear_score;
    std::optional<double> anti_siren_power;
    std::optional<bool> is_device;
    std::optional<bool> is_aircraft;
    bool complete = false;
    std::vector<std::string> read_errors;
};

/// 目录索引或单条配置无法可靠读取时的稳定定位信息。
struct EquipmentConfigReadError final {
    std::optional<std::uint32_t> catalog_index;
    std::optional<std::uint64_t> config_id;
    std::string code;
    std::string message;
};

/// 装备配置结果；目录分页按 all 顺序，显式选择按请求 ID 顺序。
struct EquipmentConfigPage final {
    bool selected_ids = false;
    std::vector<std::uint64_t> missing_ids;
    bool complete = false;
    EquipmentConfigSource source;
    std::uint32_t start_index = 0;
    std::uint32_t total_count = 0;
    std::optional<std::uint32_t> next_index;
    std::vector<EquipmentConfigRecord> configs;
    std::vector<EquipmentConfigReadError> read_errors;
};

/// 合成配方只描述静态成本，不携带依赖玩家背包数量的可合成次数。
struct EquipmentComposeRecipe final {
    std::uint64_t recipe_id = 0;
    std::uint64_t material_id = 0;
    std::uint64_t material_count = 0;
    std::uint64_t gold = 0;
    std::uint64_t equipment_id = 0;
};

/// 配方索引或字段失败时保留页内位置，便于宿主精确报告不完整数据。
struct ComposeRecipeReadError final {
    std::optional<std::uint32_t> catalog_index;
    std::optional<std::uint64_t> recipe_id;
    std::string code;
    std::string message;
};

/// 以 `compose_data_template.all` 为唯一顺序来源的合成配方页。
struct ComposeRecipePage final {
    bool complete = false;
    EquipmentConfigSource source;
    std::uint32_t start_index = 0;
    std::uint32_t total_count = 0;
    std::optional<std::uint32_t> next_index;
    std::vector<EquipmentComposeRecipe> recipes;
    std::vector<ComposeRecipeReadError> read_errors;
};

/// 装备配置分页任务的成功数据或稳定错误。
struct EquipmentConfigPageExecution final {
    bool success = false;
    EquipmentConfigPage page;
    AgentError error;
};

/// 合成配方分页任务的成功数据或稳定错误。
struct ComposeRecipePageExecution final {
    bool success = false;
    ComposeRecipePage page;
    AgentError error;
};

enum class EquipmentBatchStep : std::uint8_t { Continue, Finished };

/// 装备配置响应在帧之间保留的游标。单帧不超过帧容量。
struct EquipmentConfigBatchProgress final {
    std::vector<std::uint64_t> ids;
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
    std::string module_sha256;
    std::uint32_t cursor = 0;
    bool started = false;
    EquipmentConfigPageExecution outcome;
};

/// 合成配方响应在帧之间保留的游标。单帧不超过帧容量。
struct ComposeRecipeBatchProgress final {
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
    std::string module_sha256;
    std::uint32_t cursor = 0;
    bool started = false;
    ComposeRecipePageExecution outcome;
};

/// 推进一帧。返回 Continue 时调用方保持任务。
EquipmentBatchStep advance_equipment_config_batch(
    const LuaApi& api,
    lua_State* state,
    EquipmentConfigBatchProgress* progress) noexcept;

/// 推进一帧。返回 Continue 时调用方保持任务。
EquipmentBatchStep advance_compose_recipe_batch(
    const LuaApi& api,
    lua_State* state,
    ComposeRecipeBatchProgress* progress) noexcept;

/// 必须由 `tolua_update` 所在线程调用，读取一帧装备静态配置。
EquipmentConfigPageExecution snapshot_equipment_configs(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::string_view module_sha256) noexcept;

/// 必须由 `tolua_update` 所在线程调用，读取一页静态合成配方。
ComposeRecipePageExecution snapshot_compose_recipes(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::string_view module_sha256) noexcept;

}  // namespace azlw::agent

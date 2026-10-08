// 声明舰船静态配置表的固定白名单与分页只读采集契约。

#pragma once

#include <algorithm>
#include <array>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "lua/lua_api.h"
#include "lua/lua_value.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 由请求解析和 Lua 读取共同使用的固定舰船静态配置表白名单。
inline constexpr auto kSupportedShipCatalogTables = std::to_array<std::string_view>({
    "ship_data_group",
    "ship_data_template",
    "ship_data_statistics",
    "ship_data_breakout",
    "ship_data_trans",
    "transform_data_template",
    "ship_transform",
    "ship_data_strengthen",
    "ship_strengthen_blueprint",
    "ship_data_blueprint",
    "ship_strengthen_meta",
    "ship_meta_breakout",
    "ship_meta_repair_effect",
    "ship_meta_repair",
    "skill_data_template",
    "skill_data_display",
    "skill_need_exp",
    "fleet_tech_ship_template",
    "ship_data_by_type",
    "attribute_info_by_type",
    "collection_ship_group",
});

/// 返回固定表键是否属于舰船静态目录协议白名单。
inline bool is_supported_ship_catalog_table(std::string_view table_key) noexcept {
    return std::find(
               kSupportedShipCatalogTables.begin(),
               kSupportedShipCatalogTables.end(),
               table_key) != kSupportedShipCatalogTables.end();
}

/// 一条配置在 confNEO 代理上展开全部物理字段和 base 继承后的原始值。
struct ShipCatalogRecord final {
    std::uint64_t id = 0;
    LuaValue raw;
};

/// 单个目录位置未能完整物化时的稳定诊断。
struct ShipCatalogReadError final {
    std::optional<std::uint32_t> catalog_index;
    std::optional<std::uint64_t> id;
    std::string code;
    std::string message;
};

/// 固定白名单中一张 `pg` 配置表的完整分页结果。
struct ShipCatalogPage final {
    std::string table_key;
    bool complete = false;
    std::string module_sha256;
    std::uint32_t start_index = 0;
    std::uint32_t total_count = 0;
    std::optional<std::uint32_t> next_index;
    std::vector<ShipCatalogRecord> records;
    std::vector<ShipCatalogReadError> read_errors;
};

/// 舰船静态目录分页任务的成功数据或稳定错误。
struct ShipCatalogPageExecution final {
    bool success = false;
    ShipCatalogPage page;
    AgentError error;
};

/// 一次图鉴读取跨响应页保留的已排序键。不跨新的从表头开始的读取。
struct CollectionReadIndex final {
    bool ready = false;
    const void* groups = nullptr;
    std::vector<std::uint64_t> identifiers;
    bool has_witness = false;
    std::uint64_t witness_id = 0;
    std::uint64_t witness_level = 0;
    std::uint64_t witness_star = 0;
    /// 续页已经对照过整表键和字段。没有这一步不能把旧索引当成完整结果。
    bool fields_captured = false;
    std::uint32_t capture_rows = 0;
    std::vector<std::uint64_t> levels;
    std::vector<std::uint64_t> stars;
    bool verified = false;
    std::uint32_t verify_count = 0;
    bool verify_count_done = false;
    std::uint32_t verify_rows = 0;
    bool verify_has_key = false;
    std::uint64_t verify_key = 0;
};

/// 一次静态目录响应在帧之间保留的游标。单帧只读取帧容量，响应不超过页容量。
struct ShipCatalogBatchProgress final {
    std::string table_key;
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
    std::string module_sha256;
    std::uint32_t cursor = 0;
    bool started = false;
    ShipCatalogPageExecution outcome;
    /// 图鉴键遍历只在这一次请求里续用，不跨操作保留。
    bool collection_scan_started = false;
    bool collection_scan_complete = false;
    bool collection_sorted = false;
    bool collection_heap_ready = false;
    std::uint32_t collection_sorted_count = 0;
    /// 本帧已经执行的堆操作次数。每次推进前清零，单帧不超过帧容量。
    std::uint32_t collection_heap_ops = 0;
    bool collection_has_cursor_key = false;
    std::uint64_t collection_cursor_key = 0;
    const void* collection_groups = nullptr;
    std::vector<std::uint64_t> collection_identifiers;
    bool collection_has_witness = false;
    std::uint64_t collection_witness_id = 0;
    std::uint64_t collection_witness_level = 0;
    std::uint64_t collection_witness_star = 0;
    /// 同一次逻辑读取的多页共用索引。空表示这一页单独建索引。
    std::shared_ptr<CollectionReadIndex> collection_index;
    bool collection_using_shared = false;
    bool collection_adopted = false;
};

enum class ShipCatalogBatchStep : std::uint8_t { Continue, Finished };

/// 推进一帧。返回 Continue 时调用方保持任务，不得把本帧当成最终响应。
ShipCatalogBatchStep advance_ship_catalog_batch(
    const LuaApi& api,
    lua_State* state,
    ShipCatalogBatchProgress* progress) noexcept;

/// 必须由 `tolua_update` 所在线程调用，物化一页固定白名单舰船配置。
ShipCatalogPageExecution snapshot_ship_catalog(
    const LuaApi& api,
    lua_State* state,
    std::string_view table_key,
    std::uint32_t start_index,
    std::uint32_t page_size,
    std::string_view module_sha256) noexcept;

}  // namespace azlw::agent

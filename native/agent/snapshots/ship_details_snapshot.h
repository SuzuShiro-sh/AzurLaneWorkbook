// 声明舰船分类、派生属性和技能展示详情的只读采集入口。

#pragma once

#include <cstdint>
#include <string_view>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"
#include "ship_skill_reader.h"

namespace azlw::agent {

/// 同时承载一份舰船详情快照的成功数据或稳定错误。
struct ShipDetailsExecution final {
    bool success = false;
    ShipDetailsSnapshot snapshot;
    AgentError error;
};

/// 读取栈顶舰船的详情。失败时不写入部分对象。
bool read_ship_detail(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    std::uint64_t ship_id,
    ShipDetail* output,
    ShipSkillVisitError* detail_error);

/// 必须由 `tolua_update` 所在线程调用，逐艘解析当前客户端的展示方法。
ShipDetailsExecution snapshot_ship_details(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    std::string_view module_sha256) noexcept;

}  // namespace azlw::agent

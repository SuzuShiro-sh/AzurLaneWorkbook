// 声明主线程 Lua 背包快照的执行结果和只读采集入口。

#pragma once

#include <cstdint>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 同时承载一次快照的成功数据或稳定错误。
struct SnapshotExecution final {
    bool success = false;
    BagSnapshot snapshot;
    AgentError error;
};

/// 按线上契约整理条目与诊断顺序，确保同一背包内容得到确定性编码。
void canonicalize_bag_snapshot(BagSnapshot* snapshot);

/// 必须由 `tolua_update` 所在线程调用；函数结束时恢复进入时的 Lua 栈顶。
SnapshotExecution snapshot_bag(const LuaApi& api, lua_State* state, std::uint32_t max_items) noexcept;

}  // namespace azlw::agent

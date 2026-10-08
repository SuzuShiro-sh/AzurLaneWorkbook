// 装备命令读取和动作前检共用的错误与舰船对象访问。

#pragma once

#include <string>
#include <utility>

#include "equipment_command.h"

namespace azlw::agent {

/// 构造未触发官方通知的稳定命令错误。
inline AgentError command_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.equipment_command",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

/// 取得目标舰船对象并留在栈顶。
bool push_target_ship(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t ship_id,
    AgentError* error);

}  // namespace azlw::agent

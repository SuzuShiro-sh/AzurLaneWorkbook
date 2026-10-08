// 声明运行态快照共用的 BayProxy 舰船枚举与身份校验入口。

#pragma once

#include <cstdint>
#include <functional>
#include <limits>
#include <optional>
#include <string>
#include <vector>

#include "lua/lua_api.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 船坞键或舰船实例身份无法可靠解释时的原始诊断。
struct BayShipVisitError final {
    std::optional<std::uint64_t> ship_id;
    std::string code;
    std::string message;
};

/// 船坞枚举上限和身份错误；具体舰船字段错误由访问者自己的契约记录。
struct BayShipVisitResult final {
    bool truncated = false;
    /// 本页后面还有未访问的舰船，且尚未达到本次传入的累计上限。
    /// 达到上限时只标记 truncated，调用方据此结束船坞而不是再开一帧。
    bool more = false;
    /// 本页实际处理的船坞条目数，不含为定位页起点而跳过的条目。
    std::uint32_t consumed = 0;
    /// 本页最后一把正整数船坞键。下一帧从它后面继续，不再从头跳过。
    std::optional<std::uint64_t> last_key;
    std::vector<BayShipVisitError> read_errors;
};

/// 访问已经通过键、对象 ID 和唯一性校验的舰船。
using BayShipVisitor = std::function<void(int object_index, std::uint64_t ship_id)>;

/// 在游戏主线程枚举唯一舰船，并统一处理 Proxy、上限、身份和 Lua 栈恢复。
bool visit_bay_ships(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    const BayShipVisitor& visitor,
    BayShipVisitResult* result,
    AgentError* error,
    std::uint32_t skip = 0,
    std::uint32_t page_limit = std::numeric_limits<std::uint32_t>::max(),
    std::uint64_t resume_after = 0,
    const std::vector<std::uint64_t>* selected_ids = nullptr);

}  // namespace azlw::agent

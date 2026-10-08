// 实现运行态快照共用的 BayProxy 舰船枚举、身份校验和 Lua 栈恢复。

#include "bay_ship_reader.h"

#include <cstdint>
#include <optional>
#include <unordered_set>
#include <utility>

#include "lua/lua_reader.h"

namespace azlw::agent {

bool visit_bay_ships(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    const BayShipVisitor& visitor,
    BayShipVisitResult* result,
    AgentError* error,
    std::uint32_t skip,
    std::uint32_t page_limit,
    std::uint64_t resume_after,
    const std::vector<std::uint64_t>* selected_ids) {
    LuaStackGuard stack(api, state);
    if (!push_lua_proxy(api, state, "BayProxy", "lua_bay_proxy_invalid", error)) {
        return false;
    }
    const int proxy_index = api.get_top(state);
    if (api.get_field_protected(state, proxy_index, "data") != 0) {
        *error = make_lua_error(
            "lua_bay_data_lookup_failed",
            lua_failure_detail(api, state, "读取 BayProxy.data 失败"));
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        *error = make_lua_error(
            "lua_bay_data_invalid",
            "BayProxy.data 尚未成为 Lua table",
            "same_request");
        return false;
    }
    const int dock_index = api.get_top(state);

    std::unordered_set<std::uint64_t> ship_ids;
    ship_ids.reserve(max_ships);
    std::uint32_t seen = 0;
    if (selected_ids) {
        // 显式实例 ID 直接索引船坞，缺失项由调用方按结果集合计算。
    } else if (resume_after != 0) {
        api.push_number(state, static_cast<double>(resume_after));
        api.push_value(state, -1);
        api.raw_get(state, dock_index);
        if (api.type(state, -1) == kLuaTypeNil) {
            api.set_top(state, api.get_top(state) - 2);
            result->read_errors.push_back(BayShipVisitError{
                .ship_id = resume_after,
                .code = "ship_page_anchor_missing",
                .message = "续办时上一页的船坞键已经不在 BayProxy.data 中",
            });
            return true;
        }
        api.set_top(state, api.get_top(state) - 1);
        skip = 0;
    } else {
        api.push_nil(state);
    }
    std::size_t selected_cursor = 0;
    auto next_ship = [&]() {
        if (!selected_ids) return api.next(state, dock_index) != 0;
        api.set_top(state, dock_index);
        while (selected_cursor < selected_ids->size()) {
            api.push_number(state, static_cast<double>((*selected_ids)[selected_cursor++]));
            api.push_value(state, -1);
            api.raw_get(state, dock_index);
            if (api.type(state, -1) != kLuaTypeNil) return true;
            api.set_top(state, dock_index);
        }
        return false;
    };
    while (next_ship()) {
        if (seen >= max_ships) {
            result->truncated = true;
            api.set_top(state, api.get_top(state) - 1);
            break;
        }
        if (seen < skip) {
            api.set_top(state, api.get_top(state) - 1);
            ++seen;
            continue;
        }
        if (page_limit == 0 || seen - skip >= page_limit) {
            result->more = true;
            api.set_top(state, api.get_top(state) - 1);
            break;
        }
        ++seen;
        const int object_index = api.get_top(state);
        const LuaNumber key_id = read_lua_number(api, state, -2);
        const LuaNumber object_id = read_lua_number_field(api, state, object_index, "id");
        std::optional<std::uint64_t> known_id;
        if (object_id.status == LuaNumberStatus::Present && object_id.value > 0) {
            known_id = object_id.value;
        } else if (key_id.status == LuaNumberStatus::Present && key_id.value > 0) {
            known_id = key_id.value;
        }

        if (key_id.status == LuaNumberStatus::Present && key_id.value > 0) {
            result->last_key = key_id.value;
        }
        if (key_id.status != LuaNumberStatus::Present || key_id.value == 0 ||
            object_id.status != LuaNumberStatus::Present || object_id.value == 0) {
            result->read_errors.push_back(BayShipVisitError{
                .ship_id = known_id,
                .code = "ship_id_invalid",
                .message = "船坞键和舰船 id 必须都是正整数",
            });
        } else if (key_id.value != object_id.value) {
            result->read_errors.push_back(BayShipVisitError{
                .ship_id = object_id.value,
                .code = "ship_id_mismatch",
                .message = "船坞键与舰船 id 不一致",
            });
        } else if (!ship_ids.insert(object_id.value).second) {
            result->read_errors.push_back(BayShipVisitError{
                .ship_id = object_id.value,
                .code = "duplicate_ship_id",
                .message = "船坞数据包含重复 ship_id",
            });
        } else {
            visitor(object_index, object_id.value);
        }
        // `lua_next` 继续迭代时只保留当前键；访问者的临时值一并恢复。
        api.set_top(state, object_index - 1);
    }
    result->consumed = seen > skip ? seen - skip : 0;
    return true;
}

}  // namespace azlw::agent

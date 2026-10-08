// 实现在游戏主线程从 Lua BagProxy 读取有界背包快照并保留逐条错误。

#include "bag_snapshot.h"

#include <exception>
#include <algorithm>
#include <cstdint>
#include <optional>
#include <string>
#include <unordered_set>
#include <utility>

#include "lua/lua_reader.h"

namespace azlw::agent {
namespace {

/// 通过条目 `getConfig("name")` 受保护调用读取显示名称。
std::optional<std::string> read_config_name(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    std::string* error) {
    const int top = api.get_top(state);
    const int stable_object_index = api.stable_stack_index(state, object_index);
    if (api.get_field_protected(state, object_index, "getConfig") != 0) {
        std::string lookup_error;
        const auto detail = read_lua_string(api, state, -1, &lookup_error);
        api.set_top(state, top);
        *error = "读取背包条目的 getConfig 字段失败";
        if (detail.has_value()) {
            *error += ": " + *detail;
        }
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeFunction) {
        api.set_top(state, top);
        *error = "背包条目缺少 getConfig 方法";
        return std::nullopt;
    }
    api.push_value(state, stable_object_index);
    api.push_string(state, "name");
    if (api.protected_call(state, 2, 1, 0) != 0) {
        std::string call_error;
        const auto detail = read_lua_string(api, state, -1, &call_error);
        api.set_top(state, top);
        *error = "getConfig(name) 调用失败";
        if (detail.has_value()) {
            *error += ": " + *detail;
        }
        return std::nullopt;
    }
    const auto name = read_lua_string(api, state, -1, error);
    api.set_top(state, top);
    return name;
}

/// 追加一个可选关联物品 ID 的逐条读取错误。
void add_read_error(
    BagSnapshot* snapshot,
    std::optional<std::uint64_t> item_id,
    std::string code,
    std::string message) {
    snapshot->read_errors.push_back(ReadError{
        .item_id = item_id,
        .code = std::move(code),
        .message = std::move(message),
    });
}

/// 从合成模板读取配方；无模板返回空，畸形模板记录逐条错误。
std::optional<ComposeRecipe> read_compose_recipe(
    const LuaApi& api,
    lua_State* state,
    int template_index,
    std::uint64_t item_id,
    std::uint64_t quantity,
    BagSnapshot* snapshot) {
    const int top = api.get_top(state);
    // confNEO 返回带 __index 的空代理行，必须在 pcall 边界内完成索引和字段读取。
    if (api.get_number_index_protected(
            state, template_index, static_cast<double>(item_id)) != 0) {
        add_read_error(
            snapshot,
            item_id,
            "compose_recipe_lookup_failed",
            lua_failure_detail(api, state, "读取合成模板失败"));
        api.set_top(state, top);
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        return std::nullopt;
    }

    const int recipe_index = api.get_top(state);
    LuaNumber material_id = read_lua_number_field(api, state, recipe_index, "material_id");
    const LuaNumber material_count =
        read_lua_number_field(api, state, recipe_index, "material_num");
    LuaNumber gold = read_lua_number_field(api, state, recipe_index, "gold_num");
    const LuaNumber equipment_id =
        read_lua_number_field(api, state, recipe_index, "equip_id");

    if (material_id.status == LuaNumberStatus::Missing ||
        (material_id.status == LuaNumberStatus::Present && material_id.value == 0)) {
        material_id = LuaNumber{.status = LuaNumberStatus::Present, .value = item_id};
    }
    if (gold.status == LuaNumberStatus::Missing) {
        gold = LuaNumber{.status = LuaNumberStatus::Present, .value = 0};
    }

    bool valid = true;
    if (material_count.status != LuaNumberStatus::Present || material_count.value == 0) {
        add_read_error(
            snapshot,
            item_id,
            "compose_material_count_invalid",
            "合成模板的 material_num 缺失或不是正整数");
        valid = false;
    }
    if (material_id.status != LuaNumberStatus::Present || material_id.value == 0) {
        add_read_error(
            snapshot,
            item_id,
            "compose_material_id_invalid",
            "合成模板的 material_id 不是正整数");
        valid = false;
    }
    if (gold.status != LuaNumberStatus::Present) {
        add_read_error(snapshot, item_id, "compose_gold_invalid", "合成模板的 gold_num 不是非负整数");
        valid = false;
    }
    if (equipment_id.status == LuaNumberStatus::Invalid) {
        add_read_error(
            snapshot,
            item_id,
            "compose_equipment_id_invalid",
            "合成模板的 equip_id 不是非负整数");
        valid = false;
    }
    api.set_top(state, top);
    if (!valid) {
        return std::nullopt;
    }

    ComposeRecipe recipe{
        .recipe_id = item_id,
        .material_id = material_id.value,
        .material_count = material_count.value,
        .gold = gold.value,
        .equipment_id = std::nullopt,
        .max_count = quantity / material_count.value,
    };
    if (equipment_id.status == LuaNumberStatus::Present && equipment_id.value > 0) {
        recipe.equipment_id = equipment_id.value;
    }
    return recipe;
}

}  // namespace

void canonicalize_bag_snapshot(BagSnapshot* snapshot) {
    if (snapshot == nullptr) {
        return;
    }
    std::sort(
        snapshot->items.begin(),
        snapshot->items.end(),
        [](const BagItem& left, const BagItem& right) { return left.item_id < right.item_id; });
    std::sort(
        snapshot->read_errors.begin(),
        snapshot->read_errors.end(),
        [](const ReadError& left, const ReadError& right) {
            if (left.item_id != right.item_id) {
                return left.item_id < right.item_id;
            }
            if (left.code != right.code) {
                return left.code < right.code;
            }
            return left.message < right.message;
        });
}

// 只在当前 Lua 线程遍历 BagProxy；单条失败不会中断其他条目读取。
SnapshotExecution snapshot_bag(const LuaApi& api, lua_State* state, std::uint32_t max_items) noexcept {
    SnapshotExecution execution;
    if (!api.ready() || state == nullptr || max_items == 0 || max_items > kMaximumSnapshotItems) {
        execution.error =
            make_lua_error("lua_snapshot_arguments_invalid", "Lua API、状态或条目上限无效");
        return execution;
    }

    try {
        LuaStackGuard stack(api, state);
        if (!push_lua_proxy(
                api,
                state,
                "BagProxy",
                "lua_bag_proxy_invalid",
                &execution.error)) {
            return execution;
        }
        const int proxy_index = api.get_top(state);

        if (api.get_field_protected(state, proxy_index, "data") != 0) {
            std::string detail_error;
            const auto detail = read_lua_string(api, state, -1, &detail_error);
            execution.error = make_lua_error(
                "lua_bag_data_lookup_failed",
                detail.has_value() ? "读取 BagProxy.data 失败: " + *detail
                                   : "读取 BagProxy.data 失败");
            return execution;
        }
        if (api.type(state, -1) != kLuaTypeTable) {
            api.set_top(state, api.get_top(state) - 1);
            if (api.get_field_protected(state, proxy_index, "getRawData") != 0) {
                std::string detail_error;
                const auto detail = read_lua_string(api, state, -1, &detail_error);
                execution.error = make_lua_error(
                    "lua_bag_raw_data_lookup_failed",
                    detail.has_value() ? "读取 BagProxy.getRawData 失败: " + *detail
                                       : "读取 BagProxy.getRawData 失败");
                return execution;
            }
            if (api.type(state, -1) != kLuaTypeFunction) {
                execution.error = make_lua_error(
                    "lua_bag_data_missing",
                    "BagProxy 尚未提供 data 表或 getRawData 方法",
                    "same_request");
                return execution;
            }
            api.push_value(state, proxy_index);
            if (api.protected_call(state, 1, 1, 0) != 0) {
                std::string detail_error;
                const auto detail = read_lua_string(api, state, -1, &detail_error);
                execution.error = make_lua_error(
                    "lua_bag_raw_data_failed",
                    detail.has_value() ? "调用 BagProxy.getRawData 失败: " + *detail
                                       : "调用 BagProxy.getRawData 失败");
                return execution;
            }
        }
        if (api.type(state, -1) != kLuaTypeTable) {
            execution.error = make_lua_error(
                "lua_bag_data_invalid",
                "BagProxy 数据尚未成为 Lua table",
                "same_request");
            return execution;
        }
        const int bag_table_index = api.get_top(state);

        int template_index = 0;
        api.push_string(state, "pg");
        api.raw_get(state, kLuaGlobalsIndex);
        if (api.type(state, -1) == kLuaTypeTable) {
            const int pg_index = api.get_top(state);
            api.push_string(state, "compose_data_template");
            api.raw_get(state, pg_index);
            if (api.type(state, -1) == kLuaTypeTable) {
                template_index = api.get_top(state);
            } else {
                api.set_top(state, api.get_top(state) - 1);
            }
        } else {
            api.set_top(state, api.get_top(state) - 1);
        }

        std::unordered_set<std::uint64_t> item_ids;
        item_ids.reserve(max_items);
        std::uint32_t visited = 0;
        api.push_nil(state);
        while (api.next(state, bag_table_index) != 0) {
            if (visited >= max_items) {
                execution.snapshot.truncated = true;
                break;
            }
            ++visited;

            LuaNumber item_id = read_lua_number(api, state, -2);
            const int value_type = api.type(state, -1);
            const bool object_value = value_type == kLuaTypeTable || value_type == kLuaTypeUserData;
            const int object_index = api.get_top(state);
            if ((item_id.status != LuaNumberStatus::Present || item_id.value == 0) &&
                object_value) {
                item_id = read_lua_number_field(api, state, object_index, "id");
            }
            if (item_id.status != LuaNumberStatus::Present || item_id.value == 0) {
                add_read_error(
                    &execution.snapshot,
                    std::nullopt,
                    "item_id_invalid",
                    "背包条目的 id 缺失或不是正整数");
                api.set_top(state, api.get_top(state) - 1);
                continue;
            }
            if (!item_ids.insert(item_id.value).second) {
                add_read_error(
                    &execution.snapshot,
                    item_id.value,
                    "duplicate_item_id",
                    "背包数据包含重复 item_id");
                api.set_top(state, api.get_top(state) - 1);
                continue;
            }

            LuaNumber quantity;
            if (object_value) {
                quantity = read_lua_number_field(api, state, object_index, "count");
            } else {
                quantity = read_lua_number(api, state, -1);
            }
            if (quantity.status != LuaNumberStatus::Present) {
                add_read_error(
                    &execution.snapshot,
                    item_id.value,
                    "item_quantity_invalid",
                    "背包条目的 count 缺失或不是非负整数");
                api.set_top(state, api.get_top(state) - 1);
                continue;
            }

            std::string name_error;
            const auto name = object_value
                                  ? read_config_name(api, state, object_index, &name_error)
                                  : std::optional<std::string>{};
            if (!name.has_value() || name->empty()) {
                add_read_error(
                    &execution.snapshot,
                    item_id.value,
                    "item_name_missing",
                    name_error.empty() ? "背包条目无法解析非空名称" : name_error);
                api.set_top(state, api.get_top(state) - 1);
                continue;
            }

            BagItem item{
                .item_id = item_id.value,
                .quantity = quantity.value,
                .resolved_name = *name,
                .compose_recipe = std::nullopt,
            };
            if (template_index > 0) {
                item.compose_recipe = read_compose_recipe(
                    api,
                    state,
                    template_index,
                    item.item_id,
                    item.quantity,
                    &execution.snapshot);
            }
            execution.snapshot.items.push_back(std::move(item));
            api.set_top(state, api.get_top(state) - 1);
        }

        canonicalize_bag_snapshot(&execution.snapshot);
        execution.snapshot.complete =
            !execution.snapshot.truncated && execution.snapshot.read_errors.empty();
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = make_lua_error("lua_snapshot_exception", exception.what());
        return execution;
    } catch (...) {
        execution.error =
            make_lua_error("lua_snapshot_exception", "背包快照发生未预期的本地异常");
        return execution;
    }
}

}  // namespace azlw::agent

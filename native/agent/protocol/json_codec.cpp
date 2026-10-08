// 实现有界严格 JSON 请求解析与协议字段校验。

#include "json_codec.h"
#include "json_codec_internal.h"

#include <algorithm>
#include <array>
#include <charconv>
#include <cmath>
#include <cstddef>
#include <cstring>
#include <limits>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#define JSMN_STATIC
#define JSMN_STRICT
#define JSMN_PARENT_LINKS
#include <jsmn.h>

#include "json_parser_internal.h"

namespace azlw::agent {
namespace {

using json_codec_internal::parse_lower_hex;
using json_codec_internal::protocol_error;
using json_parser_internal::ParsedJson;
using json_parser_internal::token_text;
using json_parser_internal::subtree_end;
using json_parser_internal::visit_object;
using json_parser_internal::visit_array;
using json_parser_internal::parse_unsigned;
using json_parser_internal::parse_execute_equipment_command_payload;
using json_parser_internal::parse_equipment_command_lookup_payload;

/// 单帧允许的最大 JSON token 数，限制解析内存和遍历成本。
constexpr std::size_t kMaximumJsonTokens = 512;

/// 记录 RPC 顶层字段 token 位置，并以负值表示尚未出现。
struct TopLevelFields final {
    int protocol_version = -1;
    int request_id = -1;
    int operation = -1;
    int timeout_ms = -1;
    int payload = -1;
};

/// 解析过程中的临时字段。成功后才收成只含本操作参数的载荷。
struct RpcDraft final {
    std::uint32_t timeout_ms = 0;
    std::uint32_t max_items = 0;
    std::uint32_t max_ships = 0;
    std::uint32_t max_equipments = 0;
    std::uint32_t start_index = 0;
    std::uint32_t page_size = 0;
    Operation operation = Operation::Unsupported;
    std::string raw_operation;
    std::string table_key;
    std::vector<std::uint64_t> ids;
    std::vector<std::uint64_t> weapon_ids;
    std::vector<SkillEffectQuery> skills;
    std::vector<std::uint64_t> equipment_type_ids;
    std::vector<std::uint64_t> nation_ids;
    std::vector<std::uint64_t> ship_type_ids;
    std::vector<std::string> attribute_keys;
    std::optional<EquipmentCommand> equipment_command;
    std::string command_id;
    OwnedQuery owned_query;
};

/// 丢掉当前操作用不到的临时字段，只保留该操作的参数。
RpcOperationBody materialize_rpc_body(RpcDraft draft) {
    switch (draft.operation) {
        case Operation::Health:
            return HealthRpcRequest{};
        case Operation::Capabilities:
            return CapabilitiesRpcRequest{};
        case Operation::SnapshotBag:
            return SnapshotBagRpcRequest{draft.timeout_ms, draft.max_items};
        case Operation::QueryOwned:
            return QueryOwnedRpcRequest{draft.timeout_ms, std::move(draft.owned_query)};
        case Operation::SnapshotResources:
            return SnapshotResourcesRpcRequest{draft.timeout_ms};
        case Operation::SnapshotOwnedState:
            return SnapshotOwnedStateRpcRequest{
                draft.timeout_ms,
                draft.max_ships,
                draft.max_equipments,
                draft.max_items,
            };
        case Operation::SnapshotShipDetails:
            return SnapshotShipDetailsRpcRequest{draft.timeout_ms, draft.max_ships};
        case Operation::SnapshotAccountBefore:
            return SnapshotAccountBeforeRpcRequest{
                draft.timeout_ms,
                draft.max_ships,
                draft.max_equipments,
                draft.max_items,
            };
        case Operation::SnapshotShipCatalog:
            return SnapshotShipCatalogRpcRequest{
                draft.timeout_ms,
                std::move(draft.table_key),
                draft.start_index,
                draft.page_size,
            };
        case Operation::SnapshotEquipmentConfigs:
            return SnapshotEquipmentConfigsRpcRequest{
                std::move(draft.ids),
                draft.timeout_ms,
                draft.start_index,
                draft.page_size,
            };
        case Operation::SnapshotComposeRecipes:
            return SnapshotComposeRecipesRpcRequest{
                draft.timeout_ms,
                draft.start_index,
                draft.page_size,
            };
        case Operation::SnapshotEquipmentWeapons:
            return SnapshotEquipmentWeaponsRpcRequest{
                draft.timeout_ms,
                std::move(draft.weapon_ids),
            };
        case Operation::SnapshotSkillEffects:
            return SnapshotSkillEffectsRpcRequest{draft.timeout_ms, std::move(draft.skills)};
        case Operation::SnapshotEquipmentReferenceNames:
            return SnapshotEquipmentReferenceNamesRpcRequest{
                draft.timeout_ms,
                std::move(draft.equipment_type_ids),
                std::move(draft.nation_ids),
                std::move(draft.ship_type_ids),
                std::move(draft.attribute_keys),
            };
        case Operation::ExecuteEquipmentCommand:
            return ExecuteEquipmentCommandRpcRequest{
                draft.timeout_ms,
                std::move(*draft.equipment_command),
            };
        case Operation::QueryEquipmentCommand:
            return QueryEquipmentCommandRpcRequest{std::move(draft.command_id)};
        case Operation::CancelEquipmentCommand:
            return CancelEquipmentCommandRpcRequest{std::move(draft.command_id)};
        case Operation::Shutdown:
            return ShutdownRpcRequest{draft.timeout_ms};
        case Operation::Unsupported:
            return UnsupportedRpcRequest{std::move(draft.raw_operation)};
    }
    return UnsupportedRpcRequest{std::move(draft.raw_operation)};
}

/// 严格校验选择器，避免未知字段悄悄扩大运行态读取范围。
bool parse_owned_query_payload(const ParsedJson& parsed, int payload_index,
                               OwnedQuery* query, AgentError* error) {
    int kind = -1, ids = -1, fields = -1;
    std::string reason;
    auto fail = [&]() {
        *error = protocol_error("owned_query_invalid", reason.empty() ? "持有对象查询参数无效" : reason);
        return false;
    };
    if (!visit_object(parsed, payload_index, [&](std::string_view key, int value) {
            int* target = key == "kind" ? &kind : key == "ids" ? &ids : key == "fields" ? &fields : nullptr;
            if (!target || *target != -1) { reason = "查询包含未知或重复字段"; return false; }
            *target = value;
            return true;
        }, &reason) || kind < 0 || ids < 0 || fields < 0) return fail();
    if (parsed.tokens[kind].type != JSMN_STRING) return fail();
    query->kind = token_text(parsed, parsed.tokens[kind]);
    if (query->kind != "ships" && query->kind != "equipment") return fail();
    if (!visit_array(parsed, ids, [&](std::size_t, int token) {
            std::uint64_t id = 0;
            if (!parse_unsigned(parsed, token, 9007199254740991ULL, &id) || id == 0 ||
                query->ids.size() >= kMaximumEquipmentWeaponBatchSize ||
                std::find(query->ids.begin(), query->ids.end(), id) != query->ids.end()) return false;
            query->ids.push_back(id);
            return true;
        }, &reason)) return fail();
    const std::vector<std::string_view> ship_fields = {"ship_id", "config_id", "name", "level",
        "experience_in_level", "intimacy_raw", "energy", "proficiency", "fleet_memberships", "skills", "slots", "details"};
    const std::vector<std::string_view> equipment_fields = {"config_id", "enhance_level", "warehouse_quantity", "equipped"};
    const auto& allowed = query->kind == "ships" ? ship_fields : equipment_fields;
    if (!visit_array(parsed, fields, [&](std::size_t, int token) {
            if (parsed.tokens[token].type != JSMN_STRING) return false;
            const std::string field(token_text(parsed, parsed.tokens[token]));
            if (std::find(allowed.begin(), allowed.end(), field) == allowed.end() ||
                std::find(query->fields.begin(), query->fields.end(), field) != query->fields.end()) return false;
            query->fields.push_back(field);
            return true;
        }, &reason)) return fail();
    return true;
}

/// 判断根对象之后唯一允许出现的 JSON 空白字符。
bool is_whitespace(char value) {
    return value == ' ' || value == '\t' || value == '\r' || value == '\n';
}

/// 在固定 token 容量内解析唯一根对象，并拒绝尾随非空白内容。
bool parse_document(std::string_view json, ParsedJson* parsed, std::string* error) {
    if (json.empty()) {
        *error = "JSON 文档为空";
        return false;
    }

    parsed->source = json;
    parsed->tokens.resize(kMaximumJsonTokens);
    jsmn_parser parser{};
    jsmn_init(&parser);
    const int count = jsmn_parse(
        &parser,
        json.data(),
        json.size(),
        parsed->tokens.data(),
        static_cast<unsigned int>(parsed->tokens.size()));
    if (count <= 0) {
        *error = count == JSMN_ERROR_NOMEM ? "JSON token 数超过 512" : "JSON 语法无效";
        return false;
    }
    parsed->tokens.resize(static_cast<std::size_t>(count));
    const jsmntok_t& root = parsed->tokens.front();
    if (root.type != JSMN_OBJECT) {
        *error = "JSON 顶层必须是对象";
        return false;
    }
    for (std::size_t index = static_cast<std::size_t>(root.end); index < json.size(); ++index) {
        if (!is_whitespace(json[index])) {
            *error = "JSON 根对象后存在额外内容";
            return false;
        }
    }
    return true;
}

/// 保留对象遍历中最先发现的结构错误，避免后续字段覆盖根因。
void set_first_error(std::optional<AgentError>* destination, AgentError error) {
    if (!destination->has_value()) {
        *destination = std::move(error);
    }
}

/// 确认无需参数的操作收到严格空对象。
bool parse_empty_payload(const ParsedJson& parsed, int payload_index) {
    const jsmntok_t& payload = parsed.tokens[static_cast<std::size_t>(payload_index)];
    return payload.type == JSMN_OBJECT && subtree_end(parsed, payload_index) == payload_index + 1;
}

/// 解析唯一 `max_items` 字段并执行冻结快照上限检查。
bool parse_snapshot_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::uint32_t* max_items,
    AgentError* error) {
    int max_items_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            if (name != "max_items") {
                *error = protocol_error("unknown_field", "snapshot_bag.payload 存在未知字段");
                return false;
            }
            if (max_items_token >= 0) {
                duplicate = true;
                return true;
            }
            max_items_token = value_index;
            return true;
        },
        &parse_error);
    if (!visited) {
        if (error->code.empty()) {
            *error = protocol_error("payload_invalid", parse_error);
        }
        return false;
    }
    if (duplicate) {
        *error = protocol_error("duplicate_field", "snapshot_bag.payload.max_items 重复");
        return false;
    }
    std::uint64_t parsed_maximum = 0;
    if (max_items_token < 0 ||
        !parse_unsigned(parsed, max_items_token, kMaximumSnapshotItems, &parsed_maximum) ||
        parsed_maximum == 0) {
        *error = protocol_error("snapshot_limit_out_of_range", "max_items 只允许 1 至 2000");
        return false;
    }
    *max_items = static_cast<std::uint32_t>(parsed_maximum);
    return true;
}

/// 严格解析舰船详情快照唯一允许的 `max_ships` 字段。
bool parse_ship_details_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::uint32_t* max_ships,
    AgentError* error) {
    int max_ships_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            if (name != "max_ships") {
                *error = protocol_error(
                    "unknown_field",
                    "snapshot_ship_details.payload 存在未知字段");
                return false;
            }
            if (max_ships_token >= 0) {
                duplicate = true;
                return true;
            }
            max_ships_token = value_index;
            return true;
        },
        &parse_error);
    if (!visited) {
        if (error->code.empty()) {
            *error = protocol_error("payload_invalid", parse_error);
        }
        return false;
    }
    if (duplicate) {
        *error = protocol_error(
            "duplicate_field",
            "snapshot_ship_details.payload.max_ships 重复");
        return false;
    }
    std::uint64_t parsed_maximum = 0;
    if (max_ships_token < 0 ||
        !parse_unsigned(parsed, max_ships_token, kMaximumSnapshotItems, &parsed_maximum) ||
        parsed_maximum == 0) {
        *error = protocol_error(
            "snapshot_limit_out_of_range",
            "max_ships 只允许 1 至 2000");
        return false;
    }
    *max_ships = static_cast<std::uint32_t>(parsed_maximum);
    return true;
}

/// 严格解析静态目录页的零基起点和页容量。
bool parse_catalog_page_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::string_view operation_name,
    bool require_table_key,
    RpcDraft* request,
    AgentError* error) {
    int ids_token = -1;
    int table_key_token = -1;
    int start_index_token = -1;
    int page_size_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "ids" && request->operation == Operation::SnapshotEquipmentConfigs) {
                destination = &ids_token;
            } else if (name == "table_key" && require_table_key) {
                destination = &table_key_token;
            } else if (name == "start_index") {
                destination = &start_index_token;
            } else if (name == "page_size") {
                destination = &page_size_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    std::string(operation_name) + ".payload 存在未知字段");
                return false;
            }
            if (*destination >= 0) {
                duplicate = true;
                return true;
            }
            *destination = value_index;
            return true;
        },
        &parse_error);
    if (!visited) {
        if (error->code.empty()) {
            *error = protocol_error("payload_invalid", parse_error);
        }
        return false;
    }
    if (duplicate) {
        *error = protocol_error(
            "duplicate_field", std::string(operation_name) + ".payload 存在重复字段");
        return false;
    }

    if (ids_token >= 0) {
        if (start_index_token >= 0 || page_size_token >= 0) {
            *error = protocol_error("equipment_config_batch_invalid", "ids 不得与分页字段混用");
            return false;
        }
        const bool valid = visit_array(parsed, ids_token, [&](std::size_t, int index) {
            std::uint64_t id = 0;
            if (!parse_unsigned(parsed, index, 9'007'199'254'740'991ULL, &id) || id == 0 ||
                request->ids.size() >= kMaximumEquipmentFrameSize ||
                (!request->ids.empty() && request->ids.back() >= id)) return false;
            request->ids.push_back(id);
            return true;
        }, &parse_error);
        if (!valid || request->ids.empty()) {
            *error = protocol_error("equipment_config_batch_invalid", "ids 必须包含 1 至 250 个严格升序的 Lua 正整数标识");
            return false;
        }
        return true;
    }
    if (require_table_key) {
        if (table_key_token < 0 ||
            parsed.tokens[static_cast<std::size_t>(table_key_token)].type != JSMN_STRING) {
            *error = protocol_error(
                "ship_catalog_table_invalid", "snapshot_ship_catalog.payload.table_key 必须是字符串");
            return false;
        }
        request->table_key.assign(token_text(
            parsed,
            parsed.tokens[static_cast<std::size_t>(table_key_token)]));
        if (!is_supported_ship_catalog_table(request->table_key)) {
            *error = protocol_error(
                "ship_catalog_table_unsupported", "table_key 不属于舰船静态目录白名单");
            return false;
        }
    }

    std::uint64_t start_index = 0;
    std::uint64_t page_size = 0;
    const std::uint64_t maximum_items =
        require_table_key ? kMaximumShipCatalogItems : kMaximumEquipmentCatalogItems;
    const std::uint64_t maximum_page_size =
        require_table_key ? kMaximumShipCatalogPageSize : kMaximumEquipmentPageSize;
    if (start_index_token < 0 || page_size_token < 0 ||
        !parse_unsigned(
            parsed,
            start_index_token,
            maximum_items,
            &start_index) ||
        !parse_unsigned(
            parsed, page_size_token, maximum_page_size, &page_size) ||
        page_size == 0) {
        *error = protocol_error(
            require_table_key ? "ship_catalog_page_out_of_range" : "catalog_page_out_of_range",
            require_table_key
                ? std::string("舰船静态目录 start_index 或 page_size 超出共享构建契约")
                : "start_index 只允许 0 至 " +
                      std::to_string(kMaximumEquipmentCatalogItems) +
                      "，page_size 只允许 1 至 " +
                      std::to_string(kMaximumEquipmentPageSize));
        return false;
    }
    request->start_index = static_cast<std::uint32_t>(start_index);
    request->page_size = static_cast<std::uint32_t>(page_size);
    return true;
}

/// 严格解析升序去重的武器 ID 批次，防止单帧重复读取相同大表。
bool parse_equipment_weapon_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::vector<std::uint64_t>* weapon_ids,
    AgentError* error) {
    int identifiers_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool payload_visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            if (name != "weapon_ids") {
                *error = protocol_error(
                    "unknown_field",
                    "snapshot_equipment_weapons.payload 存在未知字段");
                return false;
            }
            if (identifiers_token >= 0) {
                duplicate = true;
                return true;
            }
            identifiers_token = value_index;
            return true;
        },
        &parse_error);
    if (!payload_visited || identifiers_token < 0 || duplicate) {
        if (error->code.empty()) {
            *error = protocol_error(
                duplicate ? "duplicate_field" : "equipment_weapon_batch_invalid",
                duplicate ? "snapshot_equipment_weapons.payload.weapon_ids 重复"
                          : (parse_error.empty() ? "weapon_ids 是必填字段" : parse_error));
        }
        return false;
    }

    weapon_ids->clear();
    const bool array_visited = visit_array(
        parsed,
        identifiers_token,
        [&](std::size_t, int value_index) {
            std::uint64_t identifier = 0;
            if (!parse_unsigned(
                    parsed,
                    value_index,
                    9'007'199'254'740'991ULL,
                    &identifier) ||
                identifier == 0) {
                *error = protocol_error(
                    "equipment_weapon_batch_invalid",
                    "weapon_ids 只能包含 Lua 可精确表示的正整数");
                return false;
            }
            if (weapon_ids->size() >= kMaximumEquipmentWeaponBatchSize) {
                *error = protocol_error(
                    "equipment_weapon_batch_invalid",
                    "weapon_ids 最多包含 128 个标识");
                return false;
            }
            if (!weapon_ids->empty() && weapon_ids->back() >= identifier) {
                *error = protocol_error(
                    "equipment_weapon_batch_invalid",
                    "weapon_ids 必须严格升序且不重复");
                return false;
            }
            weapon_ids->push_back(identifier);
            return true;
        },
        &parse_error);
    if (!array_visited || weapon_ids->empty()) {
        if (error->code.empty()) {
            *error = protocol_error(
                "equipment_weapon_batch_invalid",
                parse_error.empty() ? "weapon_ids 不能为空" : parse_error);
        }
        return false;
    }
    return true;
}

/// 解析单个技能查询对象，要求两个字段唯一且均为正整数。
bool parse_skill_effect_query(
    const ParsedJson& parsed,
    int query_index,
    SkillEffectQuery* query,
    AgentError* error) {
    int skill_id_token = -1;
    int level_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        query_index,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "skill_id") {
                destination = &skill_id_token;
            } else if (name == "level") {
                destination = &level_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    "snapshot_skill_effects.payload.skills 存在未知字段");
                return false;
            }
            if (*destination >= 0) {
                duplicate = true;
                return true;
            }
            *destination = value_index;
            return true;
        },
        &parse_error);
    std::uint64_t skill_id = 0;
    std::uint64_t level = 0;
    if (!visited || duplicate || skill_id_token < 0 || level_token < 0 ||
        !parse_unsigned(
            parsed,
            skill_id_token,
            9'007'199'254'740'991ULL,
            &skill_id) ||
        !parse_unsigned(parsed, level_token, std::numeric_limits<std::uint32_t>::max(), &level) ||
        skill_id == 0 || level == 0) {
        if (error->code.empty()) {
            *error = protocol_error(
                duplicate ? "duplicate_field" : "skill_effect_batch_invalid",
                duplicate ? "技能查询包含重复字段"
                          : "每个技能查询都必须包含正整数 skill_id 和 level");
        }
        return false;
    }
    *query = SkillEffectQuery{
        .skill_id = skill_id,
        .level = static_cast<std::uint32_t>(level),
    };
    return true;
}

/// 严格解析按 `(skill_id, level)` 升序去重的技能效果批次。
bool parse_skill_effect_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::vector<SkillEffectQuery>* skills,
    AgentError* error) {
    int skills_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool payload_visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            if (name != "skills") {
                *error = protocol_error(
                    "unknown_field",
                    "snapshot_skill_effects.payload 存在未知字段");
                return false;
            }
            if (skills_token >= 0) {
                duplicate = true;
                return true;
            }
            skills_token = value_index;
            return true;
        },
        &parse_error);
    if (!payload_visited || skills_token < 0 || duplicate) {
        if (error->code.empty()) {
            *error = protocol_error(
                duplicate ? "duplicate_field" : "skill_effect_batch_invalid",
                duplicate ? "snapshot_skill_effects.payload.skills 重复"
                          : (parse_error.empty() ? "skills 是必填字段" : parse_error));
        }
        return false;
    }

    skills->clear();
    const bool array_visited = visit_array(
        parsed,
        skills_token,
        [&](std::size_t, int value_index) {
            if (skills->size() >= kMaximumSkillEffectBatchSize) {
                *error = protocol_error(
                    "skill_effect_batch_invalid",
                    "skills 最多包含 64 个查询");
                return false;
            }
            SkillEffectQuery query;
            if (!parse_skill_effect_query(parsed, value_index, &query, error)) {
                return false;
            }
            if (!skills->empty()) {
                const SkillEffectQuery& previous = skills->back();
                if (previous.skill_id > query.skill_id ||
                    (previous.skill_id == query.skill_id && previous.level >= query.level)) {
                    *error = protocol_error(
                        "skill_effect_batch_invalid",
                        "skills 必须按 skill_id、level 严格升序且不重复");
                    return false;
                }
            }
            skills->push_back(query);
            return true;
        },
        &parse_error);
    if (!array_visited || skills->empty()) {
        if (error->code.empty()) {
            *error = protocol_error(
                "skill_effect_batch_invalid",
                parse_error.empty() ? "skills 不能为空" : parse_error);
        }
        return false;
    }
    return true;
}

/// 解析单个升序去重的整数引用数组，并按命名空间控制零值语义。
bool parse_reference_identifier_array(
    const ParsedJson& parsed,
    int array_token,
    bool allow_zero,
    std::vector<std::uint64_t>* identifiers,
    std::size_t* total_count,
    AgentError* error) {
    identifiers->clear();
    std::string parse_error;
    const bool visited = visit_array(
        parsed,
        array_token,
        [&](std::size_t, int value_index) {
            std::uint64_t identifier = 0;
            if (!parse_unsigned(
                    parsed,
                    value_index,
                    9'007'199'254'740'991ULL,
                    &identifier) ||
                (!allow_zero && identifier == 0)) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    allow_zero ? "名称引用 ID 只能包含 Lua 可精确表示的非负整数"
                               : "名称引用 ID 只能包含 Lua 可精确表示的正整数");
                return false;
            }
            if (!identifiers->empty() && identifiers->back() >= identifier) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    "名称引用 ID 数组必须严格升序且不重复");
                return false;
            }
            if (*total_count >= kMaximumEquipmentReferenceBatchSize) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    "装备引用名称批次合计最多包含 256 个键");
                return false;
            }
            identifiers->push_back(identifier);
            ++*total_count;
            return true;
        },
        &parse_error);
    if (!visited && error->code.empty()) {
        *error = protocol_error("equipment_reference_batch_invalid", parse_error);
    }
    return visited;
}

/// 属性键只接受无需 JSON 反转义的小写稳定标识，并按字节序严格升序。
bool parse_reference_attribute_array(
    const ParsedJson& parsed,
    int array_token,
    std::vector<std::string>* keys,
    std::size_t* total_count,
    AgentError* error) {
    keys->clear();
    std::string parse_error;
    const bool visited = visit_array(
        parsed,
        array_token,
        [&](std::size_t, int value_index) {
            const jsmntok_t& token = parsed.tokens[static_cast<std::size_t>(value_index)];
            if (token.type != JSMN_STRING) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    "attribute_keys 只能包含字符串");
                return false;
            }
            const std::string_view key = token_text(parsed, token);
            if (key.empty() || key.size() > 128 ||
                !std::all_of(key.begin(), key.end(), [](char current) {
                    return (current >= 'a' && current <= 'z') ||
                           (current >= '0' && current <= '9') || current == '.' ||
                           current == '_' || current == '-';
                })) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    "attribute_keys 只能包含 1 至 128 字节的小写稳定标识");
                return false;
            }
            if (!keys->empty() && keys->back() >= key) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    "attribute_keys 必须严格字典序且不重复");
                return false;
            }
            if (*total_count >= kMaximumEquipmentReferenceBatchSize) {
                *error = protocol_error(
                    "equipment_reference_batch_invalid",
                    "装备引用名称批次合计最多包含 256 个键");
                return false;
            }
            keys->emplace_back(key);
            ++*total_count;
            return true;
        },
        &parse_error);
    if (!visited && error->code.empty()) {
        *error = protocol_error("equipment_reference_batch_invalid", parse_error);
    }
    return visited;
}

/// 四个命名空间字段必须齐全；允许单类为空，但整个请求至少包含一个引用键。
bool parse_equipment_reference_payload(
    const ParsedJson& parsed,
    int payload_index,
    RpcDraft* request,
    AgentError* error) {
    int equipment_types_token = -1;
    int nations_token = -1;
    int ship_types_token = -1;
    int attribute_keys_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "equipment_type_ids") {
                destination = &equipment_types_token;
            } else if (name == "nation_ids") {
                destination = &nations_token;
            } else if (name == "ship_type_ids") {
                destination = &ship_types_token;
            } else if (name == "attribute_keys") {
                destination = &attribute_keys_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    "snapshot_equipment_reference_names.payload 存在未知字段");
                return false;
            }
            if (*destination >= 0) {
                duplicate = true;
                return true;
            }
            *destination = value_index;
            return true;
        },
        &parse_error);
    if (!visited || duplicate || equipment_types_token < 0 || nations_token < 0 ||
        ship_types_token < 0 || attribute_keys_token < 0) {
        if (error->code.empty()) {
            *error = protocol_error(
                duplicate ? "duplicate_field" : "equipment_reference_batch_invalid",
                duplicate ? "装备引用名称请求包含重复字段"
                          : (parse_error.empty() ? "装备引用名称请求缺少必填数组" : parse_error));
        }
        return false;
    }

    std::size_t total_count = 0;
    if (!parse_reference_identifier_array(
            parsed,
            equipment_types_token,
            false,
            &request->equipment_type_ids,
            &total_count,
            error) ||
        !parse_reference_identifier_array(
            parsed, nations_token, true, &request->nation_ids, &total_count, error) ||
        !parse_reference_identifier_array(
            parsed, ship_types_token, false, &request->ship_type_ids, &total_count, error) ||
        !parse_reference_attribute_array(
            parsed, attribute_keys_token, &request->attribute_keys, &total_count, error)) {
        return false;
    }
    if (total_count == 0) {
        *error = protocol_error(
            "equipment_reference_batch_invalid",
            "装备引用名称请求至少包含一个引用键");
        return false;
    }
    return true;
}

/// 严格解析完整运行态快照的三个独立条目上限。
bool parse_owned_state_payload(
    const ParsedJson& parsed,
    int payload_index,
    RpcDraft* request,
    AgentError* error) {
    int max_ships_token = -1;
    int max_equipments_token = -1;
    int max_items_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "max_ships") {
                destination = &max_ships_token;
            } else if (name == "max_equipments") {
                destination = &max_equipments_token;
            } else if (name == "max_items") {
                destination = &max_items_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    "snapshot_owned_state.payload 存在未知字段");
                return false;
            }
            if (*destination >= 0) {
                duplicate = true;
                return true;
            }
            *destination = value_index;
            return true;
        },
        &parse_error);
    if (!visited) {
        if (error->code.empty()) {
            *error = protocol_error("payload_invalid", parse_error);
        }
        return false;
    }
    if (duplicate) {
        *error = protocol_error(
            "duplicate_field",
            "snapshot_owned_state.payload 存在重复字段");
        return false;
    }

    std::uint64_t max_ships = 0;
    std::uint64_t max_equipments = 0;
    std::uint64_t max_items = 0;
    if (max_ships_token < 0 || max_equipments_token < 0 || max_items_token < 0 ||
        !parse_unsigned(parsed, max_ships_token, kMaximumSnapshotItems, &max_ships) ||
        !parse_unsigned(
            parsed,
            max_equipments_token,
            kMaximumSnapshotItems,
            &max_equipments) ||
        !parse_unsigned(parsed, max_items_token, kMaximumSnapshotItems, &max_items) ||
        max_ships == 0 || max_equipments == 0 || max_items == 0) {
        *error = protocol_error(
            "snapshot_limit_out_of_range",
            "max_ships、max_equipments 和 max_items 都只允许 1 至 2000");
        return false;
    }
    request->max_ships = static_cast<std::uint32_t>(max_ships);
    request->max_equipments = static_cast<std::uint32_t>(max_equipments);
    request->max_items = static_cast<std::uint32_t>(max_items);
    return true;
}

}  // namespace

// 先独立确认请求号；只有有效请求号存在时，连接才可收到结构错误详情。
bool parse_rpc_request(
    std::string_view json,
    RpcRequest* request,
    bool* can_respond,
    AgentError* error) {
    *can_respond = false;
    ParsedJson parsed;
    std::string parse_error;
    if (!parse_document(json, &parsed, &parse_error)) {
        *error = protocol_error("json_invalid", parse_error);
        return false;
    }

    TopLevelFields fields;
    std::optional<AgentError> structural_error;
    const bool visited = visit_object(
        parsed,
        0,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "protocol_version") {
                destination = &fields.protocol_version;
            } else if (name == "request_id") {
                destination = &fields.request_id;
            } else if (name == "operation") {
                destination = &fields.operation;
            } else if (name == "timeout_ms") {
                destination = &fields.timeout_ms;
            } else if (name == "payload") {
                destination = &fields.payload;
            } else {
                set_first_error(
                    &structural_error,
                    protocol_error("unknown_field", "RPC 请求存在未知顶层字段"));
                return true;
            }
            if (*destination >= 0) {
                set_first_error(
                    &structural_error,
                    protocol_error("duplicate_field", "RPC 请求存在重复顶层字段"));
                return true;
            }
            *destination = value_index;
            return true;
        },
        &parse_error);
    if (!visited) {
        *error = protocol_error("json_invalid", parse_error);
        return false;
    }

    if (fields.request_id >= 0) {
        const jsmntok_t& id_token = parsed.tokens[static_cast<std::size_t>(fields.request_id)];
        if (id_token.type == JSMN_STRING) {
            const std::string_view id = token_text(parsed, id_token);
            std::array<std::uint8_t, sizeof(std::uint64_t)> decoded{};
            if (parse_lower_hex(id, decoded.data(), decoded.size())) {
                std::uint64_t number = 0;
                for (const std::uint8_t byte : decoded) {
                    number = (number << 8) | byte;
                }
                request->request_id.assign(id);
                request->request_number = number;
                *can_respond = true;
            }
        }
    }
    if (!*can_respond) {
        *error = protocol_error("request_id_invalid", "request_id 必须是 16 位小写十六进制字符串");
        return false;
    }
    if (structural_error.has_value()) {
        *error = std::move(*structural_error);
        return false;
    }
    if (fields.protocol_version < 0 || fields.operation < 0 || fields.timeout_ms < 0 || fields.payload < 0) {
        *error = protocol_error("required_field_missing", "RPC 请求缺少必填字段");
        return false;
    }

    std::uint64_t protocol = 0;
    if (!parse_unsigned(parsed, fields.protocol_version, kProtocolVersion, &protocol) ||
        protocol != kProtocolVersion) {
        *error = protocol_error("protocol_version_mismatch", "RPC 协议版本必须为 1");
        return false;
    }
    std::uint64_t timeout = 0;
    if (!parse_unsigned(parsed, fields.timeout_ms, kMaximumTimeoutMs, &timeout) ||
        timeout < kMinimumTimeoutMs) {
        *error = protocol_error("timeout_out_of_range", "timeout_ms 只允许 1 至 30000");
        return false;
    }
    RpcDraft draft;
    draft.timeout_ms = static_cast<std::uint32_t>(timeout);

    const jsmntok_t& operation_token = parsed.tokens[static_cast<std::size_t>(fields.operation)];
    if (operation_token.type != JSMN_STRING) {
        *error = protocol_error("operation_invalid", "operation 必须是字符串");
        return false;
    }
    draft.raw_operation.assign(token_text(parsed, operation_token));
    namespace contract = azlw::runtime_rpc_contract;
    if (draft.raw_operation == contract::kRpcOperationHealth) {
        draft.operation = Operation::Health;
    } else if (draft.raw_operation == contract::kRpcOperationCapabilities) {
        draft.operation = Operation::Capabilities;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotBag) {
        draft.operation = Operation::SnapshotBag;
    } else if (draft.raw_operation == contract::kRpcOperationQueryOwned) {
        draft.operation = Operation::QueryOwned;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotResources) {
        draft.operation = Operation::SnapshotResources;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotOwnedState) {
        draft.operation = Operation::SnapshotOwnedState;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotShipDetails) {
        draft.operation = Operation::SnapshotShipDetails;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotAccountBefore) {
        draft.operation = Operation::SnapshotAccountBefore;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotShipCatalog) {
        draft.operation = Operation::SnapshotShipCatalog;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotEquipmentConfigs) {
        draft.operation = Operation::SnapshotEquipmentConfigs;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotComposeRecipes) {
        draft.operation = Operation::SnapshotComposeRecipes;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotEquipmentWeapons) {
        draft.operation = Operation::SnapshotEquipmentWeapons;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotSkillEffects) {
        draft.operation = Operation::SnapshotSkillEffects;
    } else if (draft.raw_operation == contract::kRpcOperationSnapshotEquipmentReferenceNames) {
        draft.operation = Operation::SnapshotEquipmentReferenceNames;
    } else if (draft.raw_operation == contract::kRpcOperationExecuteEquipmentCommand) {
        draft.operation = Operation::ExecuteEquipmentCommand;
    } else if (draft.raw_operation == contract::kRpcOperationQueryEquipmentCommand) {
        draft.operation = Operation::QueryEquipmentCommand;
    } else if (draft.raw_operation == contract::kRpcOperationCancelEquipmentCommand) {
        draft.operation = Operation::CancelEquipmentCommand;
    } else if (draft.raw_operation == contract::kRpcOperationShutdown) {
        draft.operation = Operation::Shutdown;
    } else {
        draft.operation = Operation::Unsupported;
    }

    const jsmntok_t& payload = parsed.tokens[static_cast<std::size_t>(fields.payload)];
    if (payload.type != JSMN_OBJECT) {
        *error = protocol_error("payload_invalid", "payload 必须是对象");
        return false;
    }
    if ((draft.operation == Operation::Health || draft.operation == Operation::Capabilities ||
         draft.operation == Operation::Shutdown || draft.operation == Operation::SnapshotResources) &&
        !parse_empty_payload(parsed, fields.payload)) {
        *error = protocol_error("unknown_field", "该操作的 payload 必须是空对象");
        return false;
    }
    if (draft.operation == Operation::SnapshotBag &&
        !parse_snapshot_payload(parsed, fields.payload, &draft.max_items, error)) {
        return false;
    }
    if (draft.operation == Operation::QueryOwned &&
        !parse_owned_query_payload(parsed, fields.payload, &draft.owned_query, error)) {
        return false;
    }
    if (draft.operation == Operation::SnapshotOwnedState &&
        !parse_owned_state_payload(parsed, fields.payload, &draft, error)) {
        return false;
    }
    if (draft.operation == Operation::SnapshotShipDetails &&
        !parse_ship_details_payload(parsed, fields.payload, &draft.max_ships, error)) {
        return false;
    }
    if (draft.operation == Operation::SnapshotAccountBefore &&
        !parse_owned_state_payload(parsed, fields.payload, &draft, error)) {
        return false;
    }
    if ((draft.operation == Operation::SnapshotShipCatalog ||
         draft.operation == Operation::SnapshotEquipmentConfigs ||
         draft.operation == Operation::SnapshotComposeRecipes) &&
        !parse_catalog_page_payload(
            parsed,
            fields.payload,
            draft.raw_operation,
            draft.operation == Operation::SnapshotShipCatalog,
            &draft,
            error)) {
        return false;
    }
    if (draft.operation == Operation::SnapshotEquipmentWeapons &&
        !parse_equipment_weapon_payload(
            parsed,
            fields.payload,
            &draft.weapon_ids,
            error)) {
        return false;
    }
    if (draft.operation == Operation::SnapshotSkillEffects &&
        !parse_skill_effect_payload(parsed, fields.payload, &draft.skills, error)) {
        return false;
    }
    if (draft.operation == Operation::SnapshotEquipmentReferenceNames &&
        !parse_equipment_reference_payload(parsed, fields.payload, &draft, error)) {
        return false;
    }
    if (draft.operation == Operation::ExecuteEquipmentCommand) {
        EquipmentCommand command;
        if (!parse_execute_equipment_command_payload(
                parsed,
                fields.payload,
                &command,
                error)) {
            return false;
        }
        draft.equipment_command = std::move(command);
    }
    if ((draft.operation == Operation::QueryEquipmentCommand ||
         draft.operation == Operation::CancelEquipmentCommand) &&
        !parse_equipment_command_lookup_payload(
            parsed,
            fields.payload,
            &draft.command_id,
            error)) {
        return false;
    }
    request->body = materialize_rpc_body(std::move(draft));
    return true;
}

}  // namespace azlw::agent

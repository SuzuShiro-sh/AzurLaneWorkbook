// 解析装备命令及查询载荷，保留各动作的字段约束与错误分类。

#include "json_parser_internal.h"
#include "json_codec_internal.h"

#include <array>
#include <limits>
#include <optional>
#include <utility>

namespace azlw::agent::json_parser_internal {

using json_codec_internal::parse_lower_hex;
using json_codec_internal::protocol_error;

namespace {

constexpr std::uint64_t kMaximumExactLuaInteger = 9'007'199'254'740'991;

/// 读取固定 64 位小写十六进制摘要，并保留原始规范文本。
bool parse_sha256_text(
    const ParsedJson& parsed,
    int token_index,
    std::string* value) {
    const jsmntok_t& token = parsed.tokens[static_cast<std::size_t>(token_index)];
    if (token.type != JSMN_STRING) {
        return false;
    }
    const std::string_view text = token_text(parsed, token);
    std::array<std::uint8_t, 32> decoded{};
    if (!parse_lower_hex(text, decoded.data(), decoded.size())) {
        return false;
    }
    value->assign(text);
    return true;
}

/// 严格解析命令中的单件装备描述。
bool parse_equipment_command_snapshot(
    const ParsedJson& parsed,
    int object_index,
    EquipmentSnapshot* equipment,
    AgentError* error) {
    int equipment_id_token = -1;
    int config_id_token = -1;
    int enhance_level_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        object_index,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "equipment_id") {
                destination = &equipment_id_token;
            } else if (name == "config_id") {
                destination = &config_id_token;
            } else if (name == "enhance_level") {
                destination = &enhance_level_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    "equipment_command.payload.action 装备描述存在未知字段");
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
            "equipment_command.payload.action 装备描述存在重复字段");
        return false;
    }

    std::uint64_t equipment_id = 0;
    std::uint64_t config_id = 0;
    std::uint64_t enhance_level = 0;
    if (equipment_id_token < 0 || config_id_token < 0 || enhance_level_token < 0 ||
        !parse_unsigned(
            parsed,
            equipment_id_token,
            kMaximumExactLuaInteger,
            &equipment_id) ||
        !parse_unsigned(parsed, config_id_token, kMaximumExactLuaInteger, &config_id) ||
        !parse_unsigned(
            parsed,
            enhance_level_token,
            std::numeric_limits<std::uint32_t>::max(),
            &enhance_level) ||
        equipment_id == 0 || config_id == 0) {
        *error = protocol_error(
            "equipment_command_equipment_invalid",
            "装备 ID、配置 ID 必须是 Lua 可精确表示的正整数，强化等级必须位于 u32 范围");
        return false;
    }
    *equipment = EquipmentSnapshot{
        .equipment_id = equipment_id,
        .config_id = config_id,
        .enhance_level = static_cast<std::uint32_t>(enhance_level),
    };
    return true;
}

/// 解析显式 null 或严格装备对象，避免缺失与空槽语义混淆。
bool parse_optional_equipment_command_snapshot(
    const ParsedJson& parsed,
    int token_index,
    std::optional<EquipmentSnapshot>* equipment,
    AgentError* error) {
    const jsmntok_t& token = parsed.tokens[static_cast<std::size_t>(token_index)];
    if (token.type == JSMN_PRIMITIVE && token_text(parsed, token) == "null") {
        equipment->reset();
        return true;
    }
    if (token.type != JSMN_OBJECT) {
        *error = protocol_error(
            "equipment_command_equipment_invalid",
            "target_before 和 source_before 只允许装备对象或 null");
        return false;
    }
    EquipmentSnapshot parsed_equipment;
    if (!parse_equipment_command_snapshot(parsed, token_index, &parsed_equipment, error)) {
        return false;
    }
    *equipment = parsed_equipment;
    return true;
}

/// 解析按物品 ID 严格升序的单级强化材料前态和成本。
bool parse_equipment_command_materials(
    const ParsedJson& parsed,
    int token_index,
    std::vector<EquipmentCommandMaterialCost>* materials,
    AgentError* error) {
    materials->clear();
    std::string parse_error;
    const bool visited = visit_array(
        parsed,
        token_index,
        [&](std::size_t, int material_index) {
            int item_id_token = -1;
            int quantity_before_token = -1;
            int cost_token = -1;
            bool duplicate = false;
            std::string object_error;
            const bool object_visited = visit_object(
                parsed,
                material_index,
                [&](std::string_view name, int value_index) {
                    int* destination = nullptr;
                    if (name == "item_id") {
                        destination = &item_id_token;
                    } else if (name == "quantity_before") {
                        destination = &quantity_before_token;
                    } else if (name == "cost") {
                        destination = &cost_token;
                    } else {
                        *error = protocol_error(
                            "unknown_field",
                            "enhance.materials 存在未知字段");
                        return false;
                    }
                    if (*destination >= 0) {
                        duplicate = true;
                        return true;
                    }
                    *destination = value_index;
                    return true;
                },
                &object_error);
            if (!object_visited || duplicate || item_id_token < 0 ||
                quantity_before_token < 0 || cost_token < 0) {
                if (error->code.empty()) {
                    *error = protocol_error(
                        duplicate ? "duplicate_field" : "equipment_command_precondition_invalid",
                        duplicate ? "enhance.materials 存在重复字段"
                                  : (object_error.empty()
                                         ? "enhance.materials 每项必须包含 item_id、quantity_before 和 cost"
                                         : object_error));
                }
                return false;
            }
            std::uint64_t item_id = 0;
            std::uint64_t quantity_before = 0;
            std::uint64_t cost = 0;
            if (!parse_unsigned(parsed, item_id_token, kMaximumExactLuaInteger, &item_id) ||
                !parse_unsigned(
                    parsed,
                    quantity_before_token,
                    kMaximumExactLuaInteger,
                    &quantity_before) ||
                !parse_unsigned(parsed, cost_token, kMaximumExactLuaInteger, &cost) ||
                item_id == 0 || cost == 0 || cost > quantity_before ||
                materials->size() >= kMaximumEnhanceMaterialCount ||
                (!materials->empty() && materials->back().item_id >= item_id)) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "enhance.materials 必须不超过 64 项，按正物品 ID 严格升序，且成本不能超过前态数量");
                return false;
            }
            materials->push_back(EquipmentCommandMaterialCost{
                .item_id = item_id,
                .quantity_before = quantity_before,
                .cost = cost,
            });
            return true;
        },
        &parse_error);
    if (!visited) {
        if (error->code.empty()) {
            *error = protocol_error(
                "equipment_command_precondition_invalid",
                parse_error.empty() ? "enhance.materials 必须是数组" : parse_error);
        }
        return false;
    }
    return true;
}

}  // namespace

/// 严格解析 execute_equipment_command 的版本、幂等键和局部原子前置条件。
bool parse_execute_equipment_command_payload(
    const ParsedJson& parsed,
    int payload_index,
    EquipmentCommand* command,
    AgentError* error) {
    int schema_version_token = -1;
    int command_id_token = -1;
    int target_fingerprint_token = -1;
    int plan_hash_token = -1;
    int sequence_token = -1;
    int pre_state_token = -1;
    int action_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "schema_version") {
                destination = &schema_version_token;
            } else if (name == "command_id") {
                destination = &command_id_token;
            } else if (name == "target_fingerprint_sha256") {
                destination = &target_fingerprint_token;
            } else if (name == "plan_hash") {
                destination = &plan_hash_token;
            } else if (name == "sequence") {
                destination = &sequence_token;
            } else if (name == "pre_state_content_sha256") {
                destination = &pre_state_token;
            } else if (name == "action") {
                destination = &action_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    "execute_equipment_command.payload 存在未知字段");
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
            "execute_equipment_command.payload 存在重复字段");
        return false;
    }

    std::uint64_t schema_version = 0;
    std::uint64_t sequence = 0;
    if (schema_version_token < 0 || command_id_token < 0 ||
        target_fingerprint_token < 0 || plan_hash_token < 0 || sequence_token < 0 ||
        pre_state_token < 0 || action_token < 0 ||
        !parse_unsigned(parsed, schema_version_token, 2, &schema_version) ||
        schema_version != 2 ||
        !parse_unsigned(
            parsed,
            sequence_token,
            std::numeric_limits<std::uint32_t>::max(),
            &sequence) ||
        sequence == 0 ||
        !parse_sha256_text(parsed, command_id_token, &command->command_id) ||
        !parse_sha256_text(
            parsed,
            target_fingerprint_token,
            &command->target_fingerprint_sha256) ||
        !parse_sha256_text(parsed, plan_hash_token, &command->plan_hash) ||
        !parse_sha256_text(
            parsed,
            pre_state_token,
            &command->pre_state_content_sha256)) {
        *error = protocol_error(
            "equipment_command_metadata_invalid",
            "命令版本必须为 2、sequence 必须大于 0，四个摘要必须是 64 位小写十六进制文本");
        return false;
    }
    command->schema_version = static_cast<std::uint32_t>(schema_version);
    command->sequence = static_cast<std::uint32_t>(sequence);

    int kind_token = -1;
    int ship_id_token = -1;
    int slot_index_token = -1;
    int target_before_token = -1;
    int source_before_token = -1;
    int source_quantity_token = -1;
    int dismantle_quantity_token = -1;
    int target_quantity_token = -1;
    int equipment_capacity_token = -1;
    int equipment_limit_token = -1;
    int recipe_id_token = -1;
    int compose_quantity_token = -1;
    int output_config_id_token = -1;
    int output_before_token = -1;
    int output_quantity_token = -1;
    int material_id_token = -1;
    int material_quantity_token = -1;
    int material_per_unit_token = -1;
    int gold_before_token = -1;
    int gold_per_unit_token = -1;
    int target_config_id_token = -1;
    int target_enhance_level_token = -1;
    int materials_token = -1;
    int gold_cost_token = -1;
    duplicate = false;
    parse_error.clear();
    const bool action_visited = visit_object(
        parsed,
        action_token,
        [&](std::string_view name, int value_index) {
            int* destination = nullptr;
            if (name == "kind") {
                destination = &kind_token;
            } else if (name == "ship_id") {
                destination = &ship_id_token;
            } else if (name == "slot_index") {
                destination = &slot_index_token;
            } else if (name == "target_before") {
                destination = &target_before_token;
            } else if (name == "source_before") {
                destination = &source_before_token;
            } else if (name == "source_quantity_before") {
                destination = &source_quantity_token;
            } else if (name == "dismantle_quantity") {
                destination = &dismantle_quantity_token;
            } else if (name == "target_warehouse_quantity_before") {
                destination = &target_quantity_token;
            } else if (name == "equipment_capacity_before") {
                destination = &equipment_capacity_token;
            } else if (name == "equipment_limit_before") {
                destination = &equipment_limit_token;
            } else if (name == "recipe_id") {
                destination = &recipe_id_token;
            } else if (name == "compose_quantity") {
                destination = &compose_quantity_token;
            } else if (name == "output_config_id") {
                destination = &output_config_id_token;
            } else if (name == "output_before") {
                destination = &output_before_token;
            } else if (name == "output_quantity_before") {
                destination = &output_quantity_token;
            } else if (name == "material_id") {
                destination = &material_id_token;
            } else if (name == "material_quantity_before") {
                destination = &material_quantity_token;
            } else if (name == "material_quantity_per_unit") {
                destination = &material_per_unit_token;
            } else if (name == "gold_before") {
                destination = &gold_before_token;
            } else if (name == "gold_per_unit") {
                destination = &gold_per_unit_token;
            } else if (name == "target_config_id") {
                destination = &target_config_id_token;
            } else if (name == "target_enhance_level") {
                destination = &target_enhance_level_token;
            } else if (name == "materials") {
                destination = &materials_token;
            } else if (name == "gold_cost") {
                destination = &gold_cost_token;
            } else {
                *error = protocol_error(
                    "unknown_field",
                    "execute_equipment_command.payload.action 存在未知字段");
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
    if (!action_visited) {
        if (error->code.empty()) {
            *error = protocol_error("payload_invalid", parse_error);
        }
        return false;
    }
    if (duplicate) {
        *error = protocol_error(
            "duplicate_field",
            "execute_equipment_command.payload.action 存在重复字段");
        return false;
    }
    if (kind_token < 0) {
        *error = protocol_error(
            "required_field_missing",
            "execute_equipment_command.payload.action 缺少 kind");
        return false;
    }

    const jsmntok_t& kind = parsed.tokens[static_cast<std::size_t>(kind_token)];
    if (kind.type != JSMN_STRING) {
        *error = protocol_error("equipment_command_action_invalid", "action.kind 必须是字符串");
        return false;
    }
    const std::string_view kind_text = token_text(parsed, kind);
    const auto missing_action_fields = [&]() {
        *error = protocol_error(
            "required_field_missing",
            "execute_equipment_command.payload.action 缺少当前 kind 的必填字段");
        return false;
    };
    const auto unexpected_action_fields = [&]() {
        *error = protocol_error(
            "equipment_command_action_invalid",
            "execute_equipment_command.payload.action 包含不属于当前 kind 的字段");
        return false;
    };
    const auto parse_capacity = [&](std::uint64_t* capacity, std::uint64_t* limit) {
        return parse_unsigned(
                   parsed,
                   equipment_capacity_token,
                   kMaximumExactLuaInteger,
                   capacity) &&
               parse_unsigned(
                   parsed,
                   equipment_limit_token,
                   kMaximumExactLuaInteger,
                   limit) &&
               *limit != 0 && *capacity <= *limit;
    };
    const auto has_compose_fields = [&]() {
        return recipe_id_token >= 0 || compose_quantity_token >= 0 ||
               output_config_id_token >= 0 || output_before_token >= 0 ||
               output_quantity_token >= 0 || material_id_token >= 0 ||
               material_quantity_token >= 0 || material_per_unit_token >= 0 ||
               gold_before_token >= 0 || gold_per_unit_token >= 0;
    };
    const auto has_enhance_fields = [&]() {
        return target_config_id_token >= 0 || target_enhance_level_token >= 0 ||
               materials_token >= 0 || gold_cost_token >= 0;
    };
    const auto has_compose_specific_fields = [&]() {
        return recipe_id_token >= 0 || compose_quantity_token >= 0 ||
               output_config_id_token >= 0 || output_before_token >= 0 ||
               output_quantity_token >= 0 || material_id_token >= 0 ||
               material_quantity_token >= 0 || material_per_unit_token >= 0 ||
               gold_per_unit_token >= 0;
    };
    const auto parse_enhance_common = [&] (
                                           EquipmentSnapshot* source_before,
                                           std::uint64_t* target_config_id,
                                           std::uint32_t* target_enhance_level,
                                           std::vector<EquipmentCommandMaterialCost>* materials,
                                           std::uint64_t* gold_before,
                                           std::uint64_t* gold_cost,
                                           std::uint64_t* equipment_capacity,
                                           std::uint64_t* equipment_limit) {
        std::uint64_t parsed_target_level = 0;
        if (!parse_equipment_command_snapshot(
                parsed,
                source_before_token,
                source_before,
                error) ||
            !parse_unsigned(
                parsed,
                target_config_id_token,
                kMaximumExactLuaInteger,
                target_config_id) ||
            !parse_unsigned(
                parsed,
                target_enhance_level_token,
                std::numeric_limits<std::uint32_t>::max(),
                &parsed_target_level) ||
            !parse_equipment_command_materials(parsed, materials_token, materials, error) ||
            !parse_unsigned(
                parsed,
                gold_before_token,
                kMaximumExactLuaInteger,
                gold_before) ||
            !parse_unsigned(
                parsed,
                gold_cost_token,
                kMaximumExactLuaInteger,
                gold_cost) ||
            !parse_capacity(equipment_capacity, equipment_limit)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "enhance 的来源、目标、材料、物资或容量字段无效");
            }
            return false;
        }
        *target_enhance_level = static_cast<std::uint32_t>(parsed_target_level);
        return true;
    };

    if (kind_text == "unequip") {
        if (ship_id_token < 0 || slot_index_token < 0 || target_before_token < 0 ||
            target_quantity_token < 0 || equipment_capacity_token < 0 ||
            equipment_limit_token < 0) {
            return missing_action_fields();
        }
        if (source_before_token >= 0 || source_quantity_token >= 0 ||
            dismantle_quantity_token >= 0 || has_compose_fields() || has_enhance_fields()) {
            return unexpected_action_fields();
        }
        std::uint64_t ship_id = 0;
        std::uint64_t slot_index = 0;
        std::uint64_t target_quantity = 0;
        std::uint64_t equipment_capacity = 0;
        std::uint64_t equipment_limit = 0;
        EquipmentSnapshot target_before;
        if (!parse_unsigned(parsed, ship_id_token, kMaximumExactLuaInteger, &ship_id) ||
            !parse_unsigned(parsed, slot_index_token, kShipEquipmentSlotCount, &slot_index) ||
            !parse_unsigned(
                parsed,
                target_quantity_token,
                kMaximumExactLuaInteger,
                &target_quantity) ||
            !parse_capacity(&equipment_capacity, &equipment_limit) || ship_id == 0 ||
            slot_index == 0 || equipment_capacity >= equipment_limit ||
            target_quantity > equipment_capacity ||
            !parse_equipment_command_snapshot(
                parsed,
                target_before_token,
                &target_before,
                error)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "unequip 的舰船、槽位、装备、仓库数量或容量前置条件无效");
            }
            return false;
        }
        command->action = UnequipEquipmentCommandAction{
            .ship_id = ship_id,
            .slot_index = static_cast<std::uint32_t>(slot_index),
            .target_before = target_before,
            .target_warehouse_quantity_before = target_quantity,
            .equipment_capacity_before = equipment_capacity,
            .equipment_limit_before = equipment_limit,
        };
        return true;
    }

    if (kind_text == "equip") {
        if (ship_id_token < 0 || slot_index_token < 0 || target_before_token < 0 ||
            source_before_token < 0 || source_quantity_token < 0 ||
            target_quantity_token < 0 || equipment_capacity_token < 0 ||
            equipment_limit_token < 0) {
            return missing_action_fields();
        }
        if (dismantle_quantity_token >= 0 || has_compose_fields() || has_enhance_fields()) {
            return unexpected_action_fields();
        }
        std::uint64_t ship_id = 0;
        std::uint64_t slot_index = 0;
        std::uint64_t source_quantity = 0;
        std::uint64_t target_quantity = 0;
        std::uint64_t equipment_capacity = 0;
        std::uint64_t equipment_limit = 0;
        std::optional<EquipmentSnapshot> target_before;
        EquipmentSnapshot source_before;
        if (!parse_unsigned(parsed, ship_id_token, kMaximumExactLuaInteger, &ship_id) ||
            !parse_unsigned(parsed, slot_index_token, kShipEquipmentSlotCount, &slot_index) ||
            !parse_unsigned(
                parsed,
                source_quantity_token,
                kMaximumExactLuaInteger,
                &source_quantity) ||
            !parse_unsigned(
                parsed,
                target_quantity_token,
                kMaximumExactLuaInteger,
                &target_quantity) ||
            !parse_capacity(&equipment_capacity, &equipment_limit) || ship_id == 0 ||
            slot_index == 0 || source_quantity == 0 ||
            source_quantity > equipment_capacity || target_quantity > equipment_capacity ||
            !parse_optional_equipment_command_snapshot(
                parsed,
                target_before_token,
                &target_before,
                error) ||
            !parse_equipment_command_snapshot(
                parsed,
                source_before_token,
                &source_before,
                error)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "equip 的舰船、槽位、装备、仓库数量或容量前置条件无效");
            }
            return false;
        }
        if ((!target_before.has_value() && target_quantity != 0) ||
            (target_before.has_value() &&
             target_before->equipment_id == source_before.equipment_id) ||
            (target_before.has_value() &&
             source_quantity > equipment_capacity - target_quantity)) {
            *error = protocol_error(
                "equipment_command_precondition_invalid",
                "equip 的目标与来源仓库前置条件不可区分或不完整");
            return false;
        }
        command->action = EquipEquipmentCommandAction{
            .ship_id = ship_id,
            .slot_index = static_cast<std::uint32_t>(slot_index),
            .target_before = target_before,
            .source_before = source_before,
            .source_quantity_before = source_quantity,
            .target_warehouse_quantity_before = target_quantity,
            .equipment_capacity_before = equipment_capacity,
            .equipment_limit_before = equipment_limit,
        };
        return true;
    }

    if (kind_text == "dismantle") {
        if (source_before_token < 0 || source_quantity_token < 0 ||
            dismantle_quantity_token < 0 || equipment_capacity_token < 0 ||
            equipment_limit_token < 0) {
            return missing_action_fields();
        }
        if (ship_id_token >= 0 || slot_index_token >= 0 || target_before_token >= 0 ||
            target_quantity_token >= 0 || has_compose_fields() || has_enhance_fields()) {
            return unexpected_action_fields();
        }
        std::uint64_t source_quantity = 0;
        std::uint64_t dismantle_quantity = 0;
        std::uint64_t equipment_capacity = 0;
        std::uint64_t equipment_limit = 0;
        EquipmentSnapshot source_before;
        if (!parse_unsigned(
                parsed,
                source_quantity_token,
                kMaximumExactLuaInteger,
                &source_quantity) ||
            !parse_unsigned(
                parsed,
                dismantle_quantity_token,
                kMaximumExactLuaInteger,
                &dismantle_quantity) ||
            !parse_capacity(&equipment_capacity, &equipment_limit) ||
            !parse_equipment_command_snapshot(
                parsed,
                source_before_token,
                &source_before,
                error)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "dismantle 的来源、数量、强化等级或仓库容量前置条件无效");
            }
            return false;
        }
        const DismantleEquipmentCommandAction action{
            .source_before = source_before,
            .source_quantity_before = source_quantity,
            .dismantle_quantity = dismantle_quantity,
            .equipment_capacity_before = equipment_capacity,
            .equipment_limit_before = equipment_limit,
        };
        if (!dismantle_equipment_command_action_is_valid(action)) {
            *error = protocol_error(
                "equipment_command_precondition_invalid",
                "dismantle 的来源、数量、强化等级或仓库容量前置条件无效");
            return false;
        }
        command->action = action;
        return true;
    }

    if (kind_text == "compose") {
        if (recipe_id_token < 0 || compose_quantity_token < 0 ||
            output_config_id_token < 0 || output_before_token < 0 ||
            output_quantity_token < 0 || material_id_token < 0 ||
            material_quantity_token < 0 || material_per_unit_token < 0 ||
            gold_before_token < 0 || gold_per_unit_token < 0 ||
            equipment_capacity_token < 0 || equipment_limit_token < 0) {
            return missing_action_fields();
        }
        if (ship_id_token >= 0 || slot_index_token >= 0 || target_before_token >= 0 ||
            source_before_token >= 0 || source_quantity_token >= 0 ||
            dismantle_quantity_token >= 0 || target_quantity_token >= 0 ||
            has_enhance_fields()) {
            return unexpected_action_fields();
        }
        std::uint64_t recipe_id = 0;
        std::uint64_t compose_quantity = 0;
        std::uint64_t output_config_id = 0;
        std::uint64_t output_quantity = 0;
        std::uint64_t material_id = 0;
        std::uint64_t material_quantity = 0;
        std::uint64_t material_per_unit = 0;
        std::uint64_t gold_before = 0;
        std::uint64_t gold_per_unit = 0;
        std::uint64_t equipment_capacity = 0;
        std::uint64_t equipment_limit = 0;
        std::optional<EquipmentSnapshot> output_before;
        if (!parse_unsigned(parsed, recipe_id_token, kMaximumExactLuaInteger, &recipe_id) ||
            !parse_unsigned(
                parsed,
                compose_quantity_token,
                kMaximumExactLuaInteger,
                &compose_quantity) ||
            !parse_unsigned(
                parsed,
                output_config_id_token,
                kMaximumExactLuaInteger,
                &output_config_id) ||
            !parse_optional_equipment_command_snapshot(
                parsed,
                output_before_token,
                &output_before,
                error) ||
            !parse_unsigned(
                parsed,
                output_quantity_token,
                kMaximumExactLuaInteger,
                &output_quantity) ||
            !parse_unsigned(parsed, material_id_token, kMaximumExactLuaInteger, &material_id) ||
            !parse_unsigned(
                parsed,
                material_quantity_token,
                kMaximumExactLuaInteger,
                &material_quantity) ||
            !parse_unsigned(
                parsed,
                material_per_unit_token,
                kMaximumExactLuaInteger,
                &material_per_unit) ||
            !parse_unsigned(
                parsed,
                gold_before_token,
                kMaximumExactLuaInteger,
                &gold_before) ||
            !parse_unsigned(
                parsed,
                gold_per_unit_token,
                kMaximumExactLuaInteger,
                &gold_per_unit) ||
            !parse_capacity(&equipment_capacity, &equipment_limit)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "compose 的配方、产物、材料、物资或容量字段无效");
            }
            return false;
        }
        const ComposeEquipmentCommandAction action{
            .recipe_id = recipe_id,
            .compose_quantity = compose_quantity,
            .output_config_id = output_config_id,
            .output_before = output_before,
            .output_quantity_before = output_quantity,
            .material_id = material_id,
            .material_quantity_before = material_quantity,
            .material_quantity_per_unit = material_per_unit,
            .gold_before = gold_before,
            .gold_per_unit = gold_per_unit,
            .equipment_capacity_before = equipment_capacity,
            .equipment_limit_before = equipment_limit,
        };
        if (!compose_equipment_command_action_is_valid(action)) {
            *error = protocol_error(
                "equipment_command_precondition_invalid",
                "compose 的数量、产物、材料、物资或仓库容量前置条件无效");
            return false;
        }
        command->action = action;
        return true;
    }

    if (kind_text == "enhance_warehouse") {
        if (source_before_token < 0 || source_quantity_token < 0 ||
            target_config_id_token < 0 || target_enhance_level_token < 0 ||
            target_before_token < 0 || target_quantity_token < 0 || materials_token < 0 ||
            gold_before_token < 0 || gold_cost_token < 0 || equipment_capacity_token < 0 ||
            equipment_limit_token < 0) {
            return missing_action_fields();
        }
        if (ship_id_token >= 0 || slot_index_token >= 0 || dismantle_quantity_token >= 0 ||
            has_compose_specific_fields()) {
            return unexpected_action_fields();
        }
        EquipmentSnapshot source_before;
        std::uint64_t source_quantity = 0;
        std::uint64_t target_config_id = 0;
        std::uint32_t target_enhance_level = 0;
        std::optional<EquipmentSnapshot> target_before;
        std::uint64_t target_quantity = 0;
        std::vector<EquipmentCommandMaterialCost> materials;
        std::uint64_t gold_before = 0;
        std::uint64_t gold_cost = 0;
        std::uint64_t equipment_capacity = 0;
        std::uint64_t equipment_limit = 0;
        if (!parse_unsigned(
                parsed,
                source_quantity_token,
                kMaximumExactLuaInteger,
                &source_quantity) ||
            !parse_optional_equipment_command_snapshot(
                parsed,
                target_before_token,
                &target_before,
                error) ||
            !parse_unsigned(
                parsed,
                target_quantity_token,
                kMaximumExactLuaInteger,
                &target_quantity) ||
            !parse_enhance_common(
                &source_before,
                &target_config_id,
                &target_enhance_level,
                &materials,
                &gold_before,
                &gold_cost,
                &equipment_capacity,
                &equipment_limit)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "enhance_warehouse 的来源、目标聚合或资源前置条件无效");
            }
            return false;
        }
        const EnhanceWarehouseEquipmentCommandAction action{
            .source_before = source_before,
            .source_quantity_before = source_quantity,
            .target_config_id = target_config_id,
            .target_enhance_level = target_enhance_level,
            .target_before = target_before,
            .target_warehouse_quantity_before = target_quantity,
            .materials = std::move(materials),
            .gold_before = gold_before,
            .gold_cost = gold_cost,
            .equipment_capacity_before = equipment_capacity,
            .equipment_limit_before = equipment_limit,
        };
        if (!enhance_warehouse_equipment_command_action_is_valid(action)) {
            *error = protocol_error(
                "equipment_command_precondition_invalid",
                "enhance_warehouse 的来源、目标聚合、资源或容量前置条件无效");
            return false;
        }
        command->action = action;
        return true;
    }

    if (kind_text == "enhance_ship") {
        if (ship_id_token < 0 || slot_index_token < 0 || source_before_token < 0 ||
            target_config_id_token < 0 || target_enhance_level_token < 0 ||
            materials_token < 0 || gold_before_token < 0 || gold_cost_token < 0 ||
            equipment_capacity_token < 0 || equipment_limit_token < 0) {
            return missing_action_fields();
        }
        if (target_before_token >= 0 || source_quantity_token >= 0 ||
            dismantle_quantity_token >= 0 || target_quantity_token >= 0 ||
            has_compose_specific_fields()) {
            return unexpected_action_fields();
        }
        std::uint64_t ship_id = 0;
        std::uint64_t slot_index = 0;
        EquipmentSnapshot source_before;
        std::uint64_t target_config_id = 0;
        std::uint32_t target_enhance_level = 0;
        std::vector<EquipmentCommandMaterialCost> materials;
        std::uint64_t gold_before = 0;
        std::uint64_t gold_cost = 0;
        std::uint64_t equipment_capacity = 0;
        std::uint64_t equipment_limit = 0;
        if (!parse_unsigned(parsed, ship_id_token, kMaximumExactLuaInteger, &ship_id) ||
            !parse_unsigned(parsed, slot_index_token, kShipEquipmentSlotCount, &slot_index) ||
            !parse_enhance_common(
                &source_before,
                &target_config_id,
                &target_enhance_level,
                &materials,
                &gold_before,
                &gold_cost,
                &equipment_capacity,
                &equipment_limit)) {
            if (error->code.empty()) {
                *error = protocol_error(
                    "equipment_command_precondition_invalid",
                    "enhance_ship 的舰船槽位、来源、目标或资源前置条件无效");
            }
            return false;
        }
        const EnhanceShipEquipmentCommandAction action{
            .ship_id = ship_id,
            .slot_index = static_cast<std::uint32_t>(slot_index),
            .source_before = source_before,
            .target_config_id = target_config_id,
            .target_enhance_level = target_enhance_level,
            .materials = std::move(materials),
            .gold_before = gold_before,
            .gold_cost = gold_cost,
            .equipment_capacity_before = equipment_capacity,
            .equipment_limit_before = equipment_limit,
        };
        if (!enhance_ship_equipment_command_action_is_valid(action)) {
            *error = protocol_error(
                "equipment_command_precondition_invalid",
                "enhance_ship 的舰船槽位、单级目标、资源或容量前置条件无效");
            return false;
        }
        command->action = action;
        return true;
    }

    *error = protocol_error(
        "equipment_command_action_invalid",
        "action.kind 只允许 equip、unequip、dismantle、compose、enhance_warehouse 或 enhance_ship");
    return false;
}

/// query 和 cancel 只接受一个规范命令摘要。
bool parse_equipment_command_lookup_payload(
    const ParsedJson& parsed,
    int payload_index,
    std::string* command_id,
    AgentError* error) {
    int command_id_token = -1;
    bool duplicate = false;
    std::string parse_error;
    const bool visited = visit_object(
        parsed,
        payload_index,
        [&](std::string_view name, int value_index) {
            if (name != "command_id") {
                *error = protocol_error(
                    "unknown_field",
                    "equipment_command 查询 payload 存在未知字段");
                return false;
            }
            if (command_id_token >= 0) {
                duplicate = true;
                return true;
            }
            command_id_token = value_index;
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
            "equipment_command 查询 payload.command_id 重复");
        return false;
    }
    if (command_id_token < 0 ||
        !parse_sha256_text(parsed, command_id_token, command_id)) {
        *error = protocol_error(
            "equipment_command_id_invalid",
            "command_id 必须是 64 位小写十六进制 SHA-256");
        return false;
    }
    return true;
}

}  // namespace azlw::agent::json_parser_internal

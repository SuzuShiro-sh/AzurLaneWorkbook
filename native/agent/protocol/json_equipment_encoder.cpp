// 编码装备命令收据、配置、配方和效果查询结果。

#include "json_codec.h"
#include "json_codec_internal.h"

namespace azlw::agent {

using json_codec_internal::begin_ok_response;
using json_codec_internal::validate_equipment_command_receipt;

namespace {

/// 将 Lua 值转换为确定性 JSON；混合键表保留显式键类型，避免键语义丢失。
void write_lua_value(JsonWriter* writer, const LuaValue& value) {
    switch (value.kind) {
        case LuaValueKind::Null:
            writer->null();
            return;
        case LuaValueKind::Boolean:
            writer->boolean(value.boolean);
            return;
        case LuaValueKind::Number:
            writer->number(value.number);
            return;
        case LuaValueKind::String:
            writer->string(value.text);
            return;
        case LuaValueKind::Array:
            writer->begin_array();
            for (const LuaValue& item : value.values) {
                write_lua_value(writer, item);
            }
            writer->end_array();
            return;
        case LuaValueKind::Object:
            if (value.truncated) {
                writer->begin_object();
                writer->key("lua_type");
                writer->string("object");
                writer->key("entries");
                writer->begin_array();
                for (std::size_t index = 0;
                     index < value.keys.size() && index < value.values.size();
                     ++index) {
                    writer->begin_object();
                    writer->key("key");
                    writer->string(value.keys[index].text);
                    writer->key("value");
                    write_lua_value(writer, value.values[index]);
                    writer->end_object();
                }
                writer->end_array();
                writer->key("truncated");
                writer->boolean(true);
                writer->key("reason");
                if (value.reason.empty()) {
                    writer->null();
                } else {
                    writer->string(value.reason);
                }
                writer->end_object();
                return;
            }
            writer->begin_object();
            for (std::size_t index = 0;
                 index < value.keys.size() && index < value.values.size();
                 ++index) {
                writer->key(value.keys[index].text);
                write_lua_value(writer, value.values[index]);
            }
            writer->end_object();
            return;
        case LuaValueKind::Unsupported:
            writer->begin_object();
            writer->key("lua_type");
            writer->string("unsupported");
            writer->key("type");
            writer->string(value.text);
            writer->end_object();
            return;
        case LuaValueKind::Table:
            writer->begin_object();
            writer->key("lua_type");
            writer->string("table");
            writer->key("entries");
            writer->begin_array();
            for (std::size_t index = 0;
                 index < value.keys.size() && index < value.values.size();
                 ++index) {
                const LuaValueKey& key = value.keys[index];
                writer->begin_object();
                writer->key("key_type");
                switch (key.kind) {
                    case LuaValueKey::Kind::Number:
                        writer->string("number");
                        writer->key("key");
                        writer->number(key.number);
                        writer->key("lua_type");
                        writer->null();
                        break;
                    case LuaValueKey::Kind::String:
                        writer->string("string");
                        writer->key("key");
                        writer->string(key.text);
                        writer->key("lua_type");
                        writer->null();
                        break;
                    case LuaValueKey::Kind::Unsupported:
                        writer->string("unsupported");
                        writer->key("key");
                        writer->null();
                        writer->key("lua_type");
                        writer->string(key.lua_type);
                        break;
                }
                writer->key("value");
                write_lua_value(writer, value.values[index]);
                writer->end_object();
            }
            writer->end_array();
            writer->key("truncated");
            writer->boolean(value.truncated);
            writer->key("reason");
            if (value.reason.empty()) {
                writer->null();
            } else {
                writer->string(value.reason);
            }
            writer->end_object();
            return;
    }
}

/// 技能源同时保留调用可用性、动态值完整性和各自诊断。
void write_skill_effect_source(
    JsonWriter* writer,
    const SkillEffectSource& source) {
    writer->begin_object();
    writer->key("available");
    writer->boolean(source.available);
    writer->key("complete");
    writer->boolean(source.complete);
    writer->key("value");
    write_lua_value(writer, source.value);
    writer->key("error");
    if (source.error.has_value()) {
        writer->string(*source.error);
    } else {
        writer->null();
    }
    writer->key("read_errors");
    writer->begin_array();
    for (const std::string& read_error : source.read_errors) {
        writer->string(read_error);
    }
    writer->end_array();
    writer->end_object();
}

}  // namespace

// 三个装备命令操作始终返回原命令的同一份严格状态收据。
std::string encode_equipment_command_receipt(
    std::string_view request_id,
    const EquipmentCommandReceipt& receipt) {
    if (const std::optional<AgentError> error =
            validate_equipment_command_receipt(receipt);
        error.has_value()) {
        return encode_error(request_id, *error);
    }
    const char* status = "unknown";
    switch (receipt.status) {
        case EquipmentCommandStatus::Success:
            status = "success";
            break;
        case EquipmentCommandStatus::Failed:
            status = "failed";
            break;
        case EquipmentCommandStatus::Unknown:
            break;
    }
    const char* phase = "observing";
    switch (receipt.phase) {
        case EquipmentCommandPhase::Observing:
            break;
        case EquipmentCommandPhase::Succeeded:
            phase = "succeeded";
            break;
        case EquipmentCommandPhase::Failed:
            phase = "failed";
            break;
        case EquipmentCommandPhase::Uncertain:
            phase = "uncertain";
            break;
    }

    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("schema_version");
    writer.number(receipt.schema_version);
    writer.key("command_id");
    writer.string(receipt.command_id);
    writer.key("status");
    writer.string(status);
    writer.key("phase");
    writer.string(phase);
    writer.key("write_dispatched");
    writer.boolean(receipt.write_dispatched);
    writer.key("cancel_requested");
    writer.boolean(receipt.cancel_requested);
    writer.key("observation_count");
    writer.number(receipt.observation_count);
    writer.key("error_code");
    if (receipt.error_code.has_value()) {
        writer.string(*receipt.error_code);
    } else {
        writer.null();
    }
    writer.key("message");
    if (receipt.message.has_value()) {
        writer.string(*receipt.message);
    } else {
        writer.null();
    }
    writer.end_object();
    writer.end_object();
    return writer.take();
}

std::string encode_equipment_config_page(
    std::string_view request_id,
    const EquipmentConfigPage& page) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("schema_version");
    writer.number(1U);
    writer.key("complete");
    writer.boolean(page.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(page.configs.size()));
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(page.source.module_sha256);
    writer.end_object();
    if (page.selected_ids) {
        writer.key("missing_ids");
        writer.begin_array();
        for (auto id : page.missing_ids) writer.number(id);
        writer.end_array();
    } else {
        writer.key("start_index");
        writer.number(page.start_index);
        writer.key("total_count");
        writer.number(page.total_count);
        writer.key("next_index");
        if (page.next_index.has_value()) {
            writer.number(*page.next_index);
        } else {
            writer.null();
        }
    }
    writer.key("configs");
    writer.begin_array();
    for (const EquipmentConfigRecord& config : page.configs) {
        writer.begin_object();
        writer.key("config_id");
        writer.number(config.config_id);
        writer.key("root_config_id");
        if (config.root_config_id.has_value()) {
            writer.number(*config.root_config_id);
        } else {
            writer.null();
        }
        writer.key("raw_config");
        write_lua_value(&writer, config.raw_config);
        writer.key("attributes");
        write_lua_value(&writer, config.attributes);
        writer.key("properties");
        write_lua_value(&writer, config.properties);
        writer.key("skill");
        write_lua_value(&writer, config.skill);
        writer.key("property_rate");
        write_lua_value(&writer, config.property_rate);
        writer.key("weapon_ids");
        writer.begin_array();
        for (const std::uint64_t identifier : config.weapon_ids) {
            writer.number(identifier);
        }
        writer.end_array();
        writer.key("gear_score");
        if (config.gear_score.has_value()) {
            writer.number(*config.gear_score);
        } else {
            writer.null();
        }
        writer.key("anti_siren_power");
        if (config.anti_siren_power.has_value()) {
            writer.number(*config.anti_siren_power);
        } else {
            writer.null();
        }
        writer.key("is_device");
        if (config.is_device.has_value()) {
            writer.boolean(*config.is_device);
        } else {
            writer.null();
        }
        writer.key("is_aircraft");
        if (config.is_aircraft.has_value()) {
            writer.boolean(*config.is_aircraft);
        } else {
            writer.null();
        }
        writer.key("complete");
        writer.boolean(config.complete);
        writer.key("read_errors");
        writer.begin_array();
        for (const std::string& read_error : config.read_errors) {
            writer.string(read_error);
        }
        writer.end_array();
        writer.end_object();
    }
    writer.end_array();
    writer.key("read_errors");
    writer.begin_array();
    for (const EquipmentConfigReadError& read_error : page.read_errors) {
        writer.begin_object();
        writer.key("catalog_index");
        if (read_error.catalog_index.has_value()) {
            writer.number(*read_error.catalog_index);
        } else {
            writer.null();
        }
        writer.key("config_id");
        if (read_error.config_id.has_value()) {
            writer.number(*read_error.config_id);
        } else {
            writer.null();
        }
        writer.key("code");
        writer.string(read_error.code);
        writer.key("message");
        writer.string(read_error.message);
        writer.end_object();
    }
    writer.end_array();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

std::string encode_ship_catalog_page(
    std::string_view request_id,
    const ShipCatalogPage& page) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("table_key");
    writer.string(page.table_key);
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(page.module_sha256);
    writer.end_object();
    writer.key("start_index");
    writer.number(page.start_index);
    writer.key("total_count");
    writer.number(page.total_count);
    writer.key("next_index");
    if (page.next_index.has_value()) {
        writer.number(*page.next_index);
    } else {
        writer.null();
    }
    writer.key("records");
    writer.begin_array();
    for (const ShipCatalogRecord& record : page.records) {
        writer.begin_object();
        writer.key("id");
        writer.number(record.id);
        writer.key("raw");
        write_lua_value(&writer, record.raw);
        writer.end_object();
    }
    writer.end_array();
    writer.key("read_errors");
    writer.begin_array();
    for (const ShipCatalogReadError& read_error : page.read_errors) {
        writer.begin_object();
        writer.key("catalog_index");
        if (read_error.catalog_index.has_value()) {
            writer.number(*read_error.catalog_index);
        } else {
            writer.null();
        }
        writer.key("id");
        if (read_error.id.has_value()) {
            writer.number(*read_error.id);
        } else {
            writer.null();
        }
        writer.key("code");
        writer.string(read_error.code);
        writer.key("message");
        writer.string(read_error.message);
        writer.end_object();
    }
    writer.end_array();
    writer.key("complete");
    writer.boolean(page.complete);
    writer.end_object();
    writer.end_object();
    return writer.take();
}

std::string encode_compose_recipe_page(
    std::string_view request_id,
    const ComposeRecipePage& page) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("schema_version");
    writer.number(1U);
    writer.key("complete");
    writer.boolean(page.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(page.recipes.size()));
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(page.source.module_sha256);
    writer.end_object();
    writer.key("start_index");
    writer.number(page.start_index);
    writer.key("total_count");
    writer.number(page.total_count);
    writer.key("next_index");
    if (page.next_index.has_value()) {
        writer.number(*page.next_index);
    } else {
        writer.null();
    }
    writer.key("recipes");
    writer.begin_array();
    for (const EquipmentComposeRecipe& recipe : page.recipes) {
        writer.begin_object();
        writer.key("recipe_id");
        writer.number(recipe.recipe_id);
        writer.key("material_id");
        writer.number(recipe.material_id);
        writer.key("material_count");
        writer.number(recipe.material_count);
        writer.key("gold");
        writer.number(recipe.gold);
        writer.key("equipment_id");
        writer.number(recipe.equipment_id);
        writer.end_object();
    }
    writer.end_array();
    writer.key("read_errors");
    writer.begin_array();
    for (const ComposeRecipeReadError& read_error : page.read_errors) {
        writer.begin_object();
        writer.key("catalog_index");
        if (read_error.catalog_index.has_value()) {
            writer.number(*read_error.catalog_index);
        } else {
            writer.null();
        }
        writer.key("recipe_id");
        if (read_error.recipe_id.has_value()) {
            writer.number(*read_error.recipe_id);
        } else {
            writer.null();
        }
        writer.key("code");
        writer.string(read_error.code);
        writer.key("message");
        writer.string(read_error.message);
        writer.end_object();
    }
    writer.end_array();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

std::string encode_equipment_weapon_batch(
    std::string_view request_id,
    const EquipmentWeaponBatch& batch) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("schema_version");
    writer.number(1U);
    writer.key("complete");
    writer.boolean(batch.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(batch.weapons.size()));
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(batch.source.module_sha256);
    writer.end_object();
    writer.key("weapons");
    writer.begin_array();
    for (const EquipmentWeaponDetail& weapon : batch.weapons) {
        writer.begin_object();
        writer.key("weapon_id");
        writer.number(weapon.weapon_id);
        writer.key("raw");
        write_lua_value(&writer, weapon.raw);
        writer.key("complete");
        writer.boolean(weapon.complete);
        writer.key("read_errors");
        writer.begin_array();
        for (const std::string& read_error : weapon.read_errors) {
            writer.string(read_error);
        }
        writer.end_array();
        writer.end_object();
    }
    writer.end_array();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

std::string encode_skill_effect_batch(
    std::string_view request_id,
    const SkillEffectBatch& batch) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("schema_version");
    writer.number(1U);
    writer.key("complete");
    writer.boolean(batch.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(batch.skills.size()));
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(batch.source.module_sha256);
    writer.end_object();
    writer.key("skills");
    writer.begin_array();
    for (const SkillEffectDetail& skill : batch.skills) {
        writer.begin_object();
        writer.key("skill_id");
        writer.number(skill.skill_id);
        writer.key("level");
        writer.number(skill.level);
        writer.key("display");
        write_skill_effect_source(&writer, skill.display);
        writer.key("battle_skill");
        write_skill_effect_source(&writer, skill.battle_skill);
        writer.key("battle_buff");
        write_skill_effect_source(&writer, skill.battle_buff);
        writer.key("complete");
        writer.boolean(skill.complete);
        writer.end_object();
    }
    writer.end_array();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

std::string encode_equipment_reference_name_batch(
    std::string_view request_id,
    const EquipmentReferenceNameBatch& batch) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("schema_version");
    writer.number(1U);
    writer.key("complete");
    writer.boolean(batch.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(
        batch.equipment_types.size() + batch.nations.size() + batch.ship_types.size() +
        batch.attributes.size()));
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(batch.source.module_sha256);
    writer.end_object();

    const auto write_numeric_names = [&](
                                         std::string_view field_name,
                                         std::string_view identifier_name,
                                         const std::vector<EquipmentReferenceName>& records) {
        writer.key(field_name);
        writer.begin_array();
        for (const EquipmentReferenceName& record : records) {
            writer.begin_object();
            writer.key(identifier_name);
            writer.number(record.identifier);
            writer.key("name");
            if (record.name.has_value()) {
                writer.string(*record.name);
            } else {
                writer.null();
            }
            writer.key("error");
            if (record.error.has_value()) {
                writer.string(*record.error);
            } else {
                writer.null();
            }
            writer.end_object();
        }
        writer.end_array();
    };
    write_numeric_names("equipment_types", "equipment_type_id", batch.equipment_types);
    write_numeric_names("nations", "nation_id", batch.nations);
    write_numeric_names("ship_types", "ship_type_id", batch.ship_types);
    writer.key("attributes");
    writer.begin_array();
    for (const EquipmentAttributeName& record : batch.attributes) {
        writer.begin_object();
        writer.key("attribute_key");
        writer.string(record.key);
        writer.key("name");
        if (record.name.has_value()) {
            writer.string(*record.name);
        } else {
            writer.null();
        }
        writer.key("error");
        if (record.error.has_value()) {
            writer.string(*record.error);
        } else {
            writer.null();
        }
        writer.end_object();
    }
    writer.end_array();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

}  // namespace azlw::agent

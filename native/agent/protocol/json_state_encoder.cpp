// 编码背包、持有状态和舰船详情快照。

#include "json_codec.h"
#include "json_codec_internal.h"
#include "snapshots/owned_state_snapshot.h"

namespace azlw::agent {

using json_codec_internal::begin_ok_response;

void write_ship_detail(JsonWriter& writer, const ShipDetail& ship);

namespace {

void write_player_resources(JsonWriter& writer, const PlayerResources& player) {
    writer.begin_object();
    writer.key("gold");
    writer.number(player.gold);
    writer.key("equipment_capacity");
    writer.number(player.equipment_capacity);
    writer.key("equipment_limit");
    writer.number(player.equipment_limit);
    writer.end_object();
}

/// 写入装备的运行态 ID、配置 ID 和用户可见强化等级。
void write_equipment(JsonWriter* writer, const EquipmentSnapshot& equipment) {
    writer->begin_object();
    writer->key("equipment_id");
    writer->number(equipment.equipment_id);
    writer->key("config_id");
    writer->number(equipment.config_id);
    writer->key("enhance_level");
    writer->number(equipment.enhance_level);
    writer->end_object();
}

/// 写入背包快照值；独立操作和完整运行态操作共享同一子契约。
void write_bag_snapshot(JsonWriter* writer, const BagSnapshot& snapshot) {
    writer->begin_object();
    writer->key("schema_version");
    writer->number(1U);
    writer->key("complete");
    writer->boolean(snapshot.complete);
    writer->key("count");
    writer->number(static_cast<std::uint64_t>(snapshot.items.size()));
    writer->key("truncated");
    writer->boolean(snapshot.truncated);
    writer->key("items");
    writer->begin_array();
    for (const BagItem& item : snapshot.items) {
        writer->begin_object();
        writer->key("item_id");
        writer->number(item.item_id);
        writer->key("quantity");
        writer->number(item.quantity);
        writer->key("kind");
        writer->string("bag");
        writer->key("resolved_name");
        writer->string(item.resolved_name);
        writer->key("compose_recipe");
        if (!item.compose_recipe.has_value()) {
            writer->null();
        } else {
            const ComposeRecipe& recipe = *item.compose_recipe;
            writer->begin_object();
            writer->key("recipe_id");
            writer->number(recipe.recipe_id);
            writer->key("material_id");
            writer->number(recipe.material_id);
            writer->key("material_count");
            writer->number(recipe.material_count);
            writer->key("gold");
            writer->number(recipe.gold);
            writer->key("equipment_id");
            if (recipe.equipment_id.has_value()) {
                writer->number(*recipe.equipment_id);
            } else {
                writer->null();
            }
            writer->key("max_count");
            if (recipe.max_count.has_value()) {
                writer->number(*recipe.max_count);
            } else {
                writer->null();
            }
            writer->end_object();
        }
        writer->end_object();
    }
    writer->end_array();
    writer->key("read_errors");
    writer->begin_array();
    for (const ReadError& read_error : snapshot.read_errors) {
        writer->begin_object();
        writer->key("item_id");
        if (read_error.item_id.has_value()) {
            writer->number(*read_error.item_id);
        } else {
            writer->null();
        }
        writer->key("code");
        writer->string(read_error.code);
        writer->key("message");
        writer->string(read_error.message);
        writer->end_object();
    }
    writer->end_array();
    writer->end_object();
}

}  // namespace

// 显式写出所有可空字段，避免宿主把缺失字段误当作空值。
std::string encode_snapshot(std::string_view request_id, const BagSnapshot& snapshot) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    write_bag_snapshot(&writer, snapshot);
    writer.end_object();
    return writer.take();
}

void write_owned_ship(JsonWriter& writer, const OwnedShip& ship, const OwnedQuery* query,
                      const OwnedQueryShip* entry = nullptr) {
    writer.begin_object();
    writer.key("ship_id");
    writer.number(ship.ship_id);
    writer.key("config_id");
    writer.number(ship.config_id);
    if (entry && owned_query_has_field(*query, "name")) {
        writer.key("name"); writer.string(entry->name);
    }
    if (entry && entry->details) {
        writer.key("details"); write_ship_detail(writer, *entry->details);
    }
    if (!query || owned_query_has_field(*query, "level")) {
        writer.key("level");
        writer.number(ship.level);
    }
    if (!query || owned_query_has_field(*query, "experience_in_level")) {
        writer.key("experience_in_level");
        writer.number(ship.experience_in_level);
    }
    if (!query || owned_query_has_field(*query, "intimacy_raw")) {
        writer.key("intimacy_raw");
        writer.number(ship.intimacy_raw);
    }
    if (!query || owned_query_has_field(*query, "energy")) {
        writer.key("energy");
        writer.number(ship.energy);
    }
    if (!query || owned_query_has_field(*query, "proficiency")) {
        writer.key("proficiency");
        writer.number(ship.proficiency);
    }
    if (!query || owned_query_has_field(*query, "fleet_memberships")) {
        writer.key("fleet_memberships");
        writer.begin_array();
        for (const ShipFleetMembership& membership : ship.fleet_memberships) {
            writer.begin_object();
            writer.key("fleet_id");
            writer.number(membership.fleet_id);
            writer.key("display_name");
            if (membership.display_name.has_value()) {
                writer.string(*membership.display_name);
            } else {
                writer.null();
            }
            writer.key("kind");
            writer.string(membership.kind);
            writer.key("team");
            writer.string(membership.team);
            writer.key("position");
            writer.number(membership.position);
            writer.end_object();
        }
        writer.end_array();
    }
    if (!query || owned_query_has_field(*query, "skills")) {
        writer.key("skills");
        writer.begin_array();
        for (const OwnedShipSkill& skill : ship.skills) {
            writer.begin_object();
            writer.key("skill_id");
            writer.number(skill.skill_id);
            writer.key("level");
            writer.number(skill.level);
            writer.key("experience");
            writer.number(skill.experience);
            writer.end_object();
        }
        writer.end_array();
    }
    if (!query || owned_query_has_field(*query, "slots")) {
        writer.key("slots");
        writer.begin_array();
        for (const ShipEquipmentSlot& slot : ship.slots) {
            writer.begin_object();
            writer.key("slot_index");
            writer.number(slot.slot_index);
            writer.key("equipment");
            if (slot.equipment.has_value()) {
                write_equipment(&writer, *slot.equipment);
            } else {
                writer.null();
            }
            writer.end_object();
        }
        writer.end_array();
    }
    writer.end_object();
}

void write_owned_state_result(JsonWriter& writer, const OwnedStateSnapshot& snapshot) {
    writer.begin_object();
    writer.key("schema_version");
    writer.number(3U);
    writer.key("complete");
    writer.boolean(snapshot.complete);

    writer.key("dock");
    writer.begin_object();
    writer.key("complete");
    writer.boolean(snapshot.dock.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(snapshot.dock.ships.size()));
    writer.key("truncated");
    writer.boolean(snapshot.dock.truncated);
    writer.key("ships");
    writer.begin_array();
    for (const OwnedShip& ship : snapshot.dock.ships) {
        write_owned_ship(writer, ship, nullptr);
    }
    writer.end_array();
    writer.key("read_errors");
    writer.begin_array();
    for (const ShipReadError& read_error : snapshot.dock.read_errors) {
        writer.begin_object();
        writer.key("ship_id");
        if (read_error.ship_id.has_value()) {
            writer.number(*read_error.ship_id);
        } else {
            writer.null();
        }
        writer.key("skill_id");
        if (read_error.skill_id.has_value()) {
            writer.number(*read_error.skill_id);
        } else {
            writer.null();
        }
        writer.key("slot_index");
        if (read_error.slot_index.has_value()) {
            writer.number(*read_error.slot_index);
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

    writer.key("warehouse");
    writer.begin_object();
    writer.key("complete");
    writer.boolean(snapshot.warehouse.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(snapshot.warehouse.items.size()));
    writer.key("truncated");
    writer.boolean(snapshot.warehouse.truncated);
    writer.key("items");
    writer.begin_array();
    for (const WarehouseEquipment& item : snapshot.warehouse.items) {
        writer.begin_object();
        writer.key("equipment_id");
        writer.number(item.equipment.equipment_id);
        writer.key("config_id");
        writer.number(item.equipment.config_id);
        writer.key("quantity");
        writer.number(item.quantity);
        writer.key("enhance_level");
        writer.number(item.equipment.enhance_level);
        writer.end_object();
    }
    writer.end_array();
    writer.key("read_errors");
    writer.begin_array();
    for (const EquipmentReadError& read_error : snapshot.warehouse.read_errors) {
        writer.begin_object();
        writer.key("equipment_id");
        if (read_error.equipment_id.has_value()) {
            writer.number(*read_error.equipment_id);
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

    writer.key("bag");
    write_bag_snapshot(&writer, snapshot.bag);
    writer.key("player");
    write_player_resources(writer, snapshot.player);
    writer.end_object();
}

std::string encode_resources_snapshot(std::string_view request_id, const PlayerResources& player) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    write_player_resources(writer, player);
    writer.end_object();
    return writer.take();
}

std::string encode_owned_state_snapshot(
    std::string_view request_id,
    const OwnedStateSnapshot& snapshot) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    write_owned_state_result(writer, snapshot);
    writer.end_object();
    return writer.take();
}

void write_ship_detail(JsonWriter& writer, const ShipDetail& ship) {
    const auto write_attributes = [](JsonWriter* attributes_writer, const ShipAttributeSet& attributes) {
        attributes_writer->begin_object();
        attributes_writer->key("durability");
        attributes_writer->number(attributes.durability);
        attributes_writer->key("cannon");
        attributes_writer->number(attributes.cannon);
        attributes_writer->key("torpedo");
        attributes_writer->number(attributes.torpedo);
        attributes_writer->key("anti_aircraft");
        attributes_writer->number(attributes.anti_aircraft);
        attributes_writer->key("air");
        attributes_writer->number(attributes.air);
        attributes_writer->key("reload");
        attributes_writer->number(attributes.reload);
        attributes_writer->key("hit");
        attributes_writer->number(attributes.hit);
        attributes_writer->key("dodge");
        attributes_writer->number(attributes.dodge);
        attributes_writer->key("anti_sub");
        attributes_writer->number(attributes.anti_sub);
        attributes_writer->key("luck");
        attributes_writer->number(attributes.luck);
        attributes_writer->key("speed");
        attributes_writer->number(attributes.speed);
        attributes_writer->end_object();
    };
    writer.begin_object();
    writer.key("ship_id");
    writer.number(ship.ship_id);
    writer.key("config_id");
    writer.number(ship.config_id);
    writer.key("name");
    writer.string(ship.name);
    writer.key("level");
    writer.number(ship.level);
    writer.key("max_level");
    writer.number(ship.max_level);
    writer.key("experience_in_level");
    writer.number(ship.experience_in_level);
    writer.key("total_experience");
    writer.number(ship.total_experience);
    writer.key("next_level_experience");
    writer.number(ship.next_level_experience);
    writer.key("intimacy_raw");
    writer.number(ship.intimacy_raw);
    writer.key("intimacy_maximum");
    writer.number(ship.intimacy_maximum);
    writer.key("intimacy_stage_id");
    writer.number(ship.intimacy_stage_id);
    writer.key("intimacy_stage_description");
    writer.string(ship.intimacy_stage_description);
    writer.key("proposed");
    writer.boolean(ship.proposed);
    writer.key("propose_time");
    writer.number(ship.propose_time);
    writer.key("create_time");
    writer.number(ship.create_time);
    writer.key("combat_power");
    writer.number(ship.combat_power);
    writer.key("locked");
    writer.boolean(ship.locked);
    writer.key("oil_cost");
    writer.begin_object();
    writer.key("start");
    writer.number(ship.oil_cost_start);
    writer.key("end");
    writer.number(ship.oil_cost_end);
    writer.key("total");
    writer.number(ship.oil_cost_total);
    writer.end_object();
    writer.key("classification");
    writer.begin_object();
    writer.key("group_id");
    writer.number(ship.classification.group_id);
    writer.key("ship_type_id");
    writer.number(ship.classification.ship_type_id);
    writer.key("ship_type_name");
    writer.string(ship.classification.ship_type_name);
    writer.key("armor_type_id");
    writer.number(ship.classification.armor_type_id);
    writer.key("armor_type_name");
    writer.string(ship.classification.armor_type_name);
    writer.key("nation_id");
    writer.number(ship.classification.nation_id);
    writer.key("nation_name");
    writer.string(ship.classification.nation_name);
    writer.key("rarity");
    writer.number(ship.classification.rarity);
    writer.key("star");
    writer.number(ship.classification.star);
    writer.key("max_star");
    writer.number(ship.classification.max_star);
    writer.key("skin_id");
    writer.number(ship.classification.skin_id);
    writer.end_object();
    writer.key("base_attributes");
    write_attributes(&writer, ship.base_attributes);
    writer.key("equipment_applied_attributes");
    write_attributes(&writer, ship.equipment_applied_attributes);
    writer.key("effective_attributes");
    write_attributes(&writer, ship.effective_attributes);
    writer.key("slot_rules");
    writer.begin_array();
    for (const ShipEquipmentSlotRule& rule : ship.slot_rules) {
        writer.begin_object();
        writer.key("slot_index");
        writer.number(rule.slot_index);
        writer.key("allowed_equipment_type_ids");
        writer.begin_array();
        for (const std::uint64_t equipment_type_id : rule.allowed_equipment_type_ids) {
            writer.number(equipment_type_id);
        }
        writer.end_array();
        writer.end_object();
    }
    writer.end_array();
    writer.key("skills");
    writer.begin_array();
    for (const ShipSkillDetail& skill : ship.skills) {
        writer.begin_object();
        writer.key("skill_id");
        writer.number(skill.skill_id);
        writer.key("effective_skill_id");
        writer.number(skill.effective_skill_id);
        writer.key("name");
        writer.string(skill.name);
        writer.key("level");
        writer.number(skill.level);
        writer.key("max_level");
        writer.number(skill.max_level);
        writer.key("experience");
        writer.number(skill.experience);
        writer.key("next_level_experience");
        writer.number(skill.next_level_experience);
        writer.key("description_template");
        writer.string(skill.description_template);
        writer.key("current_effect");
        writer.string(skill.current_effect);
        writer.end_object();
    }
    writer.end_array();
    writer.end_object();
}

void write_ship_details_result(JsonWriter& writer, const ShipDetailsSnapshot& snapshot) {
    writer.begin_object();
    writer.key("schema_version");
    writer.number(4U);
    writer.key("complete");
    writer.boolean(snapshot.complete);
    writer.key("count");
    writer.number(static_cast<std::uint64_t>(snapshot.ships.size()));
    writer.key("truncated");
    writer.boolean(snapshot.truncated);
    writer.key("source");
    writer.begin_object();
    writer.key("module_sha256");
    writer.string(snapshot.source.module_sha256);
    writer.end_object();
    writer.key("ships");
    writer.begin_array();
    for (const ShipDetail& ship : snapshot.ships) {
        write_ship_detail(writer, ship);
    }
    writer.end_array();
    writer.key("read_errors");
    writer.begin_array();
    for (const ShipDetailReadError& read_error : snapshot.read_errors) {
        writer.begin_object();
        writer.key("ship_id");
        if (read_error.ship_id.has_value()) {
            writer.number(*read_error.ship_id);
        } else {
            writer.null();
        }
        writer.key("skill_id");
        if (read_error.skill_id.has_value()) {
            writer.number(*read_error.skill_id);
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
}

std::string encode_ship_details_snapshot(
    std::string_view request_id,
    const ShipDetailsSnapshot& snapshot) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    write_ship_details_result(writer, snapshot);
    writer.end_object();
    return writer.take();
}

std::string encode_account_before_snapshot(
    std::string_view request_id,
    const OwnedStateSnapshot& owned,
    const ShipDetailsSnapshot& details,
    std::uint32_t dock_frames) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("owned_state");
    write_owned_state_result(writer, owned);
    writer.key("ship_details");
    write_ship_details_result(writer, details);
    writer.key("dock_frames");
    writer.number(dock_frames);
    writer.end_object();
    writer.end_object();
    return writer.take();
}


std::string encode_owned_query(std::string_view request_id, const OwnedQueryExecution& execution) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("entries"); writer.begin_array();
    for (const auto& entry : execution.ships) write_owned_ship(writer, entry.ship, &execution.query, &entry);
    for (const auto& entry : execution.equipment) {
        writer.begin_object();
        writer.key("config_id"); writer.number(entry.equipment.config_id);
        if (owned_query_has_field(execution.query, "enhance_level")) {
            writer.key("enhance_level"); writer.number(entry.equipment.enhance_level);
        }
        if (owned_query_has_field(execution.query, "warehouse_quantity")) {
            writer.key("warehouse_quantity"); writer.number(entry.warehouse_quantity);
        }
        if (owned_query_has_field(execution.query, "equipped")) {
            writer.key("equipped"); writer.begin_array();
            for (const auto& location : entry.equipped) {
                writer.begin_object();
                writer.key("ship_id"); writer.number(location.ship_id);
                writer.key("slot_index"); writer.number(location.slot_index);
                writer.key("equipment_id"); writer.number(location.equipment_id);
                writer.end_object();
            }
            writer.end_array();
        }
        writer.end_object();
    }
    writer.end_array();
    writer.key("missing_ids"); writer.begin_array();
    for (const auto id : execution.missing_ids) writer.number(id);
    writer.end_array(); writer.end_object(); writer.end_object();
    return writer.take();
}

}  // namespace azlw::agent

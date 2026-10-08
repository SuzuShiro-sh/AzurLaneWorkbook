// 实现舰船展示详情的严格 Lua 读取、字段交叉校验和确定性排序。

#include "ship_details_snapshot.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <exception>
#include <limits>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>

#include "bay_ship_reader.h"
#include "lua/lua_reader.h"
#include "ship_skill_reader.h"

namespace azlw::agent {
namespace {

constexpr std::size_t kMaximumDescriptionBytes = 4 * 1024;

/// 把单艘舰船或技能失败映射为详情快照的定位诊断。
void add_detail_error(
    ShipDetailsSnapshot* snapshot,
    std::optional<std::uint64_t> ship_id,
    std::optional<std::uint64_t> skill_id,
    std::string code,
    std::string message) {
    snapshot->read_errors.push_back(ShipDetailReadError{
        .ship_id = ship_id,
        .skill_id = skill_id,
        .code = std::move(code),
        .message = std::move(message),
    });
}

/// 读取必需的精确整数对象字段，并区分是否允许零值。
bool read_required_number_field(
    const LuaApi& api,
    lua_State* state,
    int object_index,
    const char* field_name,
    bool positive,
    std::uint64_t* output,
    std::string* error) {
    const LuaNumber value = read_lua_number_field(api, state, object_index, field_name);
    if (value.status != LuaNumberStatus::Present || (positive && value.value == 0)) {
        *error = std::string(field_name) +
                 (positive ? " 缺失或不是正整数" : " 缺失或不是非负整数");
        return false;
    }
    *output = value.value;
    return true;
}

/// 将已经校验的非负整数缩窄为 u32，拒绝静默截断。
bool narrow_u32(std::uint64_t value, std::string_view field_name, std::uint32_t* output, std::string* error) {
    if (value > std::numeric_limits<std::uint32_t>::max()) {
        *error = std::string(field_name) + " 超出 u32";
        return false;
    }
    *output = static_cast<std::uint32_t>(value);
    return true;
}

struct AttributeField final {
    const char* lua_name;
    double ShipAttributeSet::*member;
};

constexpr std::array kAttributeFields{
    AttributeField{"durability", &ShipAttributeSet::durability},
    AttributeField{"cannon", &ShipAttributeSet::cannon},
    AttributeField{"torpedo", &ShipAttributeSet::torpedo},
    AttributeField{"antiaircraft", &ShipAttributeSet::anti_aircraft},
    AttributeField{"air", &ShipAttributeSet::air},
    AttributeField{"reload", &ShipAttributeSet::reload},
    AttributeField{"hit", &ShipAttributeSet::hit},
    AttributeField{"dodge", &ShipAttributeSet::dodge},
    AttributeField{"antisub", &ShipAttributeSet::anti_sub},
    AttributeField{"luck", &ShipAttributeSet::luck},
    AttributeField{"speed", &ShipAttributeSet::speed},
};

/// 从一个客户端属性方法返回的 table 读取全部受支持面板字段。
bool read_attribute_stage(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    const char* method_name,
    std::span<const LuaCallArgument> arguments,
    ShipAttributeSet* output,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_method_result(
            api,
            state,
            ship_index,
            method_name,
            arguments,
            error)) {
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = "方法 " + std::string(method_name) + " 未返回属性 table";
        return false;
    }
    const int table_index = api.get_top(state);
    for (const AttributeField& field : kAttributeFields) {
        if (api.get_field_protected(state, table_index, field.lua_name) != 0) {
            *error = lua_failure_detail(
                api,
                state,
                "读取 " + std::string(method_name) + "." + field.lua_name + " 失败");
            api.set_top(state, top);
            return false;
        }
        const std::optional<double> value = read_lua_nonnegative_real(api, state, -1);
        api.set_top(state, table_index);
        if (!value.has_value()) {
            api.set_top(state, top);
            *error = "属性 " + std::string(method_name) + "." + field.lua_name +
                     " 不是非负有限数值";
            return false;
        }
        output->*(field.member) = *value;
    }
    api.set_top(state, top);
    return true;
}

/// 读取等级经验表中的 `exp_interval`；满级时客户端可能不再提供下一档配置。
bool read_next_level_experience(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    std::uint32_t level,
    std::uint32_t max_level,
    std::uint64_t* output,
    std::string* error) {
    if (level >= max_level) {
        *output = 0;
        return true;
    }
    const int top = api.get_top(state);
    if (!push_lua_method_result(api, state, ship_index, "getLevelExpConfig", {}, error)) {
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = "方法 getLevelExpConfig 未返回 table";
        return false;
    }
    const LuaNumber value = read_lua_number_field(api, state, -1, "exp_interval");
    api.set_top(state, top);
    if (value.status != LuaNumberStatus::Present || value.value == 0) {
        *error = "getLevelExpConfig.exp_interval 缺失或不是正整数";
        return false;
    }
    *output = value.value;
    return true;
}

/// 读取舰种、装甲、阵营和展示分类，并以对象方法覆盖易随突破变化的字段。
bool read_classification(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    ShipClassification* output,
    std::string* error) {
    if (!read_required_number_field(
            api, state, ship_index, "groupId", true, &output->group_id, error)) {
        return false;
    }

    const int top = api.get_top(state);
    if (!push_lua_method_result(api, state, ship_index, "getConfigTable", {}, error)) {
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = "方法 getConfigTable 未返回 table";
        return false;
    }
    const int config_index = api.get_top(state);
    std::uint64_t ignored_rarity = 0;
    std::uint64_t ignored_star = 0;
    std::uint64_t ignored_skin = 0;
    if (!read_required_number_field(
            api, state, config_index, "type", true, &output->ship_type_id, error) ||
        !read_required_number_field(
            api, state, config_index, "armor_type", true, &output->armor_type_id, error) ||
        !read_required_number_field(
            api, state, config_index, "nationality", true, &output->nation_id, error) ||
        !read_required_number_field(
            api, state, config_index, "rarity", true, &ignored_rarity, error) ||
        !read_required_number_field(
            api, state, config_index, "star", true, &ignored_star, error) ||
        !read_required_number_field(
            api, state, config_index, "skin_id", true, &ignored_skin, error)) {
        api.set_top(state, top);
        return false;
    }
    api.set_top(state, top);

    const std::optional<std::uint64_t> rarity =
        read_lua_number_method(api, state, ship_index, "getRarity", {}, error);
    const std::optional<std::uint64_t> star =
        rarity.has_value()
            ? read_lua_number_method(api, state, ship_index, "getStar", {}, error)
            : std::nullopt;
    const std::optional<std::uint64_t> max_star =
        star.has_value()
            ? read_lua_number_method(api, state, ship_index, "getMaxStar", {}, error)
            : std::nullopt;
    const std::optional<std::uint64_t> skin_id =
        max_star.has_value()
            ? read_lua_number_method(api, state, ship_index, "getSkinId", {}, error)
            : std::nullopt;
    if (!rarity.has_value() || !star.has_value() || !max_star.has_value() ||
        !skin_id.has_value() || *rarity == 0 || *star == 0 || *max_star == 0 ||
        *skin_id == 0 || !narrow_u32(*rarity, "rarity", &output->rarity, error) ||
        !narrow_u32(*star, "star", &output->star, error) ||
        !narrow_u32(*max_star, "max_star", &output->max_star, error)) {
        if (error->empty()) {
            *error = "分类方法返回了零值或越界值";
        }
        return false;
    }
    output->skin_id = *skin_id;
    if (output->star > output->max_star) {
        *error = "当前星级高于最大星级";
        return false;
    }

    const std::optional<std::string> armor_type_name = read_lua_text_method(
        api, state, ship_index, "getShipArmorName", {}, 512, false, error);
    if (!armor_type_name.has_value()) {
        return false;
    }
    output->armor_type_name = *armor_type_name;
    const std::optional<std::string> ship_type_name = read_lua_static_name(
        api, state, "ShipType", "Type2Name", output->ship_type_id, error);
    if (!ship_type_name.has_value()) {
        return false;
    }
    output->ship_type_name = *ship_type_name;
    const std::optional<std::string> nation_name =
        read_lua_static_name(api, state, "Nation", "Nation2Name", output->nation_id, error);
    if (!nation_name.has_value()) {
        return false;
    }
    output->nation_name = *nation_name;
    return true;
}

/// 从当前突破阶段的舰船模板读取五个槽位允许的装备类型，并规范为升序集合。
bool read_equipment_slot_rules(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t config_id,
    std::vector<ShipEquipmentSlotRule>* output,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "pg", error)) {
        api.set_top(state, top);
        return false;
    }
    const int pg_index = api.get_top(state);
    if (!push_lua_table_field(api, state, pg_index, "ship_data_template", error)) {
        *error = "读取 pg.ship_data_template 失败: " + *error;
        api.set_top(state, top);
        return false;
    }
    const int template_index = api.get_top(state);
    const std::string row_path =
        "pg.ship_data_template[" + std::to_string(config_id) + "]";
    if (api.get_number_index_protected(
            state, template_index, static_cast<double>(config_id)) != 0) {
        *error = lua_failure_detail(api, state, "读取 " + row_path + " 失败");
        api.set_top(state, top);
        return false;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = row_path + " 不是 table";
        return false;
    }

    const int config_index = api.get_top(state);
    output->clear();
    output->reserve(kShipEquipmentSlotCount);
    for (std::uint32_t slot_index = 1; slot_index <= kShipEquipmentSlotCount; ++slot_index) {
        const std::string field_name = "equip_" + std::to_string(slot_index);
        if (api.get_field_protected(state, config_index, field_name.c_str()) != 0) {
            *error = lua_failure_detail(
                api,
                state,
                "读取 " + row_path + "." + field_name + " 失败");
            api.set_top(state, top);
            return false;
        }
        if (api.type(state, -1) != kLuaTypeTable) {
            api.set_top(state, top);
            *error = row_path + "." + field_name + " 不是 table";
            return false;
        }

        const int types_index = api.get_top(state);
        const std::size_t type_count = api.object_length(state, types_index);
        if (type_count == 0 || type_count > kMaximumShipSlotEquipmentTypeCount) {
            api.set_top(state, top);
            *error = row_path + "." + field_name + " 的装备类型数量必须为 1 至 " +
                     std::to_string(kMaximumShipSlotEquipmentTypeCount);
            return false;
        }

        ShipEquipmentSlotRule rule{
            .slot_index = slot_index,
            .allowed_equipment_type_ids = {},
        };
        rule.allowed_equipment_type_ids.reserve(type_count);
        for (std::size_t type_index = 1; type_index <= type_count; ++type_index) {
            if (api.get_number_index_protected(
                    state,
                    types_index,
                    static_cast<double>(type_index)) != 0) {
                *error = lua_failure_detail(
                    api,
                    state,
                    "读取 " + row_path + "." + field_name + " 的装备类型失败");
                api.set_top(state, top);
                return false;
            }
            const LuaNumber equipment_type_id = read_lua_number(api, state, -1);
            api.set_top(state, types_index);
            if (equipment_type_id.status != LuaNumberStatus::Present ||
                equipment_type_id.value == 0) {
                api.set_top(state, top);
                *error = row_path + "." + field_name + " 包含非正整数装备类型";
                return false;
            }
            rule.allowed_equipment_type_ids.push_back(equipment_type_id.value);
        }

        std::sort(
            rule.allowed_equipment_type_ids.begin(),
            rule.allowed_equipment_type_ids.end());
        if (std::adjacent_find(
                rule.allowed_equipment_type_ids.begin(),
                rule.allowed_equipment_type_ids.end()) !=
            rule.allowed_equipment_type_ids.end()) {
            api.set_top(state, top);
            *error = row_path + "." + field_name + " 包含重复装备类型";
            return false;
        }
        output->push_back(std::move(rule));
        api.set_top(state, config_index);
    }

    api.set_top(state, top);
    return true;
}

/// 从当前客户端静态技能表读取未经等级插值的说明模板。
std::optional<std::string> read_skill_description_template(
    const LuaApi& api,
    lua_State* state,
    std::uint64_t effective_skill_id,
    std::string* error) {
    const int top = api.get_top(state);
    if (!push_lua_global_table(api, state, "pg", error)) {
        api.set_top(state, top);
        return std::nullopt;
    }
    const int pg_index = api.get_top(state);
    if (api.get_field_protected(state, pg_index, "skill_data_template") != 0) {
        *error = lua_failure_detail(api, state, "读取 pg.skill_data_template 失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = "pg.skill_data_template 不是 table";
        return std::nullopt;
    }
    const int skills_index = api.get_top(state);
    if (api.get_number_index_protected(
            state, skills_index, static_cast<double>(effective_skill_id)) != 0) {
        *error = lua_failure_detail(api, state, "读取技能静态配置失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    if (api.type(state, -1) != kLuaTypeTable) {
        api.set_top(state, top);
        *error = "技能静态配置不是 table";
        return std::nullopt;
    }
    const int config_index = api.get_top(state);
    if (api.get_field_protected(state, config_index, "desc") != 0) {
        *error = lua_failure_detail(api, state, "读取技能 desc 失败");
        api.set_top(state, top);
        return std::nullopt;
    }
    std::optional<std::string> result =
        read_lua_text(api, state, -1, kMaximumDescriptionBytes, true, error);
    api.set_top(state, top);
    if (!result.has_value()) {
        *error = "技能 desc 无效: " + *error;
    }
    return result;
}

/// 构造客户端 `ShipSkill` 展示对象，并交叉核对原始等级与经验。
bool read_skill_detail(
    const LuaApi& api,
    lua_State* state,
    int raw_skill_index,
    std::uint64_t ship_id,
    const OwnedShipSkill& raw_skill,
    ShipSkillDetail* output,
    std::string* error) {
    const int top = api.get_top(state);
    const int stable_raw_skill_index = api.stable_stack_index(state, raw_skill_index);
    if (!push_lua_global_table(api, state, "ShipSkill", error)) {
        api.set_top(state, top);
        return false;
    }
    const int ship_skill_table = api.get_top(state);
    const std::array arguments{
        LuaCallArgument::stack_value(stable_raw_skill_index),
        LuaCallArgument::number(static_cast<double>(ship_id)),
    };
    if (!push_lua_table_function_result(
            api, state, ship_skill_table, "New", arguments, error)) {
        api.set_top(state, top);
        return false;
    }
    const int skill_index = api.get_top(state);
    const int skill_type = api.type(state, skill_index);
    if (skill_type != kLuaTypeTable && skill_type != kLuaTypeUserData) {
        api.set_top(state, top);
        *error = "ShipSkill.New 未返回 table 或 userdata";
        return false;
    }

    std::uint64_t level = 0;
    std::uint64_t max_level = 0;
    std::uint64_t experience = 0;
    if (!read_required_number_field(api, state, skill_index, "level", true, &level, error) ||
        !read_required_number_field(api, state, skill_index, "maxLevel", true, &max_level, error) ||
        !read_required_number_field(api, state, skill_index, "exp", false, &experience, error) ||
        !narrow_u32(level, "skill.level", &output->level, error) ||
        !narrow_u32(max_level, "skill.max_level", &output->max_level, error)) {
        api.set_top(state, top);
        return false;
    }
    if (output->level > output->max_level || output->level != raw_skill.level ||
        experience != raw_skill.experience) {
        api.set_top(state, top);
        *error = "ShipSkill.New 的等级或经验与 Ship.skills 原始值不一致";
        return false;
    }

    const std::optional<std::uint64_t> effective_id =
        read_lua_number_method(api, state, skill_index, "GetDisplayId", {}, error);
    const std::optional<std::string> name =
        effective_id.has_value()
            ? read_lua_text_method(api, state, skill_index, "GetName", {}, 512, false, error)
            : std::nullopt;
    const std::optional<std::string> current_effect =
        name.has_value()
            ? read_lua_text_method(
                  api,
                  state,
                  skill_index,
                  "GetDesc",
                  {},
                  kMaximumDescriptionBytes,
                  true,
                  error)
            : std::nullopt;
    if (!effective_id.has_value() || *effective_id == 0 || !name.has_value() ||
        !current_effect.has_value()) {
        api.set_top(state, top);
        if (error->empty()) {
            *error = "技能展示方法返回了无效值";
        }
        return false;
    }
    std::uint64_t next_level_experience = 0;
    if (output->level < output->max_level) {
        const std::optional<std::uint64_t> next =
            read_lua_number_method(api, state, skill_index, "GetNextLevelExp", {}, error);
        if (!next.has_value() || *next == 0) {
            api.set_top(state, top);
            if (next.has_value()) {
                *error = "未满级技能的 GetNextLevelExp 返回了零值";
            }
            return false;
        }
        next_level_experience = *next;
    }
    const std::optional<std::string> description_template =
        read_skill_description_template(api, state, *effective_id, error);
    if (!description_template.has_value()) {
        api.set_top(state, top);
        return false;
    }

    output->skill_id = raw_skill.skill_id;
    output->effective_skill_id = *effective_id;
    output->name = *name;
    output->experience = experience;
    output->next_level_experience = next_level_experience;
    output->description_template = *description_template;
    output->current_effect = *current_effect;
    api.set_top(state, top);
    return true;
}

}  // namespace

/// 读取单艘舰船的所有详情；任一字段失败时不输出部分对象。
bool read_ship_detail(
    const LuaApi& api,
    lua_State* state,
    int ship_index,
    std::uint64_t ship_id,
    ShipDetail* output,
    ShipSkillVisitError* detail_error) {
    ShipDetail candidate;
    candidate.ship_id = ship_id;
    std::uint64_t level = 0;
    std::uint64_t max_level = 0;
    std::string error;
    const std::optional<std::string> name =
        read_lua_text_field(api, state, ship_index, "name", 512, false, &error);
    if (!name.has_value() ||
        !read_required_number_field(
            api, state, ship_index, "configId", true, &candidate.config_id, &error) ||
        !read_required_number_field(api, state, ship_index, "level", true, &level, &error) ||
        !read_required_number_field(
            api, state, ship_index, "maxLevel", true, &max_level, &error) ||
        !narrow_u32(level, "level", &candidate.level, &error) ||
        !narrow_u32(max_level, "max_level", &candidate.max_level, &error)) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_growth_invalid",
            .message = std::move(error),
        };
        return false;
    }
    candidate.name = *name;
    if (candidate.level > candidate.max_level) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_growth_invalid",
            .message = "舰船当前等级高于等级上限",
        };
        return false;
    }

    const std::optional<bool> proposed =
        read_lua_boolean_field(api, state, ship_index, "propose", &error);
    if (!read_required_number_field(
            api,
            state,
            ship_index,
            "exp",
            false,
            &candidate.experience_in_level,
            &error) ||
        !read_required_number_field(
            api, state, ship_index, "intimacy", false, &candidate.intimacy_raw, &error) ||
        !read_required_number_field(
            api, state, ship_index, "proposeTime", false, &candidate.propose_time, &error) ||
        !read_required_number_field(
            api, state, ship_index, "createTime", false, &candidate.create_time, &error) ||
        !proposed.has_value()) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_growth_invalid",
            .message = std::move(error),
        };
        return false;
    }
    candidate.proposed = *proposed;

    const std::optional<std::uint64_t> total_experience =
        read_lua_number_method(api, state, ship_index, "getTotalExp", {}, &error);
    const std::optional<std::uint64_t> intimacy_maximum =
        total_experience.has_value()
            ? read_lua_number_method(api, state, ship_index, "getIntimacyMax", {}, &error)
            : std::nullopt;
    const std::optional<std::uint64_t> intimacy_stage_id =
        intimacy_maximum.has_value()
            ? read_lua_number_method(api, state, ship_index, "getIntimacyLevel", {}, &error)
            : std::nullopt;
    const std::optional<std::string> intimacy_stage_description =
        intimacy_stage_id.has_value() && *intimacy_stage_id > 0
            ? read_lua_pg_config_text(
                  api,
                  state,
                  "intimacy_template",
                  *intimacy_stage_id,
                  "desc",
                  &error)
            : std::nullopt;
    const std::optional<std::uint64_t> combat_power =
        intimacy_stage_description.has_value()
            ? read_lua_number_method(api, state, ship_index, "getShipCombatPower", {}, &error)
            : std::nullopt;
    const std::optional<bool> locked =
        combat_power.has_value()
            ? read_lua_boolean_method(api, state, ship_index, "IsLocked", {}, &error)
            : std::nullopt;
    if (!total_experience.has_value() || !intimacy_maximum.has_value() ||
        !intimacy_stage_id.has_value() || *intimacy_stage_id == 0 ||
        !intimacy_stage_description.has_value() || !combat_power.has_value() ||
        !locked.has_value()) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_method_invalid",
            .message = std::move(error),
        };
        return false;
    }
    candidate.total_experience = *total_experience;
    candidate.intimacy_maximum = *intimacy_maximum;
    candidate.intimacy_stage_id = *intimacy_stage_id;
    candidate.intimacy_stage_description = *intimacy_stage_description;
    candidate.combat_power = *combat_power;
    candidate.locked = *locked;
    if (!read_next_level_experience(
            api,
            state,
            ship_index,
            candidate.level,
            candidate.max_level,
            &candidate.next_level_experience,
            &error)) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_experience_invalid",
            .message = std::move(error),
        };
        return false;
    }

    const std::optional<std::uint64_t> oil_start =
        read_lua_number_method(api, state, ship_index, "getStartBattleExpend", {}, &error);
    const std::optional<std::uint64_t> oil_end =
        oil_start.has_value()
            ? read_lua_number_method(api, state, ship_index, "getEndBattleExpend", {}, &error)
            : std::nullopt;
    const std::optional<std::uint64_t> oil_total =
        oil_end.has_value()
            ? read_lua_number_method(api, state, ship_index, "getBattleTotalExpend", {}, &error)
            : std::nullopt;
    if (!oil_start.has_value() || !oil_end.has_value() || !oil_total.has_value() ||
        *oil_start > std::numeric_limits<std::uint64_t>::max() - *oil_end ||
        *oil_start + *oil_end != *oil_total) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_oil_cost_invalid",
            .message = oil_total.has_value()
                           ? "客户端三种油耗方法的结果不一致"
                           : std::move(error),
        };
        return false;
    }
    candidate.oil_cost_start = *oil_start;
    candidate.oil_cost_end = *oil_end;
    candidate.oil_cost_total = *oil_total;

    const std::array equipment_arguments{
        LuaCallArgument::nil(),
        LuaCallArgument::boolean(true),
    };
    if (!read_classification(api, state, ship_index, &candidate.classification, &error) ||
        !read_equipment_slot_rules(
            api,
            state,
            candidate.config_id,
            &candidate.slot_rules,
            &error) ||
        !read_attribute_stage(
            api,
            state,
            ship_index,
            "getShipProperties",
            {},
            &candidate.base_attributes,
            &error) ||
        !read_attribute_stage(
            api,
            state,
            ship_index,
            "getProperties",
            equipment_arguments,
            &candidate.equipment_applied_attributes,
            &error) ||
        !read_attribute_stage(
            api,
            state,
            ship_index,
            "getProperties",
            {},
            &candidate.effective_attributes,
            &error)) {
        *detail_error = ShipSkillVisitError{
            .skill_id = std::nullopt,
            .code = "ship_detail_display_invalid",
            .message = std::move(error),
        };
        return false;
    }

    candidate.skills.reserve(4);
    ShipSkillVisitError skill_error;
    if (!visit_owned_ship_skills(
            api,
            state,
            ship_index,
            [&](int skill_index, const OwnedShipSkill& raw_skill, ShipSkillVisitError* error_out) {
                ShipSkillDetail detail;
                std::string conversion_error;
                if (!read_skill_detail(
                        api,
                        state,
                        skill_index,
                        ship_id,
                        raw_skill,
                        &detail,
                        &conversion_error)) {
                    *error_out = ShipSkillVisitError{
                        .skill_id = raw_skill.skill_id,
                        .code = "ship_skill_detail_invalid",
                        .message = std::move(conversion_error),
                    };
                    return false;
                }
                candidate.skills.push_back(std::move(detail));
                return true;
            },
            &skill_error)) {
        *detail_error = std::move(skill_error);
        return false;
    }
    std::sort(
        candidate.skills.begin(),
        candidate.skills.end(),
        [](const ShipSkillDetail& left, const ShipSkillDetail& right) {
            return left.skill_id < right.skill_id;
        });
    *output = std::move(candidate);
    return true;
}

ShipDetailsExecution snapshot_ship_details(
    const LuaApi& api,
    lua_State* state,
    std::uint32_t max_ships,
    std::string_view module_sha256) noexcept {
    ShipDetailsExecution execution;
    if (!api.ready() || state == nullptr) {
        execution.error = make_lua_error(
            "lua_api_not_ready",
            "Lua 状态或必需函数表尚未就绪",
            "same_request");
        return execution;
    }
    try {
        execution.snapshot.source.module_sha256.assign(module_sha256);
        execution.snapshot.ships.reserve(max_ships);
        BayShipVisitResult visit_result;
        if (!visit_bay_ships(
                api,
                state,
                max_ships,
                [&](int ship_index, std::uint64_t ship_id) {
                    ShipDetail detail;
                    ShipSkillVisitError detail_error;
                    if (read_ship_detail(
                            api,
                            state,
                            ship_index,
                            ship_id,
                            &detail,
                            &detail_error)) {
                        execution.snapshot.ships.push_back(std::move(detail));
                    } else {
                        add_detail_error(
                            &execution.snapshot,
                            ship_id,
                            detail_error.skill_id,
                            std::move(detail_error.code),
                            std::move(detail_error.message));
                    }
                },
                &visit_result,
                &execution.error)) {
            return execution;
        }
        execution.snapshot.truncated = visit_result.truncated;
        for (BayShipVisitError& visit_error : visit_result.read_errors) {
            add_detail_error(
                &execution.snapshot,
                visit_error.ship_id,
                std::nullopt,
                std::move(visit_error.code),
                std::move(visit_error.message));
        }
        std::sort(
            execution.snapshot.ships.begin(),
            execution.snapshot.ships.end(),
            [](const ShipDetail& left, const ShipDetail& right) {
                return left.ship_id < right.ship_id;
            });
        execution.snapshot.complete =
            !execution.snapshot.truncated && execution.snapshot.read_errors.empty();
        execution.success = true;
        return execution;
    } catch (const std::exception& exception) {
        execution.error = make_lua_error("ship_details_exception", exception.what());
        return execution;
    } catch (...) {
        execution.error = make_lua_error("ship_details_exception", "读取舰船详情时发生未知异常");
        return execution;
    }
}

}  // namespace azlw::agent

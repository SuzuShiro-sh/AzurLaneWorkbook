// 声明装备武器参数与通用技能效果证据的有界批量只读契约。

#pragma once

#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "equipment_config_snapshot.h"
#include "lua/lua_api.h"
#include "lua/lua_value.h"
#include "protocol/protocol_types.h"

namespace azlw::agent {

/// 单个武器配置保留客户端原始字段，具体武器类型由宿主映射器解释。
struct EquipmentWeaponDetail final {
    std::uint64_t weapon_id = 0;
    LuaValue raw;
    bool complete = false;
    std::vector<std::string> read_errors;
};

/// 显式请求的武器 ID 批次，记录顺序必须与请求完全一致。
struct EquipmentWeaponBatch final {
    bool complete = false;
    EquipmentConfigSource source;
    std::vector<EquipmentWeaponDetail> weapons;
};

/// 技能的一路客户端来源；调用失败与返回不完整值具有不同语义。
struct SkillEffectSource final {
    bool available = false;
    bool complete = false;
    LuaValue value;
    std::optional<std::string> error;
    std::vector<std::string> read_errors;
};

/// 同一技能等级的展示配置、战斗技能和战斗 Buff 原始证据。
struct SkillEffectDetail final {
    std::uint64_t skill_id = 0;
    std::uint32_t level = 0;
    SkillEffectSource display;
    SkillEffectSource battle_skill;
    SkillEffectSource battle_buff;
    bool complete = false;
};

/// 显式请求的技能等级批次，记录顺序必须与请求完全一致。
struct SkillEffectBatch final {
    bool complete = false;
    EquipmentConfigSource source;
    std::vector<SkillEffectDetail> skills;
};

struct EquipmentWeaponBatchExecution final {
    bool success = false;
    EquipmentWeaponBatch batch;
    AgentError error;
};

struct SkillEffectBatchExecution final {
    bool success = false;
    SkillEffectBatch batch;
    AgentError error;
};

/// 必须由 `tolua_update` 所在线程调用，读取指定武器配置的完整原始字段。
EquipmentWeaponBatchExecution snapshot_equipment_weapons(
    const LuaApi& api,
    lua_State* state,
    std::span<const std::uint64_t> weapon_ids,
    std::string_view module_sha256) noexcept;

/// 必须由 `tolua_update` 所在线程调用，读取指定技能等级的三路效果来源。
SkillEffectBatchExecution snapshot_skill_effects(
    const LuaApi& api,
    lua_State* state,
    std::span<const SkillEffectQuery> skills,
    std::string_view module_sha256) noexcept;

}  // namespace azlw::agent

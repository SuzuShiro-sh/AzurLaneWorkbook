//! 将同一会话的运行态船坞和舰船详情快照严格映射为领域名册。

use std::collections::BTreeSet;

use serde::Serialize;
use suzushiro_content_digest::sha256_compact_json;
use thiserror::Error;

use super::super::runtime::{
    MAX_SNAPSHOT_ITEMS, RuntimeFleetKind, RuntimeFleetTeam, RuntimeProtocolError, RuntimeShip,
    RuntimeShipAttributes, RuntimeShipClassification, RuntimeShipDetail, RuntimeShipSkill,
    RuntimeShipSkillDetail, RuntimeSkillEffectDetail, SnapshotOwnedStateResult,
    SnapshotShipDetailsResult,
};
use super::skill_effect::{SkillEffectEvidenceMappingError, map_skill_effect_evidence};
use crate::domain::{
    EnhanceLevel, EquipmentConfigId, NamedShipClass, ShipAttributeBreakdown, ShipAttributeValues,
    ShipClassification, ShipEquipment, ShipEquipmentSlot, ShipFleetKind, ShipFleetMembership,
    ShipFleetTeam, ShipGrowth, ShipIdentity, ShipInstanceId, ShipIntimacy, ShipOilCost,
    ShipPerformance, ShipProfile, ShipRoster, ShipRosterSource, ShipSkill, ShipSkillIdentity,
    ShipSkillProgress, SlotIndex,
};

/// 把舰船快照与已读取的通用技能效果记录合成为共享证据名册。
///
/// 只选取当前舰船技能实际引用的 `(effective_skill_id, level)`；没有证据的技能仍会
/// 进入名册，由上层投影明确标记为不完整。
pub fn map_ship_roster_with_skill_effects(
    owned_state: &SnapshotOwnedStateResult,
    details: &SnapshotShipDetailsResult,
    skill_effects: &[RuntimeSkillEffectDetail],
) -> Result<ShipRoster, ShipMappingError> {
    validate_inputs(owned_state, details)?;

    if owned_state.dock.ships.len() != details.ships.len() {
        return Err(ShipMappingError::ShipCountMismatch {
            owned_count: owned_state.dock.ships.len(),
            detail_count: details.ships.len(),
        });
    }

    let mut ships: Vec<ShipProfile> = Vec::with_capacity(details.ships.len());
    for (owned, detail) in owned_state.dock.ships.iter().zip(&details.ships) {
        validate_ship_join(owned, detail)?;
        ships.push(map_ship(owned, detail)?);
    }

    let requested_keys: BTreeSet<(u64, u32)> = details
        .ships
        .iter()
        .flat_map(|ship| &ship.skills)
        .map(|skill| (skill.effective_skill_id, skill.level))
        .collect();
    let mut selected_skill_effects: Vec<&RuntimeSkillEffectDetail> = skill_effects
        .iter()
        .filter(|record| requested_keys.contains(&(record.skill_id, record.level)))
        .collect();
    selected_skill_effects.sort_by_key(|record| (record.skill_id, record.level));
    let skill_effect_catalog =
        map_skill_effect_evidence(&selected_skill_effects).map_err(|error| match error {
            SkillEffectEvidenceMappingError::Duplicate { skill_id, level } => {
                ShipMappingError::DuplicateSkillEffectEvidence { skill_id, level }
            }
            SkillEffectEvidenceMappingError::Encode {
                skill_id,
                level,
                source,
            } => ShipMappingError::EncodeSkillEffectEvidence {
                skill_id,
                level,
                source,
            },
        })?;

    let digest_input = ShipRosterDigestInput {
        schema_version: 3,
        dock: &owned_state.dock,
        details,
        skill_effects: &selected_skill_effects,
    };
    let content_sha256 = sha256_compact_json(&digest_input).map_err(ShipMappingError::Encode)?;
    let source: ShipRosterSource =
        ShipRosterSource::new(details.source.module_sha256.clone(), content_sha256);
    Ok(ShipRoster::new_with_skill_effects(
        source,
        ships,
        skill_effect_catalog,
    ))
}

/// 内容身份只覆盖实际参与舰船领域映射的船坞、详情和技能证据。
#[derive(Serialize)]
struct ShipRosterDigestInput<'a> {
    schema_version: u32,
    dock: &'a super::super::runtime::DockSnapshot,
    details: &'a SnapshotShipDetailsResult,
    skill_effects: &'a [&'a RuntimeSkillEffectDetail],
}

fn validate_inputs(
    owned_state: &SnapshotOwnedStateResult,
    details: &SnapshotShipDetailsResult,
) -> Result<(), ShipMappingError> {
    // AgentClient 已执行相同校验；这里再次校验使公开映射函数不依赖调用顺序。
    owned_state.validate(MAX_SNAPSHOT_ITEMS, MAX_SNAPSHOT_ITEMS, MAX_SNAPSHOT_ITEMS)?;
    details.validate(MAX_SNAPSHOT_ITEMS, &details.source.module_sha256)?;

    if !owned_state.complete {
        return Err(ShipMappingError::OwnedStateIncomplete {
            dock_errors: owned_state.dock.read_errors.len(),
            warehouse_errors: owned_state.warehouse.read_errors.len(),
            bag_errors: owned_state.bag.read_errors.len(),
        });
    }
    if !details.complete {
        return Err(ShipMappingError::DetailsIncomplete {
            read_errors: details.read_errors.len(),
            truncated: details.truncated,
        });
    }
    Ok(())
}

fn validate_ship_join(
    owned: &RuntimeShip,
    detail: &RuntimeShipDetail,
) -> Result<(), ShipMappingError> {
    if owned.ship_id != detail.ship_id {
        return Err(ShipMappingError::ShipOrderMismatch {
            owned_ship_id: owned.ship_id,
            detail_ship_id: detail.ship_id,
        });
    }
    compare_ship_field(
        owned.ship_id,
        "config_id",
        owned.config_id,
        detail.config_id,
    )?;
    compare_ship_field(
        owned.ship_id,
        "level",
        u64::from(owned.level),
        u64::from(detail.level),
    )?;
    compare_ship_field(
        owned.ship_id,
        "experience_in_level",
        owned.experience_in_level,
        detail.experience_in_level,
    )?;
    compare_ship_field(
        owned.ship_id,
        "intimacy_raw",
        owned.intimacy_raw,
        detail.intimacy_raw,
    )?;

    if owned.skills.len() != detail.skills.len() {
        return Err(ShipMappingError::SkillCountMismatch {
            ship_id: owned.ship_id,
            owned_count: owned.skills.len(),
            detail_count: detail.skills.len(),
        });
    }
    for (owned_skill, detail_skill) in owned.skills.iter().zip(&detail.skills) {
        validate_skill_join(owned.ship_id, owned_skill, detail_skill)?;
    }
    Ok(())
}

fn compare_ship_field(
    ship_id: u64,
    field: &'static str,
    owned_value: u64,
    detail_value: u64,
) -> Result<(), ShipMappingError> {
    if owned_value != detail_value {
        return Err(ShipMappingError::ShipFieldMismatch {
            ship_id,
            field,
            owned_value,
            detail_value,
        });
    }
    Ok(())
}

fn validate_skill_join(
    ship_id: u64,
    owned: &RuntimeShipSkill,
    detail: &RuntimeShipSkillDetail,
) -> Result<(), ShipMappingError> {
    if owned.skill_id != detail.skill_id {
        return Err(ShipMappingError::SkillOrderMismatch {
            ship_id,
            owned_skill_id: owned.skill_id,
            detail_skill_id: detail.skill_id,
        });
    }
    if owned.level != detail.level || owned.experience != detail.experience {
        return Err(ShipMappingError::SkillProgressMismatch {
            ship_id,
            skill_id: owned.skill_id,
            owned_level: owned.level,
            detail_level: detail.level,
            owned_experience: owned.experience,
            detail_experience: detail.experience,
        });
    }
    Ok(())
}

fn map_ship(
    owned: &RuntimeShip,
    detail: &RuntimeShipDetail,
) -> Result<ShipProfile, ShipMappingError> {
    let instance_id: ShipInstanceId = ShipInstanceId::new(owned.ship_id)?;
    let identity: ShipIdentity = ShipIdentity::new(
        instance_id,
        detail.config_id,
        detail.name.clone(),
        detail.create_time,
    );
    let growth: ShipGrowth = ShipGrowth::new(
        detail.level,
        detail.max_level,
        detail.experience_in_level,
        detail.total_experience,
        detail.next_level_experience,
        owned.energy,
        owned.proficiency,
    );
    let intimacy: ShipIntimacy = ShipIntimacy::new(
        detail.intimacy_raw,
        detail.intimacy_maximum,
        detail.intimacy_stage_id,
        detail.intimacy_stage_description.clone(),
        detail.proposed,
        detail.propose_time,
    );
    let fleet_memberships = owned
        .fleet_memberships
        .iter()
        .map(map_fleet_membership)
        .collect();
    let classification: ShipClassification = map_classification(&detail.classification);
    let attributes: ShipAttributeBreakdown = ShipAttributeBreakdown::new(
        map_attributes(detail.base_attributes),
        map_attributes(detail.equipment_applied_attributes),
        map_attributes(detail.effective_attributes),
    );
    let performance: ShipPerformance = ShipPerformance::new(
        detail.combat_power,
        detail.locked,
        ShipOilCost::new(
            detail.oil_cost.start,
            detail.oil_cost.end,
            detail.oil_cost.total,
        ),
        attributes,
    );
    let skills: Vec<ShipSkill> = detail.skills.iter().map(map_skill).collect();
    let slots: [ShipEquipmentSlot; 5] = map_slots(owned, detail)?;

    Ok(ShipProfile::new(
        identity,
        growth,
        intimacy,
        fleet_memberships,
        classification,
        performance,
        skills,
        slots,
    ))
}

fn map_fleet_membership(
    membership: &super::super::runtime::RuntimeFleetMembership,
) -> ShipFleetMembership {
    let kind = match membership.kind {
        RuntimeFleetKind::Regular => ShipFleetKind::Regular,
        RuntimeFleetKind::Submarine => ShipFleetKind::Submarine,
        RuntimeFleetKind::Exercise => ShipFleetKind::Exercise,
    };
    let team = match membership.team {
        RuntimeFleetTeam::Main => ShipFleetTeam::Main,
        RuntimeFleetTeam::Vanguard => ShipFleetTeam::Vanguard,
        RuntimeFleetTeam::Submarine => ShipFleetTeam::Submarine,
    };
    ShipFleetMembership::new(
        membership.fleet_id,
        membership.display_name.clone(),
        kind,
        team,
        membership.position,
    )
}

fn map_slots(
    owned: &RuntimeShip,
    detail: &RuntimeShipDetail,
) -> Result<[ShipEquipmentSlot; 5], ShipMappingError> {
    let slots: Vec<ShipEquipmentSlot> = owned
        .slots
        .iter()
        .zip(&detail.slot_rules)
        .map(|(slot, rule)| {
            let slot_index = u8::try_from(slot.slot_index).map_err(|_| {
                ShipMappingError::SlotFieldOutOfRange {
                    ship_id: owned.ship_id,
                    slot_index: slot.slot_index,
                    field: "slot_index",
                    value: u64::from(slot.slot_index),
                }
            })?;
            let index = SlotIndex::new(slot_index)?;
            let equipment = slot
                .equipment
                .as_ref()
                .map(|equipment| -> Result<ShipEquipment, ShipMappingError> {
                    let enhance_level = u8::try_from(equipment.enhance_level).map_err(|_| {
                        ShipMappingError::SlotFieldOutOfRange {
                            ship_id: owned.ship_id,
                            slot_index: slot.slot_index,
                            field: "enhance_level",
                            value: u64::from(equipment.enhance_level),
                        }
                    })?;
                    Ok(ShipEquipment::new(
                        equipment.equipment_id,
                        EquipmentConfigId::new(equipment.config_id)?,
                        EnhanceLevel::new(enhance_level),
                    ))
                })
                .transpose()?;
            Ok(ShipEquipmentSlot::new(
                index,
                equipment,
                rule.allowed_equipment_type_ids.clone(),
            ))
        })
        .collect::<Result<Vec<_>, ShipMappingError>>()?;

    Ok(slots.try_into().expect("已验证槽位数量严格为 5"))
}

fn map_classification(source: &RuntimeShipClassification) -> ShipClassification {
    ShipClassification::new(
        source.group_id,
        NamedShipClass::new(source.ship_type_id, source.ship_type_name.clone()),
        NamedShipClass::new(source.armor_type_id, source.armor_type_name.clone()),
        NamedShipClass::new(source.nation_id, source.nation_name.clone()),
        source.rarity,
        crate::domain::ShipStars::new(source.star, source.max_star),
        source.skin_id,
    )
}

const fn map_attributes(source: RuntimeShipAttributes) -> ShipAttributeValues {
    ShipAttributeValues::new(
        source.durability,
        source.cannon,
        source.torpedo,
        source.anti_aircraft,
        source.air,
        source.reload,
        source.hit,
        source.dodge,
        source.anti_sub,
        source.luck,
        source.speed,
    )
}

fn map_skill(source: &RuntimeShipSkillDetail) -> ShipSkill {
    ShipSkill::new(
        ShipSkillIdentity::new(
            source.skill_id,
            source.effective_skill_id,
            source.name.clone(),
        ),
        ShipSkillProgress::new(
            source.level,
            source.max_level,
            source.experience,
            source.next_level_experience,
        ),
        source.description_template.clone(),
        source.current_effect.clone(),
    )
}

/// 两份快照不完整、彼此不对应或无法生成内容身份。
#[derive(Debug, Error)]
pub enum ShipMappingError {
    /// 任一输入本身违反已冻结的 RPC 契约。
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
    /// 舰船实例 ID 无法建立领域标识。
    #[error(transparent)]
    Model(#[from] crate::domain::LoadoutModelError),
    /// 同一技能和等级出现多份相互冲突的效果证据。
    #[error("技能效果证据重复: skill_id={skill_id}, level={level}")]
    DuplicateSkillEffectEvidence { skill_id: u64, level: u32 },
    /// 技能效果原始结构无法形成稳定规范 JSON。
    #[error("技能效果原始结构编码失败: skill_id={skill_id}, level={level}: {source}")]
    EncodeSkillEffectEvidence {
        skill_id: u64,
        level: u32,
        #[source]
        source: serde_json::Error,
    },
    /// 运行态快照包含截断或逐项读取失败。
    #[error(
        "完整运行态不完整: dock_errors={dock_errors}, warehouse_errors={warehouse_errors}, bag_errors={bag_errors}"
    )]
    OwnedStateIncomplete {
        dock_errors: usize,
        warehouse_errors: usize,
        bag_errors: usize,
    },
    /// 详情快照包含截断或逐项读取失败。
    #[error("舰船详情不完整: truncated={truncated}, read_errors={read_errors}")]
    DetailsIncomplete { read_errors: usize, truncated: bool },
    /// 两份完整快照返回的舰船数量不同。
    #[error("运行态有 {owned_count} 艘舰船，详情有 {detail_count} 艘")]
    ShipCountMismatch {
        owned_count: usize,
        detail_count: usize,
    },
    /// 同一排序位置对应了不同舰船，通常表示两次读取间状态变化。
    #[error("舰船顺序不一致: 运行态 ship_id={owned_ship_id}，详情 ship_id={detail_ship_id}")]
    ShipOrderMismatch {
        owned_ship_id: u64,
        detail_ship_id: u64,
    },
    /// 两份快照中的同一舰船基础字段不一致。
    #[error("ship_id={ship_id} 的 {field} 不一致: 运行态={owned_value}，详情={detail_value}")]
    ShipFieldMismatch {
        ship_id: u64,
        field: &'static str,
        owned_value: u64,
        detail_value: u64,
    },
    /// 两份快照中的自身技能数量不同。
    #[error("ship_id={ship_id} 的自身技能数量不一致: 运行态={owned_count}，详情={detail_count}")]
    SkillCountMismatch {
        ship_id: u64,
        owned_count: usize,
        detail_count: usize,
    },
    /// 同一排序位置对应了不同的原始技能 ID。
    #[error(
        "ship_id={ship_id} 的技能顺序不一致: 运行态 skill_id={owned_skill_id}，详情 skill_id={detail_skill_id}"
    )]
    SkillOrderMismatch {
        ship_id: u64,
        owned_skill_id: u64,
        detail_skill_id: u64,
    },
    /// 同一原始技能在两份快照中的等级或经验不同。
    #[error(
        "ship_id={ship_id}, skill_id={skill_id} 的进度不一致: 运行态 level={owned_level}, exp={owned_experience}；详情 level={detail_level}, exp={detail_experience}"
    )]
    SkillProgressMismatch {
        ship_id: u64,
        skill_id: u64,
        owned_level: u32,
        detail_level: u32,
        owned_experience: u64,
        detail_experience: u64,
    },
    /// 槽位或装备数值无法装入已经冻结的领域类型。
    #[error("ship_id={ship_id}, slot={slot_index} 的 {field}={value} 超出领域模型允许的范围")]
    SlotFieldOutOfRange {
        ship_id: u64,
        slot_index: u32,
        field: &'static str,
        value: u64,
    },
    /// 两份映射输入无法规范序列化，因此不能生成稳定内容摘要。
    #[error("舰船名册生成内容摘要失败: {0}")]
    Encode(#[source] serde_json::Error),
}

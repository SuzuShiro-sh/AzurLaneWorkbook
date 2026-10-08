//! 将完整装备详情原始记录投影为领域武器、技能和可追溯原始记录。

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use super::super::reading::equipment::EquipmentReadResult;
use super::super::runtime::{
    RuntimeEquipmentWeaponDetail, RuntimeSkillEffectDetail, RuntimeSkillEffectSource,
};
use crate::domain::{
    EquipmentConfigId, EquipmentDetailCatalog, EquipmentSkillDetail, EquipmentSkillDisplay,
    EquipmentSkillEffect, EquipmentSkillSource, EquipmentWeapon, RawRecord, RawRecordKey,
    RawRecordSet, SkillEffectArgument, SkillMixedTable, SkillTableEntry, SkillTableKey, SkillValue,
    SkillValueField, WeaponChargeParameter, WeaponPrecastParameter,
};
use suzushiro_content_digest::{sha256_bytes, sorted_json as canonical_json};

const MAX_SKILL_VALUE_DEPTH: usize = 32;

/// 完整装备读取结果中可供领域层使用的详情和原始记录。
#[derive(Debug, PartialEq)]
pub struct EquipmentDetailProjection {
    details: EquipmentDetailCatalog,
    raw_records: RawRecordSet,
}

impl EquipmentDetailProjection {
    /// 返回规范化武器和技能详情。
    pub const fn details(&self) -> &EquipmentDetailCatalog {
        &self.details
    }

    /// 返回按稳定键保存的完整原始记录。
    pub const fn raw_records(&self) -> &RawRecordSet {
        &self.raw_records
    }

    pub(crate) fn into_parts(self) -> (EquipmentDetailCatalog, RawRecordSet) {
        (self.details, self.raw_records)
    }
}

/// 将已通过读取器完整性校验的装备详情投影为领域类型。
pub fn map_equipment_details(
    result: &EquipmentReadResult,
) -> Result<EquipmentDetailProjection, EquipmentDetailMappingError> {
    let raw = result.raw_records();
    let weapons = map_weapons(raw.weapons())?;
    let skills = map_skills(raw.skills())?;
    let records = map_raw_records(result)?;
    Ok(EquipmentDetailProjection {
        details: EquipmentDetailCatalog::new(weapons, skills),
        raw_records: RawRecordSet::new(
            raw.schema_version(),
            result.raw_content_sha256().to_owned(),
            records,
        ),
    })
}

fn map_weapons(
    records: &[RuntimeEquipmentWeaponDetail],
) -> Result<Vec<EquipmentWeapon>, EquipmentDetailMappingError> {
    let mut previous = None;
    records
        .iter()
        .map(|record| {
            if previous.is_some_and(|value| value >= record.weapon_id) {
                return Err(EquipmentDetailMappingError::WeaponOrder {
                    previous,
                    current: record.weapon_id,
                });
            }
            previous = Some(record.weapon_id);
            map_weapon(record)
        })
        .collect()
}

fn map_weapon(
    record: &RuntimeEquipmentWeaponDetail,
) -> Result<EquipmentWeapon, EquipmentDetailMappingError> {
    let raw = record
        .raw
        .as_object()
        .ok_or_else(|| weapon_field_error(record.weapon_id, "raw", "武器正文必须是普通对象"))?;
    let embedded_id = required_u64(raw, record.weapon_id, "id")?;
    if embedded_id != record.weapon_id {
        return Err(weapon_field_error(
            record.weapon_id,
            "id",
            format!("正文 ID 为 {embedded_id}，与请求 ID 不一致"),
        ));
    }

    Ok(EquipmentWeapon::new(
        record.weapon_id,
        optional_u64(raw, record.weapon_id, "base")?,
        required_string(raw, record.weapon_id, "action_index")?,
        required_u64(raw, record.weapon_id, "aim_type")?,
        required_u64(raw, record.weapon_id, "angle")?,
        required_u64(raw, record.weapon_id, "attack_attribute")?,
        required_u64(raw, record.weapon_id, "attack_attribute_ratio")?,
        required_number(raw, record.weapon_id, "auto_aftercast")?,
        required_u64(raw, record.weapon_id, "axis_angle")?,
        required_u64_array(raw, record.weapon_id, "barrage_ID")?,
        required_u64_array(raw, record.weapon_id, "bullet_ID")?,
        charge_parameter(raw, record.weapon_id)?,
        required_u64(raw, record.weapon_id, "corrected")?,
        required_u64(raw, record.weapon_id, "damage")?,
        required_u64(raw, record.weapon_id, "effect_move")?,
        required_u64(raw, record.weapon_id, "expose")?,
        required_string(raw, record.weapon_id, "fire_fx")?,
        required_u64(raw, record.weapon_id, "fire_fx_loop_type")?,
        required_string(raw, record.weapon_id, "fire_sfx")?,
        required_u64(raw, record.weapon_id, "initial_over_heat")?,
        required_u64(raw, record.weapon_id, "min_range")?,
        required_u64_array(raw, record.weapon_id, "oxy_type")?,
        precast_parameter(raw, record.weapon_id)?,
        required_u64(raw, record.weapon_id, "queue")?,
        required_u64(raw, record.weapon_id, "range")?,
        required_number(raw, record.weapon_id, "recover_time")?,
        required_u64(raw, record.weapon_id, "reload_max")?,
        required_u64_array(raw, record.weapon_id, "search_condition")?,
        required_u64(raw, record.weapon_id, "search_type")?,
        required_u64(raw, record.weapon_id, "shakescreen")?,
        required_string(raw, record.weapon_id, "spawn_bound")?,
        required_u64(raw, record.weapon_id, "suppress")?,
        required_u64(raw, record.weapon_id, "torpedo_ammo")?,
        required_u64(raw, record.weapon_id, "type")?,
    ))
}

fn required_value<'a>(
    raw: &'a Map<String, Value>,
    weapon_id: u64,
    field: &'static str,
) -> Result<&'a Value, EquipmentDetailMappingError> {
    raw.get(field)
        .ok_or_else(|| weapon_field_error(weapon_id, field, "缺少必需字段"))
}

fn required_u64(
    raw: &Map<String, Value>,
    weapon_id: u64,
    field: &'static str,
) -> Result<u64, EquipmentDetailMappingError> {
    required_value(raw, weapon_id, field)?
        .as_u64()
        .ok_or_else(|| weapon_field_error(weapon_id, field, "必须是非负整数"))
}

fn optional_u64(
    raw: &Map<String, Value>,
    weapon_id: u64,
    field: &'static str,
) -> Result<Option<u64>, EquipmentDetailMappingError> {
    raw.get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| weapon_field_error(weapon_id, field, "存在时必须是非负整数"))
        })
        .transpose()
}

fn required_number(
    raw: &Map<String, Value>,
    weapon_id: u64,
    field: &'static str,
) -> Result<f64, EquipmentDetailMappingError> {
    let number = required_value(raw, weapon_id, field)?
        .as_f64()
        .ok_or_else(|| weapon_field_error(weapon_id, field, "必须是有限数值"))?;
    if !number.is_finite() {
        return Err(weapon_field_error(weapon_id, field, "必须是有限数值"));
    }
    Ok(number)
}

fn required_string(
    raw: &Map<String, Value>,
    weapon_id: u64,
    field: &'static str,
) -> Result<String, EquipmentDetailMappingError> {
    required_value(raw, weapon_id, field)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| weapon_field_error(weapon_id, field, "必须是字符串"))
}

fn required_u64_array(
    raw: &Map<String, Value>,
    weapon_id: u64,
    field: &'static str,
) -> Result<Vec<u64>, EquipmentDetailMappingError> {
    let values = required_value(raw, weapon_id, field)?
        .as_array()
        .ok_or_else(|| weapon_field_error(weapon_id, field, "必须是整数数组"))?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value.as_u64().ok_or_else(|| {
                weapon_field_error(
                    weapon_id,
                    field,
                    format!("第 {} 项必须是非负整数", index + 1),
                )
            })
        })
        .collect()
}

fn charge_parameter(
    raw: &Map<String, Value>,
    weapon_id: u64,
) -> Result<WeaponChargeParameter, EquipmentDetailMappingError> {
    let field = "charge_param";
    match required_value(raw, weapon_id, field)? {
        Value::String(value) if value.is_empty() => Ok(WeaponChargeParameter::Empty),
        Value::Object(value) => {
            let lock_time = value
                .get("lockTime")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
                .ok_or_else(|| weapon_field_error(weapon_id, field, "lockTime 必须是有限数值"))?;
            let max_lock = value
                .get("maxLock")
                .and_then(Value::as_u64)
                .ok_or_else(|| weapon_field_error(weapon_id, field, "maxLock 必须是非负整数"))?;
            Ok(WeaponChargeParameter::Lock {
                lock_time,
                max_lock,
            })
        }
        _ => Err(weapon_field_error(
            weapon_id,
            field,
            "必须是空字符串或锁定参数对象",
        )),
    }
}

fn precast_parameter(
    raw: &Map<String, Value>,
    weapon_id: u64,
) -> Result<WeaponPrecastParameter, EquipmentDetailMappingError> {
    let field = "precast_param";
    match required_value(raw, weapon_id, field)? {
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| {
                        weapon_field_error(
                            weapon_id,
                            field,
                            format!("第 {} 项必须是有限数值", index + 1),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(WeaponPrecastParameter::Values),
        Value::String(value) if value.chars().all(char::is_whitespace) => {
            Ok(WeaponPrecastParameter::LegacyWhitespace)
        }
        _ => Err(weapon_field_error(
            weapon_id,
            field,
            "必须是数值数组或旧版空白字符串",
        )),
    }
}

fn map_skills(
    records: &[RuntimeSkillEffectDetail],
) -> Result<Vec<EquipmentSkillDetail>, EquipmentDetailMappingError> {
    let mut previous = None;
    records
        .iter()
        .map(|record| {
            let key = (record.skill_id, record.level);
            if previous.is_some_and(|value| value >= key) {
                return Err(EquipmentDetailMappingError::SkillOrder {
                    previous,
                    current: key,
                });
            }
            previous = Some(key);
            map_skill(record)
        })
        .collect()
}

fn map_skill(
    record: &RuntimeSkillEffectDetail,
) -> Result<EquipmentSkillDetail, EquipmentDetailMappingError> {
    let display = map_display(record)?;
    let battle_skill = map_skill_source(record, "battle_skill", &record.battle_skill)?;
    let battle_buff = map_skill_source(record, "battle_buff", &record.battle_buff)?;
    if battle_skill.is_none() && battle_buff.is_none() {
        return Err(skill_field_error(
            record,
            "battle_sources",
            "战斗技能和战斗 Buff 至少需要一路可用",
        ));
    }
    Ok(EquipmentSkillDetail::new(
        record.skill_id,
        record.level,
        display,
        battle_skill,
        battle_buff,
    ))
}

fn map_display(
    record: &RuntimeSkillEffectDetail,
) -> Result<EquipmentSkillDisplay, EquipmentDetailMappingError> {
    if !record.display.available || !record.display.complete {
        return Err(skill_field_error(
            record,
            "display",
            "完整装备技能必须具有可用且完整的展示配置",
        ));
    }
    let raw = record
        .display
        .value
        .as_object()
        .ok_or_else(|| skill_field_error(record, "display.value", "展示配置必须是普通对象"))?;
    require_reference_skill_id(raw, record, "display.value")?;
    Ok(EquipmentSkillDisplay::new(
        skill_string(raw, record, "display.name", "name")?,
        skill_string(raw, record, "display.desc", "desc")?,
        skill_string(raw, record, "display.desc_get", "desc_get")?,
        map_skill_value(
            raw.get("system_transform").ok_or_else(|| {
                skill_field_error(record, "display.system_transform", "缺少必需字段")
            })?,
            record,
            "display.system_transform",
            0,
        )?,
    ))
}

fn map_skill_source(
    record: &RuntimeSkillEffectDetail,
    source_name: &'static str,
    source: &RuntimeSkillEffectSource,
) -> Result<Option<EquipmentSkillSource>, EquipmentDetailMappingError> {
    if !source.available {
        return Ok(None);
    }
    if !source.complete {
        return Err(skill_field_error(record, source_name, "可用来源必须完整"));
    }
    let raw = materialize_skill_root(&source.value, record, source_name)?;
    let config_id = skill_config_id(&raw, record, source_name)?;
    let effect_values = raw
        .get("effect_list")
        .and_then(Value::as_array)
        .ok_or_else(|| skill_field_error(record, source_name, "effect_list 必须是数组"))?;
    let effects = effect_values
        .iter()
        .enumerate()
        .map(|(index, value)| map_effect(value, index, record, source_name))
        .collect::<Result<_, _>>()?;

    Ok(Some(EquipmentSkillSource::new(
        config_id,
        skill_string(&raw, record, source_name, "name")?,
        optional_skill_string(&raw, record, source_name, "desc")?.unwrap_or_default(),
        optional_skill_number(&raw, record, source_name, "cd")?,
        optional_skill_number(&raw, record, source_name, "time")?,
        optional_skill_u64(&raw, record, source_name, "stack")?,
        effects,
    )))
}

pub(crate) fn materialize_skill_root(
    value: &Value,
    record: &RuntimeSkillEffectDetail,
    source_name: &'static str,
) -> Result<Map<String, Value>, EquipmentDetailMappingError> {
    let raw = value
        .as_object()
        .ok_or_else(|| skill_field_error(record, source_name, "可用技能源必须是对象或混合表"))?;
    if !is_mixed_table(raw) {
        return Ok(raw.clone());
    }
    validate_complete_table(raw, record, source_name)?;
    let entries = raw
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| skill_field_error(record, source_name, "混合表缺少 entries 数组"))?;
    let mut fields = Map::new();
    let mut current_level = None;
    let mut has_numeric_entries = false;
    for entry in entries {
        let entry = entry
            .as_object()
            .ok_or_else(|| skill_field_error(record, source_name, "混合表条目必须是对象"))?;
        reject_unsupported_entry(entry, record, source_name)?;
        match entry.get("key_type").and_then(Value::as_str) {
            Some("string") => {
                let key = entry.get("key").and_then(Value::as_str).ok_or_else(|| {
                    skill_field_error(record, source_name, "字符串表键必须是字符串")
                })?;
                let value = entry.get("value").ok_or_else(|| {
                    skill_field_error(record, source_name, "混合表条目缺少 value")
                })?;
                fields.insert(key.to_owned(), value.clone());
            }
            Some("number") => {
                has_numeric_entries = true;
                let key = entry.get("key").and_then(Value::as_f64).ok_or_else(|| {
                    skill_field_error(record, source_name, "数值表键必须是有限数值")
                })?;
                if key == f64::from(record.level) {
                    current_level = entry.get("value").cloned();
                }
            }
            _ => {
                return Err(skill_field_error(
                    record,
                    source_name,
                    "混合表只支持字符串键和数值键",
                ));
            }
        }
    }
    if has_numeric_entries && current_level.is_none() {
        return Err(skill_field_error(
            record,
            source_name,
            format!("混合表缺少等级 {} 的数值键", record.level),
        ));
    }
    match current_level {
        Some(Value::Object(overrides)) => fields.extend(overrides),
        Some(Value::Array(values)) if values.is_empty() => {}
        Some(_) => {
            return Err(skill_field_error(
                record,
                source_name,
                "等级覆盖必须是对象或空数组",
            ));
        }
        None => {}
    }
    Ok(fields)
}

fn skill_config_id(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    field: &'static str,
) -> Result<u64, EquipmentDetailMappingError> {
    raw.get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| skill_field_error(record, field, "技能正文必须包含非负整数 id"))
}

fn require_reference_skill_id(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    field: &'static str,
) -> Result<(), EquipmentDetailMappingError> {
    let actual = skill_config_id(raw, record, field)?;
    if actual != record.skill_id {
        return Err(skill_field_error(
            record,
            field,
            format!("正文 ID 为 {actual}，与请求 ID 不一致"),
        ));
    }
    Ok(())
}

fn skill_string(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<String, EquipmentDetailMappingError> {
    raw.get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| skill_field_error(record, context, format!("{field} 必须是字符串")))
}

fn optional_skill_string(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<Option<String>, EquipmentDetailMappingError> {
    raw.get(field)
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                skill_field_error(record, context, format!("{field} 存在时必须是字符串"))
            })
        })
        .transpose()
}

fn optional_skill_number(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<Option<f64>, EquipmentDetailMappingError> {
    raw.get(field)
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    skill_field_error(record, context, format!("{field} 必须是有限数值"))
                })
        })
        .transpose()
}

fn optional_skill_u64(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<Option<u64>, EquipmentDetailMappingError> {
    raw.get(field)
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                skill_field_error(record, context, format!("{field} 必须是非负整数"))
            })
        })
        .transpose()
}

fn map_effect(
    value: &Value,
    index: usize,
    record: &RuntimeSkillEffectDetail,
    source_name: &'static str,
) -> Result<EquipmentSkillEffect, EquipmentDetailMappingError> {
    let raw = value
        .as_object()
        .ok_or_else(|| skill_field_error(record, source_name, "effect_list 的每一项必须是对象"))?;
    let effect_type = raw
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| skill_field_error(record, source_name, "效果缺少字符串 type"))?;
    let arguments = raw
        .get("arg_list")
        .and_then(Value::as_object)
        .ok_or_else(|| skill_field_error(record, source_name, "效果缺少对象 arg_list"))?
        .iter()
        .map(|(name, value)| {
            Ok(SkillEffectArgument::new(
                name.clone(),
                map_skill_value(value, record, source_name, 0)?,
            ))
        })
        .collect::<Result<_, EquipmentDetailMappingError>>()?;
    let target_choices = optional_string_or_array(raw, record, source_name, "target_choise")?;
    let triggers = optional_string_array(raw, record, source_name, "trigger")?;
    let metadata = raw
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "type" | "arg_list" | "target_choise" | "trigger"
            )
        })
        .map(|(name, value)| {
            Ok(SkillValueField::new(
                name.clone(),
                map_skill_value(value, record, source_name, 0)?,
            ))
        })
        .collect::<Result<_, EquipmentDetailMappingError>>()?;
    let sequence = u32::try_from(index + 1)
        .map_err(|_| skill_field_error(record, source_name, "效果序号超出 u32 范围"))?;
    Ok(EquipmentSkillEffect::new(
        sequence,
        effect_type,
        target_choices,
        triggers,
        arguments,
        metadata,
    ))
}

fn optional_string_or_array(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<Vec<String>, EquipmentDetailMappingError> {
    match raw.get(field) {
        None => Ok(Vec::new()),
        Some(Value::String(value)) => Ok(vec![value.clone()]),
        Some(Value::Array(values)) => string_values(values, record, context, field),
        Some(_) => Err(skill_field_error(
            record,
            context,
            format!("{field} 必须是字符串或字符串数组"),
        )),
    }
}

fn optional_string_array(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<Vec<String>, EquipmentDetailMappingError> {
    match raw.get(field) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => string_values(values, record, context, field),
        Some(_) => Err(skill_field_error(
            record,
            context,
            format!("{field} 必须是字符串数组"),
        )),
    }
}

fn string_values(
    values: &[Value],
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    field: &'static str,
) -> Result<Vec<String>, EquipmentDetailMappingError> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                skill_field_error(
                    record,
                    context,
                    format!("{field} 第 {} 项必须是字符串", index + 1),
                )
            })
        })
        .collect()
}

pub(crate) fn map_skill_value(
    value: &Value,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    depth: usize,
) -> Result<SkillValue, EquipmentDetailMappingError> {
    if depth > MAX_SKILL_VALUE_DEPTH {
        return Err(skill_field_error(
            record,
            context,
            "技能参数递归深度超过领域上限",
        ));
    }
    match value {
        Value::Null => Ok(SkillValue::Null),
        Value::Bool(value) => Ok(SkillValue::Bool(*value)),
        Value::Number(value) => {
            let value = value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| skill_field_error(record, context, "技能参数数值必须有限"))?;
            Ok(SkillValue::Number(value))
        }
        Value::String(value) => Ok(SkillValue::String(value.clone())),
        Value::Array(values) => values
            .iter()
            .map(|value| map_skill_value(value, record, context, depth + 1))
            .collect::<Result<_, _>>()
            .map(SkillValue::List),
        Value::Object(raw) if is_mixed_table(raw) => {
            map_mixed_table(raw, record, context, depth + 1).map(SkillValue::MixedTable)
        }
        Value::Object(raw) if raw.get("lua_type").and_then(Value::as_str).is_some() => Err(
            skill_field_error(record, context, "技能参数包含无法规范化的 Lua 类型"),
        ),
        Value::Object(raw) => raw
            .iter()
            .map(|(name, value)| {
                Ok(SkillValueField::new(
                    name.clone(),
                    map_skill_value(value, record, context, depth + 1)?,
                ))
            })
            .collect::<Result<_, EquipmentDetailMappingError>>()
            .map(SkillValue::Object),
    }
}

fn is_mixed_table(raw: &Map<String, Value>) -> bool {
    raw.get("lua_type").and_then(Value::as_str) == Some("table") && raw.get("entries").is_some()
}

fn validate_complete_table(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
) -> Result<(), EquipmentDetailMappingError> {
    if raw.get("truncated").and_then(Value::as_bool) != Some(false) {
        return Err(skill_field_error(
            record,
            context,
            "截断的 Lua 混合表不能进入完整领域状态",
        ));
    }
    match raw.get("reason") {
        Some(Value::Null | Value::String(_)) => Ok(()),
        _ => Err(skill_field_error(
            record,
            context,
            "Lua 混合表 reason 必须是字符串或 null",
        )),
    }
}

fn reject_unsupported_entry(
    entry: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
) -> Result<(), EquipmentDetailMappingError> {
    if !matches!(entry.get("lua_type"), Some(Value::Null)) {
        return Err(skill_field_error(
            record,
            context,
            "Lua 混合表包含无法规范化的条目类型",
        ));
    }
    Ok(())
}

fn map_mixed_table(
    raw: &Map<String, Value>,
    record: &RuntimeSkillEffectDetail,
    context: &'static str,
    depth: usize,
) -> Result<SkillMixedTable, EquipmentDetailMappingError> {
    validate_complete_table(raw, record, context)?;
    let entries = raw
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| skill_field_error(record, context, "Lua 混合表缺少 entries 数组"))?
        .iter()
        .map(|entry| {
            let entry = entry
                .as_object()
                .ok_or_else(|| skill_field_error(record, context, "Lua 混合表条目必须是对象"))?;
            reject_unsupported_entry(entry, record, context)?;
            let key = match entry.get("key_type").and_then(Value::as_str) {
                Some("number") => SkillTableKey::Number(
                    entry
                        .get("key")
                        .and_then(Value::as_f64)
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| {
                            skill_field_error(record, context, "数值表键必须是有限数值")
                        })?,
                ),
                Some("string") => SkillTableKey::String(
                    entry
                        .get("key")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            skill_field_error(record, context, "字符串表键必须是字符串")
                        })?,
                ),
                _ => {
                    return Err(skill_field_error(
                        record,
                        context,
                        "Lua 混合表只支持数值键和字符串键",
                    ));
                }
            };
            let value = map_skill_value(
                entry.get("value").ok_or_else(|| {
                    skill_field_error(record, context, "Lua 混合表条目缺少 value")
                })?,
                record,
                context,
                depth + 1,
            )?;
            Ok(SkillTableEntry::new(key, value))
        })
        .collect::<Result<_, EquipmentDetailMappingError>>()?;
    let reason = raw
        .get("reason")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Ok(SkillMixedTable::new(entries, reason))
}

fn map_raw_records(
    result: &EquipmentReadResult,
) -> Result<Vec<RawRecord>, EquipmentDetailMappingError> {
    let raw = result.raw_records();
    let mut records = Vec::with_capacity(
        raw.configs().len() + raw.recipes().len() + raw.weapons().len() + raw.skills().len() + 1,
    );
    for config in raw.configs() {
        records.push(encode_raw_record(
            RawRecordKey::EquipmentConfig(EquipmentConfigId::new(config.config_id).map_err(
                |error| EquipmentDetailMappingError::RawRecordKey {
                    message: error.to_string(),
                },
            )?),
            config,
        )?);
    }
    for recipe in raw.recipes() {
        records.push(encode_raw_record(
            RawRecordKey::EquipmentRecipe(recipe.recipe_id),
            recipe,
        )?);
    }
    records.push(encode_raw_record(
        RawRecordKey::EquipmentReferenceNames,
        raw.reference_names(),
    )?);
    for weapon in raw.weapons() {
        records.push(encode_raw_record(
            RawRecordKey::EquipmentWeapon(weapon.weapon_id),
            weapon,
        )?);
    }
    for skill in raw.skills() {
        records.push(encode_raw_record(
            RawRecordKey::EquipmentSkill {
                skill_id: skill.skill_id,
                level: skill.level,
            },
            skill,
        )?);
    }
    records.sort_by(|left, right| left.key().cmp(right.key()));
    if let Some(pair) = records
        .windows(2)
        .find(|pair| pair[0].key() == pair[1].key())
    {
        return Err(EquipmentDetailMappingError::DuplicateRawRecord {
            key: pair[0].key().clone(),
        });
    }
    Ok(records)
}

fn encode_raw_record<T: Serialize + ?Sized>(
    key: RawRecordKey,
    value: &T,
) -> Result<RawRecord, EquipmentDetailMappingError> {
    let encoded =
        canonical_json(value).map_err(|source| EquipmentDetailMappingError::EncodeRawRecord {
            key: key.clone(),
            source,
        })?;
    let digest = sha256_bytes(encoded.as_bytes());
    Ok(RawRecord::new(key, digest, Arc::from(encoded)))
}

fn weapon_field_error(
    weapon_id: u64,
    field: &'static str,
    message: impl Into<String>,
) -> EquipmentDetailMappingError {
    EquipmentDetailMappingError::WeaponField {
        weapon_id,
        field,
        message: message.into(),
    }
}

fn skill_field_error(
    record: &RuntimeSkillEffectDetail,
    field: &'static str,
    message: impl Into<String>,
) -> EquipmentDetailMappingError {
    EquipmentDetailMappingError::SkillField {
        skill_id: record.skill_id,
        level: record.level,
        field,
        message: message.into(),
    }
}

/// 装备详情字段、排序或原始记录编码无法形成完整领域投影。
#[derive(Debug, Error)]
pub enum EquipmentDetailMappingError {
    /// 武器详情没有满足已验证样本冻结的字段类型。
    #[error("weapon_id={weapon_id} 的 {field} 无效: {message}")]
    WeaponField {
        weapon_id: u64,
        field: &'static str,
        message: String,
    },
    /// 装备技能详情没有满足递归值或效果结构约束。
    #[error("skill_id={skill_id}, level={level} 的 {field} 无效: {message}")]
    SkillField {
        skill_id: u64,
        level: u32,
        field: &'static str,
        message: String,
    },
    /// 武器详情没有按读取请求的稳定键严格升序排列。
    #[error("武器详情顺序无效: previous={previous:?}, current={current}")]
    WeaponOrder { previous: Option<u64>, current: u64 },
    /// 技能详情没有按技能 ID 和等级严格升序排列。
    #[error("技能详情顺序无效: previous={previous:?}, current={current:?}")]
    SkillOrder {
        previous: Option<(u64, u32)>,
        current: (u64, u32),
    },
    /// 原始记录的稳定键无法建立。
    #[error("原始装备记录键无效: {message}")]
    RawRecordKey { message: String },
    /// 单条原始记录无法按规范 JSON 编码。
    #[error("原始装备记录 {key:?} 编码失败: {source}")]
    EncodeRawRecord {
        key: RawRecordKey,
        #[source]
        source: serde_json::Error,
    },
    /// 两条原始记录使用了相同稳定键。
    #[error("原始装备记录键重复: {key:?}")]
    DuplicateRawRecord { key: RawRecordKey },
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::BufReader;

    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::{
        EquipmentDetailMappingError, map_equipment_details, map_skill, map_skill_value, map_weapon,
        materialize_skill_root,
    };
    use crate::adapters::device::reading::equipment::{EquipmentRawRecords, EquipmentReadResult};
    use crate::adapters::device::runtime::{
        EquipmentComposeRecipe, EquipmentReferenceNameBatchResult, RuntimeEquipmentConfig,
        RuntimeEquipmentWeaponDetail, RuntimeSkillEffectDetail, RuntimeSkillEffectSource,
    };
    use crate::domain::{
        EquipmentCatalog, EquipmentCatalogSource, SkillTableKey, SkillValue, WeaponChargeParameter,
        WeaponPrecastParameter,
    };

    #[derive(Deserialize)]
    struct LocalEquipmentSample {
        schema_version: u32,
        module_sha256: String,
        raw_content_sha256: String,
        raw: LocalEquipmentRawDocument,
    }

    #[derive(Deserialize)]
    struct LocalEquipmentRawDocument {
        schema_version: u32,
        configs: Vec<RuntimeEquipmentConfig>,
        recipes: Vec<EquipmentComposeRecipe>,
        reference_names: EquipmentReferenceNameBatchResult,
        weapons: Vec<RuntimeEquipmentWeaponDetail>,
        skills: Vec<RuntimeSkillEffectDetail>,
    }

    #[test]
    #[ignore = "需要通过 AZLW_EQUIPMENT_SAMPLE 显式指定本地完整维护样本"]
    fn maps_local_full_equipment_sample() {
        let sample_path = std::env::var("AZLW_EQUIPMENT_SAMPLE")
            .expect("运行此测试前应设置 AZLW_EQUIPMENT_SAMPLE");
        let sample: LocalEquipmentSample =
            serde_json::from_reader(BufReader::new(File::open(sample_path).unwrap())).unwrap();
        assert_eq!(sample.schema_version, 1);
        assert_eq!(sample.raw.schema_version, 2);

        let raw_records = EquipmentRawRecords::new(
            sample.raw.configs,
            sample.raw.recipes,
            sample.raw.reference_names,
            sample.raw.weapons,
            sample.raw.skills,
        )
        .unwrap();
        let catalog = EquipmentCatalog::new(
            EquipmentCatalogSource::new(sample.module_sha256, String::new()),
            Vec::new(),
            Vec::new(),
            0,
        );
        let result = EquipmentReadResult::new(catalog, raw_records);
        assert_eq!(result.raw_content_sha256(), sample.raw_content_sha256);

        let projection = map_equipment_details(&result).unwrap();
        let raw = result.raw_records();
        let details = projection.details();
        assert_eq!(details.weapons().len(), raw.weapons().len());
        assert_eq!(details.skills().len(), raw.skills().len());
        for weapon in raw.weapons() {
            assert_eq!(
                details.weapon(weapon.weapon_id).unwrap().weapon_id(),
                weapon.weapon_id
            );
        }
        for skill in raw.skills() {
            let mapped = details.skill(skill.skill_id, skill.level).unwrap();
            assert_eq!(
                (mapped.skill_id(), mapped.level()),
                (skill.skill_id, skill.level)
            );
        }

        // 按原始记录身份核对完整正文，数量随输入变化，漏项、重复或错配仍须失败。
        use crate::domain::{EquipmentConfigId, RawRecordKey};
        let mut expected = std::collections::BTreeMap::new();
        let mut insert = |key, value| assert!(expected.insert(key, value).is_none());
        for config in raw.configs() {
            insert(
                RawRecordKey::EquipmentConfig(EquipmentConfigId::new(config.config_id).unwrap()),
                serde_json::to_value(config).unwrap(),
            );
        }
        for recipe in raw.recipes() {
            insert(
                RawRecordKey::EquipmentRecipe(recipe.recipe_id),
                serde_json::to_value(recipe).unwrap(),
            );
        }
        insert(
            RawRecordKey::EquipmentReferenceNames,
            serde_json::to_value(raw.reference_names()).unwrap(),
        );
        for weapon in raw.weapons() {
            insert(
                RawRecordKey::EquipmentWeapon(weapon.weapon_id),
                serde_json::to_value(weapon).unwrap(),
            );
        }
        for skill in raw.skills() {
            insert(
                RawRecordKey::EquipmentSkill {
                    skill_id: skill.skill_id,
                    level: skill.level,
                },
                serde_json::to_value(skill).unwrap(),
            );
        }
        let records = projection.raw_records();
        assert_eq!(records.source_content_sha256(), result.raw_content_sha256());
        assert_eq!(records.schema_version(), raw.schema_version());
        assert_eq!(records.records().len(), expected.len());
        assert!(
            records
                .records()
                .windows(2)
                .all(|pair| pair[0].key() < pair[1].key())
        );
        for (key, value) in expected {
            let record = records.record(&key).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(record.canonical_json()).unwrap(),
                value,
                "{key:?}"
            );
        }
    }

    #[test]
    fn maps_complete_weapon_with_sampled_variant_fields() {
        let mut raw = weapon_value();
        raw["bullet_ID"] = json!([]);
        raw["charge_param"] = json!({"lockTime": 0.3, "maxLock": 4});
        raw["precast_param"] = json!(" ");
        let weapon = map_weapon(&RuntimeEquipmentWeaponDetail {
            weapon_id: 3_680,
            raw,
            complete: true,
            read_errors: Vec::new(),
        })
        .unwrap();

        assert_eq!(weapon.weapon_id(), 3_680);
        assert_eq!(weapon.damage(), 5);
        assert_eq!(weapon.base_weapon_id(), None);
        assert!(weapon.bullet_ids().is_empty());
        assert_eq!(
            weapon.charge_parameter(),
            WeaponChargeParameter::Lock {
                lock_time: 0.3,
                max_lock: 4,
            }
        );
        assert_eq!(
            weapon.precast_parameter(),
            &WeaponPrecastParameter::LegacyWhitespace
        );
    }

    #[test]
    fn rejects_explicit_null_for_optional_weapon_base() {
        let mut raw = weapon_value();
        raw["base"] = Value::Null;
        let error = map_weapon(&RuntimeEquipmentWeaponDetail {
            weapon_id: 3_680,
            raw,
            complete: true,
            read_errors: Vec::new(),
        })
        .unwrap_err();

        assert!(matches!(
            error,
            EquipmentDetailMappingError::WeaponField { field: "base", .. }
        ));
    }

    #[test]
    fn overlays_current_level_object_on_mixed_skill_base() {
        let record = skill_record();
        let fields =
            materialize_skill_root(&record.battle_skill.value, &record, "battle_skill").unwrap();

        assert_eq!(fields["desc"], json!("等级说明"));
        assert_eq!(fields["name"], json!("测试技能"));
        assert!(fields["effect_list"].is_array());
        let skill = map_skill(&record).unwrap();
        let source = skill.battle_skill().unwrap();
        assert_eq!(source.config_id(), 10);
        assert_eq!(source.description(), "等级说明");
        assert_eq!(source.effects()[0].target_choices(), &["TargetSelf"]);
        assert_eq!(source.effects()[0].triggers(), &["onUpdate"]);
        assert_eq!(
            source.effects()[0].metadata()[0].name(),
            "targetetAniEffect"
        );
    }

    #[test]
    fn preserves_battle_source_config_id_when_it_differs_from_reference() {
        let mut record = skill_record();
        record.battle_skill.value = json!({
            "id": 108050,
            "name": "测试技能",
            "desc": "来源使用独立配置",
            "cd": 0,
            "effect_list": []
        });

        let skill = map_skill(&record).unwrap();
        assert_eq!(skill.skill_id(), 10);
        assert_eq!(skill.display().name(), "测试技能");
        assert_eq!(skill.battle_skill().unwrap().config_id(), 108_050);
    }

    #[test]
    fn rejects_display_id_that_differs_from_equipment_reference() {
        let mut record = skill_record();
        record.display.value["id"] = json!(11);

        let error = map_skill(&record).unwrap_err();
        assert!(matches!(
            error,
            EquipmentDetailMappingError::SkillField {
                field: "display.value",
                ..
            }
        ));
    }

    #[test]
    fn preserves_nested_values_and_mixed_table_key_types() {
        let record = skill_record();
        let value = json!({
            "lua_type": "table",
            "entries": [
                {"key": 1, "key_type": "number", "lua_type": null, "value": {"group": {"id": 10, "level": 1}}},
                {"key": "effect_list", "key_type": "string", "lua_type": null, "value": [true, 0.5, "x"]}
            ],
            "truncated": false,
            "reason": null
        });
        let SkillValue::MixedTable(table) = map_skill_value(&value, &record, "test", 0).unwrap()
        else {
            panic!("混合表应保持独立领域类型");
        };
        assert!(matches!(
            table.entries()[0].key(),
            SkillTableKey::Number(value) if *value == 1.0
        ));
        assert!(matches!(
            table.entries()[1].key(),
            SkillTableKey::String(value) if value == "effect_list"
        ));
    }

    fn weapon_value() -> Value {
        json!({
            "action_index": "attack",
            "aim_type": 1,
            "angle": 180,
            "attack_attribute": 1,
            "attack_attribute_ratio": 100,
            "auto_aftercast": 0.3,
            "axis_angle": 0,
            "barrage_ID": [1115],
            "bullet_ID": [19407],
            "charge_param": "",
            "corrected": 125,
            "damage": 5,
            "effect_move": 0,
            "expose": 0,
            "fire_fx": "CLFire",
            "fire_fx_loop_type": 1,
            "fire_sfx": "battle/cannon-155mm",
            "id": 3680,
            "initial_over_heat": 0,
            "min_range": 0,
            "oxy_type": [1],
            "precast_param": [],
            "queue": 1,
            "range": 70,
            "recover_time": 0.5,
            "reload_max": 405,
            "search_condition": [1],
            "search_type": 1,
            "shakescreen": 0,
            "spawn_bound": "cannon",
            "suppress": 1,
            "torpedo_ammo": 0,
            "type": 2
        })
    }

    fn skill_record() -> RuntimeSkillEffectDetail {
        RuntimeSkillEffectDetail {
            skill_id: 10,
            level: 1,
            display: available_source(json!({
                "id": 10,
                "name": "测试技能",
                "desc": "展示说明",
                "desc_get": "",
                "system_transform": []
            })),
            battle_skill: available_source(json!({
                "lua_type": "table",
                "entries": [
                    {"key": 1, "key_type": "number", "lua_type": null, "value": {"desc": "等级说明"}},
                    {"key": "id", "key_type": "string", "lua_type": null, "value": 10},
                    {"key": "name", "key_type": "string", "lua_type": null, "value": "测试技能"},
                    {"key": "cd", "key_type": "string", "lua_type": null, "value": 0},
                    {"key": "effect_list", "key_type": "string", "lua_type": null, "value": [{
                        "type": "BattleBuffCastSkill",
                        "arg_list": {"group": {"id": 10, "level": 1}},
                        "target_choise": "TargetSelf",
                        "trigger": ["onUpdate"],
                        "targetetAniEffect": "legacy"
                    }]}
                ],
                "truncated": false,
                "reason": null
            })),
            battle_buff: unavailable_source(),
            complete: true,
        }
    }

    fn available_source(value: Value) -> RuntimeSkillEffectSource {
        RuntimeSkillEffectSource {
            available: true,
            complete: true,
            value,
            error: None,
            read_errors: Vec::new(),
        }
    }

    fn unavailable_source() -> RuntimeSkillEffectSource {
        RuntimeSkillEffectSource {
            available: false,
            complete: false,
            value: Value::Null,
            error: Some("测试来源不存在".to_owned()),
            read_errors: Vec::new(),
        }
    }
}

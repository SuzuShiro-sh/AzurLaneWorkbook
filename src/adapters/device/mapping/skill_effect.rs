//! 将通用运行态技能记录转换为舰船名册共享的当前等级效果证据。

use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use super::super::runtime::{RuntimeSkillEffectDetail, RuntimeSkillEffectSource};
use super::equipment_detail::{map_skill_value, materialize_skill_root};
use crate::domain::{
    SkillEffectArgument, SkillEffectEvidence, SkillEffectEvidenceCatalog, SkillEffectEvidenceKey,
    SkillEffectParameterSource, SkillEffectParameters, SkillEffectSourceKind,
};
use suzushiro_content_digest::sorted_json as canonical_json;

/// 把已经按键排序的运行态记录映射为不丢失局部失败信息的共享证据目录。
pub(crate) fn map_skill_effect_evidence(
    records: &[&RuntimeSkillEffectDetail],
) -> Result<SkillEffectEvidenceCatalog, SkillEffectEvidenceMappingError> {
    if let Some(pair) = records
        .windows(2)
        .find(|pair| (pair[0].skill_id, pair[0].level) == (pair[1].skill_id, pair[1].level))
    {
        return Err(SkillEffectEvidenceMappingError::Duplicate {
            skill_id: pair[0].skill_id,
            level: pair[0].level,
        });
    }

    records
        .iter()
        .map(|record| map_evidence(record))
        .collect::<Result<Vec<_>, _>>()
        .map(SkillEffectEvidenceCatalog::new)
}

fn map_evidence(
    record: &RuntimeSkillEffectDetail,
) -> Result<SkillEffectEvidence, SkillEffectEvidenceMappingError> {
    let raw_structure_json =
        canonical_json(record).map_err(|source| SkillEffectEvidenceMappingError::Encode {
            skill_id: record.skill_id,
            level: record.level,
            source,
        })?;
    let mut read_errors = Vec::new();
    if !record.complete {
        append_source_diagnostics("display", &record.display, &mut read_errors);
        append_source_diagnostics("battle_skill", &record.battle_skill, &mut read_errors);
        append_source_diagnostics("battle_buff", &record.battle_buff, &mut read_errors);
    }

    let mut parameter_sources = Vec::new();
    for (kind, source) in [
        (SkillEffectSourceKind::BattleSkill, &record.battle_skill),
        (SkillEffectSourceKind::BattleBuff, &record.battle_buff),
    ] {
        if !source.available || !source.complete {
            continue;
        }
        match map_parameter_source(record, kind, source) {
            Ok(parameters) => parameter_sources.push(parameters),
            Err(message) => read_errors.push(format!("{}.normalize: {message}", kind.as_str())),
        }
    }

    let complete = record.complete && read_errors.is_empty();
    Ok(SkillEffectEvidence::new(
        SkillEffectEvidenceKey::new(record.skill_id, record.level),
        parameter_sources,
        Arc::from(raw_structure_json),
        complete,
        read_errors,
    ))
}

fn append_source_diagnostics(
    source_name: &'static str,
    source: &RuntimeSkillEffectSource,
    diagnostics: &mut Vec<String>,
) {
    if let Some(error) = &source.error {
        diagnostics.push(format!("{source_name}.error: {error}"));
    }
    diagnostics.extend(
        source
            .read_errors
            .iter()
            .map(|error| format!("{source_name}.read_error: {error}")),
    );
}

fn map_parameter_source(
    record: &RuntimeSkillEffectDetail,
    kind: SkillEffectSourceKind,
    source: &RuntimeSkillEffectSource,
) -> Result<SkillEffectParameterSource, String> {
    let source_name = kind.as_str();
    let root = materialize_skill_root(&source.value, record, source_name)
        .map_err(|error| error.to_string())?;
    let effects = root
        .get("effect_list")
        .and_then(Value::as_array)
        .ok_or_else(|| "effect_list 必须是数组".to_owned())?
        .iter()
        .enumerate()
        .map(|(index, effect)| {
            let effect = effect
                .as_object()
                .ok_or_else(|| format!("effect_list 第 {} 项必须是对象", index + 1))?;
            let arguments = effect
                .get("arg_list")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("effect_list 第 {} 项缺少对象 arg_list", index + 1))?;
            let mut arguments = arguments
                .iter()
                .map(|(name, value)| {
                    map_skill_value(value, record, source_name, 0)
                        .map(|value| SkillEffectArgument::new(name.clone(), value))
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            arguments.sort_by(|left, right| left.name().cmp(right.name()));
            let sequence =
                u32::try_from(index + 1).map_err(|_| "effect_list 序号超出 u32 范围".to_owned())?;
            Ok(SkillEffectParameters::new(sequence, arguments))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(SkillEffectParameterSource::new(kind, effects))
}

/// 原始结构无法编码，或同一技能等级出现多份相互冲突的证据。
#[derive(Debug, Error)]
pub(crate) enum SkillEffectEvidenceMappingError {
    #[error("技能效果证据重复: skill_id={skill_id}, level={level}")]
    Duplicate { skill_id: u64, level: u32 },
    #[error("技能效果原始结构编码失败: skill_id={skill_id}, level={level}: {source}")]
    Encode {
        skill_id: u64,
        level: u32,
        #[source]
        source: serde_json::Error,
    },
}

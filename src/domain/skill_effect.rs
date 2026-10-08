//! 保存技能当前等级的规范化效果参数、原始运行态结构和完整性诊断。

use std::sync::Arc;

use super::SkillEffectArgument;

/// 以实际生效技能 ID 和当前等级唯一定位一份效果证据。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SkillEffectEvidenceKey {
    skill_id: u64,
    level: u32,
}

impl SkillEffectEvidenceKey {
    pub(crate) const fn new(skill_id: u64, level: u32) -> Self {
        Self { skill_id, level }
    }

    /// 返回实际生效的技能配置 ID。
    pub const fn skill_id(self) -> u64 {
        self.skill_id
    }

    /// 返回提取效果参数时使用的技能等级。
    pub const fn level(self) -> u32 {
        self.level
    }
}

/// 产生技能效果参数的战斗配置来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillEffectSourceKind {
    /// `GetSkillTemplate` 返回的战斗技能配置。
    BattleSkill,
    /// `GetBuffTemplate` 返回的战斗 Buff 配置。
    BattleBuff,
}

impl SkillEffectSourceKind {
    /// 返回工作簿和诊断信息使用的稳定来源名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BattleSkill => "battle_skill",
            Self::BattleBuff => "battle_buff",
        }
    }
}

/// `effect_list` 中一项效果的当前等级参数。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillEffectParameters {
    sequence: u32,
    arguments: Vec<SkillEffectArgument>,
}

impl SkillEffectParameters {
    pub(crate) fn new(sequence: u32, arguments: Vec<SkillEffectArgument>) -> Self {
        Self {
            sequence,
            arguments,
        }
    }

    /// 返回效果在来源数组中的一基序号。
    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    /// 返回按参数名稳定排列的 `arg_list`。
    pub fn arguments(&self) -> &[SkillEffectArgument] {
        &self.arguments
    }
}

/// 一路战斗配置中规范化后的当前等级效果参数。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillEffectParameterSource {
    kind: SkillEffectSourceKind,
    effects: Vec<SkillEffectParameters>,
}

impl SkillEffectParameterSource {
    pub(crate) fn new(kind: SkillEffectSourceKind, effects: Vec<SkillEffectParameters>) -> Self {
        Self { kind, effects }
    }

    /// 返回参数来自战斗技能还是战斗 Buff。
    pub const fn kind(&self) -> SkillEffectSourceKind {
        self.kind
    }

    /// 返回按运行态声明顺序保存的当前等级效果参数。
    pub fn effects(&self) -> &[SkillEffectParameters] {
        &self.effects
    }
}

/// 一组技能效果参数及其可审计的三路原始读取证据。
#[derive(Clone, Debug, PartialEq)]
pub struct SkillEffectEvidence {
    key: SkillEffectEvidenceKey,
    parameter_sources: Vec<SkillEffectParameterSource>,
    raw_structure_json: Arc<str>,
    complete: bool,
    read_errors: Vec<String>,
}

impl SkillEffectEvidence {
    pub(crate) fn new(
        key: SkillEffectEvidenceKey,
        parameter_sources: Vec<SkillEffectParameterSource>,
        raw_structure_json: Arc<str>,
        complete: bool,
        read_errors: Vec<String>,
    ) -> Self {
        Self {
            key,
            parameter_sources,
            raw_structure_json,
            complete,
            read_errors,
        }
    }

    /// 返回技能 ID 和当前等级组成的唯一键。
    pub const fn key(&self) -> SkillEffectEvidenceKey {
        self.key
    }

    /// 返回可规范化的战斗技能和战斗 Buff 当前等级参数。
    pub fn parameter_sources(&self) -> &[SkillEffectParameterSource] {
        &self.parameter_sources
    }

    /// 返回包含展示、战斗技能、战斗 Buff 和诊断状态的规范 JSON。
    pub fn raw_structure_json(&self) -> &str {
        &self.raw_structure_json
    }

    /// 返回三路读取和参数规范化是否均满足完整性要求。
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// 返回按来源和读取顺序保存的稳定诊断信息。
    pub fn read_errors(&self) -> &[String] {
        &self.read_errors
    }
}

/// 按技能 ID、等级严格升序保存且不重复的共享效果证据目录。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillEffectEvidenceCatalog {
    records: Vec<SkillEffectEvidence>,
}

impl SkillEffectEvidenceCatalog {
    pub(crate) fn new(mut records: Vec<SkillEffectEvidence>) -> Self {
        records.sort_by_key(SkillEffectEvidence::key);
        debug_assert!(
            records.windows(2).all(|pair| pair[0].key() < pair[1].key()),
            "技能效果证据必须按键唯一"
        );
        Self { records }
    }

    /// 返回全部共享技能效果证据。
    pub fn records(&self) -> &[SkillEffectEvidence] {
        &self.records
    }

    /// 按实际生效技能 ID 和等级查找效果证据。
    pub fn evidence(&self, skill_id: u64, level: u32) -> Option<&SkillEffectEvidence> {
        let key = SkillEffectEvidenceKey::new(skill_id, level);
        self.records
            .binary_search_by_key(&key, SkillEffectEvidence::key)
            .ok()
            .map(|index| &self.records[index])
    }

    /// 返回唯一技能效果证据数量。
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// 返回目录是否为空。
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

//! 保存规范化模型无法承载的原始记录正文及其稳定身份。

use std::sync::Arc;

use super::EquipmentConfigId;

/// 一条原始记录在设备读取结果中的稳定身份。
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RawRecordKey {
    /// 一种具体强化配置的完整原始记录。
    EquipmentConfig(EquipmentConfigId),
    /// 一条静态装备合成配方。
    EquipmentRecipe(u64),
    /// 当前客户端解析出的装备引用名称集合。
    EquipmentReferenceNames,
    /// 一条武器参数记录。
    EquipmentWeapon(u64),
    /// 指定等级的装备技能记录。
    EquipmentSkill { skill_id: u64, level: u32 },
    /// 舰船固定白名单表中的一条完整静态记录。
    ShipCatalog { table_key: String, record_id: u64 },
    /// 指定等级的舰船静态技能效果记录。
    ShipSkill { skill_id: u64, level: u32 },
}

/// 一条按对象键排序并紧凑编码的原始 JSON 记录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawRecord {
    key: RawRecordKey,
    content_sha256: String,
    canonical_json: Arc<str>,
}

impl RawRecord {
    pub(crate) fn new(key: RawRecordKey, content_sha256: String, canonical_json: Arc<str>) -> Self {
        Self {
            key,
            content_sha256,
            canonical_json,
        }
    }

    /// 返回记录的实体类别和稳定标识。
    pub const fn key(&self) -> &RawRecordKey {
        &self.key
    }

    /// 返回规范 JSON 正文的 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    /// 返回按对象键排序的紧凑 JSON 正文。
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }
}

/// 按稳定记录键严格排序的完整原始记录集合。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawRecordSet {
    schema_version: u32,
    source_content_sha256: String,
    records: Vec<RawRecord>,
}

impl RawRecordSet {
    pub(crate) fn new(
        schema_version: u32,
        source_content_sha256: String,
        mut records: Vec<RawRecord>,
    ) -> Self {
        records.sort_by(|left, right| left.key().cmp(right.key()));
        debug_assert!(
            records.windows(2).all(|pair| pair[0].key() < pair[1].key()),
            "原始记录必须按稳定键唯一"
        );
        Self {
            schema_version,
            source_content_sha256,
            records,
        }
    }

    /// 返回原始记录契约版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回全部原始来源组合后的稳定内容摘要。
    pub fn source_content_sha256(&self) -> &str {
        &self.source_content_sha256
    }

    /// 返回按稳定键严格升序排列的全部原始记录。
    pub fn records(&self) -> &[RawRecord] {
        &self.records
    }

    /// 按稳定键查找一条原始记录。
    pub fn record(&self, key: &RawRecordKey) -> Option<&RawRecord> {
        self.records
            .binary_search_by(|record| record.key().cmp(key))
            .ok()
            .map(|index| &self.records[index])
    }
}

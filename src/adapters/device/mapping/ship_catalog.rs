//! 将完整舰船静态表映射为组级导航、技能证据和原始记录。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use super::super::reading::ship_catalog::{ShipCatalogReadResult, ShipCatalogTable};
use super::super::runtime::{
    RuntimeProtocolError, RuntimeSkillEffectDetail, ShipCatalogRecord, ShipCatalogTableKey,
    SkillEffectQuery,
};
use super::skill_effect::map_skill_effect_evidence;
use crate::domain::{
    RawRecord, RawRecordKey, ShipCatalog, ShipCatalogGroup, ShipCatalogSource, ShipStaticSkill,
};
use suzushiro_content_digest::{sha256_bytes, sha256_sorted_json, sorted_json as canonical_json};

/// 领域映射结果及待并入完整状态的全部原始记录。
pub(crate) struct ShipCatalogProjection {
    catalog: ShipCatalog,
    raw_records: Vec<RawRecord>,
}

impl ShipCatalogProjection {
    pub(crate) fn into_parts(self) -> (ShipCatalog, Vec<RawRecord>) {
        (self.catalog, self.raw_records)
    }
}

/// 按请求范围映射固定舰船表和底层技能证据。
pub(crate) fn map_ship_catalog_with_scope(
    source: &ShipCatalogReadResult,
    skill_effects: &[RuntimeSkillEffectDetail],
    expected_module_sha256: &str,
    scope: crate::domain::GameReadScope,
) -> Result<ShipCatalogProjection, ShipCatalogMappingError> {
    if source.module_sha256() != expected_module_sha256 {
        return Err(ShipCatalogMappingError::ModuleMismatch {
            expected: expected_module_sha256.to_owned(),
            actual: source.module_sha256().to_owned(),
        });
    }

    let tables = CatalogTables::new(source)?;
    let group_sources = map_group_sources(&tables)?;
    let skill_ids = group_sources
        .iter()
        .flat_map(|group| group.skill_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let skills = map_static_skills(
        &tables,
        &skill_ids,
        skill_effects,
        scope.ship_skill_effects(),
    )?;
    let evidence_refs = sorted_skill_effect_refs(skill_effects);
    let evidence = map_skill_effect_evidence(&evidence_refs)
        .map_err(|source| ShipCatalogMappingError::SkillEffect(source.to_string()))?;
    let groups = group_sources
        .into_iter()
        .map(GroupSource::into_domain)
        .collect();
    let catalog = ShipCatalog::new(
        ShipCatalogSource::new(
            source.module_sha256().to_owned(),
            source.content_sha256().to_owned(),
        ),
        groups,
        skills,
        evidence,
    )
    .with_technology_history(source.tables().iter().any(|table| {
        table.table_key() == ShipCatalogTableKey::CollectionShipGroup
            && table.read_error().is_none()
    }))
    .with_technology_error(
        source
            .tables()
            .iter()
            .filter(|table| {
                ShipCatalogTableKey::TECHNOLOGY.contains(&table.table_key())
                    && table.table_key() != ShipCatalogTableKey::CollectionShipGroup
            })
            .find_map(|table| table.read_error().map(str::to_owned)),
    );
    let raw_records = map_raw_records(source, skill_effects)?;
    Ok(ShipCatalogProjection {
        catalog,
        raw_records,
    })
}

/// 从已经严格闭合的显式舰船关系生成全部静态技能等级请求。
pub(crate) fn collect_static_skill_queries(
    source: &ShipCatalogReadResult,
) -> Result<Vec<SkillEffectQuery>, ShipCatalogMappingError> {
    let tables = CatalogTables::new(source)?;
    let skill_ids = map_group_sources(&tables)?
        .into_iter()
        .flat_map(|group| group.skill_ids)
        .collect::<BTreeSet<_>>();
    let templates = tables.get(ShipCatalogTableKey::SkillDataTemplate);
    let mut queries = Vec::new();
    for skill_id in skill_ids {
        let Some(template) = templates.get(&skill_id).copied() else {
            continue;
        };
        let raw = object(ShipCatalogTableKey::SkillDataTemplate, template)?;
        let max_level = u32::try_from(required_nonnegative_u64(
            ShipCatalogTableKey::SkillDataTemplate,
            skill_id,
            raw,
            "max_level",
        )?)
        .map_err(|_| ShipCatalogMappingError::FieldInvalid {
            table: ShipCatalogTableKey::SkillDataTemplate,
            record_id: skill_id,
            field: "max_level",
            message: "必须可表示为 u32".to_owned(),
        })?;
        let levels = if max_level == 0 { 1..=1 } else { 1..=max_level };
        for level in levels {
            queries.push(SkillEffectQuery::new(skill_id, level)?);
        }
    }
    Ok(queries)
}

fn sorted_skill_effect_refs(
    records: &[RuntimeSkillEffectDetail],
) -> Vec<&RuntimeSkillEffectDetail> {
    let mut records = records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| (record.skill_id, record.level));
    records
}

struct CatalogTables<'a> {
    tables: BTreeMap<ShipCatalogTableKey, BTreeMap<u64, &'a ShipCatalogRecord>>,
}

impl<'a> CatalogTables<'a> {
    fn new(source: &'a ShipCatalogReadResult) -> Result<Self, ShipCatalogMappingError> {
        let mut tables = BTreeMap::new();
        for expected in ShipCatalogTableKey::ALL {
            let table = source
                .tables()
                .iter()
                .find(|table| table.table_key() == expected)
                .ok_or(ShipCatalogMappingError::TableMissing { table: expected })?;
            if tables.insert(expected, index_table(table)).is_some() {
                return Err(ShipCatalogMappingError::TableDuplicate { table: expected });
            }
        }
        for table in source
            .tables()
            .iter()
            .filter(|table| ShipCatalogTableKey::TECHNOLOGY.contains(&table.table_key()))
        {
            if tables
                .insert(table.table_key(), index_table(table))
                .is_some()
            {
                return Err(ShipCatalogMappingError::TableDuplicate {
                    table: table.table_key(),
                });
            }
        }
        if tables.len() != source.tables().len() {
            return Err(ShipCatalogMappingError::TableCount {
                expected: tables.len(),
                actual: source.tables().len(),
            });
        }
        Ok(Self { tables })
    }

    fn get(&self, table: ShipCatalogTableKey) -> &BTreeMap<u64, &'a ShipCatalogRecord> {
        self.tables.get(&table).expect("固定舰船目录表已经完整验证")
    }
}

fn index_table(table: &ShipCatalogTable) -> BTreeMap<u64, &ShipCatalogRecord> {
    table
        .records()
        .iter()
        .map(|record| (record.id, record))
        .collect()
}

struct GroupSource {
    group_id: u64,
    representative_config_id: u64,
    name: String,
    english_name: String,
    ship_type_id: u64,
    nation_id: u64,
    armor_type_id: u64,
    rarity: u8,
    maximum_level: u32,
    maximum_stars: u32,
    slot_allowed_equipment_type_ids: [Vec<u64>; 5],
    variant_config_ids: Vec<u64>,
    relationship_raw_refs: Vec<String>,
    skill_ids: Vec<u64>,
}

impl GroupSource {
    fn into_domain(self) -> ShipCatalogGroup {
        ShipCatalogGroup::new(
            self.group_id,
            self.representative_config_id,
            self.name,
            self.english_name,
            self.ship_type_id,
            self.nation_id,
            self.armor_type_id,
            self.rarity,
            self.maximum_level,
            self.maximum_stars,
            self.slot_allowed_equipment_type_ids,
            self.variant_config_ids,
            self.relationship_raw_refs,
            self.skill_ids,
        )
    }
}

fn map_group_sources(
    tables: &CatalogTables<'_>,
) -> Result<Vec<GroupSource>, ShipCatalogMappingError> {
    let groups = tables.get(ShipCatalogTableKey::ShipDataGroup);
    let templates = tables.get(ShipCatalogTableKey::ShipDataTemplate);
    let statistics = tables.get(ShipCatalogTableKey::ShipDataStatistics);
    let mut group_records = BTreeMap::new();
    for record in groups.values() {
        let raw = object(ShipCatalogTableKey::ShipDataGroup, record)?;
        let group_id = required_u64(
            ShipCatalogTableKey::ShipDataGroup,
            record.id,
            raw,
            "group_type",
        )?;
        if group_records.insert(group_id, *record).is_some() {
            return Err(ShipCatalogMappingError::DuplicateGroup { group_id });
        }
    }

    let mut variants = BTreeMap::<u64, Vec<u64>>::new();
    for record in templates.values() {
        let raw = object(ShipCatalogTableKey::ShipDataTemplate, record)?;
        let group_id = required_u64(
            ShipCatalogTableKey::ShipDataTemplate,
            record.id,
            raw,
            "group_type",
        )?;
        if !group_records.contains_key(&group_id) {
            return Err(ShipCatalogMappingError::GroupMissing {
                table: ShipCatalogTableKey::ShipDataTemplate,
                record_id: record.id,
                group_id,
            });
        }
        if !statistics.contains_key(&record.id) {
            return Err(ShipCatalogMappingError::RelatedRecordMissing {
                source_table: ShipCatalogTableKey::ShipDataTemplate,
                source_id: record.id,
                field: "id",
                target_table: ShipCatalogTableKey::ShipDataStatistics,
                target_id: record.id,
            });
        }
        variants.entry(group_id).or_default().push(record.id);
    }
    if variants.len() != group_records.len() {
        let group_id = group_records
            .keys()
            .find(|group_id| !variants.contains_key(group_id))
            .copied()
            .unwrap_or_default();
        return Err(ShipCatalogMappingError::GroupWithoutTemplate { group_id });
    }

    let mut result = Vec::with_capacity(group_records.len());
    for (group_id, group_record) in group_records {
        let mut variant_config_ids = variants.remove(&group_id).unwrap_or_default();
        variant_config_ids.sort_unstable();
        let representative_config_id = group_id
            .checked_mul(10)
            .and_then(|value| value.checked_add(1))
            .ok_or(ShipCatalogMappingError::RepresentativeOverflow { group_id })?;
        if variant_config_ids
            .binary_search(&representative_config_id)
            .is_err()
        {
            return Err(ShipCatalogMappingError::RepresentativeMissing {
                group_id,
                config_id: representative_config_id,
            });
        }
        let representative_template = templates[&representative_config_id];
        let representative_statistics = statistics[&representative_config_id];
        let template_raw = object(
            ShipCatalogTableKey::ShipDataTemplate,
            representative_template,
        )?;
        let statistics_raw = object(
            ShipCatalogTableKey::ShipDataStatistics,
            representative_statistics,
        )?;
        let mut relationship_raw_refs = BTreeSet::new();
        relationship_raw_refs.insert(raw_ref(ShipCatalogTableKey::ShipDataGroup, group_record.id));
        let mut skill_ids = BTreeSet::new();
        collect_known_skill_list(
            ShipCatalogTableKey::ShipDataGroup,
            group_record.id,
            object(ShipCatalogTableKey::ShipDataGroup, group_record)?,
            "trans_skill",
            &mut skill_ids,
        )?;
        for config_id in &variant_config_ids {
            relationship_raw_refs
                .insert(raw_ref(ShipCatalogTableKey::ShipDataTemplate, *config_id));
            relationship_raw_refs
                .insert(raw_ref(ShipCatalogTableKey::ShipDataStatistics, *config_id));
            let template = templates[config_id];
            let raw = object(ShipCatalogTableKey::ShipDataTemplate, template)?;
            for field in ["buff_list", "buff_list_display", "hide_buff_list"] {
                collect_known_skill_list(
                    ShipCatalogTableKey::ShipDataTemplate,
                    *config_id,
                    raw,
                    field,
                    &mut skill_ids,
                )?;
            }
            add_template_strengthen_ref(
                tables,
                group_id,
                *config_id,
                raw,
                &mut relationship_raw_refs,
            )?;
            for table in [
                ShipCatalogTableKey::ShipDataBreakout,
                ShipCatalogTableKey::ShipMetaBreakout,
            ] {
                if tables.get(table).contains_key(config_id) {
                    relationship_raw_refs.insert(raw_ref(table, *config_id));
                }
            }
        }
        add_retrofit_relations(tables, group_id, &mut relationship_raw_refs, &mut skill_ids)?;
        add_blueprint_relations(tables, group_id, &mut relationship_raw_refs, &mut skill_ids)?;
        add_meta_relations(tables, &variant_config_ids, &mut relationship_raw_refs)?;

        result.push(GroupSource {
            group_id,
            representative_config_id,
            name: required_string(
                ShipCatalogTableKey::ShipDataStatistics,
                representative_config_id,
                statistics_raw,
                "name",
            )?
            .to_owned(),
            english_name: optional_string(statistics_raw, "english_name")?
                .unwrap_or_default()
                .to_owned(),
            ship_type_id: required_u64(
                ShipCatalogTableKey::ShipDataTemplate,
                representative_config_id,
                template_raw,
                "type",
            )?,
            nation_id: required_u64(
                ShipCatalogTableKey::ShipDataStatistics,
                representative_config_id,
                statistics_raw,
                "nationality",
            )?,
            armor_type_id: required_u64(
                ShipCatalogTableKey::ShipDataStatistics,
                representative_config_id,
                statistics_raw,
                "armor_type",
            )?,
            rarity: u8::try_from(required_u64(
                ShipCatalogTableKey::ShipDataStatistics,
                representative_config_id,
                statistics_raw,
                "rarity",
            )?)
            .map_err(|_| ShipCatalogMappingError::FieldInvalid {
                table: ShipCatalogTableKey::ShipDataStatistics,
                record_id: representative_config_id,
                field: "rarity",
                message: "必须可表示为 u8".to_owned(),
            })?,
            maximum_level: u32::try_from(required_u64(
                ShipCatalogTableKey::ShipDataTemplate,
                representative_config_id,
                template_raw,
                "max_level",
            )?)
            .map_err(|_| ShipCatalogMappingError::FieldInvalid {
                table: ShipCatalogTableKey::ShipDataTemplate,
                record_id: representative_config_id,
                field: "max_level",
                message: "必须可表示为 u32".to_owned(),
            })?,
            maximum_stars: u32::try_from(required_u64(
                ShipCatalogTableKey::ShipDataTemplate,
                representative_config_id,
                template_raw,
                "star_max",
            )?)
            .map_err(|_| ShipCatalogMappingError::FieldInvalid {
                table: ShipCatalogTableKey::ShipDataTemplate,
                record_id: representative_config_id,
                field: "star_max",
                message: "必须可表示为 u32".to_owned(),
            })?,
            slot_allowed_equipment_type_ids: [
                required_positive_u64_array(
                    ShipCatalogTableKey::ShipDataTemplate,
                    representative_config_id,
                    template_raw,
                    "equip_1",
                )?,
                required_positive_u64_array(
                    ShipCatalogTableKey::ShipDataTemplate,
                    representative_config_id,
                    template_raw,
                    "equip_2",
                )?,
                required_positive_u64_array(
                    ShipCatalogTableKey::ShipDataTemplate,
                    representative_config_id,
                    template_raw,
                    "equip_3",
                )?,
                required_positive_u64_array(
                    ShipCatalogTableKey::ShipDataTemplate,
                    representative_config_id,
                    template_raw,
                    "equip_4",
                )?,
                required_positive_u64_array(
                    ShipCatalogTableKey::ShipDataTemplate,
                    representative_config_id,
                    template_raw,
                    "equip_5",
                )?,
            ],
            variant_config_ids,
            relationship_raw_refs: relationship_raw_refs.into_iter().collect(),
            skill_ids: skill_ids.into_iter().collect(),
        });
    }
    Ok(result)
}

fn add_template_strengthen_ref(
    tables: &CatalogTables<'_>,
    group_id: u64,
    config_id: u64,
    raw: &Map<String, Value>,
    refs: &mut BTreeSet<String>,
) -> Result<(), ShipCatalogMappingError> {
    let Some(value) = raw.get("strengthen_id") else {
        return Ok(());
    };
    let Some(strengthen_id) = value.as_u64() else {
        return invalid_field(
            ShipCatalogTableKey::ShipDataTemplate,
            config_id,
            "strengthen_id",
            "必须是非负整数",
        );
    };
    if strengthen_id == 0 {
        return Ok(());
    }
    if let Some(meta) = tables
        .get(ShipCatalogTableKey::ShipStrengthenMeta)
        .get(&strengthen_id)
        .copied()
    {
        let meta_raw = object(ShipCatalogTableKey::ShipStrengthenMeta, meta)?;
        let representative_config_id = group_id
            .checked_mul(10)
            .and_then(|value| value.checked_add(1))
            .ok_or(ShipCatalogMappingError::RepresentativeOverflow { group_id })?;
        if required_u64(
            ShipCatalogTableKey::ShipStrengthenMeta,
            strengthen_id,
            meta_raw,
            "ship_id",
        )? == representative_config_id
        {
            refs.insert(raw_ref(
                ShipCatalogTableKey::ShipStrengthenMeta,
                strengthen_id,
            ));
            return Ok(());
        }
    }
    if tables
        .get(ShipCatalogTableKey::ShipDataStrengthen)
        .contains_key(&strengthen_id)
    {
        refs.insert(raw_ref(
            ShipCatalogTableKey::ShipDataStrengthen,
            strengthen_id,
        ));
        return Ok(());
    }
    Err(ShipCatalogMappingError::RelatedRecordMissing {
        source_table: ShipCatalogTableKey::ShipDataTemplate,
        source_id: config_id,
        field: "strengthen_id",
        target_table: ShipCatalogTableKey::ShipDataStrengthen,
        target_id: strengthen_id,
    })
}

fn add_retrofit_relations(
    tables: &CatalogTables<'_>,
    group_id: u64,
    refs: &mut BTreeSet<String>,
    skills: &mut BTreeSet<u64>,
) -> Result<(), ShipCatalogMappingError> {
    if tables
        .get(ShipCatalogTableKey::ShipTransform)
        .contains_key(&group_id)
    {
        refs.insert(raw_ref(ShipCatalogTableKey::ShipTransform, group_id));
    }

    for record in tables.get(ShipCatalogTableKey::ShipDataTrans).values() {
        let raw = object(ShipCatalogTableKey::ShipDataTrans, record)?;
        if required_u64(
            ShipCatalogTableKey::ShipDataTrans,
            record.id,
            raw,
            "group_id",
        )? != group_id
        {
            continue;
        }
        refs.insert(raw_ref(ShipCatalogTableKey::ShipDataTrans, record.id));
        let columns = required_array(
            ShipCatalogTableKey::ShipDataTrans,
            record.id,
            raw,
            "transform_list",
        )?;
        let mut pending = Vec::new();
        for column in columns {
            let nodes = value_array(
                ShipCatalogTableKey::ShipDataTrans,
                record.id,
                "transform_list",
                column,
            )?;
            for node in nodes {
                let pair = value_array(
                    ShipCatalogTableKey::ShipDataTrans,
                    record.id,
                    "transform_list",
                    node,
                )?;
                if pair.len() != 2 {
                    return invalid_field(
                        ShipCatalogTableKey::ShipDataTrans,
                        record.id,
                        "transform_list",
                        "节点必须是 [位置, 节点ID]",
                    );
                }
                let node_id = positive_value_u64(
                    ShipCatalogTableKey::ShipDataTrans,
                    record.id,
                    "transform_list",
                    &pair[1],
                )?;
                pending.push(node_id);
            }
        }
        let mut visited = BTreeSet::new();
        while let Some(node_id) = pending.pop() {
            if !visited.insert(node_id) {
                continue;
            }
            let node = related_record(
                tables,
                ShipCatalogTableKey::ShipDataTrans,
                record.id,
                "transform_list",
                ShipCatalogTableKey::TransformDataTemplate,
                node_id,
            )?;
            refs.insert(raw_ref(ShipCatalogTableKey::TransformDataTemplate, node_id));
            let node_raw = object(ShipCatalogTableKey::TransformDataTemplate, node)?;
            add_positive_field_skill(node_raw, "skill_id", skills)?;
            if let Some(effects) = node_raw.get("effect") {
                for effect in value_array(
                    ShipCatalogTableKey::TransformDataTemplate,
                    node_id,
                    "effect",
                    effects,
                )? {
                    if let Some(effect) = effect.as_object() {
                        add_positive_field_skill(effect, "skill_id", skills)?;
                    }
                }
            }
            for field in ["condition_id", "edit_trans"] {
                collect_positive_u64_values(
                    ShipCatalogTableKey::TransformDataTemplate,
                    node_id,
                    node_raw.get(field),
                    field,
                    &mut pending,
                )?;
            }
        }
    }
    Ok(())
}

fn add_blueprint_relations(
    tables: &CatalogTables<'_>,
    group_id: u64,
    refs: &mut BTreeSet<String>,
    skills: &mut BTreeSet<u64>,
) -> Result<(), ShipCatalogMappingError> {
    let Some(blueprint) = tables
        .get(ShipCatalogTableKey::ShipDataBlueprint)
        .get(&group_id)
        .copied()
    else {
        return Ok(());
    };
    refs.insert(raw_ref(ShipCatalogTableKey::ShipDataBlueprint, group_id));
    let raw = object(ShipCatalogTableKey::ShipDataBlueprint, blueprint)?;
    collect_known_skill_tree(
        ShipCatalogTableKey::ShipDataBlueprint,
        group_id,
        raw.get("change_skill"),
        "change_skill",
        skills,
    )?;
    for field in ["strengthen_effect", "fate_strengthen"] {
        let mut effect_ids = Vec::new();
        collect_positive_u64_values(
            ShipCatalogTableKey::ShipDataBlueprint,
            group_id,
            raw.get(field),
            field,
            &mut effect_ids,
        )?;
        for effect_id in effect_ids {
            let effect = related_record(
                tables,
                ShipCatalogTableKey::ShipDataBlueprint,
                group_id,
                field,
                ShipCatalogTableKey::ShipStrengthenBlueprint,
                effect_id,
            )?;
            refs.insert(raw_ref(
                ShipCatalogTableKey::ShipStrengthenBlueprint,
                effect_id,
            ));
            let effect_raw = object(ShipCatalogTableKey::ShipStrengthenBlueprint, effect)?;
            for skill_field in ["change_skill", "effect_skill"] {
                collect_known_skill_tree(
                    ShipCatalogTableKey::ShipStrengthenBlueprint,
                    effect_id,
                    effect_raw.get(skill_field),
                    skill_field,
                    skills,
                )?;
            }
            add_optional_related_ref(
                tables,
                ShipCatalogTableKey::ShipStrengthenBlueprint,
                effect_id,
                effect_raw,
                "effect_breakout",
                ShipCatalogTableKey::ShipDataBreakout,
                refs,
            )?;
        }
    }
    Ok(())
}

fn add_meta_relations(
    tables: &CatalogTables<'_>,
    variant_ids: &[u64],
    refs: &mut BTreeSet<String>,
) -> Result<(), ShipCatalogMappingError> {
    for record in tables.get(ShipCatalogTableKey::ShipStrengthenMeta).values() {
        let raw = object(ShipCatalogTableKey::ShipStrengthenMeta, record)?;
        let ship_id = required_u64(
            ShipCatalogTableKey::ShipStrengthenMeta,
            record.id,
            raw,
            "ship_id",
        )?;
        if variant_ids.binary_search(&ship_id).is_err() {
            continue;
        }
        refs.insert(raw_ref(ShipCatalogTableKey::ShipStrengthenMeta, record.id));
        if let Some(repair_effects) = raw.get("repair_effect") {
            for value in value_array(
                ShipCatalogTableKey::ShipStrengthenMeta,
                record.id,
                "repair_effect",
                repair_effects,
            )? {
                let pair = value_array(
                    ShipCatalogTableKey::ShipStrengthenMeta,
                    record.id,
                    "repair_effect",
                    value,
                )?;
                if pair.len() != 2 {
                    return invalid_field(
                        ShipCatalogTableKey::ShipStrengthenMeta,
                        record.id,
                        "repair_effect",
                        "条目必须是 [进度, 效果ID]",
                    );
                }
                let effect_id = positive_value_u64(
                    ShipCatalogTableKey::ShipStrengthenMeta,
                    record.id,
                    "repair_effect",
                    &pair[1],
                )?;
                related_record(
                    tables,
                    ShipCatalogTableKey::ShipStrengthenMeta,
                    record.id,
                    "repair_effect",
                    ShipCatalogTableKey::ShipMetaRepairEffect,
                    effect_id,
                )?;
                refs.insert(raw_ref(
                    ShipCatalogTableKey::ShipMetaRepairEffect,
                    effect_id,
                ));
            }
        }
        for field in [
            "repair_air",
            "repair_cannon",
            "repair_reload",
            "repair_torpedo",
        ] {
            let mut repair_ids = Vec::new();
            collect_positive_u64_values(
                ShipCatalogTableKey::ShipStrengthenMeta,
                record.id,
                raw.get(field),
                field,
                &mut repair_ids,
            )?;
            for repair_id in repair_ids {
                related_record(
                    tables,
                    ShipCatalogTableKey::ShipStrengthenMeta,
                    record.id,
                    field,
                    ShipCatalogTableKey::ShipMetaRepair,
                    repair_id,
                )?;
                refs.insert(raw_ref(ShipCatalogTableKey::ShipMetaRepair, repair_id));
            }
        }
    }
    Ok(())
}

fn map_static_skills(
    tables: &CatalogTables<'_>,
    skill_ids: &BTreeSet<u64>,
    effects: &[RuntimeSkillEffectDetail],
    effects_requested: bool,
) -> Result<Vec<ShipStaticSkill>, ShipCatalogMappingError> {
    let templates = tables.get(ShipCatalogTableKey::SkillDataTemplate);
    let displays = tables.get(ShipCatalogTableKey::SkillDataDisplay);
    let effect_index = effects
        .iter()
        .map(|effect| ((effect.skill_id, effect.level), effect))
        .collect::<BTreeMap<_, _>>();
    let mut result = Vec::with_capacity(skill_ids.len());
    for skill_id in skill_ids {
        let template = templates.get(skill_id).copied();
        let display = displays.get(skill_id).copied();
        let (name, description, declared_max_level, effect_levels, definition_raw_ref, mut gap) =
            if let Some(template) = template {
                let raw = object(ShipCatalogTableKey::SkillDataTemplate, template)?;
                let max_level = u32::try_from(required_nonnegative_u64(
                    ShipCatalogTableKey::SkillDataTemplate,
                    *skill_id,
                    raw,
                    "max_level",
                )?)
                .map_err(|_| ShipCatalogMappingError::FieldInvalid {
                    table: ShipCatalogTableKey::SkillDataTemplate,
                    record_id: *skill_id,
                    field: "max_level",
                    message: "必须可表示为 u32".to_owned(),
                })?;
                let levels = if max_level == 0 {
                    vec![1]
                } else {
                    (1..=max_level).collect()
                };
                (
                    optional_string(raw, "name")?.unwrap_or_default().to_owned(),
                    optional_string(raw, "desc")?.unwrap_or_default().to_owned(),
                    max_level,
                    levels,
                    Some(raw_ref(ShipCatalogTableKey::SkillDataTemplate, *skill_id)),
                    None,
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    0,
                    Vec::new(),
                    None,
                    Some("关联技能缺少 skill_data_template 定义，未推定等级范围".to_owned()),
                )
            };
        let display_name = match display {
            Some(record) => optional_string(
                object(ShipCatalogTableKey::SkillDataDisplay, record)?,
                "name",
            )?
            .unwrap_or_default(),
            None => "",
        };
        let name = if name.is_empty() {
            display_name.to_owned()
        } else {
            name
        };
        for level in effect_levels.iter().filter(|_| effects_requested) {
            match effect_index.get(&(*skill_id, *level)) {
                Some(effect) if effect.complete => {}
                Some(_) => append_gap(
                    &mut gap,
                    format!("skill_id={skill_id} level={level} 的效果证据不完整"),
                ),
                None => append_gap(
                    &mut gap,
                    format!("缺少 skill_id={skill_id} level={level} 的效果证据"),
                ),
            }
        }
        result.push(ShipStaticSkill::new(
            *skill_id,
            name,
            description,
            declared_max_level,
            effect_levels,
            definition_raw_ref,
            display.map(|_| raw_ref(ShipCatalogTableKey::SkillDataDisplay, *skill_id)),
            gap,
        ));
    }
    Ok(result)
}

fn append_gap(target: &mut Option<String>, message: String) {
    match target {
        Some(target) => {
            target.push_str(" | ");
            target.push_str(&message);
        }
        None => *target = Some(message),
    }
}

fn map_raw_records(
    source: &ShipCatalogReadResult,
    skill_effects: &[RuntimeSkillEffectDetail],
) -> Result<Vec<RawRecord>, ShipCatalogMappingError> {
    let mut records = Vec::with_capacity(source.record_count() + skill_effects.len());
    for table in source.tables() {
        for record in table.records() {
            records.push(raw_record(
                RawRecordKey::ShipCatalog {
                    table_key: table.table_key().as_str().to_owned(),
                    record_id: record.id,
                },
                &record.raw,
            )?);
        }
    }
    for record in skill_effects {
        records.push(raw_record(
            RawRecordKey::ShipSkill {
                skill_id: record.skill_id,
                level: record.level,
            },
            record,
        )?);
    }
    Ok(records)
}

fn raw_record<T: Serialize>(
    key: RawRecordKey,
    value: &T,
) -> Result<RawRecord, ShipCatalogMappingError> {
    let encoded = canonical_json(value).map_err(|source| ShipCatalogMappingError::EncodeRaw {
        key: key.clone(),
        source,
    })?;
    Ok(RawRecord::new(
        key,
        sha256_bytes(encoded.as_bytes()),
        Arc::from(encoded),
    ))
}

#[derive(Serialize)]
pub(crate) struct CombinedRawDigestInput<'a> {
    pub(crate) schema_version: u32,
    pub(crate) equipment_raw_content_sha256: &'a str,
    pub(crate) ship_catalog_content_sha256: &'a str,
    pub(crate) ship_skill_effects_content_sha256: String,
}

pub(crate) fn combined_raw_content_sha256(
    equipment_raw_content_sha256: &str,
    ship_catalog_content_sha256: &str,
    ship_skill_effects: &[RuntimeSkillEffectDetail],
) -> Result<String, ShipCatalogMappingError> {
    let input = CombinedRawDigestInput {
        schema_version: 3,
        equipment_raw_content_sha256,
        ship_catalog_content_sha256,
        ship_skill_effects_content_sha256: sha256_sorted_json(ship_skill_effects)?,
    };
    Ok(sha256_sorted_json(&input)?)
}

fn add_optional_related_ref(
    tables: &CatalogTables<'_>,
    source_table: ShipCatalogTableKey,
    source_id: u64,
    raw: &Map<String, Value>,
    field: &'static str,
    target_table: ShipCatalogTableKey,
    refs: &mut BTreeSet<String>,
) -> Result<(), ShipCatalogMappingError> {
    let Some(value) = raw.get(field) else {
        return Ok(());
    };
    let Some(target_id) = value.as_u64() else {
        return invalid_field(source_table, source_id, field, "必须是非负整数");
    };
    if target_id == 0 {
        return Ok(());
    }
    related_record(
        tables,
        source_table,
        source_id,
        field,
        target_table,
        target_id,
    )?;
    refs.insert(raw_ref(target_table, target_id));
    Ok(())
}

fn related_record<'a>(
    tables: &'a CatalogTables<'a>,
    source_table: ShipCatalogTableKey,
    source_id: u64,
    field: &'static str,
    target_table: ShipCatalogTableKey,
    target_id: u64,
) -> Result<&'a ShipCatalogRecord, ShipCatalogMappingError> {
    tables.get(target_table).get(&target_id).copied().ok_or(
        ShipCatalogMappingError::RelatedRecordMissing {
            source_table,
            source_id,
            field,
            target_table,
            target_id,
        },
    )
}

fn collect_known_skill_list(
    table: ShipCatalogTableKey,
    record_id: u64,
    raw: &Map<String, Value>,
    field: &'static str,
    output: &mut BTreeSet<u64>,
) -> Result<(), ShipCatalogMappingError> {
    collect_known_skill_tree(table, record_id, raw.get(field), field, output)
}

fn collect_known_skill_tree(
    table: ShipCatalogTableKey,
    record_id: u64,
    value: Option<&Value>,
    field: &'static str,
    output: &mut BTreeSet<u64>,
) -> Result<(), ShipCatalogMappingError> {
    let Some(value) = value else {
        return Ok(());
    };
    match value {
        Value::Null => Ok(()),
        Value::String(value) if value.is_empty() => Ok(()),
        Value::Number(value) => {
            let value = value
                .as_u64()
                .ok_or_else(|| ShipCatalogMappingError::FieldInvalid {
                    table,
                    record_id,
                    field,
                    message: "技能引用必须是非负整数".to_owned(),
                })?;
            if value > 0 {
                output.insert(value);
            }
            Ok(())
        }
        Value::Array(values) => {
            for value in values {
                collect_known_skill_tree(table, record_id, Some(value), field, output)?;
            }
            Ok(())
        }
        _ => invalid_field(table, record_id, field, "技能引用必须是整数或整数数组"),
    }
}

fn add_positive_field_skill(
    raw: &Map<String, Value>,
    field: &'static str,
    output: &mut BTreeSet<u64>,
) -> Result<(), ShipCatalogMappingError> {
    let Some(value) = raw.get(field) else {
        return Ok(());
    };
    let Some(value) = value.as_u64() else {
        return Err(ShipCatalogMappingError::FieldInvalid {
            table: ShipCatalogTableKey::TransformDataTemplate,
            record_id: raw.get("id").and_then(Value::as_u64).unwrap_or_default(),
            field,
            message: "必须是非负整数".to_owned(),
        });
    };
    if value > 0 {
        output.insert(value);
    }
    Ok(())
}

fn collect_positive_u64_values(
    table: ShipCatalogTableKey,
    record_id: u64,
    value: Option<&Value>,
    field: &'static str,
    output: &mut Vec<u64>,
) -> Result<(), ShipCatalogMappingError> {
    let Some(value) = value else {
        return Ok(());
    };
    let values = value_array(table, record_id, field, value)?;
    for value in values {
        let value = positive_value_u64(table, record_id, field, value)?;
        output.push(value);
    }
    Ok(())
}

fn object(
    table: ShipCatalogTableKey,
    record: &ShipCatalogRecord,
) -> Result<&Map<String, Value>, ShipCatalogMappingError> {
    record
        .raw
        .as_object()
        .ok_or_else(|| ShipCatalogMappingError::FieldInvalid {
            table,
            record_id: record.id,
            field: "raw",
            message: "必须是对象".to_owned(),
        })
}

fn required_array<'a>(
    table: ShipCatalogTableKey,
    record_id: u64,
    raw: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a [Value], ShipCatalogMappingError> {
    let value = raw
        .get(field)
        .ok_or(ShipCatalogMappingError::FieldMissing {
            table,
            record_id,
            field,
        })?;
    value_array(table, record_id, field, value)
}

fn required_positive_u64_array(
    table: ShipCatalogTableKey,
    record_id: u64,
    raw: &Map<String, Value>,
    field: &'static str,
) -> Result<Vec<u64>, ShipCatalogMappingError> {
    required_array(table, record_id, raw, field)?
        .iter()
        .map(|value| positive_value_u64(table, record_id, field, value))
        .collect()
}

fn value_array<'a>(
    table: ShipCatalogTableKey,
    record_id: u64,
    field: &'static str,
    value: &'a Value,
) -> Result<&'a [Value], ShipCatalogMappingError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| ShipCatalogMappingError::FieldInvalid {
            table,
            record_id,
            field,
            message: "必须是数组".to_owned(),
        })
}

fn required_u64(
    table: ShipCatalogTableKey,
    record_id: u64,
    raw: &Map<String, Value>,
    field: &'static str,
) -> Result<u64, ShipCatalogMappingError> {
    let value = required_nonnegative_u64(table, record_id, raw, field)?;
    if value == 0 {
        invalid_field(table, record_id, field, "必须是正整数")
    } else {
        Ok(value)
    }
}

fn required_nonnegative_u64(
    table: ShipCatalogTableKey,
    record_id: u64,
    raw: &Map<String, Value>,
    field: &'static str,
) -> Result<u64, ShipCatalogMappingError> {
    raw.get(field)
        .ok_or(ShipCatalogMappingError::FieldMissing {
            table,
            record_id,
            field,
        })?
        .as_u64()
        .ok_or_else(|| ShipCatalogMappingError::FieldInvalid {
            table,
            record_id,
            field,
            message: "必须是非负整数".to_owned(),
        })
}

fn positive_value_u64(
    table: ShipCatalogTableKey,
    record_id: u64,
    field: &'static str,
    value: &Value,
) -> Result<u64, ShipCatalogMappingError> {
    let value = value
        .as_u64()
        .ok_or_else(|| ShipCatalogMappingError::FieldInvalid {
            table,
            record_id,
            field,
            message: "数组项必须是正整数".to_owned(),
        })?;
    if value == 0 {
        invalid_field(table, record_id, field, "数组项必须是正整数")
    } else {
        Ok(value)
    }
}

fn required_string<'a>(
    table: ShipCatalogTableKey,
    record_id: u64,
    raw: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, ShipCatalogMappingError> {
    let value = optional_string(raw, field)?.ok_or(ShipCatalogMappingError::FieldMissing {
        table,
        record_id,
        field,
    })?;
    if value.is_empty() {
        invalid_field(table, record_id, field, "不得为空")
    } else {
        Ok(value)
    }
}

fn optional_string<'a>(
    raw: &'a Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'a str>, ShipCatalogMappingError> {
    raw.get(field)
        .map(|value| {
            value
                .as_str()
                .ok_or(ShipCatalogMappingError::StringFieldInvalid { field })
        })
        .transpose()
}

fn raw_ref(table: ShipCatalogTableKey, record_id: u64) -> String {
    format!("{}:{record_id}", table.as_str())
}

fn invalid_field<T>(
    table: ShipCatalogTableKey,
    record_id: u64,
    field: &'static str,
    message: impl Into<String>,
) -> Result<T, ShipCatalogMappingError> {
    Err(ShipCatalogMappingError::FieldInvalid {
        table,
        record_id,
        field,
        message: message.into(),
    })
}

/// 静态关系闭合、字段解释或规范编码失败。
#[derive(Debug, Error)]
pub enum ShipCatalogMappingError {
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
    #[error("舰船静态目录模块摘要不一致: expected={expected}, actual={actual}")]
    ModuleMismatch { expected: String, actual: String },
    #[error("舰船静态目录缺少固定表 {}", .table.as_str())]
    TableMissing { table: ShipCatalogTableKey },
    #[error("舰船静态目录重复包含固定表 {}", .table.as_str())]
    TableDuplicate { table: ShipCatalogTableKey },
    #[error("舰船静态目录固定表数量应为 {expected}，实际为 {actual}")]
    TableCount { expected: usize, actual: usize },
    #[error("舰船组 group_id={group_id} 重复")]
    DuplicateGroup { group_id: u64 },
    #[error("{table:?}:{record_id} 引用不存在的 group_id={group_id}")]
    GroupMissing {
        table: ShipCatalogTableKey,
        record_id: u64,
        group_id: u64,
    },
    #[error("舰船组 group_id={group_id} 没有模板配置")]
    GroupWithoutTemplate { group_id: u64 },
    #[error("舰船组 group_id={group_id} 的代表配置 ID 计算溢出")]
    RepresentativeOverflow { group_id: u64 },
    #[error("舰船组 group_id={group_id} 缺少固定代表配置 config_id={config_id}")]
    RepresentativeMissing { group_id: u64, config_id: u64 },
    #[error("{table:?}:{record_id} 缺少字段 {field}")]
    FieldMissing {
        table: ShipCatalogTableKey,
        record_id: u64,
        field: &'static str,
    },
    #[error("{table:?}:{record_id}.{field} 无效: {message}")]
    FieldInvalid {
        table: ShipCatalogTableKey,
        record_id: u64,
        field: &'static str,
        message: String,
    },
    #[error("字符串字段 {field} 必须是字符串")]
    StringFieldInvalid { field: &'static str },
    #[error("{source_table:?}:{source_id}.{field} 引用缺失的 {target_table:?}:{target_id}")]
    RelatedRecordMissing {
        source_table: ShipCatalogTableKey,
        source_id: u64,
        field: &'static str,
        target_table: ShipCatalogTableKey,
        target_id: u64,
    },
    #[error("技能效果领域映射失败: {0}")]
    SkillEffect(String),
    #[error("舰船静态目录摘要编码失败: {0}")]
    Digest(#[from] serde_json::Error),
    #[error("原始记录 {key:?} 编码失败: {source}")]
    EncodeRaw {
        key: RawRecordKey,
        #[source]
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use serde::Deserialize;

    use super::{CatalogTables, collect_static_skill_queries, map_group_sources};
    use crate::adapters::device::reading::ship_catalog::{ShipCatalogReadResult, ShipCatalogTable};
    use crate::adapters::device::runtime::{ShipCatalogRecord, ShipCatalogTableKey};

    #[derive(Deserialize)]
    struct Capture {
        module_sha256: String,
        content_sha256: String,
        catalog: Catalog,
    }

    #[derive(Deserialize)]
    struct Catalog {
        tables: Vec<CapturedTable>,
    }

    #[derive(Deserialize)]
    struct CapturedTable {
        table_key: ShipCatalogTableKey,
        records: Vec<ShipCatalogRecord>,
    }

    #[test]
    #[ignore = "需要显式 AZUR_LANE_SHIP_CATALOG_CAPTURE 私人证据路径"]
    fn live_capture_closes_group_relations_and_static_skill_queries() {
        let path = PathBuf::from(
            std::env::var_os("AZUR_LANE_SHIP_CATALOG_CAPTURE")
                .expect("必须显式提供 AZUR_LANE_SHIP_CATALOG_CAPTURE"),
        );
        let capture: Capture = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let result = ShipCatalogReadResult::from_capture(
            capture.module_sha256,
            capture.content_sha256,
            capture
                .catalog
                .tables
                .into_iter()
                .map(|table| ShipCatalogTable::from_capture(table.table_key, table.records))
                .collect(),
        );

        let tables = CatalogTables::new(&result).unwrap();
        let groups = map_group_sources(&tables).unwrap();
        let queries = collect_static_skill_queries(&result).unwrap();

        let source_table = |key| {
            result
                .tables()
                .iter()
                .find(|table| table.table_key() == key)
                .unwrap()
                .records()
        };
        let source_groups = source_table(ShipCatalogTableKey::ShipDataGroup);
        let group_by_id = groups
            .iter()
            .map(|group| (group.group_id, group))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(group_by_id.len(), groups.len());
        assert_eq!(groups.len(), source_groups.len());
        let all_refs = result
            .tables()
            .iter()
            .flat_map(|table| {
                table
                    .records()
                    .iter()
                    .map(move |record| format!("{}:{}", table.table_key().as_str(), record.id))
            })
            .collect::<BTreeSet<_>>();
        for record in source_groups {
            let group_id = record.raw["group_type"].as_u64().unwrap();
            let group = group_by_id[&group_id];
            assert!(
                group
                    .relationship_raw_refs
                    .contains(&format!("ship_data_group:{}", record.id))
            );
            assert!(
                group
                    .variant_config_ids
                    .contains(&group.representative_config_id)
            );
            assert_eq!(
                group
                    .relationship_raw_refs
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len(),
                group.relationship_raw_refs.len()
            );
            for reference in &group.relationship_raw_refs {
                assert!(all_refs.contains(reference), "{group_id}: {reference}");
            }
        }
        let templates = source_table(ShipCatalogTableKey::ShipDataTemplate);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.variant_config_ids.len())
                .sum::<usize>(),
            templates.len()
        );
        let mut mapped_variants = BTreeSet::new();
        for group in &groups {
            for id in &group.variant_config_ids {
                assert!(mapped_variants.insert(*id), "重复舰船配置 {id}");
            }
        }
        for record in templates {
            let group = group_by_id[&record.raw["group_type"].as_u64().unwrap()];
            assert!(group.variant_config_ids.contains(&record.id));
            for table in ["ship_data_template", "ship_data_statistics"] {
                assert!(
                    group
                        .relationship_raw_refs
                        .contains(&format!("{table}:{}", record.id))
                );
            }
        }

        // 查询必须恰好覆盖已关联且有模板的技能，每个等级一次。
        let skill_ids = groups
            .iter()
            .flat_map(|group| group.skill_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        let skill_templates = source_table(ShipCatalogTableKey::SkillDataTemplate);
        let expected_skills = skill_templates
            .iter()
            .filter(|record| skill_ids.contains(&record.id))
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            queries
                .iter()
                .map(|query| query.skill_id)
                .collect::<BTreeSet<_>>(),
            expected_skills
        );
        assert!(
            queries
                .windows(2)
                .all(|pair| (pair[0].skill_id, pair[0].level) < (pair[1].skill_id, pair[1].level))
        );
        for record in skill_templates
            .iter()
            .filter(|record| expected_skills.contains(&record.id))
        {
            let levels = queries
                .iter()
                .filter(|query| query.skill_id == record.id)
                .map(|query| u64::from(query.level))
                .collect::<Vec<_>>();
            let maximum = record.raw["max_level"].as_u64().unwrap().max(1);
            assert_eq!(levels.len() as u64, maximum, "技能 {}", record.id);
            assert_eq!(levels.first(), Some(&1));
            assert_eq!(levels.last(), Some(&maximum));
        }
        println!(
            "live ship catalog: groups={}, skill_queries={}",
            groups.len(),
            queries.len()
        );
    }
}

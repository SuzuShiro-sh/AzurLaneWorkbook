//! 按查询领域选择已有 RPC，静态对象与动态账号状态分别读取。
use super::{RuntimeProbeError, RuntimeSession, RuntimeSessionState, query_error};
use crate::adapters::device::reading::collections::{PageState, read_restartable_pages};
use crate::adapters::device::runtime::*;
use crate::application::{GameQuery, GameQueryKind as Kind, GameQueryReport};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

impl RuntimeSession {
    pub(super) fn query_objects(
        &mut self,
        query: &GameQuery,
    ) -> Result<GameQueryReport, RuntimeProbeError> {
        let RuntimeSessionState::Active(connection) = &mut self.state else {
            return Err(query_error("读取会话已关闭"));
        };
        let mut reader = Reader {
            client: &mut connection.client,
            timeout: self.resources.options.timeout_ms,
            module: self.resources.profile.bootstrap().module_sha256(),
        };
        let entries = match query.kind() {
            Kind::Ships => reader.ships(query)?,
            Kind::Equipment | Kind::CatalogEquipment => reader.equipment(query)?,
            Kind::CatalogShips => reader.catalog_ships(query)?,
            Kind::CatalogSkills => {
                let keys = query.ids().iter().map(|&id| SkillEffectQuery::new(id,query.options().skill_level.max(1))).collect::<Result<Vec<_>,_>>()?;
                reader.skills(&keys)?
            }
            Kind::Items => reader.bag()?.items.into_iter().map(|item| json!({"item_id": item.item_id,"name":item.resolved_name,"quantity":item.quantity,"compose_recipe":item.compose_recipe})).collect(),
            Kind::Resources => vec![serde_json::to_value(reader.client.snapshot_resources(reader.timeout)?).map_err(query_error)?],
            Kind::Recipes => reader.recipes(query)?,
            Kind::Fleets => reader.fleets(query)?,
            Kind::Technology => {
                reader.technology(query)?
            }
        };
        query.project(entries).map_err(query_error)
    }
}

struct Reader<'a> {
    client: &'a mut AgentClient,
    timeout: u32,
    module: &'a str,
}

/// 名称解析是展示信息；记录原始阵营 ID 和逐项错误，不让剧情配置阻断整个目录。
fn catalog_ship_entry(
    r: ShipCatalogRecord,
    templates: &BTreeMap<u64, Value>,
    types: &BTreeMap<u64, Value>,
    nations: &BTreeMap<u64, RuntimeEquipmentNationName>,
) -> Value {
    let nation = r.raw["nationality"]
        .as_u64()
        .and_then(|id| nations.get(&id));
    json!({"config_id":r.id,"name":r.raw["name"],"type":r.raw["type"].as_u64().and_then(|id|types.get(&id)),"type_id":r.raw["type"],"nation":nation.and_then(|n|n.name.as_ref()),"nation_error":nation.and_then(|n|n.error.as_ref()),"nation_id":r.raw["nationality"],"rarity":r.raw["rarity"],"stats":r.raw["attrs"],"template":templates.get(&r.id),"definition":r.raw})
}

impl Reader<'_> {
    fn technology(&mut self, query: &GameQuery) -> Result<Vec<Value>, RuntimeProbeError> {
        let mut tables = BTreeMap::new();
        for table in ShipCatalogTableKey::TECHNOLOGY {
            tables.insert(
                table.as_str(),
                self.table(table)?
                    .into_iter()
                    .map(|r| (r.id, r.raw))
                    .collect::<BTreeMap<_, _>>(),
            );
        }
        let mut entries = Vec::new();
        for (&id, config) in &tables["fleet_tech_ship_template"] {
            if !query.includes(id) {
                continue;
            }
            let summaries = crate::application::ship_technology_summaries(id, true, |table, id| {
                Ok(tables.get(table).and_then(|rows| rows.get(&id)).cloned())
            })
            .map_err(query_error)?;
            entries.push(json!({"group_id":id,"summaries":summaries,"definition":config,"history":tables["collection_ship_group"].get(&id)}));
        }
        Ok(entries)
    }
    fn bag(&mut self) -> Result<SnapshotBagResult, RuntimeProbeError> {
        let bag = self.client.snapshot_bag(self.timeout, MAX_SNAPSHOT_ITEMS)?;
        if !bag.complete {
            return Err(query_error(format!(
                "背包采集不完整: {:?}",
                bag.read_errors
            )));
        }
        Ok(bag)
    }
    fn table(
        &mut self,
        table: ShipCatalogTableKey,
    ) -> Result<Vec<ShipCatalogRecord>, RuntimeProbeError> {
        let pages = read_restartable_pages(
            0,
            2,
            |cursor| {
                self.client.snapshot_ship_catalog(
                    self.timeout,
                    table,
                    cursor,
                    MAX_SHIP_CATALOG_PAGE_SIZE,
                    self.module,
                )
            },
            |p| PageState::new(p.next_index, !p.complete),
        )?;
        let mut records = Vec::new();
        for page in pages {
            if !page.complete {
                return Err(query_error(format!(
                    "图鉴表 {} 不完整: {:?}",
                    table.as_str(),
                    page.read_errors
                )));
            }
            records.extend(page.records);
        }
        Ok(records)
    }
    fn ships(&mut self, query: &GameQuery) -> Result<Vec<Value>, RuntimeProbeError> {
        let mut fields = query.required_fields();
        if fields
            .iter()
            .any(|s| matches!(s.as_str(), "locked" | "stats" | "classification"))
        {
            fields.insert("details".into());
        }
        fields.retain(|s| {
            !matches!(
                s.as_str(),
                "locked" | "stats" | "classification" | "equipped" | "warehouse_quantity"
            )
        });
        if query.options().slot.is_some() {
            fields.insert("slots".into());
        }
        let result = self.client.query_owned(
            self.timeout,
            &OwnedQuery {
                kind: OwnedQueryKind::Ships,
                ids: query.ids().to_vec(),
                fields: fields.into_iter().collect(),
            },
        )?;
        let mut entries = result.entries;
        for entry in &mut entries {
            if query.requests("locked") {
                entry["locked"] = entry["details"]["locked"].clone();
            }
            if query.requests("stats") {
                entry["stats"] = entry["details"]["effective_attributes"].clone();
            }
            if query.requests("classification") {
                entry["classification"] = entry["details"]["classification"].clone();
            }
        }
        Ok(entries)
    }
    fn equipment(&mut self, query: &GameQuery) -> Result<Vec<Value>, RuntimeProbeError> {
        let mut entries = if query.kind() == Kind::Equipment {
            let fields: Vec<_> = query
                .required_fields()
                .into_iter()
                .filter(|s| {
                    matches!(
                        s.as_str(),
                        "config_id" | "enhance_level" | "warehouse_quantity" | "equipped"
                    )
                })
                .collect();
            self.client
                .query_owned(
                    self.timeout,
                    &OwnedQuery {
                        kind: OwnedQueryKind::Equipment,
                        ids: query.ids().to_vec(),
                        fields: if fields.is_empty() {
                            vec!["config_id".into()]
                        } else {
                            fields
                        },
                    },
                )?
                .entries
        } else {
            Vec::new()
        };
        let need_static = query.kind() == Kind::CatalogEquipment
            || query.required_fields().iter().any(|s| {
                !matches!(
                    s.as_str(),
                    "config_id" | "enhance_level" | "warehouse_quantity" | "equipped"
                )
            });
        if !need_static || (query.kind() == Kind::Equipment && entries.is_empty()) {
            return Ok(entries);
        }
        let mut ids: Vec<_> = if query.kind() == Kind::Equipment {
            entries
                .iter()
                .filter_map(|e| e["config_id"].as_u64())
                .collect()
        } else {
            query.ids().to_vec()
        };
        ids.sort_unstable();
        ids.dedup();
        let mut configs = Vec::new();
        if ids.is_empty() {
            let pages = read_restartable_pages(
                0,
                2,
                |cursor| {
                    self.client.snapshot_equipment_configs(
                        self.timeout,
                        cursor,
                        MAX_EQUIPMENT_PAGE_SIZE,
                        self.module,
                    )
                },
                |p| PageState::new(p.next_index, !p.complete),
            )?;
            for p in pages {
                if !p.complete {
                    return Err(query_error(format!("装备配置不完整: {:?}", p.read_errors)));
                }
                configs.extend(p.configs);
            }
        } else {
            for batch in ids.chunks(128) {
                let result = self.client.snapshot_equipment_configs_by_ids(
                    self.timeout,
                    batch,
                    self.module,
                )?;
                if !result.complete {
                    return Err(query_error("装备配置采集不完整"));
                }
                configs.extend(result.configs);
            }
        }
        let configs: BTreeMap<_, _> = configs.into_iter().map(|c| (c.config_id, c)).collect();
        let (mut type_names, mut nation_names) = (BTreeMap::new(), BTreeMap::new());
        for (field, is_type) in [("type", true), ("nation", false)] {
            if !query.requests(field) {
                continue;
            }
            let ids: Vec<_> = configs
                .values()
                .filter_map(|c| c.raw_config[if is_type { "type" } else { "nationality" }].as_u64())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            for batch in ids.chunks(128) {
                let result = self.client.snapshot_equipment_reference_names(
                    self.timeout,
                    if is_type { batch } else { &[] },
                    if is_type { &[] } else { batch },
                    &[],
                    &[],
                    self.module,
                )?;
                if !result.complete {
                    return Err(query_error("装备分类名称采集不完整"));
                }
                for record in result.equipment_types {
                    type_names.insert(record.equipment_type_id, record.name);
                }
                for record in result.nations {
                    nation_names.insert(record.nation_id, record.name);
                }
            }
        }
        if query.kind() == Kind::CatalogEquipment {
            entries = configs.keys().map(|id| json!({"config_id":id})).collect();
        }
        for entry in &mut entries {
            let id = entry["config_id"]
                .as_u64()
                .ok_or_else(|| query_error("装备缺少配置 ID"))?;
            let c = configs
                .get(&id)
                .ok_or_else(|| query_error(format!("持有装备 {id} 缺少静态配置")))?;
            let raw = &c.raw_config;
            for (field, key) in [("name", "name"), ("rarity", "rarity")] {
                if query.requests(field) {
                    entry[field] = raw
                        .get(key)
                        .ok_or_else(|| query_error(format!("装备 {id} 缺少 {key}")))?
                        .clone();
                }
            }
            if query.requests("type") {
                entry["type"] = json!(
                    type_names.get(
                        &raw["type"]
                            .as_u64()
                            .ok_or_else(|| query_error("装备 type 无效"))?
                    )
                );
                entry["type_id"] = raw["type"].clone();
            }
            if query.requests("nation") {
                entry["nation"] = json!(
                    nation_names.get(
                        &raw["nationality"]
                            .as_u64()
                            .ok_or_else(|| query_error("装备 nationality 无效"))?
                    )
                );
                entry["nation_id"] = raw["nationality"].clone();
            }
            if query.requests("family_id") {
                entry["family_id"] = json!(c.root_config_id);
            }
            if query.requests("definition") {
                entry["definition"] = serde_json::to_value(c).map_err(query_error)?;
            }
            if query.requests("stats") {
                entry["stats"] = c.attributes.clone();
            }
            if query.kind() == Kind::CatalogEquipment && query.requests("enhance_level") {
                entry["enhance_level"] = json!(
                    crate::adapters::device::mapping::equipment::equipment_enhance_level(c)
                        .map_err(query_error)?
                );
            }
            if query.requests("weapons") {
                let mut weapons = Vec::new();
                for batch in c.weapon_ids.chunks(128) {
                    let result =
                        self.client
                            .snapshot_equipment_weapons(self.timeout, batch, self.module)?;
                    if !result.complete {
                        return Err(query_error("装备武器详情采集不完整"));
                    }
                    weapons.extend(result.weapons);
                }
                entry["weapons"] = serde_json::to_value(weapons).map_err(query_error)?;
            }
            if query.requests("skills") {
                use crate::adapters::device::mapping::equipment::parse_skill_references;
                use crate::domain::EquipmentSkillVisibility;
                let object = raw
                    .as_object()
                    .ok_or_else(|| query_error("装备配置必须为对象"))?;
                let mut keys = BTreeSet::new();
                for (field, visibility) in [
                    ("skill_id", EquipmentSkillVisibility::Visible),
                    ("hidden_skill_id", EquipmentSkillVisibility::Hidden),
                ] {
                    for r in parse_skill_references(object, field, visibility, id)
                        .map_err(query_error)?
                    {
                        keys.insert((r.skill_id(), r.level()));
                    }
                }
                let keys = keys
                    .into_iter()
                    .map(|(id, level)| SkillEffectQuery::new(id, level))
                    .collect::<Result<Vec<_>, _>>()?;
                entry["skills"] = json!(self.skills(&keys)?);
            }
        }
        Ok(entries)
    }
    fn skills(&mut self, keys: &[SkillEffectQuery]) -> Result<Vec<Value>, RuntimeProbeError> {
        let mut values = Vec::new();
        for batch in keys.chunks(MAX_SKILL_EFFECT_BATCH_SIZE) {
            let result = self
                .client
                .snapshot_skill_effects(self.timeout, batch, self.module)?;
            if !result.complete {
                return Err(query_error("技能效果采集不完整"));
            }
            for skill in result.skills {
                values.push(serde_json::to_value(skill).map_err(query_error)?);
            }
        }
        Ok(values)
    }
    fn catalog_ships(&mut self, query: &GameQuery) -> Result<Vec<Value>, RuntimeProbeError> {
        let templates: BTreeMap<_, _> = if query.requests("template") {
            self.table(ShipCatalogTableKey::ShipDataTemplate)?
                .into_iter()
                .map(|r| (r.id, r.raw))
                .collect()
        } else {
            BTreeMap::new()
        };
        let types: BTreeMap<_, _> = if query.requests("type") {
            self.table(ShipCatalogTableKey::ShipDataByType)?
                .into_iter()
                .map(|r| (r.id, r.raw["type_name"].clone()))
                .collect()
        } else {
            BTreeMap::new()
        };
        let records: Vec<_> = self
            .table(ShipCatalogTableKey::ShipDataStatistics)?
            .into_iter()
            .filter(|r| query.includes(r.id))
            .collect();
        let mut nations = BTreeMap::new();
        if query.requests("nation") || query.requests("nation_error") {
            let ids: Vec<_> = records
                .iter()
                .filter_map(|r| r.raw["nationality"].as_u64())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            for batch in ids.chunks(128) {
                let result = self.client.snapshot_equipment_reference_names(
                    self.timeout,
                    &[],
                    batch,
                    &[],
                    &[],
                    self.module,
                )?;
                for n in result.nations {
                    nations.insert(n.nation_id, n);
                }
            }
        }
        Ok(records
            .into_iter()
            .map(|r| catalog_ship_entry(r, &templates, &types, &nations))
            .collect())
    }
    fn recipes(&mut self, query: &GameQuery) -> Result<Vec<Value>, RuntimeProbeError> {
        let pages = read_restartable_pages(
            0,
            2,
            |cursor| {
                self.client.snapshot_compose_recipes(
                    self.timeout,
                    cursor,
                    MAX_EQUIPMENT_PAGE_SIZE,
                    self.module,
                )
            },
            |p| PageState::new(p.next_index, !p.complete),
        )?;
        let availability = if query.requests("max_count") {
            Some((self.bag()?, self.client.snapshot_resources(self.timeout)?))
        } else {
            None
        };
        let mut entries = Vec::new();
        for page in pages {
            if !page.complete {
                return Err(query_error(format!(
                    "合成配方不完整: {:?}",
                    page.read_errors
                )));
            }
            for r in page.recipes {
                let mut e = serde_json::to_value(&r).map_err(query_error)?;
                if let Some((bag, resources)) = &availability {
                    let item = bag.items.iter().find(|i| i.item_id == r.material_id);
                    let materials = item.map_or(0, |i| i.quantity) / r.material_count;
                    let gold = if r.gold == 0 {
                        u64::MAX
                    } else {
                        resources.gold / r.gold
                    };
                    let capacity = resources
                        .equipment_limit
                        .saturating_sub(resources.equipment_capacity);
                    let live = bag
                        .items
                        .iter()
                        .find(|i| i.item_id == r.recipe_id)
                        .and_then(|i| i.compose_recipe.as_ref());
                    if let Some(c) = live
                        && (c.recipe_id != r.recipe_id
                            || c.material_id != r.material_id
                            || c.material_count != r.material_count
                            || c.gold != r.gold
                            || c.equipment_id != Some(r.equipment_id))
                    {
                        return Err(query_error(format!(
                            "配方 {} 的实时数据与静态配置不一致",
                            r.recipe_id
                        )));
                    }
                    let client_max = live.map(|c| c.max_count.unwrap_or(u64::MAX)).unwrap_or(0);
                    e["max_count"] = json!(materials.min(gold).min(capacity).min(client_max));
                }
                entries.push(e);
            }
        }
        Ok(entries)
    }
    fn fleets(&mut self, query: &GameQuery) -> Result<Vec<Value>, RuntimeProbeError> {
        let full = query.requests("ships");
        let q = GameQuery::new(
            Kind::Ships,
            vec![],
            if full {
                vec![
                    "name".into(),
                    "level".into(),
                    "slots".into(),
                    "fleet_memberships".into(),
                ]
            } else {
                vec!["fleet_memberships".into()]
            },
        )
        .map_err(query_error)?;
        let mut fleets: BTreeMap<u64, Value> = BTreeMap::new();
        for ship in self.ships(&q)? {
            if let Some(memberships) = ship["fleet_memberships"].as_array() {
                for m in memberships {
                    let id = m["fleet_id"]
                        .as_u64()
                        .ok_or_else(|| query_error("编队缺少 ID"))?;
                    let fleet=fleets.entry(id).or_insert_with(||json!({"fleet_id":id,"name":m["display_name"],"kind":m["kind"],"ships":[]}));
                    let mut member = ship.clone();
                    member.as_object_mut().unwrap().remove("fleet_memberships");
                    member["team"] = m["team"].clone();
                    member["position"] = m["position"].clone();
                    fleet["ships"].as_array_mut().unwrap().push(member);
                }
            }
        }
        Ok(fleets.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_retains_config_and_name_error_without_blocking_other_nations() {
        let nations = BTreeMap::from([
            (
                1,
                RuntimeEquipmentNationName {
                    nation_id: 1,
                    name: Some("白鹰".into()),
                    error: None,
                },
            ),
            (
                99,
                RuntimeEquipmentNationName {
                    nation_id: 99,
                    name: None,
                    error: Some("Nation.Nation2Name 返回值无效: Lua 值不是字符串".into()),
                },
            ),
        ]);
        let entries = [(107061, 1), (900506, 99)]
            .into_iter()
            .map(|(id, nation)| {
                catalog_ship_entry(
                    ShipCatalogRecord {
                        id,
                        raw: json!({"name":"舰船","nationality":nation,"type":7,"rarity":5}),
                    },
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    &nations,
                )
            })
            .collect();
        let report = GameQuery::new(Kind::CatalogShips, vec![], vec![])
            .unwrap()
            .project(entries)
            .unwrap();
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0]["nation"], "白鹰");
        assert!(report.entries[0]["nation_error"].is_null());
        assert_eq!(report.entries[1]["nation_id"], 99);
        assert!(report.entries[1]["nation"].is_null());
        assert_eq!(
            report.entries[1]["nation_error"],
            nations[&99].error.as_deref().unwrap()
        );
    }
}

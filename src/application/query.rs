//! 不依赖工作簿的对象查询请求及稳定输出。

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GameQueryKind {
    Ships,
    Equipment,
    CatalogShips,
    CatalogEquipment,
    CatalogSkills,
    Recipes,
    Items,
    Resources,
    Fleets,
    Technology,
}

impl GameQueryKind {
    pub const fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Ships => &[
                "ship_id",
                "config_id",
                "name",
                "level",
                "experience_in_level",
                "intimacy_raw",
                "energy",
                "proficiency",
                "fleet_memberships",
                "skills",
                "slots",
                "details",
                "locked",
                "stats",
                "classification",
            ],
            Self::Equipment => &[
                "config_id",
                "name",
                "enhance_level",
                "warehouse_quantity",
                "equipped",
                "definition",
                "family_id",
                "type",
                "nation",
                "rarity",
                "stats",
                "weapons",
                "skills",
            ],
            Self::CatalogShips => &[
                "config_id",
                "name",
                "type",
                "nation",
                "nation_id",
                "nation_error",
                "rarity",
                "definition",
                "template",
                "stats",
            ],
            Self::CatalogEquipment => &[
                "config_id",
                "name",
                "family_id",
                "type",
                "nation",
                "rarity",
                "enhance_level",
                "definition",
                "stats",
                "weapons",
                "skills",
            ],
            Self::CatalogSkills => &[
                "skill_id",
                "level",
                "display",
                "battle_skill",
                "battle_buff",
                "complete",
            ],
            Self::Recipes => &[
                "recipe_id",
                "material_id",
                "material_count",
                "gold",
                "equipment_id",
                "max_count",
            ],
            Self::Items => &["item_id", "name", "quantity", "compose_recipe"],
            Self::Resources => &["gold", "equipment_capacity", "equipment_limit"],
            Self::Fleets => &["fleet_id", "name", "kind", "ships"],
            Self::Technology => &["group_id", "summaries", "definition", "history"],
        }
    }

    pub const fn default_fields(self) -> &'static [&'static str] {
        match self {
            Self::Ships => &["ship_id", "config_id", "name", "level"],
            Self::Equipment => &[
                "config_id",
                "name",
                "enhance_level",
                "warehouse_quantity",
                "equipped",
            ],
            Self::CatalogShips => &[
                "config_id",
                "name",
                "type",
                "nation",
                "nation_id",
                "nation_error",
                "rarity",
            ],
            Self::CatalogEquipment => &[
                "config_id",
                "name",
                "family_id",
                "type",
                "rarity",
                "enhance_level",
            ],
            Self::Recipes => &[
                "recipe_id",
                "material_id",
                "material_count",
                "gold",
                "equipment_id",
            ],
            Self::Items => &["item_id", "name", "quantity"],
            _ => self.fields(),
        }
    }

    pub const fn identity_field(self) -> &'static str {
        match self {
            Self::Ships => "ship_id",
            Self::Equipment => "config_id",
            Self::CatalogShips | Self::CatalogEquipment => "config_id",
            Self::CatalogSkills => "skill_id",
            Self::Recipes => "recipe_id",
            Self::Items => "item_id",
            Self::Fleets => "fleet_id",
            Self::Resources => "",
            Self::Technology => "group_id",
        }
    }
}

/// 舰船按实例 ID 选择；装备按配置 ID 选择并返回仓库数量及所有装载位置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameQuery {
    kind: GameQueryKind,
    ids: Vec<u64>,
    fields: Vec<String>,
    options: GameQueryOptions,
}

/// 查询条件只影响返回集合；执行操作另行读取并核验真实状态。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GameQueryOptions {
    pub name: Option<String>,
    pub object_type: Option<String>,
    pub nation: Option<String>,
    pub rarity: Option<u64>,
    pub level_min: Option<u64>,
    pub level_max: Option<u64>,
    pub locked: Option<bool>,
    pub fleet: Option<u64>,
    pub ship: Option<u64>,
    pub slot: Option<u64>,
    pub family: Option<u64>,
    pub location: Option<String>,
    pub equipment: Option<u64>,
    pub ship_type: Option<u64>,
    pub available: bool,
    pub available_only: bool,
    pub skill_level: u32,
    pub sort: Option<String>,
    pub descending: bool,
    pub limit: Option<usize>,
    pub offset: usize,
}

impl GameQuery {
    pub fn new(
        kind: GameQueryKind,
        mut ids: Vec<u64>,
        fields: Vec<String>,
    ) -> Result<Self, String> {
        if ids.contains(&0) {
            return Err("查询 ID 必须大于零".to_owned());
        }
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("查询 ID 不得重复".to_owned());
        }
        let fields = if fields.is_empty() {
            kind.default_fields()
                .iter()
                .map(|field| (*field).to_owned())
                .collect()
        } else {
            fields
        };
        let mut seen = BTreeSet::new();
        for field in &fields {
            if !kind
                .fields()
                .contains(&field.split('.').next().unwrap_or_default())
                || field.split('.').any(str::is_empty)
            {
                return Err(format!(
                    "不支持的查询字段 {field}；可用字段: {}",
                    kind.fields().join(",")
                ));
            }
            if !seen.insert(field) {
                return Err(format!("查询字段重复: {field}"));
            }
        }
        if fields.iter().any(|a| {
            fields
                .iter()
                .any(|b| a != b && b.starts_with(&format!("{a}.")))
        }) {
            return Err("不能同时选择完整字段及其子字段".into());
        }
        Ok(Self {
            kind,
            ids,
            fields,
            options: GameQueryOptions::default(),
        })
    }

    pub fn with_options(mut self, options: GameQueryOptions) -> Result<Self, String> {
        if matches!((options.level_min, options.level_max), (Some(a), Some(b)) if a > b) {
            return Err("最低等级不能大于最高等级".into());
        }
        if options.slot.is_some_and(|slot| !(1..=5).contains(&slot)) {
            return Err("槽位必须为 1 至 5".into());
        }
        if let Some(sort) = &options.sort
            && (!self
                .kind
                .fields()
                .contains(&sort.split('.').next().unwrap_or_default())
                || sort.split('.').any(str::is_empty))
        {
            return Err(format!("不支持的排序字段: {sort}"));
        }
        self.options = options;
        Ok(self)
    }
    pub fn options(&self) -> &GameQueryOptions {
        &self.options
    }
    /// 采集字段包括筛选和排序依赖，输出仍只保留显式请求字段。
    pub(crate) fn required_fields(&self) -> BTreeSet<String> {
        let mut fields: BTreeSet<_> = self
            .fields
            .iter()
            .map(|s| s.split('.').next().unwrap().to_owned())
            .collect();
        let o = &self.options;
        if o.name.is_some() {
            fields.insert("name".into());
        }
        if o.object_type.is_some() || o.nation.is_some() || o.rarity.is_some() {
            if self.kind == GameQueryKind::Ships {
                fields.insert("classification".into());
            } else {
                if o.object_type.is_some() {
                    fields.insert("type".into());
                }
                if o.nation.is_some() {
                    fields.insert("nation".into());
                }
                if o.rarity.is_some() {
                    fields.insert("rarity".into());
                }
            }
        }
        if o.level_min.is_some() || o.level_max.is_some() {
            fields.insert(
                if self.kind == GameQueryKind::Ships {
                    "level"
                } else {
                    "enhance_level"
                }
                .into(),
            );
        }
        if o.locked.is_some() {
            fields.insert("locked".into());
        }
        if o.fleet.is_some() {
            fields.insert("fleet_memberships".into());
        }
        if o.ship.is_some() || o.location.is_some() || o.slot.is_some() {
            if self.kind == GameQueryKind::Ships {
                fields.insert("slots".into());
            } else {
                fields.extend(["equipped", "warehouse_quantity"].map(str::to_owned));
            }
        }
        if o.family.is_some() {
            fields.insert("family_id".into());
        }
        if o.available || o.available_only {
            fields.insert("max_count".into());
        }
        if o.ship_type.is_some() {
            fields.insert("definition".into());
        }
        if let Some(sort) = &o.sort {
            fields.insert(sort.split('.').next().unwrap().into());
        }
        fields
    }

    pub const fn kind(&self) -> GameQueryKind {
        self.kind
    }
    pub fn ids(&self) -> &[u64] {
        &self.ids
    }
    pub fn fields(&self) -> &[String] {
        &self.fields
    }
    pub fn requests(&self, field: &str) -> bool {
        self.required_fields().contains(field)
    }
    pub fn includes(&self, id: u64) -> bool {
        self.ids.is_empty() || self.ids.binary_search(&id).is_ok()
    }

    /// 标识字段始终保留，避免批量指定字段时无法识别结果归属。
    pub(crate) fn project(&self, mut entries: Vec<Value>) -> Result<GameQueryReport, String> {
        let identity = self.kind.identity_field();
        entries.retain(|entry| {
            self.ids.is_empty() || entry[identity].as_u64().is_some_and(|id| self.includes(id))
        });
        entries.sort_by_key(|entry| entry[identity].as_u64());
        let found: BTreeSet<u64> = entries
            .iter()
            .filter_map(|entry| entry[identity].as_u64())
            .collect();
        let missing_ids = self
            .ids
            .iter()
            .copied()
            .filter(|id| !found.contains(id))
            .collect();
        entries.retain(|entry| self.matches(entry));
        if let Some(sort) = &self.options.sort {
            if entries
                .iter()
                .any(|entry| field_value(entry, sort).is_none())
            {
                return Err(format!("排序字段不存在: {sort}"));
            }
            entries.sort_by(|a, b| {
                compare_value(field_value(a, sort).unwrap(), field_value(b, sort).unwrap())
            });
        }
        if self.options.descending {
            entries.reverse();
        }
        entries = entries
            .into_iter()
            .skip(self.options.offset)
            .take(self.options.limit.unwrap_or(usize::MAX))
            .collect();
        for entry in &mut entries {
            if self.kind == GameQueryKind::Ships
                && let Some(slot) = self.options.slot
                && let Some(slots) = entry["slots"].as_array_mut()
            {
                slots.retain(|s| s["slot_index"].as_u64() == Some(slot));
            }
            let mut selected = serde_json::Map::new();
            if let Some(id) = entry.get(identity) {
                selected.insert(identity.into(), id.clone());
            }
            for field in &self.fields {
                select_path(entry, &mut selected, &field.split('.').collect::<Vec<_>>())?;
            }
            *entry = Value::Object(selected);
        }
        Ok(GameQueryReport {
            schema_version: 1,
            kind: self.kind,
            entries,
            missing_ids,
        })
    }

    fn matches(&self, e: &Value) -> bool {
        let o = &self.options;
        let level = e[if self.kind == GameQueryKind::Ships {
            "level"
        } else {
            "enhance_level"
        }]
        .as_u64();
        let classification = &e["classification"];
        let type_value = if self.kind == GameQueryKind::Ships {
            &classification["ship_type_name"]
        } else {
            &e["type"]
        };
        let nation_value = if self.kind == GameQueryKind::Ships {
            &classification["nation_name"]
        } else {
            &e["nation"]
        };
        let rarity = if self.kind == GameQueryKind::Ships {
            classification["rarity"].as_u64()
        } else {
            e["rarity"].as_u64()
        };
        let equipped = e["equipped"].as_array();
        o.name
            .as_ref()
            .is_none_or(|s| e["name"].as_str().is_some_and(|name| name.contains(s)))
            && o.object_type.as_ref().is_none_or(|s| {
                text_matches(type_value, s)
                    || text_matches(&classification["ship_type_id"], s)
                    || text_matches(&e["type_id"], s)
            })
            && o.nation.as_ref().is_none_or(|s| {
                text_matches(nation_value, s)
                    || text_matches(&classification["nation_id"], s)
                    || text_matches(&e["nation_id"], s)
            })
            && o.rarity.is_none_or(|n| rarity == Some(n))
            && o.level_min.is_none_or(|n| level.is_some_and(|v| v >= n))
            && o.level_max.is_none_or(|n| level.is_some_and(|v| v <= n))
            && o.locked.is_none_or(|b| e["locked"].as_bool() == Some(b))
            && o.fleet.is_none_or(|id| {
                e["fleet_memberships"]
                    .as_array()
                    .is_some_and(|m| m.iter().any(|v| v["fleet_id"].as_u64() == Some(id)))
            })
            && o.ship.is_none_or(|id| {
                equipped.is_some_and(|xs| {
                    xs.iter().any(|x| {
                        x["ship_id"].as_u64() == Some(id)
                            && o.slot
                                .is_none_or(|slot| x["slot_index"].as_u64() == Some(slot))
                    })
                })
            })
            && o.family
                .is_none_or(|id| e["family_id"].as_u64() == Some(id))
            && o.equipment
                .is_none_or(|id| e["equipment_id"].as_u64() == Some(id))
            && o.location.as_ref().is_none_or(|s| {
                if s == "warehouse" {
                    e["warehouse_quantity"].as_u64().is_some_and(|n| n > 0)
                } else {
                    equipped.is_some_and(|xs| !xs.is_empty())
                }
            })
            && (!o.available_only || e["max_count"].as_u64().is_some_and(|n| n > 0))
            && o.ship_type.is_none_or(|id| {
                ["add_get_shiptype", "add_level_shiptype"]
                    .iter()
                    .any(|key| {
                        e["definition"][key]
                            .as_array()
                            .is_some_and(|types| types.iter().any(|t| t.as_u64() == Some(id)))
                    })
            })
    }
}

fn text_matches(value: &Value, expected: &str) -> bool {
    value.as_str() == Some(expected) || value.as_u64().is_some_and(|n| n.to_string() == expected)
}
fn field_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, key| v.get(key))
}
fn compare_value(a: &Value, b: &Value) -> std::cmp::Ordering {
    if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
        return a.cmp(&b);
    }
    if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
        return a.cmp(&b);
    }
    if let (Some(a), Some(b)) = (a.as_str(), b.as_str()) {
        return a.cmp(b);
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        _ => a.to_string().cmp(&b.to_string()),
    }
}
fn select_path(
    value: &Value,
    output: &mut serde_json::Map<String, Value>,
    path: &[&str],
) -> Result<(), String> {
    let key = path[0];
    let source = value
        .get(key)
        .ok_or_else(|| format!("查询结果中没有字段 {}", path.join(".")))?;
    if path.len() == 1 {
        output.insert(key.into(), source.clone());
        return Ok(());
    }
    let target = output.entry(key).or_insert_with(|| {
        if source.is_array() {
            Value::Array(Vec::new())
        } else {
            serde_json::json!({})
        }
    });
    match source {
        Value::Null => *target = Value::Null,
        Value::Array(items) => {
            let result = target.as_array_mut().ok_or("字段选择存在结构冲突")?;
            if result.is_empty() {
                result.resize_with(items.len(), || serde_json::json!({}));
            }
            for (item, dst) in items.iter().zip(result) {
                select_path(
                    item,
                    dst.as_object_mut().ok_or("嵌套字段必须属于对象")?,
                    &path[1..],
                )?;
            }
        }
        Value::Object(_) => select_path(
            source,
            target.as_object_mut().ok_or("字段选择存在结构冲突")?,
            &path[1..],
        )?,
        _ => return Err(format!("字段 {key} 没有子字段")),
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct GameQueryReport {
    pub schema_version: u32,
    pub kind: GameQueryKind,
    pub entries: Vec<Value>,
    pub missing_ids: Vec<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projection_preserves_identity_and_reports_missing_objects() {
        let query = GameQuery::new(GameQueryKind::Ships, vec![9, 2], vec!["level".into()]).unwrap();
        let report = query
            .project(vec![
                json!({"ship_id": 2, "level": 100, "energy": 1}),
                json!({"ship_id": 3, "level": 90}),
            ])
            .unwrap();
        assert_eq!(report.entries, vec![json!({"ship_id": 2, "level": 100})]);
        assert_eq!(report.missing_ids, vec![9]);
        assert!(GameQuery::new(GameQueryKind::Ships, vec![0], vec![]).is_err());
        assert!(GameQuery::new(GameQueryKind::Ships, vec![1, 1], vec![]).is_err());
        assert!(GameQuery::new(GameQueryKind::Ships, vec![], vec!["typo".into()]).is_err());
    }

    #[test]
    fn nested_array_projection_merges_fields_without_leaking_dependencies() {
        let query = GameQuery::new(
            GameQueryKind::Ships,
            vec![],
            vec![
                "slots.slot_index".into(),
                "slots.equipment.config_id".into(),
            ],
        )
        .unwrap()
        .with_options(GameQueryOptions {
            locked: Some(true),
            slot: Some(2),
            ..Default::default()
        })
        .unwrap();
        let report = query.project(vec![json!({"ship_id":8,"locked":true,"slots":[{"slot_index":1,"equipment":{"config_id":100},"secret":9},{"slot_index":2,"equipment":{"config_id":200},"secret":9}]}),json!({"ship_id":9,"locked":false,"slots":[]})]).unwrap();
        assert_eq!(
            report.entries,
            vec![json!({"ship_id":8,"slots":[{"slot_index":2,"equipment":{"config_id":200}}]})]
        );
    }

    #[test]
    fn filtering_and_pagination_do_not_mark_existing_ids_as_missing() {
        let query = GameQuery::new(GameQueryKind::Ships, vec![1, 2, 3, 4], vec!["level".into()])
            .unwrap()
            .with_options(GameQueryOptions {
                level_min: Some(10),
                sort: Some("level".into()),
                descending: true,
                limit: Some(1),
                offset: 1,
                ..Default::default()
            })
            .unwrap();
        let result = query
            .project(vec![
                json!({"ship_id":1,"level":9}),
                json!({"ship_id":2,"level":10}),
                json!({"ship_id":3,"level":20}),
            ])
            .unwrap();
        assert_eq!(result.entries, vec![json!({"ship_id":2,"level":10})]);
        assert_eq!(result.missing_ids, vec![4]);
    }

    #[test]
    fn sorting_keeps_large_integer_precision_and_checks_field_structure() {
        assert!(compare_value(&json!(9007199254740992u64), &json!(9007199254740993u64)).is_lt());
        assert!(
            GameQuery::new(
                GameQueryKind::Ships,
                vec![],
                vec!["slots".into(), "slots.slot_index".into()]
            )
            .is_err()
        );
        let q = GameQuery::new(GameQueryKind::Ships, vec![], vec!["level".into()]).unwrap();
        assert!(
            q.clone()
                .with_options(GameQueryOptions {
                    sort: Some("details..name".into()),
                    ..Default::default()
                })
                .is_err()
        );
        assert!(
            q.with_options(GameQueryOptions {
                sort: Some("details.unknown".into()),
                ..Default::default()
            })
            .unwrap()
            .project(vec![json!({"ship_id":1,"level":1,"details":{}})])
            .is_err()
        );
    }
}

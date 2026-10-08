//! 持有对象局部查询的严格字段选择与响应校验。

use super::{
    MAX_EQUIPMENT_WEAPON_BATCH_SIZE, MAX_SHIP_FLEET_MEMBERSHIPS, MAX_SHIP_SKILLS,
    MAX_SNAPSHOT_ITEMS, RuntimeFleetMembership, RuntimeProtocolError, RuntimeShipDetail,
    RuntimeShipSkill, RuntimeShipSlot, SHIP_EQUIPMENT_SLOT_COUNT, validate_positive_lua_integer,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnedQueryKind {
    Ships,
    Equipment,
}

/// 舰船使用实例 ID，装备使用配置 ID；空 ids 表示全部，空 fields 使用默认字段。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedQuery {
    pub kind: OwnedQueryKind,
    pub ids: Vec<u64>,
    pub fields: Vec<String>,
}

impl OwnedQuery {
    pub fn validate(&self) -> Result<(), RuntimeProtocolError> {
        let fail = || {
            RuntimeProtocolError::new("owned_query_invalid", "查询 ID 或字段无效、重复或超过容量")
        };
        if self.ids.len() > MAX_EQUIPMENT_WEAPON_BATCH_SIZE {
            return Err(fail());
        }
        let mut ids = HashSet::new();
        for &id in &self.ids {
            validate_positive_lua_integer("query_owned.ids", id)?;
            if !ids.insert(id) {
                return Err(fail());
            }
        }
        let allowed: &[&str] = match self.kind {
            OwnedQueryKind::Ships => &[
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
            ],
            OwnedQueryKind::Equipment => &[
                "config_id",
                "enhance_level",
                "warehouse_quantity",
                "equipped",
            ],
        };
        let mut fields = HashSet::new();
        for field in &self.fields {
            if !allowed.contains(&field.as_str()) || !fields.insert(field) {
                return Err(fail());
            }
        }
        Ok(())
    }

    pub fn selected_fields(&self) -> Vec<&str> {
        let mut fields: Vec<&str> = if self.fields.is_empty() {
            match self.kind {
                OwnedQueryKind::Ships => vec!["ship_id", "config_id", "name", "level"],
                OwnedQueryKind::Equipment => vec![
                    "config_id",
                    "enhance_level",
                    "warehouse_quantity",
                    "equipped",
                ],
            }
        } else {
            self.fields.iter().map(String::as_str).collect()
        };
        if !fields.contains(&"config_id") {
            fields.push("config_id");
        }
        if self.kind == OwnedQueryKind::Ships && !fields.contains(&"ship_id") {
            fields.push("ship_id");
        }
        fields
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedQueryResult {
    pub entries: Vec<Value>,
    pub missing_ids: Vec<u64>,
}

impl OwnedQueryResult {
    pub fn validate(&self, query: &OwnedQuery) -> Result<(), RuntimeProtocolError> {
        let fail = || {
            RuntimeProtocolError::new(
                "owned_query_result_invalid",
                "查询响应字段、身份或缺失 ID 集合不符合请求",
            )
        };
        query.validate()?;
        if self.entries.len() > MAX_SNAPSHOT_ITEMS as usize {
            return Err(fail());
        }
        let selected = query.selected_fields();
        let identity = if query.kind == OwnedQueryKind::Ships {
            "ship_id"
        } else {
            "config_id"
        };
        let mut ids = HashSet::new();
        for entry in &self.entries {
            let object = entry.as_object().ok_or_else(fail)?;
            if object.len() != selected.len()
                || selected.iter().any(|key| !object.contains_key(*key))
            {
                return Err(fail());
            }
            let id = object[identity].as_u64().ok_or_else(fail)?;
            validate_positive_lua_integer(identity, id)?;
            let config = object["config_id"].as_u64().ok_or_else(fail)?;
            validate_positive_lua_integer("config_id", config)?;
            if !ids.insert(id) || (!query.ids.is_empty() && !query.ids.contains(&id)) {
                return Err(fail());
            }
            for (key, value) in object {
                match key.as_str() {
                    "ship_id" | "config_id" => {}
                    "name"
                        if value
                            .as_str()
                            .is_some_and(|name| !name.is_empty() && name.len() <= 512) => {}
                    "level"
                        if value
                            .as_u64()
                            .is_some_and(|n| n > 0 && n <= u32::MAX as u64) => {}
                    "experience_in_level"
                    | "intimacy_raw"
                    | "energy"
                    | "proficiency"
                    | "warehouse_quantity"
                        if value.as_u64().is_some() => {}
                    "enhance_level" if value.as_u64().is_some_and(|n| n <= u32::MAX as u64) => {}
                    "skills" => {
                        let skills: Vec<RuntimeShipSkill> =
                            serde_json::from_value(value.clone()).map_err(|_| fail())?;
                        if skills.len() > MAX_SHIP_SKILLS {
                            return Err(fail());
                        }
                        let mut previous = 0;
                        for skill in skills {
                            skill.validate()?;
                            if skill.skill_id <= previous {
                                return Err(fail());
                            }
                            previous = skill.skill_id;
                        }
                    }
                    "slots" => {
                        let slots: Vec<RuntimeShipSlot> =
                            serde_json::from_value(value.clone()).map_err(|_| fail())?;
                        if slots.len() != SHIP_EQUIPMENT_SLOT_COUNT {
                            return Err(fail());
                        }
                        for (index, slot) in slots.iter().enumerate() {
                            if slot.slot_index as usize != index + 1 {
                                return Err(fail());
                            }
                            if let Some(equipment) = &slot.equipment {
                                equipment.validate()?;
                            }
                        }
                    }
                    "fleet_memberships" => {
                        let memberships: Vec<RuntimeFleetMembership> =
                            serde_json::from_value(value.clone()).map_err(|_| fail())?;
                        if memberships.len() > MAX_SHIP_FLEET_MEMBERSHIPS {
                            return Err(fail());
                        }
                        for membership in memberships {
                            membership.validate()?;
                        }
                    }
                    "equipped" => {
                        #[derive(Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct Location {
                            ship_id: u64,
                            slot_index: u32,
                            equipment_id: u64,
                        }
                        let locations: Vec<Location> =
                            serde_json::from_value(value.clone()).map_err(|_| fail())?;
                        if locations.len() > MAX_SNAPSHOT_ITEMS as usize * SHIP_EQUIPMENT_SLOT_COUNT
                        {
                            return Err(fail());
                        }
                        let mut slots = HashSet::new();
                        for location in locations {
                            validate_positive_lua_integer("equipped.ship_id", location.ship_id)?;
                            validate_positive_lua_integer(
                                "equipped.equipment_id",
                                location.equipment_id,
                            )?;
                            if !(1..=SHIP_EQUIPMENT_SLOT_COUNT as u32)
                                .contains(&location.slot_index)
                                || !slots.insert((location.ship_id, location.slot_index))
                            {
                                return Err(fail());
                            }
                        }
                    }
                    "details" => {
                        let details: RuntimeShipDetail =
                            serde_json::from_value(value.clone()).map_err(|_| fail())?;
                        details.validate()?;
                        if details.ship_id != id || details.config_id != config {
                            return Err(fail());
                        }
                    }
                    _ => return Err(fail()),
                }
            }
        }
        for &id in &self.missing_ids {
            if !query.ids.contains(&id) || !ids.insert(id) {
                return Err(fail());
            }
        }
        if !query.ids.is_empty() && ids.len() != query.ids.len() {
            return Err(fail());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_selection_and_complete_id_partition() {
        let query = OwnedQuery {
            kind: OwnedQueryKind::Ships,
            ids: vec![7, 8],
            fields: vec!["level".into()],
        };
        assert!(query.validate().is_ok());
        let mut result = OwnedQueryResult {
            entries: vec![json!({"ship_id":7,"config_id":100,"level":1})],
            missing_ids: vec![8],
        };
        assert!(result.validate(&query).is_ok());
        result.missing_ids.clear();
        assert!(result.validate(&query).is_err());
        result.missing_ids = vec![7, 8];
        assert!(result.validate(&query).is_err());
    }
    #[test]
    fn rejects_unrequested_fields_and_invalid_locations() {
        let query = OwnedQuery {
            kind: OwnedQueryKind::Equipment,
            ids: vec![100],
            fields: vec!["equipped".into()],
        };
        let mut result = OwnedQueryResult {
            entries: vec![
                json!({"config_id":100,"equipped":[{"ship_id":7,"slot_index":1,"equipment_id":700}]}),
            ],
            missing_ids: vec![],
        };
        assert!(result.validate(&query).is_ok());
        result.entries[0]["equipped"][0]["slot_index"] = json!(6);
        assert!(result.validate(&query).is_err());
        result.entries[0]["equipped"] = json!([]);
        result.entries[0]["warehouse_quantity"] = json!(0);
        assert!(result.validate(&query).is_err());
    }

    #[test]
    fn rejects_unknown_duplicate_and_unsafe_selectors() {
        let mut query = OwnedQuery {
            kind: OwnedQueryKind::Equipment,
            ids: vec![],
            fields: vec!["skills".into()],
        };
        assert!(query.validate().is_err());
        query.fields = vec!["config_id".into(), "config_id".into()];
        assert!(query.validate().is_err());
        query.fields.clear();
        query.ids = vec![1, 1];
        assert!(query.validate().is_err());
        query.ids = vec![9007199254740992];
        assert!(query.validate().is_err());
        query.ids = vec![];
        assert!(query.validate().is_ok());
    }
}

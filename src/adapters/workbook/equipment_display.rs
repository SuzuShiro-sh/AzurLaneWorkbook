//! 装备字段的可读文本展示；原始投影仍用于身份、配置与语义摘要。
use crate::application::{
    LayoutValueFormat, WorkbookFieldLayout, WorkbookProjectionRow, WorkbookProjectionValue,
};
use crate::domain::{EnhanceLevel, EquipmentConfigId, EquipmentDismantleSafety};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn projected_text(
    field: &WorkbookFieldLayout,
    row: &WorkbookProjectionRow,
) -> Result<Option<String>, String> {
    equipment_text(field, |key| {
        Ok(match row.value(key) {
            Some(WorkbookProjectionValue::Text(v)) => Value::String(v.clone()),
            Some(WorkbookProjectionValue::Json(v)) => {
                serde_json::from_str(v).map_err(|e| format!("{key}: {e}"))?
            }
            Some(WorkbookProjectionValue::Integer(v)) => Value::from(*v),
            Some(WorkbookProjectionValue::Decimal(v)) => Value::from(*v),
            Some(WorkbookProjectionValue::Boolean(v)) => Value::Bool(*v),
            _ => Value::Null,
        })
    })
}

/// 以原始字段访问器复用展示规则，独立期望仍从其自身的数据模型取值。
pub(super) fn equipment_text(
    field: &WorkbookFieldLayout,
    get: impl Fn(&str) -> Result<Value, String>,
) -> Result<Option<String>, String> {
    if field.sheet_key() != "equipment_inventory" || field.value_format() != LayoutValueFormat::Text
    {
        return Ok(None);
    }
    let key = field.stable_key();
    let value = get(key)?;
    let text = match key {
        "source_type" => match value.as_str() {
            Some("warehouse") => "仓库".into(),
            Some("ship") => "舰船".into(),
            Some("unowned") => "未持有".into(),
            _ => scalar(&value),
        },
        "current_enhance_level" => pair(&value, &get("maximum_enhance_level")?),
        "family_warehouse_quantity" => pair(&value, &get("family_equipped_quantity")?),
        "equipment_type" => {
            let mut text = scalar(&value);
            for (flag, label) in [("is_device", "设备"), ("is_aircraft", "舰载机")] {
                if get(flag)?.as_bool() == Some(true) && !text.contains(label) {
                    text.push_str(&format!("\n类别标记：{label}"));
                }
            }
            text
        }
        "compatible_main_ship_types" => {
            let mut lines = Vec::new();
            for (key, label) in [
                ("compatible_main_ship_types", "主力舰种"),
                ("compatible_sub_ship_types", "潜艇舰种"),
                ("forbidden_ship_types", "禁用舰种"),
            ] {
                let text = scalar(&get(key)?);
                if !text.is_empty() {
                    lines.push(format!("{label}：{text}"));
                }
            }
            lines.join("；")
        }
        "equipment_limit" => {
            if value.as_i64() == Some(0) {
                "无互斥限制".into()
            } else if value.is_null() {
                String::new()
            } else {
                format!("限制标识：{}", scalar(&value))
            }
        }
        "attributes_json" => {
            let mut lines = Vec::new();
            let mut has_anti = false;
            for item in array(&value) {
                let name = item["name"].as_str().unwrap_or("");
                has_anti |=
                    item["key"].as_str() == Some("anti_siren_power") || name.contains("塞壬");
                let number = &item["value"];
                let sign = if number.as_f64().is_some_and(|v| v > 0.0) {
                    "+"
                } else {
                    ""
                };
                lines.push(format!(
                    "{}：{sign}{}",
                    if name.is_empty() {
                        item["key"].as_str().unwrap_or("属性")
                    } else {
                        name
                    },
                    scalar(number)
                ));
            }
            let anti = get("anti_siren_power")?;
            if !has_anti && !anti.is_null() {
                lines.push(format!(
                    "对塞壬增伤：{}{}",
                    if anti.as_f64().is_some_and(|v| v > 0.0) {
                        "+"
                    } else {
                        ""
                    },
                    scalar(&anti)
                ));
            }
            lines.join("\n")
        }
        "weapons_json" => array(&value)
            .iter()
            .enumerate()
            .map(|(index, weapon)| {
                let mut lines = vec![format!("武器{}", index + 1)];
                for (key, label) in [
                    ("damage", "伤害"),
                    ("reload_max", "装填参数"),
                    ("range", "射程"),
                    ("minimum_range", "最小射程"),
                    ("angle", "射界"),
                    ("attack_attribute_ratio", "攻击属性倍率"),
                    ("corrected", "修正参数"),
                    ("barrage_count", "弹幕配置数"),
                    ("bullet_count", "子弹配置数"),
                    ("torpedo_ammo", "鱼雷弹药"),
                    ("recover_time", "恢复参数"),
                ] {
                    if let Some(v) = weapon.get(key).filter(|v| !v.is_null()) {
                        lines.push(format!("{label}：{}", scalar(v)));
                    }
                }
                lines.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        "effect_summary" => {
            let effects = get("skill_effects_json")?;
            let mut seen = BTreeSet::new();
            let mut lines = Vec::new();
            for skill in array(&effects) {
                let hidden = skill["hidden"].as_bool() == Some(true)
                    || skill["skill_visibility"].as_str() == Some("hidden");
                if !seen.insert((
                    scalar(&skill["skill_id"]),
                    scalar(&skill["skill_level"]),
                    hidden,
                )) {
                    continue;
                }
                let name = skill["name"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("未命名技能");
                let description = skill["description"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .unwrap_or("效果说明未获取");
                lines.push(format!(
                    "{}{name}（等级{}）\n{description}",
                    if hidden { "隐藏技能：" } else { "" },
                    scalar(&skill["skill_level"])
                ));
            }
            if lines.is_empty() && !array(&get("skill_references_json")?).is_empty() {
                "效果说明未获取".into()
            } else {
                lines.join("\n\n")
            }
        }
        "family_owned_enhance_distribution" => scalar(&value)
            .split('，')
            .filter_map(|part| {
                let (level, count) = part.split_once(':')?;
                let count: u64 = count.parse().ok()?;
                if count == 0 {
                    return None;
                }
                Some(format!(
                    "{}：{count}件",
                    if level == "+0" {
                        "未强化".into()
                    } else {
                        format!("强化{level}")
                    }
                ))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "compose_material_costs" => {
            let gold = get("compose_gold_cost")?;
            if gold.is_null() && array(&value).is_empty() {
                String::new()
            } else {
                let mut lines = Vec::new();
                if !gold.is_null() {
                    lines.push(format!("每件资金：{}", scalar(&gold)));
                }
                let held = get("blueprint_count")?;
                let blueprint = get("blueprint_item_id")?;
                for item in array(&value) {
                    let owned = if scalar(&item["item_id"]) == scalar(&blueprint) {
                        scalar(&held)
                    } else {
                        "未获取".into()
                    };
                    lines.push(format!(
                        "{}\n持有：{owned}；每件需要：{}",
                        item_name(item),
                        scalar(&item["quantity"])
                    ));
                }
                lines.join("\n\n")
            }
        }
        "next_cost_json" => {
            if get("next_config_id")?.is_null() {
                "已达强化上限".into()
            } else {
                resources(&value)
            }
        }
        "dismantle_yield_json" => resources(&value),
        "dismantlable" => {
            if get("source_type")?.as_str() == Some("unowned") {
                "不可拆解\n未持有".into()
            } else if value.as_bool() == Some(true) {
                "可拆解".into()
            } else {
                let config = scalar(&get("config_id")?)
                    .parse::<u64>()
                    .map_err(|e| format!("config_id: {e}"))?;
                let config = EquipmentConfigId::new(config).map_err(|e| e.to_string())?;
                let number = |key: &str| -> Result<u32, String> {
                    u32::try_from(
                        get(key)?
                            .as_u64()
                            .ok_or_else(|| format!("{key} 缺少整数"))?,
                    )
                    .map_err(|e| e.to_string())
                };
                let level =
                    u8::try_from(number("current_enhance_level")?).map_err(|e| e.to_string())?;
                let safety = EquipmentDismantleSafety::new(
                    config,
                    number("importance")?,
                    number("rarity")?,
                    EnhanceLevel::new(level),
                );
                let mut lines = vec!["不可拆解"];
                for (reason, label) in [
                    (safety.is_important(), "重要装备"),
                    (safety.is_protected_variant(), "受保护变体"),
                    (safety.requires_rarity_confirmation(), "品质需确认"),
                    (safety.is_enhanced(), "已强化"),
                ] {
                    if reason {
                        lines.push(label);
                    }
                }
                lines.join("\n")
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(text))
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(v) => v.clone(),
        _ => value.to_string(),
    }
}
fn pair(left: &Value, right: &Value) -> String {
    if left.is_null() && right.is_null() {
        String::new()
    } else {
        format!("{}/{}", scalar(left), scalar(right))
    }
}
fn item_name(item: &Value) -> String {
    item["name"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("物品ID：{}", scalar(&item["item_id"])))
}
fn resources(value: &Value) -> String {
    let mut lines = Vec::new();
    if value["gold"].as_u64().is_some_and(|v| v > 0) {
        lines.push(format!("资金：{}", scalar(&value["gold"])));
    }
    for item in array(&value["items"]) {
        if item["quantity"].as_u64().is_some_and(|v| v > 0) {
            lines.push(format!(
                "{}；数量：{}",
                item_name(item),
                scalar(&item["quantity"])
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn render(key: &str, values: Value) -> String {
        let layout = crate::adapters::workbook::layout::template::full_test_layout();
        let field = layout
            .fields()
            .iter()
            .find(|f| f.sheet_key() == "equipment_inventory" && f.stable_key() == key)
            .unwrap();
        equipment_text(field, |key| {
            Ok(values.get(key).cloned().unwrap_or(Value::Null))
        })
        .unwrap()
        .unwrap()
    }
    #[test]
    fn merged_inventory_values_preserve_source_counts_and_compatibility() {
        assert_eq!(
            render(
                "current_enhance_level",
                json!({"current_enhance_level":6,"maximum_enhance_level":10})
            ),
            "6/10"
        );
        assert_eq!(
            render(
                "family_warehouse_quantity",
                json!({"family_warehouse_quantity":5,"family_equipped_quantity":3})
            ),
            "5/3"
        );
        assert_eq!(render("source_type", json!({"source_type":"ship"})), "舰船");
        assert_eq!(
            render(
                "compatible_main_ship_types",
                json!({"compatible_main_ship_types":"驱逐","compatible_sub_ship_types":"潜艇","forbidden_ship_types":"航母"})
            ),
            "主力舰种：驱逐；潜艇舰种：潜艇；禁用舰种：航母"
        );
        assert_eq!(render("compatible_main_ship_types", json!({})), "");
        assert_eq!(
            render("equipment_limit", json!({"equipment_limit":0})),
            "无互斥限制"
        );
        assert_eq!(
            render("equipment_limit", json!({"equipment_limit":12})),
            "限制标识：12"
        );
        assert_eq!(
            render(
                "family_owned_enhance_distribution",
                json!({"family_owned_enhance_distribution":"+0:3，+2:0，+6:2"})
            ),
            "未强化：3件\n强化+6：2件"
        );
    }
    #[test]
    fn attributes_keep_sign_zero_and_one_anti_siren_entry() {
        assert_eq!(
            render(
                "attributes_json",
                json!({"attributes_json":[{"name":"炮击","key":"cannon","value":20},{"name":"航速","key":"speed","value":-2},{"name":"命中","key":"hit","value":0}],"anti_siren_power":0.2})
            ),
            "炮击：+20\n航速：-2\n命中：0\n对塞壬增伤：+0.2"
        );
        assert_eq!(
            render(
                "attributes_json",
                json!({"attributes_json":[{"name":"对塞壬增伤","key":"anti_siren_power","value":0.2}],"anti_siren_power":0.2})
            ),
            "对塞壬增伤：+0.2"
        );
    }
    #[test]
    fn skills_deduplicate_identity_level_visibility_not_names() {
        let normal = json!({"skill_id":1,"skill_level":1,"name":"同名技能","description":"实际说明","hidden":false});
        let other = json!({"skill_id":2,"skill_level":1,"name":"同名技能","description":"另一说明","hidden":false});
        let hidden = json!({"skill_id":1,"skill_level":1,"name":"同名技能","description":"隐藏说明","hidden":true});
        assert_eq!(
            render(
                "effect_summary",
                json!({"skill_effects_json":[normal.clone(),normal,other,hidden]})
            ),
            "同名技能（等级1）\n实际说明\n\n同名技能（等级1）\n另一说明\n\n隐藏技能：同名技能（等级1）\n隐藏说明"
        );
        assert_eq!(
            render(
                "effect_summary",
                json!({"skill_references_json":[],"skill_effects_json":[]})
            ),
            ""
        );
        assert_eq!(
            render(
                "effect_summary",
                json!({"skill_references_json":[{"skill_id":1}]})
            ),
            "效果说明未获取"
        );
    }
    #[test]
    fn costs_distinguish_recipe_availability_and_enhancement_cap() {
        assert_eq!(
            render(
                "compose_material_costs",
                json!({"compose_material_costs":[],"compose_gold_cost":null})
            ),
            ""
        );
        assert_eq!(
            render(
                "compose_material_costs",
                json!({"compose_material_costs":[{"item_id":10,"quantity":15}],"compose_gold_cost":100,"blueprint_item_id":"10","blueprint_count":80})
            ),
            "每件资金：100\n\n物品ID：10\n持有：80；每件需要：15"
        );
        assert_eq!(
            render(
                "next_cost_json",
                json!({"next_config_id":"11","next_cost_json":{"gold":0,"items":[{"item_id":10,"quantity":2},{"item_id":12,"quantity":0}]}})
            ),
            "物品ID：10；数量：2"
        );
        assert_eq!(
            render(
                "next_cost_json",
                json!({"next_config_id":null,"next_cost_json":{"gold":0,"items":[]}})
            ),
            "已达强化上限"
        );
        assert_eq!(
            render(
                "next_cost_json",
                json!({"next_config_id":"11","next_cost_json":{"gold":0,"items":[]}})
            ),
            ""
        );
        assert_eq!(
            render(
                "dismantle_yield_json",
                json!({"dismantle_yield_json":{"gold":12,"items":[{"item_id":10,"quantity":2}]}})
            ),
            "资金：12\n物品ID：10；数量：2"
        );
    }
    #[test]
    fn weapon_parameters_and_dismantle_reasons_use_actual_values() {
        let weapons = render(
            "weapons_json",
            json!({"weapons_json":[{"damage":10,"reload_max":150,"range":50},{"damage":20,"reload_max":200}]}),
        );
        assert_eq!(
            weapons,
            "武器1\n伤害：10\n装填参数：150\n射程：50\n\n武器2\n伤害：20\n装填参数：200"
        );
        assert_eq!(
            render(
                "dismantlable",
                json!({"source_type":"unowned","dismantlable":false})
            ),
            "不可拆解\n未持有"
        );
        assert_eq!(
            render(
                "dismantlable",
                json!({"source_type":"warehouse","dismantlable":true})
            ),
            "可拆解"
        );
        let reasons = render(
            "dismantlable",
            json!({"source_type":"warehouse","dismantlable":false,"config_id":"1001","importance":2,"rarity":5,"current_enhance_level":6}),
        );
        assert!(reasons.contains("重要装备"));
        assert!(reasons.contains("品质需确认"));
        assert!(reasons.contains("已强化"));
    }
}

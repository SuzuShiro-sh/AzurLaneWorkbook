//! 从舰船科技配置和图鉴历史生成三个阶段的只读摘要。
use serde_json::Value;

pub(crate) const TECHNOLOGY_FIELDS: [&str; 3] =
    ["technology_get", "technology_upgrade", "technology_level"];

/// 历史记录按舰船组共用；缺少某组记录表示从未获得，而不是当前未持有。
pub(crate) fn ship_technology_summaries(
    group_id: u64,
    history_available: bool,
    get: impl Fn(&str, u64) -> Result<Option<Value>, String>,
) -> Result<[String; 3], String> {
    let Some(config) = get("fleet_tech_ship_template", group_id)? else {
        return Ok(Default::default());
    };
    let history = if history_available {
        get("collection_ship_group", group_id)?
    } else {
        None
    };
    let star = history
        .as_ref()
        .map(|v| number(v, "star"))
        .transpose()?
        .unwrap_or(0);
    let level = history
        .as_ref()
        .map(|v| number(v, "maxLV"))
        .transpose()?
        .unwrap_or(0);
    let achieved = [
        history.is_some(),
        history.is_some() && star >= number(&config, "max_star")?,
        history.is_some() && level >= 120,
    ];
    let points = ["pt_get", "pt_upgrage", "pt_level"];
    let mut summaries: [String; 3] = Default::default();
    for index in 0..3 {
        let status = if !history_available {
            "状态未获取"
        } else if achieved[index] {
            "已达成"
        } else {
            "未达成"
        };
        summaries[index] = format!("{status}\n科技点 +{}", number(&config, points[index])?);
        if index == 1 {
            continue;
        }
        let prefix = if index == 0 { "add_get" } else { "add_level" };
        let attr = number(&config, &format!("{prefix}_attr"))?;
        let value = number(&config, &format!("{prefix}_value"))?;
        if attr == 0 || value == 0 {
            continue;
        }
        let attr_config =
            get("attribute_info_by_type", attr)?.ok_or_else(|| format!("属性配置{attr}未获取"))?;
        let attr_name = attr_config["condition"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("属性配置{attr}缺少名称"))?;
        let types = config[format!("{prefix}_shiptype")]
            .as_array()
            .ok_or_else(|| format!("{prefix}_shiptype缺少舰种列表"))?;
        let mut names = Vec::new();
        for ship_type in types {
            let id = ship_type
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or("舰种标识不是正整数")?;
            let kind =
                get("ship_data_by_type", id)?.ok_or_else(|| format!("舰种配置{id}未获取"))?;
            let name = kind["type_name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("舰种配置{id}缺少名称"))?;
            if !names.iter().any(|s| s == name) {
                names.push(name.to_owned());
            }
        }
        if names.is_empty() {
            return Err(format!("{prefix}有属性奖励但没有受益舰种"));
        }
        summaries[index].push_str(&format!("\n{}：{attr_name} +{value}", names.join("／")));
    }
    Ok(summaries)
}
fn number(value: &Value, key: &str) -> Result<u64, String> {
    value[key]
        .as_u64()
        .ok_or_else(|| format!("{key}缺少非负整数"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn rows(history: Option<Value>, available: bool) -> [String; 3] {
        ship_technology_summaries(10117,available,|table,id| Ok(match table {
            "fleet_tech_ship_template" => Some(json!({"pt_get":8,"pt_upgrage":16,"pt_level":12,"max_star":5,"add_get_attr":1,"add_get_value":1,"add_get_shiptype":[1,20,21],"add_level_attr":2,"add_level_value":1,"add_level_shiptype":[1,20,21]})),
            "collection_ship_group" => history.clone(),
            "ship_data_by_type" => Some(json!({"type_name":if id == 1 {"驱逐"} else {"导驱"}})),
            "attribute_info_by_type" => Some(json!({"condition":if id == 1 {"耐久"} else {"炮击"}})),
            _ => None,
        })).unwrap()
    }
    #[test]
    fn technology_rewards_use_history_and_keep_milestones_separate() {
        assert_eq!(
            rows(Some(json!({"star":5,"maxLV":120})), true),
            [
                "已达成\n科技点 +8\n驱逐／导驱：耐久 +1",
                "已达成\n科技点 +16",
                "已达成\n科技点 +12\n驱逐／导驱：炮击 +1"
            ]
        );
        let values = rows(Some(json!({"star":4,"maxLV":119})), true);
        assert!(values[0].starts_with("已达成"));
        assert!(values[1].starts_with("未达成"));
        assert!(values[2].starts_with("未达成"));
        assert!(rows(None, true).iter().all(|s| s.starts_with("未达成")));
        assert!(
            rows(None, false)
                .iter()
                .all(|s| s.starts_with("状态未获取"))
        );
    }
    #[test]
    fn absent_config_is_blank_and_malformed_config_is_an_error() {
        assert_eq!(
            ship_technology_summaries(1, true, |_, _| Ok(None)).unwrap(),
            [String::new(), String::new(), String::new()]
        );
        assert!(ship_technology_summaries(1, true, |_, _| Ok(Some(json!({})))).is_err());
    }
}

/// 科技分类表族共用的列顺序。
pub(crate) const TECHNOLOGY_VIEW_KEY: &str = "ship_technology";
pub(crate) const TECHNOLOGY_VIEW_FIELDS: [&str; 17] = [
    "name",
    "nation",
    "ship_type",
    "armor_type",
    "locked",
    "current_stars",
    "maximum_stars",
    "level",
    "maximum_level",
    "next_level_experience",
    "technology_get",
    "technology_upgrade",
    "technology_level",
    "intimacy",
    "create_time",
    "propose_time",
    "oil_total",
];

/// 按受益舰种组合与属性分类；奖励数值保留在单元格中。
pub(crate) fn technology_categories<'a>(
    summaries: impl IntoIterator<Item = &'a str>,
) -> std::collections::BTreeSet<String> {
    summaries
        .into_iter()
        .filter_map(|summary| summary.lines().nth(2))
        .filter_map(|reward| reward.rsplit_once(" +").map(|(label, _)| label.to_owned()))
        .collect()
}

/// 汇总获得和120级奖励的筛选标签，具体数值保留在阶段摘要中。
pub(crate) fn technology_bonus_summary(summaries: &[String; 3]) -> String {
    if summaries[0].starts_with("科技数据未获取：") {
        return summaries[0].clone();
    }
    technology_categories([summaries[0].as_str(), summaries[2].as_str()])
        .into_iter()
        .map(|category| category.replace('／', "、").replace('：', "-"))
        .collect::<Vec<_>>()
        .join("；")
}

pub(crate) fn technology_template_key(key: &str) -> &str {
    if key.starts_with("ship_technology:") {
        TECHNOLOGY_VIEW_KEY
    } else {
        key
    }
}

/// Excel 工作表标签禁用 : / \ ? * [ ]；对应全角兼容形在部分 Office 中会触发工作簿修复。
pub(crate) fn excel_sheet_tab_char(c: char) -> char {
    match c {
        ':' | '：' => '-',
        '/' | '／' | '\\' | '＼' => '、',
        '?' | '？' | '*' | '＊' => '_',
        '[' | '［' => '(',
        ']' | '］' => ')',
        _ => c,
    }
}

pub(crate) fn excel_sheet_tab_name_is_legal(name: &str) -> bool {
    !name.is_empty()
        && name.encode_utf16().count() <= 31
        && !name.starts_with('\'')
        && !name.ends_with('\'')
        && !name.eq_ignore_ascii_case("history")
        && name.chars().all(|c| excel_sheet_tab_char(c) == c)
}

pub(crate) fn excel_sheet_tab_name(raw: &str) -> String {
    let mut name: String = raw.chars().map(excel_sheet_tab_char).collect();
    if name.encode_utf16().count() > 31 {
        let hash = suzushiro_content_digest::sha256_compact_json(&raw).expect("字符串可序列化");
        let mut prefix = String::new();
        for c in name.chars() {
            if prefix.encode_utf16().count() + c.len_utf16() > 22 {
                break;
            }
            prefix.push(c);
        }
        name = format!("{prefix}~{}", &hash[..8]);
    }
    name
}

/// 用受益舰种组合与属性生成紧凑页签名，并满足 Excel 名称限制。
pub(crate) fn technology_category_layout(
    template: &crate::application::WorkbookSheetLayout,
    category: &str,
) -> crate::application::WorkbookSheetLayout {
    crate::application::WorkbookSheetLayout::new(
        format!("ship_technology:{category}"),
        template.generation(),
        excel_sheet_tab_name(category),
        template.order(),
        template.freeze_cell().map(str::to_owned),
        template.default_filter(),
        template.description().to_owned(),
        false,
    )
}

#[cfg(test)]
mod category_tests {
    use super::*;
    #[test]
    fn bonus_labels_merge_stages_deduplicate_and_preserve_errors() {
        let summaries = [
            "未达成\n科技点 +8\n战巡／战列／航战：命中 +1".to_owned(),
            "已达成\n科技点 +16".to_owned(),
            "已达成\n科技点 +12\n战巡／战列／航战：命中 +2".to_owned(),
        ];
        assert_eq!(
            technology_bonus_summary(&summaries),
            "战巡、战列、航战-命中"
        );
        let mut distinct = summaries.clone();
        distinct[2] = "状态未获取\n科技点 +12\n轻巡：耐久 +2".to_owned();
        assert_eq!(
            technology_bonus_summary(&distinct),
            "战巡、战列、航战-命中；轻巡-耐久"
        );
        assert_eq!(technology_bonus_summary(&Default::default()), "");
        assert_eq!(
            technology_bonus_summary(&[
                "未达成\n科技点 +8".to_owned(),
                String::new(),
                String::new()
            ]),
            ""
        );
        let failed = std::array::from_fn(|_| "科技数据未获取：缺少属性表".to_owned());
        assert_eq!(technology_bonus_summary(&failed), failed[0]);
    }

    #[test]
    fn technology_categories_merge_reward_values_and_skip_points_only() {
        let groups = technology_categories([
            "已达成\n科技点 +8\n驱逐／导驱：耐久 +1",
            "未达成\n科技点 +12\n驱逐／导驱：耐久 +2",
            "已达成\n科技点 +16",
            "未达成\n科技点 +10\n轻巡：耐久 +3",
        ]);
        assert_eq!(
            groups.into_iter().collect::<Vec<_>>(),
            ["轻巡：耐久", "驱逐／导驱：耐久"]
        );
    }

    #[test]
    fn technology_sheet_tab_names_replace_excel_forbidden_separators() {
        assert_eq!(excel_sheet_tab_name("驱逐／导驱：耐久"), "驱逐、导驱-耐久");
        assert_eq!(excel_sheet_tab_name("轻巡：耐久"), "轻巡-耐久");
        assert!(excel_sheet_tab_name_is_legal("驱逐、导驱-耐久"));
        assert!(!excel_sheet_tab_name_is_legal("驱逐／导驱：耐久"));
        assert!(!excel_sheet_tab_name_is_legal("A:B"));
    }
}

//! 集中维护布局工作表、字段、枚举与样式的默认展示规则。

use crate::application::{
    LayoutEditor, LayoutGenerationMode, LayoutValueFormat, RegisteredLayoutField,
};

use super::{WorkbookProbeError, template_error};

#[derive(Clone, Copy)]
pub(super) struct SheetPresentation {
    pub(super) generation: LayoutGenerationMode,
    pub(super) display_name: &'static str,
    pub(super) description: &'static str,
}

pub(super) fn sheet_presentation(
    stable_key: &str,
) -> Result<SheetPresentation, WorkbookProbeError> {
    let presentation = match stable_key {
        "ship_technology" => SheetPresentation {
            generation: LayoutGenerationMode::Omitted,
            display_name: "科技分类",
            description: "按受益舰种组合与属性生成分类页签，如驱逐、导驱-耐久；各表每组保留最高星级、同星级最高等级实例。已达成标绿，未达成标红，共用本表字段设置。",
        },
        "loadout_plan" => SheetPresentation {
            generation: LayoutGenerationMode::Visible,
            display_name: "配装计划",
            description: "按持有实例或未持有舰船组逐行汇总静态身份、成长、技能和五个槽位。",
        },
        "equipment_inventory" => SheetPresentation {
            generation: LayoutGenerationMode::Visible,
            display_name: "装备总表",
            description: "每个实际来源或未持有配置一行，汇总本行装备参数、效果、库存与处理输入。",
        },
        "check_results" => SheetPresentation {
            generation: LayoutGenerationMode::Visible,
            display_name: "检查结果",
            description: "记录计划检查结论、问题定位和资源变化证据。",
        },
        "execution_results" => SheetPresentation {
            generation: LayoutGenerationMode::Visible,
            display_name: "执行结果",
            description: "记录每个执行步骤的请求、响应、回读和最终校验。",
        },
        "dictionaries" => SheetPresentation {
            generation: LayoutGenerationMode::Hidden,
            display_name: "_字典",
            description: "保存数据工作簿实际使用的稳定值与显示标签映射。",
        },
        "resource_recipes" => SheetPresentation {
            generation: LayoutGenerationMode::Hidden,
            display_name: "_资源配方",
            description: "保存合成、强化和拆解所需的资源计算明细。",
        },
        "raw_data" => SheetPresentation {
            generation: LayoutGenerationMode::Omitted,
            display_name: "_原始数据",
            description: "保存可追溯的规范原始记录分块和内容摘要。",
        },
        "plan_data" => SheetPresentation {
            generation: LayoutGenerationMode::Hidden,
            display_name: "_计划",
            description: "保存经过检查并可按哈希核对的固定执行步骤。",
        },
        "schema" => SheetPresentation {
            generation: LayoutGenerationMode::Hidden,
            display_name: "_schema",
            description: "保存生成时实际生效的布局快照、哈希和省略项。",
        },
        _ => {
            return Err(template_error(format!(
                "工作表 {stable_key} 缺少默认显示定义"
            )));
        }
    };
    Ok(presentation)
}

pub(super) fn single_value_format(
    field: &RegisteredLayoutField,
) -> Result<LayoutValueFormat, WorkbookProbeError> {
    if field.sheet_key() == "equipment_inventory"
        && field.allowed_formats().contains(&LayoutValueFormat::Text)
    {
        return Ok(LayoutValueFormat::Text);
    }
    let mut values = field.allowed_formats().iter().copied();
    let value = values.next().ok_or_else(|| {
        template_error(format!(
            "字段 {}.{} 没有默认格式",
            field.sheet_key(),
            field.stable_key()
        ))
    })?;
    if values.next().is_some() {
        return Err(template_error(format!(
            "字段 {}.{} 存在多个格式，无法确定模板默认值",
            field.sheet_key(),
            field.stable_key()
        )));
    }
    Ok(value)
}

pub(super) fn value_format_label(value: LayoutValueFormat) -> &'static str {
    match value {
        LayoutValueFormat::Text => "文本",
        LayoutValueFormat::Integer => "整数",
        LayoutValueFormat::Decimal => "小数",
        LayoutValueFormat::Percentage => "百分比",
        LayoutValueFormat::DateTime => "日期时间",
        LayoutValueFormat::Json => "JSON",
    }
}

pub(super) fn editor_label(value: LayoutEditor) -> &'static str {
    match value {
        LayoutEditor::ReadOnly => "只读",
        LayoutEditor::Boolean => "是非",
        LayoutEditor::Enumeration => "枚举",
        LayoutEditor::Integer => "整数",
        LayoutEditor::Text => "文本",
    }
}

pub(super) fn default_field_width(
    key: &str,
    format: LayoutValueFormat,
    editor: LayoutEditor,
) -> f64 {
    if matches!(format, LayoutValueFormat::Json)
        || key.ends_with("_json")
        || matches!(key, "compose_material_costs" | "compatible_main_ship_types")
    {
        42.0
    } else if key.contains("description")
        || key.contains("summary")
        || key.contains("errors")
        || matches!(
            key,
            "message" | "note" | "expected_result" | "omitted_items" | "acquisition"
        )
    {
        34.0
    } else if key.ends_with("_hash") || key.contains("sha256") {
        28.0
    } else if key.ends_with("_id") || key.ends_with("_ref") || key.ends_with("_ids") {
        22.0
    } else if matches!(format, LayoutValueFormat::DateTime) {
        20.0
    } else if matches!(
        format,
        LayoutValueFormat::Integer | LayoutValueFormat::Decimal
    ) {
        14.0
    } else if editor != LayoutEditor::ReadOnly {
        20.0
    } else {
        18.0
    }
}

pub(super) fn default_field_wrap(key: &str, format: LayoutValueFormat) -> bool {
    matches!(format, LayoutValueFormat::Json)
        || key.ends_with("_json")
        || matches!(
            key,
            "compose_material_costs"
                | "family_owned_enhance_distribution"
                | "dismantlable"
                | "equipment_type"
                | "compatible_main_ship_types"
        )
        || key.starts_with("technology_")
        || key.starts_with("skills_")
        || key.contains("description")
        || key.contains("summary")
        || key.contains("errors")
        || matches!(
            key,
            "message" | "note" | "expected_result" | "omitted_items" | "acquisition"
        )
}

pub(super) fn field_description(
    sheet_key: &str,
    field_key: &str,
    display_name: &str,
) -> Result<String, WorkbookProbeError> {
    let sheet_key = if sheet_key == "ship_technology" {
        "loadout_plan"
    } else {
        sheet_key
    };
    let description = match (sheet_key, field_key) {
        ("loadout_plan", "name") => "展示实例名；已誓约且实例名与原名不同时显示为实例名(真实名)，名称链接指向静态配置原名对应的图鉴页。".to_owned(),
        ("loadout_plan", "acquisition") => "BWiki 记录的建造、掉落和其他获取途径；活动掉落为历史记录，不代表当前开放。使用缓存时没有资料就写“资料未缓存，待更新”，同步不等待联网。显式资料更新只刷新缓存。每次同步更新会等待在线结果。执行写回只复用已有缓存。".to_owned(),
        ("loadout_plan", "original_name") => "舰船静态组的基础配置名称，用于图鉴链接。".to_owned(),
        ("loadout_plan", "technology_bonus") => "汇总获得及120级的受益舰种与属性，多个类别以分号分隔；使用文本包含筛选定位加成，数值与达成状态见阶段科技列。".to_owned(),
        ("loadout_plan", "technology_get" | "technology_upgrade" | "technology_level") => "按舰船组展示该阶段新增科技点、受益舰种和属性奖励；已达成依据图鉴历史，重复舰船共享记录，当前未持有不等于未达成。".to_owned(),
        ("loadout_plan", key) if key.ends_with("_equipment_name") => {
            "当前槽位实际穿戴的装备名称；只读。换装、卸下或拆解请使用相邻的更换／处理列。".to_owned()
        }
        ("equipment_inventory", "operation") => "留空保持现状；选择强化或拆解，并填写处理数量。".to_owned(),
        ("equipment_inventory", "processing_quantity") => "强化或拆解的正整数数量；舰船来源只能填1，仓库来源不超过当前数量。".to_owned(),
        ("loadout_plan", key) if key.ends_with("_target_equipment_family") => {
            "留空保持当前装备；选择带来源和数量的装备项进行换装，来源包括仓库、舰船槽位和图纸合成；选择卸下将装备放回仓库；选择拆解将消耗当前装备并获得拆解产物。".to_owned()
        }
        ("loadout_plan", key) if key.ends_with("_source_policy") => {
            "带来源的装备选项已确定来源，此列须留空；仅填写装备族时可指定来源顺序，简化换装留空默认仓库→合成。".to_owned()
        }
        ("loadout_plan", key) if key.ends_with("_exact_source") => {
            "带来源的装备选项已确定来源，此列须留空；使用独立来源列时，仅在指定来源策略下填写仓库或舰船槽位引用。".to_owned()
        }
        ("loadout_plan", key) if key.ends_with("_target_enhance_level") => {
            "填写目标强化等级，不能低于实际装备等级。".to_owned()
        }
        ("loadout_plan", key) if key.ends_with("_allocation_priority") => {
            "数值越小越先分配稀缺装备来源。".to_owned()
        }
        ("loadout_plan", key) if key.ends_with("_note") => {
            "仅供用户记录，不参与计划执行。".to_owned()
        }
        ("equipment_inventory", "target_enhance_level") => {
            "留空表示保持；填写时不能低于当前强化等级。".to_owned()
        }
        ("equipment_inventory", "note") => "仅供用户记录，不参与执行。".to_owned(),
        ("equipment_inventory", "quantity") => "记录该来源行自身的实际数量。".to_owned(),
        ("equipment_inventory", "family_warehouse_quantity") => {
            "记录同一装备族的仓库数量合计；该值会在同族各来源行重复展示，不能跨行相加。".to_owned()
        }
        ("equipment_inventory", "family_equipped_quantity") => {
            "记录同一装备族的已装备数量合计；该值会在同族各来源行重复展示，不能跨行相加。".to_owned()
        }
        ("equipment_inventory", "family_owned_quantity") => {
            "记录同一装备族的实际持有数量合计；该值会在同族各来源行重复展示，不能跨行相加。".to_owned()
        }
        ("equipment_inventory", "family_potential_quantity") => {
            "记录同一装备族计入可制作数量后的潜在数量合计；该值会在同族各来源行重复展示，不能跨行相加。".to_owned()
        }
        ("loadout_plan", "propose_time") => "将客户端誓约秒值转换为 UTC 日期时间；未誓约或时间为零时留空。".to_owned(),
        ("loadout_plan", "create_time") => {
            "将客户端获得时间秒值转换为 UTC 日期时间；时间为零时留空。".to_owned()
        }
        ("equipment_inventory", "data_complete") => {
            "标明该行实际来源数据是否完整。".to_owned()
        }
        ("equipment_inventory", "config_data_complete") => {
            "标明该行对应装备配置的原始数据是否完整。".to_owned()
        }
        ("loadout_plan", key) if key.ends_with("_global_delta") => {
            "记录未能稳定拆分为舰队科技和其他来源的全局属性增量。".to_owned()
        }
        (_, "read_errors") => "记录读取不完整时的结构化错误信息。".to_owned(),
        (_, "data_complete") => "标明该行依赖的原始数据是否完整。".to_owned(),
        ("schema", _) => format!("记录生成工作簿时实际生效的{display_name}。"),
        _ => {
            let sheet_name = sheet_presentation(sheet_key)?.display_name;
            format!("记录{sheet_name}中的{display_name}。")
        }
    };
    Ok(description)
}

pub(super) fn default_field_name(
    sheet_key: &str,
    field_key: &str,
) -> Result<String, WorkbookProbeError> {
    let result_name = match (sheet_key, field_key) {
        ("check_results", "checked_at") => Some("检查时间"),
        ("check_results", "status") => Some("检查结果"),
        ("check_results", "object_ref") => Some("问题位置"),
        ("check_results", "message") => Some("问题说明"),
        ("check_results", "current_value") => Some("当前情况"),
        ("check_results", "expected_value") => Some("需要满足"),
        ("execution_results", "executed_at") => Some("执行时间"),
        ("execution_results", "step_sequence") => Some("步骤"),
        ("execution_results", "step_type") => Some("操作"),
        ("execution_results", "object_ref") => Some("操作对象"),
        ("execution_results", "status") => Some("步骤结果"),
        ("execution_results", "final_verification_status") => Some("最终核验"),
        ("execution_results", "may_have_writes") => Some("可能已操作"),
        ("execution_results", "readback_summary") => Some("核验详情"),
        ("execution_results", "message") => Some("结果说明"),
        _ => None,
    };
    if let Some(name) = result_name {
        return Ok(name.to_owned());
    }
    if sheet_key == "equipment_inventory" {
        let name = match field_key {
            "name" => Some("装备名称"),
            "equipment_type" => Some("装备类型"),
            "rarity" => Some("稀有度"),
            "tech_level" => Some("装备科技等级"),
            "current_enhance_level" => Some("强化等级（当前／上限）"),
            "quantity" => Some("此来源数量"),
            "source_type" => Some("来源"),
            "ship_instance_id" => Some("舰船实例ID"),
            "ship_name" => Some("舰船名称"),
            "slot_index" => Some("装备槽位"),
            "operation" => Some("装备操作"),
            "processing_quantity" => Some("处理数量"),
            "target_enhance_level" => Some("目标强化等级"),
            "attributes_json" => Some("装备属性"),
            "weapons_json" => Some("武器性能"),
            "effect_summary" => Some("装备效果"),
            "ammo_type" => Some("弹药类型"),
            "speciality" => Some("装备特性"),
            "compatible_main_ship_types" => Some("舰种适配"),
            "equipment_limit" => Some("装备互斥限制"),
            "nation" => Some("装备阵营"),
            "gear_score" => Some("装备评分"),
            "description" => Some("装备说明"),
            "labels" => Some("装备标签"),
            "family_warehouse_quantity" => Some("同类库存（仓库／已装备）"),
            "family_owned_enhance_distribution" => Some("同类强化分布"),
            "craftable_actual" => Some("当前可合成数量"),
            "compose_material_costs" => Some("合成消耗"),
            "next_cost_json" => Some("下一级强化消耗"),
            "dismantle_yield_json" => Some("单件拆解收益"),
            "dismantlable" => Some("拆解状态"),
            "config_id" => Some("装备配置ID"),
            "read_errors" => Some("配置读取提示"),
            _ => None,
        };
        if let Some(name) = name {
            return Ok(name.to_owned());
        }
    }
    if matches!(sheet_key, "loadout_plan" | "ship_technology") {
        let label = match field_key {
            "technology_bonus" => Some("科技加成"),
            "technology_get" => Some("获得科技"),
            "technology_upgrade" => Some("满星科技"),
            "technology_level" => Some("120级科技"),
            "instance_id" => Some("舰船实例ID"),
            "name" => Some("舰船名称"),
            "original_name" => Some("舰船原名"),
            "acquisition" => Some("获取方式"),
            "locked" => Some("是否锁定"),
            "maximum_stars" => Some("星级上限"),
            "level" => Some("当前等级"),
            "maximum_level" => Some("等级上限"),
            "experience_in_level" => Some("本级已获经验"),
            "total_experience" => Some("累计经验"),
            "intimacy" => Some("当前好感"),
            "proposed" => Some("是否誓约"),
            "oil_total" => Some("总油耗"),
            _ => None,
        };
        if let Some(label) = label {
            return Ok(label.to_owned());
        }
    }
    if matches!(sheet_key, "loadout_plan" | "ship_technology") && field_key == "propose_time" {
        return Ok("誓约时间（UTC）".to_owned());
    }
    if matches!(sheet_key, "loadout_plan" | "ship_technology") && field_key == "create_time" {
        return Ok("获取时间（UTC）".to_owned());
    }
    if let Some(label) = special_field_label(field_key) {
        return Ok(label.to_owned());
    }
    if sheet_key == "loadout_plan"
        && let Some(label) = ship_stat_label(field_key)
    {
        return Ok(label);
    }
    if sheet_key == "loadout_plan"
        && let Some(label) = ship_slot_label(field_key)?
    {
        return Ok(label);
    }
    if sheet_key == "loadout_plan"
        && let Some(label) = ship_skill_label(field_key)
    {
        return Ok(label.to_owned());
    }

    let mut label = String::new();
    for token in field_key.split('_') {
        let translated = field_token_label(token).ok_or_else(|| {
            template_error(format!(
                "字段 {sheet_key}.{field_key} 包含未定义显示词元 {token}"
            ))
        })?;
        label.push_str(translated);
    }
    Ok(label)
}

fn ship_skill_label(field_key: &str) -> Option<&'static str> {
    Some(match field_key {
        "skills_progress_summary" => "技能等级与经验",
        "skills_description_summary" => "技能说明与当前效果",
        "skills_effective_skill_id" => "生效技能 ID",
        "skills_name" => "技能名称",
        "skills_description" => "技能描述",
        "skills_current_effect" => "技能当前效果",
        "skills_level" => "技能等级",
        "skills_maximum_level" => "技能等级上限",
        "skills_experience" => "技能经验",
        "skills_next_level_experience" => "技能下级所需经验",
        "skills_effect_parameters" => "技能效果参数",
        "skills_raw_structure" => "技能原始结构",
        "skills_data_complete" => "技能数据是否完整",
        "skills_read_errors" => "技能读取错误",
        _ => return None,
    })
}

fn special_field_label(field_key: &str) -> Option<&'static str> {
    match field_key {
        "operation" => Some("操作"),
        "processing_quantity" => Some("处理数量"),
        "experience_in_level" => Some("当前等级经验"),
        "next_level_experience" => Some("升级所需经验"),
        "combat_power" => Some("综合性能"),
        "action_index" => Some("动作键"),
        "acknowledged_write_count" => Some("已确认写入数"),
        "anti_siren_power" => Some("对塞壬增伤"),
        "canonical_json_chunk" => Some("规范 JSON 分块"),
        "checked_at" => Some("检查时间"),
        "content_sha256" => Some("内容 SHA-256"),
        "current_then_warehouse_then_compose_then_ship" => Some("当前装备后按来源查找"),
        "data_complete" => Some("数据是否完整"),
        "config_data_complete" => Some("配置数据是否完整"),
        "default_filter" => Some("默认筛选"),
        "executed_at" => Some("执行时间"),
        "field_width_hundredths" => Some("字段宽度百分之一值"),
        "fire_fx" => Some("开火特效"),
        "fire_fx_loop_type" => Some("开火特效循环类型"),
        "fire_sfx" => Some("开火音效"),
        "is_aircraft" => Some("是否舰载机"),
        "is_device" => Some("是否设备"),
        "read_errors" => Some("读取错误"),
        "quantity" => Some("本行数量"),
        "family_warehouse_quantity" => Some("同类合计：仓库数量"),
        "family_equipped_quantity" => Some("同类合计：已装备数量"),
        "family_owned_quantity" => Some("同类合计：实际持有数量"),
        "family_potential_quantity" => Some("同类合计：潜在数量"),
        "readback_summary" => Some("回读摘要"),
        "may_have_writes" => Some("可能发生写入"),
        "observed_state_change_count" => Some("已观察状态变化数"),
        "report_hash" => Some("报告摘要"),
        "report_status" => Some("报告状态"),
        "source_content_sha256" => Some("来源内容 SHA-256"),
        "spawn_bound" => Some("发射挂点"),
        "stop_reason" => Some("停止原因"),
        "target_fingerprint_sha256" => Some("目标指纹 SHA-256"),
        "verified_write_count" => Some("已验证写入数"),
        "write_acknowledged" => Some("写入已确认"),
        "write_effect" => Some("写入影响"),
        _ => None,
    }
}

fn ship_stat_label(field_key: &str) -> Option<String> {
    let remainder = field_key.strip_prefix("stat_")?;
    let (attribute, stage) = [
        ("_summary", "摘要"),
        ("_equipment_delta", "装备增量"),
        ("_global_delta", "全局增量"),
        ("_base", "基础值"),
        ("_final", "最终值"),
    ]
    .into_iter()
    .find_map(|(suffix, label)| remainder.strip_suffix(suffix).map(|value| (value, label)))?;
    let attribute_label = match attribute {
        "durability" => "耐久",
        "cannon" => "炮击",
        "torpedo" => "雷击",
        "air" => "航空",
        "reload" => "装填",
        "anti_aircraft" => "防空",
        "hit" => "命中",
        "dodge" => "机动",
        "anti_sub" => "反潜",
        "luck" => "幸运",
        "speed" => "航速",
        _ => return None,
    };
    Some(if stage == "摘要" {
        format!("{attribute_label}（基础/装备/其他/最终）")
    } else {
        format!("{attribute_label}{stage}")
    })
}

fn ship_slot_label(field_key: &str) -> Result<Option<String>, WorkbookProbeError> {
    let Some(remainder) = field_key.strip_prefix("slot_") else {
        return Ok(None);
    };
    let Some((slot, suffix)) = remainder.split_once('_') else {
        return Ok(None);
    };
    if slot.is_empty() || !slot.bytes().all(|value| value.is_ascii_digit()) {
        return Ok(None);
    }
    if suffix == "equipment_name" {
        return Ok(Some(format!("槽位{slot}当前装备")));
    }
    if suffix == "target_equipment_family" {
        return Ok(Some(format!("槽位{slot}更换／处理")));
    }
    let suffix_label = suffix
        .split('_')
        .map(|token| {
            field_token_label(token).ok_or_else(|| {
                template_error(format!("槽位字段 {field_key} 包含未定义显示词元 {token}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("");
    Ok(Some(format!("槽位{slot}{suffix_label}")))
}

fn field_token_label(token: &str) -> Option<&'static str> {
    Some(match token {
        "acquire" => "获取",
        "operation" => "操作",
        "processing" => "处理",
        "action" => "处理",
        "actual" => "实际",
        "after" => "后",
        "aftercast" => "收尾等待",
        "aim" => "瞄准",
        "air" => "航空",
        "aircraft" => "飞机",
        "allocation" => "分配",
        "allowed" => "允许",
        "ammo" => "弹药",
        "angle" => "角度",
        "anti" => "反",
        "arguments" => "参数",
        "armor" => "装甲",
        "at" => "时间",
        "attack" => "攻击",
        "attribute" | "attributes" => "属性",
        "auto" => "自动",
        "auxiliary" => "辅助",
        "available" => "可用",
        "axis" => "轴向",
        "barrage" => "齐射",
        "base" => "基础",
        "before" => "前",
        "blueprint" => "设计图",
        "boost" => "加成",
        "bound" => "挂点",
        "bullet" => "子弹",
        "by" => "按",
        "cannon" => "炮击",
        "canonical" => "规范",
        "catalog" => "目录",
        "category" => "分类",
        "cell" => "单元格",
        "charge" => "锁定",
        "check" | "checked" => "检查",
        "choices" => "选择",
        "chunk" => "分块",
        "code" => "代码",
        "column" => "列",
        "combat" => "战斗",
        "compatible" => "兼容",
        "complete" => "完整",
        "compose" => "合成",
        "composed" => "已合成",
        "conditions" => "条件",
        "config" => "配置",
        "content" => "内容",
        "corrected" => "修正",
        "cost" | "costs" => "成本",
        "count" => "数量",
        "craftable" => "可合成",
        "created" => "创建",
        "current" => "当前",
        "damage" => "伤害",
        "data" => "数据",
        "default" => "默认",
        "delta" | "deltas" => "变化",
        "description" => "说明",
        "destroy" => "拆解",
        "device" => "设备",
        "dictionaries" => "字典",
        "dismantlable" => "可拆解",
        "dismantle" => "拆解",
        "dismantled" => "已拆解",
        "display" => "显示",
        "distribution" => "分布",
        "dodge" => "机动",
        "durability" => "耐久",
        "editor" => "编辑器",
        "effect" | "effects" => "效果",
        "effective" => "生效",
        "end" => "结束",
        "energy" => "心情",
        "enhance" => "强化",
        "entity" => "实体",
        "enum" => "枚举",
        "equipment" => "装备",
        "equipped" => "舰上",
        "error" | "errors" => "错误",
        "exact" => "指定",
        "executed" | "execution" => "执行",
        "expected" => "预期",
        "experience" => "经验",
        "expose" => "暴露",
        "family" => "族",
        "field" => "字段",
        "filter" => "筛选",
        "final" => "最终",
        "fire" => "开火",
        "fleet" => "编队",
        "forbidden" => "禁止",
        "format" => "格式",
        "formula" => "公式",
        "freeze" => "冻结",
        "fx" => "特效",
        "gear" => "装备",
        "generation" => "生成方式",
        "global" => "全局",
        "gold" => "资金",
        "group" => "组",
        "hash" => "哈希",
        "heat" => "热",
        "hidden" => "隐藏",
        "hit" => "命中",
        "hundredths" => "百分之一",
        "id" => "ID",
        "ids" => "ID 列表",
        "importance" => "重要度",
        "in" => "内",
        "index" => "序号",
        "initial" => "初始",
        "instance" => "实例",
        "intimacy" => "好感",
        "inventory" => "库存",
        "is" => "是否",
        "issue" => "问题",
        "item" => "物品",
        "items" => "项目",
        "json" => "JSON",
        "key" => "键",
        "label" | "labels" => "标签",
        "layout" => "布局",
        "learned" => "已学习",
        "level" => "等级",
        "limit" => "限制",
        "loadout" => "配装",
        "locked" => "锁定",
        "logical" => "逻辑",
        "loop" => "循环",
        "luck" => "幸运",
        "main" => "主力",
        "material" | "materials" => "材料",
        "max" | "maximum" => "上限",
        "message" => "信息",
        "metadata" => "元数据",
        "minimum" => "最小",
        "model" => "模型",
        "move" => "移动",
        "name" => "名称",
        "nation" => "阵营",
        "next" => "下一级",
        "note" => "备注",
        "object" => "对象",
        "oil" => "油耗",
        "omitted" => "省略",
        "only" => "仅",
        "order" => "顺序",
        "over" => "过",
        "owned" => "拥有",
        "oxygen" => "氧气",
        "parameter" | "parameters" => "参数",
        "path" => "路径",
        "plan" | "planned" => "计划",
        "policy" => "顺序",
        "potential" => "潜在",
        "power" => "增伤",
        "precast" => "预施法",
        "precondition" => "前置条件",
        "previous" => "上一级",
        "priority" => "优先级",
        "proficiency" => "熟练度",
        "propose" => "誓约",
        "proposed" => "已誓约",
        "protected" => "保护",
        "quantity" => "数量",
        "queue" => "队列",
        "range" => "射程",
        "rarity" => "稀有度",
        "ratio" => "比例",
        "raw" => "原始",
        "read" => "读取",
        "readback" => "回读",
        "recipe" | "recipes" => "配方",
        "recover" => "恢复",
        "ref" | "references" => "引用",
        "reload" => "装填",
        "remaining" => "剩余",
        "request" => "请求",
        "required" => "必需",
        "resource" => "资源",
        "response" => "响应",
        "restore" => "返还",
        "result" | "results" => "结果",
        "row" => "行",
        "runtime" => "运行时",
        "schema" => "结构",
        "score" => "评分",
        "search" => "搜索",
        "sequence" => "序号",
        "severity" => "严重度",
        "sfx" => "音效",
        "sha256" => "SHA-256",
        "shakescreen" => "屏幕震动",
        "sheet" => "工作表",
        "ship" | "ships" => "舰船",
        "simulated" => "模拟",
        "siren" => "塞壬",
        "skill" | "skills" => "技能",
        "skin" => "外观",
        "slot" => "槽位",
        "snapshot" => "快照",
        "source" => "来源",
        "spawn" => "发射",
        "speciality" => "特性",
        "speed" => "航速",
        "stable" => "稳定",
        "stage" => "阶段",
        "stars" => "星级",
        "start" => "初始",
        "state" | "status" => "状态",
        "step" | "steps" => "步骤",
        "structure" => "结构",
        "sub" => "潜艇",
        "summary" => "摘要",
        "static" => "静态",
        "suppress" => "压制",
        "target" => "目标",
        "tech" => "科技",
        "time" => "时间",
        "torpedo" => "鱼雷",
        "total" => "总计",
        "triggers" => "触发条件",
        "type" | "types" => "类型",
        "usage" => "占用",
        "value" => "值",
        "verification" => "校验",
        "verified" => "已校验",
        "version" => "版本",
        "visibility" => "显示范围",
        "warehouse" => "仓库",
        "weapon" | "weapons" => "武器",
        "width" => "宽度",
        "workbook" => "工作簿",
        "wrap" => "换行",
        "yield" | "yields" => "产出",
        _ => return None,
    })
}

#[derive(Clone, Copy)]
pub(super) struct EnumPresentation {
    pub(super) label: &'static str,
    pub(super) description: &'static str,
}

pub(super) fn enum_presentation(
    category: &str,
    value: &str,
) -> Result<EnumPresentation, WorkbookProbeError> {
    let presentation = match (category, value) {
        ("generation_mode", "visible") => EnumPresentation {
            label: "显示",
            description: "生成完整内容并显示工作表或字段。",
        },
        ("generation_mode", "hidden") => EnumPresentation {
            label: "隐藏",
            description: "生成完整内容但默认隐藏。",
        },
        ("generation_mode", "omitted") => EnumPresentation {
            label: "不生成",
            description: "不写入输出工作簿，仅允许可选只读数据使用。",
        },
        ("value_format", "text") => EnumPresentation {
            label: "文本",
            description: "保留字符串语义，不转换为数值。",
        },
        ("value_format", "integer") => EnumPresentation {
            label: "整数",
            description: "使用无小数位的数值格式。",
        },
        ("value_format", "decimal") => EnumPresentation {
            label: "小数",
            description: "使用允许小数位的数值格式。",
        },
        ("value_format", "percentage") => EnumPresentation {
            label: "百分比",
            description: "使用百分比数值格式。",
        },
        ("value_format", "date_time") => EnumPresentation {
            label: "日期时间",
            description: "使用可排序的日期时间格式。",
        },
        ("value_format", "json") => EnumPresentation {
            label: "JSON",
            description: "保存规范 JSON 文本。",
        },
        ("source_policy", "current_then_warehouse_then_compose_then_ship") => EnumPresentation {
            label: "当前→仓库→合成→舰船",
            description: "优先保留当前装备，再依次查找仓库、合成和其他舰船。",
        },
        ("inventory_operation", "keep") => EnumPresentation {
            label: "不处理",
            description: "本行或本槽不安排操作。",
        },
        ("inventory_operation", "enhance") => EnumPresentation {
            label: "强化",
            description: "按处理数量强化到目标等级。",
        },
        ("inventory_operation", "dismantle") => EnumPresentation {
            label: "拆解",
            description: "拆解指定处理数量。",
        },
        ("source_policy", "warehouse_then_compose") => EnumPresentation {
            label: "仓库→合成",
            description: "保留已满足目标的当前装备，否则先用仓库、再按需求合成，不从其他舰船取用。",
        },
        ("source_policy", "warehouse_then_compose_then_ship") => EnumPresentation {
            label: "仓库→合成→舰船",
            description: "依次查找仓库、合成和其他舰船。",
        },
        ("source_policy", "warehouse_then_ship_then_compose") => EnumPresentation {
            label: "仓库→舰船→合成",
            description: "依次查找仓库、其他舰船和合成来源。",
        },
        ("source_policy", "compose_then_warehouse_then_ship") => EnumPresentation {
            label: "合成→仓库→舰船",
            description: "依次查找合成、仓库和其他舰船。",
        },
        ("source_policy", "warehouse_only") => EnumPresentation {
            label: "仅仓库",
            description: "只允许使用仓库来源。",
        },
        ("source_policy", "compose_only") => EnumPresentation {
            label: "仅合成",
            description: "只允许新合成装备。",
        },
        ("source_policy", "ship_only") => EnumPresentation {
            label: "仅舰船",
            description: "只允许使用其他舰船槽位来源。",
        },
        ("source_policy", "exact_source") => EnumPresentation {
            label: "指定来源",
            description: "只使用用户填写的精确来源引用。",
        },
        ("equipment_source_type", "warehouse") => EnumPresentation {
            label: "仓库",
            description: "装备来源位于仓库聚合记录。",
        },
        ("equipment_source_type", "ship") => EnumPresentation {
            label: "舰船",
            description: "装备来源位于舰船槽位。",
        },
        ("equipment_source_type", "unowned") => EnumPresentation {
            label: "未持有",
            description: "该配置当前没有仓库或舰船来源，仅供查看。",
        },
        ("check_status", "passed") => EnumPresentation {
            label: "通过",
            description: "计划检查未发现阻塞问题。",
        },
        ("check_status", "failed") => EnumPresentation {
            label: "未通过",
            description: "计划检查发现阻塞问题。",
        },
        ("issue_severity", "error") => EnumPresentation {
            label: "错误",
            description: "问题会阻止计划执行。",
        },
        ("issue_severity", "warning") => EnumPresentation {
            label: "警告",
            description: "问题需要确认，但不必然阻止执行。",
        },
        ("execution_status", "success") => EnumPresentation {
            label: "成功",
            description: "步骤执行并通过回读校验。",
        },
        ("execution_status", "failed") => EnumPresentation {
            label: "失败",
            description: "步骤执行或回读校验失败。",
        },
        ("execution_status", "unknown") => EnumPresentation {
            label: "未知",
            description: "无法证明步骤的最终状态。",
        },
        ("execution_status", "not_executed") => EnumPresentation {
            label: "未执行",
            description: "步骤尚未开始执行。",
        },
        ("execution_report_status", "success") => EnumPresentation {
            label: "成功",
            description: "整份计划及独立终态均已核验。",
        },
        ("execution_report_status", "failed") => EnumPresentation {
            label: "失败",
            description: "计划中存在明确失败或状态不匹配。",
        },
        ("execution_report_status", "unknown") => EnumPresentation {
            label: "未知",
            description: "至少一条命令的设备端最终状态尚未确认。",
        },
        ("execution_report_status", "cancelled") => EnumPresentation {
            label: "已取消",
            description: "调用方在步骤边界停止了尚未完成的计划。",
        },
        ("execution_stop_reason", "completed") => EnumPresentation {
            label: "全部完成",
            description: "所有计划步骤都已处理。",
        },
        ("execution_stop_reason", "cancelled") => EnumPresentation {
            label: "已取消",
            description: "调用方在步骤边界请求停止。",
        },
        ("execution_stop_reason", "command_failed") => EnumPresentation {
            label: "命令失败",
            description: "命令在发送前失败或返回明确失败。",
        },
        ("execution_stop_reason", "command_unknown") => EnumPresentation {
            label: "命令未知",
            description: "命令可能已进入运行态，但设备端结果尚未确认。",
        },
        ("execution_stop_reason", "readback_failed") => EnumPresentation {
            label: "回读失败",
            description: "命令已确认成功，但步骤后的完整状态读取失败。",
        },
        ("execution_stop_reason", "readback_mismatch") => EnumPresentation {
            label: "回读不匹配",
            description: "步骤后的完整状态不符合模拟结果。",
        },
        ("execution_stop_reason", "final_state_mismatch") => EnumPresentation {
            label: "终态不匹配",
            description: "独立终态与计划的完整预期不一致。",
        },
        ("execution_stop_reason", "final_readback_failed") => EnumPresentation {
            label: "终态读取失败",
            description: "步骤结束后无法取得独立完整状态。",
        },
        ("execution_write_effect", "none") => EnumPresentation {
            label: "无写入",
            description: "该步骤没有发送命令，或端口确认命令未发送。",
        },
        ("execution_write_effect", "possible") => EnumPresentation {
            label: "可能写入",
            description: "当前证据不能排除命令已经或稍后产生写入。",
        },
        ("execution_write_effect", "state_changed_mismatch") => EnumPresentation {
            label: "变化不匹配",
            description: "已观察到状态变化，但不符合该步骤的完整预期。",
        },
        ("execution_write_effect", "expected_post_state_observed") => EnumPresentation {
            label: "已观察预期后态",
            description: "命令回执未知，但独立读取看到了步骤预期后态。",
        },
        ("execution_write_effect", "verified") => EnumPresentation {
            label: "已核验",
            description: "命令已确认成功，且完整回读符合步骤预期。",
        },
        ("execution_final_verification_status", "verified") => EnumPresentation {
            label: "已核验",
            description: "独立终态与完整计划预期一致。",
        },
        ("execution_final_verification_status", "mismatch") => EnumPresentation {
            label: "不匹配",
            description: "独立终态与可确认的预期状态不一致。",
        },
        ("execution_final_verification_status", "incomplete") => EnumPresentation {
            label: "未完成",
            description: "计划提前停止，终态只核对到已确认进度。",
        },
        ("execution_final_verification_status", "unavailable") => EnumPresentation {
            label: "不可用",
            description: "无法取得独立终态。",
        },
        ("execution_final_verification_status", "unconfirmed_step_reached") => EnumPresentation {
            label: "未知步骤后态已观察",
            description: "命令回执仍未知，但独立终态符合该步骤预期后态。",
        },
        ("execution_final_verification_status", "unconfirmed_step_not_observed") => {
            EnumPresentation {
                label: "未知步骤尚未观察",
                description: "命令回执仍未知，独立终态仍与步骤前态一致。",
            }
        }
        _ => {
            return Err(template_error(format!(
                "枚举 {category}.{value} 缺少默认显示定义"
            )));
        }
    };
    Ok(presentation)
}

#[derive(Clone, Copy)]
pub(super) struct StylePresentation {
    pub(super) background: &'static str,
    pub(super) font: &'static str,
    pub(super) bold: bool,
    pub(super) horizontal: &'static str,
    pub(super) vertical: &'static str,
    pub(super) wrap: bool,
    pub(super) description: &'static str,
}

pub(super) fn style_presentation(key: &str) -> Result<StylePresentation, WorkbookProbeError> {
    let presentation = match key {
        "read_only" => StylePresentation {
            background: "F2F2F2",
            font: "000000",
            bold: false,
            horizontal: "中",
            vertical: "中",
            wrap: false,
            description: "程序生成且不允许用户修改的数据。",
        },
        "input" => StylePresentation {
            background: "E2F0D9",
            font: "000000",
            bold: false,
            horizontal: "中",
            vertical: "中",
            wrap: true,
            description: "允许用户填写或选择的输入单元格。",
        },
        "current_state" => StylePresentation {
            background: "F2F2F2",
            font: "000000",
            bold: false,
            horizontal: "中",
            vertical: "中",
            wrap: false,
            description: "随独立实际状态刷新且不允许用户直接修改的数据。",
        },
        "unowned" => StylePresentation {
            background: "DDEBF7",
            font: "000000",
            bold: false,
            horizontal: "中",
            vertical: "中",
            wrap: true,
            description: "当前未持有且仅供查看的装备配置。",
        },
        "warning" => StylePresentation {
            background: "FCE4D6",
            font: "9C5700",
            bold: true,
            horizontal: "中",
            vertical: "中",
            wrap: true,
            description: "需要用户确认但不必然阻止执行的问题。",
        },
        "error" => StylePresentation {
            background: "FFC7CE",
            font: "9C0006",
            bold: true,
            horizontal: "中",
            vertical: "中",
            wrap: true,
            description: "阻止检查通过或执行继续的错误。",
        },
        _ => return Err(template_error(format!("样式 {key} 缺少默认显示定义"))),
    };
    Ok(presentation)
}

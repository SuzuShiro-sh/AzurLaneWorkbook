//! 负责从每舰一行的配装总表解析五个槽位输入和装备来源。

use std::collections::BTreeMap;

use calamine::Data;

use super::{
    LOADOUT_SHEET_KEY, WorkbookPlanError, invalid_cell, optional_enhance_level, optional_enum,
    optional_priority, optional_text, parse_u8, parse_u64, require_blank, required_text_by_key,
};
use crate::application::WorkbookLayout;
use crate::domain::{
    DesiredEquipment, DesiredSlotState, EquipmentConfigId, EquipmentFamilyId, EquipmentSourceRef,
    ShipInstanceId, ShipSlotRef, SlotIndex, SlotTarget, SourcePolicy,
};

pub(super) fn parse_loadout_row(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    layout: &WorkbookLayout,
) -> Result<Option<(ShipInstanceId, Vec<DesiredSlotState>)>, WorkbookPlanError> {
    let instance = optional_text(range, row, columns, "instance_id")?;
    let source_type = optional_text(range, row, columns, "source_type")?;
    let source_ref = optional_text(range, row, columns, "source_ref")?;
    let (expected_type, expected_ref) = if let Some(instance) = &instance {
        let id = crate::domain::ShipInstanceId::new(parse_u64(instance, row, "instance_id")?)
            .map_err(|source| WorkbookPlanError::Model { source })?;
        ("owned", Some(format!("owned:{}", id.get())))
    } else {
        // 未持有行不参与配装；仅校验布局实际生成的身份字段。
        let reference = if columns.contains_key("group_id") {
            let group = required_text_by_key(range, row, columns, "group_id")?;
            Some(format!("unowned:{}", parse_u64(&group, row, "group_id")?))
        } else {
            source_ref.clone()
        };
        ("unowned", reference)
    };
    if source_type
        .as_deref()
        .is_some_and(|value| value != expected_type)
        || source_ref
            .as_deref()
            .is_some_and(|value| Some(value) != expected_ref.as_deref())
    {
        return Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            "source_ref",
            "来源类型和引用必须与实例ID或舰船组ID一致",
        ));
    }
    if expected_type == "unowned" {
        validate_unowned_row(range, row, columns, expected_ref.as_deref())?;
        return Ok(None);
    }
    let ship_instance_id = parse_ship_instance_id(range, row, columns)?;
    let mut desired_slots = Vec::new();
    for slot in SlotIndex::MIN..=SlotIndex::MAX {
        if let Some(desired) = parse_loadout_slot(
            range,
            row,
            columns,
            layout,
            ship_instance_id,
            SlotIndex::new(slot).expect("固定槽位范围必须有效"),
        )? {
            desired_slots.push(desired);
        }
    }
    Ok(Some((ship_instance_id, desired_slots)))
}

fn validate_unowned_row(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    source_ref: Option<&str>,
) -> Result<(), WorkbookPlanError> {
    if let Some(source_ref) = source_ref {
        let group_token = source_ref.strip_prefix("unowned:").ok_or_else(|| {
            invalid_cell(
                LOADOUT_SHEET_KEY,
                row,
                "source_ref",
                "未持有舰船来源引用必须使用 unowned:舰船组ID",
            )
        })?;
        let group_id = parse_u64(group_token, row, "source_ref")?;
        if group_id == 0 {
            return Err(invalid_cell(
                LOADOUT_SHEET_KEY,
                row,
                "source_ref",
                "未持有舰船组 ID 必须大于零",
            ));
        }
        if columns.contains_key("group_id") {
            let row_group_id = required_text_by_key(range, row, columns, "group_id")?;
            if parse_u64(&row_group_id, row, "group_id")? != group_id {
                return Err(invalid_cell(
                    LOADOUT_SHEET_KEY,
                    row,
                    "group_id",
                    "未持有舰船组 ID 必须与 source_ref 一致",
                ));
            }
        }
    }

    for field in [
        "instance_id",
        "skin_id",
        "fleet_status",
        "intimacy_stage",
        "read_errors",
        "current_stars",
        "level",
        "experience_in_level",
        "total_experience",
        "next_level_experience",
        "energy",
        "proficiency",
        "intimacy",
        "intimacy_maximum",
        "propose_time",
        "create_time",
        "combat_power",
        "oil_start",
        "oil_end",
        "oil_total",
        "learned_skill_count",
        "locked",
        "proposed",
        "data_complete",
        "skills_level",
        "skills_experience",
        "skills_next_level_experience",
    ] {
        require_unowned_blank(range, row, columns, field)?;
    }
    for attribute in [
        "durability",
        "cannon",
        "torpedo",
        "air",
        "reload",
        "anti_aircraft",
        "hit",
        "dodge",
        "anti_sub",
        "luck",
        "speed",
    ] {
        for stage in [
            "base",
            "equipment_delta",
            "global_delta",
            "final",
            "summary",
        ] {
            require_unowned_blank(range, row, columns, &format!("stat_{attribute}_{stage}"))?;
        }
    }
    for slot in SlotIndex::MIN..=SlotIndex::MAX {
        let prefix = format!("slot_{slot}");
        for suffix in [
            "equipment_name",
            "runtime_id",
            "config_id",
            "family_id",
            "enhance_level",
            "effect_summary",
        ] {
            require_unowned_blank(range, row, columns, &format!("{prefix}_{suffix}"))?;
        }
        for suffix in [
            "target_equipment_family",
            "source_policy",
            "exact_source",
            "target_enhance_level",
            "allocation_priority",
            "note",
        ] {
            require_unowned_blank(range, row, columns, &format!("{prefix}_{suffix}"))?;
        }
    }
    Ok(())
}

fn require_unowned_blank(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<(), WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(());
    };
    match range.get_value((row - 1, column)) {
        None | Some(Data::Empty) => Ok(()),
        Some(Data::String(value)) if value.is_empty() => Ok(()),
        _ => Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            "未持有舰船的当前值和操作输入必须为空",
        )),
    }
}

fn parse_loadout_slot(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    layout: &WorkbookLayout,
    ship_instance_id: ShipInstanceId,
    slot_index: SlotIndex,
) -> Result<Option<DesiredSlotState>, WorkbookPlanError> {
    let prefix = format!("slot_{}", slot_index.get());
    let target_family_field = format!("{prefix}_target_equipment_family");
    let source_policy_field = format!("{prefix}_source_policy");
    let exact_source_field = format!("{prefix}_exact_source");
    let target_enhance_level_field = format!("{prefix}_target_enhance_level");
    let allocation_priority_field = format!("{prefix}_allocation_priority");
    let choice = optional_text(range, row, columns, &target_family_field)?;
    let Some(choice) = choice else {
        for suffix in [
            "target_enhance_level",
            "source_policy",
            "exact_source",
            "allocation_priority",
        ] {
            let field = format!("{prefix}_{suffix}");
            if let Some(&column) = columns.get(&field) {
                match range.get_value((row - 1, column)) {
                    None | Some(Data::Empty) => {}
                    Some(Data::String(value)) if value.is_empty() => {}
                    _ => {
                        return Err(invalid_cell(
                            LOADOUT_SHEET_KEY,
                            row,
                            &target_family_field,
                            "填写目标或来源时必须选择装备或操作",
                        ));
                    }
                }
            }
        }
        return Ok(None);
    };
    let empty = matches!(choice.as_str(), "卸下" | "拆解");
    let source_policy = optional_enum(
        range,
        row,
        columns,
        layout,
        &source_policy_field,
        "source_policy",
    )?;
    let exact_source_text = optional_text(range, row, columns, &exact_source_field)?;
    let target_enhance_level =
        optional_enhance_level(range, row, columns, &target_enhance_level_field)?;
    let allocation_priority = optional_priority(range, row, columns, &allocation_priority_field)?;
    let ship_slot = ShipSlotRef::new(ship_instance_id, slot_index);
    if empty {
        require_blank(row, &source_policy_field, source_policy.as_deref())?;
        require_blank(row, &exact_source_field, exact_source_text.as_deref())?;
        if target_enhance_level.is_some() {
            return Err(invalid_cell(
                LOADOUT_SHEET_KEY,
                row,
                &target_enhance_level_field,
                "最终状态为空槽时不能填写目标强化等级",
            ));
        }
        if allocation_priority.is_some() {
            return Err(invalid_cell(
                LOADOUT_SHEET_KEY,
                row,
                &allocation_priority_field,
                "最终状态为空槽时不能填写分配优先级",
            ));
        }
        return Ok(Some(DesiredSlotState::new(ship_slot, SlotTarget::Empty, 0)));
    }

    let (family_token, choice_source) = parse_equipment_choice(&choice, row, &target_family_field)?;
    let family_id = if choice_source.is_some() {
        EquipmentFamilyId::new(parse_u64(family_token, row, &target_family_field)?).map_err(
            |source| {
                invalid_cell(
                    LOADOUT_SHEET_KEY,
                    row,
                    &target_family_field,
                    source.to_string(),
                )
            },
        )?
    } else {
        parse_family_id(family_token, row, &target_family_field)?
    };
    if choice_source.is_some() {
        require_blank(row, &source_policy_field, source_policy.as_deref())?;
        require_blank(row, &exact_source_field, exact_source_text.as_deref())?;
    }
    let policy = if let Some(source) = choice_source {
        if source == "compose" {
            SourcePolicy::ComposeOnly
        } else {
            SourcePolicy::ExactSource
        }
    } else if let Some(policy_text) = source_policy {
        source_policy_from_stable(&policy_text).ok_or_else(|| {
            invalid_cell(
                LOADOUT_SHEET_KEY,
                row,
                &source_policy_field,
                format!("来源顺序稳定值 {policy_text} 不受支持"),
            )
        })?
    } else {
        SourcePolicy::WarehouseThenCompose
    };
    let exact_source = choice_source
        .filter(|source| *source != "compose")
        .or(exact_source_text.as_deref())
        .map(|value| {
            if choice_source.is_some() {
                parse_source_token(value, row, &exact_source_field)
            } else {
                parse_exact_source(value, row, &exact_source_field)
            }
        })
        .transpose()?;
    if policy.requires_exact_source() && exact_source.is_none() {
        return Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            &exact_source_field,
            "指定来源顺序必须填写精确来源",
        ));
    }
    if !policy.requires_exact_source() && exact_source.is_some() {
        return Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            &exact_source_field,
            "非指定来源顺序不能填写精确来源",
        ));
    }
    let priority = allocation_priority.unwrap_or(0);
    if choice_source.is_some()
        && exact_source == Some(EquipmentSourceRef::ShipSlot(ship_slot))
        && target_enhance_level.is_none()
    {
        return Ok(Some(DesiredSlotState::new(
            ship_slot,
            SlotTarget::Keep,
            priority,
        )));
    }
    let equipment = DesiredEquipment::new(family_id, policy, exact_source, target_enhance_level)
        .map_err(|source| WorkbookPlanError::Model { source })?;
    Ok(Some(DesiredSlotState::new(
        ship_slot,
        SlotTarget::Equipment(equipment),
        priority,
    )))
}

/// 从舰船公共身份列解析当前行对应的真实舰船实例。
fn parse_ship_instance_id(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
) -> Result<ShipInstanceId, WorkbookPlanError> {
    let field = "instance_id";
    let ship_text = required_text_by_key(range, row, columns, field)?;
    let ship_value = parse_u64(&ship_text, row, field)?;
    ShipInstanceId::new(ship_value)
        .map_err(|source| invalid_cell(LOADOUT_SHEET_KEY, row, field, source.to_string()))
}

/// 来源标识与显示数量分离；执行时按最新游戏状态校验数量和占用。
fn parse_equipment_choice<'a>(
    value: &'a str,
    row: u32,
    field: &str,
) -> Result<(&'a str, Option<&'a str>), WorkbookPlanError> {
    if let Some((_, suffix)) = value.rsplit_once('〔')
        && let Some((family, source)) = suffix
            .strip_suffix('〕')
            .and_then(|token| token.split_once('|'))
    {
        if source != "compose" {
            parse_source_token(source, row, field)?;
        }
        return Ok((family, Some(source)));
    }
    Ok((value, None))
}

fn parse_family_id(
    value: &str,
    row: u32,
    field: &str,
) -> Result<EquipmentFamilyId, WorkbookPlanError> {
    let token = if let Some((_, suffix)) = value.rsplit_once('〔') {
        suffix
            .strip_suffix('〕')
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                invalid_cell(
                    LOADOUT_SHEET_KEY,
                    row,
                    field,
                    "装备稳定标识必须使用名称〔族ID〕",
                )
            })?
    } else {
        bracket_token(value, row, field)?
    };
    let id = parse_u64(token, row, field)?;
    EquipmentFamilyId::new(id)
        .map_err(|source| invalid_cell(LOADOUT_SHEET_KEY, row, field, source.to_string()))
}

fn parse_exact_source(
    value: &str,
    row: u32,
    field: &str,
) -> Result<EquipmentSourceRef, WorkbookPlanError> {
    parse_source_token(bracket_token(value, row, field)?, row, field)
}

fn parse_source_token(
    token: &str,
    row: u32,
    field: &str,
) -> Result<EquipmentSourceRef, WorkbookPlanError> {
    let parts: Vec<&str> = token.split(':').collect();
    match parts.as_slice() {
        ["warehouse", config] => {
            let config_id =
                EquipmentConfigId::new(parse_u64(config, row, field)?).map_err(|source| {
                    invalid_cell(LOADOUT_SHEET_KEY, row, field, source.to_string())
                })?;
            Ok(EquipmentSourceRef::Warehouse(config_id))
        }
        ["ship", ship, slot] => {
            let ship_id = ShipInstanceId::new(parse_u64(ship, row, field)?).map_err(|source| {
                invalid_cell(LOADOUT_SHEET_KEY, row, field, source.to_string())
            })?;
            let slot_index = SlotIndex::new(parse_u8(slot, row, field)?).map_err(|source| {
                invalid_cell(LOADOUT_SHEET_KEY, row, field, source.to_string())
            })?;
            Ok(EquipmentSourceRef::ShipSlot(ShipSlotRef::new(
                ship_id, slot_index,
            )))
        }
        _ => Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            "精确来源必须使用 [warehouse:配置ID] 或 [ship:舰船实例ID:槽位] 格式",
        )),
    }
}

fn bracket_token<'a>(value: &'a str, row: u32, field: &str) -> Result<&'a str, WorkbookPlanError> {
    let rest = value.strip_prefix('[').ok_or_else(|| {
        invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            "值必须以方括号中的稳定引用开头",
        )
    })?;
    let (token, _) = rest
        .split_once(']')
        .ok_or_else(|| invalid_cell(LOADOUT_SHEET_KEY, row, field, "值缺少稳定引用结束括号"))?;
    if token.is_empty() {
        return Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            "稳定引用不能为空",
        ));
    }
    Ok(token)
}

fn source_policy_from_stable(value: &str) -> Option<SourcePolicy> {
    match value {
        "current_then_warehouse_then_compose_then_ship" => {
            Some(SourcePolicy::CurrentThenWarehouseThenComposeThenShip)
        }
        "warehouse_then_compose" => Some(SourcePolicy::WarehouseThenCompose),
        "warehouse_then_compose_then_ship" => Some(SourcePolicy::WarehouseThenComposeThenShip),
        "warehouse_then_ship_then_compose" => Some(SourcePolicy::WarehouseThenShipThenCompose),
        "compose_then_warehouse_then_ship" => Some(SourcePolicy::ComposeThenWarehouseThenShip),
        "warehouse_only" => Some(SourcePolicy::WarehouseOnly),
        "compose_only" => Some(SourcePolicy::ComposeOnly),
        "ship_only" => Some(SourcePolicy::ShipOnly),
        "exact_source" => Some(SourcePolicy::ExactSource),
        _ => None,
    }
}

/// 将槽位拆解输入转换为现有库存动作，由计划编译器检查来源与拆解条件。
pub(super) fn parse_slot_inventory_action(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    ship: ShipInstanceId,
    slot: SlotIndex,
) -> Result<Option<crate::domain::EquipmentInventoryAction>, WorkbookPlanError> {
    let prefix = format!("slot_{}", slot.get());
    if optional_text(
        range,
        row,
        columns,
        &format!("{prefix}_target_equipment_family"),
    )?
    .as_deref()
        == Some("拆解")
    {
        return crate::domain::EquipmentInventoryAction::new(
            EquipmentSourceRef::ShipSlot(ShipSlotRef::new(ship, slot)),
            crate::domain::EquipmentInventoryActionKind::Dismantle,
            Some(1),
            None,
            None,
        )
        .map(Some)
        .map_err(|source| WorkbookPlanError::InventoryModel { source });
    }
    Ok(None)
}

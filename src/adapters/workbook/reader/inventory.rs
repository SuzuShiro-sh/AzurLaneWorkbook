//! 负责从装备库存工作表解析库存动作、来源和数量约束。

use std::collections::BTreeMap;

use calamine::Data;

use super::{
    INVENTORY_SHEET_KEY, WorkbookPlanError, invalid_cell, optional_integer_at, optional_text_at,
    required_column, required_integer, required_text,
};
use crate::application::WorkbookLayout;
use crate::domain::{
    EnhanceLevel, EquipmentConfigId, EquipmentFamilyId, EquipmentInventoryAction,
    EquipmentInventoryActionKind, EquipmentSourceRef, ShipInstanceId, ShipSlotRef, SlotIndex,
};

pub(super) fn parse_inventory_row(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    layout: &WorkbookLayout,
) -> Result<Option<EquipmentInventoryAction>, WorkbookPlanError> {
    let source = parse_inventory_source(range, row, columns)?;
    let operation_text = optional_inventory_text(range, row, columns, "operation")?;
    let operation = operation_text
        .as_deref()
        .map(|label| {
            layout
                .enum_options()
                .iter()
                .find(|option| {
                    option.category_key() == "inventory_operation" && option.label() == label
                })
                .map(|option| option.stable_value())
                .ok_or_else(|| {
                    invalid_cell(INVENTORY_SHEET_KEY, row, "operation", "未知的装备操作")
                })
        })
        .transpose()?;
    let compact_quantity = optional_inventory_integer(range, row, columns, "processing_quantity")?
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "processing_quantity",
                    "处理数量必须是非负整数",
                )
            })
        })
        .transpose()?;
    if operation.is_none() && compact_quantity.is_some() {
        return Err(invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            "operation",
            "填写处理数量时必须选择操作",
        ));
    }
    let kind = if operation == Some("dismantle") {
        EquipmentInventoryActionKind::Dismantle
    } else {
        EquipmentInventoryActionKind::Keep
    };
    if matches!(kind, EquipmentInventoryActionKind::Dismantle) {
        let locked = columns.contains_key("locked")
            && required_inventory_boolean(range, row, columns, "locked")?;
        let protected = columns.contains_key("protected")
            && required_inventory_boolean(range, row, columns, "protected")?;
        let dismantlable =
            match optional_inventory_text(range, row, columns, "dismantlable")?.as_deref() {
                Some("可拆解") | None => true,
                Some(text) if text.starts_with("不可拆解") => false,
                _ => required_inventory_boolean(range, row, columns, "dismantlable")?,
            };
        let data_complete = !columns.contains_key("data_complete")
            || required_inventory_boolean(range, row, columns, "data_complete")?;
        if locked {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "locked",
                "锁定装备不能拆解",
            ));
        }
        if protected {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "protected",
                "受保护装备不能拆解",
            ));
        }
        if !dismantlable {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "dismantlable",
                "当前来源未被运行态标记为可拆解",
            ));
        }
        if !data_complete {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "data_complete",
                "拆解需要完整的来源和产物数据",
            ));
        }
    }
    let mut dismantle_quantity = None;
    let target_enhance_level = optional_inventory_enhance_level(range, row, columns)?;
    let mut enhance_quantity = None;
    if operation.is_none() && target_enhance_level.is_some() {
        return Err(invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            "operation",
            "填写目标强化等级时必须选择操作",
        ));
    }
    if let Some(operation) = operation {
        match operation {
            "enhance" => {
                if compact_quantity.is_none() || target_enhance_level.is_none() {
                    return Err(invalid_cell(
                        INVENTORY_SHEET_KEY,
                        row,
                        "operation",
                        "强化必须填写处理数量和目标强化等级",
                    ));
                }
                enhance_quantity = compact_quantity;
            }
            "dismantle" => dismantle_quantity = compact_quantity,
            "keep" => {
                if compact_quantity.is_some() || target_enhance_level.is_some() {
                    return Err(invalid_cell(
                        INVENTORY_SHEET_KEY,
                        row,
                        "operation",
                        "不处理时数量和目标等级必须留空",
                    ));
                }
            }
            _ => unreachable!("操作已经校验"),
        }
    }
    if matches!(source, ParsedInventorySource::Unowned) {
        let note = optional_inventory_text(range, row, columns, "note")?;
        if !matches!(kind, EquipmentInventoryActionKind::Keep)
            || dismantle_quantity.is_some()
            || target_enhance_level.is_some()
            || enhance_quantity.is_some()
            || note.is_some()
        {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "source_ref",
                "未持有配置行仅供查看，所有处理输入必须保持为空或保持",
            ));
        }
        return Ok(None);
    }
    let ParsedInventorySource::Owned {
        source,
        quantity,
        is_ship_source,
    } = source
    else {
        unreachable!("未持有配置已在上方返回");
    };

    if matches!(kind, EquipmentInventoryActionKind::Keep)
        && dismantle_quantity.is_none()
        && target_enhance_level.is_none()
        && enhance_quantity.is_none()
    {
        return Ok(None);
    }
    if let Some(value) = dismantle_quantity {
        if value > quantity {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "processing_quantity",
                format!("拆解数量 {value} 不能超过当前数量 {quantity}"),
            ));
        }
        if is_ship_source && value != 1 {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "processing_quantity",
                "舰船来源的拆解数量只能为 1",
            ));
        }
    }
    if let Some(value) = enhance_quantity {
        if value > quantity {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "processing_quantity",
                format!("强化数量 {value} 不能超过当前数量 {quantity}"),
            ));
        }
        if is_ship_source && value != 1 {
            return Err(invalid_cell(
                INVENTORY_SHEET_KEY,
                row,
                "processing_quantity",
                "舰船来源的强化数量只能为 1",
            ));
        }
    }

    EquipmentInventoryAction::new(
        source,
        kind,
        dismantle_quantity,
        target_enhance_level,
        enhance_quantity,
    )
    .map(Some)
    .map_err(|source| WorkbookPlanError::InventoryModel { source })
}

/// 读取并交叉核对装备库存行的稳定来源引用、来源类型和只读数量。
enum ParsedInventorySource {
    Owned {
        source: EquipmentSourceRef,
        quantity: u64,
        is_ship_source: bool,
    },
    Unowned,
}

fn parse_inventory_source(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
) -> Result<ParsedInventorySource, WorkbookPlanError> {
    let source_type = required_inventory_text(range, row, columns, "source_type")?;
    let source_type = match source_type.as_str() {
        "仓库" => "warehouse",
        "舰船" => "ship",
        "未持有" => "unowned",
        other => other,
    };
    let config_id_text = required_inventory_text(range, row, columns, "config_id")?;
    let config_id = EquipmentConfigId::new(parse_inventory_u64(&config_id_text, row, "config_id")?)
        .map_err(|source| WorkbookPlanError::Model { source })?;
    if let Some(family_id_text) = optional_inventory_text(range, row, columns, "family_id")? {
        EquipmentFamilyId::new(parse_inventory_u64(&family_id_text, row, "family_id")?)
            .map_err(|source| WorkbookPlanError::Model { source })?;
    }
    let source_ref = if columns.contains_key("source_ref") {
        required_inventory_text(range, row, columns, "source_ref")?
    } else if source_type == "ship" {
        let ship = required_inventory_text(range, row, columns, "ship_instance_id")?;
        let slot = required_inventory_integer(range, row, columns, "slot_index")?;
        format!("ship:{ship}:{slot}")
    } else {
        format!("{source_type}:{config_id}")
    };
    let quantity = required_inventory_integer(range, row, columns, "quantity")?;
    let quantity = u64::try_from(quantity).map_err(|_| {
        invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            "quantity",
            "数量必须是大于 0 的整数",
        )
    })?;
    let parts: Vec<&str> = source_ref.split(':').collect();
    match parts.as_slice() {
        ["warehouse", config] => {
            if quantity == 0 {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "quantity",
                    "仓库来源的数量必须是大于 0 的整数",
                ));
            }
            if source_type != "warehouse" {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "source_type",
                    "仓库来源的来源类型必须为 warehouse",
                ));
            }
            let source_config =
                EquipmentConfigId::new(parse_inventory_u64(config, row, "source_ref")?)
                    .map_err(|source| WorkbookPlanError::Model { source })?;
            if source_config != config_id {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "config_id",
                    format!("必须与来源引用中的配置 ID {source_config} 一致"),
                ));
            }
            if optional_inventory_text(range, row, columns, "ship_instance_id")?.is_some()
                || optional_inventory_integer(range, row, columns, "slot_index")?.is_some()
            {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "source_ref",
                    "仓库来源不能填写舰船实例或槽位",
                ));
            }
            Ok(ParsedInventorySource::Owned {
                source: EquipmentSourceRef::Warehouse(config_id),
                quantity,
                is_ship_source: false,
            })
        }
        ["ship", ship, slot] => {
            if source_type != "ship" {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "source_type",
                    "舰船来源的来源类型必须为 ship",
                ));
            }
            let ship_instance_id =
                ShipInstanceId::new(parse_inventory_u64(ship, row, "source_ref")?)
                    .map_err(|source| WorkbookPlanError::Model { source })?;
            let slot_index = SlotIndex::new(parse_inventory_u8(slot, row, "source_ref")?)
                .map_err(|source| WorkbookPlanError::Model { source })?;
            let ship_cell = required_inventory_text(range, row, columns, "ship_instance_id")?;
            if ship_cell != *ship {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "ship_instance_id",
                    format!("必须与来源引用中的舰船实例 ID {ship} 一致"),
                ));
            }
            let slot_cell = required_inventory_integer(range, row, columns, "slot_index")?;
            if slot_cell != i64::from(slot_index.get()) {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "slot_index",
                    format!("必须与来源引用中的槽位 {} 一致", slot_index.get()),
                ));
            }
            if quantity != 1 {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "quantity",
                    "舰船来源的数量必须为 1",
                ));
            }
            Ok(ParsedInventorySource::Owned {
                source: EquipmentSourceRef::ShipSlot(ShipSlotRef::new(
                    ship_instance_id,
                    slot_index,
                )),
                quantity,
                is_ship_source: true,
            })
        }
        ["unowned", config] => {
            if source_type != "unowned" {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "source_type",
                    "未持有配置行的来源类型必须为 unowned",
                ));
            }
            let source_config =
                EquipmentConfigId::new(parse_inventory_u64(config, row, "source_ref")?)
                    .map_err(|source| WorkbookPlanError::Model { source })?;
            if source_config != config_id {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "config_id",
                    format!("必须与未持有引用中的配置 ID {source_config} 一致"),
                ));
            }
            if quantity != 0 {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "quantity",
                    "未持有配置行的数量必须为 0",
                ));
            }
            for field in ["runtime_id", "ship_instance_id", "ship_name"] {
                if optional_inventory_text(range, row, columns, field)?.is_some() {
                    return Err(invalid_cell(
                        INVENTORY_SHEET_KEY,
                        row,
                        field,
                        "未持有配置行不能声明实际来源身份",
                    ));
                }
            }
            if optional_inventory_integer(range, row, columns, "slot_index")?.is_some() {
                return Err(invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "slot_index",
                    "未持有配置行不能声明实际槽位",
                ));
            }
            Ok(ParsedInventorySource::Unowned)
        }
        _ => Err(invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            "source_ref",
            "来源引用必须使用 warehouse:配置ID、ship:舰船实例ID:槽位或 unowned:配置ID 格式",
        )),
    }
}

fn required_inventory_text(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<String, WorkbookPlanError> {
    let column = required_column(columns, INVENTORY_SHEET_KEY, field, row)?;
    required_text(range, row - 1, column, INVENTORY_SHEET_KEY, row, field)
}

fn optional_inventory_text(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<Option<String>, WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(None);
    };
    optional_text_at(range, row - 1, column, INVENTORY_SHEET_KEY, row, field)
}

fn required_inventory_integer(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<i64, WorkbookPlanError> {
    let column = required_column(columns, INVENTORY_SHEET_KEY, field, row)?;
    required_integer(range, row - 1, column, INVENTORY_SHEET_KEY, row, field)
}

fn optional_inventory_integer(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<Option<i64>, WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(None);
    };
    optional_integer_at(range, row - 1, column, INVENTORY_SHEET_KEY, row, field)
}

fn required_inventory_boolean(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<bool, WorkbookPlanError> {
    let value = required_inventory_text(range, row, columns, field)?;
    match value.as_str() {
        "是" => Ok(true),
        "否" => Ok(false),
        _ => Err(invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            field,
            "布尔值只能填写 是 或 否",
        )),
    }
}

fn optional_inventory_enhance_level(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
) -> Result<Option<EnhanceLevel>, WorkbookPlanError> {
    let value = optional_inventory_integer(range, row, columns, "target_enhance_level")?;
    value
        .map(|value| {
            u8::try_from(value).map(EnhanceLevel::new).map_err(|_| {
                invalid_cell(
                    INVENTORY_SHEET_KEY,
                    row,
                    "target_enhance_level",
                    "强化等级必须在 0 至 255 之间",
                )
            })
        })
        .transpose()
}

fn parse_inventory_u64(value: &str, row: u32, field: &str) -> Result<u64, WorkbookPlanError> {
    value.parse::<u64>().map_err(|_| {
        invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            field,
            format!("{value:?} 不是非负整数"),
        )
    })
}

fn parse_inventory_u8(value: &str, row: u32, field: &str) -> Result<u8, WorkbookPlanError> {
    value.parse::<u8>().map_err(|_| {
        invalid_cell(
            INVENTORY_SHEET_KEY,
            row,
            field,
            format!("{value:?} 不是 0 至 255 的整数"),
        )
    })
}

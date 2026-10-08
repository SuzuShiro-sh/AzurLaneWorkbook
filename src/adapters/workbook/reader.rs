//! 从当前布局快照读取配装计划和装备库存编辑列。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;

use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook_from_rs};
use thiserror::Error;

use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::document::WorkbookDocument;
use crate::adapters::workbook::package::{MAX_RAW_PACKAGE_BYTES, read_bounded_workbook_bytes};
use crate::adapters::workbook::projection_writer::reject_cell_formulas;
use crate::application::{
    AppError, AppErrorCode, LayoutGenerationMode, WORKBOOK_PROJECTION_SCHEMA_VERSION,
    WorkbookFieldLayout, WorkbookLayout, WorkbookPlanInputs,
};
use crate::domain::{
    DesiredSlotState, DesiredState, EnhanceLevel, EquipmentInventoryAction,
    EquipmentInventoryActionError, EquipmentInventoryPlan, LoadoutModelError,
};

mod inventory;
mod loadout;

use inventory::parse_inventory_row;
use loadout::{parse_loadout_row, parse_slot_inventory_action};

const LOADOUT_SHEET_KEY: &str = "loadout_plan";
const INVENTORY_SHEET_KEY: &str = "equipment_inventory";
const SCHEMA_SHEET_KEY: &str = "schema";

/// 读取工作簿计划输入时发现的结构或用户值错误。
#[derive(Debug, Error)]
pub(crate) enum WorkbookPlanError {
    /// 工作簿包、OOXML 或 Calamine 读取失败。
    #[error(transparent)]
    Probe(#[from] WorkbookProbeError),
    /// 单元格值不能按当前稳定字段契约解释。
    #[error("工作表 {sheet} 第 {row} 行字段 {field} 无效: {message}")]
    InvalidCell {
        sheet: String,
        row: u32,
        field: String,
        message: String,
    },
    /// 领域值对象拒绝了工作簿转换结果。
    #[error("配装输入不满足领域约束: {source}")]
    Model {
        #[source]
        source: LoadoutModelError,
    },
    /// 装备库存处理模型拒绝了工作簿转换结果。
    #[error("装备库存输入不满足领域约束: {source}")]
    InventoryModel {
        #[source]
        source: EquipmentInventoryActionError,
    },
}

/// 读取生成标识用于查找同内容文件；命中后由生成端完整重读验证。
pub(super) fn read_workbook_hash(
    path: &Path,
    layout: &WorkbookLayout,
) -> Result<Option<String>, WorkbookProbeError> {
    let bytes = read_bounded_workbook_bytes(path, MAX_RAW_PACKAGE_BYTES, "工作簿生成摘要")?;
    let mut workbook: Xlsx<Cursor<&[u8]>> = open_workbook_from_rs(Cursor::new(bytes.as_slice()))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    let Some(sheet) = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == SCHEMA_SHEET_KEY)
    else {
        return Ok(None);
    };
    if !workbook
        .sheet_names()
        .iter()
        .any(|name| name == sheet.display_name())
    {
        return Ok(None);
    }
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    let Some(field) = layout.fields().iter().find(|field| {
        field.sheet_key() == SCHEMA_SHEET_KEY && field.stable_key() == "workbook_hash"
    }) else {
        return Ok(None);
    };
    let Some(header) = range.rows().next() else {
        return Ok(None);
    };
    let Some(column) = header
        .iter()
        .position(|cell| matches!(cell, Data::String(value) if value == field.display_name()))
    else {
        return Ok(None);
    };
    Ok(range
        .rows()
        .nth(1)
        .and_then(|row| row.get(column))
        .and_then(|cell| match cell {
            Data::String(value) => Some(value.clone()),
            _ => None,
        }))
}

/// 从同一份数据工作簿字节读取布局选择、配装目标和库存处理。
#[cfg(test)]
pub(crate) fn load_workbook_plan_from_xlsx(
    path: &Path,
    root_layout: &WorkbookLayout,
) -> Result<WorkbookPlanInputs, WorkbookPlanError> {
    load_workbook_plan_with_documents(
        path,
        root_layout,
        &super::document::WorkbookDocuments::new(),
    )
}

pub(crate) fn load_workbook_plan_with_documents(
    path: &Path,
    root_layout: &WorkbookLayout,
    documents: &super::document::WorkbookDocuments,
) -> Result<WorkbookPlanInputs, WorkbookPlanError> {
    let document = WorkbookDocument::read(path, "数据工作簿")?;
    let mut workbook = open_checked_workbook(&document)?;
    let layout = select_layout_snapshot(&mut workbook, root_layout)?;
    let loadout = parse_open_loadout(&mut workbook, &layout)?;
    let mut actions = parse_inventory_sheet_actions(&mut workbook, &layout)?;
    actions.extend(loadout.slot_actions);
    let inputs = WorkbookPlanInputs {
        layout,
        desired: loadout.desired,
        inventory: EquipmentInventoryPlan::new(actions)
            .map_err(|source| WorkbookPlanError::InventoryModel { source })?,
        source_package_sha256: document.source_package_sha256().to_owned(),
    };
    documents.remember(path.to_path_buf(), document);
    Ok(inputs)
}

pub(in crate::adapters::workbook) fn open_checked_workbook(
    document: &WorkbookDocument,
) -> Result<Xlsx<Cursor<&[u8]>>, WorkbookPlanError> {
    reject_cell_formulas(document.package())?;
    open_workbook_from_rs(Cursor::new(document.bytes()))
        .map_err(|source| WorkbookPlanError::Probe(WorkbookProbeError::XlsxRead { source }))
}

pub(in crate::adapters::workbook) fn select_layout_snapshot(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
) -> Result<WorkbookLayout, WorkbookPlanError> {
    let sheet = layout_sheet(layout, SCHEMA_SHEET_KEY)?;
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookPlanError::Probe(WorkbookProbeError::XlsxRead { source }))?;
    let fields = generated_fields(layout, SCHEMA_SHEET_KEY)?;
    let columns = field_columns(&fields);
    let column = required_column(&columns, SCHEMA_SHEET_KEY, "layout_hash", 1)?;
    let hash = required_text(&range, 1, column, SCHEMA_SHEET_KEY, 2, "layout_hash")?;
    let selected = if hash == layout.content_sha256() {
        layout.clone()
    } else {
        layout
            .without_ship_acquisition()
            .map_err(|error| invalid_cell(SCHEMA_SHEET_KEY, 2, "layout_hash", error.to_string()))?
    };
    validate_schema_snapshot(workbook, &selected)?;
    Ok(selected)
}

/// 将可读标签映射为字典稳定值；仅改动内存中的计划输入，不改写工作簿。
fn resolve_equipment_choice_labels(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
    mut range: calamine::Range<Data>,
    columns: &BTreeMap<String, u32>,
) -> Result<calamine::Range<Data>, WorkbookPlanError> {
    let choices = read_equipment_choice_labels(workbook, layout)?;
    for (key, column) in columns {
        if !key.ends_with("_target_equipment_family") {
            continue;
        }
        for row in 1..range.height() as u32 {
            if let Some(Data::String(label)) = range.get_value((row, *column))
                && let Some(stable) = choices.get(label.as_str())
            {
                range.set_value((row, *column), Data::String(format!("〔{stable}〕")));
            }
        }
    }
    Ok(range)
}

/// 读取装备标签的唯一来源绑定，供计划读取和快照写回复用。
pub(super) fn read_equipment_choice_labels(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
) -> Result<BTreeMap<String, String>, WorkbookPlanError> {
    let sheet = layout_sheet(layout, "dictionaries")?;
    if !workbook
        .sheet_names()
        .iter()
        .any(|name| name == sheet.display_name())
    {
        return Ok(BTreeMap::new());
    }
    let fields = generated_fields(layout, "dictionaries")?;
    let dictionary_columns = field_columns(&fields);
    let dictionary = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookPlanError::Probe(WorkbookProbeError::XlsxRead { source }))?;
    validate_headers("dictionaries", &fields, &dictionary)?;
    let mut choices = BTreeMap::new();
    for row in 1..dictionary.height() as u32 {
        let value = |key: &str| {
            dictionary_columns
                .get(key)
                .and_then(|column| dictionary.get_value((row, *column)))
                .and_then(|cell| match cell {
                    Data::String(value) => Some(value.as_str()),
                    _ => None,
                })
        };
        if value("category_key").is_some_and(|category| {
            category.starts_with("equipment_choice_") || category == "equipment_selection"
        }) && let (Some(label), Some(stable)) = (value("display_label"), value("stable_value"))
            && stable.contains('|')
            && choices
                .insert(label.to_owned(), stable.to_owned())
                .is_some_and(|previous| previous != stable)
        {
            return Err(invalid_cell(
                "dictionaries",
                row + 1,
                "display_label",
                format!("装备标签 {label} 对应多个来源"),
            ));
        }
    }
    Ok(choices)
}

#[derive(Debug)]
struct ParsedLoadout {
    desired: DesiredState,
    slot_actions: Vec<EquipmentInventoryAction>,
}

fn parse_loadout_rows(
    range: &calamine::Range<Data>,
    columns: &BTreeMap<String, u32>,
    layout: &WorkbookLayout,
) -> Result<ParsedLoadout, WorkbookPlanError> {
    let height = u32::try_from(range.height())
        .map_err(|_| invalid_cell(LOADOUT_SHEET_KEY, 1, "行数", "工作表行数超出当前读取上限"))?;
    let mut slots: Vec<DesiredSlotState> = Vec::new();
    let mut slot_actions = Vec::new();
    let mut seen_ship_ids = BTreeSet::new();
    for row_index in 1..height {
        let row_number = row_index + 1;
        if row_is_blank(range, row_index, columns.len()) {
            continue;
        }
        let Some((ship_instance_id, row_slots)) =
            parse_loadout_row(range, row_number, columns, layout)?
        else {
            continue;
        };
        if !seen_ship_ids.insert(ship_instance_id) {
            return Err(invalid_cell(
                LOADOUT_SHEET_KEY,
                row_number,
                "instance_id",
                format!("舰船实例 ID {ship_instance_id} 重复，每艘舰船只能占一行"),
            ));
        }
        for slot in 1..=5 {
            if let Some(action) = parse_slot_inventory_action(
                range,
                row_number,
                columns,
                ship_instance_id,
                crate::domain::SlotIndex::new(slot).expect("固定槽位"),
            )? {
                slot_actions.push(action);
            }
        }
        slots.extend(row_slots);
    }

    Ok(ParsedLoadout {
        desired: DesiredState::new(slots).map_err(|source| WorkbookPlanError::Model { source })?,
        slot_actions,
    })
}

fn parse_inventory_sheet_actions(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
) -> Result<Vec<EquipmentInventoryAction>, WorkbookPlanError> {
    let sheet = layout_sheet(layout, INVENTORY_SHEET_KEY)?;
    if sheet.generation() == LayoutGenerationMode::Omitted {
        return Err(invalid_cell(
            INVENTORY_SHEET_KEY,
            1,
            "装备库存",
            "装备库存工作表不能被省略",
        ));
    }
    let fields = generated_fields(layout, INVENTORY_SHEET_KEY)?;
    let columns = field_columns(&fields);
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookPlanError::Probe(WorkbookProbeError::XlsxRead { source }))?;
    validate_headers(INVENTORY_SHEET_KEY, &fields, &range)?;
    let height = u32::try_from(range.height())
        .map_err(|_| invalid_cell(INVENTORY_SHEET_KEY, 1, "行数", "工作表行数超出当前读取上限"))?;
    let mut actions = Vec::new();
    for row_index in 1..height {
        let row_number = row_index + 1;
        if row_is_blank(&range, row_index, fields.len()) {
            continue;
        }
        if let Some(action) = parse_inventory_row(&range, row_number, &columns, layout)? {
            actions.push(action);
        }
    }
    Ok(actions)
}

fn parse_open_loadout(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
) -> Result<ParsedLoadout, WorkbookPlanError> {
    let sheet = layout_sheet(layout, LOADOUT_SHEET_KEY)?;
    if sheet.generation() == LayoutGenerationMode::Omitted {
        return Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            1,
            "配装计划",
            "配装计划工作表不能被省略",
        ));
    }
    let fields = generated_fields(layout, LOADOUT_SHEET_KEY)?;
    let columns = field_columns(&fields);
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookPlanError::Probe(WorkbookProbeError::XlsxRead { source }))?;
    validate_headers(LOADOUT_SHEET_KEY, &fields, &range)?;
    let range = resolve_equipment_choice_labels(workbook, layout, range, &columns)?;
    parse_loadout_rows(&range, &columns, layout)
}

/// 将读取失败映射为带路径、工作表、行和字段上下文的应用错误。
pub(crate) fn map_desired_state_error(path: &Path, error: WorkbookPlanError) -> AppError {
    let code = match &error {
        WorkbookPlanError::InvalidCell { .. }
        | WorkbookPlanError::Model { .. }
        | WorkbookPlanError::InventoryModel { .. } => AppErrorCode::InputInvalid,
        WorkbookPlanError::Probe(WorkbookProbeError::WorkbookLocked { .. }) => {
            AppErrorCode::WorkbookLocked
        }
        WorkbookPlanError::Probe(_) => AppErrorCode::WorkbookInvalid,
    };
    let message = match code {
        AppErrorCode::InputInvalid => "配装计划输入未通过严格校验",
        AppErrorCode::WorkbookLocked => "数据工作簿正在被占用",
        _ => "数据工作簿未通过严格读取校验",
    };
    let cell_context = match &error {
        WorkbookPlanError::InvalidCell {
            sheet, row, field, ..
        } => Some((sheet.clone(), *row, field.clone())),
        WorkbookPlanError::Probe(_)
        | WorkbookPlanError::Model { .. }
        | WorkbookPlanError::InventoryModel { .. } => None,
    };
    let mut application_error = AppError::from_source("workbook.plan.load", code, message, error)
        .with_context("path", path.to_string_lossy());
    if let Some((sheet, row, field)) = cell_context {
        application_error = application_error
            .with_context("sheet", sheet)
            .with_context("row", row.to_string())
            .with_context("field", field);
    }
    application_error
}

pub(super) fn validate_schema_snapshot(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
) -> Result<(), WorkbookPlanError> {
    let sheet = layout_sheet(layout, SCHEMA_SHEET_KEY)?;
    if sheet.generation() == LayoutGenerationMode::Omitted {
        return Err(invalid_cell(
            SCHEMA_SHEET_KEY,
            1,
            "布局快照",
            "数据工作簿缺少不可省略的布局快照",
        ));
    }
    let fields = generated_fields(layout, SCHEMA_SHEET_KEY)?;
    let columns = field_columns(&fields);
    let range = workbook
        .worksheet_range(sheet.display_name())
        .map_err(|source| WorkbookPlanError::Probe(WorkbookProbeError::XlsxRead { source }))?;
    validate_headers(SCHEMA_SHEET_KEY, &fields, &range)?;
    let expected_height = layout
        .fields()
        .len()
        .checked_add(1)
        .ok_or_else(|| invalid_cell(SCHEMA_SHEET_KEY, 1, "行数", "布局字段数量溢出"))?;
    if range.height() != expected_height {
        return Err(invalid_cell(
            SCHEMA_SHEET_KEY,
            1,
            "行数",
            format!(
                "布局快照行数错误，期望 {expected_height}，实际 {}",
                range.height()
            ),
        ));
    }
    let layout_hash_column = required_column(&columns, SCHEMA_SHEET_KEY, "layout_hash", 1)?;
    let workbook_schema_column =
        required_column(&columns, SCHEMA_SHEET_KEY, "workbook_schema_version", 1)?;
    let layout_schema_column =
        required_column(&columns, SCHEMA_SHEET_KEY, "layout_schema_version", 1)?;
    let field_key_column = required_column(&columns, SCHEMA_SHEET_KEY, "field_key", 1)?;
    let sheet_key_column = required_column(&columns, SCHEMA_SHEET_KEY, "sheet_key", 1)?;

    for row_index in 1..u32::try_from(range.height()).unwrap_or(u32::MAX) {
        let row_number = row_index + 1;
        let expected_field = layout.fields().get(row_index as usize - 1).ok_or_else(|| {
            invalid_cell(SCHEMA_SHEET_KEY, row_number, "field_key", "布局字段行缺失")
        })?;
        let layout_hash = required_text(
            &range,
            row_index,
            layout_hash_column,
            SCHEMA_SHEET_KEY,
            row_number,
            "layout_hash",
        )?;
        if layout_hash != layout.content_sha256() {
            return Err(invalid_cell(
                SCHEMA_SHEET_KEY,
                row_number,
                "layout_hash",
                format!(
                    "布局摘要不匹配，期望 {}，实际 {layout_hash}",
                    layout.content_sha256()
                ),
            ));
        }
        let workbook_schema = required_integer(
            &range,
            row_index,
            workbook_schema_column,
            SCHEMA_SHEET_KEY,
            row_number,
            "workbook_schema_version",
        )?;
        if workbook_schema != i64::from(WORKBOOK_PROJECTION_SCHEMA_VERSION) {
            return Err(invalid_cell(
                SCHEMA_SHEET_KEY,
                row_number,
                "workbook_schema_version",
                format!(
                    "工作簿投影版本不匹配，期望 {WORKBOOK_PROJECTION_SCHEMA_VERSION}，实际 {workbook_schema}"
                ),
            ));
        }
        let layout_schema = required_integer(
            &range,
            row_index,
            layout_schema_column,
            SCHEMA_SHEET_KEY,
            row_number,
            "layout_schema_version",
        )?;
        if layout_schema != i64::from(layout.schema_version()) {
            return Err(invalid_cell(
                SCHEMA_SHEET_KEY,
                row_number,
                "layout_schema_version",
                format!(
                    "布局版本不匹配，期望 {}，实际 {layout_schema}",
                    layout.schema_version()
                ),
            ));
        }
        let field_key = required_text(
            &range,
            row_index,
            field_key_column,
            SCHEMA_SHEET_KEY,
            row_number,
            "field_key",
        )?;
        if field_key != expected_field.stable_key() {
            return Err(invalid_cell(
                SCHEMA_SHEET_KEY,
                row_number,
                "field_key",
                format!(
                    "布局字段顺序不匹配，期望 {}，实际 {field_key}",
                    expected_field.stable_key()
                ),
            ));
        }
        let sheet_key = required_text(
            &range,
            row_index,
            sheet_key_column,
            SCHEMA_SHEET_KEY,
            row_number,
            "sheet_key",
        )?;
        if sheet_key != expected_field.sheet_key() {
            return Err(invalid_cell(
                SCHEMA_SHEET_KEY,
                row_number,
                "sheet_key",
                format!(
                    "布局字段所属工作表不匹配，期望 {}，实际 {sheet_key}",
                    expected_field.sheet_key()
                ),
            ));
        }
    }
    Ok(())
}

fn layout_sheet<'a>(
    layout: &'a WorkbookLayout,
    stable_key: &str,
) -> Result<&'a crate::application::WorkbookSheetLayout, WorkbookPlanError> {
    layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == stable_key)
        .ok_or_else(|| invalid_cell(stable_key, 1, "工作表", "布局缺少必需工作表"))
}

fn generated_fields<'a>(
    layout: &'a WorkbookLayout,
    sheet_key: &str,
) -> Result<Vec<&'a WorkbookFieldLayout>, WorkbookPlanError> {
    let fields = layout.generated_fields_for_sheet(sheet_key);
    if fields.is_empty() {
        return Err(invalid_cell(sheet_key, 1, "字段", "布局缺少可读取字段"));
    }
    Ok(fields)
}

fn field_columns(fields: &[&WorkbookFieldLayout]) -> BTreeMap<String, u32> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| (field.stable_key().to_owned(), index as u32))
        .collect()
}

fn validate_headers(
    sheet_key: &str,
    fields: &[&WorkbookFieldLayout],
    range: &calamine::Range<Data>,
) -> Result<(), WorkbookPlanError> {
    if range.width() != fields.len() || range.height() == 0 {
        return Err(invalid_cell(
            sheet_key,
            1,
            "表头",
            format!(
                "工作表范围错误，期望 {} 列且至少一行，实际 {} 列 {} 行",
                fields.len(),
                range.width(),
                range.height()
            ),
        ));
    }
    for (index, field) in fields.iter().enumerate() {
        let column = u32::try_from(index).unwrap_or(u32::MAX);
        match range.get_value((0, column)) {
            Some(Data::String(value)) if value == field.display_name() => {}
            Some(actual) => {
                return Err(invalid_cell(
                    sheet_key,
                    1,
                    field.stable_key(),
                    format!("表头应为 {:?}，实际为 {actual:?}", field.display_name()),
                ));
            }
            None => {
                return Err(invalid_cell(
                    sheet_key,
                    1,
                    field.stable_key(),
                    "表头单元格缺失",
                ));
            }
        }
    }
    Ok(())
}

fn row_is_blank(range: &calamine::Range<Data>, row: u32, width: usize) -> bool {
    (0..width).all(|column| {
        let column = u32::try_from(column).unwrap_or(u32::MAX);
        match range.get_value((row, column)) {
            None | Some(Data::Empty) => true,
            Some(Data::String(value)) => value.is_empty(),
            Some(_) => false,
        }
    })
}

fn required_column(
    columns: &BTreeMap<String, u32>,
    sheet: &str,
    field: &str,
    row: u32,
) -> Result<u32, WorkbookPlanError> {
    columns
        .get(field)
        .copied()
        .ok_or_else(|| invalid_cell(sheet, row, field, "布局缺少必需字段"))
}

fn required_text_by_key(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<String, WorkbookPlanError> {
    let column = required_column(columns, LOADOUT_SHEET_KEY, field, row)?;
    required_text(range, row - 1, column, LOADOUT_SHEET_KEY, row, field)
}

fn optional_enum(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    layout: &WorkbookLayout,
    field: &str,
    category: &str,
) -> Result<Option<String>, WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(None);
    };
    let Some(value) = optional_text_at(range, row - 1, column, LOADOUT_SHEET_KEY, row, field)?
    else {
        return Ok(None);
    };
    layout
        .enum_options()
        .iter()
        .find(|option| option.category_key() == category && option.label() == value)
        .map(|option| Some(option.stable_value().to_owned()))
        .ok_or_else(|| {
            invalid_cell(
                LOADOUT_SHEET_KEY,
                row,
                field,
                format!("未知的枚举显示值 {value}"),
            )
        })
}

fn optional_text(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<Option<String>, WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(None);
    };
    optional_text_at(range, row - 1, column, LOADOUT_SHEET_KEY, row, field)
}

fn optional_text_at(
    range: &calamine::Range<Data>,
    row_index: u32,
    column: u32,
    sheet: &str,
    row: u32,
    field: &str,
) -> Result<Option<String>, WorkbookPlanError> {
    match range.get_value((row_index, column)) {
        None | Some(Data::Empty) => Ok(None),
        Some(Data::String(value)) if value.is_empty() => Ok(None),
        Some(Data::String(value)) => Ok(Some(value.clone())),
        Some(actual) => Err(invalid_cell(
            sheet,
            row,
            field,
            format!("必须是文本或空白，实际为 {actual:?}"),
        )),
    }
}

fn required_text(
    range: &calamine::Range<Data>,
    row_index: u32,
    column: u32,
    sheet: &str,
    row: u32,
    field: &str,
) -> Result<String, WorkbookPlanError> {
    optional_text_at(range, row_index, column, sheet, row, field)?
        .ok_or_else(|| invalid_cell(sheet, row, field, "必填文本不能为空"))
}

fn required_integer(
    range: &calamine::Range<Data>,
    row_index: u32,
    column: u32,
    sheet: &str,
    row: u32,
    field: &str,
) -> Result<i64, WorkbookPlanError> {
    optional_integer_at(range, row_index, column, sheet, row, field)?
        .ok_or_else(|| invalid_cell(sheet, row, field, "必填整数不能为空"))
}

fn optional_integer_at(
    range: &calamine::Range<Data>,
    row_index: u32,
    column: u32,
    sheet: &str,
    row: u32,
    field: &str,
) -> Result<Option<i64>, WorkbookPlanError> {
    match range.get_value((row_index, column)) {
        None | Some(Data::Empty) => Ok(None),
        Some(Data::Int(value)) => Ok(Some(*value)),
        Some(Data::String(value)) if value.is_empty() => Ok(None),
        Some(Data::String(value)) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|_| invalid_cell(sheet, row, field, format!("{value:?} 不是整数或空白"))),
        Some(Data::Float(value)) if value.is_finite() && value.fract() == 0.0 => {
            if *value < i64::MIN as f64 || *value > i64::MAX as f64 {
                Err(invalid_cell(sheet, row, field, "整数超出 i64 范围"))
            } else {
                Ok(Some(*value as i64))
            }
        }
        Some(actual) => Err(invalid_cell(
            sheet,
            row,
            field,
            format!("必须是整数或空白，实际为 {actual:?}"),
        )),
    }
}

fn optional_enhance_level(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<Option<EnhanceLevel>, WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(None);
    };
    let value = optional_integer_at(range, row - 1, column, LOADOUT_SHEET_KEY, row, field)?;
    value
        .map(|value| {
            u8::try_from(value).map(EnhanceLevel::new).map_err(|_| {
                invalid_cell(
                    LOADOUT_SHEET_KEY,
                    row,
                    field,
                    "强化等级必须在 0 至 255 之间",
                )
            })
        })
        .transpose()
}

fn optional_priority(
    range: &calamine::Range<Data>,
    row: u32,
    columns: &BTreeMap<String, u32>,
    field: &str,
) -> Result<Option<i32>, WorkbookPlanError> {
    let Some(&column) = columns.get(field) else {
        return Ok(None);
    };
    let value = optional_integer_at(range, row - 1, column, LOADOUT_SHEET_KEY, row, field)?;
    value
        .map(|value| {
            i32::try_from(value)
                .map_err(|_| invalid_cell(LOADOUT_SHEET_KEY, row, field, "分配优先级超出 i32 范围"))
        })
        .transpose()
}

fn require_blank(row: u32, field: &str, value: Option<&str>) -> Result<(), WorkbookPlanError> {
    if value.is_some() {
        return Err(invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            "当前最终状态不允许填写该字段",
        ));
    }
    Ok(())
}

fn parse_u64(value: &str, row: u32, field: &str) -> Result<u64, WorkbookPlanError> {
    value.parse::<u64>().map_err(|_| {
        invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            format!("{value:?} 不是非负整数"),
        )
    })
}

fn parse_u8(value: &str, row: u32, field: &str) -> Result<u8, WorkbookPlanError> {
    value.parse::<u8>().map_err(|_| {
        invalid_cell(
            LOADOUT_SHEET_KEY,
            row,
            field,
            format!("{value:?} 不是 0 至 255 的整数"),
        )
    })
}

fn invalid_cell(
    sheet: &str,
    row: u32,
    field: &str,
    message: impl Into<String>,
) -> WorkbookPlanError {
    WorkbookPlanError::InvalidCell {
        sheet: sheet.to_owned(),
        row,
        field: field.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;

impl From<suzushiro_xlsx_toolkit::XlsxError> for WorkbookPlanError {
    fn from(source: suzushiro_xlsx_toolkit::XlsxError) -> Self {
        Self::from(crate::adapters::workbook::WorkbookProbeError::from(source))
    }
}

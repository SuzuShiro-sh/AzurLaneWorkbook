//! 按实例身份保留配装输入，并把保留的技术历史纳入同一规范投影。

use std::collections::BTreeMap;
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx, open_workbook_from_rs};

use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::reader::validate_schema_snapshot;
use crate::application::{
    LayoutEditor, LayoutGenerationMode, LayoutValueFormat, WorkbookFieldLayout, WorkbookLayout,
    WorkbookProjectionRow, WorkbookProjectionV4, WorkbookProjectionValue,
};

use super::super::invalid;

type Values = BTreeMap<String, WorkbookProjectionValue>;
type Rows = Vec<(String, Values)>;

pub(super) const REFRESHED_SHEETS: &[&str] = &[
    "dictionaries",
    "equipment_inventory",
    "loadout_plan",
    "resource_recipes",
    "raw_data",
    "execution_results",
    "schema",
    "ship_technology",
];

pub(super) fn merge_projection(
    source: &[u8],
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    results: &[WorkbookProjectionRow],
) -> Result<WorkbookProjectionV4, WorkbookProbeError> {
    let mut workbook: Xlsx<Cursor<&[u8]>> = open_workbook_from_rs(Cursor::new(source))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    validate_schema_snapshot(&mut workbook, layout)
        .map_err(|error| invalid("schema", format!("布局快照校验失败: {error}")))?;
    let mut replacements = BTreeMap::new();
    for sheet in layout
        .sheets()
        .iter()
        .filter(|sheet| sheet.generation() != LayoutGenerationMode::Omitted)
    {
        let key = sheet.stable_key();
        if key != "loadout_plan" && (REFRESHED_SHEETS.contains(&key) || key == "dictionaries") {
            continue;
        }
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let range = workbook
            .worksheet_range(sheet.display_name())
            .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
        for (column, field) in fields.iter().enumerate() {
            if range.get_value((0, column as u32))
                != Some(&Data::String(field.display_name().to_owned()))
            {
                return Err(invalid(
                    key,
                    format!("字段 {} 的原表头与布局不一致", field.stable_key()),
                ));
            }
        }
        if key == "loadout_plan" {
            replacements.insert(
                key.to_owned(),
                merge_loadout_inputs(&range, &fields, layout, projection)?,
            );
        } else {
            let all_fields: Vec<_> = layout
                .fields()
                .iter()
                .filter(|field| field.sheet_key() == key)
                .collect();
            let mut rows = Vec::new();
            for row in 1..range.height() {
                if fields
                    .iter()
                    .enumerate()
                    .all(|(column, _)| is_blank(range.get_value((row as u32, column as u32))))
                {
                    continue;
                }
                let mut values: Values = all_fields
                    .iter()
                    .map(|field| {
                        (
                            field.stable_key().to_owned(),
                            WorkbookProjectionValue::Blank,
                        )
                    })
                    .collect();
                for (column, field) in fields.iter().enumerate() {
                    values.insert(
                        field.stable_key().to_owned(),
                        read_value(range.get_value((row as u32, column as u32)), field, layout)?,
                    );
                }
                rows.push((format!("preserved:{row:010}"), values));
            }
            replacements.insert(key.to_owned(), rows);
        }
    }
    refresh_equipment_selection_labels(&mut workbook, layout, projection, &mut replacements)?;
    replacements.insert(
        "execution_results".to_owned(),
        results
            .iter()
            .map(|row| (row.object_ref().to_owned(), row.values().clone()))
            .collect(),
    );
    // 库存直接复用最终投影的操作默认值：不处理及空白参数。
    projection
        .clone()
        .with_replaced_rows(replacements)
        .map_err(|error| invalid("snapshot", format!("合并最终状态与保留输入失败: {error}")))
}

/// 数量更新时保留选择的来源；已消耗来源只保留输入绑定，不加入可选列表。
fn refresh_equipment_selection_labels(
    workbook: &mut Xlsx<Cursor<&[u8]>>,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    replacements: &mut BTreeMap<String, Rows>,
) -> Result<(), WorkbookProbeError> {
    let old = crate::adapters::workbook::reader::read_equipment_choice_labels(workbook, layout)
        .map_err(|error| invalid("dictionaries", error.to_string()))?;
    let dictionary = projection
        .sheet("dictionaries")
        .ok_or_else(|| invalid("dictionaries", "缺少装备来源字典"))?;
    let mut by_source = BTreeMap::new();
    for row in dictionary.rows() {
        if let (
            Some(WorkbookProjectionValue::Text(stable)),
            Some(WorkbookProjectionValue::Text(label)),
        ) = (row.value("stable_value"), row.value("display_label"))
            && stable.contains('|')
        {
            by_source.insert(stable.clone(), label.clone());
        }
    }
    let mut retained = BTreeMap::new();
    if let Some(rows) = replacements.get_mut("loadout_plan") {
        for (_, values) in rows {
            for (key, value) in values {
                if key.ends_with("_target_equipment_family")
                    && let WorkbookProjectionValue::Text(label) = value
                    && let Some(stable) = old.get(label)
                {
                    if let Some(updated) = by_source.get(stable) {
                        *label = updated.clone();
                    } else {
                        retained.insert(label.clone(), stable.clone());
                    }
                }
            }
        }
    }
    if !retained.is_empty() {
        let mut rows: Rows = dictionary
            .rows()
            .iter()
            .map(|row| (row.object_ref().to_owned(), row.values().clone()))
            .collect();
        for (index, (label, stable)) in retained.into_iter().enumerate() {
            let values = BTreeMap::from([
                (
                    "category_key".to_owned(),
                    WorkbookProjectionValue::text("equipment_selection"),
                ),
                (
                    "stable_value".to_owned(),
                    WorkbookProjectionValue::text(stable),
                ),
                (
                    "display_label".to_owned(),
                    WorkbookProjectionValue::text(label),
                ),
                ("object_ref".to_owned(), WorkbookProjectionValue::Blank),
                ("description".to_owned(), WorkbookProjectionValue::Blank),
                ("layout_hash".to_owned(), WorkbookProjectionValue::Blank),
                (
                    "order".to_owned(),
                    WorkbookProjectionValue::Integer(index as i64 + 1),
                ),
            ]);
            rows.push((format!("equipment_selection:{index:020}"), values));
        }
        replacements.insert("dictionaries".to_owned(), rows);
    }
    Ok(())
}

fn merge_loadout_inputs(
    range: &calamine::Range<Data>,
    fields: &[&WorkbookFieldLayout],
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<Rows, WorkbookProbeError> {
    let instance_column = fields
        .iter()
        .position(|field| field.stable_key() == "instance_id")
        .ok_or_else(|| invalid("loadout_plan", "配装输入保留需要实例ID列"))?;
    let mut inputs = BTreeMap::new();
    for row in 1..range.height() {
        let mut values = BTreeMap::new();
        for (column, field) in fields.iter().enumerate().filter(|(_, field)| {
            field.editor() != LayoutEditor::ReadOnly && field.stable_key().starts_with("slot_")
        }) {
            values.insert(
                field.stable_key().to_owned(),
                read_value(range.get_value((row as u32, column as u32)), field, layout)?,
            );
        }
        let has_input = values.values().any(meaningful_input);
        let instance = match range.get_value((row as u32, instance_column as u32)) {
            Some(Data::String(value)) if !value.is_empty() => value.clone(),
            _ if !has_input => continue,
            _ => {
                return Err(invalid(
                    "loadout_plan",
                    format!("第 {} 行存在用户输入但缺少实例ID，保留原工作簿", row + 1),
                ));
            }
        };
        if inputs.insert(instance.clone(), values).is_some() {
            return Err(invalid(
                "loadout_plan",
                format!("实例 {instance} 重复，配装输入归属不唯一"),
            ));
        }
    }
    let sheet = projection
        .sheet("loadout_plan")
        .ok_or_else(|| invalid("loadout_plan", "缺少最终舰船投影"))?;
    let mut rows = Vec::new();
    for row in sheet.rows() {
        let mut values = row.values().clone();
        if let Some(WorkbookProjectionValue::Text(instance)) = row.value("instance_id")
            && let Some(input) = inputs.remove(instance)
        {
            // 卸下和拆解是单次操作；最终快照保留装备目标，不重复提交已处理的操作。
            values.extend(input.into_iter().filter(|(key, value)| {
                !(key.ends_with("_target_equipment_family")
                    && matches!(value, WorkbookProjectionValue::Text(value) if matches!(value.as_str(), "卸下" | "拆解")))
            }));
        }
        rows.push((row.object_ref().to_owned(), values));
    }
    if let Some((instance, _)) = inputs
        .iter()
        .find(|(_, values)| values.values().any(meaningful_input))
    {
        return Err(invalid(
            "loadout_plan",
            format!("最终快照缺少仍有用户输入的实例 {instance}，保留原工作簿，请核实该实例"),
        ));
    }
    Ok(rows)
}

fn meaningful_input(value: &WorkbookProjectionValue) -> bool {
    !matches!(value, WorkbookProjectionValue::Blank)
}

fn is_blank(value: Option<&Data>) -> bool {
    matches!(value, None | Some(Data::Empty))
        || matches!(value, Some(Data::String(value)) if value.is_empty())
}

fn read_value(
    value: Option<&Data>,
    field: &WorkbookFieldLayout,
    layout: &WorkbookLayout,
) -> Result<WorkbookProjectionValue, WorkbookProbeError> {
    if is_blank(value) {
        return Ok(WorkbookProjectionValue::Blank);
    }
    let error = || {
        invalid(
            field.sheet_key(),
            format!(
                "字段 {} 的原值格式不符合布局: {value:?}",
                field.stable_key()
            ),
        )
    };
    let value = value.ok_or_else(error)?;
    if field.editor() == LayoutEditor::Boolean {
        return match value {
            Data::String(value) if value == "是" => Ok(WorkbookProjectionValue::Boolean(true)),
            Data::String(value) if value == "否" => Ok(WorkbookProjectionValue::Boolean(false)),
            _ => Err(error()),
        };
    }
    if let Some(category) = field.enum_category() {
        if let Data::String(label) = value
            && let Some(option) = layout
                .enum_options()
                .iter()
                .find(|option| option.category_key() == category && option.label() == label)
        {
            return Ok(WorkbookProjectionValue::enumeration(
                category,
                option.stable_value(),
            ));
        }
        return Err(error());
    }
    match (field.value_format(), value) {
        (LayoutValueFormat::Text, Data::String(value)) => {
            Ok(WorkbookProjectionValue::Text(value.clone()))
        }
        (LayoutValueFormat::Json, Data::String(value)) => {
            Ok(WorkbookProjectionValue::Json(value.clone()))
        }
        (LayoutValueFormat::Integer, Data::Int(value)) => {
            Ok(WorkbookProjectionValue::Integer(*value))
        }
        (LayoutValueFormat::Integer, Data::Float(value))
            if value.is_finite()
                && value.fract() == 0.0
                && value.abs() <= 9_007_199_254_740_991.0 =>
        {
            Ok(WorkbookProjectionValue::Integer(*value as i64))
        }
        (LayoutValueFormat::Integer, Data::String(value))
            if field.editor() == LayoutEditor::Integer =>
        {
            value
                .parse()
                .map(WorkbookProjectionValue::Integer)
                .map_err(|_| error())
        }
        (LayoutValueFormat::Decimal | LayoutValueFormat::Percentage, Data::Float(value)) => {
            Ok(WorkbookProjectionValue::Decimal(*value))
        }
        (LayoutValueFormat::Decimal | LayoutValueFormat::Percentage, Data::Int(value)) => {
            Ok(WorkbookProjectionValue::Decimal(*value as f64))
        }
        (LayoutValueFormat::DateTime, Data::DateTime(value)) => {
            excel_millis(value.as_f64()).map(WorkbookProjectionValue::DateTimeUnixMillis)
        }
        (LayoutValueFormat::DateTime, Data::Float(value)) => {
            excel_millis(*value).map(WorkbookProjectionValue::DateTimeUnixMillis)
        }
        _ => Err(error()),
    }
}

fn excel_millis(serial: f64) -> Result<i64, WorkbookProbeError> {
    let value = ((serial - 25_569.0) * 86_400_000.0).round();
    if !value.is_finite() || value < i64::MIN as f64 || value > i64::MAX as f64 {
        return Err(invalid("snapshot", "保留的日期值超出Unix毫秒范围"));
    }
    Ok(value as i64)
}

//! 将共享装备字典绑定到持有舰船的目标装备输入单元格。

use super::{WorkbookProbeError, build_error, integer_overflow};
use crate::application::{
    WorkbookFieldLayout, WorkbookLayout, WorkbookProjectionV4, WorkbookProjectionValue,
};
use rust_xlsxwriter::{DataValidation, Formula, Worksheet};
use std::collections::BTreeMap;

pub(super) fn add_equipment_validations(
    worksheet: &mut Worksheet,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    fields: &[&WorkbookFieldLayout],
) -> Result<(), WorkbookProbeError> {
    for ((row, column), formula) in equipment_validations(layout, projection, fields)? {
        let validation = DataValidation::new()
            .allow_list_formula(Formula::new(formula))
            .set_input_title("更换装备或处理当前装备")?
            .set_input_message(
                "留空保持；按来源选择装备，数量为生成时快照；选舰船来源会调拨该槽位装备；卸下：回仓库；拆解：消耗装备。",
            )?
            .set_error_title("请选择装备或操作")?
            .set_error_message("请从下拉列表选择装备、卸下或拆解；保持现状请清空单元格。")?;
        worksheet.add_data_validation(row, column, row, column, &validation)?;
    }
    Ok(())
}

pub(super) fn equipment_validations(
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    fields: &[&WorkbookFieldLayout],
) -> Result<BTreeMap<(u32, u16), String>, WorkbookProbeError> {
    let mut result = BTreeMap::new();
    let dictionary = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "dictionaries")
        .ok_or_else(|| build_error("缺少字典工作表"))?;
    let dictionary_fields = layout.generated_fields_for_sheet(dictionary.stable_key());
    let column = dictionary_fields
        .iter()
        .position(|field| field.stable_key() == "display_label")
        .ok_or_else(|| build_error("装备字典缺少显示标签列"))?;
    let column = u16::try_from(column).map_err(integer_overflow)?;
    let rows = projection
        .sheet("dictionaries")
        .ok_or_else(|| build_error("缺少装备字典投影"))?
        .rows();
    let mut ranges: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(WorkbookProjectionValue::Text(category)) = row.value("category_key") else {
            return Err(build_error("装备字典分类为空"));
        };
        if category.starts_with("equipment_choice_") {
            let number =
                u32::try_from(layout.enum_options().len() + index + 1).map_err(integer_overflow)?;
            ranges
                .entry(category)
                .and_modify(|range| range.1 = number)
                .or_insert((number, number));
        }
    }
    let ships = projection
        .sheet("loadout_plan")
        .ok_or_else(|| build_error("缺少舰船投影"))?;
    let ship_rows: BTreeMap<_, _> = ships
        .rows()
        .iter()
        .enumerate()
        .map(|(index, row)| (row.object_ref(), index + 1))
        .collect();
    for row in rows {
        if row.value("category_key")
            != Some(&WorkbookProjectionValue::Text(
                "equipment_target".to_owned(),
            ))
        {
            continue;
        }
        let get_text = |key| match row.value(key) {
            Some(WorkbookProjectionValue::Text(value)) => Ok(value.as_str()),
            _ => Err(build_error(format!("装备目标绑定缺少 {key}"))),
        };
        let field_key = format!("slot_{}_target_equipment_family", get_text("stable_value")?);
        let Some(target_column) = fields
            .iter()
            .position(|field| field.stable_key() == field_key)
        else {
            continue;
        };
        let target_row = ship_rows
            .get(get_text("object_ref")?)
            .ok_or_else(|| build_error("装备下拉引用不存在的舰船"))?;
        let (first, last) = ranges
            .get(get_text("display_label")?)
            .ok_or_else(|| build_error("装备下拉引用不存在的候选组"))?;
        let first = rust_xlsxwriter::utility::row_col_to_cell_absolute(*first, column);
        let last = rust_xlsxwriter::utility::row_col_to_cell_absolute(*last, column);
        let name = dictionary.display_name().replace('\'', "''");
        let reference = format!("'{name}'!{first}:{last}").replace('"', "\"\"");

        let target_row = u32::try_from(*target_row).map_err(integer_overflow)?;
        let target_column = u16::try_from(target_column).map_err(integer_overflow)?;
        result.insert(
            (target_row, target_column),
            format!("INDIRECT(\"{reference}\")"),
        );
    }
    Ok(result)
}

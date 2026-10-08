//! 从共享科技模板派生只读分类表，复用舰船投影及标准写入与校验流程。
use super::{WorkbookProbeError, build_error};
use crate::adapters::workbook::rendering::table_name;
use crate::application::{
    LayoutGenerationMode, TECHNOLOGY_VIEW_KEY, WorkbookLayout, WorkbookProjectionSheet,
    WorkbookProjectionV4, WorkbookSheetLayout, technology_category_layout,
};
use std::borrow::Cow;
use std::collections::BTreeSet;

pub(in crate::adapters::workbook) struct OutputSheet<'a> {
    pub(in crate::adapters::workbook) layout: Cow<'a, WorkbookSheetLayout>,
    pub(in crate::adapters::workbook) projection: Cow<'a, WorkbookProjectionSheet>,
}
pub(in crate::adapters::workbook) fn output_sheets<'a>(
    layout: &'a WorkbookLayout,
    projection: &'a WorkbookProjectionV4,
) -> Result<Vec<OutputSheet<'a>>, WorkbookProbeError> {
    let mut outputs = Vec::new();
    let mut names = BTreeSet::new();
    for template in layout
        .sheets()
        .iter()
        .filter(|sheet| sheet.generation() != LayoutGenerationMode::Omitted)
    {
        if template.stable_key() == TECHNOLOGY_VIEW_KEY {
            let source = projection
                .sheet("loadout_plan")
                .ok_or_else(|| build_error("科技分类缺少舰船投影"))?;
            for (category, category_rows) in source.technology_category_sheets() {
                let sheet = technology_category_layout(template, &category);
                if !names.insert(sheet.display_name().to_lowercase()) {
                    return Err(build_error(format!(
                        "科技分类页签名称重复：{}",
                        sheet.display_name()
                    )));
                }
                outputs.push(OutputSheet {
                    layout: Cow::Owned(sheet),
                    projection: Cow::Owned(category_rows),
                });
            }
        } else {
            if !names.insert(template.display_name().to_lowercase()) {
                return Err(build_error(format!(
                    "页签名称重复：{}",
                    template.display_name()
                )));
            }
            let source = projection
                .sheet(template.stable_key())
                .ok_or_else(|| build_error(format!("投影缺少工作表 {}", template.stable_key())))?;
            outputs.push(OutputSheet {
                layout: Cow::Borrowed(template),
                projection: Cow::Borrowed(source),
            });
        }
    }
    Ok(outputs)
}

/// 沿稳定表名绑定实际页签，并在写入前检查分类集合和规范名称冲突。
pub(in crate::adapters::workbook) fn source_sheet_bindings(
    bytes: &[u8],
    package: &crate::adapters::workbook::package::PackageSnapshot,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<std::collections::BTreeMap<String, String>, WorkbookProbeError> {
    use crate::adapters::workbook::sheet_parts::worksheet_table_part_name;
    use calamine::{Reader as _, Xlsx};
    use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;
    let mut workbook: Xlsx<std::io::Cursor<&[u8]>> =
        calamine::open_workbook_from_rs(std::io::Cursor::new(bytes))
            .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    workbook
        .load_tables()
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    let definitions = super::package_validation::read_table_definitions(package)?;
    let outputs = output_sheets(layout, projection)?;
    let expected: BTreeSet<_> = outputs
        .iter()
        .filter(|output| output.layout.stable_key().starts_with("ship_technology:"))
        .map(|output| table_name(&output.layout))
        .collect();
    let actual: BTreeSet<_> = definitions
        .keys()
        .filter(|name| name.starts_with("AZLW_ship_technology_"))
        .cloned()
        .collect();
    if actual != expected {
        return Err(build_error(
            "科技分类表集合与当前状态不一致，请重新生成工作簿后再执行",
        ));
    }
    let mut bindings = std::collections::BTreeMap::new();
    let mut renames = std::collections::BTreeMap::new();
    for output in outputs {
        let sheet = output.layout.as_ref();
        let stable = table_name(sheet);
        let matches: Vec<_> = workbook
            .sheet_names()
            .iter()
            .filter(|name| {
                workbook
                    .table_names_in_sheet(name)
                    .iter()
                    .any(|table| table.as_str() == stable)
            })
            .cloned()
            .collect();
        if matches.len() != 1 {
            return Err(build_error(format!(
                "工作表 {} 必须唯一关联稳定表 {stable}，实际为 {}",
                sheet.display_name(),
                matches.len()
            )));
        }
        let actual_name = &matches[0];
        if !sheet.stable_key().starts_with("ship_technology:")
            && actual_name != sheet.display_name()
        {
            return Err(build_error(format!(
                "工作表 {} 的名称与布局不一致",
                sheet.display_name()
            )));
        }
        let part = worksheet_part_name(package, actual_name)?;
        worksheet_table_part_name(package, &part)?;
        if actual_name != sheet.display_name() {
            renames.insert(actual_name.clone(), sheet.display_name().to_owned());
        }
        bindings.insert(sheet.display_name().to_owned(), actual_name.clone());
    }
    let mut final_names = BTreeSet::new();
    for name in workbook.sheet_names() {
        let target = renames.get(&name).unwrap_or(&name);
        if !final_names.insert(target.to_lowercase()) {
            return Err(build_error(format!(
                "科技页签规范名称 {target} 与其他工作表冲突"
            )));
        }
    }
    Ok(bindings)
}

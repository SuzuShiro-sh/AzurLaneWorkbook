//! 将解析记录与程序注册表逐项核对并生成规范布局摘要。

use std::collections::{BTreeMap, BTreeSet};

use crate::application::layout_content_sha256;
use crate::application::{
    LayoutEditor, LayoutGenerationMode, LayoutValueFormat, WorkbookFieldLayout, WorkbookLayout,
    WorkbookLayoutEnumOption, WorkbookLayoutRegistry, WorkbookLayoutStyle, WorkbookSheetLayout,
    excel_sheet_tab_name_is_legal,
};

use super::parser::{
    ControlEnumLabels, ParsedEnumOption, ParsedField, ParsedLayout, ParsedSheet, ParsedStyle,
};
use super::{FIELD_SETTINGS, FORMAT_SETTINGS, SHEET_SETTINGS, WorkbookLayoutError};

/// 逐类核对注册项、检查最终唯一性并计算规范模型摘要。
pub(super) fn validate_layout(
    parsed: ParsedLayout,
    registry: &WorkbookLayoutRegistry,
) -> Result<WorkbookLayout, WorkbookLayoutError> {
    let mut missing: Vec<String> = Vec::new();
    let control_labels = &parsed.control_labels;
    let mut sheets = validate_sheets(parsed.sheets, registry, control_labels, &mut missing)?;
    let mut fields = validate_fields(
        parsed.fields,
        registry,
        &sheets,
        control_labels,
        &mut missing,
    )?;
    let mut enum_options = validate_enum_options(parsed.enum_options, registry, &mut missing)?;
    let mut styles = validate_styles(parsed.styles, registry, &mut missing)?;
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(WorkbookLayoutError::UpgradeRequired { missing });
    }

    sheets.sort_by(|left, right| {
        (left.order(), left.stable_key()).cmp(&(right.order(), right.stable_key()))
    });
    let sheet_ranks: BTreeMap<String, usize> = sheets
        .iter()
        .enumerate()
        .map(|(index, sheet)| (sheet.stable_key().to_owned(), index))
        .collect();
    fields.sort_by(|left, right| {
        (
            sheet_ranks.get(left.sheet_key()),
            left.order(),
            left.stable_key(),
        )
            .cmp(&(
                sheet_ranks.get(right.sheet_key()),
                right.order(),
                right.stable_key(),
            ))
    });
    enum_options.sort_by(|left, right| {
        (left.category_key(), left.order(), left.stable_value()).cmp(&(
            right.category_key(),
            right.order(),
            right.stable_value(),
        ))
    });
    styles.sort_by(|left, right| left.stable_key().cmp(right.stable_key()));

    let content_sha256 = layout_content_sha256(
        parsed.schema_version,
        &parsed.template_name,
        &parsed.purpose,
        &sheets,
        &fields,
        &enum_options,
        &styles,
    )?;
    WorkbookLayout::new(
        parsed.schema_version,
        parsed.template_name,
        parsed.purpose,
        sheets,
        fields,
        enum_options,
        styles,
        content_sha256,
    )
    .map_err(WorkbookLayoutError::from)
}

/// 核对工作表稳定键和必需标记，并收集注册表新增项。
fn validate_sheets(
    rows: Vec<ParsedSheet>,
    registry: &WorkbookLayoutRegistry,
    control_labels: &ControlEnumLabels,
    missing: &mut Vec<String>,
) -> Result<Vec<WorkbookSheetLayout>, WorkbookLayoutError> {
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    let mut final_names: BTreeMap<String, (String, u32)> = BTreeMap::new();
    let mut final_orders: BTreeMap<u32, (String, u32)> = BTreeMap::new();
    let mut sheets: Vec<WorkbookSheetLayout> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(previous_row) = seen.insert(row.stable_key.clone(), row.row) {
            return Err(WorkbookLayoutError::invalid(
                SHEET_SETTINGS,
                Some(row.row),
                Some(row.stable_key),
                format!("工作表稳定键重复，首次出现于第 {previous_row} 行"),
            ));
        }
        let registered = registry.sheet(&row.stable_key).ok_or_else(|| {
            WorkbookLayoutError::invalid(
                SHEET_SETTINGS,
                Some(row.row),
                Some(row.stable_key.clone()),
                "工作表稳定键未在当前程序注册",
            )
        })?;
        if row.required != registered.required() {
            return Err(WorkbookLayoutError::mismatch(
                SHEET_SETTINGS,
                Some(row.row),
                Some(row.stable_key),
                yes_no(row.required),
                yes_no(registered.required()),
                "锁定的必需标记与程序注册表不一致",
            ));
        }
        if registered.required() && row.generation == LayoutGenerationMode::Omitted {
            let expected = generation_labels(
                control_labels,
                &[LayoutGenerationMode::Visible, LayoutGenerationMode::Hidden],
            )?;
            return Err(WorkbookLayoutError::mismatch(
                SHEET_SETTINGS,
                Some(row.row),
                Some(row.stable_key),
                row.generation_label,
                expected,
                "必需工作表不能设为不生成",
            ));
        }
        if row.generation != LayoutGenerationMode::Omitted {
            validate_excel_sheet_name(row.row, &row.stable_key, &row.display_name)?;
            let normalized_name = row.display_name.to_lowercase();
            if let Some((previous_key, previous_row)) =
                final_names.insert(normalized_name, (row.stable_key.clone(), row.row))
            {
                return Err(WorkbookLayoutError::mismatch(
                    SHEET_SETTINGS,
                    Some(row.row),
                    Some(row.stable_key),
                    row.display_name,
                    format!("工作表内唯一；该名称已由 {previous_key} 在第 {previous_row} 行使用"),
                    "最终表名重复",
                ));
            }
            if let Some((previous_key, previous_row)) =
                final_orders.insert(row.order, (row.stable_key.clone(), row.row))
            {
                return Err(WorkbookLayoutError::mismatch(
                    SHEET_SETTINGS,
                    Some(row.row),
                    Some(row.stable_key),
                    row.order.to_string(),
                    format!("工作表内唯一；该顺序已由 {previous_key} 在第 {previous_row} 行使用"),
                    "最终工作表顺序重复",
                ));
            }
        }
        sheets.push(WorkbookSheetLayout::new(
            row.stable_key,
            row.generation,
            row.display_name,
            row.order,
            row.freeze_cell,
            row.default_filter,
            row.description,
            row.required,
        ));
    }
    for registered in registry.sheets() {
        if !seen.contains_key(registered.stable_key()) {
            missing.push(format!("sheet:{}", registered.stable_key()));
        }
    }
    Ok(sheets)
}

/// 核对字段模型路径、编辑器、格式和必需性，并嵌入枚举分类。
fn validate_fields(
    rows: Vec<ParsedField>,
    registry: &WorkbookLayoutRegistry,
    sheets: &[WorkbookSheetLayout],
    control_labels: &ControlEnumLabels,
    missing: &mut Vec<String>,
) -> Result<Vec<WorkbookFieldLayout>, WorkbookLayoutError> {
    let generated_sheets: BTreeSet<&str> = sheets
        .iter()
        .filter(|sheet| sheet.generation() != LayoutGenerationMode::Omitted)
        .map(WorkbookSheetLayout::stable_key)
        .collect();
    let mut seen: BTreeMap<(String, String), u32> = BTreeMap::new();
    let mut final_names: BTreeMap<(String, String), (String, u32)> = BTreeMap::new();
    let mut final_orders: BTreeMap<(String, u32), (String, u32)> = BTreeMap::new();
    let mut fields: Vec<WorkbookFieldLayout> = Vec::with_capacity(rows.len());
    for row in rows {
        let key = (row.sheet_key.clone(), row.stable_key.clone());
        let context_key = format!("{}.{}", row.sheet_key, row.stable_key);
        if let Some(previous_row) = seen.insert(key, row.row) {
            return Err(WorkbookLayoutError::invalid(
                FIELD_SETTINGS,
                Some(row.row),
                Some(context_key),
                format!("字段稳定键重复，首次出现于第 {previous_row} 行"),
            ));
        }
        let registered = registry
            .field(&row.sheet_key, &row.stable_key)
            .ok_or_else(|| {
                WorkbookLayoutError::invalid(
                    FIELD_SETTINGS,
                    Some(row.row),
                    Some(context_key.clone()),
                    "字段未在当前程序注册",
                )
            })?;
        if row.model_path != registered.model_path() {
            return Err(WorkbookLayoutError::mismatch(
                FIELD_SETTINGS,
                Some(row.row),
                Some(context_key),
                row.model_path,
                registered.model_path(),
                "锁定的来源模型字段与程序注册表不一致",
            ));
        }
        if row.editor != registered.editor() {
            return Err(WorkbookLayoutError::mismatch(
                FIELD_SETTINGS,
                Some(row.row),
                Some(context_key),
                editor_name(row.editor),
                editor_name(registered.editor()),
                "锁定的编辑器与程序注册表不一致",
            ));
        }
        if row.required != registered.required() {
            return Err(WorkbookLayoutError::mismatch(
                FIELD_SETTINGS,
                Some(row.row),
                Some(context_key),
                yes_no(row.required),
                yes_no(registered.required()),
                "锁定的必需标记与程序注册表不一致",
            ));
        }
        if !registered.allowed_formats().contains(&row.value_format) {
            let expected = value_format_labels(control_labels, registered.allowed_formats())?;
            return Err(WorkbookLayoutError::mismatch(
                FIELD_SETTINGS,
                Some(row.row),
                Some(context_key),
                row.value_format_label,
                expected,
                "字段格式不在程序注册表允许范围",
            ));
        }
        if registered.required() && row.generation == LayoutGenerationMode::Omitted {
            let expected = generation_labels(
                control_labels,
                &[LayoutGenerationMode::Visible, LayoutGenerationMode::Hidden],
            )?;
            return Err(WorkbookLayoutError::mismatch(
                FIELD_SETTINGS,
                Some(row.row),
                Some(context_key),
                row.generation_label,
                expected,
                "必需字段不能设为不生成",
            ));
        }
        if generated_sheets.contains(row.sheet_key.as_str())
            && row.generation != LayoutGenerationMode::Omitted
        {
            if row.display_name.encode_utf16().count() > 255 {
                return Err(WorkbookLayoutError::mismatch(
                    FIELD_SETTINGS,
                    Some(row.row),
                    Some(context_key),
                    row.display_name.encode_utf16().count().to_string(),
                    "不超过 255 个 UTF-16 单元",
                    "最终列名过长",
                ));
            }
            let name_key = (row.sheet_key.clone(), row.display_name.to_lowercase());
            if let Some((previous_key, previous_row)) =
                final_names.insert(name_key, (row.stable_key.clone(), row.row))
            {
                return Err(WorkbookLayoutError::mismatch(
                    FIELD_SETTINGS,
                    Some(row.row),
                    Some(context_key),
                    row.display_name,
                    format!(
                        "同一工作表内唯一；该列名已由 {previous_key} 在第 {previous_row} 行使用"
                    ),
                    "最终列名重复",
                ));
            }
            let order_key = (row.sheet_key.clone(), row.order);
            if let Some((previous_key, previous_row)) =
                final_orders.insert(order_key, (row.stable_key.clone(), row.row))
            {
                return Err(WorkbookLayoutError::mismatch(
                    FIELD_SETTINGS,
                    Some(row.row),
                    Some(context_key),
                    row.order.to_string(),
                    format!(
                        "同一工作表内唯一；该顺序已由 {previous_key} 在第 {previous_row} 行使用"
                    ),
                    "最终字段顺序重复",
                ));
            }
        }
        fields.push(WorkbookFieldLayout::new(
            row.sheet_key,
            row.stable_key,
            row.generation,
            row.display_name,
            row.order,
            row.width,
            row.value_format,
            row.wrap,
            row.description,
            row.model_path,
            row.editor,
            registered.enum_category().map(str::to_owned),
            row.required,
        ));
    }
    for registered in registry.fields() {
        let key = (
            registered.sheet_key().to_owned(),
            registered.stable_key().to_owned(),
        );
        if !seen.contains_key(&key) {
            missing.push(format!(
                "field:{}.{}",
                registered.sheet_key(),
                registered.stable_key()
            ));
        }
    }
    Ok(fields)
}

/// 要求每个程序枚举稳定值在布局中恰好出现一次。
fn validate_enum_options(
    rows: Vec<ParsedEnumOption>,
    registry: &WorkbookLayoutRegistry,
    missing: &mut Vec<String>,
) -> Result<Vec<WorkbookLayoutEnumOption>, WorkbookLayoutError> {
    let registered: BTreeSet<(String, String)> = registry
        .enum_options()
        .iter()
        .map(|option| {
            (
                option.category_key().to_owned(),
                option.stable_value().to_owned(),
            )
        })
        .collect();
    let mut seen: BTreeMap<(String, String), u32> = BTreeMap::new();
    let mut labels: BTreeMap<(String, String), (String, u32)> = BTreeMap::new();
    let mut orders: BTreeMap<(String, u32), (String, u32)> = BTreeMap::new();
    let mut options: Vec<WorkbookLayoutEnumOption> = Vec::with_capacity(rows.len());
    for row in rows {
        let pair = (row.category_key.clone(), row.stable_value.clone());
        let context_key = format!("{}.{}", row.category_key, row.stable_value);
        if let Some(previous_row) = seen.insert(pair.clone(), row.row) {
            return Err(WorkbookLayoutError::invalid(
                FORMAT_SETTINGS,
                Some(row.row),
                Some(context_key),
                format!("枚举稳定值重复，首次出现于第 {previous_row} 行"),
            ));
        }
        if !registered.contains(&pair) {
            return Err(WorkbookLayoutError::invalid(
                FORMAT_SETTINGS,
                Some(row.row),
                Some(context_key),
                "枚举分类或稳定值未在当前程序注册",
            ));
        }
        let label_key = (row.category_key.clone(), row.label.to_lowercase());
        if let Some((previous_value, previous_row)) =
            labels.insert(label_key, (row.stable_value.clone(), row.row))
        {
            return Err(WorkbookLayoutError::mismatch(
                FORMAT_SETTINGS,
                Some(row.row),
                Some(context_key),
                row.label,
                format!(
                    "同一枚举分类内唯一；该标签已由 {previous_value} 在第 {previous_row} 行使用"
                ),
                "枚举标签重复",
            ));
        }
        let order_key = (row.category_key.clone(), row.order);
        if let Some((previous_value, previous_row)) =
            orders.insert(order_key, (row.stable_value.clone(), row.row))
        {
            return Err(WorkbookLayoutError::mismatch(
                FORMAT_SETTINGS,
                Some(row.row),
                Some(context_key),
                row.order.to_string(),
                format!(
                    "同一枚举分类内唯一；该顺序已由 {previous_value} 在第 {previous_row} 行使用"
                ),
                "枚举顺序重复",
            ));
        }
        options.push(WorkbookLayoutEnumOption::new(
            row.category_key,
            row.stable_value,
            row.label,
            row.order,
            row.description,
        ));
    }
    for (category_key, stable_value) in registered {
        if !seen.contains_key(&(category_key.clone(), stable_value.clone())) {
            missing.push(format!("enum:{category_key}.{stable_value}"));
        }
    }
    Ok(options)
}

/// 要求每个程序样式稳定键在布局中恰好出现一次。
fn validate_styles(
    rows: Vec<ParsedStyle>,
    registry: &WorkbookLayoutRegistry,
    missing: &mut Vec<String>,
) -> Result<Vec<WorkbookLayoutStyle>, WorkbookLayoutError> {
    let registered: BTreeSet<&str> = registry.style_keys().iter().map(String::as_str).collect();
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    let mut styles: Vec<WorkbookLayoutStyle> = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(previous_row) = seen.insert(row.stable_key.clone(), row.row) {
            return Err(WorkbookLayoutError::invalid(
                FORMAT_SETTINGS,
                Some(row.row),
                Some(row.stable_key),
                format!("样式稳定键重复，首次出现于第 {previous_row} 行"),
            ));
        }
        if !registered.contains(row.stable_key.as_str()) {
            return Err(WorkbookLayoutError::invalid(
                FORMAT_SETTINGS,
                Some(row.row),
                Some(row.stable_key),
                "样式稳定键未在当前程序注册",
            ));
        }
        styles.push(WorkbookLayoutStyle::new(
            row.stable_key,
            row.background_color,
            row.font_color,
            row.bold,
            row.horizontal_alignment,
            row.vertical_alignment,
            row.wrap,
            row.description,
        ));
    }
    for stable_key in registry.style_keys() {
        if !seen.contains_key(stable_key) {
            missing.push(format!("style:{stable_key}"));
        }
    }
    Ok(styles)
}

/// 执行 Excel 标签长度、引号和禁用字符约束。
fn validate_excel_sheet_name(
    row: u32,
    stable_key: &str,
    display_name: &str,
) -> Result<(), WorkbookLayoutError> {
    if !excel_sheet_tab_name_is_legal(display_name) {
        return Err(WorkbookLayoutError::mismatch(
            SHEET_SETTINGS,
            Some(row),
            Some(stable_key.to_owned()),
            display_name,
            "不超过 31 个 UTF-16 单元、不含 : / \\ ? * [ ] 及其全角兼容形、首尾无单引号且不使用保留名 History",
            "最终表名不符合 Excel 工作表名称约束",
        ));
    }
    Ok(())
}

/// 为锁定布尔值错误生成与模板一致的中文文本。
const fn yes_no(value: bool) -> &'static str {
    if value { "是" } else { "否" }
}

/// 返回配置工作簿使用的编辑器名称。
const fn editor_name(value: LayoutEditor) -> &'static str {
    match value {
        LayoutEditor::ReadOnly => "只读",
        LayoutEditor::Boolean => "是非",
        LayoutEditor::Enumeration => "枚举",
        LayoutEditor::Integer => "整数",
        LayoutEditor::Text => "文本",
    }
}

/// 按当前布局中的可编辑标签描述一组稳定生成方式。
fn generation_labels(
    labels: &ControlEnumLabels,
    values: &[LayoutGenerationMode],
) -> Result<String, WorkbookLayoutError> {
    values
        .iter()
        .map(|value| {
            labels.generation_label(*value).ok_or_else(|| {
                WorkbookLayoutError::invalid(
                    FORMAT_SETTINGS,
                    None,
                    Some("generation_mode".to_owned()),
                    "内部生成方式标签映射不完整",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join("、"))
}

/// 按当前布局中的可编辑标签描述一组稳定值格式。
fn value_format_labels(
    labels: &ControlEnumLabels,
    values: &BTreeSet<LayoutValueFormat>,
) -> Result<String, WorkbookLayoutError> {
    values
        .iter()
        .map(|value| {
            labels.value_format_label(*value).ok_or_else(|| {
                WorkbookLayoutError::invalid(
                    FORMAT_SETTINGS,
                    None,
                    Some("value_format".to_owned()),
                    "内部值格式标签映射不完整",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| values.join("、"))
}

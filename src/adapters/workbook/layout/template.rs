//! 将生产投影注册表写成可复现、可编辑且能被严格加载器验证的 XLSX 布局模板。

use std::collections::BTreeMap;
use std::path::Path;

use crate::application::{
    LAYOUT_SCHEMA_VERSION, LayoutEditor, LayoutGenerationMode, LayoutHorizontalAlignment,
    LayoutValueFormat, LayoutVerticalAlignment, WorkbookLayout, WorkbookLayoutRegistry,
    WorkbookProjectionV4,
};

use super::super::WorkbookProbeError;
use super::super::package::write_new_file_bytes;
#[cfg(test)]
use super::{FIELD_SETTINGS, FORMAT_SETTINGS, SHEET_SETTINGS};
use suzushiro_xlsx_toolkit::paths::validate_new_xlsx_destination;

mod presentation;
mod render;

use presentation::{
    default_field_name, default_field_width, default_field_wrap, enum_presentation,
    field_description, sheet_presentation, single_value_format, style_presentation,
    value_format_label,
};
use render::build_workbook;

const CHECK_RESULT_COLUMN_ORDER: &[&str] = &[
    "checked_at",
    "status",
    "object_ref",
    "message",
    "current_value",
    "expected_value",
];
const EXECUTION_RESULT_COLUMN_ORDER: &[&str] = &[
    "executed_at",
    "step_sequence",
    "step_type",
    "object_ref",
    "status",
    "final_verification_status",
    "may_have_writes",
    "readback_summary",
    "message",
];

fn result_column_order(sheet: &str) -> Option<&'static [&'static str]> {
    match sheet {
        "check_results" => Some(CHECK_RESULT_COLUMN_ORDER),
        "execution_results" => Some(EXECUTION_RESULT_COLUMN_ORDER),
        _ => None,
    }
}

const INVENTORY_COLUMN_ORDER: &[&str] = &[
    "name",
    "equipment_type",
    "quantity",
    "family_warehouse_quantity",
    "craftable_actual",
    "family_owned_enhance_distribution",
    "rarity",
    "tech_level",
    "current_enhance_level",
    "source_type",
    "ship_instance_id",
    "ship_name",
    "slot_index",
    "operation",
    "processing_quantity",
    "target_enhance_level",
    "attributes_json",
    "weapons_json",
    "effect_summary",
    "ammo_type",
    "speciality",
    "compatible_main_ship_types",
    "equipment_limit",
    "nation",
    "gear_score",
    "description",
    "labels",
    "compose_material_costs",
    "next_cost_json",
    "dismantle_yield_json",
    "dismantlable",
    "config_id",
    "read_errors",
];

const TEMPLATE_NAME: &str = "碧蓝航线标准工作簿布局";
const TEMPLATE_PURPOSE: &str = "生成全量只读快照、可编辑配装计划和可核验执行证据";
const TABLE_SHEETS: &str = "LayoutSheets";
const TABLE_FIELDS: &str = "LayoutFields";
const TABLE_INFO: &str = "LayoutInfo";
const TABLE_ENUMS: &str = "LayoutEnums";
const TABLE_STYLES: &str = "LayoutStyles";
const ENUM_TABLE_FIRST_ROW: u32 = 5;
const ENUM_LABEL_COLUMN: u16 = 2;
const GENERATION_REQUIRED_NAME: &str = "AZLW_GenerationRequired";
const GENERATION_OPTIONAL_NAME: &str = "AZLW_GenerationOptional";

/// 与具体 XLSX API 解耦的完整布局模板行集合。
struct LayoutWorkbookTemplate {
    schema_version: u32,
    template_name: String,
    purpose: String,
    sheets: Vec<TemplateSheet>,
    fields: Vec<TemplateField>,
    enum_options: Vec<TemplateEnumOption>,
    styles: Vec<TemplateStyle>,
}

/// 一行工作表配置的完整写出值。
struct TemplateSheet {
    stable_key: String,
    generation_label: String,
    display_name: String,
    order: u32,
    freeze_cell: Option<String>,
    default_filter: bool,
    description: String,
    required: bool,
}

/// 一行字段配置的完整写出值。
struct TemplateField {
    sheet_key: String,
    stable_key: String,
    generation_label: String,
    display_name: String,
    order: u32,
    width: f64,
    value_format: LayoutValueFormat,
    value_format_label: String,
    allowed_format_labels: Vec<String>,
    wrap: bool,
    description: String,
    model_path: String,
    editor: LayoutEditor,
    required: bool,
}

/// 一行枚举配置的完整写出值。
struct TemplateEnumOption {
    category_key: String,
    stable_value: String,
    label: String,
    order: u32,
    description: String,
}

/// 一行样式配置的完整写出值。
struct TemplateStyle {
    stable_key: String,
    background_color: String,
    font_color: String,
    bold: bool,
    horizontal_alignment: String,
    vertical_alignment: String,
    wrap: bool,
    description: String,
}

/// 根据当前生产投影契约排他建立默认布局文件。
pub fn create_default_layout_workbook(path: &Path) -> Result<(), WorkbookProbeError> {
    validate_new_xlsx_destination(path)?;

    let bytes = build_default_layout_workbook_bytes(path)?;
    write_new_file_bytes(path, &bytes)?;
    Ok(())
}

/// 生成当前生产投影的默认布局字节，供默认文件和布局升级共享。
pub(crate) fn build_default_layout_workbook_bytes(
    path: &Path,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let template = LayoutWorkbookTemplate::from_defaults()?;
    build_workbook(path, &template)
}

/// 将已经严格校验的当前布局重新写成完整受控模板。
pub(crate) fn build_layout_workbook_bytes(
    path: &Path,
    layout: &WorkbookLayout,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let template = LayoutWorkbookTemplate::from_layout(layout)?;
    build_workbook(path, &template)
}

#[cfg(test)]
/// 建立缺少指定字段的生产布局，用于验证真实增量升级。
pub(in crate::adapters::workbook) fn build_default_layout_without_field(
    path: &Path,
    sheet_key: &str,
    stable_key: &str,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let mut template = LayoutWorkbookTemplate::from_defaults()?;
    template
        .fields
        .retain(|field| field.sheet_key != sheet_key || field.stable_key != stable_key);
    build_workbook(path, &template)
}

impl LayoutWorkbookTemplate {
    /// 从生产投影及其默认显示定义建立模板行。
    fn from_defaults() -> Result<Self, WorkbookProbeError> {
        let sheets = WorkbookProjectionV4::registered_sheets();
        let fields = WorkbookProjectionV4::registered_fields();
        let enum_options = WorkbookProjectionV4::registered_enum_options();
        let style_keys = WorkbookProjectionV4::registered_style_keys();
        WorkbookLayoutRegistry::new(
            sheets.clone(),
            fields.clone(),
            enum_options.clone(),
            style_keys.clone(),
        )
        .map_err(|source| WorkbookProbeError::LayoutContract { source })?;

        let mut template_sheets = Vec::with_capacity(sheets.len());
        for (index, sheet) in sheets.iter().enumerate() {
            let presentation = sheet_presentation(sheet.stable_key())?;
            template_sheets.push(TemplateSheet {
                stable_key: sheet.stable_key().to_owned(),
                generation_label: generation_default_label(presentation.generation).to_owned(),
                display_name: presentation.display_name.to_owned(),
                order: u32::try_from(index + 1).map_err(integer_overflow)?,
                freeze_cell: Some(
                    match sheet.stable_key() {
                        "loadout_plan" => "D2",
                        "equipment_inventory" => "C2",
                        "ship_technology" => "B2",
                        _ => "A2",
                    }
                    .to_owned(),
                ),
                default_filter: true,
                description: presentation.description.to_owned(),
                required: sheet.required(),
            });
        }

        let ship_column_order = [
            "instance_id",
            "name",
            "acquisition",
            "nation",
            "ship_type",
            "armor_type",
            "locked",
            "fleet_status",
            "current_stars",
            "maximum_stars",
            "level",
            "maximum_level",
            "experience_in_level",
            "next_level_experience",
            "total_experience",
            "technology_bonus",
            "technology_get",
            "technology_upgrade",
            "technology_level",
            "energy",
            "intimacy",
            "intimacy_maximum",
            "proposed",
            "create_time",
            "propose_time",
            "combat_power",
            "oil_total",
            "stat_durability_summary",
            "stat_cannon_summary",
            "stat_air_summary",
            "stat_torpedo_summary",
            "stat_reload_summary",
            "stat_hit_summary",
            "stat_dodge_summary",
            "stat_anti_aircraft_summary",
            "stat_luck_summary",
            "stat_speed_summary",
            "stat_anti_sub_summary",
        ];
        let mut field_orders: BTreeMap<&str, u32> = BTreeMap::new();
        let mut template_fields = Vec::with_capacity(fields.len());
        let mut ordered_fields: Vec<_> = fields.iter().collect();
        ordered_fields.sort_by_key(|field| {
            (
                sheets
                    .iter()
                    .position(|sheet| sheet.stable_key() == field.sheet_key())
                    .expect("注册工作表"),
                !matches!(field.sheet_key(), "loadout_plan" | "equipment_inventory")
                    && field.editor() != LayoutEditor::ReadOnly,
                if let Some(order) = result_column_order(field.sheet_key()) {
                    order
                        .iter()
                        .position(|key| *key == field.stable_key())
                        .unwrap_or(order.len())
                } else if field.sheet_key() == "loadout_plan" {
                    ship_column_order
                        .iter()
                        .position(|key| *key == field.stable_key())
                        .unwrap_or(ship_column_order.len())
                } else if field.sheet_key() == "ship_technology" {
                    crate::application::TECHNOLOGY_VIEW_FIELDS
                        .iter()
                        .position(|key| *key == field.stable_key())
                        .unwrap()
                } else if field.sheet_key() == "equipment_inventory" {
                    INVENTORY_COLUMN_ORDER
                        .iter()
                        .position(|key| *key == field.stable_key())
                        .unwrap_or(INVENTORY_COLUMN_ORDER.len())
                } else {
                    0
                },
            )
        });
        // 每个槽位的更换目标紧邻当前装备，其余信息保留在同一槽位内。
        for slot in 1..=5 {
            let target_key = format!("slot_{slot}_target_equipment_family");
            let current_key = format!("slot_{slot}_equipment_name");
            let target = ordered_fields
                .iter()
                .position(|field| {
                    field.sheet_key() == "loadout_plan" && field.stable_key() == target_key
                })
                .expect("注册槽位目标");
            let field = ordered_fields.remove(target);
            let current = ordered_fields
                .iter()
                .position(|field| {
                    field.sheet_key() == "loadout_plan" && field.stable_key() == current_key
                })
                .expect("注册当前装备");
            ordered_fields.insert(current + 1, field);
        }
        for field in ordered_fields {
            let order = field_orders.entry(field.sheet_key()).or_default();
            *order = order
                .checked_add(1)
                .ok_or_else(|| template_error(format!("{} 字段顺序溢出", field.sheet_key())))?;
            let value_format = single_value_format(field)?;
            let display_name = default_field_name(field.sheet_key(), field.stable_key())?;
            let description =
                field_description(field.sheet_key(), field.stable_key(), &display_name)?;
            template_fields.push(TemplateField {
                sheet_key: field.sheet_key().to_owned(),
                stable_key: field.stable_key().to_owned(),
                generation_label: generation_default_label(default_field_generation(field))
                    .to_owned(),
                display_name,
                order: *order,
                width: default_field_width(field.stable_key(), value_format, field.editor()),
                value_format,
                value_format_label: value_format_label(value_format).to_owned(),
                allowed_format_labels: field
                    .allowed_formats()
                    .iter()
                    .map(|format| value_format_label(*format).to_owned())
                    .collect(),
                wrap: default_field_wrap(field.stable_key(), value_format),
                description,
                model_path: field.model_path().to_owned(),
                editor: field.editor(),
                required: field.required(),
            });
        }

        let mut category_orders: BTreeMap<&str, u32> = BTreeMap::new();
        let mut template_enum_options = Vec::with_capacity(enum_options.len());
        for option in &enum_options {
            let order = category_orders.entry(option.category_key()).or_default();
            *order = order
                .checked_add(1)
                .ok_or_else(|| template_error(format!("{} 枚举顺序溢出", option.category_key())))?;
            let presentation = enum_presentation(option.category_key(), option.stable_value())?;
            template_enum_options.push(TemplateEnumOption {
                category_key: option.category_key().to_owned(),
                stable_value: option.stable_value().to_owned(),
                label: presentation.label.to_owned(),
                order: *order,
                description: presentation.description.to_owned(),
            });
        }

        let mut template_styles = Vec::with_capacity(style_keys.len());
        for key in style_keys {
            let presentation = style_presentation(&key)?;
            template_styles.push(TemplateStyle {
                stable_key: key,
                background_color: presentation.background.to_owned(),
                font_color: presentation.font.to_owned(),
                bold: presentation.bold,
                horizontal_alignment: presentation.horizontal.to_owned(),
                vertical_alignment: presentation.vertical.to_owned(),
                wrap: presentation.wrap,
                description: presentation.description.to_owned(),
            });
        }

        Ok(Self {
            schema_version: LAYOUT_SCHEMA_VERSION,
            template_name: TEMPLATE_NAME.to_owned(),
            purpose: TEMPLATE_PURPOSE.to_owned(),
            sheets: template_sheets,
            fields: template_fields,
            enum_options: template_enum_options,
            styles: template_styles,
        })
    }

    /// 按生产投影的物理行顺序读取用户已经校验的全部配置值。
    fn from_layout(layout: &WorkbookLayout) -> Result<Self, WorkbookProbeError> {
        let registered_sheets = WorkbookProjectionV4::registered_sheets();
        let registered_fields = WorkbookProjectionV4::registered_fields();
        let registered_enum_options = WorkbookProjectionV4::registered_enum_options();
        let registered_style_keys = WorkbookProjectionV4::registered_style_keys();

        let mut sheets = Vec::with_capacity(registered_sheets.len());
        for registered in &registered_sheets {
            let sheet = layout
                .sheets()
                .iter()
                .find(|sheet| sheet.stable_key() == registered.stable_key())
                .ok_or_else(|| {
                    template_error(format!("严格布局缺少工作表 {}", registered.stable_key()))
                })?;
            sheets.push(TemplateSheet {
                stable_key: sheet.stable_key().to_owned(),
                generation_label: layout_control_label(
                    layout,
                    "generation_mode",
                    LayoutGenerationMode::stable_value(sheet.generation()),
                )?
                .to_owned(),
                display_name: sheet.display_name().to_owned(),
                order: sheet.order(),
                freeze_cell: sheet.freeze_cell().map(str::to_owned),
                default_filter: sheet.default_filter(),
                description: sheet.description().to_owned(),
                required: sheet.required(),
            });
        }

        let mut fields = Vec::with_capacity(registered_fields.len());
        for registered in &registered_fields {
            let field = layout
                .fields()
                .iter()
                .find(|field| {
                    field.sheet_key() == registered.sheet_key()
                        && field.stable_key() == registered.stable_key()
                })
                .ok_or_else(|| {
                    template_error(format!(
                        "严格布局缺少字段 {}.{}",
                        registered.sheet_key(),
                        registered.stable_key()
                    ))
                })?;
            fields.push(TemplateField {
                sheet_key: field.sheet_key().to_owned(),
                stable_key: field.stable_key().to_owned(),
                generation_label: layout_control_label(
                    layout,
                    "generation_mode",
                    LayoutGenerationMode::stable_value(field.generation()),
                )?
                .to_owned(),
                display_name: field.display_name().to_owned(),
                order: field.order(),
                width: f64::from(field.width().hundredths()) / 100.0,
                value_format: field.value_format(),
                value_format_label: layout_control_label(
                    layout,
                    "value_format",
                    LayoutValueFormat::stable_value(field.value_format()),
                )?
                .to_owned(),
                allowed_format_labels: registered
                    .allowed_formats()
                    .iter()
                    .map(|format| {
                        layout_control_label(
                            layout,
                            "value_format",
                            LayoutValueFormat::stable_value(*format),
                        )
                        .map(str::to_owned)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                wrap: field.wrap(),
                description: field.description().to_owned(),
                model_path: field.model_path().to_owned(),
                editor: field.editor(),
                required: field.required(),
            });
        }

        let mut enum_options = Vec::with_capacity(registered_enum_options.len());
        for registered in &registered_enum_options {
            let option = layout
                .enum_options()
                .iter()
                .find(|option| {
                    option.category_key() == registered.category_key()
                        && option.stable_value() == registered.stable_value()
                })
                .ok_or_else(|| {
                    template_error(format!(
                        "严格布局缺少枚举 {}.{}",
                        registered.category_key(),
                        registered.stable_value()
                    ))
                })?;
            enum_options.push(TemplateEnumOption {
                category_key: option.category_key().to_owned(),
                stable_value: option.stable_value().to_owned(),
                label: option.label().to_owned(),
                order: option.order(),
                description: option.description().to_owned(),
            });
        }

        let mut styles = Vec::with_capacity(registered_style_keys.len());
        for registered in registered_style_keys {
            let style = layout
                .styles()
                .iter()
                .find(|style| style.stable_key() == registered)
                .ok_or_else(|| template_error(format!("严格布局缺少样式 {registered}")))?;
            styles.push(TemplateStyle {
                stable_key: style.stable_key().to_owned(),
                background_color: style.background_color().to_owned(),
                font_color: style.font_color().to_owned(),
                bold: style.bold(),
                horizontal_alignment: horizontal_alignment_label(style.horizontal_alignment())
                    .to_owned(),
                vertical_alignment: vertical_alignment_label(style.vertical_alignment()).to_owned(),
                wrap: style.wrap(),
                description: style.description().to_owned(),
            });
        }

        Ok(Self {
            schema_version: layout.schema_version(),
            template_name: layout.template_name().to_owned(),
            purpose: layout.purpose().to_owned(),
            sheets,
            fields,
            enum_options,
            styles,
        })
    }
}

fn layout_control_label<'a>(
    layout: &'a WorkbookLayout,
    category_key: &str,
    stable_value: &str,
) -> Result<&'a str, WorkbookProbeError> {
    layout
        .enum_options()
        .iter()
        .find(|option| {
            option.category_key() == category_key && option.stable_value() == stable_value
        })
        .map(|option| option.label())
        .ok_or_else(|| {
            template_error(format!(
                "严格布局缺少控制枚举 {category_key}.{stable_value}"
            ))
        })
}

fn generation_default_label(value: LayoutGenerationMode) -> &'static str {
    match value {
        LayoutGenerationMode::Visible => "显示",
        LayoutGenerationMode::Hidden => "隐藏",
        LayoutGenerationMode::Omitted => "不生成",
    }
}

fn horizontal_alignment_label(value: LayoutHorizontalAlignment) -> &'static str {
    match value {
        LayoutHorizontalAlignment::Left => "左",
        LayoutHorizontalAlignment::Center => "中",
        LayoutHorizontalAlignment::Right => "右",
    }
}

fn vertical_alignment_label(value: LayoutVerticalAlignment) -> &'static str {
    match value {
        LayoutVerticalAlignment::Top => "上",
        LayoutVerticalAlignment::Center => "中",
        LayoutVerticalAlignment::Bottom => "下",
    }
}

fn integer_overflow(error: impl std::fmt::Display) -> WorkbookProbeError {
    template_error(format!("整数转换失败: {error}"))
}

fn template_error(message: impl Into<String>) -> WorkbookProbeError {
    WorkbookProbeError::LayoutTemplate {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;

/// 默认只展示日常操作输入，完整字段清单仍保留在布局模板中供选择。
fn default_field_generation(
    field: &crate::application::RegisteredLayoutField,
) -> LayoutGenerationMode {
    if let Some(order) = result_column_order(field.sheet_key()) {
        return if order.contains(&field.stable_key()) {
            LayoutGenerationMode::Visible
        } else if field.required() {
            LayoutGenerationMode::Hidden
        } else {
            LayoutGenerationMode::Omitted
        };
    }
    if field.sheet_key() == "equipment_inventory" {
        return if INVENTORY_COLUMN_ORDER.contains(&field.stable_key()) {
            LayoutGenerationMode::Visible
        } else {
            LayoutGenerationMode::Omitted
        };
    }
    if field.sheet_key() == "loadout_plan" && field.editor() == LayoutEditor::ReadOnly {
        let key = field.stable_key();
        if key == "group_id" || (key.starts_with("slot_") && !key.ends_with("_equipment_name")) {
            return LayoutGenerationMode::Omitted;
        }
        if matches!(
            key,
            "original_name"
                | "source_type"
                | "source_ref"
                | "static_summary"
                | "static_raw_ref"
                | "config_id"
                | "skin_id"
                | "intimacy_stage"
                | "read_errors"
                | "rarity"
                | "proficiency"
                | "oil_start"
                | "oil_end"
                | "learned_skill_count"
                | "data_complete"
        ) || (key.starts_with("stat_") && !key.ends_with("_summary"))
            || (key.starts_with("skills_")
                && !matches!(
                    key,
                    "skills_progress_summary" | "skills_description_summary"
                ))
        {
            return LayoutGenerationMode::Omitted;
        }
    }
    if field.editor() == LayoutEditor::ReadOnly {
        return LayoutGenerationMode::Visible;
    }
    let key = field.stable_key();
    let enabled = match field.sheet_key() {
        "loadout_plan" => key == "technology_bonus" || key.ends_with("_target_equipment_family"),
        "equipment_inventory" => matches!(
            key,
            "operation" | "processing_quantity" | "target_enhance_level"
        ),
        _ => true,
    };
    if enabled {
        LayoutGenerationMode::Visible
    } else {
        LayoutGenerationMode::Omitted
    }
}

/// 完整输入布局用于覆盖可选细项；不改变分发模板的默认生成选择。
#[cfg(test)]
pub(in crate::adapters::workbook) fn full_test_layout() -> WorkbookLayout {
    let mut template = LayoutWorkbookTemplate::from_defaults().unwrap();
    for sheet in &mut template.sheets {
        if sheet.stable_key == "raw_data" {
            sheet.generation_label =
                generation_default_label(LayoutGenerationMode::Hidden).to_owned();
        }
    }
    for field in &mut template.fields {
        field.generation_label = generation_default_label(LayoutGenerationMode::Visible).to_owned();
    }
    let path = Path::new("full-input-layout.xlsx");
    let bytes = build_workbook(path, &template).unwrap();
    super::load_layout_snapshot(
        path,
        &bytes,
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap()
}

/// 构建含科技分类的布局，验证既有分类工作簿的读写契约。
#[cfg(test)]
pub(in crate::adapters::workbook) fn technology_test_layout() -> WorkbookLayout {
    let mut template = LayoutWorkbookTemplate::from_defaults().unwrap();
    template
        .sheets
        .iter_mut()
        .find(|sheet| sheet.stable_key == "ship_technology")
        .unwrap()
        .generation_label = generation_default_label(LayoutGenerationMode::Visible).to_owned();
    let path = Path::new("technology-layout.xlsx");
    let bytes = build_workbook(path, &template).unwrap();
    super::load_layout_snapshot(
        path,
        &bytes,
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap()
}

//! 将稳定工作簿投影按严格布局写入 XLSX，并独立重载验证完整语义。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;
use suzushiro_xlsx_toolkit::limits::MAX_EXCEL_CELL_UTF16_UNITS;

use calamine::{Reader as CalamineReader, Xlsx, open_workbook_from_rs};
use rust_xlsxwriter::{
    Color, DocProperties, ExcelDateTime, FormatUnderline, Note, Url, Workbook, Worksheet,
};
use serde::Serialize;

use crate::application::{
    LayoutGenerationMode, WORKBOOK_PROJECTION_SCHEMA_VERSION, WorkbookFieldLayout, WorkbookLayout,
    WorkbookProjectionRow, WorkbookProjectionSheet, WorkbookProjectionV4, WorkbookProjectionValue,
    WorkbookSheetLayout,
};
use suzushiro_content_digest::sha256_bytes;

use super::WorkbookProbeError;
use super::editor::{reject_external_content, reject_macro_content};
use super::package::PackageSnapshot;
use super::rendering::{
    SchemaValues, WorkbookFormats, add_layout_worksheet, build_error, define_dictionary_names,
    dictionary_ranges, finalize_generated_workbook, finish_layout_worksheet, integer_overflow,
    invalid_generated_workbook, omitted_items_json, workbook_sheet_states, write_dictionary_rows,
    write_schema_rows,
};
use super::ship_wiki::ship_wiki_url;
use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;

mod equipment_choices;
pub(in crate::adapters::workbook) mod technology_views;
use technology_views::output_sheets;
mod package_validation;
mod semantic_validation;

use package_validation::{
    read_table_definitions, verify_data_validations, verify_dictionary_names,
    verify_table_definition,
};
pub(in crate::adapters::workbook) use package_validation::{
    reject_cell_formulas, reject_registered_cell_formulas, verify_physical_table_cells,
};
pub(in crate::adapters::workbook) use semantic_validation::verify_projection_cell;
#[cfg(test)]
use semantic_validation::{assert_datetime_cell, assert_number_cell};
use semantic_validation::{
    verify_dictionary_values, verify_headers_and_extent, verify_projection_values,
    verify_schema_values,
};

const ARTIFACT_NAME: &str = "data-workbook.xlsx";
/// 图鉴链接字色：深蓝，配合单下划线标明可点击，并保留单元格原有底色。
const SHIP_WIKI_LINK_COLOR: u32 = 0x1F4E79;
const EXCEL_UNIX_EPOCH_DAYS: f64 = 25_569.0;
const MILLIS_PER_DAY: f64 = 86_400_000.0;
const SEMANTIC_HASH_CONTRACT: &str = "azurlane-workbook-semantic-v1";

/// 数据工作簿字节及应用报告所需的稳定计数。
pub(in crate::adapters::workbook) struct ProjectionWorkbookBuild {
    pub(in crate::adapters::workbook) bytes: Vec<u8>,
    pub(in crate::adapters::workbook) workbook_semantic_sha256: String,
    pub(in crate::adapters::workbook) generated_at_unix_millis: i64,
    pub(in crate::adapters::workbook) generated_sheets: usize,
    pub(in crate::adapters::workbook) hidden_sheets: usize,
    pub(in crate::adapters::workbook) omitted_sheets: usize,
    pub(in crate::adapters::workbook) generated_fields: usize,
    pub(in crate::adapters::workbook) hidden_fields: usize,
    pub(in crate::adapters::workbook) omitted_fields: usize,
    pub(in crate::adapters::workbook) projected_rows: usize,
    pub(in crate::adapters::workbook) dictionary_rows: usize,
    pub(in crate::adapters::workbook) schema_rows: usize,
}

/// 从既有工作簿独立读取出的生成时间和语义行数。
pub(in crate::adapters::workbook) struct ProjectionWorkbookEvidence {
    pub(in crate::adapters::workbook) generated_at_unix_millis: i64,
    pub(in crate::adapters::workbook) projected_rows: usize,
    pub(in crate::adapters::workbook) dictionary_rows: usize,
    pub(in crate::adapters::workbook) schema_rows: usize,
}

#[derive(Serialize)]
struct WorkbookSemanticDigest<'a> {
    contract: &'static str,
    layout_schema_version: u32,
    layout_content_sha256: &'a str,
    projection_schema_version: u32,
    projection_registry_sha256: &'a str,
    projection_content_sha256: &'a str,
    game_state_content_sha256: &'a str,
}

/// 计算不包含自身、输出路径、ZIP 元数据和生成时间的稳定工作簿语义摘要。
pub(in crate::adapters::workbook) fn workbook_semantic_sha256(
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<String, WorkbookProbeError> {
    let input = WorkbookSemanticDigest {
        contract: SEMANTIC_HASH_CONTRACT,
        layout_schema_version: layout.schema_version(),
        layout_content_sha256: layout.content_sha256(),
        projection_schema_version: projection.schema_version(),
        projection_registry_sha256: projection.registry_sha256(),
        projection_content_sha256: projection.content_sha256(),
        game_state_content_sha256: projection.source().game_state_content_sha256(),
    };
    let bytes = serde_json::to_vec(&input)
        .map_err(|error| build_error(format!("编码工作簿语义摘要失败: {error}")))?;
    Ok(sha256_bytes(&bytes))
}

/// 将布局和投影物化为经过规范化、重载验证的数据工作簿字节。
pub(in crate::adapters::workbook) fn build_projection_workbook_bytes(
    path: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    generated_at_unix_millis: i64,
) -> Result<ProjectionWorkbookBuild, WorkbookProbeError> {
    build_projection_workbook_bytes_with_progress(
        path,
        layout,
        projection,
        generated_at_unix_millis,
        &mut |_| {},
    )
}

pub(in crate::adapters::workbook) fn build_projection_workbook_bytes_with_progress(
    path: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    generated_at_unix_millis: i64,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
) -> Result<ProjectionWorkbookBuild, WorkbookProbeError> {
    build_projection_workbook_bytes_filtered(
        path,
        layout,
        projection,
        generated_at_unix_millis,
        None,
        progress,
    )
}

/// 只物化执行写回要合并的工作表，供字符串、样式和表页合并使用。
pub(in crate::adapters::workbook) fn build_refreshed_sheet_donor(
    path: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    generated_at_unix_millis: i64,
    template_keys: &[&str],
) -> Result<ProjectionWorkbookBuild, WorkbookProbeError> {
    build_projection_workbook_bytes_filtered(
        path,
        layout,
        projection,
        generated_at_unix_millis,
        Some(template_keys),
        &mut |_| {},
    )
}

fn build_projection_workbook_bytes_filtered(
    path: &Path,
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    generated_at_unix_millis: i64,
    included_template_keys: Option<&[&str]>,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
) -> Result<ProjectionWorkbookBuild, WorkbookProbeError> {
    use crate::application::OperationProgress;
    progress(OperationProgress::stage("正在校验工作簿布局与数据"));
    validate_layout_projection(layout, projection)?;
    validate_final_strings(layout, projection)?;

    let semantic_sha256 = workbook_semantic_sha256(layout, projection)?;
    let generated_at = excel_datetime_from_unix_millis(generated_at_unix_millis)?;
    // 文档属性序列化需要年月日字段；仅含 Excel 序列值的日期对象只适合写入单元格。
    let document_properties_datetime =
        ExcelDateTime::from_timestamp(generated_at_unix_millis.div_euclid(1_000))?;
    let schema_values = SchemaValues {
        workbook_hash: &semantic_sha256,
        plan_hash: "",
        snapshot_hash: projection.source().game_state_content_sha256(),
        formula_hash: "",
        formula_key: "",
        created_at: &generated_at,
    };
    let dictionary_ranges = dictionary_ranges(layout)?;
    let omitted_items = omitted_items_json(layout)?;
    validate_cell_text("schema.omitted_items", &omitted_items)?;
    let formats = WorkbookFormats::new(layout)?;
    let enum_labels = enum_labels(layout);
    let composable = projection.directly_composable_equipment_configs();
    let mut outputs = output_sheets(layout, projection)?;
    if let Some(keys) = included_template_keys {
        outputs.retain(|output| {
            let key = output.layout.stable_key();
            keys.contains(&crate::application::technology_template_key(key)) || keys.contains(&key)
        });
    }
    let generated_sheets: Vec<&WorkbookSheetLayout> = outputs
        .iter()
        .map(|output| output.layout.as_ref())
        .collect();
    let active_key = generated_sheets
        .iter()
        .find(|sheet| sheet.generation() == LayoutGenerationMode::Visible)
        .or_else(|| generated_sheets.first())
        .map(|sheet| sheet.stable_key())
        .ok_or_else(|| {
            build_error(if included_template_keys.is_none() {
                "布局至少需要一张可见工作表"
            } else {
                "没有需要合并的刷新工作表"
            })
        })?;

    let mut workbook = Workbook::new();
    let properties = DocProperties::new()
        .set_title("碧蓝航线数据工作簿")
        .set_subject("完整游戏状态稳定投影")
        .set_author("AzurLaneWorkbook")
        .set_company("AzurLaneWorkbook")
        .set_creation_datetime(&document_properties_datetime);
    workbook.set_properties(&properties);

    let mut projected_rows = 0_usize;
    let mut dictionary_rows = 0_usize;
    let mut schema_rows = 0_usize;
    for (index, sheet) in generated_sheets.iter().enumerate() {
        progress(OperationProgress::counted(
            format!("正在写入工作表 {}", sheet.stable_key()),
            index,
            generated_sheets.len(),
        ));
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let projection_sheet = outputs[index].projection.as_ref();
        let worksheet = add_layout_worksheet(&mut workbook, sheet, &fields)?;
        if sheet.stable_key() == active_key
            && let Some(field) = fields.first()
        {
            let note = Note::new(format!(
                "{}\n\n此表为游戏数据快照，不会实时同步。生成时间见 schema 表 created_at（Unix 毫秒：{generated_at_unix_millis}）。\n直接操作游戏后，请重新生成工作簿读取最新状态；已有文件与计划保留。",
                field.description()
            )).set_author("AzurLaneWorkbook").set_visible(true).set_width(360).set_height(140);
            worksheet.insert_note(0, 0, &note)?;
        }
        let row_count = match sheet.stable_key() {
            "dictionaries" => {
                let count = write_dictionary_rows(worksheet, layout, &fields, &formats)?
                    + write_projection_rows(
                        worksheet,
                        sheet,
                        &fields,
                        projection_sheet,
                        &composable,
                        &formats,
                        &enum_labels,
                        layout.enum_options().len(),
                    )?;
                dictionary_rows = count;
                count
            }
            "schema" => {
                let count = write_schema_rows(
                    worksheet,
                    layout,
                    &fields,
                    &formats,
                    &omitted_items,
                    &schema_values,
                )?;
                schema_rows = count;
                count
            }
            _ => {
                let count = write_projection_rows(
                    worksheet,
                    sheet,
                    &fields,
                    projection_sheet,
                    &composable,
                    &formats,
                    &enum_labels,
                    0,
                )?;
                projected_rows = projected_rows
                    .checked_add(count)
                    .ok_or_else(|| build_error("投影数据行计数溢出"))?;
                count
            }
        };
        if row_count == 0 {
            write_empty_table_row(worksheet, &fields, &formats)?;
        }
        finish_layout_worksheet(
            worksheet,
            sheet,
            &fields,
            &formats,
            &dictionary_ranges,
            row_count,
        )?;
        if sheet.stable_key() == "loadout_plan" {
            equipment_choices::add_equipment_validations(worksheet, layout, projection, &fields)?;
        }
        if sheet.stable_key() == active_key {
            worksheet.set_active(true);
        }
        if sheet.generation() == LayoutGenerationMode::Hidden {
            worksheet.set_hidden(true);
        }
    }
    progress(OperationProgress::counted(
        "工作表写入完成",
        generated_sheets.len(),
        generated_sheets.len(),
    ));
    define_dictionary_names(&mut workbook, layout, &dictionary_ranges)?;

    progress(OperationProgress::stage("正在压缩工作簿"));
    let bytes = workbook
        .save_to_buffer()
        .map_err(WorkbookProbeError::from)?;
    progress(OperationProgress::stage("正在规范化并校验工作簿内容"));
    let bytes = finalize_generated_workbook(&bytes, path)?;
    if included_template_keys.is_some() {
        let configured_fields = layout
            .fields()
            .iter()
            .filter(|field| {
                field.generation() != LayoutGenerationMode::Omitted
                    && layout.sheets().iter().any(|sheet| {
                        sheet.stable_key() == field.sheet_key()
                            && sheet.generation() != LayoutGenerationMode::Omitted
                    })
            })
            .count();
        let hidden_fields = generated_sheets
            .iter()
            .flat_map(|sheet| layout.generated_fields_for_sheet(sheet.stable_key()))
            .filter(|field| field.generation() == LayoutGenerationMode::Hidden)
            .count();
        return Ok(ProjectionWorkbookBuild {
            bytes,
            workbook_semantic_sha256: semantic_sha256,
            generated_at_unix_millis,
            generated_sheets: generated_sheets.len(),
            hidden_sheets: generated_sheets
                .iter()
                .filter(|sheet| sheet.generation() == LayoutGenerationMode::Hidden)
                .count(),
            omitted_sheets: layout
                .sheets()
                .iter()
                .filter(|sheet| sheet.generation() == LayoutGenerationMode::Omitted)
                .count(),
            generated_fields: generated_sheets
                .iter()
                .map(|sheet| layout.generated_fields_for_sheet(sheet.stable_key()).len())
                .sum(),
            hidden_fields,
            omitted_fields: layout.fields().len() - configured_fields,
            projected_rows,
            dictionary_rows,
            schema_rows,
        });
    }
    let evidence = verify_projection_workbook(path, &bytes, layout, projection)?;
    if evidence.generated_at_unix_millis != generated_at_unix_millis
        || evidence.projected_rows != projected_rows
        || evidence.dictionary_rows != dictionary_rows
        || evidence.schema_rows != schema_rows
    {
        return Err(build_error("数据工作簿重载证据与写入结果不一致"));
    }

    let configured_fields = layout
        .fields()
        .iter()
        .filter(|field| {
            field.generation() != LayoutGenerationMode::Omitted
                && layout.sheets().iter().any(|sheet| {
                    sheet.stable_key() == field.sheet_key()
                        && sheet.generation() != LayoutGenerationMode::Omitted
                })
        })
        .count();
    let hidden_fields = generated_sheets
        .iter()
        .flat_map(|sheet| layout.generated_fields_for_sheet(sheet.stable_key()))
        .filter(|field| field.generation() == LayoutGenerationMode::Hidden)
        .count();
    Ok(ProjectionWorkbookBuild {
        bytes,
        workbook_semantic_sha256: semantic_sha256,
        generated_at_unix_millis,
        generated_sheets: generated_sheets.len(),
        hidden_sheets: generated_sheets
            .iter()
            .filter(|sheet| sheet.generation() == LayoutGenerationMode::Hidden)
            .count(),
        omitted_sheets: layout
            .sheets()
            .iter()
            .filter(|sheet| sheet.generation() == LayoutGenerationMode::Omitted)
            .count(),
        generated_fields: generated_sheets
            .iter()
            .map(|sheet| layout.generated_fields_for_sheet(sheet.stable_key()).len())
            .sum(),
        hidden_fields,
        omitted_fields: layout.fields().len() - configured_fields,
        projected_rows,
        dictionary_rows,
        schema_rows,
    })
}

/// 用受限包解析器和独立语义读取器核对正式数据工作簿。
pub(in crate::adapters::workbook) fn verify_projection_workbook(
    path: &Path,
    bytes: &[u8],
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<ProjectionWorkbookEvidence, WorkbookProbeError> {
    verify_projection_workbook_contents(path, bytes, layout, projection, false)
}

/// 验证更新后的注册表内容，同时允许原包中独立保留的额外用户工作表。
pub(in crate::adapters::workbook) fn verify_refreshed_projection_workbook(
    path: &Path,
    bytes: &[u8],
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<ProjectionWorkbookEvidence, WorkbookProbeError> {
    verify_projection_workbook_contents(path, bytes, layout, projection, true)
}

fn verify_projection_workbook_contents(
    path: &Path,
    bytes: &[u8],
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
    preserve_extra_sheets: bool,
) -> Result<ProjectionWorkbookEvidence, WorkbookProbeError> {
    validate_layout_projection(layout, projection)?;
    let expected_semantic_sha256 = workbook_semantic_sha256(layout, projection)?;
    let package = PackageSnapshot::from_bytes(bytes, path)?;
    reject_external_content(&package)?;
    reject_macro_content(&package)?;
    if preserve_extra_sheets {
        reject_registered_cell_formulas(&package, layout)?;
    } else {
        reject_cell_formulas(&package)?;
    }
    verify_dictionary_names(&package, layout)?;
    let outputs = output_sheets(layout, projection)?;
    verify_data_validations(&package, layout, projection, &outputs)?;
    let expected_sheets: Vec<&WorkbookSheetLayout> = outputs
        .iter()
        .map(|output| output.layout.as_ref())
        .collect();
    let states = workbook_sheet_states(package.part("xl/workbook.xml")?, ARTIFACT_NAME)?;
    if !preserve_extra_sheets && states.len() != expected_sheets.len() {
        return Err(invalid(format!(
            "工作表数量错误: expected={}, actual={}",
            expected_sheets.len(),
            states.len()
        )));
    }
    let registered_states: Vec<_> = states
        .iter()
        .filter(|actual| {
            !preserve_extra_sheets
                || expected_sheets
                    .iter()
                    .any(|sheet| sheet.display_name() == actual.name)
        })
        .collect();
    if registered_states.len() != expected_sheets.len() {
        return Err(invalid("注册工作表数量与布局不一致"));
    }
    for (actual, expected) in registered_states.iter().zip(&expected_sheets) {
        let expected_hidden = expected.generation() == LayoutGenerationMode::Hidden;
        if actual.name != expected.display_name() || actual.hidden != expected_hidden {
            return Err(invalid(format!(
                "工作表状态错误: expected={} hidden={}, actual={} hidden={}",
                expected.display_name(),
                expected_hidden,
                actual.name,
                actual.hidden
            )));
        }
    }

    let tables = read_table_definitions(&package)?;
    let expected_technology_tables: BTreeSet<_> = expected_sheets
        .iter()
        .filter(|sheet| sheet.stable_key().starts_with("ship_technology:"))
        .map(|sheet| super::rendering::table_name(sheet))
        .collect();
    if tables.keys().any(|name| {
        name.starts_with("AZLW_ship_technology_") && !expected_technology_tables.contains(name)
    }) {
        return Err(invalid("科技分类与当前状态不一致，请重新生成工作簿"));
    }

    if !preserve_extra_sheets && tables.len() != expected_sheets.len() {
        return Err(invalid(format!(
            "Excel 表格数量错误: expected={}, actual={}",
            expected_sheets.len(),
            tables.len()
        )));
    }
    let mut workbook: Xlsx<Cursor<&[u8]>> = open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
    let expected_names: Vec<String> = expected_sheets
        .iter()
        .map(|sheet| sheet.display_name().to_owned())
        .collect();
    if !preserve_extra_sheets && workbook.sheet_names() != expected_names {
        return Err(invalid("语义读取的工作表顺序与布局不一致"));
    }

    let enum_labels = enum_labels(layout);
    let mut projected_rows = 0_usize;
    let mut dictionary_rows = 0_usize;
    let mut schema_rows = 0_usize;
    let mut generated_at_unix_millis = None;
    for (index, sheet) in expected_sheets.into_iter().enumerate() {
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        let projection_sheet = outputs[index].projection.as_ref();
        let expected_rows = match sheet.stable_key() {
            "dictionaries" => layout.enum_options().len() + projection_sheet.rows().len(),
            "schema" => layout.fields().len(),
            _ => projection_sheet.rows().len(),
        };
        verify_table_definition(&tables, sheet, &fields, expected_rows)?;
        let worksheet_part = worksheet_part_name(&package, sheet.display_name())?;
        if sheet.stable_key().starts_with("ship_technology:") {
            package_validation::reject_part_cell_formulas(&package, &worksheet_part)?;
        }
        verify_physical_table_cells(&package, &worksheet_part, expected_rows, fields.len())?;
        let range = workbook
            .worksheet_range(sheet.display_name())
            .map_err(|source| WorkbookProbeError::XlsxRead { source })?;
        verify_headers_and_extent(sheet, &fields, expected_rows, &range)?;
        match sheet.stable_key() {
            "dictionaries" => {
                verify_dictionary_values(layout, &fields, &range)?;
                verify_projection_values(
                    sheet,
                    &fields,
                    projection_sheet,
                    &range,
                    &enum_labels,
                    layout.enum_options().len(),
                )?;
                dictionary_rows = expected_rows;
            }
            "schema" => {
                let generated_at = verify_schema_values(
                    layout,
                    &fields,
                    &range,
                    projection,
                    &expected_semantic_sha256,
                )?;
                generated_at_unix_millis = Some(generated_at);
                schema_rows = expected_rows;
            }
            _ => {
                verify_projection_values(
                    sheet,
                    &fields,
                    projection_sheet,
                    &range,
                    &enum_labels,
                    0,
                )?;
                projected_rows = projected_rows
                    .checked_add(expected_rows)
                    .ok_or_else(|| invalid("投影数据行计数溢出"))?;
            }
        }
    }
    let generated_at_unix_millis =
        generated_at_unix_millis.ok_or_else(|| invalid("schema 工作表没有提供生成时间"))?;
    Ok(ProjectionWorkbookEvidence {
        generated_at_unix_millis,
        projected_rows,
        dictionary_rows,
        schema_rows,
    })
}

/// 核对布局、投影注册项和由写入器合成的技术表边界。
fn validate_layout_projection(
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<(), WorkbookProbeError> {
    if projection.schema_version() != WORKBOOK_PROJECTION_SCHEMA_VERSION {
        return Err(build_error(format!(
            "工作簿投影版本错误: expected={WORKBOOK_PROJECTION_SCHEMA_VERSION}, actual={}",
            projection.schema_version()
        )));
    }
    if !layout
        .sheets()
        .iter()
        .any(|sheet| sheet.generation() == LayoutGenerationMode::Visible)
    {
        return Err(build_error("布局至少需要一张可见工作表"));
    }
    if layout.read_scope().ship_technology() && !projection.source().read_scope().ship_technology()
    {
        return Err(build_error(
            "模板请求舰船科技，但本次状态未读取；请按当前模板重新读取",
        ));
    }
    if layout.read_scope().ship_skill_effects()
        && !projection.source().read_scope().ship_skill_effects()
    {
        return Err(build_error(
            "模板请求舰船底层技能效果，但本次状态未读取；请按当前模板重新读取",
        ));
    }
    let requested = layout.read_scope();
    let actual = projection.source().read_scope();
    if requested.equipment_weapons() && !actual.equipment_weapons()
        || requested.equipment_skill_effects() && !actual.equipment_skill_effects()
    {
        return Err(build_error(
            "模板请求装备详情，但本次状态未读取；请按当前模板重新读取",
        ));
    }
    for required in ["dictionaries", "schema"] {
        let sheet = layout
            .sheets()
            .iter()
            .find(|sheet| sheet.stable_key() == required)
            .ok_or_else(|| build_error(format!("布局缺少 {required} 工作表")))?;
        if sheet.generation() == LayoutGenerationMode::Omitted {
            return Err(build_error(format!("布局不能省略 {required} 工作表")));
        }
        let projection_sheet = projection
            .sheet(required)
            .ok_or_else(|| build_error(format!("投影缺少 {required} 工作表")))?;
        if required == "schema" && !projection_sheet.rows().is_empty() {
            return Err(build_error(format!(
                "投影工作表 {required} 必须由写入器合成，不能携带数据行"
            )));
        }
    }
    for sheet in layout.sheets() {
        let projection_sheet = projection
            .sheet(sheet.stable_key())
            .ok_or_else(|| build_error(format!("投影缺少布局工作表 {}", sheet.stable_key())))?;
        let fields = layout.generated_fields_for_sheet(sheet.stable_key());
        if sheet.generation() != LayoutGenerationMode::Omitted && fields.is_empty() {
            return Err(build_error(format!(
                "生成工作表 {} 没有可写字段",
                sheet.stable_key()
            )));
        }
        for field in layout
            .fields()
            .iter()
            .filter(|field| field.sheet_key() == sheet.stable_key())
        {
            if projection_sheet
                .field_keys()
                .binary_search_by(|key| key.as_str().cmp(field.stable_key()))
                .is_err()
            {
                return Err(build_error(format!(
                    "投影工作表 {} 缺少布局字段 {}",
                    sheet.stable_key(),
                    field.stable_key()
                )));
            }
        }
    }
    for projection_sheet in projection.sheets() {
        if !layout
            .sheets()
            .iter()
            .any(|sheet| sheet.stable_key() == projection_sheet.stable_key())
        {
            return Err(build_error(format!(
                "投影包含布局未登记的工作表 {}",
                projection_sheet.stable_key()
            )));
        }
    }
    Ok(())
}

/// 在创建 XLSX 前检查全部最终可见字符串的 Excel UTF-16 长度上限。
fn validate_final_strings(
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> Result<(), WorkbookProbeError> {
    validate_cell_text("layout.template_name", layout.template_name())?;
    validate_cell_text("layout.purpose", layout.purpose())?;
    for sheet in layout.sheets() {
        validate_cell_text(
            &format!("sheet.{}.stable_key", sheet.stable_key()),
            sheet.stable_key(),
        )?;
        validate_cell_text(
            &format!("sheet.{}.display_name", sheet.stable_key()),
            sheet.display_name(),
        )?;
        validate_cell_text(
            &format!("sheet.{}.description", sheet.stable_key()),
            sheet.description(),
        )?;
    }
    for field in layout.fields() {
        let label = format!("field.{}.{}", field.sheet_key(), field.stable_key());
        validate_cell_text(&format!("{label}.stable_key"), field.stable_key())?;
        validate_cell_text(&format!("{label}.display_name"), field.display_name())?;
        validate_cell_text(&format!("{label}.description"), field.description())?;
        validate_cell_text(&format!("{label}.model_path"), field.model_path())?;
    }
    for option in layout.enum_options() {
        let label = format!("enum.{}.{}", option.category_key(), option.stable_value());
        validate_cell_text(&format!("{label}.category"), option.category_key())?;
        validate_cell_text(&format!("{label}.value"), option.stable_value())?;
        validate_cell_text(&format!("{label}.label"), option.label())?;
        validate_cell_text(&format!("{label}.description"), option.description())?;
    }
    for sheet in projection.sheets() {
        for row in sheet.rows() {
            for value in row.values().values() {
                if let WorkbookProjectionValue::Enumeration {
                    category_key,
                    stable_value,
                } = value
                {
                    validate_cell_text("projection.enum.category", category_key)?;
                    validate_cell_text("projection.enum.value", stable_value)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_cell_text(label: &str, value: &str) -> Result<(), WorkbookProbeError> {
    let actual = value.encode_utf16().count();
    if actual > MAX_EXCEL_CELL_UTF16_UNITS {
        return Err(build_error(format!(
            "{label} 含 {actual} 个 UTF-16 单元，超过 Excel 单元格上限 {MAX_EXCEL_CELL_UTF16_UNITS}"
        )));
    }
    Ok(())
}

/// 以稳定分类和值建立枚举显示标签索引，写入时不再依赖布局行顺序。
pub(in crate::adapters::workbook) type EnumLabels = BTreeMap<String, BTreeMap<String, String>>;

/// 以稳定分类和值建立枚举显示标签索引，写入时按借用键查找。
pub(in crate::adapters::workbook) fn enum_labels(layout: &WorkbookLayout) -> EnumLabels {
    let mut labels = EnumLabels::new();
    for option in layout.enum_options() {
        labels
            .entry(option.category_key().to_owned())
            .or_default()
            .insert(option.stable_value().to_owned(), option.label().to_owned());
    }
    labels
}

/// 按布局字段顺序写入一张投影表，并要求每行具备全部已注册字段。
#[allow(clippy::too_many_arguments)]
fn write_projection_rows(
    worksheet: &mut Worksheet,
    sheet: &WorkbookSheetLayout,
    fields: &[&WorkbookFieldLayout],
    projection_sheet: &WorkbookProjectionSheet,
    composable: &BTreeSet<String>,
    formats: &WorkbookFormats,
    enum_labels: &EnumLabels,
    row_offset: usize,
) -> Result<usize, WorkbookProbeError> {
    for (row_index, projection_row) in projection_sheet.rows().iter().enumerate() {
        let row = u32::try_from(row_index + row_offset + 1).map_err(integer_overflow)?;
        let unowned = is_unowned_row(sheet, projection_row);
        let directly_composable = sheet.stable_key() == "equipment_inventory"
            && projection_row.is_directly_composable_equipment(composable);
        let row_style = unowned.then_some(
            if directly_composable || projection_row.has_family_owned_enhance_distribution() {
                "read_only"
            } else {
                "unowned"
            },
        );
        for (column_index, field) in fields.iter().enumerate() {
            let column = u16::try_from(column_index).map_err(integer_overflow)?;
            let mut format = formats.for_field(field, row_style)?;
            if unowned {
                format = format.set_locked();
            }
            let value = projection_row.value(field.stable_key()).ok_or_else(|| {
                build_error(format!(
                    "工作表 {} 的对象 {} 缺少字段 {}",
                    sheet.stable_key(),
                    projection_row.object_ref(),
                    field.stable_key()
                ))
            })?;
            if matches!(
                crate::application::technology_template_key(sheet.stable_key()),
                "loadout_plan" | "ship_technology"
            ) && crate::application::TECHNOLOGY_FIELDS.contains(&field.stable_key())
                && matches!(value, WorkbookProjectionValue::Text(_))
            {
                let status = match value {
                    WorkbookProjectionValue::Text(text) => text.lines().next(),
                    _ => None,
                };
                // 复用模板绿色和红色配色，科技字段始终保持只读。
                let style = match status {
                    Some("已达成") => "input",
                    Some("未达成") => "error",
                    _ => "read_only",
                };
                format = formats.for_field(field, Some(style))?.set_locked();
            }
            write_projection_value(
                worksheet,
                row,
                column,
                sheet,
                field,
                projection_row,
                value,
                &format,
                enum_labels,
            )?;
        }
    }
    Ok(projection_sheet.rows().len())
}

fn is_unowned_row(sheet: &WorkbookSheetLayout, row: &WorkbookProjectionRow) -> bool {
    let unowned_source = matches!(
        row.value("source_type"),
        Some(WorkbookProjectionValue::Text(value)) if value == "unowned"
    );
    match crate::application::technology_template_key(sheet.stable_key()) {
        "loadout_plan" | "ship_technology" => unowned_source,
        "equipment_inventory" => {
            unowned_source
                && matches!(
                    row.value("quantity"),
                    Some(WorkbookProjectionValue::Integer(0))
                )
        }
        _ => false,
    }
}

/// 将强类型投影值写为字面单元格；文本走字符串或图鉴超链接，不写入公式。
#[allow(clippy::too_many_arguments)]
fn write_projection_value(
    worksheet: &mut Worksheet,
    row: u32,
    column: u16,
    sheet: &WorkbookSheetLayout,
    field: &WorkbookFieldLayout,
    projection_row: &WorkbookProjectionRow,
    value: &WorkbookProjectionValue,
    format: &rust_xlsxwriter::Format,
    enum_labels: &EnumLabels,
) -> Result<(), WorkbookProbeError> {
    if let Some(text) =
        super::equipment_display::projected_text(field, projection_row).map_err(build_error)?
    {
        validate_cell_text(
            &format!(
                "{}.{}.{}",
                sheet.stable_key(),
                projection_row.object_ref(),
                field.stable_key()
            ),
            &text,
        )?;
        worksheet.write_string_with_format(row, column, &text, format)?;
        return Ok(());
    }
    match value {
        WorkbookProjectionValue::Blank => {
            worksheet.write_blank(row, column, format)?;
        }
        WorkbookProjectionValue::Text(value) => {
            write_text_cell(
                worksheet,
                row,
                column,
                sheet,
                field,
                projection_row,
                value,
                format,
            )?;
        }
        WorkbookProjectionValue::Json(value) => {
            worksheet.write_string_with_format(row, column, value, format)?;
        }
        WorkbookProjectionValue::Integer(value) => {
            worksheet.write_number_with_format(row, column, *value as f64, format)?;
        }
        WorkbookProjectionValue::Decimal(value) => {
            worksheet.write_number_with_format(row, column, *value, format)?;
        }
        WorkbookProjectionValue::DateTimeUnixMillis(value) => {
            let datetime = excel_datetime_from_unix_millis(*value)?;
            worksheet.write_datetime_with_format(row, column, &datetime, format)?;
        }
        WorkbookProjectionValue::Boolean(value) => {
            worksheet.write_string_with_format(
                row,
                column,
                if *value { "是" } else { "否" },
                format,
            )?;
        }
        WorkbookProjectionValue::Enumeration {
            category_key,
            stable_value,
        } => {
            let label = enum_labels
                .get(category_key.as_str())
                .and_then(|values| values.get(stable_value.as_str()))
                .ok_or_else(|| {
                    build_error(format!(
                        "工作表 {} 的对象 {} 字段 {} 找不到枚举标签 {}.{}",
                        sheet.stable_key(),
                        projection_row.object_ref(),
                        field.stable_key(),
                        category_key,
                        stable_value
                    ))
                })?;
            worksheet.write_string_with_format(row, column, label, format)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_text_cell(
    worksheet: &mut Worksheet,
    row: u32,
    column: u16,
    sheet: &WorkbookSheetLayout,
    field: &WorkbookFieldLayout,
    projection_row: &WorkbookProjectionRow,
    text: &str,
    format: &rust_xlsxwriter::Format,
) -> Result<(), WorkbookProbeError> {
    if is_ship_name_field(sheet, field) {
        let Some(WorkbookProjectionValue::Text(original_name)) =
            projection_row.value("original_name")
        else {
            return Err(build_error(format!(
                "舰船 {} 缺少原名",
                projection_row.object_ref()
            )));
        };
        if let Some(url) = ship_wiki_url(original_name) {
            worksheet.write_url_with_format(
                row,
                column,
                Url::new(url).set_text(text),
                &format
                    .clone()
                    .set_font_color(Color::RGB(SHIP_WIKI_LINK_COLOR))
                    .set_underline(FormatUnderline::Single),
            )?;
            return Ok(());
        }
    }
    worksheet.write_string_with_format(row, column, text, format)?;
    Ok(())
}

fn is_ship_name_field(sheet: &WorkbookSheetLayout, field: &WorkbookFieldLayout) -> bool {
    field.stable_key() == "name"
        && matches!(
            crate::application::technology_template_key(sheet.stable_key()),
            "loadout_plan" | "ship_technology"
        )
}

/// 为零数据表写出带格式的完整空白行，使表格和后续人工编辑范围保持有效。
fn write_empty_table_row(
    worksheet: &mut Worksheet,
    fields: &[&WorkbookFieldLayout],
    formats: &WorkbookFormats,
) -> Result<(), WorkbookProbeError> {
    for (column_index, field) in fields.iter().enumerate() {
        let column = u16::try_from(column_index).map_err(integer_overflow)?;
        let format = formats.for_field(field, None)?;
        worksheet.write_blank(1, column, &format)?;
    }
    Ok(())
}

/// 将 Unix 毫秒精确转换为 Excel 序列日期，并保留毫秒余数。
pub(in crate::adapters::workbook) fn excel_datetime_from_unix_millis(
    value: i64,
) -> Result<ExcelDateTime, WorkbookProbeError> {
    let seconds = value.div_euclid(1_000);
    let remainder_millis = value.rem_euclid(1_000);
    let seconds_datetime = ExcelDateTime::from_timestamp(seconds)?;
    let serial = seconds_datetime.to_excel() + remainder_millis as f64 / MILLIS_PER_DAY;
    ExcelDateTime::from_serial_datetime(serial).map_err(WorkbookProbeError::from)
}

fn invalid(message: impl Into<String>) -> WorkbookProbeError {
    invalid_generated_workbook(ARTIFACT_NAME, message)
}

#[cfg(test)]
mod tests;

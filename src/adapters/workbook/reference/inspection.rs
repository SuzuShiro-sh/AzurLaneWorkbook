//! 代表性工作簿的语义与 OOXML 特性检查。

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde::Serialize;

use super::super::WorkbookProbeError;
use super::super::package::{
    PackageRelationship, PackageSnapshot, optional_attribute, parse_relationships,
    required_attribute, resolve_relationship_target,
};
use super::fixture::{OPAQUE_PART, OPAQUE_RELATIONSHIP_ID, OPAQUE_RELATIONSHIP_TYPE};

pub(crate) use super::super::sheet_parts::worksheet_relationships_name;
#[cfg(test)]
pub(crate) use super::super::sheet_parts::{rename_workbook_sheets, worksheet_table_part_name};
#[cfg(test)]
pub(crate) use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;
pub(crate) use suzushiro_xlsx_toolkit::workbook::{
    SheetReference, WORKBOOK_PART, WORKBOOK_RELATIONSHIPS_PART, parse_sheet_references,
};

/// 代表性工作簿通过全部特性断言后的结构化证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookFeatureEvidence {
    pub sheet_names: Vec<String>,
    pub text_id: String,
    pub formula: String,
    pub has_comment: bool,
    pub has_table: bool,
    pub has_hidden_sheet: bool,
    pub has_hidden_column: bool,
    pub has_style: bool,
    pub has_frozen_panes: bool,
    pub has_auto_filter: bool,
    pub has_formula: bool,
    pub has_data_validation: bool,
    pub has_protection: bool,
    pub has_opaque_part: bool,
    pub has_opaque_relationship: bool,
    pub zip_entry_count: usize,
}

/// 重新打开样本并同时核对单元格语义和 OOXML 结构特性。
pub fn inspect_representative_workbook(
    path: &Path,
) -> Result<WorkbookFeatureEvidence, WorkbookProbeError> {
    let package: PackageSnapshot = PackageSnapshot::read(path)?;
    let sheet_references: Vec<SheetReference> =
        parse_sheet_references(package.part(WORKBOOK_PART)?)?;
    let data_sheet: &SheetReference = sheet_references
        .iter()
        .find(|sheet: &&SheetReference| sheet.name == "数据")
        .ok_or(WorkbookProbeError::FeatureMissing {
            feature: "数据工作表",
        })?;
    let hidden_sheet: &SheetReference = sheet_references
        .iter()
        .find(|sheet: &&SheetReference| sheet.name == "隐藏配置")
        .ok_or(WorkbookProbeError::FeatureMissing {
            feature: "隐藏工作表",
        })?;
    require_feature(hidden_sheet.hidden, "隐藏工作表状态")?;

    let workbook_relationships: Vec<PackageRelationship> = parse_relationships(
        WORKBOOK_RELATIONSHIPS_PART,
        package.part(WORKBOOK_RELATIONSHIPS_PART)?,
    )?;
    let data_relationship: &PackageRelationship = workbook_relationships
        .iter()
        .find(|relationship: &&PackageRelationship| relationship.id == data_sheet.relationship_id)
        .ok_or(WorkbookProbeError::FeatureMissing {
            feature: "数据工作表关系",
        })?;
    let data_part_name: String =
        resolve_relationship_target(WORKBOOK_PART, &data_relationship.target)?;
    let worksheet_features: WorksheetFeatures =
        inspect_worksheet_part(&data_part_name, package.part(&data_part_name)?)?;
    require_feature(worksheet_features.hidden_column, "隐藏列")?;
    require_feature(worksheet_features.styled_header, "单元格样式")?;
    require_feature(worksheet_features.frozen_panes, "冻结窗格")?;
    require_feature(worksheet_features.auto_filter, "自动筛选")?;
    require_feature(worksheet_features.formula, "公式")?;
    require_feature(worksheet_features.data_validation, "数据验证")?;
    require_feature(worksheet_features.protection, "工作表保护")?;
    require_feature(worksheet_features.table_relationship, "工作表表格关系")?;

    let sheet_relationships_name: String = worksheet_relationships_name(&data_part_name)?;
    let sheet_relationships: Vec<PackageRelationship> = parse_relationships(
        &sheet_relationships_name,
        package.part(&sheet_relationships_name)?,
    )?;
    let comments_relationship: &PackageRelationship = sheet_relationships
        .iter()
        .find(|relationship: &&PackageRelationship| {
            relationship.relationship_type.ends_with("/comments")
        })
        .ok_or(WorkbookProbeError::FeatureMissing {
            feature: "批注关系",
        })?;
    let table_relationship: &PackageRelationship = sheet_relationships
        .iter()
        .find(|relationship: &&PackageRelationship| {
            relationship.relationship_type.ends_with("/table")
        })
        .ok_or(WorkbookProbeError::FeatureMissing {
            feature: "表格关系",
        })?;
    let comments_part_name: String =
        resolve_relationship_target(&data_part_name, &comments_relationship.target)?;
    let table_part_name: String =
        resolve_relationship_target(&data_part_name, &table_relationship.target)?;
    let has_comment: bool =
        inspect_comment_part(&comments_part_name, package.part(&comments_part_name)?)?;
    let has_table: bool = inspect_table_part(&table_part_name, package.part(&table_part_name)?)?;
    require_feature(has_comment, "A2 单元格批注")?;
    require_feature(has_table, "装备清单表格")?;

    let has_opaque_part: bool = package.entry_names().any(|name: &str| name == OPAQUE_PART);
    require_feature(has_opaque_part, "未知 OOXML 部件")?;
    let root_relationships: Vec<PackageRelationship> =
        parse_relationships("_rels/.rels", package.part("_rels/.rels")?)?;
    let has_opaque_relationship: bool =
        root_relationships
            .iter()
            .any(|relationship: &PackageRelationship| {
                relationship.id == OPAQUE_RELATIONSHIP_ID
                    && relationship.relationship_type == OPAQUE_RELATIONSHIP_TYPE
                    && relationship.target == OPAQUE_PART
                    && !relationship.external
            });
    require_feature(has_opaque_relationship, "未知 OOXML 关系")?;

    let semantic: SemanticEvidence = inspect_semantics(path)?;
    let sheet_names: Vec<String> = sheet_references
        .iter()
        .map(|sheet: &SheetReference| sheet.name.clone())
        .collect();
    Ok(WorkbookFeatureEvidence {
        sheet_names,
        text_id: semantic.text_id,
        formula: semantic.formula,
        has_comment,
        has_table,
        has_hidden_sheet: hidden_sheet.hidden,
        has_hidden_column: worksheet_features.hidden_column,
        has_style: worksheet_features.styled_header,
        has_frozen_panes: worksheet_features.frozen_panes,
        has_auto_filter: worksheet_features.auto_filter,
        has_formula: worksheet_features.formula,
        has_data_validation: worksheet_features.data_validation,
        has_protection: worksheet_features.protection,
        has_opaque_part,
        has_opaque_relationship,
        zip_entry_count: package.entry_count(),
    })
}

/// 目标工作表 XML 中的固定特性集合。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WorksheetFeatures {
    hidden_column: bool,
    styled_header: bool,
    frozen_panes: bool,
    auto_filter: bool,
    formula: bool,
    data_validation: bool,
    protection: bool,
    table_relationship: bool,
}

/// 独立读取器确认的关键单元格语义。
#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticEvidence {
    text_id: String,
    formula: String,
}

/// 检查数据工作表的全部结构特性。
fn inspect_worksheet_part(
    part_name: &str,
    bytes: &[u8],
) -> Result<WorksheetFeatures, WorkbookProbeError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut features: WorksheetFeatures = WorksheetFeatures::default();
    loop {
        let event: Event<'_> = reader.read_event().map_err(|source: quick_xml::Error| {
            WorkbookProbeError::InvalidOoxml {
                part: part_name.to_owned(),
                message: source.to_string(),
            }
        })?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                inspect_worksheet_element(&reader, part_name, element, &mut features)?;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(features)
}

/// 将单个工作表元素归入对应特性证据。
fn inspect_worksheet_element(
    reader: &Reader<&[u8]>,
    part_name: &str,
    element: &BytesStart<'_>,
    features: &mut WorksheetFeatures,
) -> Result<(), WorkbookProbeError> {
    match element.local_name().as_ref() {
        b"col" => {
            features.hidden_column |=
                optional_attribute(reader, part_name, element, b"hidden")?.as_deref() == Some("1");
        }
        b"c" => {
            let reference: Option<String> = optional_attribute(reader, part_name, element, b"r")?;
            let style: Option<String> = optional_attribute(reader, part_name, element, b"s")?;
            features.styled_header |= reference.as_deref() == Some("E1")
                && style.as_deref().is_some_and(|value: &str| value != "0");
        }
        b"pane" => {
            features.frozen_panes |= optional_attribute(reader, part_name, element, b"state")?
                .as_deref()
                == Some("frozen");
        }
        b"autoFilter" => features.auto_filter = true,
        b"f" => features.formula = true,
        b"dataValidation" => features.data_validation = true,
        b"sheetProtection" => features.protection = true,
        b"tablePart" => features.table_relationship = true,
        _ => {}
    }
    Ok(())
}

/// 检查批注部件仍包含 A2 的固定说明。
fn inspect_comment_part(part_name: &str, bytes: &[u8]) -> Result<bool, WorkbookProbeError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut in_target_comment: bool = false;
    let mut has_target_comment: bool = false;
    let mut text: String = String::new();
    loop {
        let event: Event<'_> = reader.read_event().map_err(|source: quick_xml::Error| {
            WorkbookProbeError::InvalidOoxml {
                part: part_name.to_owned(),
                message: source.to_string(),
            }
        })?;
        match event {
            Event::Start(ref element) if element.local_name().as_ref() == b"comment" => {
                in_target_comment =
                    required_attribute(&reader, part_name, element, b"ref")? == "A2";
                has_target_comment |= in_target_comment;
            }
            Event::End(ref element) if element.local_name().as_ref() == b"comment" => {
                in_target_comment = false;
            }
            Event::Text(value) if in_target_comment => {
                let decoded: String = value
                    .decode()
                    .map_err(|source: quick_xml::encoding::EncodingError| {
                        WorkbookProbeError::InvalidOoxml {
                            part: part_name.to_owned(),
                            message: source.to_string(),
                        }
                    })?
                    .into_owned();
                text.push_str(&decoded);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(has_target_comment && text.contains("文本编号必须保留前导零"))
}

/// 检查表格部件的稳定名称和范围。
fn inspect_table_part(part_name: &str, bytes: &[u8]) -> Result<bool, WorkbookProbeError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    loop {
        let event: Event<'_> = reader.read_event().map_err(|source: quick_xml::Error| {
            WorkbookProbeError::InvalidOoxml {
                part: part_name.to_owned(),
                message: source.to_string(),
            }
        })?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"table" =>
            {
                let display_name: String =
                    required_attribute(&reader, part_name, element, b"displayName")?;
                let table_range: String = required_attribute(&reader, part_name, element, b"ref")?;
                return Ok(display_name == "装备清单" && table_range == "A1:C3");
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(false)
}

/// 用只读库重新读取文本编号和公式，避免只验证写入器自己的结构。
fn inspect_semantics(path: &Path) -> Result<SemanticEvidence, WorkbookProbeError> {
    let mut workbook: Xlsx<BufReader<File>> = open_workbook(path)
        .map_err(|source: calamine::XlsxError| WorkbookProbeError::XlsxRead { source })?;
    let range: calamine::Range<Data> = workbook
        .worksheet_range("数据")
        .map_err(|source: calamine::XlsxError| WorkbookProbeError::XlsxRead { source })?;
    let text_id: String = match range.get_value((1, 0)) {
        Some(Data::String(value)) => value.clone(),
        Some(other) => {
            return Err(WorkbookProbeError::InvalidOoxml {
                part: "xl/worksheets/sheet1.xml".to_owned(),
                message: format!("A2 必须是文本，实际为 {other:?}"),
            });
        }
        None => {
            return Err(WorkbookProbeError::FeatureMissing {
                feature: "A2 文本编号",
            });
        }
    };
    let formulas: calamine::Range<String> = workbook
        .worksheet_formula("数据")
        .map_err(|source: calamine::XlsxError| WorkbookProbeError::XlsxRead { source })?;
    let formula: String =
        formulas
            .get_value((1, 2))
            .cloned()
            .ok_or(WorkbookProbeError::FeatureMissing {
                feature: "C2 公式"
            })?;
    if formula.trim_start_matches('=') != "B2*2" {
        return Err(WorkbookProbeError::InvalidOoxml {
            part: "xl/worksheets/sheet1.xml".to_owned(),
            message: format!("C2 公式错误: {formula}"),
        });
    }
    Ok(SemanticEvidence { text_id, formula })
}

/// 把布尔断言转换为稳定特性错误。
fn require_feature(value: bool, feature: &'static str) -> Result<(), WorkbookProbeError> {
    if value {
        Ok(())
    } else {
        Err(WorkbookProbeError::FeatureMissing { feature })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_sheet_references;

    #[test]
    fn rejects_case_insensitive_duplicate_sheet_names() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Data" sheetId="1" r:id="rId1"/>
    <sheet name="data" sheetId="2" r:id="rId2"/>
  </sheets>
</workbook>"#;

        let error = parse_sheet_references(xml).unwrap_err();

        assert!(error.to_string().contains("工作表显示名称 \"data\" 重复"));
    }

    #[test]
    fn rejects_duplicate_sheet_relationship_identifiers() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="First" sheetId="1" r:id="rId1"/>
    <sheet name="Second" sheetId="2" r:id="rId1"/>
  </sheets>
</workbook>"#;

        let error = parse_sheet_references(xml).unwrap_err();

        assert!(error.to_string().contains("工作表关系编号 rId1 重复"));
    }
}

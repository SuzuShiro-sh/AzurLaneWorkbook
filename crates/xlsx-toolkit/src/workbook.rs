//! 工作表身份解析与包内关系定位。
use crate::XlsxError;
use crate::package::{
    PackageRelationship, PackageSnapshot, optional_attribute, parse_relationships,
    required_attribute, resolve_relationship_target,
};
use quick_xml::{Reader, events::Event};
use std::collections::BTreeSet;
pub const WORKBOOK_PART: &str = "xl/workbook.xml";
pub const WORKBOOK_RELATIONSHIPS_PART: &str = "xl/_rels/workbook.xml.rels";

/// 工作簿 XML 中的工作表身份和可见性。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetReference {
    pub name: String,
    pub relationship_id: String,
    pub hidden: bool,
}

/// 解析工作簿内工作表顺序、关系编号和隐藏状态。
pub fn parse_sheet_references(bytes: &[u8]) -> Result<Vec<SheetReference>, XlsxError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut sheets: Vec<SheetReference> = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut relationship_ids: BTreeSet<String> = BTreeSet::new();
    loop {
        let event: Event<'_> =
            reader
                .read_event()
                .map_err(|source: quick_xml::Error| XlsxError::InvalidOoxml {
                    part: WORKBOOK_PART.to_owned(),
                    message: source.to_string(),
                })?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"sheet" =>
            {
                let name: String = required_attribute(&reader, WORKBOOK_PART, element, b"name")?;
                let relationship_id: String =
                    required_attribute(&reader, WORKBOOK_PART, element, b"id")?;
                if !names.insert(name.to_lowercase()) {
                    return Err(XlsxError::InvalidOoxml {
                        part: WORKBOOK_PART.to_owned(),
                        message: format!("工作表显示名称 {name:?} 重复"),
                    });
                }
                if !relationship_ids.insert(relationship_id.clone()) {
                    return Err(XlsxError::InvalidOoxml {
                        part: WORKBOOK_PART.to_owned(),
                        message: format!("工作表关系编号 {relationship_id} 重复"),
                    });
                }
                let state: Option<String> =
                    optional_attribute(&reader, WORKBOOK_PART, element, b"state")?;
                let hidden: bool = matches!(state.as_deref(), Some("hidden" | "veryHidden"));
                sheets.push(SheetReference {
                    name,
                    relationship_id,
                    hidden,
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if sheets.is_empty() {
        return Err(XlsxError::FeatureMissing {
            feature: "工作表列表",
        });
    }
    Ok(sheets)
}

/// 根据工作表显示名称解析实际 OOXML 部件名。
pub fn worksheet_part_name(
    package: &PackageSnapshot,
    sheet_name: &str,
) -> Result<String, XlsxError> {
    let sheet_references: Vec<SheetReference> =
        parse_sheet_references(package.part(WORKBOOK_PART)?)?;
    let sheet: &SheetReference = sheet_references
        .iter()
        .find(|candidate: &&SheetReference| candidate.name == sheet_name)
        .ok_or_else(|| XlsxError::InvalidOoxml {
            part: WORKBOOK_PART.to_owned(),
            message: format!("找不到工作表 {sheet_name}"),
        })?;
    let relationships: Vec<PackageRelationship> = parse_relationships(
        WORKBOOK_RELATIONSHIPS_PART,
        package.part(WORKBOOK_RELATIONSHIPS_PART)?,
    )?;
    let relationship: &PackageRelationship = relationships
        .iter()
        .find(|candidate: &&PackageRelationship| candidate.id == sheet.relationship_id)
        .ok_or_else(|| XlsxError::InvalidOoxml {
            part: WORKBOOK_RELATIONSHIPS_PART.to_owned(),
            message: format!("找不到工作表 {sheet_name} 的关系 {}", sheet.relationship_id),
        })?;
    if relationship.external {
        return Err(XlsxError::ExternalRelationship {
            part: WORKBOOK_RELATIONSHIPS_PART.to_owned(),
            relationship_id: relationship.id.clone(),
            target: relationship.target.clone(),
        });
    }
    resolve_relationship_target(WORKBOOK_PART, &relationship.target)
}

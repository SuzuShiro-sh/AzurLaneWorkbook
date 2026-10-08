//! 定位工作表关系、表格部件，并只改已绑定工作表的显示名称。

use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::WorkbookProbeError;
use super::package::{
    PackageSnapshot, parse_relationships, required_attribute, resolve_relationship_target,
};
use suzushiro_xlsx_toolkit::workbook::{
    WORKBOOK_PART, parse_sheet_references, worksheet_part_name,
};

const TABLE_RELATIONSHIP_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";
const TABLE_RELATIONSHIP_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/table";

/// 沿工作表登记和关系部件解析唯一的包内 Excel 表格部件。
pub(crate) fn worksheet_table_part_name(
    package: &PackageSnapshot,
    worksheet_part: &str,
) -> Result<String, WorkbookProbeError> {
    let relationship_id =
        single_table_relationship_id(worksheet_part, package.part(worksheet_part)?)?;
    let relationships_part = worksheet_relationships_name(worksheet_part)?;
    let relationships =
        parse_relationships(&relationships_part, package.part(&relationships_part)?)?;
    let relationship = relationships
        .iter()
        .find(|candidate| candidate.id == relationship_id)
        .ok_or_else(|| WorkbookProbeError::InvalidOoxml {
            part: relationships_part.clone(),
            message: format!("找不到工作表登记的表格关系 {relationship_id}"),
        })?;
    if relationship.external {
        return Err(WorkbookProbeError::ExternalRelationship {
            part: relationships_part,
            relationship_id: relationship.id.clone(),
            target: relationship.target.clone(),
        });
    }
    if !matches!(
        relationship.relationship_type.as_str(),
        TABLE_RELATIONSHIP_TRANSITIONAL | TABLE_RELATIONSHIP_STRICT
    ) {
        return Err(WorkbookProbeError::InvalidOoxml {
            part: relationships_part,
            message: format!(
                "关系 {relationship_id} 不是受支持的 Excel 表格关系: {}",
                relationship.relationship_type
            ),
        });
    }
    let table_relationship_count = relationships
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.relationship_type.as_str(),
                TABLE_RELATIONSHIP_TRANSITIONAL | TABLE_RELATIONSHIP_STRICT
            )
        })
        .count();
    if table_relationship_count != 1 {
        return Err(WorkbookProbeError::InvalidOoxml {
            part: worksheet_part.to_owned(),
            message: format!(
                "工作表必须且只能关联一个 Excel 表格，实际为 {table_relationship_count}"
            ),
        });
    }
    let table_part = resolve_relationship_target(worksheet_part, &relationship.target)?;
    package.part(&table_part)?;
    Ok(table_part)
}

fn single_table_relationship_id(
    part_name: &str,
    bytes: &[u8],
) -> Result<String, WorkbookProbeError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut declared_count: Option<usize> = None;
    let mut identifiers: Vec<String> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|source| WorkbookProbeError::InvalidOoxml {
                part: part_name.to_owned(),
                message: source.to_string(),
            })?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tableParts" =>
            {
                if declared_count.is_some() {
                    return Err(WorkbookProbeError::InvalidOoxml {
                        part: part_name.to_owned(),
                        message: "tableParts 元素重复".to_owned(),
                    });
                }
                let count = required_attribute(&reader, part_name, element, b"count")?;
                declared_count =
                    Some(
                        count
                            .parse()
                            .map_err(|_| WorkbookProbeError::InvalidOoxml {
                                part: part_name.to_owned(),
                                message: "tableParts.count 不是非负整数".to_owned(),
                            })?,
                    );
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tablePart" =>
            {
                let identifier = required_attribute(&reader, part_name, element, b"id")?;
                if identifiers.contains(&identifier) {
                    return Err(WorkbookProbeError::InvalidOoxml {
                        part: part_name.to_owned(),
                        message: format!("Excel 表格关系编号 {identifier} 重复"),
                    });
                }
                identifiers.push(identifier);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if declared_count != Some(identifiers.len()) || identifiers.len() != 1 {
        return Err(WorkbookProbeError::InvalidOoxml {
            part: part_name.to_owned(),
            message: format!(
                "工作表必须登记一个表格关系: declared={declared_count:?}, actual={}",
                identifiers.len()
            ),
        });
    }
    Ok(identifiers.remove(0))
}

/// 读取工作表里已经登记的单元格超链接。关系目标保持原样，调用方再核对图鉴地址。
pub(crate) fn worksheet_hyperlinks(
    package: &PackageSnapshot,
    sheet_name: &str,
) -> Result<BTreeMap<String, String>, WorkbookProbeError> {
    let part = worksheet_part_name(package, sheet_name)?;
    let relationships_name = worksheet_relationships_name(&part)?;
    let relationships = match package.part(&relationships_name) {
        Ok(bytes) => parse_relationships(&relationships_name, bytes)?,
        Err(suzushiro_xlsx_toolkit::XlsxError::MissingPart { .. }) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let targets: BTreeMap<String, String> = relationships
        .into_iter()
        .map(|relationship| (relationship.id, relationship.target))
        .collect();
    let bytes = package.part(&part)?;
    let mut reader = Reader::from_reader(bytes);
    let mut links = BTreeMap::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|source| WorkbookProbeError::InvalidOoxml {
                part: part.clone(),
                message: source.to_string(),
            })?;
        match event {
            Event::Empty(element) | Event::Start(element)
                if element.local_name().as_ref() == b"hyperlink" =>
            {
                let cell = super::package::required_attribute(&reader, &part, &element, b"ref")?;
                let relationship_id =
                    super::package::required_attribute(&reader, &part, &element, b"id")?;
                let target = targets.get(&relationship_id).ok_or_else(|| {
                    WorkbookProbeError::InvalidOoxml {
                        part: relationships_name.clone(),
                        message: format!("超链接 {cell} 找不到关系 {relationship_id}"),
                    }
                })?;
                links.insert(cell, target.clone());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(links)
}

/// 根据工作表部件名确定同目录关系部件名。
pub(crate) fn worksheet_relationships_name(
    worksheet_part: &str,
) -> Result<String, WorkbookProbeError> {
    let (directory, file_name): (&str, &str) =
        worksheet_part
            .rsplit_once('/')
            .ok_or_else(|| WorkbookProbeError::InvalidOoxml {
                part: worksheet_part.to_owned(),
                message: "工作表部件缺少目录".to_owned(),
            })?;
    Ok(format!("{directory}/_rels/{file_name}.rels"))
}

/// 只更新已绑定工作表的显示名称，保留关系编号、顺序及其他工作簿节点。
pub(crate) fn rename_workbook_sheets(
    bytes: &[u8],
    names: &BTreeMap<String, String>,
) -> Result<Vec<u8>, WorkbookProbeError> {
    parse_sheet_references(bytes)?;
    let mut reader = Reader::from_reader(bytes);
    let mut writer = quick_xml::Writer::new(Vec::new());
    loop {
        let event = reader
            .read_event()
            .map_err(|source| WorkbookProbeError::InvalidOoxml {
                part: WORKBOOK_PART.to_owned(),
                message: source.to_string(),
            })?;
        if matches!(event, Event::Eof) {
            break;
        }
        let empty = matches!(&event, Event::Empty(_));
        let event = match event {
            Event::Start(element) | Event::Empty(element)
                if element.local_name().as_ref() == b"sheet" =>
            {
                let old = required_attribute(&reader, WORKBOOK_PART, &element, b"name")?;
                let mut updated = element.to_owned();
                if let Some(name) = names.get(&old) {
                    updated.clear_attributes();
                    for attribute in element.attributes() {
                        let attribute =
                            attribute.map_err(|source| WorkbookProbeError::InvalidOoxml {
                                part: WORKBOOK_PART.to_owned(),
                                message: source.to_string(),
                            })?;
                        if attribute.key.as_ref() != b"name" {
                            updated
                                .push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
                        }
                    }
                    updated.push_attribute(("name", name.as_str()));
                }
                if empty {
                    Event::Empty(updated)
                } else {
                    Event::Start(updated)
                }
            }
            event => event.into_owned(),
        };
        writer
            .write_event(event)
            .map_err(|source| WorkbookProbeError::InvalidOoxml {
                part: WORKBOOK_PART.to_owned(),
                message: source.to_string(),
            })?;
    }
    let output = writer.into_inner();
    parse_sheet_references(&output)?;
    Ok(output)
}

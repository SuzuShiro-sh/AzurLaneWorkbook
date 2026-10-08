//! 解析 OPC 关系和 OOXML 元素属性，并保留部件级错误上下文。

use std::collections::BTreeSet;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use super::{XlsxError, invalid_part};

/// 一个 OPC 关系及其外部目标标记。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRelationship {
    pub id: String,
    pub relationship_type: String,
    pub target: String,
    pub external: bool,
}

/// 解析 OPC 关系，拒绝缺字段和重复关系编号。
pub fn parse_relationships(
    part_name: &str,
    bytes: &[u8],
) -> Result<Vec<PackageRelationship>, XlsxError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut relationships: Vec<PackageRelationship> = Vec::new();
    let mut identifiers: BTreeSet<String> = BTreeSet::new();
    loop {
        let event: Event<'_> = reader
            .read_event()
            .map_err(|source: quick_xml::Error| invalid_xml(part_name, source.to_string()))?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"Relationship" =>
            {
                let id: String = required_attribute(&reader, part_name, element, b"Id")?;
                let relationship_type: String =
                    required_attribute(&reader, part_name, element, b"Type")?;
                let target: String = required_attribute(&reader, part_name, element, b"Target")?;
                let target_mode: Option<String> =
                    optional_attribute(&reader, part_name, element, b"TargetMode")?;
                if !identifiers.insert(id.clone()) {
                    return Err(invalid_part(part_name, format!("关系编号 {id} 重复")));
                }
                let external: bool = match target_mode.as_deref() {
                    None => false,
                    Some(value) if value.eq_ignore_ascii_case("Internal") => false,
                    Some(value) if value.eq_ignore_ascii_case("External") => true,
                    Some(value) => {
                        return Err(invalid_part(
                            part_name,
                            format!("关系 {id} 的 TargetMode {value:?} 无效"),
                        ));
                    }
                };
                relationships.push(PackageRelationship {
                    id,
                    relationship_type,
                    target,
                    external,
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(relationships)
}

/// 将关系目标解析为包内规范部件路径并拒绝越过包根目录。
pub fn resolve_relationship_target(source_part: &str, target: &str) -> Result<String, XlsxError> {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\\')
        || target.contains(':')
        || target.contains('\0')
    {
        return Err(invalid_part(
            source_part,
            format!("关系目标 {target:?} 非法"),
        ));
    }
    let source_parent: &str = source_part
        .rsplit_once('/')
        .map_or("", |value: (&str, &str)| value.0);
    let combined: String = if source_parent.is_empty() {
        target.to_owned()
    } else {
        format!("{source_parent}/{target}")
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(invalid_part(source_part, "关系目标越过包根目录"));
                }
            }
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        return Err(invalid_part(source_part, "关系目标为空"));
    }
    Ok(segments.join("/"))
}

/// 返回 XML 元素的可选属性值。
pub fn optional_attribute(
    reader: &Reader<&[u8]>,
    part_name: &str,
    element: &BytesStart<'_>,
    expected_name: &[u8],
) -> Result<Option<String>, XlsxError> {
    let mut matched_value = None;
    for attribute_result in element.attributes().with_checks(true) {
        let attribute: quick_xml::events::attributes::Attribute<'_> =
            attribute_result.map_err(|source: quick_xml::events::attributes::AttrError| {
                invalid_xml(part_name, source.to_string())
            })?;
        if attribute.key.as_ref() == expected_name
            || attribute.key.local_name().as_ref() == expected_name
        {
            let value: String = attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|source: quick_xml::Error| invalid_xml(part_name, source.to_string()))?
                .into_owned();
            if matched_value.replace(value).is_some() {
                return Err(invalid_xml(
                    part_name,
                    format!("属性 {} 定义不唯一", String::from_utf8_lossy(expected_name)),
                ));
            }
        }
    }
    Ok(matched_value)
}

/// 返回 XML 元素的必需属性值。
pub fn required_attribute(
    reader: &Reader<&[u8]>,
    part_name: &str,
    element: &BytesStart<'_>,
    expected_name: &[u8],
) -> Result<String, XlsxError> {
    optional_attribute(reader, part_name, element, expected_name)?.ok_or_else(|| {
        invalid_part(
            part_name,
            format!("元素缺少属性 {}", String::from_utf8_lossy(expected_name)),
        )
    })
}

fn invalid_xml(part: &str, message: impl Into<String>) -> XlsxError {
    invalid_part(part, format!("XML 解码失败: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::parse_relationships;

    #[test]
    fn relationships_reject_duplicate_attributes_after_the_requested_value() {
        for mode in ["Internal", "External"] {
            let xml = format!(
                r#"<Relationships><Relationship Id="r1" Type="fixture" Target="target.xml" TargetMode="{mode}" TargetMode="External"/></Relationships>"#
            );
            let error =
                parse_relationships("xl/_rels/workbook.xml.rels", xml.as_bytes()).unwrap_err();
            assert!(matches!(
                error,
                super::XlsxError::InvalidOoxml { part, .. } if part == "xl/_rels/workbook.xml.rels"
            ));
        }
    }

    #[test]
    fn relationships_reject_duplicates_in_other_attributes() {
        let xml = br#"<Relationships><Relationship Id="r1" Type="fixture" Target="target.xml" label="one" label="two"/></Relationships>"#;
        assert!(parse_relationships("xl/_rels/workbook.xml.rels", xml).is_err());
    }
}

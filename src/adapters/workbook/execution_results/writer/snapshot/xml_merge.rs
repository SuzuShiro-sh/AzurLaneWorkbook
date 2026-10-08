//! 将生成表导入原包，保留旧样式索引，并去重合并共享字符串。

use std::collections::BTreeMap;

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};

use super::super::invalid;
use crate::adapters::workbook::WorkbookProbeError;

#[derive(Clone)]
enum Node {
    Element(Element),
    Event(Event<'static>),
}

#[derive(Clone)]
pub(super) struct Element {
    name: String,
    attributes: BTreeMap<String, String>,
    children: Vec<Node>,
}

impl Element {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, WorkbookProbeError> {
        let mut reader = Reader::from_reader(bytes);
        let mut stack: Vec<Element> = Vec::new();
        let mut root = None;
        loop {
            match reader
                .read_event()
                .map_err(|error| invalid("snapshot.xml", error.to_string()))?
            {
                Event::Start(element) => {
                    if root.is_some() || stack.len() >= 128 {
                        return Err(invalid("snapshot.xml", "XML包含多个根元素或嵌套过深"));
                    }
                    stack.push(Self::from_start(&reader, &element)?);
                }
                Event::Empty(element) => {
                    if root.is_some() {
                        return Err(invalid("snapshot.xml", "XML包含多个根元素"));
                    }
                    let element = Self::from_start(&reader, &element)?;
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(Node::Element(element));
                    } else {
                        root = Some(element);
                    }
                }
                Event::End(_) => {
                    let element = stack
                        .pop()
                        .ok_or_else(|| invalid("snapshot.xml", "XML结束标签没有对应起始标签"))?;
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(Node::Element(element));
                    } else {
                        root = Some(element);
                    }
                }
                Event::Eof => {
                    return if stack.is_empty() {
                        root.ok_or_else(|| invalid("snapshot.xml", "XML缺少根元素"))
                    } else {
                        Err(invalid("snapshot.xml", "XML缺少完整根元素"))
                    };
                }
                Event::Text(text)
                    if stack.is_empty() && !text.iter().all(u8::is_ascii_whitespace) =>
                {
                    return Err(invalid("snapshot.xml", "XML根元素之外包含正文"));
                }
                event => {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(Node::Event(event.into_owned()));
                    }
                }
            }
        }
    }

    fn from_start(
        reader: &Reader<&[u8]>,
        element: &BytesStart<'_>,
    ) -> Result<Self, WorkbookProbeError> {
        let name = String::from_utf8(element.name().as_ref().to_vec())
            .map_err(|error| invalid("snapshot.xml", error.to_string()))?;
        let mut attributes = BTreeMap::new();
        for attribute in element.attributes() {
            let attribute =
                attribute.map_err(|error| invalid("snapshot.xml", error.to_string()))?;
            let key = String::from_utf8(attribute.key.as_ref().to_vec())
                .map_err(|error| invalid("snapshot.xml", error.to_string()))?;
            let value = attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|error| invalid("snapshot.xml", error.to_string()))?
                .into_owned();
            attributes.insert(key, value);
        }
        Ok(Self {
            name,
            attributes,
            children: Vec::new(),
        })
    }

    fn local_name(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|element| element.local_name() == name)
    }

    fn child_mut(&mut self, name: &str) -> Option<&mut Element> {
        self.children.iter_mut().find_map(|node| match node {
            Node::Element(element) if element.local_name() == name => Some(element),
            _ => None,
        })
    }

    fn write(&self, writer: &mut Writer<Vec<u8>>) -> Result<(), WorkbookProbeError> {
        let mut start = BytesStart::new(&self.name);
        for (key, value) in &self.attributes {
            start.push_attribute((key.as_str(), value.as_str()));
        }
        if self.children.is_empty() {
            writer
                .write_event(Event::Empty(start))
                .map_err(|error| invalid("snapshot.xml", error.to_string()))?;
        } else {
            writer
                .write_event(Event::Start(start))
                .map_err(|error| invalid("snapshot.xml", error.to_string()))?;
            for node in &self.children {
                match node {
                    Node::Element(element) => element.write(writer)?,
                    Node::Event(event) => writer
                        .write_event(event.clone())
                        .map_err(|error| invalid("snapshot.xml", error.to_string()))?,
                }
            }
            writer
                .write_event(Event::End(BytesEnd::new(&self.name)))
                .map_err(|error| invalid("snapshot.xml", error.to_string()))?;
        }
        Ok(())
    }

    pub(super) fn bytes(&self) -> Result<Vec<u8>, WorkbookProbeError> {
        let mut writer = Writer::new(Vec::new());
        self.write(&mut writer)?;
        Ok(writer.into_inner())
    }
}

/// 旧样式项索引始终不变，新格式按依赖顺序去重追加。
pub(super) fn merge_styles(
    source: &[u8],
    generated: &[u8],
) -> Result<(Vec<u8>, Vec<usize>), WorkbookProbeError> {
    let mut source = Element::parse(source)?;
    let generated = Element::parse(generated)?;
    let number_formats = merge_number_formats(&mut source, &generated)?;
    let fonts = append_styles(&mut source, &generated, "fonts", |element| {
        Ok(element.clone())
    })?;
    let fills = append_styles(&mut source, &generated, "fills", |element| {
        Ok(element.clone())
    })?;
    let borders = append_styles(&mut source, &generated, "borders", |element| {
        Ok(element.clone())
    })?;
    let remap_format = |element: &Element| -> Result<Element, WorkbookProbeError> {
        let mut element = element.clone();
        remap_attribute(&mut element, "fontId", &fonts)?;
        remap_attribute(&mut element, "fillId", &fills)?;
        remap_attribute(&mut element, "borderId", &borders)?;
        if let Some(id) = element.attributes.get("numFmtId")
            && let Some(mapped) = number_formats.get(id)
        {
            element
                .attributes
                .insert("numFmtId".to_owned(), mapped.clone());
        }
        Ok(element)
    };
    let base_formats = append_styles(&mut source, &generated, "cellStyleXfs", remap_format)?;
    let styles = append_styles(&mut source, &generated, "cellXfs", |element| {
        let mut element = remap_format(element)?;
        remap_attribute(&mut element, "xfId", &base_formats)?;
        Ok(element)
    })?;
    Ok((source.bytes()?, styles))
}

fn remap_attribute(
    element: &mut Element,
    key: &str,
    indices: &[usize],
) -> Result<(), WorkbookProbeError> {
    if let Some(id) = element.attributes.get(key) {
        let index: usize = id
            .parse()
            .map_err(|error| invalid("snapshot.styles", format!("{key}索引无效: {error}")))?;
        let mapped = indices
            .get(index)
            .ok_or_else(|| invalid("snapshot.styles", format!("{key}索引 {index} 越界")))?;
        element
            .attributes
            .insert(key.to_owned(), mapped.to_string());
    }
    Ok(())
}

fn append_styles(
    source: &mut Element,
    generated: &Element,
    key: &str,
    transform: impl Fn(&Element) -> Result<Element, WorkbookProbeError>,
) -> Result<Vec<usize>, WorkbookProbeError> {
    let incoming = generated
        .child(key)
        .ok_or_else(|| invalid("snapshot.styles", format!("生成样式缺少 {key}")))?;
    let existing = source
        .child_mut(key)
        .ok_or_else(|| invalid("snapshot.styles", format!("原样式缺少 {key}")))?;
    let mut serialized = existing
        .elements()
        .map(Element::bytes)
        .collect::<Result<Vec<_>, _>>()?;
    let mut mapping = Vec::new();
    for element in incoming.elements() {
        let element = transform(element)?;
        let bytes = element.bytes()?;
        let index = if let Some(index) = serialized.iter().position(|value| value == &bytes) {
            index
        } else {
            let index = serialized.len();
            serialized.push(bytes);
            existing.children.push(Node::Element(element));
            index
        };
        mapping.push(index);
    }
    existing
        .attributes
        .insert("count".to_owned(), serialized.len().to_string());
    Ok(mapping)
}

fn merge_number_formats(
    source: &mut Element,
    generated: &Element,
) -> Result<BTreeMap<String, String>, WorkbookProbeError> {
    let Some(incoming) = generated.child("numFmts") else {
        return Ok(BTreeMap::new());
    };
    if source.child("numFmts").is_none() {
        let mut empty = incoming.clone();
        empty.children.clear();
        source.children.insert(0, Node::Element(empty));
    }
    let existing = source
        .child_mut("numFmts")
        .ok_or_else(|| invalid("snapshot.styles", "缺少numFmts容器"))?;
    let mut by_code = BTreeMap::new();
    let mut next_id = 164_u32;
    for element in existing.elements() {
        let id = element
            .attributes
            .get("numFmtId")
            .ok_or_else(|| invalid("snapshot.styles", "数字格式缺少ID"))?;
        let number: u32 = id
            .parse()
            .map_err(|error| invalid("snapshot.styles", format!("数字格式ID无效: {error}")))?;
        next_id = next_id.max(
            number
                .checked_add(1)
                .ok_or_else(|| invalid("snapshot.styles", "数字格式ID溢出"))?,
        );
        by_code.insert(
            element
                .attributes
                .get("formatCode")
                .cloned()
                .ok_or_else(|| invalid("snapshot.styles", "数字格式缺少formatCode"))?,
            id.clone(),
        );
    }
    let mut mapping = BTreeMap::new();
    for element in incoming.elements() {
        let id = element
            .attributes
            .get("numFmtId")
            .ok_or_else(|| invalid("snapshot.styles", "生成数字格式缺少ID"))?;
        let code = element
            .attributes
            .get("formatCode")
            .ok_or_else(|| invalid("snapshot.styles", "生成数字格式缺少formatCode"))?;
        let target = if let Some(target) = by_code.get(code) {
            target.clone()
        } else {
            let target = next_id.to_string();
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| invalid("snapshot.styles", "数字格式ID溢出"))?;
            let mut element = element.clone();
            element
                .attributes
                .insert("numFmtId".to_owned(), target.clone());
            existing.children.push(Node::Element(element));
            by_code.insert(code.clone(), target.clone());
            target
        };
        mapping.insert(id.clone(), target);
    }
    existing
        .attributes
        .insert("count".to_owned(), existing.elements().count().to_string());
    Ok(mapping)
}

/// 保留原有文本索引，新增文本按规范 XML 去重追加；计数字段由实际条目重建。
pub(super) fn merge_shared_strings(
    source: &[u8],
    generated: &[u8],
) -> Result<(Vec<u8>, Vec<usize>), WorkbookProbeError> {
    let mut source = Element::parse(source)?;
    let generated = Element::parse(generated)?;
    let mut indices = BTreeMap::new();
    for (index, item) in source.elements().enumerate() {
        indices.entry(item.bytes()?).or_insert(index);
    }
    let mut count = source.elements().count();
    let mut mapping = Vec::new();
    for item in generated.elements() {
        let bytes = item.bytes()?;
        let index = if let Some(index) = indices.get(&bytes) {
            *index
        } else {
            let index = count;
            count += 1;
            indices.insert(bytes, index);
            source.children.push(Node::Element(item.clone()));
            index
        };
        mapping.push(index);
    }
    // count 是引用总数，旧表与新表合并后不再沿用旧值；OOXML 允许省略该提示属性。
    source.attributes.remove("count");
    source
        .attributes
        .insert("uniqueCount".to_owned(), count.to_string());
    Ok((source.bytes()?, mapping))
}

/// 仅刷新数据区域、范围和下拉校验，保留原表关系、用户视图及其他扩展内容。
pub(super) fn merge_worksheet(
    source: &[u8],
    generated: &[u8],
    shared_strings: &[usize],
    styles: &[usize],
) -> Result<Vec<u8>, WorkbookProbeError> {
    let mut source = Element::parse(source)?;
    let mut generated = Element::parse(generated)?;
    remap_cells(&mut generated, shared_strings, styles)?;
    for key in ["dimension", "sheetData", "dataValidations"] {
        let incoming = generated.child(key).cloned().map(|mut element| {
            if source.attributes.get("xmlns") != generated.attributes.get("xmlns")
                && let Some(namespace) = generated.attributes.get("xmlns")
            {
                element
                    .attributes
                    .insert("xmlns".to_owned(), namespace.clone());
            }
            element
        });
        if let Some(existing) = source
            .children
            .iter()
            .position(|node| matches!(node, Node::Element(element) if element.local_name() == key))
        {
            if let Some(incoming) = incoming {
                source.children[existing] = Node::Element(incoming);
            } else {
                source.children.remove(existing);
            }
        } else if let Some(incoming) = incoming {
            // 只有可选数据验证允许原表不存在；其位置必须在表关系和页面设置之前。
            if key != "dataValidations" {
                return Err(invalid("snapshot.sheet", format!("原表缺少 {key}")));
            }
            let position = source.children.iter().position(|node| matches!(node, Node::Element(element) if matches!(element.local_name(), "hyperlinks" | "printOptions" | "pageMargins" | "pageSetup" | "headerFooter" | "drawing" | "tableParts" | "extLst"))).unwrap_or(source.children.len());
            source.children.insert(position, Node::Element(incoming));
        }
    }
    source.bytes()
}

fn remap_cells(
    element: &mut Element,
    shared_strings: &[usize],
    styles: &[usize],
) -> Result<(), WorkbookProbeError> {
    if element.local_name() == "c" {
        remap_attribute(element, "s", styles)?;
        if element
            .attributes
            .get("t")
            .is_some_and(|value| value == "s")
        {
            let value = element
                .child("v")
                .ok_or_else(|| invalid("snapshot.strings", "共享字符串单元格缺少索引"))?;
            let text = value
                .children
                .iter()
                .find_map(|node| match node {
                    Node::Event(Event::Text(text)) => Some(text.as_ref()),
                    _ => None,
                })
                .ok_or_else(|| invalid("snapshot.strings", "共享字符串索引为空"))?;
            let index: usize = std::str::from_utf8(text)
                .map_err(|error| invalid("snapshot.strings", error.to_string()))?
                .parse()
                .map_err(|error| {
                    invalid("snapshot.strings", format!("共享字符串索引无效: {error}"))
                })?;
            let mapped = shared_strings.get(index).ok_or_else(|| {
                invalid("snapshot.strings", format!("共享字符串索引 {index} 越界"))
            })?;
            let value = element
                .child_mut("v")
                .ok_or_else(|| invalid("snapshot.strings", "共享字符串缺少值"))?;
            value.children = vec![Node::Event(Event::Text(
                BytesText::new(&mapped.to_string()).into_owned(),
            ))];
        }
    }
    for child in &mut element.children {
        if let Node::Element(child) = child {
            remap_cells(child, shared_strings, styles)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_strings_preserve_old_indices_and_reuse_new_entries() {
        let source =
            br#"<sst count="3" uniqueCount="2"><si><t>old</t></si><si><t>kept</t></si></sst>"#;
        let incoming = br#"<sst><si><t>kept</t></si><si><t>new</t></si><si><t>old</t></si></sst>"#;
        let (bytes, mapping) = merge_shared_strings(source, incoming).unwrap();
        assert_eq!(mapping, [1, 2, 0]);
        let old = Element::parse(source).unwrap();
        let merged = Element::parse(&bytes).unwrap();
        for (left, right) in old.elements().zip(merged.elements()) {
            assert_eq!(left.bytes().unwrap(), right.bytes().unwrap());
        }
        let (twice, second_mapping) = merge_shared_strings(&bytes, incoming).unwrap();
        assert_eq!(bytes, twice);
        assert_eq!(mapping, second_mapping);
    }
    #[test]
    fn root_sheet_data_replacement_keeps_extension_payload() {
        let source = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><extLst><ext uri="urn:fixture:extension"><custom:sheetData xmlns:custom="urn:fixture:extension"><custom:payload>KEEP_EXTENSION_PAYLOAD</custom:payload></custom:sheetData></ext></extLst></worksheet>"#;
        let generated = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>2</v></c></row></sheetData></worksheet>"#;
        let merged = merge_worksheet(source, generated, &[], &[]).unwrap();
        let text = String::from_utf8(merged).unwrap();
        assert!(text.contains(">2<"), "{text}");
        assert!(text.contains("KEEP_EXTENSION_PAYLOAD"), "{text}");
        assert!(!text.contains("<custom:sheetData/>"), "{text}");
    }
    #[test]
    #[ignore = "需要通过 AZLW_SST_WORKBOOK 指定真实工作簿，只读验证共享文本合并"]
    fn shared_strings_real_workbook_reuses_every_existing_index() {
        use std::io::Read;
        let path = std::env::var_os("AZLW_SST_WORKBOOK").expect("指定工作簿路径");
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut bytes = Vec::new();
        zip.by_name("xl/sharedStrings.xml")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let (merged, mapping) = merge_shared_strings(&bytes, &bytes).unwrap();
        assert!(mapping.iter().copied().eq(0..mapping.len()));
        assert!(merged.len() as u64 <= crate::adapters::workbook::package::MAX_PART_BYTES);
        eprintln!(
            "shared strings: input_bytes={}, output_bytes={}, entries={}",
            bytes.len(),
            merged.len(),
            mapping.len()
        );
    }
}

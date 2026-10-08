//! XLSX 文本单元格定点编辑和未修改部件保真验证。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::BufReader;
use std::ops::Range;
use std::path::Path;

use super::XlsxError;
use super::package::{
    MAX_RAW_PACKAGE_BYTES, PackageEntry, PackageRelationship, PackageSnapshot,
    cleanup_created_file, parse_relationships, read_bounded_workbook_bytes,
    rewrite_package_from_bytes,
};
use super::paths::validate_new_xlsx_destination;
use super::workbook::worksheet_part_name;
use super::worksheet_primitives::{namespace_prefix, qualified_name};
use calamine::{Data, Reader as CalamineReader, Xlsx, open_workbook};
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::name::QName;
use quick_xml::{Reader, Writer};
use serde::Serialize;

const MAX_EXCEL_COLUMN: u32 = 16_384;
const MAX_EXCEL_ROW: u32 = 1_048_576;

/// 一次文本单元格编辑和包级保真的结构化证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TextCellEditEvidence {
    pub sheet_name: String,
    pub cell_reference: String,
    pub worksheet_part: String,
    pub replacement_value: String,
    pub preserved_prefix_bytes: usize,
    pub preserved_suffix_bytes: usize,
    pub package: PackagePreservationEvidence,
}

/// 编辑前后 ZIP 条目集合和内容比较结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PackagePreservationEvidence {
    pub entry_count: usize,
    pub unchanged_entry_count: usize,
    pub changed_parts: Vec<String>,
}

/// 只读验证 XLSX 容器、容量边界以及外部内容和宏禁令。
pub fn validate_workbook_package(
    path: &Path,
    allow_external: fn(&PackageRelationship) -> bool,
) -> Result<(), XlsxError> {
    let package: PackageSnapshot = PackageSnapshot::read(path)?;
    reject_external_content(&package, allow_external)?;
    reject_macro_content(&package)
}

/// 将现有工作簿中的一个已存在单元格改为文本，并写入新的排他目标文件。
pub fn edit_text_cell_to_new_file(
    source_path: &Path,
    destination_path: &Path,
    sheet_name: &str,
    cell_reference: &str,
    replacement_value: &str,
    allow_external: fn(&PackageRelationship) -> bool,
) -> Result<TextCellEditEvidence, XlsxError> {
    validate_new_xlsx_destination(destination_path)?;
    let coordinate: CellCoordinate = validate_cell_reference(cell_reference)?;
    validate_text_value(replacement_value)?;

    let source_bytes: Vec<u8> =
        read_bounded_workbook_bytes(source_path, MAX_RAW_PACKAGE_BYTES, "工作簿文件")?;
    let source_package: PackageSnapshot = PackageSnapshot::from_bytes(&source_bytes, source_path)?;
    reject_external_content(&source_package, allow_external)?;
    reject_macro_content(&source_package)?;
    let worksheet_part: String = worksheet_part_name(&source_package, sheet_name)?;
    let cell_edit: CellEdit = replace_text_cell(
        &worksheet_part,
        source_package.part(&worksheet_part)?,
        cell_reference,
        replacement_value,
    )?;
    let replacements: BTreeMap<String, Vec<u8>> =
        BTreeMap::from([(worksheet_part.clone(), cell_edit.bytes)]);

    write_package_to_new_file(&source_bytes, source_path, destination_path, &replacements)?;
    let validation: Result<PackagePreservationEvidence, XlsxError> = (|| {
        let destination_package: PackageSnapshot = PackageSnapshot::read(destination_path)?;
        let package: PackagePreservationEvidence = compare_packages(
            &source_package,
            &destination_package,
            &[worksheet_part.as_str()],
        )?;
        verify_text_cell(destination_path, sheet_name, coordinate, replacement_value)?;
        Ok(package)
    })();
    let package: PackagePreservationEvidence = match validation {
        Ok(package) => package,
        Err(operation) => {
            return Err(cleanup_created_file(destination_path, operation));
        }
    };

    Ok(TextCellEditEvidence {
        sheet_name: sheet_name.to_owned(),
        cell_reference: cell_reference.to_owned(),
        worksheet_part,
        replacement_value: replacement_value.to_owned(),
        preserved_prefix_bytes: cell_edit.preserved_prefix_bytes,
        preserved_suffix_bytes: cell_edit.preserved_suffix_bytes,
        package,
    })
}

/// 内部单元格编辑结果及目标区间之外的保留字节数。
#[derive(Clone, Debug, Eq, PartialEq)]
struct CellEdit {
    bytes: Vec<u8>,
    preserved_prefix_bytes: usize,
    preserved_suffix_bytes: usize,
}

/// 零基的 Excel 单元格坐标。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellCoordinate {
    pub row: u32,
    pub column: u32,
}

/// 扫描全部关系部件；图鉴超链接之外的外部目标和外部数据连接部件一律拒绝。
pub fn reject_external_content(
    package: &PackageSnapshot,
    allow_external: fn(&PackageRelationship) -> bool,
) -> Result<(), XlsxError> {
    for part_name in package.entry_names() {
        if part_name == "xl/connections.xml"
            || part_name.starts_with("xl/externalLinks/")
            || part_name.starts_with("xl/queryTables/")
        {
            return Err(XlsxError::UnsupportedPart {
                part: part_name.to_owned(),
            });
        }
    }

    let relationship_parts: Vec<String> = package
        .entry_names()
        .filter(|part_name: &&str| part_name.ends_with(".rels"))
        .map(str::to_owned)
        .collect();
    for part_name in relationship_parts {
        let relationships: Vec<PackageRelationship> =
            parse_relationships(&part_name, package.part(&part_name)?)?;
        for relationship in relationships {
            if relationship.external && !allow_external(&relationship) {
                return Err(XlsxError::ExternalRelationship {
                    part: part_name,
                    relationship_id: relationship.id,
                    target: relationship.target,
                });
            }
        }
    }
    Ok(())
}

/// `.xlsx` 扩展名不是安全边界，仍需拒绝宏部件和宏启用内容类型。
pub fn reject_macro_content(package: &PackageSnapshot) -> Result<(), XlsxError> {
    for part_name in package.entry_names() {
        let normalized = part_name.to_ascii_lowercase();
        if normalized == "xl/vbaproject.bin"
            || normalized == "xl/vbadata.xml"
            || normalized.starts_with("xl/macrosheets/")
            || normalized.starts_with("xl/dialogsheets/")
        {
            return Err(XlsxError::UnsupportedPart {
                part: part_name.to_owned(),
            });
        }
    }

    const MACRO_CONTENT_TYPES: [&str; 4] = [
        "application/vnd.ms-office.vbaproject",
        "application/vnd.ms-excel.sheet.macroenabled",
        "application/vnd.ms-excel.template.macroenabled",
        "application/vnd.ms-excel.addin.macroenabled",
    ];
    let part_name = "[Content_Types].xml";
    let mut reader = Reader::from_reader(package.part(part_name)?);
    loop {
        let event = reader.read_event().map_err(|source| {
            super::package::invalid_part(part_name, format!("XML 解码失败: {source}"))
        })?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if matches!(element.local_name().as_ref(), b"Default" | b"Override") =>
            {
                let content_type = super::package::required_attribute(
                    &reader,
                    part_name,
                    element,
                    b"ContentType",
                )?
                .to_ascii_lowercase();
                if MACRO_CONTENT_TYPES
                    .iter()
                    .any(|expected| content_type.starts_with(expected))
                {
                    return Err(XlsxError::UnsupportedPart {
                        part: "[Content_Types].xml#macro".to_owned(),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// 仅替换目标 c 元素的字节范围，工作表其余前缀和后缀保持原样。
fn replace_text_cell(
    worksheet_part: &str,
    bytes: &[u8],
    cell_reference: &str,
    replacement_value: &str,
) -> Result<CellEdit, XlsxError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut in_sheet_data: bool = false;
    let mut replacement: Option<(Range<usize>, Vec<u8>)> = None;

    loop {
        let event_start: usize = usize::try_from(reader.buffer_position())
            .map_err(|_| invalid_worksheet(worksheet_part, "XML 偏移无法在当前平台表示"))?;
        let event: Event<'_> = reader.read_event().map_err(|source: quick_xml::Error| {
            invalid_worksheet(worksheet_part, source.to_string())
        })?;
        match event {
            Event::Start(ref element) if element.local_name().as_ref() == b"sheetData" => {
                in_sheet_data = true;
            }
            Event::End(ref element) if element.local_name().as_ref() == b"sheetData" => {
                in_sheet_data = false;
            }
            Event::Start(ref element)
                if in_sheet_data
                    && element.local_name().as_ref() == b"c"
                    && cell_matches(&reader, worksheet_part, element, cell_reference)? =>
            {
                ensure_unique_cell(&replacement, worksheet_part, cell_reference)?;
                let replacement_bytes: Vec<u8> =
                    build_text_cell(worksheet_part, element, replacement_value)?;
                let element_name: Vec<u8> = element.name().as_ref().to_vec();
                reader
                    .read_to_end(QName(&element_name))
                    .map_err(|source: quick_xml::Error| {
                        invalid_worksheet(worksheet_part, source.to_string())
                    })?;
                let event_end: usize = usize::try_from(reader.buffer_position())
                    .map_err(|_| invalid_worksheet(worksheet_part, "XML 偏移无法在当前平台表示"))?;
                replacement = Some((event_start..event_end, replacement_bytes));
            }
            Event::Empty(ref element)
                if in_sheet_data
                    && element.local_name().as_ref() == b"c"
                    && cell_matches(&reader, worksheet_part, element, cell_reference)? =>
            {
                ensure_unique_cell(&replacement, worksheet_part, cell_reference)?;
                let replacement_bytes: Vec<u8> =
                    build_text_cell(worksheet_part, element, replacement_value)?;
                let event_end: usize = usize::try_from(reader.buffer_position())
                    .map_err(|_| invalid_worksheet(worksheet_part, "XML 偏移无法在当前平台表示"))?;
                replacement = Some((event_start..event_end, replacement_bytes));
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let (range, replacement_bytes): (Range<usize>, Vec<u8>) = replacement.ok_or_else(|| {
        invalid_worksheet(worksheet_part, format!("找不到目标单元格 {cell_reference}"))
    })?;
    let preserved_prefix_bytes: usize = range.start;
    let preserved_suffix_bytes: usize = bytes.len() - range.end;
    let mut edited: Vec<u8> = Vec::with_capacity(
        preserved_prefix_bytes + replacement_bytes.len() + preserved_suffix_bytes,
    );
    edited.extend_from_slice(&bytes[..range.start]);
    edited.extend_from_slice(&replacement_bytes);
    edited.extend_from_slice(&bytes[range.end..]);
    Ok(CellEdit {
        bytes: edited,
        preserved_prefix_bytes,
        preserved_suffix_bytes,
    })
}

/// 判断单元格的 r 属性是否等于目标引用。
fn cell_matches(
    reader: &Reader<&[u8]>,
    worksheet_part: &str,
    element: &BytesStart<'_>,
    cell_reference: &str,
) -> Result<bool, XlsxError> {
    let reference: Option<String> =
        super::package::optional_attribute(reader, worksheet_part, element, b"r")?;
    Ok(reference.as_deref() == Some(cell_reference))
}

/// 拒绝同一工作表内重复的目标单元格引用。
fn ensure_unique_cell(
    replacement: &Option<(Range<usize>, Vec<u8>)>,
    worksheet_part: &str,
    cell_reference: &str,
) -> Result<(), XlsxError> {
    if replacement.is_some() {
        return Err(invalid_worksheet(
            worksheet_part,
            format!("目标单元格 {cell_reference} 重复"),
        ));
    }
    Ok(())
}

/// 保留目标单元格除类型外的属性，并建立内联文本内容。
fn build_text_cell(
    worksheet_part: &str,
    element: &BytesStart<'_>,
    replacement_value: &str,
) -> Result<Vec<u8>, XlsxError> {
    let qualified_cell_name: QName<'_> = element.name();
    let cell_name: &str = std::str::from_utf8(qualified_cell_name.as_ref()).map_err(
        |source: std::str::Utf8Error| invalid_worksheet(worksheet_part, source.to_string()),
    )?;
    let namespace_prefix: Option<&str> = namespace_prefix(cell_name);
    let inline_string_name: String = qualified_name(namespace_prefix, "is");
    let text_name: String = qualified_name(namespace_prefix, "t");

    let mut cell: BytesStart<'static> = element.to_owned();
    cell.clear_attributes();
    for attribute_result in element.attributes().with_checks(false) {
        let attribute: quick_xml::events::attributes::Attribute<'_> =
            attribute_result.map_err(|source: quick_xml::events::attributes::AttrError| {
                invalid_worksheet(worksheet_part, source.to_string())
            })?;
        if attribute.key.local_name().as_ref() != b"t" {
            cell.push_attribute((attribute.key.as_ref(), attribute.value.as_ref()));
        }
    }
    cell.push_attribute((b"t".as_slice(), b"inlineStr".as_slice()));
    let cell_end: BytesEnd<'static> = cell.to_end().into_owned();
    let mut text: BytesStart<'_> = BytesStart::new(text_name.as_str());
    text.push_attribute(("xml:space", "preserve"));

    let mut writer: Writer<Vec<u8>> = Writer::new(Vec::new());
    writer
        .write_event(Event::Start(cell))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    writer
        .write_event(Event::Start(BytesStart::new(inline_string_name.as_str())))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    writer
        .write_event(Event::Start(text))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    writer
        .write_event(Event::Text(BytesText::new(replacement_value)))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(text_name.as_str())))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    writer
        .write_event(Event::End(BytesEnd::new(inline_string_name.as_str())))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    writer
        .write_event(Event::End(cell_end))
        .map_err(|source: std::io::Error| invalid_worksheet(worksheet_part, source.to_string()))?;
    Ok(writer.into_inner())
}

/// 将源包写到新的排他目标，写出失败时不留下半成品。
pub fn write_package_to_new_file(
    source_bytes: &[u8],
    source_path: &Path,
    destination_path: &Path,
    replacements: &BTreeMap<String, Vec<u8>>,
) -> Result<(), XlsxError> {
    let destination_file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination_path)
        .map_err(|source: std::io::Error| XlsxError::Io {
            stage: "建立编辑结果",
            path: destination_path.to_path_buf(),
            source,
        })?;
    let write_result: Result<(), XlsxError> = rewrite_package_from_bytes(
        source_bytes,
        destination_file,
        source_path,
        destination_path,
        replacements,
        &[],
    )
    .and_then(|file: File| {
        file.sync_all()
            .map_err(|source: std::io::Error| XlsxError::Io {
                stage: "同步编辑结果",
                path: destination_path.to_path_buf(),
                source,
            })
    });
    if let Err(operation) = write_result {
        return Err(cleanup_created_file(destination_path, operation));
    }
    Ok(())
}

/// 比较编辑前后所有解压条目，要求实际变化集合与调用方声明完全一致。
pub fn compare_packages(
    source: &PackageSnapshot,
    destination: &PackageSnapshot,
    expected_changed_parts: &[&str],
) -> Result<PackagePreservationEvidence, XlsxError> {
    let source_names: BTreeSet<&str> = source.entry_names().collect();
    let destination_names: BTreeSet<&str> = destination.entry_names().collect();
    if source_names != destination_names {
        return Err(XlsxError::InvalidOoxml {
            part: "[Content_Types].xml".to_owned(),
            message: "编辑前后 ZIP 条目集合发生变化".to_owned(),
        });
    }

    let mut changed_parts: Vec<String> = Vec::new();
    for name in source_names {
        let source_entry: &PackageEntry = source.entry(name)?;
        let destination_entry: &PackageEntry = destination.entry(name)?;
        if source_entry.is_directory != destination_entry.is_directory {
            return Err(XlsxError::InvalidOoxml {
                part: name.to_owned(),
                message: "编辑前后目录标记发生变化".to_owned(),
            });
        }
        if source_entry.bytes != destination_entry.bytes {
            changed_parts.push(name.to_owned());
        }
    }
    let mut expected: Vec<String> = expected_changed_parts
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    expected.sort();
    if expected.windows(2).any(|parts| parts[0] == parts[1]) {
        return Err(XlsxError::InvalidOoxml {
            part: "[Content_Types].xml".to_owned(),
            message: "预期变化部件集合包含重复项".to_owned(),
        });
    }
    if changed_parts != expected {
        return Err(XlsxError::InvalidOoxml {
            part: expected
                .first()
                .cloned()
                .unwrap_or_else(|| "[Content_Types].xml".to_owned()),
            message: format!("预期变化部件为 {expected:?}，实际变化为 {changed_parts:?}"),
        });
    }
    Ok(PackagePreservationEvidence {
        entry_count: destination.entry_count(),
        unchanged_entry_count: destination.entry_count() - changed_parts.len(),
        changed_parts,
    })
}

/// 用独立读取器重开结果并确认目标单元格保持文本类型和值。
fn verify_text_cell(
    path: &Path,
    sheet_name: &str,
    coordinate: CellCoordinate,
    expected_value: &str,
) -> Result<(), XlsxError> {
    let mut workbook: Xlsx<BufReader<File>> = open_workbook(path)
        .map_err(|source: calamine::XlsxError| XlsxError::XlsxRead { source })?;
    let range: calamine::Range<Data> = workbook
        .worksheet_range(sheet_name)
        .map_err(|source: calamine::XlsxError| XlsxError::XlsxRead { source })?;
    match range.get_value((coordinate.row, coordinate.column)) {
        Some(Data::String(value)) if value == expected_value => Ok(()),
        Some(value) => Err(XlsxError::InvalidOoxml {
            part: path.to_string_lossy().into_owned(),
            message: format!("目标单元格读取值错误: {value:?}"),
        }),
        None => Err(XlsxError::InvalidOoxml {
            part: path.to_string_lossy().into_owned(),
            message: "目标单元格在重开后缺失".to_owned(),
        }),
    }
}

/// 将 A1 引用解析为 Excel 边界内的零基坐标。
pub fn validate_cell_reference(reference: &str) -> Result<CellCoordinate, XlsxError> {
    let split: usize = reference
        .bytes()
        .position(|value: u8| value.is_ascii_digit())
        .ok_or_else(|| invalid_cell_reference(reference))?;
    let (column_text, row_text): (&str, &str) = reference.split_at(split);
    if column_text.is_empty()
        || row_text.is_empty()
        || row_text.starts_with('0')
        || !column_text
            .bytes()
            .all(|value: u8| value.is_ascii_uppercase())
        || !row_text.bytes().all(|value: u8| value.is_ascii_digit())
    {
        return Err(invalid_cell_reference(reference));
    }

    let mut one_based_column: u32 = 0;
    for value in column_text.bytes() {
        one_based_column = one_based_column
            .checked_mul(26)
            .and_then(|column: u32| column.checked_add(u32::from(value - b'A' + 1)))
            .ok_or_else(|| invalid_cell_reference(reference))?;
    }
    let one_based_row: u32 = row_text
        .parse()
        .map_err(|_: std::num::ParseIntError| invalid_cell_reference(reference))?;
    if one_based_column == 0
        || one_based_column > MAX_EXCEL_COLUMN
        || one_based_row == 0
        || one_based_row > MAX_EXCEL_ROW
    {
        return Err(invalid_cell_reference(reference));
    }
    Ok(CellCoordinate {
        row: one_based_row - 1,
        column: one_based_column - 1,
    })
}

/// 拒绝空文本和 XML 1.0 无法表示的控制字符。
fn validate_text_value(value: &str) -> Result<(), XlsxError> {
    if value.is_empty() || !value.chars().all(is_xml_1_0_character) {
        return Err(XlsxError::InvalidOoxml {
            part: "文本单元格输入".to_owned(),
            message: "文本不能为空且必须符合 XML 1.0 字符范围".to_owned(),
        });
    }
    Ok(())
}

/// 判断字符是否属于 XML 1.0 第五版允许范围。
fn is_xml_1_0_character(value: char) -> bool {
    matches!(value, '\u{9}' | '\u{A}' | '\u{D}')
        || ('\u{20}'..='\u{D7FF}').contains(&value)
        || ('\u{E000}'..='\u{FFFD}').contains(&value)
        || ('\u{10000}'..='\u{10FFFF}').contains(&value)
}

/// 建立稳定的单元格引用格式错误。
fn invalid_cell_reference(reference: &str) -> XlsxError {
    XlsxError::InvalidOoxml {
        part: "工作表坐标".to_owned(),
        message: format!("单元格引用 {reference:?} 不合法"),
    }
}

/// 建立带工作表部件上下文的结构错误。
fn invalid_worksheet(worksheet_part: &str, message: impl Into<String>) -> XlsxError {
    XlsxError::InvalidOoxml {
        part: worksheet_part.to_owned(),
        message: message.into(),
    }
}

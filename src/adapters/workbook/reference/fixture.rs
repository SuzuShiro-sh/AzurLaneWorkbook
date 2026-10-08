//! 代表性 XLSX 工作簿建立器。

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use rust_xlsxwriter::{
    Color, DataValidation, DocProperties, ExcelDateTime, Format, Formula, Note, Table, Workbook,
    Worksheet,
};
use zip::ZipArchive;
use zip::read::ZipFile;

use super::super::WorkbookProbeError;
use super::super::package::{PackageAddition, rewrite_package, write_new_file_bytes};
use suzushiro_xlsx_toolkit::paths::validate_new_xlsx_destination;

pub(crate) const OPAQUE_PART: &str = "customXml/azlw-preservation-fixture.xml";
pub(crate) const OPAQUE_RELATIONSHIP_ID: &str = "azlwOpaqueProbe";
pub(crate) const OPAQUE_RELATIONSHIP_TYPE: &str = "urn:azlw:workbook:opaque-fixture";
const ROOT_RELATIONSHIPS_PART: &str = "_rels/.rels";
const OPAQUE_PART_BYTES: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><azlw:opaque xmlns:azlw=\"urn:azlw:workbook:opaque-fixture\" marker=\"preserve-byte-for-byte\"/>";

/// 建立覆盖工作簿门槛中全部已知 Excel 特性的固定样本。
pub fn create_representative_workbook(path: &Path) -> Result<(), WorkbookProbeError> {
    validate_new_xlsx_destination(path)?;

    let mut workbook: Workbook = Workbook::new();
    let creation_time: ExcelDateTime = ExcelDateTime::from_ymd(2000, 1, 1).map_err(write_error)?;
    let properties: DocProperties = DocProperties::new().set_creation_datetime(&creation_time);
    workbook.set_properties(&properties);
    let header_format: Format = Format::new()
        .set_bold()
        .set_font_color(Color::White)
        .set_background_color(Color::Green);
    let text_id_format: Format = Format::new().set_num_format("@");

    let data_sheet: &mut Worksheet = workbook.add_worksheet();
    data_sheet.set_name("数据").map_err(write_error)?;
    data_sheet
        .write_with_format(0, 0, "文本编号", &header_format)
        .map_err(write_error)?;
    data_sheet
        .write_with_format(0, 1, "数量", &header_format)
        .map_err(write_error)?;
    data_sheet
        .write_with_format(0, 2, "双倍", &header_format)
        .map_err(write_error)?;
    data_sheet
        .write_with_format(1, 0, "000123", &text_id_format)
        .map_err(write_error)?;
    data_sheet.write_number(1, 1, 7.0).map_err(write_error)?;
    data_sheet
        .write_formula(1, 2, Formula::new("=B2*2"))
        .map_err(write_error)?;
    data_sheet
        .write_with_format(2, 0, "000456", &text_id_format)
        .map_err(write_error)?;
    data_sheet.write_number(2, 1, 11.0).map_err(write_error)?;
    data_sheet
        .write_formula(2, 2, Formula::new("=B3*2"))
        .map_err(write_error)?;

    let table: Table = Table::new().set_name("装备清单");
    data_sheet
        .add_table(0, 0, 2, 2, &table)
        .map_err(write_error)?;
    let note: Note = Note::new("文本编号必须保留前导零").set_author("AzurLaneWorkbook");
    data_sheet.insert_note(1, 0, &note).map_err(write_error)?;
    data_sheet.set_column_hidden(3).map_err(write_error)?;
    data_sheet.set_freeze_panes(1, 0).map_err(write_error)?;

    data_sheet
        .write_with_format(0, 4, "状态", &header_format)
        .map_err(write_error)?;
    data_sheet.write_string(1, 4, "启用").map_err(write_error)?;
    data_sheet.write_string(2, 4, "停用").map_err(write_error)?;
    data_sheet.autofilter(0, 4, 2, 4).map_err(write_error)?;
    let validation: DataValidation = DataValidation::new()
        .allow_list_strings(&["启用", "停用"])
        .map_err(write_error)?;
    data_sheet
        .add_data_validation(1, 4, 2, 4, &validation)
        .map_err(write_error)?;
    data_sheet.protect();

    let hidden_sheet: &mut Worksheet = workbook.add_worksheet();
    hidden_sheet.set_name("隐藏配置").map_err(write_error)?;
    hidden_sheet
        .write_string(0, 0, "内部配置")
        .map_err(write_error)?;
    hidden_sheet.set_hidden(true);

    let base_package: Vec<u8> = workbook.save_to_buffer().map_err(write_error)?;
    let complete_package: Vec<u8> = add_opaque_probe_part(base_package, path)?;
    write_new_file_bytes(path, &complete_package)?;
    Ok(())
}

/// 向写入器样本追加调用方未知的部件和内部关系，用于验证原样保留。
fn add_opaque_probe_part(
    base_package: Vec<u8>,
    destination_path: &Path,
) -> Result<Vec<u8>, WorkbookProbeError> {
    let mut archive: ZipArchive<Cursor<Vec<u8>>> = ZipArchive::new(Cursor::new(base_package))
        .map_err(|source: zip::result::ZipError| WorkbookProbeError::Zip {
            stage: "打开基础样本",
            path: destination_path.to_path_buf(),
            source,
        })?;
    let root_relationships: Vec<u8> = {
        let mut entry: ZipFile<'_, Cursor<Vec<u8>>> = archive
            .by_name(ROOT_RELATIONSHIPS_PART)
            .map_err(|source: zip::result::ZipError| WorkbookProbeError::Zip {
                stage: "读取根关系部件",
                path: destination_path.to_path_buf(),
                source,
            })?;
        let expected_size: usize =
            usize::try_from(entry.size()).map_err(|_| WorkbookProbeError::InvalidOoxml {
                part: ROOT_RELATIONSHIPS_PART.to_owned(),
                message: "关系部件大小无法在当前平台表示".to_owned(),
            })?;
        let mut bytes: Vec<u8> = Vec::with_capacity(expected_size);
        entry
            .read_to_end(&mut bytes)
            .map_err(|source: std::io::Error| WorkbookProbeError::Io {
                stage: "解压根关系部件",
                path: PathBuf::from(ROOT_RELATIONSHIPS_PART),
                source,
            })?;
        bytes
    };
    let updated_relationships: Vec<u8> = append_opaque_relationship(&root_relationships)?;
    let replacements: BTreeMap<String, Vec<u8>> =
        BTreeMap::from([(ROOT_RELATIONSHIPS_PART.to_owned(), updated_relationships)]);
    let additions: Vec<PackageAddition> = vec![PackageAddition {
        name: OPAQUE_PART.to_owned(),
        bytes: OPAQUE_PART_BYTES.to_vec(),
    }];
    let output: Cursor<Vec<u8>> = rewrite_package(
        archive,
        Cursor::new(Vec::new()),
        destination_path,
        destination_path,
        &replacements,
        &additions,
    )?;
    Ok(output.into_inner())
}

/// 在根关系结束标签前插入固定内部关系，其余字节保持原状。
fn append_opaque_relationship(bytes: &[u8]) -> Result<Vec<u8>, WorkbookProbeError> {
    let mut reader: Reader<&[u8]> = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let insertion_offset: usize = loop {
        let event_start: usize = usize::try_from(reader.buffer_position()).map_err(|_| {
            WorkbookProbeError::InvalidOoxml {
                part: ROOT_RELATIONSHIPS_PART.to_owned(),
                message: "XML 偏移无法在当前平台表示".to_owned(),
            }
        })?;
        let event: Event<'_> = reader.read_event().map_err(|source: quick_xml::Error| {
            WorkbookProbeError::InvalidOoxml {
                part: ROOT_RELATIONSHIPS_PART.to_owned(),
                message: source.to_string(),
            }
        })?;
        match event {
            Event::End(ref element) if element.local_name().as_ref() == b"Relationships" => {
                break event_start;
            }
            Event::Eof => {
                return Err(WorkbookProbeError::InvalidOoxml {
                    part: ROOT_RELATIONSHIPS_PART.to_owned(),
                    message: "缺少 Relationships 结束标签".to_owned(),
                });
            }
            _ => {}
        }
    };

    let mut relationship: BytesStart<'_> = BytesStart::new("Relationship");
    relationship.push_attribute(("Id", OPAQUE_RELATIONSHIP_ID));
    relationship.push_attribute(("Type", OPAQUE_RELATIONSHIP_TYPE));
    relationship.push_attribute(("Target", OPAQUE_PART));
    let mut writer: Writer<Vec<u8>> = Writer::new(Vec::new());
    writer
        .write_event(Event::Empty(relationship))
        .map_err(|source: std::io::Error| WorkbookProbeError::InvalidOoxml {
            part: ROOT_RELATIONSHIPS_PART.to_owned(),
            message: source.to_string(),
        })?;
    let relationship_bytes: Vec<u8> = writer.into_inner();
    let mut updated: Vec<u8> = Vec::with_capacity(bytes.len() + relationship_bytes.len());
    updated.extend_from_slice(&bytes[..insertion_offset]);
    updated.extend_from_slice(&relationship_bytes);
    updated.extend_from_slice(&bytes[insertion_offset..]);
    Ok(updated)
}

/// 把写出后端错误收敛到工作簿适配器的稳定错误类型。
fn write_error(source: rust_xlsxwriter::XlsxError) -> WorkbookProbeError {
    WorkbookProbeError::XlsxWrite { source }
}

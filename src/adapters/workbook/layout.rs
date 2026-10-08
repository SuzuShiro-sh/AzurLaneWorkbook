//! 从三张受控配置表加载并严格校验不可变工作簿布局。

pub(in crate::adapters::workbook) mod preview;
pub(in crate::adapters::workbook) mod template;

mod parser;
mod upgrade;
mod validation;

pub(crate) use upgrade::XlsxLayoutUpgradePort;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use thiserror::Error;

use crate::adapters::tool_root::has_link_semantics;
use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::editor::{
    CellCoordinate, reject_external_content, reject_macro_content, validate_cell_reference,
};
use crate::adapters::workbook::package::{
    PackageRelationship, PackageSnapshot, optional_attribute, parse_relationships,
    read_bounded_workbook_bytes, required_attribute, resolve_relationship_target,
};
use crate::adapters::workbook::sheet_parts::worksheet_relationships_name;
use crate::application::{
    AppError, AppErrorCode, LayoutModelError, WorkbookLayout, WorkbookLayoutRegistry, WorkbookPort,
    WorkbookRef,
};
use suzushiro_xlsx_toolkit::workbook::worksheet_part_name;

pub(super) const SHEET_SETTINGS: &str = "工作表设置";
pub(super) const FIELD_SETTINGS: &str = "字段设置";
pub(super) const FORMAT_SETTINGS: &str = "格式与下拉";
const MAX_LAYOUT_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LAYOUT_WORKSHEET_ROWS: u32 = 10_000;
const MAX_LAYOUT_WORKSHEET_COLUMNS: u32 = 64;
const MAX_LAYOUT_TABLE_ROWS: u32 = 10_000;
const TABLE_RELATIONSHIP_TRANSITIONAL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";
const TABLE_RELATIONSHIP_STRICT: &str =
    "http://purl.oclc.org/ooxml/officeDocument/relationships/table";

pub(super) const SHEET_HEADERS: [&str; 8] = [
    "稳定键",
    "生成方式",
    "显示名称",
    "顺序",
    "冻结位置",
    "默认筛选",
    "说明",
    "必需",
];
pub(super) const FIELD_HEADERS: [&str; 12] = [
    "工作表稳定键",
    "稳定字段键",
    "生成方式",
    "显示列名",
    "顺序",
    "宽度",
    "格式",
    "换行",
    "说明",
    "来源模型字段",
    "编辑器",
    "必需",
];
pub(super) const INFO_HEADERS: [&str; 3] = ["配置项稳定键", "配置值", "说明"];
pub(super) const ENUM_HEADERS: [&str; 5] =
    ["枚举分类稳定键", "枚举稳定值", "中文标签", "顺序", "说明"];
pub(super) const STYLE_HEADERS: [&str; 8] = [
    "样式稳定键",
    "背景色",
    "字体色",
    "加粗",
    "水平对齐",
    "垂直对齐",
    "换行",
    "说明",
];

/// 使用调用方提供的稳定注册表读取一个布局工作簿。
pub fn load_workbook_layout(
    path: &Path,
    registry: &WorkbookLayoutRegistry,
) -> Result<WorkbookLayout, AppError> {
    load_layout_from_xlsx(path, registry).map_err(|error| map_layout_error(path, error))
}

/// 把 XLSX 实现隐藏在应用端口之后，避免应用层接触包部件和单元格对象。
pub(crate) struct XlsxWorkbookPort {
    tool_root: crate::adapters::tool_root::ToolRoot,
    path: PathBuf,
    registry: WorkbookLayoutRegistry,
    documents: super::document::WorkbookDocuments,
}

impl XlsxWorkbookPort {
    /// 持有固定布局路径和注册表，供长生命周期应用服务重复检查。
    pub(crate) fn new(
        tool_root: crate::adapters::tool_root::ToolRoot,
        path: PathBuf,
        registry: WorkbookLayoutRegistry,
    ) -> Self {
        Self {
            tool_root,
            path,
            registry,
            documents: super::document::WorkbookDocuments::new(),
        }
    }

    pub(crate) fn with_documents(mut self, documents: super::document::WorkbookDocuments) -> Self {
        self.documents = documents;
        self
    }
}

impl WorkbookPort for XlsxWorkbookPort {
    /// 将适配器错误映射为带稳定分类和路径上下文的应用错误。
    fn load_layout(&self) -> Result<WorkbookLayout, AppError> {
        load_layout_from_xlsx(&self.path, &self.registry)
            .map_err(|error| map_layout_error(&self.path, error))
    }

    /// 从同一份数据工作簿字节读取布局选择、配装目标和库存处理。
    fn load_plan_inputs(
        &self,
        workbook: &WorkbookRef,
    ) -> Result<crate::application::WorkbookPlanInputs, AppError> {
        let path = self
            .tool_root
            .existing_file(workbook.relative_path())
            .map_err(|source| {
                AppError::from_source(
                    "workbook.plan.load",
                    AppErrorCode::WorkbookInvalid,
                    "数据工作簿路径无效",
                    source,
                )
                .with_context("path", workbook.relative_path().to_string_lossy())
            })?;
        let root_layout = load_layout_from_xlsx(&self.path, &self.registry)
            .map_err(|error| map_layout_error(&self.path, error))?;
        super::reader::load_workbook_plan_with_documents(&path, &root_layout, &self.documents)
            .map_err(|error| super::reader::map_desired_state_error(&path, error))
    }
}

/// 按包预检、表格发现、单元格解析和注册表核对的固定顺序加载布局。
fn load_layout_from_xlsx(
    path: &Path,
    registry: &WorkbookLayoutRegistry,
) -> Result<WorkbookLayout, WorkbookLayoutError> {
    validate_layout_path(path)?;
    let bytes = read_layout_bytes(path)?;
    load_layout_snapshot(path, &bytes, registry)
}

/// 让包预检和 Calamine 语义读取严格共享同一份不可变文件字节。
fn load_layout_snapshot(
    path: &Path,
    bytes: &[u8],
    registry: &WorkbookLayoutRegistry,
) -> Result<WorkbookLayout, WorkbookLayoutError> {
    let parsed = parse_layout_snapshot(path, bytes, false)?;
    validation::validate_layout(parsed, registry)
}

/// 执行严格或迁移兼容解析，并始终保留相同的包安全预检。
fn parse_layout_snapshot(
    path: &Path,
    bytes: &[u8],
    for_upgrade: bool,
) -> Result<parser::ParsedLayout, WorkbookLayoutError> {
    let package = PackageSnapshot::from_bytes(bytes, path)?;
    reject_external_content(&package)?;
    reject_macro_content(&package)?;
    validate_layout_worksheets(&package)?;
    let tables = discover_layout_tables(&package)?;
    if for_upgrade {
        parser::parse_layout_workbook_for_upgrade(bytes, &tables)
    } else {
        parser::parse_layout_workbook(bytes, &tables)
    }
}

/// 限制输入为具有 `.xlsx` 扩展名的非链接普通文件。
fn validate_layout_path(path: &Path) -> Result<(), WorkbookLayoutError> {
    let extension_is_xlsx = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("xlsx"));
    if !extension_is_xlsx {
        return Err(WorkbookProbeError::InvalidPath {
            path: path.to_path_buf(),
            message: "布局文件扩展名必须是 .xlsx".to_owned(),
        }
        .into());
    }

    let metadata = std::fs::symlink_metadata(path).map_err(|source| WorkbookProbeError::Io {
        stage: "检查布局路径",
        path: path.to_path_buf(),
        source,
    })?;
    if has_link_semantics(&metadata) || !metadata.is_file() {
        return Err(WorkbookProbeError::InvalidPath {
            path: path.to_path_buf(),
            message: "布局路径必须是普通文件且不能是符号链接".to_owned(),
        }
        .into());
    }
    Ok(())
}

/// 以固定上限一次读入原始归档，避免预检和语义解析再次按路径打开不同文件。
fn read_layout_bytes(path: &Path) -> Result<Vec<u8>, WorkbookLayoutError> {
    read_bounded_workbook_bytes(path, MAX_LAYOUT_ARCHIVE_BYTES, "布局文件").map_err(Into::into)
}

/// 在 Calamine 读取缓存值前拒绝公式和过远坐标。
fn validate_layout_worksheets(package: &PackageSnapshot) -> Result<(), WorkbookLayoutError> {
    for sheet_name in [SHEET_SETTINGS, FIELD_SETTINGS, FORMAT_SETTINGS] {
        let part_name = worksheet_part_name(package, sheet_name)?;
        validate_layout_worksheet_cells(sheet_name, &part_name, package.part(&part_name)?)?;
    }
    Ok(())
}

/// 扫描实际单元格，拒绝任意公式节点并限制布局配置允许使用的行列区域。
fn validate_layout_worksheet_cells(
    sheet_name: &str,
    part_name: &str,
    bytes: &[u8],
) -> Result<(), WorkbookLayoutError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut current_row: u32 = 0;
    let mut current_column: u32 = 0;
    let mut active_cell: Option<(u32, String)> = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|source| WorkbookProbeError::InvalidOoxml {
                part: part_name.to_owned(),
                message: source.to_string(),
            })?;
        match event {
            Event::Start(ref element) if element.local_name().as_ref() == b"row" => {
                if let Some(value) = optional_attribute(&reader, part_name, element, b"r")? {
                    current_row = parse_worksheet_row_number(sheet_name, &value)?;
                }
                current_column = 0;
                active_cell = None;
            }
            Event::End(ref element) if element.local_name().as_ref() == b"row" => {
                current_row = current_row.checked_add(1).ok_or_else(|| {
                    WorkbookLayoutError::invalid(sheet_name, None, None, "工作表推断行号溢出")
                })?;
                current_column = 0;
                active_cell = None;
            }
            Event::Start(ref element) if element.local_name().as_ref() == b"c" => {
                let (row, reference) = validate_layout_cell(
                    &reader,
                    sheet_name,
                    part_name,
                    element,
                    current_row,
                    &mut current_column,
                )?;
                active_cell = Some((row, reference));
            }
            Event::Empty(ref element) if element.local_name().as_ref() == b"c" => {
                validate_layout_cell(
                    &reader,
                    sheet_name,
                    part_name,
                    element,
                    current_row,
                    &mut current_column,
                )?;
                active_cell = None;
            }
            Event::End(ref element) if element.local_name().as_ref() == b"c" => {
                active_cell = None;
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"f" =>
            {
                let (row, key) = active_cell
                    .as_ref()
                    .map(|(row, reference)| (Some(*row), Some(reference.clone())))
                    .unwrap_or((None, None));
                return Err(WorkbookLayoutError::invalid(
                    sheet_name,
                    row,
                    key,
                    "布局配置不允许公式",
                ));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// 按 Calamine 的游标规则解析一个单元格，并在进入稠密范围前验证其坐标。
fn validate_layout_cell(
    reader: &Reader<&[u8]>,
    sheet_name: &str,
    part_name: &str,
    element: &BytesStart<'_>,
    current_row: u32,
    current_column: &mut u32,
) -> Result<(u32, String), WorkbookLayoutError> {
    let (coordinate, reference) =
        if let Some(reference) = optional_attribute(reader, part_name, element, b"r")? {
            (validate_cell_reference(&reference)?, reference)
        } else {
            let row = current_row.checked_add(1).ok_or_else(|| {
                WorkbookLayoutError::invalid(sheet_name, None, None, "工作表推断行号溢出")
            })?;
            let column = current_column.checked_add(1).ok_or_else(|| {
                WorkbookLayoutError::invalid(sheet_name, Some(row), None, "工作表推断列号溢出")
            })?;
            (
                CellCoordinate {
                    row: current_row,
                    column: *current_column,
                },
                format!("推断行 {row}、列 {column}"),
            )
        };
    let row = coordinate.row.checked_add(1).ok_or_else(|| {
        WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(reference.clone()),
            "工作表单元格行号溢出",
        )
    })?;
    *current_column = coordinate.column.checked_add(1).ok_or_else(|| {
        WorkbookLayoutError::invalid(
            sheet_name,
            Some(row),
            Some(reference.clone()),
            "工作表推断列号溢出",
        )
    })?;
    if coordinate.row >= MAX_LAYOUT_WORKSHEET_ROWS
        || coordinate.column >= MAX_LAYOUT_WORKSHEET_COLUMNS
    {
        return Err(WorkbookLayoutError::mismatch(
            sheet_name,
            Some(row),
            Some(reference.clone()),
            reference,
            format!(
                "行不超过 {MAX_LAYOUT_WORKSHEET_ROWS}、列不超过 {MAX_LAYOUT_WORKSHEET_COLUMNS}"
            ),
            "配置工作表坐标超出受控范围",
        ));
    }
    Ok((row, reference))
}

/// 将可选 `row.r` 转换为 Calamine 使用的零基推断行号。
fn parse_worksheet_row_number(sheet_name: &str, value: &str) -> Result<u32, WorkbookLayoutError> {
    let one_based: u32 = value.parse().map_err(|_| {
        WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some("row.r".to_owned()),
            format!("工作表行号 {value:?} 不是正整数"),
        )
    })?;
    one_based.checked_sub(1).ok_or_else(|| {
        WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some("row.r".to_owned()),
            "工作表行号必须从 1 开始",
        )
    })
}

/// 五类固定 Excel 表格在解析流程中的稳定标识。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LayoutTableKind {
    Sheets,
    Fields,
    Info,
    Enums,
    Styles,
}

impl LayoutTableKind {
    /// 返回对应表格必须位于的配置工作表。
    const fn sheet_name(self) -> &'static str {
        match self {
            Self::Sheets => SHEET_SETTINGS,
            Self::Fields => FIELD_SETTINGS,
            Self::Info | Self::Enums | Self::Styles => FORMAT_SETTINGS,
        }
    }
}

type LayoutTables = BTreeMap<LayoutTableKind, LayoutTable>;

/// 从 OOXML 表格部件提取的工作表位置和固定列定义。
#[derive(Clone, Debug, Eq, PartialEq)]
struct LayoutTable {
    name: String,
    sheet_name: &'static str,
    first: CellCoordinate,
    last: CellCoordinate,
    headers: Vec<String>,
}

/// 遍历三张工作表关系并把五个固定表头映射成唯一表格类别。
fn discover_layout_tables(package: &PackageSnapshot) -> Result<LayoutTables, WorkbookLayoutError> {
    let mut discovered: Vec<LayoutTable> = Vec::new();
    for sheet_name in [SHEET_SETTINGS, FIELD_SETTINGS, FORMAT_SETTINGS] {
        let worksheet_part = worksheet_part_name(package, sheet_name)?;
        let table_parts = worksheet_table_parts(package, &worksheet_part)?;
        for table_part in table_parts {
            discovered.push(parse_table_metadata(
                sheet_name,
                &table_part,
                package.part(&table_part)?,
            )?);
        }
    }
    validate_layout_table_geometry(&discovered)?;

    let mut table_names: BTreeSet<String> = BTreeSet::new();
    for table in &discovered {
        if !table_names.insert(table.name.to_lowercase()) {
            return Err(WorkbookLayoutError::invalid(
                table.sheet_name,
                None,
                Some(table.name.clone()),
                "Excel 表格名称重复",
            ));
        }
    }

    let mut tables = LayoutTables::new();
    for table in discovered {
        let kind = table_kind(&table)?;
        if tables.insert(kind, table).is_some() {
            return Err(WorkbookLayoutError::invalid(
                kind.sheet_name(),
                None,
                None,
                "同一配置结构出现多个 Excel 表格",
            ));
        }
    }
    for kind in [
        LayoutTableKind::Sheets,
        LayoutTableKind::Fields,
        LayoutTableKind::Info,
        LayoutTableKind::Enums,
        LayoutTableKind::Styles,
    ] {
        if !tables.contains_key(&kind) {
            return Err(WorkbookLayoutError::invalid(
                kind.sheet_name(),
                None,
                None,
                "缺少具有固定列的 Excel 表格",
            ));
        }
    }
    if tables.len() != 5 {
        return Err(WorkbookLayoutError::invalid(
            FORMAT_SETTINGS,
            None,
            None,
            "配置工作簿只能包含五个已定义的 Excel 表格",
        ));
    }
    Ok(tables)
}

/// 拒绝同一工作表内相交的配置表格，避免一个单元格被解释成两种结构。
fn validate_layout_table_geometry(tables: &[LayoutTable]) -> Result<(), WorkbookLayoutError> {
    for (index, left) in tables.iter().enumerate() {
        for right in tables.iter().skip(index + 1) {
            let rows_overlap = left.first.row <= right.last.row && right.first.row <= left.last.row;
            let columns_overlap =
                left.first.column <= right.last.column && right.first.column <= left.last.column;
            if left.sheet_name == right.sheet_name && rows_overlap && columns_overlap {
                return Err(WorkbookLayoutError::mismatch(
                    right.sheet_name,
                    Some(right.first.row + 1),
                    Some(right.name.clone()),
                    format!("{} 与 {} 的范围相交", left.name, right.name),
                    "同一工作表内的 Excel 表格范围互不相交",
                    "配置表格范围重叠",
                ));
            }
        }
    }
    Ok(())
}

/// 根据所在工作表和完整表头确定表格类别，不依赖可变的表格名称。
fn table_kind(table: &LayoutTable) -> Result<LayoutTableKind, WorkbookLayoutError> {
    let headers: Vec<&str> = table.headers.iter().map(String::as_str).collect();
    let kind = match table.sheet_name {
        SHEET_SETTINGS if headers == SHEET_HEADERS => LayoutTableKind::Sheets,
        FIELD_SETTINGS if headers == FIELD_HEADERS => LayoutTableKind::Fields,
        FORMAT_SETTINGS if headers == INFO_HEADERS => LayoutTableKind::Info,
        FORMAT_SETTINGS if headers == ENUM_HEADERS => LayoutTableKind::Enums,
        FORMAT_SETTINGS if headers == STYLE_HEADERS => LayoutTableKind::Styles,
        _ => {
            return Err(WorkbookLayoutError::invalid(
                table.sheet_name,
                Some(table.first.row + 1),
                Some(table.name.clone()),
                format!("表格列不符合固定契约: {:?}", table.headers),
            ));
        }
    };
    Ok(kind)
}

/// 解析工作表登记的全部表格关系，并拒绝未登记或类型错误的关系。
fn worksheet_table_parts(
    package: &PackageSnapshot,
    worksheet_part: &str,
) -> Result<Vec<String>, WorkbookLayoutError> {
    let relationship_ids =
        parse_table_relationship_ids(worksheet_part, package.part(worksheet_part)?)?;
    if relationship_ids.is_empty() {
        return Err(WorkbookLayoutError::invalid(
            worksheet_part,
            None,
            None,
            "工作表没有 Excel 表格关系",
        ));
    }

    let relationships_name = worksheet_relationships_name(worksheet_part)?;
    let relationships =
        parse_relationships(&relationships_name, package.part(&relationships_name)?)?;
    let mut referenced: BTreeSet<&str> = BTreeSet::new();
    let mut parts: Vec<String> = Vec::with_capacity(relationship_ids.len());
    for relationship_id in &relationship_ids {
        let relationship = relationships
            .iter()
            .find(|candidate| candidate.id == *relationship_id)
            .ok_or_else(|| {
                WorkbookLayoutError::invalid(
                    worksheet_part,
                    None,
                    Some(relationship_id.clone()),
                    "找不到 Excel 表格关系",
                )
            })?;
        validate_table_relationship(&relationships_name, relationship)?;
        referenced.insert(relationship.id.as_str());
        let part = resolve_relationship_target(worksheet_part, &relationship.target)?;
        package.part(&part)?;
        parts.push(part);
    }

    if relationships.iter().any(|relationship| {
        is_table_relationship_type(&relationship.relationship_type)
            && !referenced.contains(relationship.id.as_str())
    }) {
        return Err(WorkbookLayoutError::invalid(
            worksheet_part,
            None,
            None,
            "工作表关系中存在未登记的 Excel 表格",
        ));
    }
    Ok(parts)
}

/// 读取 `tableParts` 数量和关系编号并验证二者严格一致。
fn parse_table_relationship_ids(
    part_name: &str,
    bytes: &[u8],
) -> Result<Vec<String>, WorkbookLayoutError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut declared_count: Option<usize> = None;
    let mut identifiers: Vec<String> = Vec::new();
    let mut unique: BTreeSet<String> = BTreeSet::new();
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
                    return Err(WorkbookLayoutError::invalid(
                        part_name,
                        None,
                        None,
                        "tableParts 元素重复",
                    ));
                }
                let value = required_attribute(&reader, part_name, element, b"count")?;
                declared_count = Some(value.parse().map_err(|_| {
                    WorkbookLayoutError::invalid(
                        part_name,
                        None,
                        None,
                        "tableParts.count 不是非负整数",
                    )
                })?);
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tablePart" =>
            {
                let identifier = required_attribute(&reader, part_name, element, b"id")?;
                if !unique.insert(identifier.clone()) {
                    return Err(WorkbookLayoutError::invalid(
                        part_name,
                        None,
                        Some(identifier),
                        "Excel 表格关系编号重复",
                    ));
                }
                identifiers.push(identifier);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if declared_count != Some(identifiers.len()) {
        return Err(WorkbookLayoutError::invalid(
            part_name,
            None,
            None,
            "tableParts.count 与实际关系数量不一致",
        ));
    }
    Ok(identifiers)
}

/// 确认关系是包内 Excel 表格目标。
fn validate_table_relationship(
    relationships_name: &str,
    relationship: &PackageRelationship,
) -> Result<(), WorkbookLayoutError> {
    if relationship.external {
        return Err(WorkbookProbeError::ExternalRelationship {
            part: relationships_name.to_owned(),
            relationship_id: relationship.id.clone(),
            target: relationship.target.clone(),
        }
        .into());
    }
    if !is_table_relationship_type(&relationship.relationship_type) {
        return Err(WorkbookLayoutError::mismatch(
            relationships_name,
            None,
            Some(relationship.id.clone()),
            relationship.relationship_type.clone(),
            format!("{TABLE_RELATIONSHIP_TRANSITIONAL} 或 {TABLE_RELATIONSHIP_STRICT}"),
            "关系类型不是受支持的 Excel 表格关系",
        ));
    }
    Ok(())
}

/// 只接受 Transitional 与 Strict OOXML 定义的两种精确表格关系 URI。
fn is_table_relationship_type(value: &str) -> bool {
    matches!(
        value,
        TABLE_RELATIONSHIP_TRANSITIONAL | TABLE_RELATIONSHIP_STRICT
    )
}

/// 从表格部件提取名称、范围和列名，并拒绝表格公式。
fn parse_table_metadata(
    sheet_name: &'static str,
    part_name: &str,
    bytes: &[u8],
) -> Result<LayoutTable, WorkbookLayoutError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut table_name: Option<String> = None;
    let mut display_name: Option<String> = None;
    let mut range_reference: Option<String> = None;
    let mut declared_columns: Option<usize> = None;
    let mut headers: Vec<String> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|source| WorkbookProbeError::InvalidOoxml {
                part: part_name.to_owned(),
                message: source.to_string(),
            })?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"table" =>
            {
                if table_name.is_some() {
                    return Err(WorkbookLayoutError::invalid(
                        sheet_name,
                        None,
                        Some(part_name.to_owned()),
                        "表格部件包含多个 table 根元素",
                    ));
                }
                table_name = Some(required_attribute(&reader, part_name, element, b"name")?);
                display_name = Some(required_attribute(
                    &reader,
                    part_name,
                    element,
                    b"displayName",
                )?);
                range_reference = Some(required_attribute(&reader, part_name, element, b"ref")?);
                let header_row_count =
                    optional_attribute(&reader, part_name, element, b"headerRowCount")?
                        .unwrap_or_else(|| "1".to_owned());
                if header_row_count != "1" {
                    return Err(WorkbookLayoutError::mismatch(
                        sheet_name,
                        None,
                        Some(part_name.to_owned()),
                        header_row_count,
                        "1",
                        "布局配置表必须且只能包含一行表头",
                    ));
                }
                let totals_row_count =
                    optional_attribute(&reader, part_name, element, b"totalsRowCount")?
                        .unwrap_or_else(|| "0".to_owned());
                if totals_row_count != "0" {
                    return Err(WorkbookLayoutError::mismatch(
                        sheet_name,
                        None,
                        Some(part_name.to_owned()),
                        totals_row_count,
                        "0",
                        "布局配置表不允许汇总行",
                    ));
                }
                if optional_attribute(&reader, part_name, element, b"totalsRowShown")?
                    .is_some_and(|value| !matches!(value.as_str(), "0" | "false"))
                {
                    return Err(WorkbookLayoutError::invalid(
                        sheet_name,
                        None,
                        Some(part_name.to_owned()),
                        "布局配置表不允许显示汇总行",
                    ));
                }
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tableColumns" =>
            {
                if declared_columns.is_some() {
                    return Err(WorkbookLayoutError::invalid(
                        sheet_name,
                        None,
                        Some(part_name.to_owned()),
                        "tableColumns 元素重复",
                    ));
                }
                let value = required_attribute(&reader, part_name, element, b"count")?;
                declared_columns = Some(value.parse().map_err(|_| {
                    WorkbookLayoutError::invalid(
                        sheet_name,
                        None,
                        Some(part_name.to_owned()),
                        "tableColumns.count 不是非负整数",
                    )
                })?);
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if element.local_name().as_ref() == b"tableColumn" =>
            {
                headers.push(required_attribute(&reader, part_name, element, b"name")?);
            }
            Event::Start(ref element) | Event::Empty(ref element)
                if matches!(
                    element.local_name().as_ref(),
                    b"calculatedColumnFormula" | b"totalsRowFormula"
                ) =>
            {
                return Err(WorkbookLayoutError::invalid(
                    sheet_name,
                    None,
                    Some(part_name.to_owned()),
                    "布局配置表不允许计算列或汇总公式",
                ));
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let name = table_name.ok_or_else(|| {
        WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(part_name.to_owned()),
            "表格部件缺少 table 根元素",
        )
    })?;
    if display_name.as_deref() != Some(name.as_str()) {
        return Err(WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(name),
            "表格 name 与 displayName 不一致",
        ));
    }
    if declared_columns != Some(headers.len()) {
        return Err(WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(name),
            "tableColumns.count 与实际列数量不一致",
        ));
    }
    let (first, last) = parse_table_range(
        range_reference.as_deref().ok_or_else(|| {
            WorkbookLayoutError::invalid(sheet_name, None, Some(name.clone()), "表格缺少单元格范围")
        })?,
        sheet_name,
        &name,
    )?;
    let width = usize::try_from(last.column - first.column + 1).map_err(|_| {
        WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(name.clone()),
            "表格列宽无法在当前平台表示",
        )
    })?;
    let height = last.row - first.row + 1;
    if height > MAX_LAYOUT_TABLE_ROWS
        || last.row >= MAX_LAYOUT_WORKSHEET_ROWS
        || last.column >= MAX_LAYOUT_WORKSHEET_COLUMNS
    {
        return Err(WorkbookLayoutError::mismatch(
            sheet_name,
            Some(first.row + 1),
            Some(name),
            format!(
                "rows={height},last_row={},last_column={}",
                last.row + 1,
                last.column + 1
            ),
            format!(
                "rows<={MAX_LAYOUT_TABLE_ROWS},last_row<={MAX_LAYOUT_WORKSHEET_ROWS},last_column<={MAX_LAYOUT_WORKSHEET_COLUMNS}"
            ),
            "配置表格范围超出受控大小",
        ));
    }
    if first.row >= last.row || width != headers.len() {
        return Err(WorkbookLayoutError::invalid(
            sheet_name,
            Some(first.row + 1),
            Some(name),
            "表格必须包含表头、至少一行数据且范围宽度与列定义一致",
        ));
    }

    Ok(LayoutTable {
        name,
        sheet_name,
        first,
        last,
        headers,
    })
}

/// 将 OOXML 表格的 `A1:B2` 范围转换为零基坐标边界。
fn parse_table_range(
    value: &str,
    sheet_name: &str,
    table_name: &str,
) -> Result<(CellCoordinate, CellCoordinate), WorkbookLayoutError> {
    let (first, last) = value.split_once(':').ok_or_else(|| {
        WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(table_name.to_owned()),
            format!("表格范围 {value:?} 不是 A1:B2 形式"),
        )
    })?;
    if last.contains(':') {
        return Err(WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(table_name.to_owned()),
            format!("表格范围 {value:?} 包含多余分隔符"),
        ));
    }
    let first = validate_cell_reference(first)?;
    let last = validate_cell_reference(last)?;
    if first.row > last.row || first.column > last.column {
        return Err(WorkbookLayoutError::invalid(
            sheet_name,
            None,
            Some(table_name.to_owned()),
            "表格范围起点位于终点之后",
        ));
    }
    Ok((first, last))
}

/// 将内部失败映射为稳定应用错误码并复制可定位的结构与底层部件上下文。
fn map_layout_error(path: &Path, error: WorkbookLayoutError) -> AppError {
    map_layout_error_at("workbook.layout.load", path, error)
}

/// 将布局读取错误映射到调用方指定的稳定应用阶段。
fn map_layout_error_at(stage: &'static str, path: &Path, error: WorkbookLayoutError) -> AppError {
    let (code, message) = match &error {
        WorkbookLayoutError::UpgradeRequired { .. }
        | WorkbookLayoutError::UpgradeRequiredAt { .. } => (
            AppErrorCode::LayoutUpgradeRequired,
            "布局缺少当前程序注册项，请先升级布局",
        ),
        _ => (AppErrorCode::LayoutInvalid, "工作簿布局未通过严格校验"),
    };
    let location = error
        .location()
        .map(|(sheet, row, key)| (sheet.to_owned(), row, key.map(str::to_owned)));
    let values = error
        .values()
        .map(|(actual, expected)| (actual.to_owned(), expected.to_owned()));
    let missing = error.missing().map(<[String]>::to_vec);
    let workbook_context = error.workbook_context();
    let mut application_error = AppError::from_source(stage, code, message, error)
        .with_context("path", path.to_string_lossy());
    if let Some((sheet, row, key)) = location {
        application_error = application_error.with_context("sheet", sheet);
        if let Some(row) = row {
            application_error = application_error.with_context("row", row.to_string());
        }
        if let Some(key) = key {
            application_error = application_error.with_context("key", key);
        }
    }
    if let Some((actual, expected)) = values {
        application_error = application_error
            .with_context("actual", actual)
            .with_context("expected", expected);
    }
    if let Some(missing) = missing {
        application_error = application_error.with_context("missing", missing.join(","));
    }
    for (key, value) in workbook_context {
        application_error = application_error.with_context(key, value);
    }
    application_error
}

/// 布局适配器内部保留的底层、结构、升级和摘要错误。
#[derive(Debug, Error)]
enum WorkbookLayoutError {
    #[error(transparent)]
    Workbook(#[from] WorkbookProbeError),
    #[error("布局配置无效: {diagnostic}")]
    Invalid { diagnostic: Box<LayoutDiagnostic> },
    #[error("布局缺少当前程序注册项: {missing:?}")]
    UpgradeRequired { missing: Vec<String> },
    #[error("布局需要升级: {missing:?}")]
    UpgradeRequiredAt {
        missing: Vec<String>,
        diagnostic: Box<LayoutDiagnostic>,
    },
    #[error(transparent)]
    Model(#[from] LayoutModelError),
    #[error("计算规范布局摘要失败: {0}")]
    Digest(#[from] serde_json::Error),
}

impl WorkbookLayoutError {
    /// 建立具有可选行号和稳定键的结构错误。
    fn invalid(
        sheet: impl Into<String>,
        row: Option<u32>,
        key: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Invalid {
            diagnostic: Box::new(LayoutDiagnostic {
                sheet: sheet.into(),
                row,
                key,
                actual: None,
                expected: None,
                message: message.into(),
            }),
        }
    }

    /// 建立同时携带当前值和期望值的可定位契约错误。
    fn mismatch(
        sheet: impl Into<String>,
        row: Option<u32>,
        key: Option<String>,
        actual: impl Into<String>,
        expected: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Invalid {
            diagnostic: Box::new(LayoutDiagnostic {
                sheet: sheet.into(),
                row,
                key,
                actual: Some(actual.into()),
                expected: Some(expected.into()),
                message: message.into(),
            }),
        }
    }

    /// 建立携带具体配置位置和版本差异的升级错误。
    fn upgrade_required_at(
        missing: Vec<String>,
        sheet: impl Into<String>,
        row: u32,
        key: impl Into<String>,
        actual: impl Into<String>,
        expected: impl Into<String>,
    ) -> Self {
        Self::UpgradeRequiredAt {
            missing,
            diagnostic: Box::new(LayoutDiagnostic {
                sheet: sheet.into(),
                row: Some(row),
                key: Some(key.into()),
                actual: Some(actual.into()),
                expected: Some(expected.into()),
                message: "布局版本低于程序要求".to_owned(),
            }),
        }
    }

    /// 返回可以复制到应用错误上下文中的配置位置。
    fn location(&self) -> Option<(&str, Option<u32>, Option<&str>)> {
        match self {
            Self::Invalid { diagnostic } | Self::UpgradeRequiredAt { diagnostic, .. } => {
                Some((&diagnostic.sheet, diagnostic.row, diagnostic.key.as_deref()))
            }
            _ => None,
        }
    }

    /// 返回契约比较的当前值和期望值。
    fn values(&self) -> Option<(&str, &str)> {
        match self {
            Self::Invalid { diagnostic } | Self::UpgradeRequiredAt { diagnostic, .. } => diagnostic
                .actual
                .as_deref()
                .zip(diagnostic.expected.as_deref()),
            _ => None,
        }
    }

    /// 返回需要布局升级补齐的注册项。
    fn missing(&self) -> Option<&[String]> {
        match self {
            Self::UpgradeRequired { missing } | Self::UpgradeRequiredAt { missing, .. } => {
                Some(missing)
            }
            _ => None,
        }
    }

    /// 把底层 OOXML 部件和外部关系标识提升为机器可读上下文。
    fn workbook_context(&self) -> Vec<(&'static str, String)> {
        let Self::Workbook(source) = self else {
            return Vec::new();
        };
        match source {
            WorkbookProbeError::MissingPart { part }
            | WorkbookProbeError::InvalidOoxml { part, .. }
            | WorkbookProbeError::UnsupportedPart { part } => {
                vec![("part", part.clone())]
            }
            WorkbookProbeError::ExternalRelationship {
                part,
                relationship_id,
                target,
            } => vec![
                ("part", part.clone()),
                ("relationship_id", relationship_id.clone()),
                ("target", target.clone()),
            ],
            _ => Vec::new(),
        }
    }
}

/// 保存可复制到应用错误上下文的布局诊断字段。
#[derive(Debug)]
struct LayoutDiagnostic {
    sheet: String,
    row: Option<u32>,
    key: Option<String>,
    actual: Option<String>,
    expected: Option<String>,
    message: String,
}

impl fmt::Display for LayoutDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<suzushiro_xlsx_toolkit::XlsxError> for WorkbookLayoutError {
    fn from(source: suzushiro_xlsx_toolkit::XlsxError) -> Self {
        Self::from(crate::adapters::workbook::WorkbookProbeError::from(source))
    }
}

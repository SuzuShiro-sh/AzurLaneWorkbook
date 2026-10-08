//! 定义最终数据工作簿使用的版本化投影、稳定字段和行值契约。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use suzushiro_xlsx_toolkit::limits::{
    MAX_EXCEL_CELL_UTF16_UNITS, MAX_EXCEL_DATA_ROWS, MAX_EXCEL_EXACT_INTEGER,
    MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE, MIN_EXCEL_UNIX_MILLIS,
};

use serde::Serialize;
use suzushiro_content_digest::sha256_compact_json;
use thiserror::Error;

use super::super::{
    LayoutEditor, LayoutModelError, LayoutValueFormat, RegisteredLayoutField,
    WorkbookLayoutRegistry,
};

mod registry;

/// 当前程序承诺生成和读取的工作簿投影版本。
pub const WORKBOOK_PROJECTION_SCHEMA_VERSION: u32 = 19;

/// 工作簿生成器、解析器和布局模板共享的第四版数据投影。
///
/// 行值只使用稳定工作表键和字段键，不暴露设备 DTO、XLSX 单元格或布局显示名称。
/// 投影摘要覆盖完整来源身份、全部注册表结构和所有行值，但不包含摘要自身。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkbookProjectionV4 {
    schema_version: u32,
    source: WorkbookProjectionSource,
    registry_sha256: String,
    sheets: Vec<WorkbookProjectionSheet>,
    content_sha256: String,
}

type ReplacementRows = BTreeMap<String, Vec<(String, BTreeMap<String, WorkbookProjectionValue>)>>;

impl WorkbookProjectionV4 {
    /// 替换指定表的规范行并重新校验完整投影及摘要，保留其他表的稳定行身份。
    pub(crate) fn with_replaced_rows(
        mut self,
        mut replacements: ReplacementRows,
    ) -> Result<Self, WorkbookProjectionError> {
        for key in replacements.keys() {
            if self.sheet(key).is_none() {
                return Err(WorkbookProjectionError::UnknownSheet {
                    sheet_key: key.clone(),
                    object_ref: format!("sheet:{key}"),
                });
            }
        }
        let refresh_equipment_order = replacements
            .keys()
            .any(|key| key == "equipment_inventory" || key == "resource_recipes");
        let validator = WorkbookProjectionBuilder::new(self.source.clone())?;
        for sheet in &mut self.sheets {
            let Some(rows) = replacements.remove(sheet.stable_key()) else {
                continue;
            };
            let fields = validator
                .index
                .sheet_fields
                .get(sheet.stable_key())
                .ok_or_else(|| WorkbookProjectionError::UnknownSheet {
                    sheet_key: sheet.stable_key.clone(),
                    object_ref: format!("sheet:{}", sheet.stable_key),
                })?;
            let mut built = Vec::with_capacity(rows.len());
            let mut seen = BTreeSet::new();
            for (object_ref, values) in rows {
                if !seen.insert(object_ref.clone()) {
                    return Err(WorkbookProjectionError::DuplicateRowReference {
                        sheet_key: sheet.stable_key.clone(),
                        object_ref,
                    });
                }
                ensure_row_capacity(sheet.stable_key(), &object_ref, built.len())?;
                built.push(build_projection_row(
                    sheet.stable_key(),
                    object_ref,
                    values,
                    fields,
                    &validator.index.enum_options,
                )?);
            }
            built.sort_by(|left, right| left.object_ref().cmp(right.object_ref()));
            sheet.rows = built;
        }
        if refresh_equipment_order {
            let composable = directly_composable_equipment_configs(
                self.sheets
                    .iter()
                    .find(|sheet| sheet.stable_key == "resource_recipes")
                    .into_iter()
                    .flat_map(|sheet| sheet.rows.iter()),
            );
            if let Some(sheet) = self
                .sheets
                .iter_mut()
                .find(|sheet| sheet.stable_key == "equipment_inventory")
            {
                sheet
                    .rows
                    .sort_by_key(|row| row.equipment_inventory_sort_group(&composable));
            }
        }
        let digest_input = WorkbookProjectionDigest {
            schema_version: self.schema_version,
            source: &self.source,
            registry_sha256: &self.registry_sha256,
            sheets: &self.sheets,
        };
        self.content_sha256 = sha256_compact_json(&digest_input)
            .map_err(|source| WorkbookProjectionError::Encode { source })?;
        Ok(self)
    }

    /// 返回工作簿数据投影自身的 schema 版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 用一张已有业务表的字段结构扩展到目标行数。新行使用独立对象引用。
    #[cfg(test)]
    pub(crate) fn scale_business_sheet(
        self,
        sheet_key: &str,
        target_rows: usize,
    ) -> Result<Self, WorkbookProjectionError> {
        let template = self
            .sheet(sheet_key)
            .and_then(|sheet| sheet.rows().first())
            .cloned()
            .ok_or_else(|| WorkbookProjectionError::UnknownSheet {
                sheet_key: sheet_key.to_owned(),
                object_ref: format!("sheet:{sheet_key}"),
            })?;
        let existing = self
            .sheet(sheet_key)
            .map(|sheet| sheet.rows().len())
            .unwrap_or(0);
        if existing >= target_rows {
            return Ok(self);
        }
        let mut rows = self
            .sheet(sheet_key)
            .unwrap()
            .rows()
            .iter()
            .map(|row| (row.object_ref().to_owned(), row.values().clone()))
            .collect::<Vec<_>>();
        for index in existing..target_rows {
            let mut values = template.values().clone();
            if let Some(WorkbookProjectionValue::Text(text)) = values.get_mut("instance_id") {
                *text = format!("{text}:scale:{index}");
            }
            rows.push((format!("{}:scale:{index}", template.object_ref()), values));
        }
        self.with_replaced_rows(BTreeMap::from([(sheet_key.to_owned(), rows)]))
    }

    /// 返回建立投影时使用的完整游戏状态身份。
    pub const fn source(&self) -> &WorkbookProjectionSource {
        &self.source
    }

    /// 返回字段格式、编辑器、枚举和样式注册表的稳定 SHA-256。
    pub fn registry_sha256(&self) -> &str {
        &self.registry_sha256
    }

    /// 返回注册表定义的全部投影表，按工作表稳定键排序。
    pub fn sheets(&self) -> &[WorkbookProjectionSheet] {
        &self.sheets
    }

    /// 按稳定键查找一张投影表。
    pub fn sheet(&self, stable_key: &str) -> Option<&WorkbookProjectionSheet> {
        self.sheets
            .binary_search_by(|sheet| sheet.stable_key().cmp(stable_key))
            .ok()
            .map(|index| &self.sheets[index])
    }

    /// 从合成配方的实际产出与资源约束派生当前可直接合成的具体配置。
    pub(crate) fn directly_composable_equipment_configs(&self) -> BTreeSet<String> {
        directly_composable_equipment_configs(
            self.sheet("resource_recipes")
                .into_iter()
                .flat_map(|sheet| sheet.rows()),
        )
    }

    /// 返回规范投影正文的小写 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// 完整游戏状态进入工作簿投影时保留的版本和内容身份。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookProjectionSource {
    read_scope: crate::domain::GameReadScope,
    game_state_schema_version: u32,
    module_sha256: String,
    owned_state_schema_version: u32,
    ship_details_schema_version: u32,
    ship_catalog_schema_version: u32,
    equipment_catalog_schema_version: u32,
    raw_records_schema_version: u32,
    owned_state_content_sha256: String,
    ship_roster_content_sha256: String,
    ship_catalog_content_sha256: String,
    equipment_catalog_content_sha256: String,
    raw_records_content_sha256: String,
    game_state_content_sha256: String,
}

impl WorkbookProjectionSource {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        game_state_schema_version: u32,
        module_sha256: String,
        owned_state_schema_version: u32,
        ship_details_schema_version: u32,
        ship_catalog_schema_version: u32,
        equipment_catalog_schema_version: u32,
        raw_records_schema_version: u32,
        owned_state_content_sha256: String,
        ship_roster_content_sha256: String,
        ship_catalog_content_sha256: String,
        equipment_catalog_content_sha256: String,
        raw_records_content_sha256: String,
        game_state_content_sha256: String,
    ) -> Self {
        Self {
            read_scope: crate::domain::GameReadScope::full(),
            game_state_schema_version,
            module_sha256,
            owned_state_schema_version,
            ship_details_schema_version,
            ship_catalog_schema_version,
            equipment_catalog_schema_version,
            raw_records_schema_version,
            owned_state_content_sha256,
            ship_roster_content_sha256,
            ship_catalog_content_sha256,
            equipment_catalog_content_sha256,
            raw_records_content_sha256,
            game_state_content_sha256,
        }
    }

    pub const fn read_scope(&self) -> crate::domain::GameReadScope {
        self.read_scope
    }

    pub(crate) fn with_read_scope(mut self, scope: crate::domain::GameReadScope) -> Self {
        self.read_scope = scope;
        self
    }

    /// 返回完整游戏状态领域契约版本。
    pub const fn game_state_schema_version(&self) -> u32 {
        self.game_state_schema_version
    }

    /// 返回目标客户端模块 SHA-256。
    pub fn module_sha256(&self) -> &str {
        &self.module_sha256
    }

    /// 返回账号持有状态运行时 schema 版本。
    pub const fn owned_state_schema_version(&self) -> u32 {
        self.owned_state_schema_version
    }

    /// 返回舰船详情运行时 schema 版本。
    pub const fn ship_details_schema_version(&self) -> u32 {
        self.ship_details_schema_version
    }

    /// 返回舰船静态目录 schema 版本。
    pub const fn ship_catalog_schema_version(&self) -> u32 {
        self.ship_catalog_schema_version
    }

    /// 返回装备目录 schema 版本。
    pub const fn equipment_catalog_schema_version(&self) -> u32 {
        self.equipment_catalog_schema_version
    }

    /// 返回装备原始记录 schema 版本。
    pub const fn raw_records_schema_version(&self) -> u32 {
        self.raw_records_schema_version
    }

    /// 返回账号持有状态内容 SHA-256。
    pub fn owned_state_content_sha256(&self) -> &str {
        &self.owned_state_content_sha256
    }

    /// 返回舰船名册内容 SHA-256。
    pub fn ship_roster_content_sha256(&self) -> &str {
        &self.ship_roster_content_sha256
    }

    /// 返回舰船静态目录内容 SHA-256。
    pub fn ship_catalog_content_sha256(&self) -> &str {
        &self.ship_catalog_content_sha256
    }

    /// 返回装备目录内容 SHA-256。
    pub fn equipment_catalog_content_sha256(&self) -> &str {
        &self.equipment_catalog_content_sha256
    }

    /// 返回装备原始记录内容 SHA-256。
    pub fn raw_records_content_sha256(&self) -> &str {
        &self.raw_records_content_sha256
    }

    /// 返回输入 `GameState` 的稳定语义 SHA-256。
    pub fn game_state_content_sha256(&self) -> &str {
        &self.game_state_content_sha256
    }
}

type TechnologyRank = (bool, i64, i64, std::cmp::Reverse<i64>);

fn technology_number(row: &WorkbookProjectionRow, key: &str) -> i64 {
    match row.value(key) {
        Some(WorkbookProjectionValue::Integer(value)) => *value,
        Some(WorkbookProjectionValue::Text(value)) => value.parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

fn technology_rank(row: &WorkbookProjectionRow) -> TechnologyRank {
    (
        !matches!(row.value("source_type"), Some(WorkbookProjectionValue::Text(value)) if value == "unowned"),
        technology_number(row, "current_stars"),
        technology_number(row, "level"),
        std::cmp::Reverse(technology_number(row, "instance_id")),
    )
}

fn technology_row_order(row: &WorkbookProjectionRow) -> (bool, &str) {
    (
        matches!(row.value("source_type"), Some(WorkbookProjectionValue::Text(value)) if value == "unowned"),
        row.object_ref(),
    )
}

fn row_technology_categories(row: &WorkbookProjectionRow) -> BTreeSet<String> {
    super::technology::technology_categories(
        ["technology_get", "technology_level"]
            .into_iter()
            .filter_map(|key| match row.value(key) {
                Some(WorkbookProjectionValue::Text(value)) => Some(value.as_str()),
                _ => None,
            }),
    )
}

/// 一张投影表的固定字段顺序和稳定数据行。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkbookProjectionSheet {
    stable_key: String,
    field_keys: Vec<String>,
    rows: Vec<WorkbookProjectionRow>,
}

impl WorkbookProjectionSheet {
    /// 一次选出各组代表并按全表分类分配；没有代表的分类仍保留空页。
    pub(crate) fn technology_category_sheets(&self) -> Vec<(String, Self)> {
        let mut categories = BTreeSet::new();
        let mut chosen: BTreeMap<&str, (TechnologyRank, BTreeSet<String>, &WorkbookProjectionRow)> =
            BTreeMap::new();
        for row in &self.rows {
            let row_categories = row_technology_categories(row);
            categories.extend(row_categories.iter().cloned());
            let group = match row.value("group_id") {
                Some(WorkbookProjectionValue::Text(value)) => value.as_str(),
                _ => row.object_ref(),
            };
            let row_rank = technology_rank(row);
            match chosen.get_mut(group) {
                Some(selected) if row_rank > selected.0 => {
                    *selected = (row_rank, row_categories, row);
                }
                Some(_) => {}
                None => {
                    chosen.insert(group, (row_rank, row_categories, row));
                }
            }
        }
        let mut representatives: Vec<_> = chosen
            .into_values()
            .map(|(_, cats, row)| (cats, row))
            .collect();
        representatives.sort_by(|(_, left), (_, right)| {
            technology_row_order(left).cmp(&technology_row_order(right))
        });
        categories
            .iter()
            .map(|category| {
                let rows = representatives
                    .iter()
                    .filter(|(row_categories, _)| row_categories.contains(category))
                    .map(|(_, row)| (*row).clone())
                    .collect();
                (category.clone(), self.with_rows(rows))
            })
            .collect()
    }

    fn with_rows(&self, rows: Vec<WorkbookProjectionRow>) -> Self {
        Self {
            stable_key: self.stable_key.clone(),
            field_keys: self.field_keys.clone(),
            rows,
        }
    }

    /// 返回工作表稳定键。
    pub fn stable_key(&self) -> &str {
        &self.stable_key
    }

    /// 返回按字段稳定键排序的完整字段集合。
    pub fn field_keys(&self) -> &[String] {
        &self.field_keys
    }

    /// 返回全部行：装备按持有、可合成未持有、其余未持有分组，组内及其他表按稳定业务对象引用排序。
    pub fn rows(&self) -> &[WorkbookProjectionRow] {
        &self.rows
    }
}

/// 一行只允许通过所属工作表注册表建立的稳定字段值。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkbookProjectionRow {
    object_ref: String,
    values: BTreeMap<String, WorkbookProjectionValue>,
}

impl WorkbookProjectionRow {
    /// 返回映射器用于排序、去重和诊断的稳定业务对象引用。
    pub fn object_ref(&self) -> &str {
        &self.object_ref
    }

    /// 按稳定字段键读取一个值。
    pub fn value(&self, stable_key: &str) -> Option<&WorkbookProjectionValue> {
        self.values.get(stable_key)
    }

    pub(crate) fn is_directly_composable_equipment(&self, configs: &BTreeSet<String>) -> bool {
        matches!(self.value("config_id"), Some(WorkbookProjectionValue::Text(id)) if configs.contains(id))
    }

    pub(crate) fn has_family_owned_enhance_distribution(&self) -> bool {
        match self.value("family_owned_enhance_distribution") {
            Some(WorkbookProjectionValue::Text(value)) => !value.is_empty(),
            _ => false,
        }
    }

    pub(crate) fn equipment_inventory_sort_group(&self, composable: &BTreeSet<String>) -> u8 {
        equipment_inventory_row_group(
            matches!(self.value("source_type"), Some(WorkbookProjectionValue::Text(value)) if value == "unowned"),
            self.is_directly_composable_equipment(composable),
            self.has_family_owned_enhance_distribution(),
        )
    }

    /// 返回按稳定字段键排序的全部值。
    pub const fn values(&self) -> &BTreeMap<String, WorkbookProjectionValue> {
        &self.values
    }

    /// 使用生产注册表校验并规范化一张工作表的独立行集。
    pub(crate) fn validated_for_sheet(
        sheet_key: &str,
        rows: Vec<(String, Vec<(String, WorkbookProjectionValue)>)>,
    ) -> Result<Vec<Self>, WorkbookProjectionError> {
        let index = projection_registry_index()?;
        let diagnostic_ref = rows
            .first()
            .map(|(object_ref, _)| object_ref.clone())
            .unwrap_or_else(|| format!("sheet:{sheet_key}"));
        let Some(fields) = index.sheet_fields.get(sheet_key) else {
            return Err(WorkbookProjectionError::UnknownSheet {
                sheet_key: sheet_key.to_owned(),
                object_ref: diagnostic_ref,
            });
        };
        let mut validated = BTreeMap::new();
        for (object_ref, values) in rows {
            let row = build_projection_row(
                sheet_key,
                object_ref.clone(),
                values,
                fields,
                &index.enum_options,
            )?;
            if validated.contains_key(&object_ref) {
                return Err(WorkbookProjectionError::DuplicateRowReference {
                    sheet_key: sheet_key.to_owned(),
                    object_ref,
                });
            }
            ensure_row_capacity(sheet_key, &object_ref, validated.len())?;
            validated.insert(object_ref, row);
        }
        Ok(validated.into_values().collect())
    }
}

/// 与具体 XLSX 库无关的工作簿单元格语义值。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum WorkbookProjectionValue {
    /// 语义上没有值，写入器应保留空白单元格。
    Blank,
    /// 文本和所有必须按文本保存的标识。
    Text(String),
    /// Excel 整数列使用的有符号整数。
    Integer(i64),
    /// 已由领域模型保证有限的十进制数。
    Decimal(f64),
    /// 与时区无关的 UTC Unix 毫秒时间戳，由工作簿适配器转换为日期单元格。
    DateTimeUnixMillis(i64),
    /// 由写入器按布局显示为“是”或“否”的布尔值。
    Boolean(bool),
    /// 由写入器根据布局中文标签显示的稳定枚举值。
    Enumeration {
        category_key: String,
        stable_value: String,
    },
    /// 完整 JSON 或按 `_原始数据` 契约切分的一段规范 JSON。
    Json(String),
}

impl WorkbookProjectionValue {
    /// 建立文本值。
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// 建立稳定枚举值。
    pub fn enumeration(category_key: impl Into<String>, stable_value: impl Into<String>) -> Self {
        Self::Enumeration {
            category_key: category_key.into(),
            stable_value: stable_value.into(),
        }
    }

    /// 建立与具体日期时间库无关的 UTC Unix 毫秒时间戳。
    pub const fn date_time_unix_millis(value: i64) -> Self {
        Self::DateTimeUnixMillis(value)
    }
}

/// 建立投影时发现的字段覆盖、值类型或规范摘要错误。
#[derive(Debug, Error)]
pub enum WorkbookProjectionError {
    /// 程序内置投影注册表自身不合法。
    #[error("工作簿投影注册表不合法: {source}")]
    Registry {
        #[source]
        source: LayoutModelError,
    },
    /// 映射器引用了未登记的工作表。
    #[error("对象 {object_ref} 使用了未登记的工作表 {sheet_key}")]
    UnknownSheet {
        sheet_key: String,
        object_ref: String,
    },
    /// 同一张表重复使用了同一个稳定行引用。
    #[error("工作表 {sheet_key} 重复提供对象 {object_ref}")]
    DuplicateRowReference {
        sheet_key: String,
        object_ref: String,
    },
    /// 一行重复提供同一个稳定字段。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 重复提供字段 {field_key}")]
    DuplicateField {
        sheet_key: String,
        object_ref: String,
        field_key: String,
    },
    /// 一行提供了所属工作表未登记的字段。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 提供了未知字段 {field_key}")]
    UnknownField {
        sheet_key: String,
        object_ref: String,
        field_key: String,
    },
    /// 一行没有覆盖所属工作表的全部稳定字段。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 缺少字段 {field_keys:?}")]
    MissingFields {
        sheet_key: String,
        object_ref: String,
        field_keys: Vec<String>,
    },
    /// 值类型与字段登记的固定格式不一致。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 不接受值类型 {value_kind}")]
    ValueTypeMismatch {
        sheet_key: String,
        object_ref: String,
        field_key: String,
        value_kind: &'static str,
    },
    /// 小数值不是 Excel 和规范 JSON 都能稳定表达的有限数值。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 使用了非有限小数")]
    NonFiniteDecimal {
        sheet_key: String,
        object_ref: String,
        field_key: String,
    },
    /// 整数超出 Excel 数值单元格能够无损表达的闭区间。
    #[error(
        "工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 使用整数 {value}，超过精确范围 [{minimum}, {maximum}]"
    )]
    InexactExcelInteger {
        sheet_key: String,
        object_ref: String,
        field_key: String,
        value: i64,
        minimum: i64,
        maximum: i64,
    },
    /// 日期时间超出固定 XLSX 写入器可表达的年份范围。
    #[error(
        "工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 使用时间戳 {value}，允许范围为 [{minimum}, {maximum_exclusive})"
    )]
    DateTimeOutOfRange {
        sheet_key: String,
        object_ref: String,
        field_key: String,
        value: i64,
        minimum: i64,
        maximum_exclusive: i64,
    },
    /// 枚举值或枚举分类没有进入稳定注册表。
    #[error(
        "工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 使用了未知枚举 {category_key}.{stable_value}"
    )]
    UnknownEnumeration {
        sheet_key: String,
        object_ref: String,
        field_key: String,
        category_key: String,
        stable_value: String,
    },
    /// 游戏的无符号数值超出投影整数范围。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 的整数 {value} 超出 i64 范围")]
    IntegerOverflow {
        sheet_key: String,
        object_ref: String,
        field_key: String,
        value: u64,
    },
    /// 文本或 JSON 无法完整放入一个 Excel 单元格。
    #[error(
        "工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 含 {actual_utf16_units} 个 UTF-16 单元，超过单元格上限 {maximum_utf16_units}"
    )]
    CellTextLimitExceeded {
        sheet_key: String,
        object_ref: String,
        field_key: String,
        maximum_utf16_units: usize,
        actual_utf16_units: usize,
    },
    /// 一张表的数据行无法完整放入 Excel 工作表。
    #[error(
        "工作表 {sheet_key} 加入对象 {object_ref} 后有 {actual_rows} 行，超过数据行上限 {maximum_rows}"
    )]
    RowLimitExceeded {
        sheet_key: String,
        object_ref: String,
        maximum_rows: usize,
        actual_rows: usize,
    },
    /// 完整状态中的关联对象在投影索引中不存在。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 找不到 {target_type} {target_ref}")]
    MissingReference {
        sheet_key: String,
        object_ref: String,
        target_type: &'static str,
        target_ref: String,
    },
    /// 单行字段无法表达同一装备族的多条基础合成配方。
    #[error("装备族 {family_id} 同时关联了多条基础合成配方 {recipe_ids:?}")]
    AmbiguousComposeRecipe {
        family_id: u64,
        recipe_ids: Vec<u64>,
    },
    /// 计数、资源或行号计算超出冻结整数范围。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 计算 {operation} 时整数溢出")]
    ArithmeticOverflow {
        sheet_key: &'static str,
        object_ref: String,
        operation: &'static str,
    },
    /// JSON 输出字段不能编码为稳定正文。
    #[error("工作表 {sheet_key} 的对象 {object_ref} 字段 {field_key} 编码 JSON 失败: {source}")]
    JsonEncode {
        sheet_key: &'static str,
        object_ref: String,
        field_key: &'static str,
        #[source]
        source: serde_json::Error,
    },
    /// 投影正文不能规范序列化。
    #[error("编码工作簿投影摘要失败: {source}")]
    Encode {
        #[source]
        source: serde_json::Error,
    },
    /// 程序内置投影注册表不能规范序列化。
    #[error("编码工作簿投影注册表摘要失败: {source}")]
    RegistryEncode {
        #[source]
        source: serde_json::Error,
    },
}

/// 固定投影注册表的查询索引。字段、枚举和摘要在进程内只建立一次。
struct ProjectionRegistryIndex {
    registry_sha256: String,
    sheet_fields: BTreeMap<String, BTreeMap<String, RegisteredLayoutField>>,
    enum_options: BTreeSet<(String, String)>,
}

fn projection_registry_index() -> Result<&'static ProjectionRegistryIndex, WorkbookProjectionError>
{
    static INDEX: OnceLock<ProjectionRegistryIndex> = OnceLock::new();
    if let Some(index) = INDEX.get() {
        return Ok(index);
    }
    let registry = WorkbookProjectionV4::layout_registry()
        .map_err(|source| WorkbookProjectionError::Registry { source })?;
    let registry_sha256 = projection_registry_sha256(&registry)?;
    let mut sheet_fields: BTreeMap<String, BTreeMap<String, RegisteredLayoutField>> = registry
        .sheets()
        .iter()
        .map(|sheet| (sheet.stable_key().to_owned(), BTreeMap::new()))
        .collect();
    for field in registry.fields() {
        sheet_fields
            .get_mut(field.sheet_key())
            .expect("布局注册表已经校验字段所属工作表")
            .insert(field.stable_key().to_owned(), field.clone());
    }
    let enum_options = registry
        .enum_options()
        .iter()
        .map(|option| {
            (
                option.category_key().to_owned(),
                option.stable_value().to_owned(),
            )
        })
        .collect();
    Ok(INDEX.get_or_init(|| ProjectionRegistryIndex {
        registry_sha256,
        sheet_fields,
        enum_options,
    }))
}

/// 复用同一注册表逐行建立并校验投影，避免大型目录为每行重复构造索引。
pub(crate) struct WorkbookProjectionBuilder {
    source: WorkbookProjectionSource,
    index: &'static ProjectionRegistryIndex,
    rows: BTreeMap<String, BTreeMap<String, WorkbookProjectionRow>>,
}

impl WorkbookProjectionBuilder {
    pub(crate) fn new(source: WorkbookProjectionSource) -> Result<Self, WorkbookProjectionError> {
        let index = projection_registry_index()?;
        let rows = index
            .sheet_fields
            .keys()
            .cloned()
            .map(|sheet_key| (sheet_key, BTreeMap::new()))
            .collect();
        Ok(Self {
            source,
            index,
            rows,
        })
    }

    pub(crate) fn push_row(
        &mut self,
        sheet_key: &str,
        object_ref: impl Into<String>,
        values: impl IntoIterator<Item = (String, WorkbookProjectionValue)>,
    ) -> Result<(), WorkbookProjectionError> {
        let object_ref = object_ref.into();
        let fields = self.index.sheet_fields.get(sheet_key).ok_or_else(|| {
            WorkbookProjectionError::UnknownSheet {
                sheet_key: sheet_key.to_owned(),
                object_ref: object_ref.clone(),
            }
        })?;
        let row = build_projection_row(
            sheet_key,
            object_ref.clone(),
            values,
            fields,
            &self.index.enum_options,
        )?;
        let rows = self
            .rows
            .get_mut(sheet_key)
            .expect("构造器已经为每张注册表建立行集合");
        if rows.contains_key(&object_ref) {
            return Err(WorkbookProjectionError::DuplicateRowReference {
                sheet_key: sheet_key.to_owned(),
                object_ref,
            });
        }
        ensure_row_capacity(sheet_key, &object_ref, rows.len())?;
        rows.insert(object_ref, row);
        Ok(())
    }

    pub(crate) fn directly_composable_equipment_configs(&self) -> BTreeSet<String> {
        directly_composable_equipment_configs(
            self.rows
                .get("resource_recipes")
                .into_iter()
                .flat_map(|rows| rows.values()),
        )
    }

    pub(crate) fn finish(self) -> Result<WorkbookProjectionV4, WorkbookProjectionError> {
        let Self {
            source,
            index,
            mut rows,
        } = self;
        let registry_sha256 = index.registry_sha256.clone();
        let sheet_fields = &index.sheet_fields;
        let composable = directly_composable_equipment_configs(
            rows.get("resource_recipes")
                .into_iter()
                .flat_map(|rows| rows.values()),
        );
        let mut sheets: Vec<WorkbookProjectionSheet> = sheet_fields
            .iter()
            .map(|(stable_key, fields)| WorkbookProjectionSheet {
                rows: rows
                    .remove(stable_key)
                    .expect("构造器已经为每张注册表建立行集合")
                    .into_values()
                    .collect(),
                stable_key: stable_key.clone(),
                field_keys: fields.keys().cloned().collect(),
            })
            .collect();
        if let Some(sheet) = sheets
            .iter_mut()
            .find(|sheet| sheet.stable_key == "equipment_inventory")
        {
            // 稳定排序保留每组原有的业务对象引用顺序；完全没有同类持有的未持有行在最后。
            sheet
                .rows
                .sort_by_key(|row| row.equipment_inventory_sort_group(&composable));
        }
        let schema_version = WORKBOOK_PROJECTION_SCHEMA_VERSION;
        let digest_input = WorkbookProjectionDigest {
            schema_version,
            source: &source,
            registry_sha256: &registry_sha256,
            sheets: &sheets,
        };
        let content_sha256 = sha256_compact_json(&digest_input)
            .map_err(|source| WorkbookProjectionError::Encode { source })?;
        Ok(WorkbookProjectionV4 {
            schema_version,
            source,
            registry_sha256,
            sheets,
            content_sha256,
        })
    }
}

/// 装备总表行分组：已持有、可直接合成未持有、同类已有持有的未持有、完全没有同类持有的未持有。
pub(crate) fn equipment_inventory_row_group(
    unowned: bool,
    directly_composable: bool,
    has_family_owned_enhance_distribution: bool,
) -> u8 {
    if !unowned {
        0
    } else if directly_composable {
        1
    } else if has_family_owned_enhance_distribution {
        2
    } else {
        3
    }
}

fn directly_composable_equipment_configs<'a>(
    rows: impl IntoIterator<Item = &'a WorkbookProjectionRow>,
) -> BTreeSet<String> {
    rows.into_iter().filter_map(|row| {
        let is_text = |key, expected| matches!(row.value(key), Some(WorkbookProjectionValue::Text(value)) if value == expected);
        if is_text("recipe_type", "compose")
            && is_text("resource_type", "equipment")
            && matches!(row.value("result_quantity"), Some(WorkbookProjectionValue::Integer(value)) if *value > 0)
            && matches!(row.value("maximum_craftable"), Some(WorkbookProjectionValue::Integer(value)) if *value > 0)
            && let Some(WorkbookProjectionValue::Text(config_id)) = row.value("equipment_config_id")
        {
            return Some(config_id.clone());
        }
        None
    }).collect()
}

fn build_projection_row(
    sheet_key: &str,
    object_ref: String,
    values: impl IntoIterator<Item = (String, WorkbookProjectionValue)>,
    fields: &BTreeMap<String, RegisteredLayoutField>,
    enum_options: &BTreeSet<(String, String)>,
) -> Result<WorkbookProjectionRow, WorkbookProjectionError> {
    let mut row_values = BTreeMap::new();
    for (field_key, value) in values {
        if row_values.insert(field_key.clone(), value).is_some() {
            return Err(WorkbookProjectionError::DuplicateField {
                sheet_key: sheet_key.to_owned(),
                object_ref,
                field_key,
            });
        }
    }
    if let Some(field_key) = row_values.keys().find(|key| !fields.contains_key(*key)) {
        return Err(WorkbookProjectionError::UnknownField {
            sheet_key: sheet_key.to_owned(),
            object_ref,
            field_key: field_key.clone(),
        });
    }
    let missing: Vec<String> = fields
        .keys()
        .filter(|key| !row_values.contains_key(*key))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(WorkbookProjectionError::MissingFields {
            sheet_key: sheet_key.to_owned(),
            object_ref,
            field_keys: missing,
        });
    }
    for (field_key, value) in &row_values {
        let field = fields.get(field_key).expect("上方已经拒绝未登记字段");
        validate_projection_value(sheet_key, &object_ref, field, value, enum_options)?;
    }
    Ok(WorkbookProjectionRow {
        object_ref,
        values: row_values,
    })
}

#[derive(Serialize)]
struct WorkbookProjectionDigest<'a> {
    schema_version: u32,
    source: &'a WorkbookProjectionSource,
    registry_sha256: &'a str,
    sheets: &'a [WorkbookProjectionSheet],
}

#[derive(Serialize)]
struct ProjectionRegistryDigest<'a> {
    schema_version: u32,
    sheets: Vec<ProjectionRegistrySheetDigest<'a>>,
    fields: Vec<ProjectionRegistryFieldDigest<'a>>,
    enum_options: Vec<ProjectionRegistryEnumDigest<'a>>,
    style_keys: &'a [String],
}

#[derive(Serialize)]
struct ProjectionRegistrySheetDigest<'a> {
    stable_key: &'a str,
    required: bool,
}

#[derive(Serialize)]
struct ProjectionRegistryFieldDigest<'a> {
    sheet_key: &'a str,
    stable_key: &'a str,
    model_path: &'a str,
    allowed_formats: &'a BTreeSet<LayoutValueFormat>,
    editor: LayoutEditor,
    required: bool,
    enum_category: Option<&'a str>,
}

#[derive(Serialize)]
struct ProjectionRegistryEnumDigest<'a> {
    category_key: &'a str,
    stable_value: &'a str,
}

fn projection_registry_sha256(
    registry: &WorkbookLayoutRegistry,
) -> Result<String, WorkbookProjectionError> {
    let digest = ProjectionRegistryDigest {
        schema_version: WORKBOOK_PROJECTION_SCHEMA_VERSION,
        sheets: registry
            .sheets()
            .iter()
            .map(|sheet| ProjectionRegistrySheetDigest {
                stable_key: sheet.stable_key(),
                required: sheet.required(),
            })
            .collect(),
        fields: registry
            .fields()
            .iter()
            .map(|field| ProjectionRegistryFieldDigest {
                sheet_key: field.sheet_key(),
                stable_key: field.stable_key(),
                model_path: field.model_path(),
                allowed_formats: field.allowed_formats(),
                editor: field.editor(),
                required: field.required(),
                enum_category: field.enum_category(),
            })
            .collect(),
        enum_options: registry
            .enum_options()
            .iter()
            .map(|option| ProjectionRegistryEnumDigest {
                category_key: option.category_key(),
                stable_value: option.stable_value(),
            })
            .collect(),
        style_keys: registry.style_keys(),
    };
    sha256_compact_json(&digest)
        .map_err(|source| WorkbookProjectionError::RegistryEncode { source })
}

fn validate_projection_value(
    sheet_key: &str,
    object_ref: &str,
    field: &RegisteredLayoutField,
    value: &WorkbookProjectionValue,
    enum_options: &BTreeSet<(String, String)>,
) -> Result<(), WorkbookProjectionError> {
    if matches!(value, WorkbookProjectionValue::Blank) {
        return Ok(());
    }
    if let Some(text) = projection_text(value) {
        let actual_utf16_units = text.encode_utf16().count();
        if actual_utf16_units > MAX_EXCEL_CELL_UTF16_UNITS {
            return Err(WorkbookProjectionError::CellTextLimitExceeded {
                sheet_key: sheet_key.to_owned(),
                object_ref: object_ref.to_owned(),
                field_key: field.stable_key().to_owned(),
                maximum_utf16_units: MAX_EXCEL_CELL_UTF16_UNITS,
                actual_utf16_units,
            });
        }
    }
    if matches!(value, WorkbookProjectionValue::Decimal(value) if !value.is_finite()) {
        return Err(WorkbookProjectionError::NonFiniteDecimal {
            sheet_key: sheet_key.to_owned(),
            object_ref: object_ref.to_owned(),
            field_key: field.stable_key().to_owned(),
        });
    }
    if let WorkbookProjectionValue::Integer(value) = value
        && !(-MAX_EXCEL_EXACT_INTEGER..=MAX_EXCEL_EXACT_INTEGER).contains(value)
    {
        return Err(WorkbookProjectionError::InexactExcelInteger {
            sheet_key: sheet_key.to_owned(),
            object_ref: object_ref.to_owned(),
            field_key: field.stable_key().to_owned(),
            value: *value,
            minimum: -MAX_EXCEL_EXACT_INTEGER,
            maximum: MAX_EXCEL_EXACT_INTEGER,
        });
    }
    if let WorkbookProjectionValue::DateTimeUnixMillis(value) = value
        && !(MIN_EXCEL_UNIX_MILLIS..MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE).contains(value)
    {
        return Err(WorkbookProjectionError::DateTimeOutOfRange {
            sheet_key: sheet_key.to_owned(),
            object_ref: object_ref.to_owned(),
            field_key: field.stable_key().to_owned(),
            value: *value,
            minimum: MIN_EXCEL_UNIX_MILLIS,
            maximum_exclusive: MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE,
        });
    }
    if let WorkbookProjectionValue::Enumeration {
        category_key,
        stable_value,
    } = value
    {
        if !field.allowed_formats().contains(&LayoutValueFormat::Text) {
            return Err(WorkbookProjectionError::ValueTypeMismatch {
                sheet_key: sheet_key.to_owned(),
                object_ref: object_ref.to_owned(),
                field_key: field.stable_key().to_owned(),
                value_kind: projection_value_kind(value),
            });
        }
        if field.enum_category() != Some(category_key.as_str())
            || !enum_options.contains(&(category_key.clone(), stable_value.clone()))
        {
            return Err(WorkbookProjectionError::UnknownEnumeration {
                sheet_key: sheet_key.to_owned(),
                object_ref: object_ref.to_owned(),
                field_key: field.stable_key().to_owned(),
                category_key: category_key.clone(),
                stable_value: stable_value.clone(),
            });
        }
        return Ok(());
    }
    let accepted = field.allowed_formats().iter().any(|format| {
        matches!(
            (format, value),
            (
                LayoutValueFormat::Text,
                WorkbookProjectionValue::Text(_) | WorkbookProjectionValue::Boolean(_)
            ) | (
                LayoutValueFormat::Integer,
                WorkbookProjectionValue::Integer(_)
            ) | (
                LayoutValueFormat::Decimal | LayoutValueFormat::Percentage,
                WorkbookProjectionValue::Decimal(_)
            ) | (
                LayoutValueFormat::DateTime,
                WorkbookProjectionValue::DateTimeUnixMillis(_)
            ) | (LayoutValueFormat::Json, WorkbookProjectionValue::Json(_))
        )
    });
    if accepted {
        Ok(())
    } else {
        Err(WorkbookProjectionError::ValueTypeMismatch {
            sheet_key: sheet_key.to_owned(),
            object_ref: object_ref.to_owned(),
            field_key: field.stable_key().to_owned(),
            value_kind: projection_value_kind(value),
        })
    }
}

fn projection_text(value: &WorkbookProjectionValue) -> Option<&str> {
    match value {
        WorkbookProjectionValue::Text(value) | WorkbookProjectionValue::Json(value) => Some(value),
        WorkbookProjectionValue::Enumeration { stable_value, .. } => Some(stable_value),
        WorkbookProjectionValue::Blank
        | WorkbookProjectionValue::Integer(_)
        | WorkbookProjectionValue::Decimal(_)
        | WorkbookProjectionValue::DateTimeUnixMillis(_)
        | WorkbookProjectionValue::Boolean(_) => None,
    }
}

fn ensure_row_capacity(
    sheet_key: &str,
    object_ref: &str,
    current_rows: usize,
) -> Result<(), WorkbookProjectionError> {
    let actual_rows =
        current_rows
            .checked_add(1)
            .ok_or_else(|| WorkbookProjectionError::RowLimitExceeded {
                sheet_key: sheet_key.to_owned(),
                object_ref: object_ref.to_owned(),
                maximum_rows: MAX_EXCEL_DATA_ROWS,
                actual_rows: usize::MAX,
            })?;
    if actual_rows > MAX_EXCEL_DATA_ROWS {
        Err(WorkbookProjectionError::RowLimitExceeded {
            sheet_key: sheet_key.to_owned(),
            object_ref: object_ref.to_owned(),
            maximum_rows: MAX_EXCEL_DATA_ROWS,
            actual_rows,
        })
    } else {
        Ok(())
    }
}

const fn projection_value_kind(value: &WorkbookProjectionValue) -> &'static str {
    match value {
        WorkbookProjectionValue::Blank => "blank",
        WorkbookProjectionValue::Text(_) => "text",
        WorkbookProjectionValue::Integer(_) => "integer",
        WorkbookProjectionValue::Decimal(_) => "decimal",
        WorkbookProjectionValue::DateTimeUnixMillis(_) => "date_time",
        WorkbookProjectionValue::Boolean(_) => "boolean",
        WorkbookProjectionValue::Enumeration { .. } => "enumeration",
        WorkbookProjectionValue::Json(_) => "json",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        MAX_EXCEL_CELL_UTF16_UNITS, MAX_EXCEL_DATA_ROWS, MAX_EXCEL_EXACT_INTEGER,
        MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE, MIN_EXCEL_UNIX_MILLIS, WORKBOOK_PROJECTION_SCHEMA_VERSION,
        WorkbookLayoutRegistry, WorkbookProjectionBuilder, WorkbookProjectionError,
        WorkbookProjectionRow, WorkbookProjectionSource, WorkbookProjectionV4,
        WorkbookProjectionValue, ensure_row_capacity, projection_registry_sha256,
        validate_projection_value,
    };
    use crate::application::{LayoutEditor, RegisteredLayoutEnumOption};

    #[test]
    fn projection_contract_registers_every_output_sheet_and_explicit_field() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        assert_eq!(WORKBOOK_PROJECTION_SCHEMA_VERSION, 19);
        assert_eq!(registry.sheets().len(), 10);
        assert_eq!(registry.fields().len(), 403);
        assert_eq!(registry.enum_options().len(), 55);
        assert_eq!(registry.style_keys().len(), 6);

        let mut fields_per_sheet: BTreeMap<&str, usize> = BTreeMap::new();
        for field in registry.fields() {
            *fields_per_sheet.entry(field.sheet_key()).or_default() += 1;
            assert_eq!(
                field.model_path(),
                format!(
                    "WorkbookProjectionV4.{}[].{}",
                    field.sheet_key(),
                    field.stable_key()
                )
            );
        }
        assert_eq!(
            fields_per_sheet,
            BTreeMap::from([
                ("check_results", 27),
                ("dictionaries", 7),
                ("equipment_inventory", 72),
                ("execution_results", 33),
                ("loadout_plan", 177),
                ("plan_data", 16),
                ("raw_data", 9),
                ("resource_recipes", 13),
                ("schema", 32),
                ("ship_technology", 17),
            ])
        );
    }

    #[test]
    fn execution_result_contract_keeps_report_and_step_statuses_machine_readable() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        for (field_key, enum_category) in [
            ("report_status", "execution_report_status"),
            ("stop_reason", "execution_stop_reason"),
            ("status", "execution_status"),
            ("write_effect", "execution_write_effect"),
            (
                "final_verification_status",
                "execution_final_verification_status",
            ),
        ] {
            let field = registry.field("execution_results", field_key).unwrap();
            assert_eq!(field.editor(), LayoutEditor::ReadOnly);
            assert_eq!(field.enum_category(), Some(enum_category));
            assert!(field.required());
        }
        for field_key in [
            "target_fingerprint_sha256",
            "report_hash",
            "may_have_writes",
            "write_acknowledged",
            "acknowledged_write_count",
            "observed_state_change_count",
            "verified_write_count",
        ] {
            assert!(
                registry
                    .field("execution_results", field_key)
                    .unwrap()
                    .required()
            );
        }
        let registered_fields = WorkbookProjectionV4::registered_fields();
        let execution_fields: Vec<&str> = registered_fields
            .iter()
            .filter(|field| field.sheet_key() == "execution_results")
            .map(|field| field.stable_key())
            .collect();
        assert_eq!(
            &execution_fields[23..],
            [
                "report_status",
                "stop_reason",
                "target_fingerprint_sha256",
                "report_hash",
                "may_have_writes",
                "write_acknowledged",
                "write_effect",
                "acknowledged_write_count",
                "observed_state_change_count",
                "verified_write_count",
            ]
        );
    }

    #[test]
    fn projection_contract_expands_fixed_ship_attributes_and_slots() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        for attribute in [
            "durability",
            "cannon",
            "torpedo",
            "air",
            "reload",
            "anti_aircraft",
            "hit",
            "dodge",
            "anti_sub",
            "luck",
            "speed",
        ] {
            for stage in ["base", "equipment_delta", "global_delta", "final"] {
                assert!(
                    registry
                        .field("loadout_plan", &format!("stat_{attribute}_{stage}"))
                        .is_some()
                );
            }
        }
        for slot in 1..=5 {
            assert!(
                registry
                    .field("loadout_plan", &format!("slot_{slot}_type"))
                    .is_none()
            );
            for suffix in [
                "allowed_equipment_types",
                "equipment_name",
                "runtime_id",
                "config_id",
                "family_id",
                "enhance_level",
                "effect_summary",
                "target_equipment_family",
                "source_policy",
                "exact_source",
                "target_enhance_level",
                "allocation_priority",
                "note",
            ] {
                assert!(
                    registry
                        .field("loadout_plan", &format!("slot_{slot}_{suffix}"))
                        .is_some()
                );
            }
        }
        for field in [
            "skills_effective_skill_id",
            "skills_name",
            "skills_description",
            "skills_current_effect",
            "skills_level",
            "skills_maximum_level",
            "skills_experience",
            "skills_next_level_experience",
            "skills_effect_parameters",
            "skills_raw_structure",
            "skills_data_complete",
            "skills_read_errors",
        ] {
            assert!(registry.field("loadout_plan", field).is_some());
        }
        assert!(
            !registry
                .field("loadout_plan", "instance_id")
                .unwrap()
                .required()
        );
        for key in ["skill_summary", "skills_skill_id"] {
            assert!(registry.field("loadout_plan", key).is_none());
        }
        for key in [
            "attribute_summary",
            "weapon_summary",
            "main_skill_summary",
            "hidden_skill_summary",
        ] {
            assert!(registry.field("equipment_inventory", key).is_none());
        }
        assert!(
            registry
                .field("equipment_inventory", "effect_summary")
                .is_some()
        );
        assert!(registry.sheet("ships").is_none());
        assert!(registry.sheet("ship_skills").is_none());
    }

    #[test]
    fn projection_contract_registers_editable_text_and_plan_columns() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let actual: std::collections::BTreeSet<(String, String)> = registry
            .fields()
            .iter()
            .filter(|field| field.editor() != LayoutEditor::ReadOnly)
            .map(|field| (field.sheet_key().to_owned(), field.stable_key().to_owned()))
            .collect();
        let mut expected = std::collections::BTreeSet::from(
            [
                ("equipment_inventory", "operation"),
                ("equipment_inventory", "processing_quantity"),
                ("equipment_inventory", "target_enhance_level"),
                ("equipment_inventory", "note"),
                ("loadout_plan", "technology_bonus"),
            ]
            .map(|(sheet, field)| (sheet.to_owned(), field.to_owned())),
        );
        for slot in 1..=5 {
            for suffix in [
                "target_equipment_family",
                "source_policy",
                "exact_source",
                "target_enhance_level",
                "allocation_priority",
                "note",
            ] {
                expected.insert(("loadout_plan".to_owned(), format!("slot_{slot}_{suffix}")));
            }
        }

        assert_eq!(actual, expected);
    }

    #[test]
    fn projection_rejects_text_beyond_the_excel_cell_limit() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let field = registry.field("loadout_plan", "name").unwrap();
        let enum_options = registry
            .enum_options()
            .iter()
            .map(|option| {
                (
                    option.category_key().to_owned(),
                    option.stable_value().to_owned(),
                )
            })
            .collect();
        let value = WorkbookProjectionValue::text("𐐷".repeat(MAX_EXCEL_CELL_UTF16_UNITS / 2 + 1));

        let error =
            validate_projection_value("loadout_plan", "ship:9001", field, &value, &enum_options)
                .unwrap_err();

        assert!(matches!(
            error,
            WorkbookProjectionError::CellTextLimitExceeded {
                actual_utf16_units,
                maximum_utf16_units: MAX_EXCEL_CELL_UTF16_UNITS,
                ..
            } if actual_utf16_units == MAX_EXCEL_CELL_UTF16_UNITS + 1
        ));
    }

    #[test]
    fn projection_rejects_non_finite_decimals() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let field = registry
            .field("equipment_inventory", "anti_siren_power")
            .unwrap();

        let error = validate_projection_value(
            "equipment_inventory",
            "unowned:1000",
            field,
            &WorkbookProjectionValue::Decimal(f64::NAN),
            &BTreeSet::new(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            WorkbookProjectionError::NonFiniteDecimal {
                sheet_key,
                field_key,
                ..
            } if sheet_key == "equipment_inventory" && field_key == "anti_siren_power"
        ));
    }

    #[test]
    fn projection_rejects_integers_that_excel_cannot_represent_exactly() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let field = registry.field("equipment_inventory", "quantity").unwrap();
        let expected_minimum = -MAX_EXCEL_EXACT_INTEGER;
        let expected_maximum = MAX_EXCEL_EXACT_INTEGER;

        for value in [expected_minimum, expected_maximum] {
            validate_projection_value(
                "equipment_inventory",
                "warehouse:1000",
                field,
                &WorkbookProjectionValue::Integer(value),
                &BTreeSet::new(),
            )
            .unwrap();
        }

        for value in [expected_minimum - 1, expected_maximum + 1] {
            let error = validate_projection_value(
                "equipment_inventory",
                "warehouse:1000",
                field,
                &WorkbookProjectionValue::Integer(value),
                &BTreeSet::new(),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                WorkbookProjectionError::InexactExcelInteger {
                    value: actual,
                    minimum,
                    maximum,
                    ..
                } if actual == value
                    && minimum == expected_minimum
                    && maximum == expected_maximum
            ));
        }
    }

    #[test]
    fn projection_accepts_unix_milliseconds_for_datetime_fields() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let field = registry.field("check_results", "checked_at").unwrap();

        for value in [
            MIN_EXCEL_UNIX_MILLIS,
            946_684_800_000,
            MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE - 1,
        ] {
            validate_projection_value(
                "check_results",
                "check:1",
                field,
                &WorkbookProjectionValue::date_time_unix_millis(value),
                &BTreeSet::new(),
            )
            .unwrap();
        }
    }

    #[test]
    fn projection_rejects_datetime_outside_the_excel_range() {
        let registry = WorkbookProjectionV4::layout_registry().unwrap();
        let field = registry.field("check_results", "checked_at").unwrap();

        for value in [MIN_EXCEL_UNIX_MILLIS - 1, MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE] {
            let error = validate_projection_value(
                "check_results",
                "check:1",
                field,
                &WorkbookProjectionValue::date_time_unix_millis(value),
                &BTreeSet::new(),
            )
            .unwrap_err();
            assert!(matches!(
                error,
                WorkbookProjectionError::DateTimeOutOfRange {
                    value: actual,
                    minimum: MIN_EXCEL_UNIX_MILLIS,
                    maximum_exclusive: MAX_EXCEL_UNIX_MILLIS_EXCLUSIVE,
                    ..
                } if actual == value
            ));
        }
    }

    #[test]
    fn projection_sorts_rows_by_reference_and_rejects_duplicates() {
        let forward = projection_with_raw_rows(&["record:z", "record:a"]);
        let reverse = projection_with_raw_rows(&["record:a", "record:z"]);

        assert_eq!(forward, reverse);
        assert_eq!(
            forward.registry_sha256(),
            projection_registry_sha256(&WorkbookProjectionV4::layout_registry().unwrap()).unwrap()
        );
        let serialized = serde_json::to_value(&forward).unwrap();
        assert_eq!(
            serialized["schema_version"],
            WORKBOOK_PROJECTION_SCHEMA_VERSION
        );
        assert_eq!(serialized["registry_sha256"], forward.registry_sha256());
        assert_eq!(serialized["content_sha256"], forward.content_sha256());
        let rows = forward.sheet("raw_data").unwrap().rows();
        assert_eq!(
            rows.iter()
                .map(WorkbookProjectionRow::object_ref)
                .collect::<Vec<_>>(),
            vec!["record:a", "record:z"]
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.value("source_ref").unwrap())
                .collect::<Vec<_>>(),
            vec![
                &WorkbookProjectionValue::text("record:a"),
                &WorkbookProjectionValue::text("record:z"),
            ]
        );

        let mut builder = WorkbookProjectionBuilder::new(test_projection_source()).unwrap();
        builder
            .push_row("raw_data", "record:a", raw_data_row("record:a"))
            .unwrap();
        let error = builder
            .push_row("raw_data", "record:a", raw_data_row("record:a"))
            .unwrap_err();
        assert!(matches!(
            error,
            WorkbookProjectionError::DuplicateRowReference {
                sheet_key,
                object_ref,
            } if sheet_key == "raw_data" && object_ref == "record:a"
        ));
    }

    #[test]
    fn projection_registry_digest_covers_registered_enum_options() {
        let baseline = WorkbookProjectionV4::layout_registry().unwrap();
        let baseline_sha256 = projection_registry_sha256(&baseline).unwrap();
        let mut options = WorkbookProjectionV4::registered_enum_options();
        options.push(RegisteredLayoutEnumOption::new("check_status", "skipped"));
        let changed = WorkbookLayoutRegistry::new(
            WorkbookProjectionV4::registered_sheets(),
            WorkbookProjectionV4::registered_fields(),
            options,
            WorkbookProjectionV4::registered_style_keys(),
        )
        .unwrap();

        assert_ne!(
            baseline_sha256,
            projection_registry_sha256(&changed).unwrap()
        );
    }

    #[test]
    fn replaced_rows_move_unchanged_sheets_and_revalidate_replacements() {
        let mut builder = WorkbookProjectionBuilder::new(test_projection_source()).unwrap();
        builder
            .push_row("raw_data", "record:b", raw_data_row("record:b"))
            .unwrap();
        builder
            .push_row("raw_data", "record:a", raw_data_row("record:a"))
            .unwrap();
        let plan_values: Vec<_> = WorkbookProjectionV4::registered_fields()
            .into_iter()
            .filter(|field| field.sheet_key() == "plan_data")
            .map(|field| {
                (
                    field.stable_key().to_owned(),
                    WorkbookProjectionValue::Blank,
                )
            })
            .collect();
        builder
            .push_row("plan_data", "plan:1", plan_values.clone())
            .unwrap();
        let projection = builder.finish().unwrap();
        let stable = projection.sheet("plan_data").unwrap().rows()[0]
            .object_ref()
            .as_ptr();
        let digest = projection.content_sha256().to_owned();

        let unknown = projection
            .clone()
            .with_replaced_rows(BTreeMap::from([("missing_sheet".to_owned(), Vec::new())]))
            .unwrap_err();
        assert!(matches!(
            unknown,
            WorkbookProjectionError::UnknownSheet { ref sheet_key, .. } if sheet_key == "missing_sheet"
        ));

        let mut invalid = raw_data_row("record:a");
        invalid.push(("not_a_field".to_owned(), WorkbookProjectionValue::text("x")));
        let illegal = projection
            .clone()
            .with_replaced_rows(BTreeMap::from([(
                "raw_data".to_owned(),
                vec![("record:a".to_owned(), invalid.into_iter().collect())],
            )]))
            .unwrap_err();
        assert!(matches!(
            illegal,
            WorkbookProjectionError::UnknownField { .. }
        ));

        let replaced = projection
            .with_replaced_rows(BTreeMap::from([(
                "raw_data".to_owned(),
                vec![
                    (
                        "record:b".to_owned(),
                        raw_data_row("record:b").into_iter().collect(),
                    ),
                    (
                        "record:a".to_owned(),
                        raw_data_row("record:a").into_iter().collect(),
                    ),
                ],
            )]))
            .unwrap();
        assert_eq!(
            replaced.sheet("plan_data").unwrap().rows()[0]
                .object_ref()
                .as_ptr(),
            stable
        );
        assert_eq!(
            replaced
                .sheet("raw_data")
                .unwrap()
                .rows()
                .iter()
                .map(WorkbookProjectionRow::object_ref)
                .collect::<Vec<_>>(),
            vec!["record:a", "record:b"]
        );
        assert_eq!(replaced.content_sha256(), digest);

        let mut changed = raw_data_row("record:a");
        let source_ref = changed
            .iter_mut()
            .find(|(key, _)| key == "source_ref")
            .unwrap();
        source_ref.1 = WorkbookProjectionValue::text("changed");
        let changed_row = changed;
        let updated = replaced
            .with_replaced_rows(BTreeMap::from([(
                "raw_data".to_owned(),
                vec![
                    (
                        "record:b".to_owned(),
                        raw_data_row("record:b").into_iter().collect(),
                    ),
                    (
                        "record:a".to_owned(),
                        changed_row.clone().into_iter().collect(),
                    ),
                ],
            )]))
            .unwrap();
        assert_eq!(
            updated.sheet("plan_data").unwrap().rows()[0]
                .object_ref()
                .as_ptr(),
            stable
        );
        assert_ne!(updated.content_sha256(), digest);
        let mut expected = WorkbookProjectionBuilder::new(test_projection_source()).unwrap();
        expected
            .push_row("raw_data", "record:b", raw_data_row("record:b"))
            .unwrap();
        expected
            .push_row("raw_data", "record:a", changed_row)
            .unwrap();
        expected
            .push_row("plan_data", "plan:1", plan_values)
            .unwrap();
        assert_eq!(
            updated.content_sha256(),
            expected.finish().unwrap().content_sha256()
        );
    }

    #[test]
    fn projection_reserves_one_excel_row_for_the_header() {
        assert!(ensure_row_capacity("raw_data", "chunk:1", MAX_EXCEL_DATA_ROWS - 1).is_ok());

        let error = ensure_row_capacity("raw_data", "chunk:last", MAX_EXCEL_DATA_ROWS).unwrap_err();

        assert!(matches!(
            error,
            WorkbookProjectionError::RowLimitExceeded {
                maximum_rows: MAX_EXCEL_DATA_ROWS,
                actual_rows,
                ..
            } if actual_rows == MAX_EXCEL_DATA_ROWS + 1
        ));
    }

    fn projection_with_raw_rows(object_refs: &[&str]) -> WorkbookProjectionV4 {
        let mut builder = WorkbookProjectionBuilder::new(test_projection_source()).unwrap();
        for object_ref in object_refs {
            builder
                .push_row("raw_data", *object_ref, raw_data_row(object_ref))
                .unwrap();
        }
        builder.finish().unwrap()
    }

    fn raw_data_row(source_ref: &str) -> Vec<(String, WorkbookProjectionValue)> {
        WorkbookProjectionV4::registered_fields()
            .into_iter()
            .filter(|field| field.sheet_key() == "raw_data")
            .map(|field| {
                let value = if field.stable_key() == "source_ref" {
                    WorkbookProjectionValue::text(source_ref)
                } else {
                    WorkbookProjectionValue::Blank
                };
                (field.stable_key().to_owned(), value)
            })
            .collect()
    }

    fn test_projection_source() -> WorkbookProjectionSource {
        let digest = "0".repeat(64);
        WorkbookProjectionSource::new(
            1,
            digest.clone(),
            1,
            1,
            1,
            1,
            1,
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest,
        )
    }
}

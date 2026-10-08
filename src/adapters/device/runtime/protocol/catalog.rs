//! 定义固定白名单舰船静态配置表的分页 RPC 模型与完整性校验。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    EquipmentConfigSource, MAX_SHIP_CATALOG_ITEMS, MAX_SHIP_CATALOG_PAGE_SIZE,
    RuntimeProtocolError, deserialize_required_nullable, validate_lua_json_value,
    validate_next_catalog_index, validate_non_empty, validate_positive_lua_integer,
    validate_stable_token,
};

/// 当前静态舰船目录允许读取的 `pg` 表；序列化值直接对应客户端表名。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipCatalogTableKey {
    ShipDataGroup,
    ShipDataTemplate,
    ShipDataStatistics,
    ShipDataBreakout,
    ShipDataTrans,
    TransformDataTemplate,
    ShipTransform,
    ShipDataStrengthen,
    ShipStrengthenBlueprint,
    ShipDataBlueprint,
    ShipStrengthenMeta,
    ShipMetaBreakout,
    ShipMetaRepairEffect,
    ShipMetaRepair,
    SkillDataTemplate,
    SkillDataDisplay,
    SkillNeedExp,
    FleetTechShipTemplate,
    ShipDataByType,
    AttributeInfoByType,
    CollectionShipGroup,
}

impl ShipCatalogTableKey {
    /// 固定顺序同时作为整套读取范围和捕获文件表顺序的唯一来源。
    pub const ALL: [Self; 17] = [
        Self::ShipDataGroup,
        Self::ShipDataTemplate,
        Self::ShipDataStatistics,
        Self::ShipDataBreakout,
        Self::ShipDataTrans,
        Self::TransformDataTemplate,
        Self::ShipTransform,
        Self::ShipDataStrengthen,
        Self::ShipStrengthenBlueprint,
        Self::ShipDataBlueprint,
        Self::ShipStrengthenMeta,
        Self::ShipMetaBreakout,
        Self::ShipMetaRepairEffect,
        Self::ShipMetaRepair,
        Self::SkillDataTemplate,
        Self::SkillDataDisplay,
        Self::SkillNeedExp,
    ];

    /// 按模板请求追加的科技配置与账号图鉴历史。
    pub const TECHNOLOGY: [Self; 4] = [
        Self::FleetTechShipTemplate,
        Self::ShipDataByType,
        Self::AttributeInfoByType,
        Self::CollectionShipGroup,
    ];

    /// 返回 RPC、日志和捕获文件共用的稳定表键。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ShipDataGroup => "ship_data_group",
            Self::ShipDataTemplate => "ship_data_template",
            Self::ShipDataStatistics => "ship_data_statistics",
            Self::ShipDataBreakout => "ship_data_breakout",
            Self::ShipDataTrans => "ship_data_trans",
            Self::TransformDataTemplate => "transform_data_template",
            Self::ShipTransform => "ship_transform",
            Self::ShipDataStrengthen => "ship_data_strengthen",
            Self::ShipStrengthenBlueprint => "ship_strengthen_blueprint",
            Self::ShipDataBlueprint => "ship_data_blueprint",
            Self::ShipStrengthenMeta => "ship_strengthen_meta",
            Self::ShipMetaBreakout => "ship_meta_breakout",
            Self::ShipMetaRepairEffect => "ship_meta_repair_effect",
            Self::ShipMetaRepair => "ship_meta_repair",
            Self::SkillDataTemplate => "skill_data_template",
            Self::SkillDataDisplay => "skill_data_display",
            Self::SkillNeedExp => "skill_need_exp",
            Self::FleetTechShipTemplate => "fleet_tech_ship_template",
            Self::ShipDataByType => "ship_data_by_type",
            Self::AttributeInfoByType => "attribute_info_by_type",
            Self::CollectionShipGroup => "collection_ship_group",
        }
    }
}

/// 单张舰船配置表请求使用固定表键、零基起点和有界页容量。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShipCatalogPagePayload {
    table_key: ShipCatalogTableKey,
    start_index: u32,
    page_size: u32,
}

impl ShipCatalogPagePayload {
    pub(crate) fn new(
        table_key: ShipCatalogTableKey,
        start_index: u32,
        page_size: u32,
    ) -> Result<Self, RuntimeProtocolError> {
        if start_index > MAX_SHIP_CATALOG_ITEMS
            || !(1..=MAX_SHIP_CATALOG_PAGE_SIZE).contains(&page_size)
        {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_page_out_of_range",
                format!(
                    "舰船静态目录 start_index 只允许 0 至 {MAX_SHIP_CATALOG_ITEMS}，page_size 只允许 1 至 {MAX_SHIP_CATALOG_PAGE_SIZE}"
                ),
            ));
        }
        Ok(Self {
            table_key,
            start_index,
            page_size,
        })
    }
}

/// confNEO 代理按物理字段并集和 base 继承展开后的完整记录。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShipCatalogRecord {
    pub id: u64,
    pub raw: Value,
}

impl ShipCatalogRecord {
    fn validate(&self) -> Result<(), RuntimeProtocolError> {
        validate_positive_lua_integer("ship_catalog.record.id", self.id)?;
        validate_lua_json_value("ship_catalog.record.raw", &self.raw, 0)?;
        let Some(raw) = self.raw.as_object() else {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_record_invalid",
                "舰船静态目录 raw 必须是完整物化后的 JSON 对象",
            ));
        };
        if raw.is_empty() {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_record_empty",
                "舰船静态目录 raw 不得是空对象",
            ));
        }
        reject_incomplete_lua_marker(&self.raw)?;
        if let Some(raw_id) = raw.get("id")
            && raw_id.as_u64() != Some(self.id)
        {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_record_id_mismatch",
                format!("舰船静态目录记录 ID {} 与 raw.id {raw_id} 不一致", self.id),
            ));
        }
        Ok(())
    }
}

/// 完整目录不得接收原生读取器为截断对象或不支持值保留的诊断结构。
fn reject_incomplete_lua_marker(value: &Value) -> Result<(), RuntimeProtocolError> {
    match value {
        Value::Array(values) => {
            for child in values {
                reject_incomplete_lua_marker(child)?;
            }
        }
        Value::Object(values) => {
            let lua_type = values.get("lua_type").and_then(Value::as_str);
            let diagnostic_marker = match lua_type {
                Some("unsupported") => values.len() == 2 && values.contains_key("type"),
                Some("object" | "table") => {
                    values.len() == 4
                        && values.contains_key("entries")
                        && values.contains_key("reason")
                        && values.get("truncated").and_then(Value::as_bool) == Some(true)
                }
                _ => false,
            };
            if diagnostic_marker {
                return Err(RuntimeProtocolError::new(
                    "ship_catalog_record_incomplete",
                    "舰船静态目录 raw 包含截断或不支持的 Lua 值",
                ));
            }
            for child in values.values() {
                reject_incomplete_lua_marker(child)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

/// 单个目录位置物化失败时的稳定定位信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShipCatalogPageReadError {
    pub catalog_index: u32,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub id: Option<u64>,
    pub code: String,
    pub message: String,
}

impl ShipCatalogPageReadError {
    fn validate(&self, start_index: u32, end_index: u32) -> Result<(), RuntimeProtocolError> {
        if self.catalog_index < start_index || self.catalog_index >= end_index {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_error_index_invalid",
                "舰船静态目录错误位置不在当前页消费范围内",
            ));
        }
        if let Some(id) = self.id {
            validate_positive_lua_integer("ship_catalog.read_error.id", id)?;
        }
        validate_stable_token("ship_catalog.read_error.code", &self.code)?;
        validate_non_empty("ship_catalog.read_error.message", &self.message, 4096)
    }
}

/// `snapshot_ship_catalog` 返回的一页完整物化静态配置。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShipCatalogPageResult {
    pub table_key: ShipCatalogTableKey,
    pub source: EquipmentConfigSource,
    pub start_index: u32,
    pub total_count: u32,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub next_index: Option<u32>,
    pub records: Vec<ShipCatalogRecord>,
    pub read_errors: Vec<ShipCatalogPageReadError>,
    pub complete: bool,
}

impl ShipCatalogPageResult {
    /// 复核固定表身份、来源、精确游标、记录唯一性和完整性声明。
    pub(crate) fn validate(
        &self,
        requested_table: ShipCatalogTableKey,
        requested_start_index: u32,
        requested_page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<(), RuntimeProtocolError> {
        if self.table_key != requested_table {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_table_mismatch",
                format!(
                    "舰船静态目录表应为 {}，实际为 {}",
                    requested_table.as_str(),
                    self.table_key.as_str()
                ),
            ));
        }
        self.source.validate(expected_module_sha256)?;
        if self.start_index != requested_start_index
            || (self.total_count == 0
                && requested_table != ShipCatalogTableKey::CollectionShipGroup)
            || self.total_count > MAX_SHIP_CATALOG_ITEMS
            || self.start_index > self.total_count
        {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_cursor_invalid",
                "舰船静态目录页起点或总数不符合请求边界",
            ));
        }
        validate_next_catalog_index(
            "ship_catalog",
            self.start_index,
            self.total_count,
            requested_page_size,
            self.next_index,
        )?;
        let end_index = self.next_index.unwrap_or(self.total_count);
        let consumed =
            usize::try_from(end_index - self.start_index).expect("u32 页容量必须可表示为 usize");
        if self.records.len() + self.read_errors.len() != consumed {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_page_count_mismatch",
                "舰船静态目录 records 与 read_errors 必须逐项覆盖当前页",
            ));
        }
        let mut identifiers = BTreeSet::new();
        for record in &self.records {
            record.validate()?;
            if !identifiers.insert(record.id) {
                return Err(RuntimeProtocolError::new(
                    "ship_catalog_id_duplicate",
                    format!("舰船静态目录页内存在重复 ID {}", record.id),
                ));
            }
        }
        let mut error_indexes = BTreeSet::new();
        for error in &self.read_errors {
            error.validate(self.start_index, end_index)?;
            if !error_indexes.insert(error.catalog_index) {
                return Err(RuntimeProtocolError::new(
                    "ship_catalog_error_index_duplicate",
                    "舰船静态目录页内存在重复错误位置",
                ));
            }
        }
        if self.complete != self.read_errors.is_empty() {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_completeness_mismatch",
                "舰船静态目录 complete 必须与页级错误保持一致",
            ));
        }
        Ok(())
    }
}

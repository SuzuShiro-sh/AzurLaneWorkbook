//! 编排固定白名单舰船静态配置表的完整分页读取与稳定内容身份。

use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;

use super::collections::{PageState, read_restartable_pages};
use crate::adapters::device::runtime::{
    AgentClient, MAX_SHIP_CATALOG_PAGE_SIZE, RuntimeClientError, RuntimeProtocolError,
    ShipCatalogPageReadError, ShipCatalogPageResult, ShipCatalogRecord, ShipCatalogTableKey,
};
use suzushiro_content_digest::sha256_sorted_json;

pub(crate) const SHIP_CATALOG_SCHEMA_VERSION: u32 = 1;
const MAX_INCOMPLETE_PAGE_RETRIES: u8 = 2;

/// 一张固定白名单配置表按客户端 `all` 顺序展开后的完整记录。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShipCatalogTable {
    table_key: ShipCatalogTableKey,
    records: Vec<ShipCatalogRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_error: Option<String>,
}

impl ShipCatalogTable {
    #[cfg(test)]
    pub(crate) fn from_capture(
        table_key: ShipCatalogTableKey,
        records: Vec<ShipCatalogRecord>,
    ) -> Self {
        Self {
            table_key,
            records,
            read_error: None,
        }
    }
    /// 返回 RPC 与捕获文件共用的固定表键。
    pub const fn table_key(&self) -> ShipCatalogTableKey {
        self.table_key
    }

    pub fn read_error(&self) -> Option<&str> {
        self.read_error.as_deref()
    }
    /// 返回按客户端目录顺序排列的完整物化记录。
    pub fn records(&self) -> &[ShipCatalogRecord] {
        &self.records
    }
}

/// 当前目标模块内全部固定舰船静态表及其规范内容摘要。
#[derive(Clone, Debug, PartialEq)]
pub struct ShipCatalogReadResult {
    module_sha256: String,
    content_sha256: String,
    tables: Vec<ShipCatalogTable>,
}

impl ShipCatalogReadResult {
    #[cfg(test)]
    pub(crate) fn from_capture(
        module_sha256: String,
        content_sha256: String,
        tables: Vec<ShipCatalogTable>,
    ) -> Self {
        Self {
            module_sha256,
            content_sha256,
            tables,
        }
    }
    /// 返回 loader 已验证的目标模块 SHA-256。
    pub fn module_sha256(&self) -> &str {
        &self.module_sha256
    }

    /// 返回排除分页边界和模块二进制身份后的规范正文 SHA-256。
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    /// 返回严格按固定白名单顺序排列的全部配置表。
    pub fn tables(&self) -> &[ShipCatalogTable] {
        &self.tables
    }

    /// 去掉科技配置和账号图鉴，只保留已验证的静态白名单。
    pub(crate) fn static_tables_only(mut self) -> Result<Self, ShipCatalogReadError> {
        let had_account_tables = self
            .tables
            .iter()
            .any(|table| ShipCatalogTableKey::TECHNOLOGY.contains(&table.table_key()));
        self.tables
            .retain(|table| !ShipCatalogTableKey::TECHNOLOGY.contains(&table.table_key()));
        if self.tables.len() != ShipCatalogTableKey::ALL.len()
            || self
                .tables
                .iter()
                .zip(ShipCatalogTableKey::ALL)
                .any(|(table, key)| table.table_key() != key)
        {
            return Err(RuntimeProtocolError::new(
                "ship_catalog_cache_incomplete",
                "缓存舰船静态目录不完整",
            )
            .into());
        }
        if had_account_tables {
            let document = ShipCatalogDocument {
                schema_version: SHIP_CATALOG_SCHEMA_VERSION,
                tables: &self.tables,
            };
            self.content_sha256 =
                sha256_sorted_json(&document).map_err(ShipCatalogReadError::EncodeContentDigest)?;
        }
        Ok(self)
    }

    /// 返回全部表中的记录总数。
    pub fn record_count(&self) -> usize {
        self.tables.iter().map(|table| table.records.len()).sum()
    }

    pub(crate) fn document(&self) -> ShipCatalogDocument<'_> {
        ShipCatalogDocument {
            schema_version: SHIP_CATALOG_SCHEMA_VERSION,
            tables: &self.tables,
        }
    }
}

/// 内容摘要与外部捕获共用的稳定序列化投影。
#[derive(Serialize)]
pub(crate) struct ShipCatalogDocument<'a> {
    schema_version: u32,
    tables: &'a [ShipCatalogTable],
}

/// 在同一已认证连接中完整读取固定白名单内全部舰船静态配置表。
pub fn read_ship_catalog(
    client: &mut AgentClient,
    timeout_ms: u32,
    expected_module_sha256: &str,
) -> Result<ShipCatalogReadResult, ShipCatalogReadError> {
    read_ship_catalog_with(
        client,
        timeout_ms,
        expected_module_sha256,
        MAX_SHIP_CATALOG_PAGE_SIZE,
    )
}

/// 科技配置和历史记录随本次展示请求读取，不复用账号历史快照。
/// 已验证的静态白名单在模块身份一致时直接复用，不再重复分页。
pub(crate) fn read_ship_catalog_scoped(
    client: &mut AgentClient,
    timeout_ms: u32,
    expected_module_sha256: &str,
    technology: bool,
    cached: Option<ShipCatalogReadResult>,
) -> Result<ShipCatalogReadResult, ShipCatalogReadError> {
    read_ship_catalog_scoped_with(
        client,
        timeout_ms,
        expected_module_sha256,
        technology,
        cached,
    )
}

fn read_ship_catalog_scoped_with<R: ShipCatalogRuntime>(
    client: &mut R,
    timeout_ms: u32,
    expected_module_sha256: &str,
    technology: bool,
    cached: Option<ShipCatalogReadResult>,
) -> Result<ShipCatalogReadResult, ShipCatalogReadError> {
    let mut result = match cached {
        Some(cached) => {
            if cached.module_sha256() != expected_module_sha256 {
                return Err(RuntimeProtocolError::new(
                    "ship_catalog_cache_module_mismatch",
                    "缓存舰船静态目录与当前模块身份不一致",
                )
                .into());
            }
            cached.static_tables_only()?
        }
        None => read_ship_catalog_with(
            client,
            timeout_ms,
            expected_module_sha256,
            MAX_SHIP_CATALOG_PAGE_SIZE,
        )?,
    };
    if technology {
        for key in ShipCatalogTableKey::TECHNOLOGY {
            let table = read_table(
                client,
                timeout_ms,
                expected_module_sha256,
                key,
                MAX_SHIP_CATALOG_PAGE_SIZE,
            )
            .unwrap_or_else(|error| ShipCatalogTable {
                table_key: key,
                records: Vec::new(),
                read_error: Some(error.to_string()),
            });
            result.tables.push(table);
        }
        result.content_sha256 = sha256_sorted_json(&result.document())
            .map_err(ShipCatalogReadError::EncodeContentDigest)?;
    }
    Ok(result)
}

fn read_ship_catalog_with<R: ShipCatalogRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    expected_module_sha256: &str,
    page_size: u32,
) -> Result<ShipCatalogReadResult, ShipCatalogReadError> {
    debug_assert!(page_size > 0);
    let mut tables = Vec::with_capacity(ShipCatalogTableKey::ALL.len());
    for table_key in ShipCatalogTableKey::ALL {
        tables.push(read_table(
            runtime,
            timeout_ms,
            expected_module_sha256,
            table_key,
            page_size,
        )?);
    }
    let document = ShipCatalogDocument {
        schema_version: SHIP_CATALOG_SCHEMA_VERSION,
        tables: &tables,
    };
    let content_sha256 =
        sha256_sorted_json(&document).map_err(ShipCatalogReadError::EncodeContentDigest)?;
    Ok(ShipCatalogReadResult {
        module_sha256: expected_module_sha256.to_owned(),
        content_sha256,
        tables,
    })
}

fn read_table<R: ShipCatalogRuntime>(
    runtime: &mut R,
    timeout_ms: u32,
    expected_module_sha256: &str,
    table_key: ShipCatalogTableKey,
    page_size: u32,
) -> Result<ShipCatalogTable, ShipCatalogReadError> {
    let mut expected_total = None;
    let pages = read_restartable_pages(
        0,
        MAX_INCOMPLETE_PAGE_RETRIES,
        |start_index| {
            let page = runtime
                .snapshot_ship_catalog(
                    timeout_ms,
                    table_key,
                    start_index,
                    page_size,
                    expected_module_sha256,
                )
                .map_err(|source| ShipCatalogReadError::PageRuntime {
                    table_key,
                    start_index,
                    source,
                })?;
            page.validate(table_key, start_index, page_size, expected_module_sha256)
                .map_err(|source| ShipCatalogReadError::PageProtocol {
                    table_key,
                    start_index,
                    source,
                })?;
            match expected_total {
                None => expected_total = Some(page.total_count),
                Some(expected) if expected != page.total_count => {
                    return Err(ShipCatalogReadError::TotalCountChanged {
                        table_key,
                        expected,
                        actual: page.total_count,
                    });
                }
                Some(_) => {}
            }
            Ok(page)
        },
        |page| {
            PageState::new(
                page.next_index,
                !page.complete || !page.read_errors.is_empty(),
            )
        },
    )?;

    if let Some(page) = pages
        .iter()
        .find(|page| !page.complete || !page.read_errors.is_empty())
    {
        return Err(ShipCatalogReadError::IncompletePage {
            table_key,
            start_index: page.start_index,
            failures: page.read_errors.clone(),
        });
    }

    let expected = expected_total.expect("成功的分页读取必须至少返回一页");
    let mut identifiers = BTreeSet::new();
    let mut records = Vec::with_capacity(expected as usize);
    for record in pages.into_iter().flat_map(|page| page.records) {
        if !identifiers.insert(record.id) {
            return Err(ShipCatalogReadError::DuplicateRecord {
                table_key,
                id: record.id,
            });
        }
        records.push(record);
    }
    if records.len() != expected as usize {
        return Err(ShipCatalogReadError::RecordCountMismatch {
            table_key,
            expected,
            actual: records.len(),
        });
    }
    Ok(ShipCatalogTable {
        table_key,
        records,
        read_error: None,
    })
}

trait ShipCatalogRuntime {
    fn snapshot_ship_catalog(
        &mut self,
        timeout_ms: u32,
        table_key: ShipCatalogTableKey,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<ShipCatalogPageResult, RuntimeClientError>;
}

impl ShipCatalogRuntime for AgentClient {
    fn snapshot_ship_catalog(
        &mut self,
        timeout_ms: u32,
        table_key: ShipCatalogTableKey,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<ShipCatalogPageResult, RuntimeClientError> {
        AgentClient::snapshot_ship_catalog(
            self,
            timeout_ms,
            table_key,
            start_index,
            page_size,
            expected_module_sha256,
        )
    }
}

/// 静态目录读取在运行态、分页完整性和规范序列化边界上的失败分类。
#[derive(Debug, Error)]
pub enum ShipCatalogReadError {
    #[error("读取 {table} 静态目录页 start_index={start_index} 失败: {source}", table = .table_key.as_str())]
    PageRuntime {
        table_key: ShipCatalogTableKey,
        start_index: u32,
        #[source]
        source: RuntimeClientError,
    },
    #[error("校验 {table} 静态目录页 start_index={start_index} 失败: {source}", table = .table_key.as_str())]
    PageProtocol {
        table_key: ShipCatalogTableKey,
        start_index: u32,
        #[source]
        source: RuntimeProtocolError,
    },
    #[error("{table_key:?} 在索引 {start_index} 的页面最终仍不完整，共 {count} 条诊断", count = .failures.len())]
    IncompletePage {
        table_key: ShipCatalogTableKey,
        start_index: u32,
        failures: Vec<ShipCatalogPageReadError>,
    },
    #[error("{table_key:?} 分页期间总数从 {expected} 变为 {actual}")]
    TotalCountChanged {
        table_key: ShipCatalogTableKey,
        expected: u32,
        actual: u32,
    },
    #[error("{table_key:?} 跨页重复返回记录 ID {id}")]
    DuplicateRecord {
        table_key: ShipCatalogTableKey,
        id: u64,
    },
    #[error("{table_key:?} 应有 {expected} 条记录，完整汇总后实际为 {actual} 条")]
    RecordCountMismatch {
        table_key: ShipCatalogTableKey,
        expected: u32,
        actual: usize,
    },
    #[error("序列化舰船静态目录摘要失败：{0}")]
    EncodeContentDigest(#[source] serde_json::Error),
    /// 会话缓存与当前模块身份不一致，或静态白名单不完整。
    #[error(transparent)]
    Protocol(#[from] RuntimeProtocolError),
}

#[cfg(test)]
mod tests;

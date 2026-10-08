//! 以舰船静态名称读取 BWiki 获取途径；不可变缓存快照同时服务生成和执行写回。

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use calamine::Reader;
use serde::{Deserialize, Serialize};
use suzushiro_content_digest::sha256_bytes;

use crate::adapters::json_artifact::write_new_pretty_json;
use crate::adapters::tool_root::ToolRoot;
use crate::application::{
    AcquisitionGenerationOutcome, AcquisitionGenerationState, AcquisitionGenerationSummary,
    AcquisitionUpdateMode, AcquisitionUpdatePolicy, AppError, AppErrorCode, LayoutGenerationMode,
    OperationProgress, WorkbookLayout, WorkbookProjectionV4, WorkbookProjectionValue,
};

mod http;
mod parser;

const CACHE_DIRECTORY: &str = "data/cache/ship-acquisition";
const CACHE_MAX_BYTES: u64 = 128 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    schema_version: u32,
    title: String,
    source_url: String,
    fetched_at: u64,
    summary: String,
    #[serde(default)]
    missing: bool,
}

#[derive(Debug, thiserror::Error)]
enum CacheReadError {
    #[error("{0}")]
    Access(String),
    #[error("{detail}")]
    Invalid { timestamp: u64, detail: String },
}

impl From<String> for CacheReadError {
    fn from(detail: String) -> Self {
        Self::Access(detail)
    }
}

#[derive(Clone, Debug)]
struct Acquisition {
    text: String,
    status: AcquisitionStatus,
    notice: Option<String>,
    stored: bool,
    retained_previous: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcquisitionStatus {
    Current,
    Missing,
    Fallback,
    Unavailable,
    Incomplete,
    CacheWarning,
    NotStarted,
}

impl Acquisition {
    fn current(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            status: AcquisitionStatus::Current,
            notice: None,
            stored: false,
            retained_previous: false,
        }
    }

    fn not_started() -> Self {
        Self {
            text: String::new(),
            status: AcquisitionStatus::NotStarted,
            notice: None,
            stored: false,
            retained_previous: false,
        }
    }

    fn unavailable(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            notice: Some(text.clone()),
            text,
            status: AcquisitionStatus::Unavailable,
            stored: false,
            retained_previous: false,
        }
    }

    fn with_notice(text: impl Into<String>, status: AcquisitionStatus, notice: String) -> Self {
        Self {
            text: text.into(),
            status,
            notice: Some(notice),
            stored: false,
            retained_previous: false,
        }
    }

    fn keeping_previous(mut self) -> Self {
        self.retained_previous = true;
        self
    }

    fn cache_update(&self, name: &str) -> crate::application::AcquisitionCacheUpdate {
        use crate::application::AcquisitionCacheUpdateOutcome;
        let detail = self.notice.clone().unwrap_or_else(|| self.text.clone());
        let outcome = match self.status {
            AcquisitionStatus::NotStarted => AcquisitionCacheUpdateOutcome::NotStarted,
            AcquisitionStatus::Missing => AcquisitionCacheUpdateOutcome::Missing,
            AcquisitionStatus::Current if !self.stored => AcquisitionCacheUpdateOutcome::Cached,
            AcquisitionStatus::Current if self.stored => AcquisitionCacheUpdateOutcome::Updated,
            _ if self.retained_previous => AcquisitionCacheUpdateOutcome::KeptPrevious { detail },
            _ => AcquisitionCacheUpdateOutcome::Failed { detail },
        };
        crate::application::AcquisitionCacheUpdate::new(name, outcome)
    }

    fn needs_review(&self) -> bool {
        self.status != AcquisitionStatus::Current || self.notice.is_some()
    }

    fn record_generation(&self, name: &str, summary: &mut AcquisitionGenerationSummary) {
        let outcome = match self.status {
            AcquisitionStatus::Current if self.stored => AcquisitionGenerationOutcome::Updated,
            AcquisitionStatus::Current => AcquisitionGenerationOutcome::Cached,
            AcquisitionStatus::Missing => AcquisitionGenerationOutcome::Missing,
            AcquisitionStatus::Fallback => AcquisitionGenerationOutcome::Fallback,
            AcquisitionStatus::Unavailable => AcquisitionGenerationOutcome::Failed,
            AcquisitionStatus::Incomplete => AcquisitionGenerationOutcome::Incomplete,
            AcquisitionStatus::CacheWarning => AcquisitionGenerationOutcome::CacheWarning,
            AcquisitionStatus::NotStarted => AcquisitionGenerationOutcome::NotStarted,
        };
        let detail = self
            .needs_review()
            .then(|| self.notice.clone().unwrap_or_else(|| self.text.clone()));
        summary.record(name, outcome, detail);
    }
}

impl PartialEq for Acquisition {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.status == other.status && self.notice == other.notice
    }
}

impl Eq for Acquisition {}

impl std::ops::Deref for Acquisition {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl PartialEq<str> for Acquisition {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for Acquisition {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl std::fmt::Display for Acquisition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.text)
    }
}

#[derive(Debug)]
pub(crate) struct AcquisitionEnrichment {
    pub projection: WorkbookProjectionV4,
    pub values: BTreeMap<String, String>,
    pub summary: AcquisitionGenerationSummary,
}

pub(crate) struct ShipAcquisition {
    root: ToolRoot,
}

impl crate::application::ShipAcquisitionCachePort for ShipAcquisition {
    fn update_cache(
        &self,
        names: &[String],
        mode: AcquisitionUpdateMode,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<crate::application::AcquisitionCacheUpdate>, AppError> {
        let progress = RefCell::new(progress);
        self.update_cache_with(
            names,
            mode,
            &mut |event| progress.borrow_mut()(event),
            is_cancelled,
            &|title| {
                http::fetch(&self.root, title, is_cancelled, &mut |message| {
                    progress.borrow_mut()(OperationProgress::stage(message))
                })
            },
        )
    }
}

impl ShipAcquisition {
    pub(crate) fn new(root: ToolRoot) -> Self {
        Self { root }
    }

    /// 只处理实际生成的列；同步传入在线策略，执行写回以 None 限定只读缓存。
    pub(crate) fn enrich(
        &self,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
        policy: Option<AcquisitionUpdatePolicy>,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<AcquisitionEnrichment, AppError> {
        let progress = RefCell::new(progress);
        self.enrich_with(
            layout,
            projection,
            policy,
            &mut |event| progress.borrow_mut()(event),
            is_cancelled,
            &|title| {
                http::fetch(&self.root, title, is_cancelled, &mut |message| {
                    progress.borrow_mut()(OperationProgress::stage(message))
                })
            },
        )
    }

    fn enrich_with(
        &self,
        layout: &WorkbookLayout,
        projection: WorkbookProjectionV4,
        policy: Option<AcquisitionUpdatePolicy>,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
        fetch: &dyn Fn(&str) -> Result<Vec<u8>, http::FetchError>,
    ) -> Result<AcquisitionEnrichment, AppError> {
        if !layout.sheets().iter().any(|s| {
            s.stable_key() == "loadout_plan" && s.generation() != LayoutGenerationMode::Omitted
        }) || !layout.fields().iter().any(|f| {
            f.sheet_key() == "loadout_plan"
                && f.stable_key() == "acquisition"
                && f.generation() != LayoutGenerationMode::Omitted
        }) {
            return Ok(AcquisitionEnrichment {
                projection,
                values: BTreeMap::new(),
                summary: Default::default(),
            });
        }
        if projection.sheet("loadout_plan").is_none() {
            return Ok(AcquisitionEnrichment {
                projection,
                values: BTreeMap::new(),
                summary: Default::default(),
            });
        }
        let sheet = projection
            .sheet("loadout_plan")
            .expect("已确认存在配装计划表");
        let names = sheet
            .rows()
            .iter()
            .filter_map(|row| match row.value("original_name") {
                Some(WorkbookProjectionValue::Text(name)) if !name.trim().is_empty() => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| failure(e.to_string()))?
            .as_secs();
        let network_failed = OnceLock::new();
        let mut summaries = BTreeMap::new();
        progress(OperationProgress::stage(format!(
            "正在读取 {} 个舰船名称的获取方式与缓存",
            names.len()
        )));
        let mut reported = 0_usize;
        let records = Self::dispatch_names(
            &names,
            &|name| {
                if is_cancelled() {
                    return Acquisition::not_started();
                }
                self.resolve(name, now, policy, &network_failed, fetch)
            },
            &mut |name, record| {
                if let Some(detail) = &record.notice {
                    progress(OperationProgress::stage(format!(
                        "获取方式 {name}：{detail}"
                    )));
                }
                reported = reported.saturating_add(1);
                progress(OperationProgress::counted(
                    format!("获取方式：{name}"),
                    reported,
                    names.len(),
                ));
            },
        );
        crate::application::check_generation_cancelled(is_cancelled)?;
        for (name, record) in records {
            summaries.insert(name, record);
        }
        let mut generation_summary = AcquisitionGenerationSummary {
            state: AcquisitionGenerationState::Completed,
            ..Default::default()
        };
        for (name, record) in &summaries {
            record.record_generation(name, &mut generation_summary);
        }
        let mut applied = BTreeMap::new();
        let mut changed = false;
        for row in sheet.rows() {
            let summary = match row.value("original_name") {
                Some(WorkbookProjectionValue::Text(name)) => {
                    summaries.get(name).map(|record| record.text.clone())
                }
                _ => None,
            }
            .unwrap_or_else(|| {
                let detail = "获取方式未获取：舰船静态名称缺失".to_owned();
                generation_summary.record(
                    row.object_ref(),
                    AcquisitionGenerationOutcome::Failed,
                    Some(detail.clone()),
                );
                detail
            });
            changed |= !matches!(
                row.value("acquisition"),
                Some(WorkbookProjectionValue::Text(value)) if value == &summary
            );
            applied.insert(row.object_ref().to_owned(), summary);
        }
        let unavailable = generation_summary.warnings.len();
        progress(OperationProgress::stage(format!(
            "获取方式完成：{} 个舰船名称，{} 项需查看获取方式状态；来源 BWiki",
            summaries.len(),
            unavailable
        )));
        if !changed {
            return Ok(AcquisitionEnrichment {
                projection,
                values: applied,
                summary: generation_summary,
            });
        }
        let rows = sheet
            .rows()
            .iter()
            .map(|row| {
                let mut values = row.values().clone();
                let summary = applied[row.object_ref()].clone();
                values.insert(
                    "acquisition".to_owned(),
                    WorkbookProjectionValue::text(summary),
                );
                (row.object_ref().to_owned(), values)
            })
            .collect();
        let enriched = projection
            .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
            .map_err(|error| failure(error.to_string()))?;
        Ok(AcquisitionEnrichment {
            projection: enriched,
            values: applied,
            summary: generation_summary,
        })
    }

    /// 按布局稳定键读取图鉴名称。原名列省略时使用名称单元格里已经写入的图鉴链接。
    pub(crate) fn ship_titles(&self, workbook_path: &Path) -> Result<Vec<String>, AppError> {
        let document = super::document::WorkbookDocument::read(workbook_path, "获取方式工作簿")
            .map_err(|error| failure(error.to_string()))?;
        let layout_path = self
            .root
            .existing_file(Path::new("workbook-layout.xlsx"))
            .map_err(|error| failure(error.to_string()))?;
        let registry = crate::application::WorkbookProjectionV4::layout_registry()
            .map_err(|error| failure(error.to_string()))?;
        let root_layout = super::layout::load_workbook_layout(&layout_path, &registry)?;
        let mut workbook = super::reader::open_checked_workbook(&document)
            .map_err(|error| failure(error.to_string()))?;
        let layout = super::reader::select_layout_snapshot(&mut workbook, &root_layout)
            .map_err(|error| failure(error.to_string()))?;
        let sheet = layout
            .sheets()
            .iter()
            .find(|sheet| sheet.stable_key() == "loadout_plan")
            .ok_or_else(|| failure("布局没有 loadout_plan 工作表".to_owned()))?;
        let original = layout
            .fields()
            .iter()
            .find(|field| {
                field.sheet_key() == "loadout_plan" && field.stable_key() == "original_name"
            })
            .ok_or_else(|| failure("布局没有舰船原名字段".to_owned()))?;
        let range = workbook
            .worksheet_range(sheet.display_name())
            .map_err(|error| failure(format!("工作表不可读：{error}")))?;
        if original.generation() != LayoutGenerationMode::Omitted {
            return titles_from_original_column(&range, original);
        }
        let name_column = layout
            .generated_fields_for_sheet("loadout_plan")
            .iter()
            .position(|field| field.stable_key() == "name")
            .ok_or_else(|| failure("布局没有可用的舰船名称列".to_owned()))?;
        titles_from_name_links(
            document.package(),
            sheet.display_name(),
            &range,
            name_column,
        )
    }

    pub(in crate::adapters::workbook::ship_acquisition) fn update_cache_with(
        &self,
        names: &[String],
        mode: AcquisitionUpdateMode,
        progress: &mut dyn FnMut(OperationProgress),
        is_cancelled: &(dyn Fn() -> bool + Sync),
        fetch: &dyn Fn(&str) -> Result<Vec<u8>, http::FetchError>,
    ) -> Result<Vec<crate::application::AcquisitionCacheUpdate>, AppError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| failure(error.to_string()))?
            .as_secs();
        let network_failed = OnceLock::new();
        let mut reported = 0_usize;
        let records = Self::dispatch_names(
            names,
            &|name| {
                if is_cancelled() {
                    return Acquisition::not_started();
                }
                let record = self.resolve_with_mode(
                    name,
                    now,
                    Some(AcquisitionUpdatePolicy::Refresh),
                    &network_failed,
                    fetch,
                    mode,
                );
                if is_cancelled() && !record.stored {
                    Acquisition::not_started()
                } else {
                    record
                }
            },
            &mut |name, record| {
                reported = reported.saturating_add(1);
                let detail = record.notice.as_deref().unwrap_or(record.text.as_str());
                progress(OperationProgress::counted(
                    format!("资料更新 {name}：{detail}"),
                    reported,
                    names.len(),
                ));
            },
        );
        Ok(records
            .into_iter()
            .map(|(name, record)| record.cache_update(&name))
            .collect())
    }

    /// 串行处理并立即回报，缓存发布完成后才开始下一项。
    pub(super) fn dispatch_names<T>(
        names: &[String],
        work: &dyn Fn(&str) -> T,
        on_result: &mut dyn FnMut(&str, &T),
    ) -> Vec<(String, T)> {
        names
            .iter()
            .map(|name| {
                let record = work(name);
                on_result(name, &record);
                (name.clone(), record)
            })
            .collect()
    }

    fn resolve(
        &self,
        title: &str,
        now: u64,
        policy: Option<AcquisitionUpdatePolicy>,
        network_failed: &OnceLock<String>,
        fetch: &(impl Fn(&str) -> Result<Vec<u8>, http::FetchError> + ?Sized),
    ) -> Acquisition {
        self.resolve_with_mode(
            title,
            now,
            policy,
            network_failed,
            fetch,
            AcquisitionUpdateMode::Refresh,
        )
    }

    fn resolve_with_mode(
        &self,
        title: &str,
        now: u64,
        policy: Option<AcquisitionUpdatePolicy>,
        network_failed: &OnceLock<String>,
        fetch: &(impl Fn(&str) -> Result<Vec<u8>, http::FetchError> + ?Sized),
        mode: AcquisitionUpdateMode,
    ) -> Acquisition {
        let (cached, damaged) = match self.read_cache(title) {
            Ok(entry) => (entry, None),
            Err(CacheReadError::Invalid { timestamp, detail })
                if policy == Some(AcquisitionUpdatePolicy::Refresh) =>
            {
                (None, Some((timestamp, detail)))
            }
            Err(error) => return Acquisition::unavailable(format!("获取方式未获取：{error}")),
        };
        if policy != Some(AcquisitionUpdatePolicy::Refresh) {
            return match cached {
                Some(entry) => Acquisition {
                    status: if entry.missing {
                        AcquisitionStatus::Missing
                    } else {
                        AcquisitionStatus::Current
                    },
                    ..Acquisition::current(entry.summary)
                },
                None => Acquisition::unavailable("获取方式未获取：资料未缓存，待更新"),
            };
        }
        if mode == AcquisitionUpdateMode::Missing
            && let Some(entry) = cached.as_ref()
        {
            match self.last_attempt_failed(title) {
                Ok(false) => {
                    return Acquisition {
                        status: if entry.missing {
                            AcquisitionStatus::Missing
                        } else {
                            AcquisitionStatus::Current
                        },
                        ..Acquisition::current(entry.summary.clone())
                    };
                }
                Ok(true) => {}
                Err(detail) => {
                    return self.retain_cache(cached, &detail, AcquisitionStatus::CacheWarning);
                }
            }
        }
        let mut result = (|| {
            // 每次发布都排在已有快照之后，避免同秒刷新或时钟回拨读到旧资料。
            let previous_timestamp = cached
                .as_ref()
                .map(|entry| entry.fetched_at)
                .or_else(|| damaged.as_ref().map(|(timestamp, _)| *timestamp));
            let now = match previous_timestamp {
                Some(timestamp) => match timestamp.checked_add(1) {
                    Some(next) => now.max(next),
                    None => {
                        return self.retain_cache(
                            cached,
                            "缓存时间序号已达上限",
                            AcquisitionStatus::CacheWarning,
                        );
                    }
                },
                None => now,
            };
            if let Some(error) = network_failed.get() {
                return match cached {
                    Some(entry) => {
                        let notice = format!("缓存更新暂停：{error}");
                        Acquisition::with_notice(
                            format!("{}\n{notice}", entry.summary),
                            AcquisitionStatus::Fallback,
                            notice,
                        )
                        .keeping_previous()
                    }
                    None => Acquisition::unavailable(format!(
                        "获取方式未获取：本次在线查询已暂停：{error}"
                    )),
                };
            }
            let response = fetch(title);
            if let Err(error) = &response
                && error.stop_batch
            {
                let _ = network_failed.set(error.message.clone());
            }
            let parsed = match response {
                Ok(bytes) => parser::parse_response(&bytes),
                Err(error) => {
                    return self.retain_cache(cached, &error.message, AcquisitionStatus::Fallback);
                }
            };
            if let parser::ParsedPage::Failed(issue) = &parsed
                && let Some(code) = issue.api_code()
                && http::api_error_stops_batch(code)
            {
                let _ = network_failed.set(issue.detail().to_owned());
            }
            match parsed {
                parser::ParsedPage::Ready(summary) => {
                    self.store_current(title, now, summary, cached.is_some())
                }
                parser::ParsedPage::Missing => self.store_missing(
                    title,
                    now,
                    "BWiki 未收录该舰船页面".to_owned(),
                    cached.is_some(),
                ),
                parser::ParsedPage::Partial { summary, detail } => match cached {
                    Some(entry) => {
                        let notice = format!("获取方式不完整：{detail}；已保留原缓存");
                        Acquisition::with_notice(
                            format!("{}\n{notice}", entry.summary),
                            AcquisitionStatus::Incomplete,
                            notice,
                        )
                        .keeping_previous()
                    }
                    None => {
                        let notice = format!("获取方式不完整：{detail}");
                        Acquisition::with_notice(
                            format!("{summary}\n{notice}"),
                            AcquisitionStatus::Incomplete,
                            notice,
                        )
                    }
                },
                parser::ParsedPage::Failed(issue) => {
                    self.retain_cache(cached, issue.detail(), AcquisitionStatus::Fallback)
                }
            }
        })();
        if let Some((_, detail)) = damaged {
            let notice = format!("原获取方式缓存损坏，已保留原文件：{detail}");
            result.text.push_str(&format!("\n{notice}"));
            result.notice = Some(match result.notice {
                Some(previous) => format!("{previous}；{notice}"),
                None => notice,
            });
        }
        if let Err(detail) = self.save_attempt(title, !result.stored) {
            let notice = format!("保存获取方式更新状态失败：{detail}");
            result.notice = Some(match result.notice {
                Some(previous) => format!("{previous}；{notice}"),
                None => notice,
            });
            result.status = AcquisitionStatus::CacheWarning;
        }
        result
    }

    // 更新结果独立于有效资料快照，失败重试不能覆盖上一次可用摘要。
    fn last_attempt_failed(&self, title: &str) -> Result<bool, String> {
        let directory = Path::new(CACHE_DIRECTORY).join(sha256_bytes(title.as_bytes()));
        let files = self
            .root
            .list_direct_files(&directory)
            .map_err(|e| e.to_string())?;
        let newest = files
            .iter()
            .filter(|(path, _)| path.extension().is_some_and(|ext| ext == "attempt"))
            .max_by_key(|(path, _)| path.file_name());
        let Some((path, size)) = newest else {
            return Ok(false);
        };
        if *size > 16 {
            return Err("获取方式更新状态过大".to_owned());
        }
        let path = self.root.existing_file(path).map_err(|e| e.to_string())?;
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        self.root
            .ensure_open_file_matches(&file, &path)
            .map_err(|e| e.to_string())?;
        serde_json::from_reader(file.take(17)).map_err(|e| format!("获取方式更新状态：{e}"))
    }

    fn save_attempt(&self, title: &str, failed: bool) -> Result<(), String> {
        let directory = Path::new(CACHE_DIRECTORY).join(sha256_bytes(title.as_bytes()));
        self.root
            .ensure_directory(&directory)
            .map_err(|e| e.to_string())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|e| e.to_string())?;
        let name = format!("{now:039}-{:016x}", u64::from_le_bytes(random));
        write_new_pretty_json(
            &self.root,
            &directory.join(format!(".{name}.tmp")),
            &directory.join(format!("{name}.attempt")),
            16,
            &failed,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    fn store_current(
        &self,
        title: &str,
        now: u64,
        summary: String,
        had_previous: bool,
    ) -> Acquisition {
        self.store_summary(
            title,
            now,
            summary,
            AcquisitionStatus::Current,
            had_previous,
        )
    }

    fn store_missing(
        &self,
        title: &str,
        now: u64,
        summary: String,
        had_previous: bool,
    ) -> Acquisition {
        self.store_summary(
            title,
            now,
            summary,
            AcquisitionStatus::Missing,
            had_previous,
        )
    }

    fn store_summary(
        &self,
        title: &str,
        now: u64,
        summary: String,
        status: AcquisitionStatus,
        had_previous: bool,
    ) -> Acquisition {
        let entry = Entry {
            schema_version: 1,
            title: title.to_owned(),
            source_url: super::ship_wiki::ship_wiki_url(title).unwrap_or_default(),
            fetched_at: now,
            summary: summary.clone(),
            missing: status == AcquisitionStatus::Missing,
        };
        match self.save_cache(&entry) {
            Ok(()) => Acquisition {
                text: summary,
                status,
                notice: None,
                stored: true,
                retained_previous: false,
            },
            Err(error) => {
                let notice = format!("获取方式缓存失败：{error}");
                let record = Acquisition::with_notice(
                    format!("{summary}\n{notice}"),
                    AcquisitionStatus::CacheWarning,
                    notice,
                );
                if had_previous {
                    record.keeping_previous()
                } else {
                    record
                }
            }
        }
    }

    fn retain_cache(
        &self,
        cached: Option<Entry>,
        detail: &str,
        status: AcquisitionStatus,
    ) -> Acquisition {
        match cached {
            Some(entry) => {
                let notice = format!("缓存更新失败：{detail}");
                Acquisition::with_notice(format!("{}\n{notice}", entry.summary), status, notice)
                    .keeping_previous()
            }
            None => Acquisition::unavailable(format!("获取方式未获取：{detail}")),
        }
    }

    fn read_cache(&self, title: &str) -> Result<Option<Entry>, CacheReadError> {
        let directory = Path::new(CACHE_DIRECTORY).join(sha256_bytes(title.as_bytes()));
        let files = self
            .root
            .list_direct_files(&directory)
            .map_err(|e| format!("读取获取方式缓存目录：{e}"))?;
        let newest = files
            .iter()
            .filter_map(|(path, size)| {
                if path.extension()?.to_str()? != "json" {
                    return None;
                }
                let stem = path.file_stem()?.to_str()?;
                let (timestamp, _) = stem.split_once('-')?;
                let timestamp = timestamp.parse::<u64>().ok()?;
                Some((timestamp, path, *size))
            })
            .max_by_key(|(time, _, _)| *time);
        let Some((timestamp, path, size)) = newest else {
            return Ok(None);
        };
        let path = self.root.existing_file(path).map_err(|e| e.to_string())?;
        let file = std::fs::File::open(&path).map_err(|e| format!("打开获取方式缓存：{e}"))?;
        self.root
            .ensure_open_file_matches(&file, &path)
            .map_err(|e| e.to_string())?;
        let invalid = |detail| CacheReadError::Invalid { timestamp, detail };
        if size > CACHE_MAX_BYTES {
            return Err(invalid("获取方式缓存超过大小上限".to_owned()));
        }
        let mut bytes = Vec::new();
        file.take(CACHE_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > CACHE_MAX_BYTES {
            return Err(invalid("获取方式缓存超过大小上限".to_owned()));
        }
        let entry: Entry = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("获取方式缓存 JSON：{e}")))?;
        if entry.schema_version != 1
            || entry.title != title
            || entry.fetched_at != timestamp
            || entry.source_url != super::ship_wiki::ship_wiki_url(title).unwrap_or_default()
            || entry.summary.is_empty()
            || entry.summary.encode_utf16().count() > 28_000
        {
            return Err(invalid("获取方式缓存身份或内容无效".to_owned()));
        }
        Ok(Some(entry))
    }

    fn save_cache(&self, entry: &Entry) -> Result<(), String> {
        let directory = Path::new(CACHE_DIRECTORY).join(sha256_bytes(entry.title.as_bytes()));
        self.root
            .ensure_directory(&directory)
            .map_err(|e| e.to_string())?;
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|e| e.to_string())?;
        let temporary = directory.join(format!(
            ".{}.{}.tmp",
            entry.fetched_at,
            u64::from_le_bytes(random)
        ));
        let target = directory.join(format!(
            "{}-{}.json",
            entry.fetched_at,
            u64::from_le_bytes(random)
        ));
        write_new_pretty_json(&self.root, &temporary, &target, CACHE_MAX_BYTES, entry)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn titles_from_original_column(
    range: &calamine::Range<calamine::Data>,
    field: &crate::application::WorkbookFieldLayout,
) -> Result<Vec<String>, AppError> {
    let mut rows = range.rows();
    let header = rows
        .next()
        .ok_or_else(|| failure("工作表没有表头".to_owned()))?;
    let column = header
        .iter()
        .position(|cell| cell.to_string().trim() == field.display_name())
        .ok_or_else(|| failure(format!("工作表没有“{}”列", field.display_name())))?;
    let mut names = BTreeSet::new();
    for row in rows {
        let text = row
            .get(column)
            .map(|cell| cell.to_string())
            .unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        names.insert(text.to_owned());
    }
    Ok(names.into_iter().collect())
}

fn titles_from_name_links(
    package: &super::package::PackageSnapshot,
    sheet_name: &str,
    range: &calamine::Range<calamine::Data>,
    name_column: usize,
) -> Result<Vec<String>, AppError> {
    let links = super::sheet_parts::worksheet_hyperlinks(package, sheet_name)
        .map_err(|error| failure(error.to_string()))?;
    let column_name = excel_column_name(name_column);
    let mut rows = range.rows();
    let header = rows
        .next()
        .ok_or_else(|| failure("工作表没有表头".to_owned()))?;
    let header_text = header
        .get(name_column)
        .map(|cell| cell.to_string())
        .unwrap_or_default();
    if header_text.trim().is_empty() {
        return Err(failure("工作表没有舰船名称列".to_owned()));
    }
    let mut names = BTreeSet::new();
    let mut saw_ship = false;
    for (row_index, row) in rows.enumerate() {
        let excel_row = row_index + 2;
        let display = row
            .get(name_column)
            .map(|cell| cell.to_string())
            .unwrap_or_default();
        let cell = format!("{column_name}{excel_row}");
        let link = links.get(&cell);
        if display.trim().is_empty() && link.is_none() {
            continue;
        }
        saw_ship = true;
        let Some(url) = link else {
            return Err(failure(format!("{cell} 缺少舰船原名")));
        };
        let Some(title) = super::ship_wiki::original_name_from_wiki_url(url) else {
            return Err(failure(format!("{cell} 的图鉴链接不能还原舰船原名")));
        };
        names.insert(title);
    }
    if saw_ship && names.is_empty() {
        return Err(failure("工作簿里的舰船没有可核对的原名".to_owned()));
    }
    Ok(names.into_iter().collect())
}

fn excel_column_name(mut zero_based: usize) -> String {
    let mut value = String::new();
    zero_based += 1;
    while zero_based > 0 {
        let remainder = (zero_based - 1) % 26;
        value.push(char::from(b'A' + u8::try_from(remainder).unwrap()));
        zero_based = (zero_based - 1) / 26;
    }
    value.chars().rev().collect()
}

fn failure(message: String) -> AppError {
    AppError::from_source(
        "workbook.acquisition",
        AppErrorCode::WorkbookInvalid,
        "获取方式投影失败",
        std::io::Error::other(message),
    )
}

#[cfg(test)]
mod tests;

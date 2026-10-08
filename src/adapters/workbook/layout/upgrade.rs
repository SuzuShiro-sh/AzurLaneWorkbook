//! 以稳定键合并布局设置，并将升级结果排他发布到独立 XLSX 文件。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::layout::template::{
    build_default_layout_workbook_bytes, build_layout_workbook_bytes,
};
use crate::adapters::workbook::package::{PackageSnapshot, write_new_file_bytes};
use crate::application::{
    AppError, AppErrorCode, LAYOUT_SCHEMA_VERSION, LayoutGenerationMode, LayoutUpgradeItemCounts,
    LayoutUpgradePort, LayoutUpgradeReport, WorkbookLayout, WorkbookLayoutRegistry,
};
use suzushiro_content_digest::sha256_bytes;

use super::parser::{
    ControlEnumLabels, ParsedEnumOption, ParsedField, ParsedLayout, ParsedSheet, ParsedStyle,
};
use super::{
    WorkbookLayoutError, load_layout_snapshot, map_layout_error_at, parse_layout_snapshot,
    read_layout_bytes, validate_layout_path, validation,
};

const SOURCE_LAYOUT_PATH: &str = "workbook-layout.xlsx";
const OUTPUT_DIRECTORY: &str = "data/workbooks";
const OUTPUT_LAYOUT_PATH: &str = "data/workbooks/workbook-layout.updated.xlsx";
/// 使用工具根目录和当前注册表完成一次固定路径的布局升级。
pub(crate) struct XlsxLayoutUpgradePort {
    tool_root: ToolRoot,
    registry: WorkbookLayoutRegistry,
}

impl XlsxLayoutUpgradePort {
    /// 固定受控工具根目录，升级时不接受外部输入或输出路径。
    pub(crate) fn new(tool_root: ToolRoot, registry: WorkbookLayoutRegistry) -> Self {
        Self {
            tool_root,
            registry,
        }
    }
}

impl LayoutUpgradePort for XlsxLayoutUpgradePort {
    fn upgrade_layout(&self) -> Result<LayoutUpgradeReport, AppError> {
        perform_upgrade(&self.tool_root, &self.registry).map_err(|error| {
            map_upgrade_error(
                self.tool_root.as_path().join(SOURCE_LAYOUT_PATH),
                self.tool_root.as_path().join(OUTPUT_LAYOUT_PATH),
                error,
            )
        })
    }
}

fn perform_upgrade(
    tool_root: &ToolRoot,
    registry: &WorkbookLayoutRegistry,
) -> Result<LayoutUpgradeReport, LayoutUpgradeError> {
    let source_path = tool_root
        .existing_file(Path::new(SOURCE_LAYOUT_PATH))
        .map_err(|source| LayoutUpgradeError::ToolRoot {
            operation: "读取源布局",
            source,
        })?;
    validate_layout_path(&source_path).map_err(LayoutUpgradeError::SourceLayout)?;
    let source_bytes = read_layout_bytes(&source_path).map_err(LayoutUpgradeError::SourceLayout)?;
    let source_package_sha256 = sha256_bytes(&source_bytes);

    let intended_output_path = tool_root.as_path().join(OUTPUT_LAYOUT_PATH);
    let default_bytes = build_default_layout_workbook_bytes(&intended_output_path)
        .map_err(LayoutUpgradeError::Workbook)?;
    reject_unmodeled_parts(&source_path, &source_bytes, &default_bytes)?;

    let source = parse_layout_snapshot(&source_path, &source_bytes, true)
        .map_err(LayoutUpgradeError::SourceLayout)?;
    let defaults = parse_layout_snapshot(&intended_output_path, &default_bytes, false)
        .map_err(LayoutUpgradeError::DefaultLayout)?;
    let merge = merge_layout(source, defaults, registry)?;
    let output_bytes = build_layout_workbook_bytes(&intended_output_path, &merge.layout)
        .map_err(LayoutUpgradeError::Workbook)?;
    let output_package_sha256 = sha256_bytes(&output_bytes);

    tool_root
        .ensure_directory(Path::new(OUTPUT_DIRECTORY))
        .map_err(|source| LayoutUpgradeError::ToolRoot {
            operation: "建立布局升级输出目录",
            source,
        })?;
    tool_root
        .prepare_new_file(Path::new(OUTPUT_LAYOUT_PATH))
        .map_err(|source| LayoutUpgradeError::ToolRoot {
            operation: "检查布局升级输出",
            source,
        })?;

    let temporary_relative = temporary_layout_path().map_err(LayoutUpgradeError::Workbook)?;
    let temporary_path = tool_root
        .prepare_new_file(&temporary_relative)
        .map_err(|source| LayoutUpgradeError::ToolRoot {
            operation: "准备布局升级临时文件",
            source,
        })?;
    // 排他写入原语只清理自己已经创建的文件；创建失败时不能删除同名竞争者。
    let temporary_file = write_new_file_bytes(&temporary_path, &output_bytes)
        .map_err(|source| LayoutUpgradeError::Workbook(source.into()))?;
    let publish_result = verify_and_publish_layout(
        tool_root,
        &temporary_file,
        registry,
        &source_path,
        &source_package_sha256,
        &temporary_relative,
        &temporary_path,
        &output_package_sha256,
        merge.layout.content_sha256(),
    );
    let verified = match publish_result {
        Ok((_, verified)) => verified,
        Err(operation) => {
            return match tool_root.remove_file_if_exists(&temporary_relative, Some(&temporary_file))
            {
                Ok(_) => Err(operation),
                Err(source) => Err(LayoutUpgradeError::Cleanup {
                    operation: Box::new(operation),
                    source,
                }),
            };
        }
    };

    Ok(LayoutUpgradeReport::new(
        SOURCE_LAYOUT_PATH.to_owned(),
        OUTPUT_LAYOUT_PATH.to_owned(),
        merge.source_schema_version,
        verified.schema_version(),
        merge.preserved,
        merge.added,
        merge.added_items,
        source_package_sha256,
        output_package_sha256,
        verified.content_sha256().to_owned(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn verify_and_publish_layout(
    tool_root: &ToolRoot,
    temporary_file: &std::fs::File,
    registry: &WorkbookLayoutRegistry,
    source_path: &Path,
    source_package_sha256: &str,
    temporary_relative: &Path,
    temporary_path: &Path,
    expected_package_sha256: &str,
    expected_content_sha256: &str,
) -> Result<(PathBuf, WorkbookLayout), LayoutUpgradeError> {
    let temporary_bytes =
        read_layout_bytes(temporary_path).map_err(LayoutUpgradeError::OutputLayout)?;
    let actual_package_sha256 = sha256_bytes(&temporary_bytes);
    if actual_package_sha256 != expected_package_sha256 {
        return Err(LayoutUpgradeError::OutputPackageDigestMismatch {
            expected: expected_package_sha256.to_owned(),
            actual: actual_package_sha256,
        });
    }
    let verified = load_layout_snapshot(temporary_path, &temporary_bytes, registry)
        .map_err(LayoutUpgradeError::OutputLayout)?;
    if verified.content_sha256() != expected_content_sha256 {
        return Err(LayoutUpgradeError::OutputDigestMismatch {
            expected: expected_content_sha256.to_owned(),
            actual: verified.content_sha256().to_owned(),
        });
    }

    let current_source =
        read_layout_bytes(source_path).map_err(LayoutUpgradeError::SourceLayout)?;
    let current_source_sha256 = sha256_bytes(&current_source);
    if current_source_sha256 != source_package_sha256 {
        return Err(LayoutUpgradeError::SourceChanged {
            expected: source_package_sha256.to_owned(),
            actual: current_source_sha256,
        });
    }

    let output_path = tool_root
        .rename_new_file(
            temporary_file,
            temporary_relative,
            Path::new(OUTPUT_LAYOUT_PATH),
        )
        .map_err(|source| LayoutUpgradeError::ToolRoot {
            operation: "排他发布布局升级文件",
            source,
        })?;
    Ok((output_path, verified))
}

fn temporary_layout_path() -> Result<PathBuf, WorkbookProbeError> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(WorkbookProbeError::RandomSource)?;
    Ok(PathBuf::from(OUTPUT_DIRECTORY).join(format!(
        ".workbook-layout.updated.{}.{:016x}.tmp.xlsx",
        std::process::id(),
        u64::from_le_bytes(random)
    )))
}

/// 重新生成只允许丢弃当前模板本来就会管理的标准部件。
fn reject_unmodeled_parts(
    source_path: &Path,
    source_bytes: &[u8],
    default_bytes: &[u8],
) -> Result<(), LayoutUpgradeError> {
    let source = PackageSnapshot::from_bytes(source_bytes, source_path)
        .map_err(|source| LayoutUpgradeError::Workbook(source.into()))?;
    let baseline = PackageSnapshot::from_bytes(default_bytes, source_path)
        .map_err(|source| LayoutUpgradeError::Workbook(source.into()))?;
    let supported: BTreeSet<&str> = baseline.entry_names().collect();
    let unsupported: Vec<String> = source
        .entry_names()
        .filter(|name| !supported.contains(*name))
        .map(str::to_owned)
        .collect();
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(LayoutUpgradeError::UnsupportedParts { unsupported })
    }
}

#[derive(Debug)]
pub(super) struct LayoutMergeResult {
    pub(super) layout: WorkbookLayout,
    pub(super) source_schema_version: u32,
    pub(super) preserved: LayoutUpgradeItemCounts,
    pub(super) added: LayoutUpgradeItemCounts,
    pub(super) added_items: Vec<String>,
}

pub(super) fn merge_layout(
    source: ParsedLayout,
    mut defaults: ParsedLayout,
    registry: &WorkbookLayoutRegistry,
) -> Result<LayoutMergeResult, LayoutUpgradeError> {
    let source_schema_version = source.schema_version;
    if source.schema_version != LAYOUT_SCHEMA_VERSION {
        return Err(LayoutUpgradeError::UnsupportedSchema {
            actual: source.schema_version,
            expected: LAYOUT_SCHEMA_VERSION,
        });
    }

    let unsupported = unknown_stable_items(&source, &defaults);
    if !unsupported.is_empty() {
        return Err(LayoutUpgradeError::UnsupportedStableItems { unsupported });
    }
    match validation::validate_layout(source.clone(), registry) {
        Ok(_) | Err(WorkbookLayoutError::UpgradeRequired { .. }) => {}
        Err(error) => return Err(LayoutUpgradeError::SourceLayout(error)),
    }

    let preserved = LayoutUpgradeItemCounts::new(
        source.sheets.len(),
        source.fields.len(),
        source.enum_options.len(),
        source.styles.len(),
    );
    let added = LayoutUpgradeItemCounts::new(
        defaults.sheets.len() - source.sheets.len(),
        defaults.fields.len() - source.fields.len(),
        defaults.enum_options.len() - source.enum_options.len(),
        defaults.styles.len() - source.styles.len(),
    );
    let mut added_items = Vec::new();

    defaults.template_name = source.template_name;
    defaults.purpose = source.purpose;
    merge_sheets(&mut defaults.sheets, source.sheets, &mut added_items);
    merge_fields(&mut defaults.fields, source.fields, &mut added_items);
    merge_enum_options(
        &mut defaults.enum_options,
        source.enum_options,
        &mut added_items,
    );
    merge_styles(&mut defaults.styles, source.styles, &mut added_items);
    defaults.control_labels = ControlEnumLabels::new(&defaults.enum_options, true)
        .map_err(LayoutUpgradeError::MergedLayout)?;
    added_items.sort();

    let layout = validation::validate_layout(defaults, registry)
        .map_err(LayoutUpgradeError::MergedLayout)?;
    Ok(LayoutMergeResult {
        layout,
        source_schema_version,
        preserved,
        added,
        added_items,
    })
}

fn unknown_stable_items(source: &ParsedLayout, defaults: &ParsedLayout) -> Vec<String> {
    let sheets: BTreeSet<&str> = defaults
        .sheets
        .iter()
        .map(|row| row.stable_key.as_str())
        .collect();
    let fields: BTreeSet<(&str, &str)> = defaults
        .fields
        .iter()
        .map(|row| (row.sheet_key.as_str(), row.stable_key.as_str()))
        .collect();
    let enum_options: BTreeSet<(&str, &str)> = defaults
        .enum_options
        .iter()
        .map(|row| (row.category_key.as_str(), row.stable_value.as_str()))
        .collect();
    let styles: BTreeSet<&str> = defaults
        .styles
        .iter()
        .map(|row| row.stable_key.as_str())
        .collect();
    let mut unsupported = Vec::new();
    for row in &source.sheets {
        if !sheets.contains(row.stable_key.as_str()) {
            unsupported.push(format!("sheet:{}", row.stable_key));
        }
    }
    for row in &source.fields {
        if !fields.contains(&(row.sheet_key.as_str(), row.stable_key.as_str())) {
            unsupported.push(format!("field:{}.{}", row.sheet_key, row.stable_key));
        }
    }
    for row in &source.enum_options {
        if !enum_options.contains(&(row.category_key.as_str(), row.stable_value.as_str())) {
            unsupported.push(format!("enum:{}.{}", row.category_key, row.stable_value));
        }
    }
    for row in &source.styles {
        if !styles.contains(row.stable_key.as_str()) {
            unsupported.push(format!("style:{}", row.stable_key));
        }
    }
    unsupported.sort();
    unsupported.dedup();
    unsupported
}

fn merge_sheets(
    defaults: &mut [ParsedSheet],
    source: Vec<ParsedSheet>,
    added_items: &mut Vec<String>,
) {
    let mut source_by_key: BTreeMap<String, ParsedSheet> = source
        .into_iter()
        .map(|row| (row.stable_key.clone(), row))
        .collect();
    let mut used_orders: BTreeSet<u32> = source_by_key
        .values()
        .filter(|row| row.generation != LayoutGenerationMode::Omitted)
        .map(|row| row.order)
        .collect();
    for row in defaults {
        if let Some(source_row) = source_by_key.remove(&row.stable_key) {
            *row = source_row;
        } else {
            row.order = reserve_order(row.order, &mut used_orders);
            added_items.push(format!("sheet:{}", row.stable_key));
        }
    }
}

fn merge_fields(
    defaults: &mut [ParsedField],
    source: Vec<ParsedField>,
    added_items: &mut Vec<String>,
) {
    let mut source_by_key: BTreeMap<(String, String), ParsedField> = source
        .into_iter()
        .map(|row| ((row.sheet_key.clone(), row.stable_key.clone()), row))
        .collect();
    let mut used_orders: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for row in source_by_key.values() {
        if row.generation != LayoutGenerationMode::Omitted {
            used_orders
                .entry(row.sheet_key.clone())
                .or_default()
                .insert(row.order);
        }
    }
    for row in defaults {
        let key = (row.sheet_key.clone(), row.stable_key.clone());
        if let Some(source_row) = source_by_key.remove(&key) {
            *row = source_row;
        } else {
            row.order = reserve_order(
                row.order,
                used_orders.entry(row.sheet_key.clone()).or_default(),
            );
            added_items.push(format!("field:{}.{}", row.sheet_key, row.stable_key));
        }
    }
}

fn merge_enum_options(
    defaults: &mut [ParsedEnumOption],
    source: Vec<ParsedEnumOption>,
    added_items: &mut Vec<String>,
) {
    let mut source_by_key: BTreeMap<(String, String), ParsedEnumOption> = source
        .into_iter()
        .map(|row| ((row.category_key.clone(), row.stable_value.clone()), row))
        .collect();
    let mut used_orders: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for row in source_by_key.values() {
        used_orders
            .entry(row.category_key.clone())
            .or_default()
            .insert(row.order);
    }
    for row in defaults {
        let key = (row.category_key.clone(), row.stable_value.clone());
        if let Some(source_row) = source_by_key.remove(&key) {
            *row = source_row;
        } else {
            row.order = reserve_order(
                row.order,
                used_orders.entry(row.category_key.clone()).or_default(),
            );
            added_items.push(format!("enum:{}.{}", row.category_key, row.stable_value));
        }
    }
}

fn merge_styles(
    defaults: &mut [ParsedStyle],
    source: Vec<ParsedStyle>,
    added_items: &mut Vec<String>,
) {
    let mut source_by_key: BTreeMap<String, ParsedStyle> = source
        .into_iter()
        .map(|row| (row.stable_key.clone(), row))
        .collect();
    for row in defaults {
        if let Some(source_row) = source_by_key.remove(&row.stable_key) {
            *row = source_row;
        } else {
            added_items.push(format!("style:{}", row.stable_key));
        }
    }
}

fn reserve_order(preferred: u32, used: &mut BTreeSet<u32>) -> u32 {
    if used.insert(preferred) {
        return preferred;
    }
    let mut candidate = 1_u32;
    while !used.insert(candidate) {
        candidate = candidate
            .checked_add(1)
            .expect("布局表格行数上限保证存在可用顺序值");
    }
    candidate
}

fn map_upgrade_error(
    source_path: PathBuf,
    output_path: PathBuf,
    error: LayoutUpgradeError,
) -> AppError {
    let error = match error {
        LayoutUpgradeError::SourceLayout(source) if source.missing().is_none() => {
            return map_layout_error_at("workbook.layout.upgrade.read", &source_path, source);
        }
        LayoutUpgradeError::SourceLayout(source) => LayoutUpgradeError::SourceLayout(source),
        error => error,
    };

    let mut context = Vec::new();
    match &error {
        LayoutUpgradeError::UnsupportedSchema { actual, expected } => {
            context.push(("actual", actual.to_string()));
            context.push(("expected", expected.to_string()));
            context.push(("migration_schema", format!("{actual}->{expected}")));
        }
        LayoutUpgradeError::UnsupportedStableItems { unsupported } => {
            context.push(("deprecated", unsupported.join(",")));
        }
        LayoutUpgradeError::UnsupportedParts { unsupported } => {
            context.push(("unsupported_parts", unsupported.join(",")));
        }
        LayoutUpgradeError::OutputPackageDigestMismatch { expected, actual }
        | LayoutUpgradeError::OutputDigestMismatch { expected, actual } => {
            context.push(("actual", actual.clone()));
            context.push(("expected", expected.clone()));
            context.push(("output_digest_mismatch", "true".to_owned()));
        }
        LayoutUpgradeError::SourceChanged { expected, actual } => {
            context.push(("actual", actual.clone()));
            context.push(("expected", expected.clone()));
            context.push(("source_changed", "true".to_owned()));
        }
        LayoutUpgradeError::SourceLayout(source) => {
            if let Some(missing) = source.missing() {
                context.push(("missing", missing.join(",")));
            }
        }
        _ => {}
    }
    let message = error.user_message();
    let mut application_error = AppError::from_source(
        "workbook.layout.upgrade",
        AppErrorCode::LayoutMigrationFailed,
        message,
        error,
    )
    .with_context("path", source_path.to_string_lossy())
    .with_context("output_path", output_path.to_string_lossy());
    for (key, value) in context {
        application_error = application_error.with_context(key, value);
    }
    application_error
}

#[derive(Debug, Error)]
pub(super) enum LayoutUpgradeError {
    #[error("源布局未通过升级读取: {0}")]
    SourceLayout(#[source] WorkbookLayoutError),
    #[error("当前版本没有从布局 schema {actual} 到 {expected} 的显式迁移规则")]
    UnsupportedSchema { actual: u32, expected: u32 },
    #[error("旧稳定项没有显式迁移规则: {unsupported:?}")]
    UnsupportedStableItems { unsupported: Vec<String> },
    #[error("源布局包含重建写入器不能保留的部件: {unsupported:?}")]
    UnsupportedParts { unsupported: Vec<String> },
    #[error("程序内置默认布局无效: {0}")]
    DefaultLayout(#[source] WorkbookLayoutError),
    #[error("合并后的布局未通过当前契约: {0}")]
    MergedLayout(#[source] WorkbookLayoutError),
    #[error("升级文件严格重载失败: {0}")]
    OutputLayout(#[source] WorkbookLayoutError),
    #[error("升级文件语义摘要不一致: expected={expected}, actual={actual}")]
    OutputDigestMismatch { expected: String, actual: String },
    #[error("升级文件包摘要不一致: expected={expected}, actual={actual}")]
    OutputPackageDigestMismatch { expected: String, actual: String },
    #[error("升级期间源布局发生变化: expected={expected}, actual={actual}")]
    SourceChanged { expected: String, actual: String },
    #[error("{operation}失败: {source}")]
    ToolRoot {
        operation: &'static str,
        #[source]
        source: ToolRootError,
    },
    #[error("工作簿升级处理失败: {0}")]
    Workbook(#[source] WorkbookProbeError),
    #[error("原操作失败且临时文件清理失败: operation={operation}; cleanup={source}")]
    Cleanup {
        operation: Box<LayoutUpgradeError>,
        #[source]
        source: ToolRootError,
    },
}

impl LayoutUpgradeError {
    fn user_message(&self) -> &'static str {
        match self {
            Self::UnsupportedSchema { .. } | Self::UnsupportedStableItems { .. } => {
                "布局缺少可验证的显式迁移规则"
            }
            Self::UnsupportedParts { .. } => "布局包含不能无损迁移的附加内容",
            Self::SourceChanged { .. } => "升级期间源布局发生变化，未发布输出",
            Self::ToolRoot { .. } => "布局升级路径或非覆盖发布条件不满足",
            Self::Cleanup { .. } => "布局升级失败后未能清理临时文件",
            Self::DefaultLayout(_)
            | Self::MergedLayout(_)
            | Self::OutputLayout(_)
            | Self::OutputPackageDigestMismatch { .. }
            | Self::OutputDigestMismatch { .. }
            | Self::Workbook(_) => "布局升级产物未通过完整验证",
            Self::SourceLayout(source) if source.missing().is_some() => {
                "布局缺少可验证的显式迁移规则"
            }
            Self::SourceLayout(_) => "源布局未通过升级读取",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::application::AppErrorCode;

    use super::{LayoutUpgradeError, WorkbookLayoutError, map_upgrade_error};

    #[test]
    fn upgrade_required_inside_the_upgrade_command_needs_a_migration_rule() {
        let error = map_upgrade_error(
            PathBuf::from("tool/workbook-layout.xlsx"),
            PathBuf::from("tool/data/workbooks/workbook-layout.updated.xlsx"),
            LayoutUpgradeError::SourceLayout(WorkbookLayoutError::UpgradeRequired {
                missing: vec!["config:template_name".to_owned()],
            }),
        );

        assert_eq!(error.code(), AppErrorCode::LayoutMigrationFailed);
        assert_eq!(
            error.context().get("missing").map(String::as_str),
            Some("config:template_name")
        );
    }

    #[test]
    fn unsupported_schema_is_an_explicit_migration_failure() {
        let error = map_upgrade_error(
            PathBuf::from("tool/workbook-layout.xlsx"),
            PathBuf::from("tool/data/workbooks/workbook-layout.updated.xlsx"),
            LayoutUpgradeError::UnsupportedSchema {
                actual: 2,
                expected: 1,
            },
        );

        assert_eq!(error.code(), AppErrorCode::LayoutMigrationFailed);
        assert_eq!(
            error.context().get("migration_schema").map(String::as_str),
            Some("2->1")
        );
    }
}

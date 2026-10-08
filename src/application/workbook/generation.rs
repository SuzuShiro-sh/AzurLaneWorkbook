//! 编排布局、完整游戏状态、稳定投影和数据工作簿生成端口。

use serde::Serialize;

use super::super::{
    AppError, AppErrorCode, GamePort, WorkbookGenerationPort, WorkbookLayout, WorkbookPort,
    WorkbookProjectionError, WorkbookProjectionV4,
};
use super::projection_mapper::project_game_state_for_layout;

/// 显式黄金采集启用时，一次成功读取附加到生成回执的脱敏捕获身份。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FullStateCaptureBinding {
    schema_version: u32,
    size_bytes: u64,
    sha256: String,
    session_id: String,
    read_index: u64,
    game_state_content_sha256: String,
}

impl FullStateCaptureBinding {
    /// 由已经排他发布并重读核对的完整捕获证据建立绑定。
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn new(
        schema_version: u32,
        size_bytes: u64,
        sha256: String,
        session_id: String,
        read_index: u64,
        game_state_content_sha256: String,
    ) -> Self {
        Self {
            schema_version,
            size_bytes,
            sha256,
            session_id,
            read_index,
            game_state_content_sha256,
        }
    }

    /// 返回完整捕获格式版本。
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 返回捕获文件的实际字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回捕获文件字节的 SHA-256。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// 返回产生该捕获的运行态会话标识。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 返回该会话内从一开始计数的读取序号。
    pub const fn read_index(&self) -> u64 {
        self.read_index
    }

    /// 返回该次捕获映射出的完整游戏状态摘要。
    pub fn game_state_content_sha256(&self) -> &str {
        &self.game_state_content_sha256
    }
}

/// 获取方式参考资料是否参与本次生成。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionGenerationState {
    #[default]
    NotRequested,
    Disabled,
    Completed,
}

/// 单个舰船名称的参考资料处理结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionGenerationOutcome {
    Updated,
    Cached,
    Missing,
    Fallback,
    Failed,
    Incomplete,
    CacheWarning,
    NotStarted,
}

/// 获取方式需检查的名称或行及原始诊断。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AcquisitionGenerationWarning {
    pub name: String,
    pub outcome: AcquisitionGenerationOutcome,
    pub detail: String,
}

/// 按唯一舰船名称汇总；静态名称缺失的行各计为一项失败。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AcquisitionGenerationSummary {
    pub state: AcquisitionGenerationState,
    pub updated: usize,
    pub cached: usize,
    pub missing: usize,
    pub fallback: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub cache_warning: usize,
    pub not_started: usize,
    pub warnings: Vec<AcquisitionGenerationWarning>,
}

impl AcquisitionGenerationSummary {
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    pub(crate) fn record(
        &mut self,
        name: &str,
        outcome: AcquisitionGenerationOutcome,
        detail: Option<String>,
    ) {
        use AcquisitionGenerationOutcome::*;
        let count = match outcome {
            Updated => &mut self.updated,
            Cached => &mut self.cached,
            Missing => &mut self.missing,
            Fallback => &mut self.fallback,
            Failed => &mut self.failed,
            Incomplete => &mut self.incomplete,
            CacheWarning => &mut self.cache_warning,
            NotStarted => &mut self.not_started,
        };
        *count += 1;
        if let Some(detail) = detail {
            self.warnings.push(AcquisitionGenerationWarning {
                name: name.to_owned(),
                outcome,
                detail,
            });
        }
    }
}

/// 一次数据工作簿经过写出、重载和排他发布后形成的稳定摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkbookGenerationReport {
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    ship_acquisition: std::collections::BTreeMap<String, String>,
    acquisition_summary: AcquisitionGenerationSummary,
    status: &'static str,
    read_scope: crate::domain::GameReadScope,
    message: &'static str,
    output_path: String,
    reused_existing: bool,
    generated_at_unix_millis: i64,
    layout_schema_version: u32,
    workbook_schema_version: u32,
    generated_sheets: usize,
    hidden_sheets: usize,
    omitted_sheets: usize,
    generated_fields: usize,
    hidden_fields: usize,
    omitted_fields: usize,
    projected_rows: usize,
    dictionary_rows: usize,
    schema_rows: usize,
    layout_content_sha256: String,
    projection_content_sha256: String,
    game_state_content_sha256: String,
    workbook_semantic_sha256: String,
    output_package_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_state_capture: Option<FullStateCaptureBinding>,
}

impl WorkbookGenerationReport {
    /// 汇总经过独立重载确认的输出路径、写入范围和全部稳定摘要。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        output_path: String,
        reused_existing: bool,
        generated_at_unix_millis: i64,
        layout_schema_version: u32,
        workbook_schema_version: u32,
        generated_sheets: usize,
        hidden_sheets: usize,
        omitted_sheets: usize,
        generated_fields: usize,
        hidden_fields: usize,
        omitted_fields: usize,
        projected_rows: usize,
        dictionary_rows: usize,
        schema_rows: usize,
        layout_content_sha256: String,
        projection_content_sha256: String,
        game_state_content_sha256: String,
        workbook_semantic_sha256: String,
        output_package_sha256: String,
    ) -> Self {
        Self {
            ship_acquisition: Default::default(),
            acquisition_summary: Default::default(),
            status: "completed",
            read_scope: crate::domain::GameReadScope::full(),
            message: if reused_existing {
                "已复用内容一致的数据工作簿"
            } else {
                "数据工作簿生成完成"
            },
            output_path,
            reused_existing,
            generated_at_unix_millis,
            layout_schema_version,
            workbook_schema_version,
            generated_sheets,
            hidden_sheets,
            omitted_sheets,
            generated_fields,
            hidden_fields,
            omitted_fields,
            projected_rows,
            dictionary_rows,
            schema_rows,
            layout_content_sha256,
            projection_content_sha256,
            game_state_content_sha256,
            workbook_semantic_sha256,
            output_package_sha256,
            full_state_capture: None,
        }
    }

    /// 外部参考列随回执保存，离线黄金核验使用生成时的快照。
    pub(crate) fn with_ship_acquisition(
        mut self,
        values: std::collections::BTreeMap<String, String>,
        summary: AcquisitionGenerationSummary,
    ) -> Self {
        if summary.has_warnings() {
            self.status = "completed_with_warnings";
            self.message = if self.reused_existing {
                "已复用数据工作簿，获取方式参考资料存在警告"
            } else {
                "数据工作簿生成完成，获取方式参考资料存在警告"
            };
        }
        self.acquisition_summary = summary;
        self.ship_acquisition = values;
        self
    }

    pub fn acquisition_summary(&self) -> &AcquisitionGenerationSummary {
        &self.acquisition_summary
    }

    pub fn has_warnings(&self) -> bool {
        self.acquisition_summary.has_warnings()
    }

    pub const fn status(&self) -> &'static str {
        self.status
    }

    pub(crate) fn with_read_scope(mut self, scope: crate::domain::GameReadScope) -> Self {
        self.read_scope = scope;
        self
    }

    pub const fn read_scope(&self) -> crate::domain::GameReadScope {
        self.read_scope
    }

    /// 将同一次成功读取产生的完整捕获身份附加到生成回执。
    pub(crate) fn with_full_state_capture(mut self, capture: FullStateCaptureBinding) -> Self {
        self.full_state_capture = Some(capture);
        self
    }

    /// 返回面向用户的生成结论。
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// 返回相对工具根目录的数据工作簿路径。
    pub fn output_path(&self) -> &str {
        &self.output_path
    }

    /// 返回是否直接复用了此前已经完整验证的同语义文件。
    pub const fn reused_existing(&self) -> bool {
        self.reused_existing
    }

    /// 返回首次生成该文件时的 Unix 毫秒时间戳。
    pub const fn generated_at_unix_millis(&self) -> i64 {
        self.generated_at_unix_millis
    }

    /// 返回生成时使用的布局 schema 版本。
    pub const fn layout_schema_version(&self) -> u32 {
        self.layout_schema_version
    }

    /// 返回数据工作簿投影 schema 版本。
    pub const fn workbook_schema_version(&self) -> u32 {
        self.workbook_schema_version
    }

    /// 返回实际写入的工作表数量。
    pub const fn generated_sheets(&self) -> usize {
        self.generated_sheets
    }

    /// 返回实际写入且隐藏的工作表数量。
    pub const fn hidden_sheets(&self) -> usize {
        self.hidden_sheets
    }

    /// 返回按布局明确省略的工作表数量。
    pub const fn omitted_sheets(&self) -> usize {
        self.omitted_sheets
    }

    /// 返回实际写入的字段数量。
    pub const fn generated_fields(&self) -> usize {
        self.generated_fields
    }

    /// 返回实际写入且隐藏的字段数量。
    pub const fn hidden_fields(&self) -> usize {
        self.hidden_fields
    }

    /// 返回按布局明确省略的字段数量。
    pub const fn omitted_fields(&self) -> usize {
        self.omitted_fields
    }

    /// 返回从游戏状态投影并写入的数据行总数。
    pub const fn projected_rows(&self) -> usize {
        self.projected_rows
    }

    /// 返回由布局合成的字典行数量。
    pub const fn dictionary_rows(&self) -> usize {
        self.dictionary_rows
    }

    /// 返回由布局合成的 schema 行数量。
    pub const fn schema_rows(&self) -> usize {
        self.schema_rows
    }

    /// 返回生成时采用的规范布局内容摘要。
    pub fn layout_content_sha256(&self) -> &str {
        &self.layout_content_sha256
    }

    /// 返回完整工作簿投影的语义摘要。
    pub fn projection_content_sha256(&self) -> &str {
        &self.projection_content_sha256
    }

    /// 返回输入完整游戏状态的语义摘要。
    pub fn game_state_content_sha256(&self) -> &str {
        &self.game_state_content_sha256
    }

    /// 返回不包含自身和生成时间的数据工作簿语义摘要。
    pub fn workbook_semantic_sha256(&self) -> &str {
        &self.workbook_semantic_sha256
    }

    /// 返回最终 XLSX 包字节的 SHA-256。
    pub fn output_package_sha256(&self) -> &str {
        &self.output_package_sha256
    }

    /// 返回同一次生成读取附带的完整捕获身份；正常生成默认不存在。
    pub const fn full_state_capture(&self) -> Option<&FullStateCaptureBinding> {
        self.full_state_capture.as_ref()
    }
}

/// 工作簿发布之后的生成终态。清理未完成仍保留已发布报告。
#[derive(Debug)]
pub enum WorkbookGenerationOutcome {
    /// 发布和会话清理都已完成。
    Completed(WorkbookGenerationReport),
    /// 工作簿已经发布，但会话清理没有完成。
    CleanupIncomplete {
        report: WorkbookGenerationReport,
        cleanup: crate::application::SessionCleanup,
    },
}

impl WorkbookGenerationOutcome {
    /// 返回已经发布的工作簿报告。
    pub fn report(&self) -> &WorkbookGenerationReport {
        match self {
            Self::Completed(report) | Self::CleanupIncomplete { report, .. } => report,
        }
    }

    /// 返回发布后的清理错误；清理完成时没有该错误。
    pub const fn cleanup_error(&self) -> Option<&AppError> {
        match self {
            Self::Completed(_) => None,
            Self::CleanupIncomplete { cleanup, .. } => cleanup.error(),
        }
    }
}

/// 严格按布局、游戏状态、投影和物化顺序生成一份全新工作簿。
#[cfg(test)]
pub(crate) fn generate_workbook(
    workbook: &dyn WorkbookPort,
    game: &mut dyn GamePort,
    generation: &dyn WorkbookGenerationPort,
    requested_name: Option<&str>,
) -> Result<WorkbookGenerationReport, AppError> {
    generate_workbook_with_progress(
        workbook,
        game,
        generation,
        requested_name,
        &mut |_| {},
        &|| false,
    )
}

/// 同步安全停止点；进度通知本身不承担控制职责。
pub(crate) fn check_generation_cancelled(
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), AppError> {
    if is_cancelled() {
        Err(AppError::from_source(
            "workbook.generate.cancelled",
            AppErrorCode::OperationCancelled,
            "同步并生成已取消，未发布工作簿",
            std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "workbook generation cancelled",
            ),
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn generate_workbook_with_progress(
    workbook: &dyn WorkbookPort,
    game: &mut dyn GamePort,
    generation: &dyn WorkbookGenerationPort,
    requested_name: Option<&str>,
    progress: &mut dyn FnMut(crate::application::OperationProgress),
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<WorkbookGenerationReport, AppError> {
    use crate::application::OperationProgress;
    progress(OperationProgress::stage("正在读取工作簿布局"));
    check_generation_cancelled(is_cancelled)?;
    let layout: WorkbookLayout = workbook.load_layout()?;
    progress(OperationProgress::stage("正在连接游戏并按模板读取状态"));
    check_generation_cancelled(is_cancelled)?;
    let observation = game.read_state_with_scope(layout.read_scope(), progress)?;
    let full_state_capture = observation.capture().cloned();
    let state = observation.state();
    if let Some(capture) = &full_state_capture
        && capture.game_state_content_sha256() != state.source().content_sha256()
    {
        return Err(AppError::from_source(
            "workbook.generate.capture",
            AppErrorCode::FullCheckFailed,
            "完整状态捕获身份与本次生成读取不一致",
            std::io::Error::other("capture game-state digest does not match the current read"),
        ));
    }
    progress(OperationProgress::stage("正在转换工作簿数据"));
    check_generation_cancelled(is_cancelled)?;
    let projection: WorkbookProjectionV4 =
        project_game_state_for_layout(state, &layout).map_err(map_projection_error)?;
    check_generation_cancelled(is_cancelled)?;
    let report = generation.generate_workbook_with_progress(
        requested_name,
        &layout,
        projection,
        progress,
        is_cancelled,
    )?;
    Ok(match full_state_capture {
        Some(capture) => report.with_full_state_capture(capture),
        None => report,
    })
}

/// 将投影失败转换为稳定工作簿错误，并保留可定位的工作表、对象和字段上下文。
fn map_projection_error(error: WorkbookProjectionError) -> AppError {
    let context = projection_error_context(&error);
    let mut application_error = AppError::from_source(
        "workbook.projection",
        AppErrorCode::WorkbookInvalid,
        "完整游戏状态未能建立有效的数据工作簿投影",
        error,
    );
    for (key, value) in context {
        application_error = application_error.with_context(key, value);
    }
    application_error
}

/// 提取不同投影错误共同具备的稳定诊断字段。
fn projection_error_context(error: &WorkbookProjectionError) -> Vec<(&'static str, String)> {
    match error {
        WorkbookProjectionError::UnknownSheet {
            sheet_key,
            object_ref,
        }
        | WorkbookProjectionError::DuplicateRowReference {
            sheet_key,
            object_ref,
        } => vec![
            ("sheet_key", sheet_key.clone()),
            ("object_ref", object_ref.clone()),
        ],
        WorkbookProjectionError::DuplicateField {
            sheet_key,
            object_ref,
            field_key,
        }
        | WorkbookProjectionError::UnknownField {
            sheet_key,
            object_ref,
            field_key,
        }
        | WorkbookProjectionError::ValueTypeMismatch {
            sheet_key,
            object_ref,
            field_key,
            ..
        }
        | WorkbookProjectionError::NonFiniteDecimal {
            sheet_key,
            object_ref,
            field_key,
        }
        | WorkbookProjectionError::InexactExcelInteger {
            sheet_key,
            object_ref,
            field_key,
            ..
        }
        | WorkbookProjectionError::DateTimeOutOfRange {
            sheet_key,
            object_ref,
            field_key,
            ..
        }
        | WorkbookProjectionError::IntegerOverflow {
            sheet_key,
            object_ref,
            field_key,
            ..
        }
        | WorkbookProjectionError::CellTextLimitExceeded {
            sheet_key,
            object_ref,
            field_key,
            ..
        } => vec![
            ("sheet_key", sheet_key.clone()),
            ("object_ref", object_ref.clone()),
            ("field_key", field_key.clone()),
        ],
        WorkbookProjectionError::MissingFields {
            sheet_key,
            object_ref,
            field_keys,
        } => vec![
            ("sheet_key", sheet_key.clone()),
            ("object_ref", object_ref.clone()),
            ("field_keys", field_keys.join(",")),
        ],
        WorkbookProjectionError::UnknownEnumeration {
            sheet_key,
            object_ref,
            field_key,
            category_key,
            stable_value,
        } => vec![
            ("sheet_key", sheet_key.clone()),
            ("object_ref", object_ref.clone()),
            ("field_key", field_key.clone()),
            ("enum_category", category_key.clone()),
            ("enum_value", stable_value.clone()),
        ],
        WorkbookProjectionError::RowLimitExceeded {
            sheet_key,
            object_ref,
            ..
        } => vec![
            ("sheet_key", sheet_key.clone()),
            ("object_ref", object_ref.clone()),
        ],
        WorkbookProjectionError::ArithmeticOverflow {
            sheet_key,
            object_ref,
            ..
        }
        | WorkbookProjectionError::JsonEncode {
            sheet_key,
            object_ref,
            ..
        } => vec![
            ("sheet_key", (*sheet_key).to_string()),
            ("object_ref", object_ref.clone()),
        ],
        WorkbookProjectionError::MissingReference {
            sheet_key,
            object_ref,
            target_type,
            target_ref,
        } => vec![
            ("sheet_key", sheet_key.clone()),
            ("object_ref", object_ref.clone()),
            ("target_type", (*target_type).to_owned()),
            ("target_ref", target_ref.clone()),
        ],
        WorkbookProjectionError::AmbiguousComposeRecipe {
            family_id,
            recipe_ids,
        } => vec![
            ("family_id", family_id.to_string()),
            (
                "recipe_ids",
                recipe_ids
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ],
        WorkbookProjectionError::Registry { .. }
        | WorkbookProjectionError::Encode { .. }
        | WorkbookProjectionError::RegistryEncode { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use crate::domain::{
        AccountResources, BagInventory, EquipmentCatalog, EquipmentCatalogSource,
        EquipmentDetailCatalog, EquipmentInventory, GameState, GameStateSource, RawRecordSet,
        ShipRoster, ShipRosterSource,
    };

    use super::{FullStateCaptureBinding, WorkbookGenerationReport, generate_workbook};
    use crate::application::{
        AppError, AppErrorCode, GamePort, LAYOUT_SCHEMA_VERSION, WorkbookGenerationPort,
        WorkbookLayout, WorkbookPort, WorkbookProjectionV4,
    };

    struct RecordingWorkbookPort {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl WorkbookPort for RecordingWorkbookPort {
        fn load_layout(&self) -> Result<WorkbookLayout, AppError> {
            self.events.lock().unwrap().push("layout");
            empty_layout()
        }
    }

    struct RecordingGamePort {
        events: Arc<Mutex<Vec<&'static str>>>,
        capture: Option<FullStateCaptureBinding>,
    }

    impl GamePort for RecordingGamePort {
        fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
            self.events.lock().unwrap().push("game");
            Ok(crate::application::GameObservation::from_shared(
                std::sync::Arc::new(empty_game_state()),
                self.capture.clone(),
            ))
        }

        fn read_state_with_scope(
            &mut self,
            scope: crate::domain::GameReadScope,
            _progress: &mut dyn FnMut(crate::application::OperationProgress),
        ) -> Result<crate::application::GameObservation, AppError> {
            assert!(!scope.ship_skill_effects(), "空布局不请求技能效果");
            self.read_full_state()
        }
    }

    struct RecordingGenerationPort {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl WorkbookGenerationPort for RecordingGenerationPort {
        fn generate_workbook(
            &self,
            requested_name: Option<&str>,
            layout: &WorkbookLayout,
            projection: WorkbookProjectionV4,
        ) -> Result<WorkbookGenerationReport, AppError> {
            self.events.lock().unwrap().push("generation");
            assert_eq!(requested_name, Some("fixture.xlsx"));
            assert_eq!(layout.template_name(), "测试布局");
            assert_eq!(projection.sheets().len(), 10);
            Ok(report(layout, &projection))
        }
    }

    struct FailingWorkbookPort;

    impl WorkbookPort for FailingWorkbookPort {
        fn load_layout(&self) -> Result<WorkbookLayout, AppError> {
            Err(AppError::from_source(
                "workbook.layout.fixture",
                AppErrorCode::LayoutInvalid,
                "测试布局无效",
                io::Error::other("fixture layout failure"),
            ))
        }
    }

    struct UncalledGamePort {
        calls: usize,
    }

    impl GamePort for UncalledGamePort {
        fn read_full_state(&mut self) -> Result<crate::application::GameObservation, AppError> {
            self.calls += 1;
            Ok(crate::application::GameObservation::from_state(
                empty_game_state(),
            ))
        }
    }

    #[test]
    fn reads_layout_before_game_and_only_then_materializes_projection() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let workbook = RecordingWorkbookPort {
            events: Arc::clone(&events),
        };
        let mut game = RecordingGamePort {
            events: Arc::clone(&events),
            capture: None,
        };
        let generation = RecordingGenerationPort {
            events: Arc::clone(&events),
        };

        let report =
            generate_workbook(&workbook, &mut game, &generation, Some("fixture.xlsx")).unwrap();

        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["layout", "game", "generation"]
        );
        assert_eq!(report.output_path(), "data/workbooks/fixture.xlsx");
    }

    #[test]
    fn layout_failure_prevents_game_access() {
        let mut game = UncalledGamePort { calls: 0 };
        let events = Arc::new(Mutex::new(Vec::new()));
        let generation = RecordingGenerationPort { events };

        let error = generate_workbook(
            &FailingWorkbookPort,
            &mut game,
            &generation,
            Some("fixture.xlsx"),
        )
        .unwrap_err();

        assert_eq!(error.code(), AppErrorCode::LayoutInvalid);
        assert_eq!(game.calls, 0);
    }

    #[test]
    fn serializes_the_generation_report_as_a_single_line_cli_contract() {
        let layout = empty_layout().unwrap();
        let projection = crate::application::project_game_state_to_workbook(&empty_game_state())
            .expect("空状态应能建立投影");
        let json = serde_json::to_string(&report(&layout, &projection)).unwrap();

        assert!(!json.contains(['\r', '\n']));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = value.as_object().expect("生成报告应编码为 JSON 对象");
        let expected_fields = [
            "acquisition_summary",
            "status",
            "message",
            "output_path",
            "reused_existing",
            "generated_at_unix_millis",
            "layout_schema_version",
            "workbook_schema_version",
            "generated_sheets",
            "hidden_sheets",
            "omitted_sheets",
            "generated_fields",
            "hidden_fields",
            "omitted_fields",
            "projected_rows",
            "dictionary_rows",
            "schema_rows",
            "layout_content_sha256",
            "projection_content_sha256",
            "game_state_content_sha256",
            "workbook_semantic_sha256",
            "output_package_sha256",
            "read_scope",
        ];
        assert_eq!(object.len(), expected_fields.len());
        for field in expected_fields {
            assert!(object.contains_key(field), "生成报告缺少字段 {field}");
        }
        assert_eq!(value["status"], "completed");
        assert_eq!(value["acquisition_summary"]["state"], "not_requested");
        assert_eq!(value["message"], "数据工作簿生成完成");
        assert_eq!(value["output_path"], "data/workbooks/fixture.xlsx");
        assert_eq!(value["reused_existing"], false);
        assert_eq!(value["generated_at_unix_millis"], 1_700_000_000_123_i64);
        assert!(value.get("full_state_capture").is_none());
    }

    #[test]
    fn attaches_the_exact_capture_identity_to_the_generation_receipt() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let workbook = RecordingWorkbookPort {
            events: Arc::clone(&events),
        };
        let state = empty_game_state();
        let capture = capture_binding(state.source().content_sha256());
        let mut game = RecordingGamePort {
            events: Arc::clone(&events),
            capture: Some(capture.clone()),
        };
        let generation = RecordingGenerationPort {
            events: Arc::clone(&events),
        };

        let report =
            generate_workbook(&workbook, &mut game, &generation, Some("fixture.xlsx")).unwrap();

        assert_eq!(report.full_state_capture(), Some(&capture));
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["layout", "game", "generation"]
        );
        let value = serde_json::to_value(&report).unwrap();
        let binding = value["full_state_capture"]
            .as_object()
            .expect("显式采集应附加捕获身份");
        assert_eq!(binding.len(), 6);
        assert_eq!(binding["schema_version"], 1);
        assert_eq!(binding["size_bytes"], 4096);
        assert_eq!(binding["sha256"], "a".repeat(64));
        assert_eq!(binding["session_id"], "b".repeat(32));
        assert_eq!(binding["read_index"], 7);
        assert_eq!(
            binding["game_state_content_sha256"],
            state.source().content_sha256()
        );
    }

    #[test]
    fn rejects_a_capture_identity_from_a_different_game_state_before_generation() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let workbook = RecordingWorkbookPort {
            events: Arc::clone(&events),
        };
        let mut game = RecordingGamePort {
            events: Arc::clone(&events),
            capture: Some(capture_binding(&"f".repeat(64))),
        };
        let generation = RecordingGenerationPort {
            events: Arc::clone(&events),
        };

        let error =
            generate_workbook(&workbook, &mut game, &generation, Some("fixture.xlsx")).unwrap_err();

        assert_eq!(error.code(), AppErrorCode::FullCheckFailed);
        assert_eq!(error.stage(), "workbook.generate.capture");
        assert_eq!(events.lock().unwrap().as_slice(), ["layout", "game"]);
    }

    fn capture_binding(game_state_content_sha256: &str) -> FullStateCaptureBinding {
        FullStateCaptureBinding::new(
            1,
            4096,
            "a".repeat(64),
            "b".repeat(32),
            7,
            game_state_content_sha256.to_owned(),
        )
    }

    fn empty_layout() -> Result<WorkbookLayout, AppError> {
        WorkbookLayout::new(
            LAYOUT_SCHEMA_VERSION,
            "测试布局".to_owned(),
            "验证生成顺序".to_owned(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "0".repeat(64),
        )
        .map_err(|source| {
            AppError::from_source(
                "workbook.layout.fixture",
                AppErrorCode::LayoutInvalid,
                "测试布局构造失败",
                source,
            )
        })
    }

    fn empty_game_state() -> GameState {
        let digest = "1".repeat(64);
        GameState::new(
            GameStateSource::new(
                "2".repeat(64),
                1,
                1,
                1,
                2,
                1,
                digest.clone(),
                digest.clone(),
                digest.clone(),
                digest.clone(),
                digest.clone(),
                digest.clone(),
            ),
            ShipRoster::new(
                ShipRosterSource::new("2".repeat(64), digest.clone()),
                Vec::new(),
            ),
            crate::application::test_support::test_ship_catalog(
                "2".repeat(64),
                digest.clone(),
                &[],
            ),
            EquipmentCatalog::new(
                EquipmentCatalogSource::new("2".repeat(64), digest.clone()),
                Vec::new(),
                Vec::new(),
                0,
            ),
            EquipmentDetailCatalog::new(Vec::new(), Vec::new()),
            EquipmentInventory::new(Vec::new()),
            BagInventory::new(Vec::new()),
            AccountResources::new(0, 0, 0),
            RawRecordSet::new(1, digest, Vec::new()),
        )
    }

    fn report(
        layout: &WorkbookLayout,
        projection: &WorkbookProjectionV4,
    ) -> WorkbookGenerationReport {
        WorkbookGenerationReport::new(
            "data/workbooks/fixture.xlsx".to_owned(),
            false,
            1_700_000_000_123,
            layout.schema_version(),
            projection.schema_version(),
            9,
            5,
            0,
            377,
            0,
            0,
            0,
            56,
            377,
            layout.content_sha256().to_owned(),
            projection.content_sha256().to_owned(),
            projection.source().game_state_content_sha256().to_owned(),
            "3".repeat(64),
            "4".repeat(64),
        )
    }
}

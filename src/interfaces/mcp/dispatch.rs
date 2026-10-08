//! MCP 参数转换、应用服务调用和完整业务结果投影。

use super::registry;
use crate::application::{
    AppError, CheckAndSaveOutcome, DiagnosticArtifactRef, DirectActionBatch, GameQuery,
    GameQueryOptions, OperationProgress, OperationTerminal, SessionCleanup, StageResult,
    UserPreferences, WorkbookExecuteOutcome, WorkbookGenerationOutcome,
};
use crate::bootstrap::{
    OperationConfiguration, OperationContext, OperationIdentity, OperationRecord, OperationSignals,
    bootstrap_check_service_with_instance, bootstrap_direct_action_service,
    bootstrap_execute_service_with_instance, bootstrap_layout_service,
    bootstrap_sync_service_with_instance, bootstrap_workspace_service_with_configuration,
    open_offline_doctor, query_game, read_history_details, read_log_details,
    update_ship_acquisition_cache_logged, verify_release,
};
#[cfg(target_os = "windows")]
use crate::{
    application::{AgentManagementAction, AgentManagementOutcome},
    bootstrap::bootstrap_agent_management,
};
use rmcp::model::{CallToolResult, Tool};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{error::Error, path::Path};

type Failure = Box<dyn Error + Send + Sync>;
type Result<T> = std::result::Result<T, Failure>;

pub(super) fn validate_arguments(
    tool: &Tool,
    args: &Map<String, Value>,
) -> std::result::Result<(), String> {
    let properties = tool.input_schema["properties"]
        .as_object()
        .expect("工具参数对象");
    for (key, value) in args {
        if !properties.contains_key(key) {
            return Err(format!(
                "工具 {} 不支持参数 {key}；可用参数：{}。请删除多余参数，按 tools/list 的 inputSchema 调用",
                tool.name,
                properties
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if properties[key]["type"] == "string" && value.as_str().is_none_or(|s| s.trim().is_empty())
        {
            return Err(format!(
                "工具 {} 的参数 {key} 必须为非空文本；{}",
                tool.name,
                properties[key]["description"]
                    .as_str()
                    .unwrap_or("请传入 JSON 字符串")
            ));
        }
    }
    for required in tool.input_schema["required"].as_array().expect("必选字段") {
        let key = required.as_str().unwrap();
        if !args.contains_key(key) {
            return Err(format!(
                "工具 {} 缺少参数 {key}；{}",
                tool.name,
                properties[key]["description"]
                    .as_str()
                    .unwrap_or("请按 tools/list 的 inputSchema 补齐该字段")
            ));
        }
    }
    if let Some(hash) = args.get("plan_hash") {
        let valid = hash.as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if !valid {
            return Err(format!(
                "工具 {} 的 plan_hash 必须为对应检查返回的 64 位小写十六进制摘要；先调用 {}，再复制 result.plan_hash，不要使用 task_id 或 result_id",
                tool.name,
                if tool.name == "workbook_execute" {
                    "workbook_check"
                } else {
                    "equipment_actions_check"
                }
            ));
        }
    }
    Ok(())
}

struct Args(Map<String, Value>);
impl Args {
    fn optional<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.0
            .get(key)
            .map(|v| {
                serde_path_to_error::deserialize(v.clone()).map_err(|e| {
                    let path = e.path().to_string();
                    let separator = if path.is_empty() || path == "." || path.starts_with('[') { "" } else { "." };
                    let path = if path == "." { "" } else { &path };
                    invalid(format!("参数 {key}{separator}{path} 格式错误：{}；请按该工具 inputSchema 的字段类型、必填项和枚举值修正", e.inner()))
                })
            })
            .transpose()
    }
    fn required<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        self.optional(key)?
            .ok_or_else(|| invalid(format!("缺少参数 {key}")))
    }
    fn default<T: DeserializeOwned + Default>(&self, key: &str) -> Result<T> {
        Ok(self.optional(key)?.unwrap_or_default())
    }
}
fn invalid(message: impl Into<String>) -> Failure {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()).into()
}
fn encoded(value: impl Serialize) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}
fn error_value(error: &(dyn Error + 'static)) -> Value {
    let mut causes = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        causes.push(cause.to_string());
        source = cause.source();
    }
    let mut result = json!({"message":error.to_string(),"causes":causes});
    if let Some(app) = error.downcast_ref::<AppError>() {
        result["stage"] = json!(app.stage());
        result["code"] = json!(app.code().to_string());
        result["context"] = json!(app.context());
    }
    result
}
fn cleanup(value: &SessionCleanup) -> Value {
    match value {
        SessionCleanup::Completed => json!({"status":"completed"}),
        SessionCleanup::Recovered(e) => json!({"status":"recovered","error":error_value(e)}),
        SessionCleanup::Failed(e) => json!({"status":"failed","error":error_value(e)}),
    }
}
fn stage<T: Serialize>(value: &StageResult<T>) -> Value {
    match value {
        StageResult::NotAttempted => json!({"status":"not_attempted"}),
        StageResult::Failed(e) => json!({"status":"failed","error":error_value(e)}),
        StageResult::Completed(v) => json!({"status":"completed","result":v}),
    }
}
fn persisted<T: Serialize>(value: &std::result::Result<T, AppError>) -> Value {
    match value {
        Ok(v) => json!({"status":"completed","result":v}),
        Err(e) => json!({"status":"failed","error":error_value(e)}),
    }
}

pub(super) fn run(
    root: &Path,
    name: &str,
    args: Map<String, Value>,
    signals: &(impl OperationSignals + crate::application::ExecutionCancellation + Sync),
) -> CallToolResult {
    let args = Args(args);
    let configuration = OperationConfiguration::load(root);
    let instance = args.0.get("instance").and_then(Value::as_str);
    let context = OperationContext::new(
        root,
        OperationIdentity {
            name,
            workbook: args.0.get("workbook").and_then(Value::as_str),
        },
        instance,
        &configuration.operation_settings(),
        signals,
    );
    let mut terminal = OperationTerminal::Succeeded;
    if let Err(error) = context.report_activity(None, format!("开始 {name}")) {
        eprintln!("MCP 进度记录失败: {error}");
    }
    let result = if signals.cancelled() {
        terminal = OperationTerminal::Cancelled;
        Err(invalid("请求已取消，尚未开始业务操作"))
    } else {
        invoke(
            root,
            name,
            &args,
            signals,
            &configuration,
            &context,
            &mut terminal,
        )
    };
    let mut output = match result {
        Ok(value) => json!({"status":terminal.status(),"result":value}),
        Err(error) => {
            terminal = if error
                .downcast_ref::<AppError>()
                .is_some_and(AppError::is_cancelled)
                || terminal == OperationTerminal::Cancelled
            {
                OperationTerminal::Cancelled
            } else {
                OperationTerminal::Failed
            };
            let mut detail = error_value(error.as_ref());
            detail["message"] = json!(format!("工具 {name}：{}", error));
            json!({"status":terminal.status(),"error":detail})
        }
    };
    let errors = context.finish(&OperationRecord {
        terminal,
        message: name.into(),
        detail: if terminal == OperationTerminal::Succeeded {
            String::new()
        } else {
            output.to_string()
        },
    });
    if !errors.is_empty() {
        output["diagnostic_errors"] = json!(errors);
    }
    if matches!(name, "equipment_actions_apply" | "workbook_generate") {
        output["workbook_snapshot"] = super::results::snapshot_notice();
    }
    if super::results::needs_summary(name) {
        output = super::results::summarize(root, name, output);
    }
    if terminal == OperationTerminal::Succeeded {
        CallToolResult::structured(output)
    } else {
        CallToolResult::structured_error(output)
    }
}

fn query(name: &str, args: &Args) -> Result<GameQuery> {
    let kind = registry::query_kind(name).ok_or_else(|| invalid("未知查询"))?;
    let ids: Vec<u64> = args.default("ids")?;
    let full: bool = args.default("full")?;
    if full && args.0.contains_key("fields") {
        return Err(invalid(
            "full=true 与 fields 不能同时使用；需要全部字段时删除 fields，需要指定字段时删除 full 或设为 false",
        ));
    }
    let mut fields: Vec<String> = if full {
        kind.fields().iter().map(|s| (*s).into()).collect()
    } else {
        args.default("fields")?
    };
    let mut options = GameQueryOptions {
        name: args.optional("name")?,
        object_type: args.optional("object_type")?,
        nation: args.optional("nation")?,
        rarity: args.optional("rarity")?,
        level_min: args.optional("level_min")?,
        level_max: args.optional("level_max")?,
        locked: args.optional("locked")?,
        fleet: args.optional("fleet")?,
        ship: args.optional("ship")?,
        slot: args.optional("slot")?,
        family: args.optional("family")?,
        location: args.optional("location")?,
        equipment: args.optional("equipment")?,
        ship_type: args.optional("ship_type")?,
        available: args.default("available")?,
        available_only: args.default("available_only")?,
        skill_level: args.optional("skill_level")?.unwrap_or(1),
        sort: args.optional("sort")?,
        descending: args.default("descending")?,
        limit: args.optional("limit")?,
        offset: args.default("offset")?,
    };
    if options.descending && options.sort.is_none() {
        return Err(invalid(
            "descending=true 需要 sort；指定排序字段，或将 descending 设为 false",
        ));
    }
    if name == "equipment" && options.slot.is_some() && options.ship.is_none() {
        return Err(invalid(
            "equipment 的 slot 需要同时提供 ship；ship 使用 ships 返回的 ship_id，slot 使用该船的 slot_index（1 至 5）",
        ));
    }
    if name == "catalog_skills" && (ids.is_empty() || options.skill_level == 0) {
        return Err(invalid(
            "catalog_skills 需要非空 ids（skill_id 数组）与正整数 skill_level；skill_level 可省略，默认 1",
        ));
    }
    if options
        .location
        .as_deref()
        .is_some_and(|s| !matches!(s, "warehouse" | "equipped"))
    {
        return Err(invalid(
            "location 必须为 warehouse（仓库有库存）或 equipped（有舰上挂载）；省略时不按位置筛选",
        ));
    }
    options.available |= options.available_only;
    if name == "recipes" && options.available && fields.is_empty() {
        fields = kind
            .default_fields()
            .iter()
            .map(|s| (*s).into())
            .chain(std::iter::once("max_count".into()))
            .collect();
    }
    GameQuery::new(kind, ids, fields)
        .and_then(|q| q.with_options(options))
        .map_err(invalid)
}

fn invoke(
    root: &Path,
    name: &str,
    args: &Args,
    signals: &(impl OperationSignals + crate::application::ExecutionCancellation + Sync),
    configuration: &OperationConfiguration,
    context: &OperationContext<'_>,
    terminal: &mut OperationTerminal,
) -> Result<Value> {
    let instance: Option<String> = args.optional("instance")?;
    let instance = instance.as_deref();
    let related = context.related_logs();
    let mut progress = |p: OperationProgress| {
        if let Err(error) = context.report_activity(p.units, p.message) {
            eprintln!("MCP 进度记录失败: {error}");
        }
    };
    if registry::query_kind(name).is_some() {
        return encoded(query_game(
            root,
            instance,
            related.as_ref(),
            configuration,
            &query(name, args)?,
        )?);
    }
    match name {
        "equipment_actions_check" | "equipment_actions_apply" => {
            let batch = DirectActionBatch {
                actions: args.required("actions")?,
            };
            let hash: Option<String> = args.optional("plan_hash")?;
            let mut service =
                bootstrap_direct_action_service(root, instance, related.as_ref(), configuration)?;
            let outcome = if name == "equipment_actions_apply" {
                service.apply_checked(
                    &batch,
                    hash.as_deref().ok_or_else(|| invalid("缺少 plan_hash"))?,
                    signals,
                    &mut progress,
                )?
            } else {
                service.check_with_progress(&batch, &mut progress)?
            };
            *terminal = outcome.log_terminal();
            Ok(
                json!({"plan_hash":outcome.check.plan().content_sha256(),"check":outcome.check,"execution":outcome.execution,"cleanup":cleanup(&outcome.cleanup)}),
            )
        }
        "agent_status" | "agent_inject" | "agent_unload" => {
            #[cfg(target_os = "windows")]
            {
                let action = match name {
                    "agent_status" => AgentManagementAction::Status,
                    "agent_inject" => AgentManagementAction::Inject,
                    _ => AgentManagementAction::Unload,
                };
                let outcome = bootstrap_agent_management(root, related.as_ref(), configuration)
                    .manage(instance, action)?;
                if matches!(outcome, AgentManagementOutcome::NeedsAttention(_)) {
                    *terminal = OperationTerminal::Failed;
                }
                encoded(outcome.report())
            }
            #[cfg(not(target_os = "windows"))]
            {
                Err(invalid("代理管理需要 Windows 设备运行环境"))
            }
        }
        "workbook_generate" => {
            let output_name: Option<String> = args.optional("name")?;
            let outcome = bootstrap_sync_service_with_instance(
                root,
                instance,
                related.as_ref(),
                configuration,
            )?
            .generate_workbook_with_progress(
                output_name.as_deref(),
                &mut progress,
                &|| signals.cancelled(),
            )?;
            if outcome.cleanup_error().is_some() || outcome.report().has_warnings() {
                *terminal = OperationTerminal::Incomplete;
            }
            match outcome {
                WorkbookGenerationOutcome::Completed(report) => encoded(report),
                WorkbookGenerationOutcome::CleanupIncomplete { report, cleanup: c } => {
                    Ok(json!({"report":report,"cleanup":cleanup(&c)}))
                }
            }
        }
        "workbook_check" | "workbook_check_save" | "workbook_execute" | "workbook_open" => {
            let workspace = bootstrap_workspace_service_with_configuration(root, configuration)?;
            let workbook_name: String = args.required("workbook")?;
            let workbook = workspace.select_workbook(&workbook_name)?;
            match name {
                "workbook_open" => encoded(workspace.open_workbook(&workbook)?),
                "workbook_check" => {
                    let report = bootstrap_check_service_with_instance(
                        root,
                        instance,
                        related.as_ref(),
                        configuration,
                    )?
                    .check_workbook_plan_with_progress(&workbook, &mut progress)?;
                    Ok(json!({"plan_hash":report.plan().content_sha256(),"check":report}))
                }
                "workbook_check_save" => {
                    let outcome = bootstrap_check_service_with_instance(
                        root,
                        instance,
                        related.as_ref(),
                        configuration,
                    )?
                    .check_and_save_workbook_plan_with_progress(&workbook, &mut progress)?;
                    *terminal = outcome.log_terminal();
                    match outcome {
                        CheckAndSaveOutcome::Saved(report) => {
                            let filename = Path::new(report.relative_path())
                                .file_name()
                                .and_then(|s| s.to_str())
                                .ok_or_else(|| invalid("历史报告文件名无效"))?;
                            match read_history_details(root, filename, None) {
                                Ok(history) => Ok(
                                    json!({"plan_hash":report.plan_content_sha256(),"history":report,"details":history}),
                                ),
                                Err(error) => {
                                    *terminal = OperationTerminal::Incomplete;
                                    Ok(
                                        json!({"history":report,"details_error":error_value(&error)}),
                                    )
                                }
                            }
                        }
                        CheckAndSaveOutcome::CheckFailed { error, writeback } => Ok(
                            json!({"error":error_value(&error),"writeback":persisted(&writeback)}),
                        ),
                        CheckAndSaveOutcome::SaveIncomplete {
                            report,
                            history,
                            writeback,
                        } => Ok(
                            json!({"plan_hash":report.plan().content_sha256(),"check":report,"history":persisted(&history),"writeback":persisted(&writeback)}),
                        ),
                        CheckAndSaveOutcome::ReadWithoutCheck { cleanup: c } => {
                            Ok(json!({"cleanup":cleanup(&c)}))
                        }
                    }
                }
                _ => {
                    let hash: String = args.required("plan_hash")?;
                    let outcome = bootstrap_execute_service_with_instance(
                        root,
                        instance,
                        related.as_ref(),
                        configuration,
                    )?
                    .execute_workbook_checked(
                        &workbook,
                        Some(&hash),
                        signals,
                        &mut progress,
                    )?;
                    *terminal = outcome.log_terminal();
                    match outcome {
                        WorkbookExecuteOutcome::Completed(report) => encoded(report),
                        WorkbookExecuteOutcome::NoModifications => {
                            Ok(json!({"no_modifications":true}))
                        }
                        WorkbookExecuteOutcome::Finished {
                            execution,
                            backup,
                            history,
                            writeback,
                            cleanup: c,
                        } => Ok(
                            json!({"execution":execution,"backup":backup,"history":stage(&history),"writeback":stage(&writeback),"cleanup":cleanup(&c)}),
                        ),
                    }
                }
            }
        }
        "layout_check" => encoded(bootstrap_layout_service(root)?.check_layout()?),
        "layout_upgrade" => encoded(bootstrap_layout_service(root)?.upgrade_layout()?),
        "layout_preview" => encoded(bootstrap_layout_service(root)?.preview_layout()?),
        "doctor" => encoded(open_offline_doctor(root)?.diagnose()?),
        "release_verify" => encoded(verify_release(root)?),
        "acquisition_update" => {
            let workbook: String = args.required("workbook")?;
            let report = update_ship_acquisition_cache_logged(
                root,
                &workbook,
                args.default("mode")?,
                context,
            )
            .map_err(invalid)?;
            *terminal = crate::application::AcquisitionCacheUpdate::operation_terminal(&report);
            encoded(report)
        }
        "history_show" => read_history_details(
            root,
            &args.required::<String>("filename")?,
            args.optional::<Vec<String>>("fields")?.as_deref(),
        )
        .map_err(Into::into),
        "logs_show" => read_log_details(
            root,
            &args.required::<String>("filename")?,
            args.optional("tail")?,
            args.optional::<String>("status")?.as_deref(),
        )
        .map_err(Into::into),
        _ => {
            let workspace = bootstrap_workspace_service_with_configuration(root, configuration)?;
            match name {
                "instances" => encoded(workspace.list_emulator_instances()?),
                "workbooks_list" => encoded(workspace.list_workbooks()?),
                "settings_get" => encoded(workspace.read_settings()?),
                "preferences_get" => encoded(workspace.read_preferences()?),
                "preferences_update" => {
                    let original: UserPreferences = args.required("original")?;
                    let preferences: UserPreferences = args.required("preferences")?;
                    for key in ["original", "preferences"] {
                        if args.0[key].as_object().is_none_or(|v| v.len() != 4) {
                            return Err(invalid(format!(
                                "{key} 必须提供全部四项偏好：ship_acquisition_enabled、acquisition_update_policy、detailed_diagnostics、unload_after_sync；先用 preferences_get 读取完整对象"
                            )));
                        }
                    }
                    workspace.save_preferences(original, preferences)?;
                    encoded(preferences)
                }
                "history_list" => encoded(workspace.list_history(&|| signals.cancelled())?),
                "logs_list" => encoded(workspace.list_logs(&|| signals.cancelled())?),
                "diagnostic_open" => {
                    let filename: String = args.required("filename")?;
                    let matches = |relative: &str| {
                        relative == filename
                            || Path::new(relative).file_name().and_then(|p| p.to_str())
                                == Some(filename.as_str())
                    };
                    let source = match args.required::<String>("kind")?.as_str() {
                        "history" => workspace
                            .list_history(&|| signals.cancelled())?
                            .entries()
                            .iter()
                            .find(|e| matches(e.relative_path()))
                            .map(DiagnosticArtifactRef::from_history),
                        "log" => workspace
                            .list_logs(&|| signals.cancelled())?
                            .entries()
                            .iter()
                            .find(|e| matches(e.relative_path()))
                            .map(DiagnosticArtifactRef::from_log),
                        _ => return Err(invalid("kind 必须为 history 或 log")),
                    }
                    .ok_or_else(|| invalid(format!("diagnostic_open 找不到 filename={filename}；请先调用与 kind 对应的 history_list 或 logs_list，再复制返回的文件名")))?;
                    workspace.open_diagnostic(&source)?;
                    Ok(json!({"opened":source.relative_path()}))
                }
                _ => Err(invalid(format!("未知工具 {name}"))),
            }
        }
    }
}

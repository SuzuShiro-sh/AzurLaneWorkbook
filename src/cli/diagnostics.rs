//! 将 CLI 边界错误格式化为稳定诊断与可执行恢复建议。

use std::collections::BTreeMap;
use std::error::Error;

use azur_lane_workbook::adapters::release::ReleaseError;
use azur_lane_workbook::adapters::tool_root::ToolRootError;
use azur_lane_workbook::application::{AppError, AppErrorCode, SessionCleanup, StageResult};
use azur_lane_workbook::bootstrap::BootstrapError;

pub(crate) fn format_doctor_tool_root_error(error: &ToolRootError) -> String {
    format!(
        "doctor 检查失败 [APPLICATION_INITIALIZATION_FAILED]\n阶段：doctor.tool_root\n说明：工具根目录未通过普通目录校验\n原因：{}\n修复建议：请将程序放在完整且没有链接重定向的发布目录后重新运行 doctor。",
        doctor_tool_root_detail(error)
    )
}

fn doctor_tool_root_detail(error: &ToolRootError) -> String {
    match error {
        ToolRootError::InvalidRelativePath { message, .. }
        | ToolRootError::UnsafePath { message, .. }
        | ToolRootError::PathConflict { message, .. } => message.clone(),
        ToolRootError::Io { operation, .. } => format!("{operation}失败"),
    }
}

pub(crate) fn format_doctor_error(error: &AppError) -> String {
    match error.context().get("doctor_step").map(String::as_str) {
        Some("settings") => format!(
            "doctor 检查失败 [SETTINGS_INVALID]\n阶段：doctor.settings\n说明：{}\n原因：{}\n修复建议：请修复发布目录内 settings.json 的结构和路径配置后重新运行 doctor。",
            error.message(),
            error
                .context()
                .get("detail")
                .map(String::as_str)
                .unwrap_or("设置未能读取")
        ),
        Some("registry") => format!(
            "doctor 检查失败 [APPLICATION_INITIALIZATION_FAILED]\n阶段：doctor.layout_registry\n说明：{}\n原因：{}\n修复建议：请重新取得与程序版本匹配的完整发布包后重新运行 doctor。",
            error.message(),
            error
                .source()
                .map(ToString::to_string)
                .unwrap_or_else(|| "注册表无效".to_owned())
        ),
        Some("layout_path") => format!(
            "doctor 检查失败 [LAYOUT_INVALID]\n阶段：doctor.layout\n说明：{}\n原因：{}\n修复建议：请重新取得完整发布目录后重新运行 doctor。",
            error.message(),
            error
                .context()
                .get("detail")
                .map(String::as_str)
                .unwrap_or("布局文件不可读")
        ),
        Some("release") => {
            let mut lines = vec![
                format!("doctor 检查失败 [{}]", error.code()),
                format!("阶段：{}", error.stage()),
                format!("说明：{}", error.message()),
                format!(
                    "原因：{}",
                    error
                        .context()
                        .get("detail")
                        .map(String::as_str)
                        .unwrap_or("发布校验失败")
                ),
            ];
            for key in [
                "component",
                "field",
                "missing",
                "unexpected",
                "actual",
                "expected",
            ] {
                if let Some(value) = error.context().get(key) {
                    lines.push(format!("{}：{value}", doctor_context_label(key)));
                }
            }
            lines.push("修复建议：请重新取得完整发布目录后重新运行 doctor。".to_owned());
            lines.join("\n")
        }
        _ => format_doctor_layout_error(error),
    }
}

pub(crate) fn format_doctor_layout_error(error: &AppError) -> String {
    let mut lines: Vec<String> = vec![
        format!("doctor 检查失败 [{}]", error.code()),
        format!("阶段：{}", error.stage()),
        format!("说明：{}", error.message()),
    ];
    for key in [
        "sheet", "row", "key", "actual", "expected", "missing", "part",
    ] {
        if let Some(value) = error.context().get(key) {
            lines.push(format!("{}：{value}", doctor_context_label(key)));
        }
    }
    lines.push("修复建议：请修复 workbook-layout.xlsx 后重新运行 doctor。".to_owned());
    lines.join("\n")
}

fn doctor_context_label(key: &str) -> &'static str {
    match key {
        "sheet" => "工作表",
        "row" => "配置行",
        "key" => "稳定键",
        "actual" => "当前值",
        "expected" => "期望值",
        "missing" => "缺少文件",
        "unexpected" => "未登记文件",
        "part" => "OOXML 部件",
        "component" => "组件",
        "field" => "字段",
        _ => "上下文",
    }
}

/// 将应用组合错误格式化为稳定分类、完整原因和可执行恢复动作。
pub(crate) fn format_layout_check_bootstrap_error(error: &BootstrapError) -> String {
    format_bootstrap_error("布局检查", error)
}

pub(crate) fn format_bootstrap_error(operation: &str, error: &BootstrapError) -> String {
    format!(
        "{operation}初始化失败 [{}]\n说明：{error}\n修复建议：{}",
        error.code(),
        bootstrap_suggestion(error)
    )
}

/// 根据初始化失败边界给出与当前版本能力一致的恢复动作。
fn bootstrap_suggestion(error: &BootstrapError) -> &'static str {
    match error {
        BootstrapError::ToolRoot { .. } => "请确认程序位于完整且没有链接重定向的发布目录。",
        BootstrapError::WorkbookLayoutRegistry { .. } => {
            "请重新取得当前版本的完整发布包后再运行当前命令。"
        }
    }
}

/// 将布局应用错误展开为包含对象位置、原因链和修复动作的中文诊断。
pub(crate) fn format_layout_check_error(error: &AppError) -> String {
    format_operation_error("布局检查失败", error, layout_check_suggestion)
}

pub(crate) fn format_layout_upgrade_error(error: &AppError) -> String {
    format_operation_error("布局升级失败", error, layout_upgrade_suggestion)
}

pub(crate) fn format_layout_preview_error(error: &AppError) -> String {
    format_operation_error("布局预览失败", error, layout_preview_suggestion)
}

pub(crate) fn format_generation_error(error: &AppError) -> String {
    format_operation_error("工作簿生成失败", error, generation_suggestion)
}

/// 已发布工作簿的会话清理，只区分恢复完成和尚未完成。
#[derive(Clone, Copy)]
enum PublishedCleanupKind {
    Recovered,
    Incomplete,
}

/// 工作簿已经发布，但会话清理没有完成。调用方只在该终态使用本格式。
pub(crate) fn format_published_generation_cleanup(cleanup: &SessionCleanup) -> String {
    let Some(error) = cleanup.error() else {
        return "工作簿生成失败\n说明：已发布终态缺少会话清理错误".to_owned();
    };
    let kind = match cleanup {
        SessionCleanup::Recovered(_) => PublishedCleanupKind::Recovered,
        SessionCleanup::Failed(_) | SessionCleanup::Completed => PublishedCleanupKind::Incomplete,
    };
    format_operation_error("工作簿生成失败", error, move |_code, context| {
        published_generation_suggestion(kind, context)
    })
}

pub(crate) fn format_workbook_check_error(error: &AppError) -> String {
    let path_label = if error.stage() == "workbook.layout.load" {
        "布局文件"
    } else {
        "工作簿文件"
    };
    format_operation_error_with_path_label(
        "工作簿检查失败",
        error,
        path_label,
        if error.stage() == "check.workbook.writeback" {
            check_results_writeback_suggestion
        } else {
            workbook_check_suggestion
        },
    )
}

fn format_check_history_error(error: &AppError) -> String {
    format_operation_error_with_path_label(
        "工作簿检查记录保存失败",
        error,
        "历史文件",
        check_history_suggestion,
    )
}

pub(crate) fn format_check_save_incomplete(
    outcome: azur_lane_workbook::application::CheckAndSaveOutcome,
) -> String {
    use azur_lane_workbook::application::CheckAndSaveOutcome;
    match outcome {
        CheckAndSaveOutcome::Saved(report) => report.message().to_owned(),
        CheckAndSaveOutcome::CheckFailed { error, writeback } => {
            let check = format_check_and_save_error(error);
            match writeback {
                Ok(()) => format!("{check}\n检查结果已写入工作簿"),
                Err(writeback) => format!(
                    "{check}\n{}",
                    format_operation_error_with_path_label(
                        "检查结果写回失败",
                        &writeback,
                        "工作簿文件",
                        check_results_writeback_suggestion,
                    )
                ),
            }
        }
        CheckAndSaveOutcome::SaveIncomplete {
            history, writeback, ..
        } => match (history, writeback) {
            (Ok(history), Err(error)) => format!(
                "计划检查已通过，记录 {} 已保存，但结果写回失败\n{}",
                history.relative_path(),
                format_operation_error_with_path_label(
                    "检查结果写回失败",
                    &error,
                    "工作簿文件",
                    check_results_writeback_suggestion,
                )
            ),
            (Err(error), Ok(())) => format!(
                "计划检查已通过，结果已写回，但历史保存失败\n{}",
                format_check_history_error(&error)
            ),
            (Err(history), Err(writeback)) => format!(
                "计划检查已通过，但历史保存和结果写回都失败\n{}\n{}",
                format_check_history_error(&history),
                format_operation_error_with_path_label(
                    "检查结果写回失败",
                    &writeback,
                    "工作簿文件",
                    check_results_writeback_suggestion,
                )
            ),
            (Ok(history), Ok(())) => history.message().to_owned(),
        },
        CheckAndSaveOutcome::ReadWithoutCheck { cleanup } => {
            let Some(error) = cleanup.error() else {
                return "已读取游戏状态，但会话清理未完成，本次没有开始检查".to_owned();
            };
            format!(
                "已读取游戏状态，本次没有开始检查\n{}",
                format_operation_error_with_path_label(
                    "会话清理未完成",
                    error,
                    "工作簿文件",
                    workbook_check_suggestion,
                )
            )
        }
    }
}

pub(crate) fn format_check_and_save_error(error: AppError) -> String {
    if error.stage() == "check.history.save" {
        format_check_history_error(&error)
    } else {
        format_workbook_check_error(&error)
    }
}

pub(crate) fn format_workbook_catalog_error(error: &AppError) -> String {
    format_operation_error_with_path_label(
        "工作簿目录读取失败",
        error,
        "工作簿目录",
        workbook_catalog_suggestion,
    )
}

pub(crate) fn format_history_catalog_error(error: AppError) -> String {
    format_operation_error_with_path_label(
        "历史目录读取失败",
        &error,
        "历史目录",
        history_catalog_suggestion,
    )
}

/// 操作日志没有留下时的诊断。业务 stdout 和退出码不因这条说明改变。
pub(crate) fn format_operation_log_failure(error: &str) -> String {
    format!(
        "操作日志失败\n说明：{error}\n修复建议：请确认 data/logs 是可写的普通目录。业务结果仍然有效，日志失败不会代替它。"
    )
}

pub(crate) fn format_settings_error(error: AppError) -> String {
    let title = if error.stage() == "settings.save" {
        "设置保存失败"
    } else {
        "运行设置读取失败"
    };
    format_operation_error_with_path_label(title, &error, "设置文件", settings_summary_suggestion)
}

pub(crate) fn format_log_catalog_error(error: AppError) -> String {
    format_operation_error_with_path_label(
        "日志目录读取失败",
        &error,
        "日志目录",
        log_catalog_suggestion,
    )
}

pub(crate) fn format_execution_error(error: &AppError) -> String {
    format_operation_error_with_path_label(
        "工作簿执行失败",
        error,
        "工作簿文件",
        execution_suggestion,
    )
}

/// 直接游戏命令保留底层原因，便于区分连接、驻留版本和业务校验失败。
pub(crate) fn format_game_operation_error(title: &str, error: &AppError) -> String {
    format_operation_error(title, error, |_, _| {
        "请按上述原因处理后重试；若提示驻留代理版本不一致，请用原安装卸载代理，或重启游戏并重新登录后再运行命令。".to_owned()
    })
}

/// 将默认程序启动失败整理为稳定、可行动的诊断，并声明工作簿未被修改。
pub(crate) fn format_workbook_open_error(error: AppError) -> String {
    format_operation_error_with_path_label(
        "工作簿打开失败",
        &error,
        "工作簿文件",
        |_, _| "请确认系统已安装可打开 .xlsx 的程序后重新运行 open；工作簿内容未修改。".to_owned(),
    )
}

/// 将应用层工作簿选择错误整理成保持路径边界的稳定诊断。
pub(crate) fn format_workbook_selection_app_error(
    operation: &str,
    command: &str,
    error: &AppError,
) -> String {
    let mut lines: Vec<String> = vec![
        format!("{operation}失败 [{}]", error.code()),
        format!("阶段：{}", error.stage()),
        format!("说明：{}", error.message()),
    ];
    if let Some(path) = error.context().get("path") {
        lines.push(format!("工作簿目录：{path}"));
    }
    if let Some(name) = error.context().get("workbook_name") {
        lines.push(format!("工作簿名称：{name}"));
    }
    if let Some(causes) = error_cause_summary(error) {
        lines.push(format!("原因：{causes}"));
    }
    lines.push(format!(
        "修复建议：请将普通 .xlsx 文件放入程序目录的 data/workbooks，并只传入文件名后重新运行 {command}。"
    ));
    lines.join("\n")
}

/// 将发布校验错误整理成脚本和用户都能稳定识别的多行诊断。
pub(crate) fn format_release_error(error: &ReleaseError) -> String {
    const CONTEXT_LABELS: [(&str, &str); 9] = [
        ("path", "路径"),
        ("field", "字段"),
        ("component", "组件"),
        ("operation", "操作"),
        ("actual", "当前值"),
        ("expected", "期望值"),
        ("missing", "缺少文件"),
        ("unexpected", "未登记文件"),
        ("source_code", "底层错误码"),
    ];
    let context: BTreeMap<String, String> = error.context();
    let detail: String = error.to_string();
    let mut lines: Vec<String> = vec![
        format!("发布校验失败 [{}]", error.code()),
        format!("阶段：{}", error.stage()),
        format!("说明：{detail}"),
    ];
    for (key, label) in CONTEXT_LABELS {
        if let Some(value) = context.get(key) {
            lines.push(format!("{label}：{value}"));
        }
    }
    for (key, value) in &context {
        if !CONTEXT_LABELS
            .iter()
            .any(|(known_key, _)| *known_key == key)
        {
            lines.push(format!("上下文 {key}：{value}"));
        }
    }
    if let Some(causes) = error_cause_summary(error)
        && causes != detail
    {
        lines.push(format!("原因：{causes}"));
    }
    lines.push(format!("修复建议：{}", release_suggestion(error.code())));
    lines.join("\n")
}

/// 根据发布错误分类给出不改变既有文件的恢复动作。
fn release_suggestion(code: &str) -> &'static str {
    match code {
        "MANIFEST_INVALID" => {
            "请重新取得完整发布目录，确认 manifest.json、清单文件和文件摘要未被修改后重新运行 verify-release。"
        }
        "SETTINGS_INVALID" => {
            "请修复发布目录内 settings.json 的结构和路径配置后重新运行 verify-release。"
        }
        "RUNTIME_INCOMPATIBLE" => "请使用与当前程序版本匹配的 runtime profile 后重新装配发布目录。",
        "APPLICATION_INITIALIZATION_FAILED" => {
            "请确认程序位于完整且可访问的发布目录，再重新运行 verify-release。"
        }
        _ => "请根据“说明”和“原因”修复发布目录后重新运行 verify-release。",
    }
}

fn format_operation_error(
    title: &str,
    error: &AppError,
    suggestion: impl Fn(AppErrorCode, &BTreeMap<String, String>) -> String,
) -> String {
    format_operation_error_with_path_label(title, error, "布局文件", suggestion)
}

const OPERATION_CONTEXT_LABELS: &[(&str, &str)] = &[
    ("output_path", "输出文件"),
    ("source_path", "源工作簿"),
    ("backup_path", "执行前备份"),
    ("source_package_sha256", "源工作簿摘要"),
    ("backup_package_sha256", "备份工作簿摘要"),
    ("sheet", "工作表"),
    ("row", "配置行"),
    ("field", "字段"),
    ("key", "稳定键"),
    ("actual", "当前值"),
    ("expected", "期望值"),
    ("missing", "缺少注册项"),
    ("part", "OOXML 部件"),
    ("relationship_id", "关系编号"),
    ("target", "外部目标"),
    ("deprecated", "缺少迁移规则的稳定项"),
    ("unsupported_parts", "不支持迁移的部件"),
    ("migration_schema", "缺少迁移规则的 schema"),
    ("source_changed", "源文件在发布前发生变化"),
    ("output_digest_mismatch", "升级产物摘要不一致"),
    ("component", "组件"),
    ("missing_port", "缺少端口"),
    ("ship_instance_id", "舰船实例"),
    ("family_id", "装备族"),
    ("config_id", "装备配置"),
    ("source_policy", "来源策略"),
    ("source", "装备来源"),
    ("source_slot", "来源槽位"),
    ("slot", "目标槽位"),
    ("equipment_type_id", "装备类型"),
    ("ship_type_id", "舰种"),
    ("allowed_equipment_type_ids", "槽位允许装备类型"),
    ("forbidden_ship_type_ids", "装备禁用舰种"),
    ("actual_family_id", "实际装备族"),
    ("expected_family_id", "目标装备族"),
    ("available", "可用数量"),
    ("required", "需要数量"),
    ("action", "库存动作"),
    ("reason", "限制原因"),
    ("execution_completed", "执行已结束"),
    ("report_content_sha256", "执行报告摘要"),
    ("report_status", "执行报告状态"),
    ("may_have_writes", "可能已写入游戏"),
    ("history_path", "历史文件"),
    ("check_passed", "计划检查已通过"),
    ("check_results_written", "检查结果已写入"),
    ("check_results_writeback_error", "检查结果写回错误"),
    ("target_level", "目标强化等级"),
    ("source_level", "来源强化等级"),
    ("quantity", "数量"),
];

fn format_operation_error_with_path_label(
    title: &str,
    error: &AppError,
    path_label: &str,
    suggestion: impl Fn(AppErrorCode, &BTreeMap<String, String>) -> String,
) -> String {
    let context: &BTreeMap<String, String> = error.context();
    let mut lines: Vec<String> = vec![
        format!("{title} [{}]", error.code()),
        format!("阶段：{}", error.stage()),
        format!("说明：{}", error.message()),
    ];
    if let Some(path) = context.get("path") {
        lines.push(format!("{path_label}：{path}"));
    }
    for &(key, label) in OPERATION_CONTEXT_LABELS {
        if let Some(value) = context.get(key) {
            lines.push(format!("{label}：{value}"));
        }
    }
    for (key, value) in context {
        if key != "path"
            && !OPERATION_CONTEXT_LABELS
                .iter()
                .any(|(known_key, _)| *known_key == key)
        {
            lines.push(format!("上下文 {key}：{value}"));
        }
    }
    if let Some(causes) = error_cause_summary(error) {
        lines.push(format!("原因：{causes}"));
    }
    lines.push(format!("修复建议：{}", suggestion(error.code(), context)));
    lines.join("\n")
}

/// 按顺序展开底层错误并去掉透明错误包装产生的相邻重复文本。
fn error_cause_summary(error: &(dyn Error + 'static)) -> Option<String> {
    let mut messages: Vec<String> = Vec::new();
    let mut current: Option<&(dyn Error + 'static)> = error.source();
    while let Some(source) = current {
        let message: String = source.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
        current = source.source();
    }
    (!messages.is_empty()).then(|| messages.join("；"))
}

/// 根据稳定错误码和定位上下文给出不会隐藏原始问题的下一步动作。
fn layout_check_suggestion(code: AppErrorCode, context: &BTreeMap<String, String>) -> String {
    if code == AppErrorCode::LayoutUpgradeRequired {
        return "请保留当前布局且不要覆盖用户设置，运行 layout-upgrade 生成独立升级文件，再对新文件执行检查。"
            .to_owned();
    }
    if context.contains_key("expected") {
        return "请按“期望值”修正对应配置行，然后重新运行 layout-check。".to_owned();
    }
    if context.contains_key("sheet") || context.contains_key("row") || context.contains_key("key") {
        return "请根据工作表、配置行和稳定键修正该项，然后重新运行 layout-check。".to_owned();
    }
    "请根据“原因”修复布局文件结构，然后重新运行 layout-check。".to_owned()
}

/// 根据迁移失败边界说明如何保留源文件并处理独立输出。
fn layout_upgrade_suggestion(code: AppErrorCode, context: &BTreeMap<String, String>) -> String {
    if code != AppErrorCode::LayoutMigrationFailed {
        return layout_check_suggestion(code, context);
    }
    if context.contains_key("deprecated")
        || context.contains_key("migration_schema")
        || context.contains_key("missing")
    {
        return "请保留源布局，使用当前默认布局重新同步生成工作簿；布局升级只补齐当前契约内缺少的注册项。"
            .to_owned();
    }
    if context.contains_key("unsupported_parts") {
        return "请保留源布局，并从受控布局副本中移除列出的附加部件后重试；程序不会静默丢弃这些内容。"
            .to_owned();
    }
    if context.contains_key("source_changed") {
        return "请先停止编辑源布局，确认文件稳定后重新运行 layout-upgrade；本次没有发布输出。"
            .to_owned();
    }
    if context.contains_key("output_digest_mismatch") {
        return "请保留源布局并重新取得完整程序文件后重试；本次生成内容未通过摘要核对，也没有发布输出。"
            .to_owned();
    }
    "请保留源布局；若独立输出文件已经存在，请先完成核对并移走该文件，然后重新运行 layout-upgrade。"
        .to_owned()
}

/// 根据布局读取、产物验证或文件占用边界给出不会破坏旧预览的恢复动作。
fn layout_preview_suggestion(code: AppErrorCode, context: &BTreeMap<String, String>) -> String {
    if code == AppErrorCode::WorkbookLocked {
        return "请关闭占用 layout-preview.xlsx 的 Excel 窗口后重新运行 layout-preview；已有预览保持不变。"
            .to_owned();
    }
    if matches!(
        code,
        AppErrorCode::LayoutInvalid | AppErrorCode::LayoutUpgradeRequired
    ) {
        return format!(
            "{} 修复或升级根布局后重新运行 layout-preview；已有预览保持不变。",
            layout_check_suggestion(code, context)
        );
    }
    if code == AppErrorCode::ApplicationInitializationFailed {
        return "请确认 data/workbooks 是工具目录内可写的普通目录，并移走同名冲突项后重新运行 layout-preview；已有预览保持不变。"
            .to_owned();
    }
    "请根据“原因”修复布局或程序文件后重新运行 layout-preview；未通过验证的临时文件不会替换已有预览。"
        .to_owned()
}

fn published_generation_suggestion(
    cleanup: PublishedCleanupKind,
    context: &BTreeMap<String, String>,
) -> String {
    let output = context
        .get("output_path")
        .map(String::as_str)
        .unwrap_or("已发布的工作簿");
    match cleanup {
        PublishedCleanupKind::Recovered => format!(
            "工作簿已经生成：{output}，但游戏运行态正常卸载失败，恢复清理已完成；请保留输出文件并核对运行态日志，禁止直接重跑 generate。"
        ),
        PublishedCleanupKind::Incomplete => format!(
            "工作簿已经生成：{output}，但游戏会话清理未完整确认；请保留输出文件，根据运行态日志核对游戏进程和残留状态，禁止直接重跑 generate。"
        ),
    }
}

fn generation_suggestion(code: AppErrorCode, context: &BTreeMap<String, String>) -> String {
    match code {
        AppErrorCode::GameNotReady
            if context
                .get("missing_port")
                .is_some_and(|port| port == "game") =>
        {
            "请先完成已认证的游戏运行态连接，再运行 generate；本次没有读取游戏状态或写入工作簿。"
                .to_owned()
        }
        AppErrorCode::GameNotReady => {
            "请根据“原因”恢复游戏运行态后重新运行 generate；本次未发布工作簿。".to_owned()
        }
        AppErrorCode::EmulatorNotFound => {
            "请启动已安装碧蓝航线的 模拟器 实例后重新运行 generate；本次未发布工作簿。"
                .to_owned()
        }
        AppErrorCode::EmulatorManagerAmbiguous => {
            "请在 device.instance 指定提供方与实例；同类模拟器有多个安装时，用 manager_path 指定管理器后重新运行 generate；本次未发布工作簿。"
                .to_owned()
        }
        AppErrorCode::EmulatorInstanceAmbiguous => {
            "请在 settings.json 的 device.instance 中指定目标实例后重新运行 generate；本次未发布工作簿。"
                .to_owned()
        }
        AppErrorCode::AdbTargetUnavailable => {
            "请确认目标实例在线，并在 模拟器 中同意本工具的 ADB 连接后重新运行 generate；本次未发布工作簿。"
                .to_owned()
        }
        AppErrorCode::SettingsInvalid if context.get("component").is_some_and(|v| v == "game") => {
            "请按字段提示修复程序目录内的 settings.json 后重新运行 generate；本次未读取游戏状态或发布工作簿。"
                .to_owned()
        }
        AppErrorCode::SettingsInvalid => {
            "请使用 data/workbooks 目录内的有效文件名，然后重新运行 generate。".to_owned()
        }
        AppErrorCode::RuntimeIncompatible => {
            "请更新与当前游戏版本匹配的完整运行态资源后重新运行 generate；本次未发布工作簿。"
                .to_owned()
        }
        AppErrorCode::RuntimeBootstrapFailed => {
            "请根据运行态日志修复连接或发布资源后重新运行 generate；本次未发布工作簿。"
                .to_owned()
        }
        AppErrorCode::WorkbookLocked => {
            "请关闭占用目标工作簿的程序后重新运行 generate；既有文件保持不变。".to_owned()
        }
        AppErrorCode::ApplicationInitializationFailed => {
            "请确认程序目录完整且可写，然后重新运行 generate；未通过验证的文件不会发布。".to_owned()
        }
        _ if context.contains_key("output_path") => {
            "请根据输出路径和原因修复工作簿环境后重新运行 generate；既有文件不会被覆盖。".to_owned()
        }
        _ => "请根据“原因”修复工作簿或运行态问题后重新运行 generate。".to_owned(),
    }
}

/// 根据输入、运行态和计划错误给出保持只读边界的恢复动作。
fn workbook_check_suggestion(code: AppErrorCode, context: &BTreeMap<String, String>) -> String {
    match code {
        AppErrorCode::WorkbookInvalid | AppErrorCode::InputInvalid => {
            "请根据工作表、配置行和原因修正该工作簿后重新运行 check；本次未读取或改动游戏状态。"
                .to_owned()
        }
        AppErrorCode::LayoutInvalid | AppErrorCode::LayoutUpgradeRequired => {
            "请先运行 layout-check，并按检查结果修复或升级根布局，再重新运行 check；本次未改动游戏状态。"
                .to_owned()
        }
        AppErrorCode::GameNotReady
            if context
                .get("missing_port")
                .is_some_and(|port| port == "game") =>
        {
            "请在 Windows 发布环境中建立已认证的游戏只读运行态后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::GameNotReady => {
            "请确认游戏已登录并停留在可读取状态后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::EmulatorNotFound => {
            "请启动已安装碧蓝航线的 模拟器 实例后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::EmulatorManagerAmbiguous => {
            "请在 device.instance 指定提供方与实例；同类模拟器有多个安装时，用 manager_path 指定管理器后重新运行 check。"
                .to_owned()
        }
        AppErrorCode::EmulatorInstanceAmbiguous => {
            "请在 settings.json 的 device.instance 中指定目标实例后重新运行 check。"
                .to_owned()
        }
        AppErrorCode::AdbTargetUnavailable => {
            "请确认目标实例在线，并在 模拟器 中同意本工具的 ADB 连接后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::SettingsInvalid => {
            "请按字段提示修复程序目录内的 settings.json 后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::RuntimeIncompatible => {
            "请更新与当前游戏版本匹配的完整运行态资源后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::RuntimeBootstrapFailed => {
            "请根据运行态日志修复连接或发布资源后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::CapabilityMissing if context.contains_key("action") => {
            "当前游戏快照缺少验证该库存动作所需的安全信息；请将对应动作改为保持后重新运行 check。"
                .to_owned()
        }
        AppErrorCode::CapabilityMissing => {
            "请使用具备当前只读能力的完整运行态资源后重新运行 check；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::EquipmentNotFound | AppErrorCode::EquipmentStateChanged => {
            "工作簿与当前游戏状态已经不一致；请重新生成工作簿，保留仍适用的目标后再次运行 check。"
                .to_owned()
        }
        AppErrorCode::EquipmentConflict => {
            "请根据装备来源、槽位和数量上下文消除互相冲突的目标后重新运行 check。"
                .to_owned()
        }
        AppErrorCode::EquipmentIncompatible => {
            "请按目标槽位允许的装备类型和舰种禁用信息选择兼容装备后重新运行 check；未通过检查的计划不会执行。"
                .to_owned()
        }
        AppErrorCode::EnhanceInvalid => {
            "请将目标强化等级改为当前装备链和只读计划器已支持的等级后重新运行 check。"
                .to_owned()
        }
        AppErrorCode::FullCheckFailed => {
            "请根据原因和对象上下文修正计划输入后重新运行 check；未通过完整检查的计划不会执行。"
                .to_owned()
        }
        AppErrorCode::WorkbookLocked => {
            "请关闭占用该工作簿的程序后重新运行 check；本次未修改工作簿或游戏状态。"
                .to_owned()
        }
        AppErrorCode::ApplicationInitializationFailed => {
            "请确认程序位于完整且可访问的发布目录后重新运行 check。".to_owned()
        }
        _ => "请根据说明、原因和对象上下文修复问题后重新运行 check。".to_owned(),
    }
}

fn workbook_catalog_suggestion(_code: AppErrorCode, _context: &BTreeMap<String, String>) -> String {
    "请确认程序目录的 data/workbooks 是普通目录，移除链接、重解析点或嵌套目录后重新运行 workbooks。"
        .to_owned()
}

fn history_catalog_suggestion(_code: AppErrorCode, _context: &BTreeMap<String, String>) -> String {
    "请确认 data/history 是工具目录内的普通目录，移除链接、临时文件或损坏历史后重新运行 history；既有历史文件不会被修改。".to_owned()
}

fn settings_summary_suggestion(_code: AppErrorCode, _context: &BTreeMap<String, String>) -> String {
    "请根据具体原因检查设置文件内容、访问权限或文件占用；设置发生变化时重新读取后重试。".to_owned()
}

fn log_catalog_suggestion(_code: AppErrorCode, _context: &BTreeMap<String, String>) -> String {
    "请确认 data/logs 是工具目录内的普通目录，当前支持 app、runtime 和 adb 日志，请备份并检查未登记文件后重新运行 logs；日志正文不会被命令输出。".to_owned()
}

fn check_results_writeback_suggestion(
    code: AppErrorCode,
    context: &BTreeMap<String, String>,
) -> String {
    if context
        .get("source_changed")
        .is_some_and(|value| value == "true")
    {
        return "工作簿在检查期间发生编辑，请保留当前文件并重新运行 check-save，以当前输入更新检查结果。".to_owned();
    }
    if code == AppErrorCode::WorkbookLocked {
        return "请关闭占用工作簿的程序后重新运行 check-save；已保存的检查历史可从历史文件路径查看。".to_owned();
    }
    "请根据原因修复工作簿的结果表结构或写入权限后重新运行 check-save；已保存的检查历史可从历史文件路径查看。".to_owned()
}

fn check_history_suggestion(code: AppErrorCode, _context: &BTreeMap<String, String>) -> String {
    if code == AppErrorCode::HistoryWriteFailed {
        return "请确认 data/history 是工具目录内可写的普通目录，并移走同名历史文件后重新运行 check-save；已有历史记录保持不变，检查结果工作表的写回状态见错误上下文。".to_owned();
    }
    "请根据说明和原因修复历史目录后重新运行 check-save。".to_owned()
}

/// 根据执行历史阶段给出不会导致命令重放的恢复动作。
pub(crate) fn finished_execution_suggestion<T>(history: &StageResult<T>) -> String {
    match history {
        StageResult::Completed(_) => {
            "游戏执行已经结束，执行历史已经保存；请按历史路径核对结果并修复工作簿占用或内容变化，禁止直接重跑 execute。"
                .to_owned()
        }
        StageResult::Failed(_) | StageResult::NotAttempted => {
            "游戏执行已经结束，但审计历史未能发布；请保留报告摘要和现有备份，修复 data/history 后人工核对，禁止直接重跑 execute。"
                .to_owned()
        }
    }
}

fn execution_suggestion(code: AppErrorCode, context: &BTreeMap<String, String>) -> String {
    match code {
        AppErrorCode::CapabilityMissing
            if context
                .get("missing_port")
                .is_some_and(|port| port == "execution") =>
        {
            "请在 Windows 完整发布环境中建立具备写能力的认证游戏会话后重新运行 execute；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::WorkbookBackupFailed => {
            "请根据备份路径和原因修复 data/backups 后重新运行 execute；备份未完成前不会发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::WorkbookLocked => {
            "请关闭占用该工作簿的 Excel 窗口后重新运行 execute；本次尚未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::WorkbookInvalid
        | AppErrorCode::InputInvalid
        | AppErrorCode::LayoutInvalid
        | AppErrorCode::LayoutUpgradeRequired => {
            "请根据工作表、字段和原因修复或升级工作簿后重新运行 execute；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::EquipmentNotFound
        | AppErrorCode::EquipmentStateChanged
        | AppErrorCode::EquipmentConflict
        | AppErrorCode::EquipmentIncompatible
        | AppErrorCode::EnhanceInvalid
        | AppErrorCode::FullCheckFailed => {
            "工作簿计划与最新完整游戏状态未能通过执行前复核；请重新生成或修正计划后再运行 execute，本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::GameNotReady
        | AppErrorCode::EmulatorNotFound
        | AppErrorCode::EmulatorManagerAmbiguous
        | AppErrorCode::EmulatorInstanceAmbiguous
        | AppErrorCode::AdbTargetUnavailable
        | AppErrorCode::SettingsInvalid
        | AppErrorCode::RuntimeIncompatible
        | AppErrorCode::RuntimeBootstrapFailed => {
            "请根据原因恢复唯一且已认证的游戏运行态后重新运行 execute；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::ApplicationInitializationFailed => {
            "请确认程序目录、布局和配置完整可用后重新运行 execute；本次未发送游戏写命令。"
                .to_owned()
        }
        AppErrorCode::HistoryWriteFailed | AppErrorCode::ExecutionResultsWriteFailed => {
            "请根据路径和原因修复持久化环境后重新检查当前状态；没有执行完成证据时才可重新运行 execute。"
                .to_owned()
        }
        _ => "请根据说明、原因和对象上下文修复问题后重新运行 execute。".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use azur_lane_workbook::adapters::release::ReleaseError;
    use azur_lane_workbook::adapters::tool_root::ToolRootError;
    use azur_lane_workbook::application::{AppErrorCode, LayoutModelError, StageResult};

    use super::PublishedCleanupKind;
    use azur_lane_workbook::bootstrap::BootstrapError;
    #[cfg(not(target_os = "windows"))]
    use azur_lane_workbook::bootstrap::bootstrap_sync_service;

    #[cfg(not(target_os = "windows"))]
    use super::format_generation_error;
    use super::{
        OPERATION_CONTEXT_LABELS, check_history_suggestion, execution_suggestion,
        finished_execution_suggestion, format_layout_check_bootstrap_error, format_release_error,
        layout_check_suggestion, layout_preview_suggestion, layout_upgrade_suggestion,
        log_catalog_suggestion, published_generation_suggestion, workbook_check_suggestion,
    };

    #[test]
    fn direct_game_diagnostic_preserves_resident_failure() {
        let error = super::AppError::from_source(
            "game.runtime",
            AppErrorCode::RuntimeIncompatible,
            "当前游戏运行环境与只读运行态不兼容",
            std::io::Error::other("驻留代理目标或资源版本与当前安装不一致"),
        );
        let diagnostic = super::format_game_operation_error("游戏查询失败", &error);
        assert!(diagnostic.contains("游戏查询失败 [RUNTIME_INCOMPATIBLE]"));
        assert!(diagnostic.contains("原因：驻留代理目标或资源版本与当前安装不一致"));
        assert!(diagnostic.contains("重启游戏并重新登录"));
    }

    #[test]
    fn layout_suggestions_distinguish_upgrade_and_value_repairs() {
        let upgrade =
            layout_check_suggestion(AppErrorCode::LayoutUpgradeRequired, &BTreeMap::new());
        assert!(upgrade.contains("保留当前布局"));
        assert!(upgrade.contains("不要覆盖用户设置"));
        assert!(upgrade.contains("独立升级文件"));
        assert!(upgrade.contains("layout-upgrade"));

        let context = BTreeMap::from([("expected".to_owned(), "整数".to_owned())]);
        assert!(layout_check_suggestion(AppErrorCode::LayoutInvalid, &context).contains("期望值"));

        let changed = BTreeMap::from([
            ("actual".to_owned(), "new".to_owned()),
            ("expected".to_owned(), "old".to_owned()),
            ("source_changed".to_owned(), "true".to_owned()),
        ]);
        let changed_suggestion =
            layout_upgrade_suggestion(AppErrorCode::LayoutMigrationFailed, &changed);
        assert!(changed_suggestion.contains("停止编辑源布局"));
        assert!(!changed_suggestion.contains("迁移规则"));

        let schema = BTreeMap::from([
            ("actual".to_owned(), "0".to_owned()),
            ("expected".to_owned(), "1".to_owned()),
            ("migration_schema".to_owned(), "0->1".to_owned()),
        ]);
        assert!(
            layout_upgrade_suggestion(AppErrorCode::LayoutMigrationFailed, &schema)
                .contains("使用当前默认布局重新同步生成工作簿")
        );

        let locked = layout_preview_suggestion(AppErrorCode::WorkbookLocked, &BTreeMap::new());
        assert!(locked.contains("关闭"));
        assert!(locked.contains("已有预览保持不变"));
        assert!(locked.contains("layout-preview"));

        let output_path = layout_preview_suggestion(
            AppErrorCode::ApplicationInitializationFailed,
            &BTreeMap::new(),
        );
        assert!(output_path.contains("data/workbooks"));
        assert!(output_path.contains("可写的普通目录"));
        assert!(output_path.contains("已有预览保持不变"));
    }

    #[test]
    fn bootstrap_diagnostics_keep_the_shared_code_and_specific_repair() {
        let tool_root_error = BootstrapError::ToolRoot {
            source: ToolRootError::UnsafePath {
                path: PathBuf::from("tool-root"),
                message: "测试路径不安全".to_owned(),
            },
        };
        let tool_root_diagnostic = format_layout_check_bootstrap_error(&tool_root_error);
        assert!(tool_root_diagnostic.contains("[APPLICATION_INITIALIZATION_FAILED]"));
        assert!(tool_root_diagnostic.contains("没有链接重定向"));

        let registry_error = BootstrapError::WorkbookLayoutRegistry {
            source: LayoutModelError::RegistryEmpty {
                component: "工作表",
            },
        };
        let registry_diagnostic = format_layout_check_bootstrap_error(&registry_error);
        assert!(registry_diagnostic.contains("[APPLICATION_INITIALIZATION_FAILED]"));
        assert!(registry_diagnostic.contains("完整发布包"));
    }

    #[test]
    fn check_save_formatting_keeps_both_failures_and_existing_suggestions() {
        use azur_lane_workbook::application::{
            AppError, AppErrorCode, CheckAndSaveOutcome, OperationTerminal, SessionCleanup,
        };

        let check = AppError::from_source(
            "plan.check",
            AppErrorCode::WorkbookInvalid,
            "计划不成立",
            std::io::Error::other("row 4"),
        )
        .with_context("sheet", "loadout_plan");
        let writeback = AppError::from_source(
            "check.workbook.writeback",
            AppErrorCode::WorkbookLocked,
            "工作簿被占用",
            std::io::Error::other("sharing violation"),
        )
        .with_context("path", "data/workbooks/plan.xlsx");
        let failed = CheckAndSaveOutcome::CheckFailed {
            error: check,
            writeback: Err(writeback),
        };
        assert_eq!(failed.log_terminal(), OperationTerminal::Failed);
        let text = super::format_check_save_incomplete(failed);
        assert!(text.contains("阶段：plan.check"), "{text}");
        assert!(text.contains("原因：row 4"), "{text}");
        assert!(text.contains("阶段：check.workbook.writeback"), "{text}");
        assert!(text.contains("原因：sharing violation"), "{text}");
        assert!(text.contains("修复建议："), "{text}");
        assert!(text.contains("重新运行 check"), "{text}");
        assert!(text.contains("重新运行 check-save"), "{text}");

        let cleanup = AppError::from_source(
            "game.cleanup.fixture",
            AppErrorCode::RuntimeBootstrapFailed,
            "清理失败",
            std::io::Error::other("socket closed"),
        )
        .with_context("check_stage", "read_without_check");
        let unread = CheckAndSaveOutcome::ReadWithoutCheck {
            cleanup: SessionCleanup::Failed(cleanup),
        };
        assert_eq!(unread.log_terminal(), OperationTerminal::Incomplete);
        let text = super::format_check_save_incomplete(unread);
        assert!(text.contains("阶段：game.cleanup.fixture"), "{text}");
        assert!(
            text.contains("上下文 check_stage：read_without_check"),
            "{text}"
        );
        assert!(text.contains("原因：socket closed"), "{text}");
        assert!(text.contains("修复建议："), "{text}");
        assert!(text.contains("重新运行 check"), "{text}");
    }

    #[test]
    fn workbook_check_suggestions_distinguish_input_runtime_and_plan_failures() {
        let input = workbook_check_suggestion(AppErrorCode::InputInvalid, &BTreeMap::new());
        assert!(input.contains("修正该工作簿"));
        assert!(input.contains("未读取或改动游戏状态"));

        let missing_game = BTreeMap::from([("missing_port".to_owned(), "game".to_owned())]);
        let runtime = workbook_check_suggestion(AppErrorCode::GameNotReady, &missing_game);
        assert!(runtime.contains("Windows"));
        assert!(runtime.contains("未发送游戏写命令"));

        let unsupported_action = BTreeMap::from([("action".to_owned(), "dismantle".to_owned())]);
        let capability =
            workbook_check_suggestion(AppErrorCode::CapabilityMissing, &unsupported_action);
        assert!(capability.contains("安全信息"));
        assert!(capability.contains("改为保持"));

        let incompatible =
            workbook_check_suggestion(AppErrorCode::EquipmentIncompatible, &BTreeMap::new());
        assert!(incompatible.contains("目标槽位允许的装备类型"));
        assert!(incompatible.contains("舰种禁用"));
        assert!(incompatible.contains("计划不会执行"));
    }

    #[test]
    fn check_writeback_recovery_retries_the_writing_check_command() {
        let changed = BTreeMap::from([("source_changed".to_owned(), "true".to_owned())]);
        let suggestion =
            super::check_results_writeback_suggestion(AppErrorCode::WorkbookInvalid, &changed);
        assert!(suggestion.contains("保留当前文件"));
        assert!(suggestion.contains("check-save"));
        let locked = super::check_results_writeback_suggestion(
            AppErrorCode::WorkbookLocked,
            &BTreeMap::new(),
        );
        assert!(locked.contains("关闭占用工作簿"));
        assert!(locked.contains("check-save"));
    }

    #[test]
    fn check_history_suggestion_preserves_history_and_reports_workbook_status() {
        let suggestion =
            check_history_suggestion(AppErrorCode::HistoryWriteFailed, &BTreeMap::new());

        assert!(suggestion.contains("data/history"));
        assert!(suggestion.contains("移走同名历史文件"));
        assert!(suggestion.contains("已有历史记录保持不变"));
        assert!(suggestion.contains("检查结果工作表的写回状态"));
    }

    #[test]
    fn log_catalog_suggestion_keeps_log_content_out_of_cli_output() {
        let suggestion = log_catalog_suggestion(AppErrorCode::LogReadFailed, &BTreeMap::new());

        assert!(suggestion.contains("data/logs"));
        assert!(suggestion.contains("runtime"));
        assert!(suggestion.contains("日志正文不会被命令输出"));
    }

    #[test]
    fn execution_suggestion_for_completed_persistence_failure_forbids_replay() {
        let suggestion = finished_execution_suggestion::<()>(&StageResult::Completed(()));

        assert!(suggestion.contains("游戏执行已经结束"));
        assert!(suggestion.contains("执行历史已经保存"));
        assert!(suggestion.contains("禁止直接重跑 execute"));
    }

    #[test]
    fn execution_suggestion_treats_incompatibility_as_a_preflight_failure() {
        let suggestion =
            execution_suggestion(AppErrorCode::EquipmentIncompatible, &BTreeMap::new());

        assert!(suggestion.contains("执行前复核"));
        assert!(suggestion.contains("本次未发送游戏写命令"));
    }

    #[test]
    fn operation_context_labels_cover_backup_evidence_without_duplicates() {
        for expected in [
            ("source_path", "源工作簿"),
            ("backup_path", "执行前备份"),
            ("source_package_sha256", "源工作簿摘要"),
            ("backup_package_sha256", "备份工作簿摘要"),
            ("equipment_type_id", "装备类型"),
            ("ship_type_id", "舰种"),
            ("allowed_equipment_type_ids", "槽位允许装备类型"),
            ("forbidden_ship_type_ids", "装备禁用舰种"),
        ] {
            assert!(OPERATION_CONTEXT_LABELS.contains(&expected));
        }
        assert_eq!(
            OPERATION_CONTEXT_LABELS
                .iter()
                .filter(|(key, _)| *key == "source_changed")
                .count(),
            1
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn generation_diagnostic_keeps_stage_and_runtime_recovery_action() {
        let mut service = bootstrap_sync_service(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("测试组合根应成功");
        let error = service
            .generate_workbook(None, &|| false)
            .expect_err("未配置运行态时必须稳定失败");

        let diagnostic = format_generation_error(&error);

        assert!(diagnostic.contains("工作簿生成失败 [GAME_NOT_READY]"));
        assert!(diagnostic.contains("阶段：workbook.generate"));
        assert!(diagnostic.contains("已认证的游戏运行态连接"));
        assert!(diagnostic.contains("没有读取游戏状态或写入工作簿"));
    }

    #[test]
    fn completed_generation_suggestion_does_not_claim_that_nothing_was_published() {
        let context = BTreeMap::from([(
            "output_path".to_owned(),
            "data/workbooks/generated.xlsx".to_owned(),
        )]);

        let suggestion =
            published_generation_suggestion(PublishedCleanupKind::Incomplete, &context);

        assert!(suggestion.contains("工作簿已经生成"));
        assert!(suggestion.contains("禁止直接重跑 generate"));
        assert!(!suggestion.contains("未发布工作簿"));
    }

    #[test]
    fn release_diagnostic_keeps_stable_context_and_repair_action() {
        let error = ReleaseError::InvalidManifest {
            field: "files[0].path".to_owned(),
            message: "路径顺序错误".to_owned(),
        };

        let diagnostic = format_release_error(&error);

        assert!(diagnostic.contains("发布校验失败 [MANIFEST_INVALID]"));
        assert!(diagnostic.contains("阶段：release.validate_manifest"));
        assert!(diagnostic.contains("字段：files[0].path"));
        assert!(diagnostic.contains("路径顺序错误"));
        assert!(diagnostic.contains("重新取得完整发布目录"));
    }
    #[test]
    fn recovered_shutdown_suggestion_preserves_unload_failure() {
        let context = BTreeMap::from([(
            "output_path".to_owned(),
            "data/workbooks/fixture.xlsx".to_owned(),
        )]);
        let suggestion = published_generation_suggestion(PublishedCleanupKind::Recovered, &context);
        assert!(suggestion.contains("正常卸载失败，恢复清理已完成"));
        assert!(suggestion.contains("保留输出文件"));
        assert!(!suggestion.contains("清理未完整确认"));
    }
}

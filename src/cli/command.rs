//! 定义主程序支持的命令及其严格参数解析。

use azur_lane_workbook::application::{AcquisitionUpdateMode, GameQuery, GameQueryKind};
use std::ffi::OsString;

const VERIFY_RELEASE_COMMAND: &str = "verify-release";
const DOCTOR_COMMAND: &str = "doctor";
const LAYOUT_CHECK_COMMAND: &str = "layout-check";
const LAYOUT_UPGRADE_COMMAND: &str = "layout-upgrade";
const LAYOUT_PREVIEW_COMMAND: &str = "layout-preview";
const GENERATE_COMMAND: &str = "generate";
pub(crate) const CHECK_COMMAND: &str = "check";
pub(crate) const CHECK_SAVE_COMMAND: &str = "check-save";
pub(crate) const EXECUTE_COMMAND: &str = "execute";
pub(crate) const OPEN_COMMAND: &str = "open";
const WORKBOOKS_COMMAND: &str = "workbooks";
const HISTORY_COMMAND: &str = "history";
const SETTINGS_COMMAND: &str = "settings";
const LOGS_COMMAND: &str = "logs";
const UPDATE_ACQUISITION_COMMAND: &str = "update-acquisition";

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum StartupCommand {
    Help(Option<String>),
    Mcp,
    ArtifactDetails(super::artifact_details::ArtifactDetails),
    Gui,
    VerifyRelease,
    Doctor,
    LayoutCheck,
    LayoutUpgrade,
    LayoutPreview,
    Generate(Option<String>),
    Check(String),
    CheckSave(String),
    Execute(String),
    Open(String),
    Workbooks,
    History,
    Settings,
    Preferences,
    SetPreference(String, String),
    Instances,
    Agent(AgentCommand, Option<String>),
    WithInstance(String, Box<StartupCommand>),
    Logs,
    UpdateAcquisition(String, AcquisitionUpdateMode),
    Query(Box<GameQuery>),
    EquipmentActions(super::equipment_actions::ActionInput, bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentCommand {
    Status,
    Inject,
    Unload,
}

fn parse_agent_command(value: &OsString) -> Result<AgentCommand, String> {
    match value.to_str() {
        Some("status") => Ok(AgentCommand::Status),
        Some("inject") => Ok(AgentCommand::Inject),
        Some("unload") => Ok(AgentCommand::Unload),
        _ => Err("代理命令用法: agent <status|inject|unload> [INSTANCE]".to_owned()),
    }
}

pub(crate) fn parse_command(arguments: &[OsString]) -> Result<StartupCommand, String> {
    match arguments {
        [command, catalog, category] if command == "help" && catalog == "catalog" => {
            let topic = format!("catalog {}", parse_text_argument(category, "图鉴类别")?);
            super::help::render(Some(&topic))?;
            return Ok(StartupCommand::Help(Some(topic)));
        }
        [command, category, flag] if command == "catalog" && (flag == "--help" || flag == "-h") => {
            let topic = format!("catalog {}", category.to_str().ok_or("帮助主题无效")?);
            super::help::render(Some(&topic))?;
            return Ok(StartupCommand::Help(Some(topic)));
        }
        [command, category, rest @ ..]
            if command == "catalog" && category != "--help" && category != "-h" =>
        {
            let kind = match category.to_str() {
                Some("ships") => GameQueryKind::CatalogShips,
                Some("equipment") => GameQueryKind::CatalogEquipment,
                Some("skills") => GameQueryKind::CatalogSkills,
                _ => return Err("用法: catalog ships|equipment|skills [查询参数]".into()),
            };
            return parse_query(kind, rest)
                .map(Box::new)
                .map(StartupCommand::Query);
        }
        [command, rest @ ..]
            if matches!(
                command.to_str(),
                Some("recipes" | "items" | "resources" | "fleets" | "technology")
            ) && !rest.iter().any(|s| s == "--help" || s == "-h") =>
        {
            let kind = match command.to_str().unwrap() {
                "recipes" => GameQueryKind::Recipes,
                "items" => GameQueryKind::Items,
                "resources" => GameQueryKind::Resources,
                "fleets" => GameQueryKind::Fleets,
                _ => GameQueryKind::Technology,
            };
            return parse_query(kind, rest)
                .map(Box::new)
                .map(StartupCommand::Query);
        }
        [flag] if flag == "--help" || flag == "-h" || flag == "help" => {
            return Ok(StartupCommand::Help(None));
        }
        [command, flag] if flag == "--help" || flag == "-h" => {
            let topic = parse_text_argument(command, "帮助主题")?;
            super::help::render(Some(&topic))?;
            return Ok(StartupCommand::Help(Some(topic)));
        }
        [command, topic] if command == "help" => {
            let topic = parse_text_argument(topic, "帮助主题")?;
            super::help::render(Some(&topic))?;
            return Ok(StartupCommand::Help(Some(topic)));
        }
        _ => {}
    }
    if let [flag, instance, rest @ ..] = arguments
        && flag == "--instance"
    {
        let instance = parse_text_argument(instance, "实例标识")?;
        let command = parse_command(rest)?;
        if matches!(command, StartupCommand::Help(_)) {
            return Ok(command);
        }
        if !matches!(
            command,
            StartupCommand::Generate(_)
                | StartupCommand::Check(_)
                | StartupCommand::CheckSave(_)
                | StartupCommand::Execute(_)
                | StartupCommand::Query(_)
                | StartupCommand::EquipmentActions(_, _)
        ) {
            return Err(
                "--instance 适用于 generate、check、check-save、execute、ships、equipment 及装备操作命令；代理命令在末尾指定实例"
                    .to_owned(),
            );
        }
        return Ok(StartupCommand::WithInstance(instance, Box::new(command)));
    }
    match arguments {
        [command, rest @ ..]
            if matches!(
                command.to_str(),
                Some("equip" | "unequip" | "enhance" | "dismantle" | "compose" | "equipment-actions")
            ) =>
        {
            let (input, apply) =
                super::equipment_actions::parse(command.to_str().expect("命令已验证"), rest)?;
            Ok(StartupCommand::EquipmentActions(input, apply))
        }
        [command, rest @ ..] if command == "ships" || command == "equipment" => {
            let kind = if command == "ships" {
                GameQueryKind::Ships
            } else {
                GameQueryKind::Equipment
            };
            parse_query(kind, rest).map(Box::new).map(StartupCommand::Query)
        }
        [] => Ok(StartupCommand::Gui),
        [command] if command == VERIFY_RELEASE_COMMAND => Ok(StartupCommand::VerifyRelease),
        [command] if command == DOCTOR_COMMAND => Ok(StartupCommand::Doctor),
        [command] if command == "mcp" => Ok(StartupCommand::Mcp),
        [command] if command == LAYOUT_CHECK_COMMAND => Ok(StartupCommand::LayoutCheck),
        [command] if command == LAYOUT_UPGRADE_COMMAND => Ok(StartupCommand::LayoutUpgrade),
        [command] if command == LAYOUT_PREVIEW_COMMAND => Ok(StartupCommand::LayoutPreview),
        [command] if command == GENERATE_COMMAND => Ok(StartupCommand::Generate(None)),
        [command, requested_name] if command == GENERATE_COMMAND => Ok(StartupCommand::Generate(
            Some(parse_requested_name(requested_name)?),
        )),
        [command, workbook_name] if command == CHECK_COMMAND => {
            Ok(StartupCommand::Check(parse_requested_name(workbook_name)?))
        }
        [command, workbook_name] if command == CHECK_SAVE_COMMAND => Ok(StartupCommand::CheckSave(
            parse_requested_name(workbook_name)?,
        )),
        [command, workbook_name] if command == EXECUTE_COMMAND => Ok(StartupCommand::Execute(
            parse_requested_name(workbook_name)?,
        )),
        [command, workbook_name] if command == OPEN_COMMAND => {
            Ok(StartupCommand::Open(parse_requested_name(workbook_name)?))
        }
        [command] if command == WORKBOOKS_COMMAND => Ok(StartupCommand::Workbooks),
        [command] if command == HISTORY_COMMAND => Ok(StartupCommand::History),
        [command] if command == SETTINGS_COMMAND => Ok(StartupCommand::Settings),
        [command, action] if command == SETTINGS_COMMAND && action == "preferences" => {
            Ok(StartupCommand::Preferences)
        }
        [command, action, key, value] if command == SETTINGS_COMMAND && action == "set" => {
            Ok(StartupCommand::SetPreference(
                parse_text_argument(key, "设置名称")?,
                parse_text_argument(value, "设置值")?,
            ))
        }
        [command] if command == "instances" => Ok(StartupCommand::Instances),
        [command, action] if command == "agent" => {
            Ok(StartupCommand::Agent(parse_agent_command(action)?, None))
        }
        [command, action, instance] if command == "agent" => Ok(StartupCommand::Agent(
            parse_agent_command(action)?,
            Some(parse_text_argument(instance, "实例标识")?),
        )),
        [command] if command == LOGS_COMMAND => Ok(StartupCommand::Logs),
        [command, rest @ ..] if command == "history" || command == "logs" => {
            super::artifact_details::parse(command.to_str().expect("命令已验证"),rest).map(StartupCommand::ArtifactDetails)
        }
        [command, workbook_name, flag, mode] if command == UPDATE_ACQUISITION_COMMAND && flag == "--mode" => {
            let mode = match mode.to_str() {
                Some("missing") => AcquisitionUpdateMode::Missing,
                Some("refresh") => AcquisitionUpdateMode::Refresh,
                _ => return Err("获取方式更新模式必须是 missing 或 refresh".to_owned()),
            };
            Ok(StartupCommand::UpdateAcquisition(parse_requested_name(workbook_name)?, mode))
        }
        [command, workbook_name] if command == UPDATE_ACQUISITION_COMMAND => Ok(
            StartupCommand::UpdateAcquisition(parse_requested_name(workbook_name)?, AcquisitionUpdateMode::Missing),
        ),
        _ => Err(r"未知命令或参数；运行 .\AzurLaneWorkbook.exe --help 查看全部命令，或 COMMAND --help 查看详细用法。".to_owned()),
    }
}

fn parse_query(kind: GameQueryKind, arguments: &[OsString]) -> Result<GameQuery, String> {
    super::query::parse(kind, arguments)
}
fn parse_requested_name(argument: &OsString) -> Result<String, String> {
    parse_text_argument(argument, "工作簿名称")
}

fn parse_text_argument(argument: &OsString, label: &str) -> Result<String, String> {
    let name: &str = argument
        .to_str()
        .ok_or_else(|| format!("{label}必须是有效文本"))?;
    if name.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{StartupCommand, parse_command};
    use azur_lane_workbook::application::AcquisitionUpdateMode;
    use std::ffi::OsString;

    #[test]
    fn parses_object_queries_and_rejects_ambiguous_options() {
        let parse =
            |args: &[&str]| parse_command(&args.iter().map(OsString::from).collect::<Vec<_>>());
        let StartupCommand::Query(query) =
            parse(&["ships", "--ids", "8,2", "--fields", "level"]).unwrap()
        else {
            panic!("需要查询命令");
        };
        assert_eq!(query.ids(), &[2, 8]);
        assert_eq!(query.fields(), &["level"]);
        assert!(parse(&["--instance", "0", "equipment", "--full"]).is_ok());
        for args in [
            &["ships", "--ids"][..],
            &["ships", "--ids", "0"],
            &["ships", "--full", "--fields", "level"],
            &["equipment", "--fields", "level"],
            &["ships", "--ids", "1,1"],
            &["ships", "--fields", "name,name"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_agent_management_and_preferences() {
        use super::AgentCommand;
        let parse =
            |args: &[&str]| parse_command(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(
            parse(&["agent", "status"]).unwrap(),
            StartupCommand::Agent(AgentCommand::Status, None)
        );
        assert_eq!(
            parse(&["agent", "inject", "0"]).unwrap(),
            StartupCommand::Agent(AgentCommand::Inject, Some("0".to_owned()))
        );
        assert_eq!(
            parse(&["agent", "unload"]).unwrap(),
            StartupCommand::Agent(AgentCommand::Unload, None)
        );
        assert_eq!(parse(&["instances"]).unwrap(), StartupCommand::Instances);
        assert_eq!(
            parse(&["--instance", "0", "generate"]).unwrap(),
            StartupCommand::WithInstance("0".to_owned(), Box::new(StartupCommand::Generate(None)))
        );
        assert!(parse(&["--instance", "0", "--instance", "1", "generate"]).is_err());
        assert!(parse(&["--instance", "0", "settings"]).is_err());
        assert_eq!(
            parse(&["settings", "preferences"]).unwrap(),
            StartupCommand::Preferences
        );
        assert_eq!(
            parse(&["settings", "set", "unload_after_sync", "true"]).unwrap(),
            StartupCommand::SetPreference("unload_after_sync".to_owned(), "true".to_owned())
        );
        for args in [
            &["agent"][..],
            &["agent", "restart"],
            &["agent", "status", ""],
            &["agent", "status", "0", "1"],
            &["settings", "set", "", "true"],
        ] {
            assert!(parse(args).is_err());
        }
    }

    #[test]
    fn parses_gui_and_supported_commands() {
        assert_eq!(parse_command(&[]).unwrap(), StartupCommand::Gui);
        assert_eq!(
            parse_command(&[OsString::from("verify-release")]).unwrap(),
            StartupCommand::VerifyRelease
        );
        assert_eq!(
            parse_command(&[OsString::from("doctor")]).unwrap(),
            StartupCommand::Doctor
        );
        assert_eq!(
            parse_command(&[OsString::from("layout-check")]).unwrap(),
            StartupCommand::LayoutCheck
        );
        assert_eq!(
            parse_command(&[OsString::from("layout-upgrade")]).unwrap(),
            StartupCommand::LayoutUpgrade
        );
        assert_eq!(
            parse_command(&[OsString::from("layout-preview")]).unwrap(),
            StartupCommand::LayoutPreview
        );
        assert_eq!(
            parse_command(&[OsString::from("generate")]).unwrap(),
            StartupCommand::Generate(None)
        );
        assert_eq!(
            parse_command(&[OsString::from("generate"), OsString::from("plan.xlsx")]).unwrap(),
            StartupCommand::Generate(Some("plan.xlsx".to_owned()))
        );
        assert_eq!(
            parse_command(&[OsString::from("check"), OsString::from("plan.xlsx")]).unwrap(),
            StartupCommand::Check("plan.xlsx".to_owned())
        );
        assert_eq!(
            parse_command(&[OsString::from("check-save"), OsString::from("plan.xlsx")]).unwrap(),
            StartupCommand::CheckSave("plan.xlsx".to_owned())
        );
        assert_eq!(
            parse_command(&[OsString::from("execute"), OsString::from("plan.xlsx")]).unwrap(),
            StartupCommand::Execute("plan.xlsx".to_owned())
        );
        assert_eq!(
            parse_command(&[OsString::from("open"), OsString::from("plan.xlsx")]).unwrap(),
            StartupCommand::Open("plan.xlsx".to_owned())
        );
        assert_eq!(
            parse_command(&[OsString::from("workbooks")]).unwrap(),
            StartupCommand::Workbooks
        );
        assert_eq!(
            parse_command(&[OsString::from("history")]).unwrap(),
            StartupCommand::History
        );
        assert_eq!(
            parse_command(&[OsString::from("settings")]).unwrap(),
            StartupCommand::Settings
        );
        assert_eq!(
            parse_command(&[OsString::from("logs")]).unwrap(),
            StartupCommand::Logs
        );
        assert_eq!(
            parse_command(&[
                OsString::from("update-acquisition"),
                OsString::from("plan.xlsx")
            ])
            .unwrap(),
            StartupCommand::UpdateAcquisition(
                "plan.xlsx".to_owned(),
                AcquisitionUpdateMode::Missing
            )
        );
        for (value, expected) in [
            ("missing", AcquisitionUpdateMode::Missing),
            ("refresh", AcquisitionUpdateMode::Refresh),
        ] {
            assert_eq!(
                parse_command(
                    &["update-acquisition", "plan.xlsx", "--mode", value].map(OsString::from)
                )
                .unwrap(),
                StartupCommand::UpdateAcquisition("plan.xlsx".to_owned(), expected)
            );
        }
        assert!(
            parse_command(
                &["update-acquisition", "plan.xlsx", "--mode", "invalid"].map(OsString::from)
            )
            .is_err()
        );
        assert!(parse_command(&[OsString::from("unknown")]).is_err());
        assert!(parse_command(&[OsString::from("check")]).is_err());
        assert!(parse_command(&[OsString::from("check-save")]).is_err());
        assert!(parse_command(&[OsString::from("execute")]).is_err());
        assert!(parse_command(&[OsString::from("open")]).is_err());
        assert!(
            parse_command(&[
                OsString::from("check"),
                OsString::from("a.xlsx"),
                OsString::from("b.xlsx")
            ])
            .is_err()
        );
        assert!(
            parse_command(&[
                OsString::from("layout-check"),
                OsString::from("workbook-layout.xlsx")
            ])
            .is_err()
        );
        assert!(parse_command(&[OsString::from("generate"), OsString::new()]).is_err());
        assert!(parse_command(&[OsString::from("check"), OsString::new()]).is_err());
        assert!(parse_command(&[OsString::from("check-save"), OsString::new()]).is_err());
        assert!(parse_command(&[OsString::from("execute"), OsString::new()]).is_err());
        assert!(parse_command(&[OsString::from("open"), OsString::new()]).is_err());
        assert!(
            parse_command(&[
                OsString::from("generate"),
                OsString::from("a"),
                OsString::from("b")
            ])
            .is_err()
        );
    }
}

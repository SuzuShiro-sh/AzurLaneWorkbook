//! 解析离线文件详情命令，并将受控读取结果输出为 JSON。

use std::ffi::OsString;
use std::path::Path;
use suzushiro_cli_args::set_once;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ArtifactDetails {
    History {
        filename: String,
        fields: Option<Vec<String>>,
    },
    Logs {
        filename: String,
        tail: Option<usize>,
        status: Option<String>,
    },
}

pub(crate) fn parse(command: &str, arguments: &[OsString]) -> Result<ArtifactDetails, String> {
    let [action, filename, options @ ..] = arguments else {
        return Err(
            "用法: history show FILE [--fields a,b] | logs show FILE [--tail N] [--status STATUS]"
                .to_owned(),
        );
    };
    if action != "show" {
        return Err("详情命令必须使用 show".to_owned());
    }
    let filename = text(filename, "文件名")?.to_owned();
    if filename.contains(['/', '\\']) || filename.trim() != filename {
        return Err("必须指定目录内的单个文件名".to_owned());
    }
    let mut fields = None;
    let mut tail = None;
    let mut status = None;
    for pair in options.chunks(2) {
        let [flag, value] = pair else {
            return Err("详情选项缺少值".to_owned());
        };
        let flag = text(flag, "选项")?;
        let value = text(value, "选项值")?;
        match (command, flag) {
            ("history", "--fields") => {
                let values: Vec<String> = value.split(',').map(str::to_owned).collect();
                if values
                    .iter()
                    .any(|field| field.is_empty() || field.trim() != field)
                {
                    return Err("--fields 必须是逗号分隔的非空顶层字段名".to_owned());
                }
                set_once(&mut fields, values, flag)?;
            }
            ("logs", "--tail") => {
                if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("--tail 必须是非负整数".to_owned());
                }
                let value = value
                    .parse::<usize>()
                    .map_err(|error| format!("--tail 无效: {error}"))?;
                set_once(&mut tail, value, flag)?;
            }
            ("logs", "--status") => {
                set_once(&mut status, value.to_owned(), flag)?;
            }
            _ => return Err(format!("{command} show 不支持选项 {flag}")),
        }
    }
    match command {
        "history" => Ok(ArtifactDetails::History { filename, fields }),
        "logs" => Ok(ArtifactDetails::Logs {
            filename,
            tail,
            status,
        }),
        _ => Err(format!("未知详情命令 {command}")),
    }
}

fn text<'a>(value: &'a OsString, label: &str) -> Result<&'a str, String> {
    value
        .to_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{label}必须是非空 Unicode 文本"))
}

pub(crate) fn run(root: &Path, request: ArtifactDetails) -> Result<(), String> {
    let result = match request {
        ArtifactDetails::History { filename, fields } => {
            azur_lane_workbook::bootstrap::read_history_details(root, &filename, fields.as_deref())
        }
        ArtifactDetails::Logs {
            filename,
            tail,
            status,
        } => azur_lane_workbook::bootstrap::read_log_details(
            root,
            &filename,
            tail,
            status.as_deref(),
        ),
    }
    .map_err(|error| {
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(error) = source {
            message.push_str(&format!("\n原因: {error}"));
            source = error.source();
        }
        message
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result)
            .map_err(|error| format!("详情 JSON 序列化失败: {error}"))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_explicit_details_and_rejects_ambiguous_options() {
        assert_eq!(
            parse(
                "history",
                &args(&[
                    "show",
                    "001-check.json",
                    "--fields",
                    "report,schema_version"
                ])
            )
            .unwrap(),
            ArtifactDetails::History {
                filename: "001-check.json".to_owned(),
                fields: Some(vec!["report".to_owned(), "schema_version".to_owned()])
            }
        );
        assert_eq!(
            parse(
                "logs",
                &args(&["show", "001-app.log", "--status", "failed", "--tail", "0"])
            )
            .unwrap(),
            ArtifactDetails::Logs {
                filename: "001-app.log".to_owned(),
                tail: Some(0),
                status: Some("failed".to_owned())
            }
        );
        for values in [
            vec!["show", "../001-app.log"],
            vec!["show", "001-app.log", "--tail"],
            vec!["show", "001-app.log", "--tail", "-1"],
            vec!["show", "001-app.log", "--tail", "1", "--tail", "2"],
            vec!["show", "001-app.log", "--fields", "status"],
        ] {
            assert!(parse("logs", &args(&values)).is_err());
        }
        assert!(
            parse(
                "history",
                &args(&["show", "001-check.json", "--fields", "report,"])
            )
            .is_err()
        );
    }
}

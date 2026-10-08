//! 生成可复现的默认工作簿布局配置。

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use azur_lane_workbook::adapters::workbook::create_default_layout_workbook;
use suzushiro_cli_args::{ArgumentCursor, set_once};

const USAGE: &str = "用法: generate-workbook-layout --output FILE.xlsx";

fn main() -> ExitCode {
    match run() {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("生成默认布局失败: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<PathBuf, String> {
    let arguments = Arguments::parse(env::args_os())?;
    create_default_layout_workbook(&arguments.output).map_err(|error| error.to_string())?;
    Ok(arguments.output)
}

struct Arguments {
    output: PathBuf,
}

impl Arguments {
    /// 拒绝未知项、重复项和缺值，路径值保留操作系统原生编码。
    fn parse(values: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut cursor = ArgumentCursor::new(values.into_iter());
        let mut output = None;

        while let Some((name, value)) = cursor.next_pair(USAGE)? {
            match name.as_str() {
                "--output" => set_once(&mut output, PathBuf::from(value), "--output")?,
                _ => return Err(format!("未知参数 {name}。{USAGE}")),
            }
        }

        Ok(Self {
            output: output.ok_or_else(|| format!("缺少 --output。{USAGE}"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::Arguments;

    #[test]
    fn parser_requires_one_output_path() {
        let arguments = Arguments::parse(
            [
                "generate-workbook-layout",
                "--output",
                "workbook-layout.xlsx",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(arguments.output, Path::new("workbook-layout.xlsx"));
        assert!(
            Arguments::parse(
                [
                    "generate-workbook-layout",
                    "--output",
                    "one.xlsx",
                    "--output",
                    "two.xlsx",
                ]
                .map(OsString::from),
            )
            .is_err()
        );
        assert!(
            Arguments::parse(
                ["generate-workbook-layout", "--unknown", "value"].map(OsString::from)
            )
            .is_err()
        );
    }
}

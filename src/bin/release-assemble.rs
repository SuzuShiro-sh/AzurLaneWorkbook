//! 把 Windows 宿主程序和固定 runtime 产物装配为可校验的单目录发布包。

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use azur_lane_workbook::adapters::release_assembly::{
    ReleaseAssemblyOptions, ReleaseAssemblyReport, assemble_release,
};
use suzushiro_cli_args::{ArgumentCursor, set_once};

const USAGE: &str = "用法: release-assemble --output DIRECTORY --executable AzurLaneWorkbook.exe --runtime-root DIRECTORY";

/// 严格解析来源和目标路径，成功时只输出结构化发布摘要。
fn main() -> ExitCode {
    match run() {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("编码发布装配摘要失败: {error}");
                ExitCode::from(1)
            }
        },
        Err(error) => {
            eprintln!("发布装配失败: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ReleaseAssemblyReport, String> {
    let arguments: Arguments = Arguments::parse(env::args_os())?;
    assemble_release(ReleaseAssemblyOptions::new(
        arguments.output,
        arguments.executable,
        arguments.runtime_root,
    ))
    .map_err(|error| error.to_string())
}

struct Arguments {
    output: PathBuf,
    executable: PathBuf,
    runtime_root: PathBuf,
}

impl Arguments {
    /// 拒绝未知项、重复项和缺值，路径值保留操作系统原生编码。
    fn parse(values: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut cursor = ArgumentCursor::new(values.into_iter());
        let mut output: Option<PathBuf> = None;
        let mut executable: Option<PathBuf> = None;
        let mut runtime_root: Option<PathBuf> = None;

        while let Some((name, value)) = cursor.next_pair(USAGE)? {
            match name.as_str() {
                "--output" => set_once(&mut output, PathBuf::from(value), "--output")?,
                "--executable" => {
                    set_once(&mut executable, PathBuf::from(value), "--executable")?;
                }
                "--runtime-root" => {
                    set_once(&mut runtime_root, PathBuf::from(value), "--runtime-root")?;
                }
                _ => return Err(format!("未知参数 {name}。{USAGE}")),
            }
        }

        Ok(Self {
            output: output.ok_or_else(|| format!("缺少 --output。{USAGE}"))?,
            executable: executable.ok_or_else(|| format!("缺少 --executable。{USAGE}"))?,
            runtime_root: runtime_root.ok_or_else(|| format!("缺少 --runtime-root。{USAGE}"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::Arguments;

    #[test]
    fn parser_requires_each_path_exactly_once() {
        let arguments: Arguments = Arguments::parse(
            [
                "release-assemble",
                "--output",
                "release",
                "--executable",
                "target/release/AzurLaneWorkbook.exe",
                "--runtime-root",
                "target/release",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(arguments.output, Path::new("release"));
        assert_eq!(
            arguments.executable,
            Path::new("target/release/AzurLaneWorkbook.exe")
        );
        assert_eq!(arguments.runtime_root, Path::new("target/release"));
        assert!(
            Arguments::parse(
                ["release-assemble", "--output", "one", "--output", "two"].map(OsString::from)
            )
            .is_err()
        );
    }
}

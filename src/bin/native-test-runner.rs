//! 解析锁定工具链和唯一设备参数，调用 Native 实机测试运行器并输出稳定收据。

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use azur_lane_workbook::adapters::device::native_test_runner::{
    NativeTestRunOutcome, NativeTestRunnerOptions, NativeTestToolchain, run_native_tests,
};
use suzushiro_cli_args::{ArgumentCursor, set_once};

const USAGE: &str = "用法: native-test-runner --repository-root PATH --tool-root PATH --cmake PATH --ctest PATH --ninja PATH --ndk-toolchain PATH --build-relative PATH --serial HOST:PORT";

/// 执行 Native 实机测试并只输出稳定收据或单条错误。
fn main() -> ExitCode {
    match run() {
        Ok(outcome) => {
            println!(
                "AZLW_NATIVE_TEST_RECEIPT status=passed report={} report_sha256={} report_size={}",
                outcome.report_path.display(),
                outcome.report_sha256,
                outcome.report_size_bytes
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("AZLW_NATIVE_TEST_ERROR {message}");
            ExitCode::from(1)
        }
    }
}

/// 解析命令行并调用唯一库级执行入口。
fn run() -> Result<NativeTestRunOutcome, String> {
    let arguments = Arguments::parse(env::args_os())?;
    let outcome = run_native_tests(arguments.into_options()).map_err(|error| error.to_string())?;
    if !outcome.report.passed() {
        return Err(format!(
            "Native 实机测试未全部通过，报告位于 {}",
            outcome.report_path.display()
        ));
    }
    Ok(outcome)
}

/// 保存固定工具链、受控目录和唯一 ADB 目标。
#[derive(Debug)]
struct Arguments {
    repository_root: PathBuf,
    tool_root: PathBuf,
    cmake: PathBuf,
    ctest: PathBuf,
    ninja: PathBuf,
    ndk_toolchain: PathBuf,
    build_relative: PathBuf,
    serial: String,
}

impl Arguments {
    /// 严格解析成对参数，拒绝缺值、重复项、未知项和非 Unicode serial。
    fn parse(values: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut cursor = ArgumentCursor::new(values.into_iter());
        let mut repository_root = None;
        let mut tool_root = None;
        let mut cmake = None;
        let mut ctest = None;
        let mut ninja = None;
        let mut ndk_toolchain = None;
        let mut build_relative = None;
        let mut serial = None;

        while let Some((name, value)) = cursor.next_pair(USAGE)? {
            match name.as_str() {
                "--repository-root" => set_once(
                    &mut repository_root,
                    PathBuf::from(value),
                    "--repository-root",
                )?,
                "--tool-root" => {
                    set_once(&mut tool_root, PathBuf::from(value), "--tool-root")?;
                }
                "--cmake" => set_once(&mut cmake, PathBuf::from(value), "--cmake")?,
                "--ctest" => set_once(&mut ctest, PathBuf::from(value), "--ctest")?,
                "--ninja" => set_once(&mut ninja, PathBuf::from(value), "--ninja")?,
                "--ndk-toolchain" => {
                    set_once(&mut ndk_toolchain, PathBuf::from(value), "--ndk-toolchain")?
                }
                "--build-relative" => set_once(
                    &mut build_relative,
                    PathBuf::from(value),
                    "--build-relative",
                )?,
                "--serial" => {
                    let value = value
                        .into_string()
                        .map_err(|_| "参数 --serial 的值不是有效 Unicode".to_owned())?;
                    set_once(&mut serial, value, "--serial")?;
                }
                _ => return Err(format!("未知参数 {name}。{USAGE}")),
            }
        }

        Ok(Self {
            repository_root: require(repository_root, "--repository-root")?,
            tool_root: require(tool_root, "--tool-root")?,
            cmake: require(cmake, "--cmake")?,
            ctest: require(ctest, "--ctest")?,
            ninja: require(ninja, "--ninja")?,
            ndk_toolchain: require(ndk_toolchain, "--ndk-toolchain")?,
            build_relative: require(build_relative, "--build-relative")?,
            serial: require(serial, "--serial")?,
        })
    }

    fn into_options(self) -> NativeTestRunnerOptions {
        NativeTestRunnerOptions::new(
            self.repository_root,
            self.tool_root,
            NativeTestToolchain::new(self.cmake, self.ctest, self.ninja, self.ndk_toolchain),
            self.build_relative,
            self.serial,
        )
    }
}

fn require<T>(value: Option<T>, name: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("缺少 {name}。{USAGE}"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::Arguments;

    fn complete_arguments() -> Vec<OsString> {
        [
            "native-test-runner",
            "--repository-root",
            "C:\\repo",
            "--tool-root",
            "C:\\evidence",
            "--cmake",
            "C:\\tools\\cmake.exe",
            "--ctest",
            "C:\\tools\\ctest.exe",
            "--ninja",
            "C:\\tools\\ninja.exe",
            "--ndk-toolchain",
            "C:\\ndk\\android.toolchain.cmake",
            "--build-relative",
            "target\\native-device-tests",
            "--serial",
            "127.0.0.1:16385",
        ]
        .map(OsString::from)
        .to_vec()
    }

    /// 完整参数必须原样保留路径和唯一设备 serial。
    #[test]
    fn parser_accepts_complete_contract() {
        let parsed = Arguments::parse(complete_arguments()).unwrap();

        assert_eq!(parsed.repository_root, Path::new("C:\\repo"));
        assert_eq!(
            parsed.build_relative,
            Path::new("target\\native-device-tests")
        );
        assert_eq!(parsed.serial, "127.0.0.1:16385");
    }

    /// 缺少参数、重复参数和未知参数都必须在启动执行前失败。
    #[test]
    fn parser_rejects_incomplete_duplicate_and_unknown_contracts() {
        let mut missing = complete_arguments();
        missing.truncate(missing.len() - 2);
        assert!(
            Arguments::parse(missing)
                .unwrap_err()
                .contains("缺少 --serial")
        );

        let mut duplicate = complete_arguments();
        duplicate.extend(["--serial", "127.0.0.1:16385"].map(OsString::from));
        assert!(Arguments::parse(duplicate).unwrap_err().contains("重复"));

        let mut unknown = complete_arguments();
        unknown.extend(["--unexpected", "value"].map(OsString::from));
        assert!(Arguments::parse(unknown).unwrap_err().contains("未知参数"));
    }
}

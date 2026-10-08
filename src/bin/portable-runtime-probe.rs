//! 在原生 Windows 与 MuMu12 环境执行便携 ADB 和运行态联合验证。

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use azur_lane_workbook::adapters::device::portable::{
    PortableMode, PortableProbeOptions, PortableProbeOutcome, PortableShipCatalogCaptureOutcome,
    run_portable_probe, run_portable_ship_catalog_capture,
};
use suzushiro_cli_args::{ArgumentCursor, set_once};

const USAGE: &str = "用法: portable-runtime-probe --mode auto|manual [--manager PATH] [--instance INDEX] [--serial HOST:PORT] [--game-package PACKAGE] [--timeout-ms 1..30000] [--max-items 1..2000] [--full-state-capture-root PATH | --ship-catalog-capture-root PATH]；manual 必须同时提供 instance 和 serial，manager 省略时只接受唯一登记安装；捕获目录必须位于发布目录外";

/// 执行便携探针并只输出稳定收据摘要或单条错误。
fn main() -> ExitCode {
    match run() {
        Ok(CliOutcome::Probe(outcome)) => {
            let capture_summary: String = outcome
                .runtime
                .full_state_capture
                .as_ref()
                .map(|capture| {
                    format!(
                        " full_state_capture={} full_state_capture_sha256={} full_state_capture_schema={} full_state_capture_read={}",
                        capture.path().display(),
                        capture.sha256(),
                        capture.schema_version(),
                        capture.read_index()
                    )
                })
                .unwrap_or_default();
            println!(
                "AZLW_PORTABLE_RUNTIME_RECEIPT status=passed mode={} session_id={} snapshots={} adb_port={} receipt={} runtime_receipt={} equipment_sample={} journal={}{}",
                outcome.report.mode.as_str(),
                outcome.report.runtime_session_id,
                outcome.report.runtime_snapshot_count,
                outcome.report.adb.server_port,
                outcome.receipt_path.display(),
                outcome.runtime.receipt_path.display(),
                outcome.runtime.equipment_sample_path.display(),
                outcome.runtime.journal_path.display(),
                capture_summary
            );
            ExitCode::SUCCESS
        }
        Ok(CliOutcome::ShipCatalog(outcome)) => {
            println!(
                "AZLW_PORTABLE_SHIP_CATALOG_RECEIPT status=passed mode={} session_id={} tables={} records={} module_sha256={} content_sha256={} capture={} capture_sha256={} receipt={} journal={}",
                outcome.report.mode.as_str(),
                outcome.report.capture.session_id,
                outcome.report.capture.table_count,
                outcome.report.capture.record_count,
                outcome.report.capture.module_sha256,
                outcome.report.capture.content_sha256,
                outcome.report.capture.path.display(),
                outcome.report.capture.file_sha256,
                outcome.receipt_path.display(),
                outcome.report.runtime_journal_path.display(),
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("AZLW_PORTABLE_RUNTIME_ERROR {message}");
            ExitCode::from(1)
        }
    }
}

/// 解析命令行、定位发布根目录并调用唯一的便携编排入口。
fn run() -> Result<CliOutcome, String> {
    let arguments: Arguments = Arguments::parse(env::args_os())?;
    let executable: PathBuf =
        env::current_exe().map_err(|error| format!("读取当前可执行文件路径失败: {error}"))?;
    let tool_root: PathBuf = executable
        .parent()
        .ok_or_else(|| "当前可执行文件缺少父目录".to_owned())?
        .to_path_buf();
    let ship_catalog_capture_root = arguments.ship_catalog_capture_root.clone();
    let options: PortableProbeOptions = arguments.into_options(tool_root)?;
    match ship_catalog_capture_root {
        Some(capture_root) => run_portable_ship_catalog_capture(options, capture_root)
            .map(|outcome| CliOutcome::ShipCatalog(Box::new(outcome)))
            .map_err(|error| error.to_string()),
        None => run_portable_probe(options)
            .map(|outcome| CliOutcome::Probe(Box::new(outcome)))
            .map_err(|error| error.to_string()),
    }
}

enum CliOutcome {
    Probe(Box<PortableProbeOutcome>),
    ShipCatalog(Box<PortableShipCatalogCaptureOutcome>),
}

/// 保存便携探针模式、可选发现提示和有界运行参数。
struct Arguments {
    mode: PortableMode,
    manager: Option<PathBuf>,
    instance: Option<String>,
    serial: Option<String>,
    game_package: Option<String>,
    timeout_ms: u32,
    max_items: u32,
    full_state_capture_root: Option<PathBuf>,
    ship_catalog_capture_root: Option<PathBuf>,
}

impl Arguments {
    /// 严格解析成对参数，拒绝未知项、重复项、缺值和非 Unicode 文本值。
    fn parse(values: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut cursor = ArgumentCursor::new(values.into_iter());
        let mut mode: Option<PortableMode> = None;
        let mut manager: Option<PathBuf> = None;
        let mut instance: Option<String> = None;
        let mut serial: Option<String> = None;
        let mut game_package: Option<String> = None;
        let mut timeout_ms: Option<u32> = None;
        let mut max_items: Option<u32> = None;
        let mut full_state_capture_root: Option<PathBuf> = None;
        let mut ship_catalog_capture_root: Option<PathBuf> = None;

        while let Some((name, raw_value)) = cursor.next_pair(USAGE)? {
            match name.as_str() {
                "--mode" => {
                    let value: String = unicode_value("--mode", raw_value)?;
                    let parsed: PortableMode = PortableMode::from_str(&value)
                        .map_err(|error| format!("{error}。{USAGE}"))?;
                    set_once(&mut mode, parsed, "--mode")?;
                }
                "--manager" => set_once(&mut manager, PathBuf::from(raw_value), "--manager")?,
                "--instance" => {
                    let value: String = unicode_value("--instance", raw_value)?;
                    set_once(&mut instance, value, "--instance")?;
                }
                "--serial" => {
                    let value: String = unicode_value("--serial", raw_value)?;
                    set_once(&mut serial, value, "--serial")?;
                }
                "--game-package" => {
                    let value: String = unicode_value("--game-package", raw_value)?;
                    set_once(&mut game_package, value, "--game-package")?;
                }
                "--timeout-ms" => {
                    let value: String = unicode_value("--timeout-ms", raw_value)?;
                    set_once(
                        &mut timeout_ms,
                        parse_u32("--timeout-ms", &value)?,
                        "--timeout-ms",
                    )?;
                }
                "--max-items" => {
                    let value: String = unicode_value("--max-items", raw_value)?;
                    set_once(
                        &mut max_items,
                        parse_u32("--max-items", &value)?,
                        "--max-items",
                    )?;
                }
                "--full-state-capture-root" => set_once(
                    &mut full_state_capture_root,
                    PathBuf::from(raw_value),
                    "--full-state-capture-root",
                )?,
                "--ship-catalog-capture-root" => set_once(
                    &mut ship_catalog_capture_root,
                    PathBuf::from(raw_value),
                    "--ship-catalog-capture-root",
                )?,
                _ => return Err(format!("未知参数 {name}。{USAGE}")),
            }
        }

        if full_state_capture_root.is_some() && ship_catalog_capture_root.is_some() {
            return Err(format!(
                "--full-state-capture-root 与 --ship-catalog-capture-root 不得同时启用。{USAGE}"
            ));
        }

        Ok(Self {
            mode: mode.ok_or_else(|| format!("缺少 --mode。{USAGE}"))?,
            manager,
            instance,
            serial,
            game_package,
            timeout_ms: timeout_ms.unwrap_or(10_000),
            max_items: max_items.unwrap_or(2_000),
            full_state_capture_root,
            ship_catalog_capture_root,
        })
    }

    /// 把解析结果映射到库级选项，并由库统一检查模式组合与数值边界。
    fn into_options(self, tool_root: PathBuf) -> Result<PortableProbeOptions, String> {
        let mut options: PortableProbeOptions = PortableProbeOptions::new(tool_root, self.mode);
        if let Some(manager) = self.manager {
            options = options.with_manager_hint(manager);
        }
        if let Some(instance) = self.instance {
            options = options.with_instance_hint(instance);
        }
        if let Some(serial) = self.serial {
            options = options.with_serial_hint(serial);
        }
        if let Some(game_package) = self.game_package {
            options = options.with_game_package_hint(game_package);
        }
        if let Some(capture_root) = self.full_state_capture_root {
            options = options.with_full_state_capture_root(capture_root);
        }
        let options = options
            .with_timeout_ms(self.timeout_ms)
            .and_then(|options| options.with_max_items(self.max_items))
            .map_err(|error| error.to_string())?;
        options.validate().map_err(|error| error.to_string())?;
        Ok(options)
    }
}

/// 将要求文本语义的参数值转换为 Unicode，并保留字段名用于诊断。
fn unicode_value(name: &str, value: OsString) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| format!("参数 {name} 的值不是有效 Unicode"))
}

/// 将选项值解析为无符号十进制整数，并保留字段名用于诊断。
fn parse_u32(name: &str, value: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("参数 {name} 必须是无符号十进制整数，实际为 {value:?}"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::Arguments;

    /// 自动模式只需模式和可选实例提示，并保留有界默认值。
    #[test]
    fn parser_accepts_auto_mode_and_defaults() {
        let arguments: Arguments =
            Arguments::parse(["probe", "--mode", "auto", "--instance", "0"].map(OsString::from))
                .unwrap();

        assert_eq!(arguments.mode.as_str(), "auto");
        assert_eq!(arguments.instance.as_deref(), Some("0"));
        assert_eq!(arguments.timeout_ms, 10_000);
        assert_eq!(arguments.max_items, 2_000);
        assert!(arguments.full_state_capture_root.is_none());
        assert!(arguments.ship_catalog_capture_root.is_none());
    }

    /// 静态目录采集必须由专用参数显式启用，并与完整状态采集互斥。
    #[test]
    fn parser_accepts_explicit_ship_catalog_capture_root() {
        let arguments: Arguments = Arguments::parse(
            [
                "probe",
                "--mode",
                "auto",
                "--ship-catalog-capture-root",
                "C:\\catalog",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(
            arguments.ship_catalog_capture_root.as_deref(),
            Some(std::path::Path::new("C:\\catalog"))
        );
        assert!(
            Arguments::parse(
                [
                    "probe",
                    "--mode",
                    "auto",
                    "--full-state-capture-root",
                    "C:\\full",
                    "--ship-catalog-capture-root",
                    "C:\\catalog",
                ]
                .map(OsString::from)
            )
            .is_err()
        );
    }

    /// 完整状态采集必须由单独参数显式启用并原样保留路径。
    #[test]
    fn parser_accepts_explicit_full_state_capture_root() {
        let arguments: Arguments = Arguments::parse(
            [
                "probe",
                "--mode",
                "auto",
                "--full-state-capture-root",
                "C:\\capture",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(
            arguments.full_state_capture_root.as_deref(),
            Some(std::path::Path::new("C:\\capture"))
        );
    }

    /// 手工模式接受管理器、带提供方前缀的实例标识和设备地址组合。
    #[test]
    fn parser_accepts_complete_manual_target() {
        let arguments: Arguments = Arguments::parse(
            [
                "probe",
                "--mode",
                "manual",
                "--manager",
                "manager.exe",
                "--instance",
                "mumu12:0",
                "--serial",
                "127.0.0.1:16384",
            ]
            .map(OsString::from),
        )
        .unwrap();

        assert_eq!(arguments.mode.as_str(), "manual");
        assert_eq!(arguments.instance.as_deref(), Some("mumu12:0"));
        assert_eq!(
            arguments.manager.as_deref(),
            Some(std::path::Path::new("manager.exe"))
        );
        assert_eq!(arguments.serial.as_deref(), Some("127.0.0.1:16384"));
        assert!(
            arguments
                .into_options(std::path::PathBuf::from("."))
                .is_ok()
        );
    }

    /// 解析器拒绝旧外部 ADB 参数、重复参数、缺值和缺失模式。
    #[test]
    fn parser_rejects_external_adb_unknown_duplicate_and_missing_values() {
        assert!(Arguments::parse(["probe", "--adb", "adb.exe"].map(OsString::from)).is_err());
        assert!(
            Arguments::parse(["probe", "--mode", "auto", "--mode", "manual"].map(OsString::from))
                .is_err()
        );
        assert!(Arguments::parse(["probe", "--mode"].map(OsString::from)).is_err());
        assert!(Arguments::parse(["probe", "--instance", "0"].map(OsString::from)).is_err());
    }
}

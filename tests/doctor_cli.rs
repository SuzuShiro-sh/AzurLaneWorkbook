//! 在独立发布目录中验证 doctor 的离线报告、失败边界和 data 目录只读契约。

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use azur_lane_workbook::adapters::release_assembly::{ReleaseAssemblyOptions, assemble_release};
use azur_lane_workbook::adapters::workbook::edit_text_cell_to_new_file;

mod common;

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(1);
const AGENT_FIXTURE: &[u8] = b"fixture\nAZLW_AGENT_VERSION=0.11.0\0";

#[test]
fn reports_verified_files_and_explicitly_unavailable_runtime_capabilities() {
    let fixture = ReleaseFixture::new("valid");
    let release_root = fixture.assemble("release");
    let working_directory = fixture.parent.join("unrelated-working-directory");
    fs::create_dir(&working_directory).unwrap();
    let before = data_snapshot(&release_root);

    let output: Output = Command::new(release_root.join("AzurLaneWorkbook.exe"))
        .arg("doctor")
        .current_dir(working_directory)
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(!stdout.contains(fixture.parent.to_string_lossy().as_ref()));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "offline_ready");
    assert_eq!(report["checks"]["tool_root"]["status"], "ready");
    assert_eq!(report["checks"]["settings"]["status"], "ready");
    assert_eq!(report["checks"]["settings"]["device_mode"], "auto");
    assert_eq!(report["checks"]["settings"]["adb_path_configured"], false);
    assert_eq!(report["checks"]["layout"]["schema_version"], 1);
    assert_eq!(report["checks"]["layout"]["sheet_count"], 10);
    assert_eq!(report["checks"]["layout"]["field_count"], 403);
    assert_eq!(report["checks"]["release"]["checked_files"], 9);
    assert_eq!(report["checks"]["release"]["immutable_files"], 6);
    assert_eq!(
        report["checks"]["release"]["modified_configurations"],
        serde_json::json!([])
    );
    assert_eq!(
        report["capabilities"]["runtime_probe"]["status"],
        "unavailable"
    );
    assert_eq!(
        report["capabilities"]["runtime_probe"]["reason_code"],
        "offline_not_probed"
    );
    assert_eq!(
        report["capabilities"]["runtime_probe"]["message"],
        "doctor 只读取本地文件，不启动模拟器、游戏或 ADB"
    );
    assert_eq!(
        report["capabilities"]["write_operations"]["status"],
        "unavailable"
    );
    assert_eq!(
        report["capabilities"]["write_operations"]["reason_code"],
        "offline_not_executed"
    );
    assert_eq!(data_snapshot(&release_root), before);
}

#[test]
fn rejects_invalid_settings_without_writing_data() {
    let fixture = ReleaseFixture::new("invalid-settings");
    let release_root = fixture.assemble("release");
    fs::write(
        common::resource_root(&release_root).join("settings.json"),
        b"{",
    )
    .unwrap();
    let before = data_snapshot(&release_root);

    let output: Output = Command::new(release_root.join("AzurLaneWorkbook.exe"))
        .arg("doctor")
        .current_dir(&release_root)
        .output()
        .unwrap();

    assert_failure(
        &output,
        [
            "doctor 检查失败 [SETTINGS_INVALID]",
            "阶段：doctor.settings",
            "settings.json 未通过严格校验",
        ],
    );
    assert_eq!(data_snapshot(&release_root), before);
}

#[test]
fn rejects_invalid_layout_without_writing_data() {
    let fixture = ReleaseFixture::new("invalid-layout");
    let release_root = fixture.assemble("release");
    let edited_layout = fixture.parent.join("edited-layout.xlsx");
    let layout = common::resource_root(&release_root).join("workbook-layout.xlsx");
    edit_text_cell_to_new_file(&layout, &edited_layout, "字段设置", "B2", "unknown_field").unwrap();
    fs::remove_file(&layout).unwrap();
    fs::rename(edited_layout, layout).unwrap();
    let before = data_snapshot(&release_root);

    let output: Output = Command::new(release_root.join("AzurLaneWorkbook.exe"))
        .arg("doctor")
        .current_dir(&release_root)
        .output()
        .unwrap();

    assert_failure(
        &output,
        [
            "doctor 检查失败 [LAYOUT_INVALID]",
            "阶段：workbook.layout.load",
            "修复建议：请修复 workbook-layout.xlsx 后重新运行 doctor。",
        ],
    );
    assert_eq!(data_snapshot(&release_root), before);
}

#[test]
fn reports_missing_manifest_after_local_checks() {
    let fixture = common::TestDirectory::new("azlw-doctor-cli-tests", "missing-manifest");
    let executable = common::copy_executable(fixture.path());
    let resources = common::resource_root(fixture.path());
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("settings.json"),
        resources.join("settings.json"),
    )
    .unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        resources.join("workbook-layout.xlsx"),
    )
    .unwrap();
    let before = data_snapshot(fixture.path());

    let output: Output = Command::new(executable)
        .arg("doctor")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_failure(
        &output,
        [
            "doctor 检查失败 [MANIFEST_INVALID]",
            "阶段：release.locate_manifest",
            "组件：manifest",
        ],
    );
    assert_eq!(data_snapshot(fixture.path()), before);
}

fn assert_failure<const N: usize>(output: &Output, expected: [&str; N]) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = stderr(output);
    for fragment in expected {
        assert!(error.contains(fragment), "诊断缺少 {fragment:?}: {error}");
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DataEntry {
    Directory,
    File(Vec<u8>),
}

fn data_snapshot(root: &Path) -> Option<Vec<(String, DataEntry)>> {
    let data = common::resource_root(root).join("data");
    if !data.exists() {
        return None;
    }
    let mut files = Vec::new();
    collect_files(&data, &data, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Some(files)
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<(String, DataEntry)>) {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(directory)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        if metadata.is_dir() {
            files.push((relative, DataEntry::Directory));
            collect_files(root, &path, files);
        } else {
            files.push((relative, DataEntry::File(fs::read(path).unwrap())));
        }
    }
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

struct ReleaseFixture {
    parent: PathBuf,
    executable: PathBuf,
    runtime_root: PathBuf,
    output_root: PathBuf,
}

impl ReleaseFixture {
    fn new(label: &str) -> Self {
        let home: OsString = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .expect("测试环境必须提供 HOME 或 USERPROFILE");
        let parent: PathBuf = PathBuf::from(home)
            .join("suzushiro/scratch/azlw-doctor-cli-tests")
            .join(format!("{label}-{}", unique_suffix()));
        let executable_root = parent.join("executable");
        let executable = executable_root.join("AzurLaneWorkbook.exe");
        let runtime_root = parent.join("runtime-source");
        let output_root = parent.join("outputs");
        for directory in [
            &executable_root,
            &runtime_root.join("runtime/adb"),
            &runtime_root.join("runtime/inject"),
            &runtime_root.join("runtime/resources/profiles"),
            &output_root,
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::copy(
            Path::new(env!("CARGO_BIN_EXE_AzurLaneWorkbook")),
            &executable,
        )
        .unwrap();
        for (path, bytes) in [
            ("runtime/adb/adb.exe", b"adb".as_slice()),
            ("runtime/adb/AdbWinApi.dll", b"adb api".as_slice()),
            ("runtime/adb/NOTICE.txt", b"adb notice".as_slice()),
            (
                "runtime/adb/source.properties",
                b"Pkg.Revision=37.0.1\n".as_slice(),
            ),
            ("runtime/inject/azlw-loader-x86_64", b"loader".as_slice()),
            ("runtime/inject/libazlw-agent-x86_64.so", AGENT_FIXTURE),
            (
                "runtime/resources/profiles/default.json",
                include_bytes!("../runtime/resources/profiles/default.json").as_slice(),
            ),
        ] {
            fs::write(runtime_root.join(path), bytes).unwrap();
        }
        Self {
            parent,
            executable,
            runtime_root,
            output_root,
        }
    }

    fn assemble(&self, name: &str) -> PathBuf {
        let output = self.output_root.join(name);
        assemble_release(ReleaseAssemblyOptions::new(
            &output,
            &self.executable,
            &self.runtime_root,
        ))
        .unwrap();
        make_executable(&output.join("AzurLaneWorkbook.exe"));
        output
    }
}

impl Drop for ReleaseFixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.parent)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("清理 doctor CLI 测试目录失败: {error}");
        }
    }
}

fn unique_suffix() -> String {
    let identifier = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{identifier}", std::process::id())
}

//! 在可移动目录中验证 verify-release 的成功输出和稳定失败诊断。

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use azur_lane_workbook::adapters::release_assembly::{ReleaseAssemblyOptions, assemble_release};

mod common;

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(1);
const AGENT_FIXTURE: &[u8] = b"fixture\nAZLW_AGENT_VERSION=0.11.0\0";

#[test]
fn verifies_an_assembled_release_from_an_unrelated_working_directory() {
    let fixture = ReleaseFixture::new("valid");
    let release_root = fixture.assemble("release");
    let unrelated_directory = fixture.parent.join("unrelated-working-directory");
    fs::create_dir(&unrelated_directory).unwrap();

    let output: Output = Command::new(release_root.join("AzurLaneWorkbook.exe"))
        .arg("verify-release")
        .current_dir(unrelated_directory)
        .output()
        .unwrap();

    assert!(output.status.success(), "命令失败: {}", stderr(&output));
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["product_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["checked_files"], 9);
    assert_eq!(report["immutable_files"], 6);
    assert_eq!(report["modified_configurations"], serde_json::json!([]));
}

#[test]
fn reports_a_missing_manifest_with_a_stable_release_diagnostic() {
    let fixture = common::TestDirectory::new("azlw-release-cli-tests", "missing-manifest");
    common::copy_executable(fixture.path());

    let output: Output = Command::new(fixture.path().join("AzurLaneWorkbook.exe"))
        .arg("verify-release")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_release_failure(
        &output,
        [
            "发布校验失败 [MANIFEST_INVALID]",
            "阶段：release.locate_manifest",
            "组件：manifest",
            "路径：manifest.json",
            "原因：",
            "修复建议：",
            "verify-release",
        ],
    );
}

#[test]
fn reports_invalid_manifest_json_with_field_context() {
    let fixture = common::TestDirectory::new("azlw-release-cli-tests", "invalid-manifest");
    common::copy_executable(fixture.path());
    fs::write(
        common::resource_root(fixture.path()).join("manifest.json"),
        b"{",
    )
    .unwrap();

    let output: Output = Command::new(fixture.path().join("AzurLaneWorkbook.exe"))
        .arg("verify-release")
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_release_failure(
        &output,
        [
            "发布校验失败 [MANIFEST_INVALID]",
            "阶段：release.parse_manifest",
            "字段：$",
            "说明：manifest.json 字段 $ 无效",
            "原因：",
            "修复建议：",
        ],
    );
}

fn assert_release_failure<const N: usize>(output: &Output, expected: [&str; N]) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: String = stderr(output);
    for fragment in expected {
        assert!(error.contains(fragment), "诊断缺少 {fragment:?}: {error}");
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
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
            .join("suzushiro/scratch/azlw-release-cli-tests")
            .join(format!("{label}-{}", unique_suffix()));
        let executable_root: PathBuf = parent.join("executable");
        let executable: PathBuf = executable_root.join("AzurLaneWorkbook.exe");
        let runtime_root: PathBuf = parent.join("runtime-source");
        let output_root: PathBuf = parent.join("outputs");
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
        let output: PathBuf = self.output_root.join(name);
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
            eprintln!("清理发布 CLI 测试目录失败: {error}");
        }
    }
}

fn unique_suffix() -> String {
    let identifier: u64 = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{identifier}", std::process::id())
}

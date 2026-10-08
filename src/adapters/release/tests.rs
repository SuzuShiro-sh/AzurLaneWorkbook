//! 覆盖发布清单、文件完整性与配置校验的单元测试。

use std::fs;
use std::path::PathBuf;

use super::payload::embed_sidecar_payload;
use super::{
    RELEASE_FILE_SPECS, ReleaseArchitecture, ReleaseError, ReleaseFileEntry, ReleaseFileKind,
    ReleaseFilePolicy, ReleaseFileSpec, ReleaseFileValidator, ReleaseManifest,
    ensure_extracted_release, verify_release,
};
use crate::adapters::tool_root::ToolRoot;
use crate::adapters::workbook::edit_text_cell_atomically;
use crate::application::AppErrorCode;

const SETTINGS: &str = include_str!("../../../settings.json");
const WORKBOOK_LAYOUT: &[u8] = include_bytes!("../../../workbook-layout.xlsx");
const PROFILE: &str = include_str!("../../../runtime/resources/profiles/default.json");
const AGENT_FIXTURE: &[u8] = b"fixture\nAZLW_AGENT_VERSION=0.11.0\0";

#[test]
fn verifies_exact_release_and_reports_valid_configuration_change() {
    let fixture: ReleaseFixture = ReleaseFixture::new("valid");
    fixture.write_manifest();
    let initial = verify_release(&fixture.root).unwrap();
    assert_eq!(initial.checked_files, 9);
    assert!(initial.modified_configurations.is_empty());

    fs::write(
        fixture.root.join("settings.json"),
        SETTINGS.replace(
            "\"connect_timeout_seconds\": 30",
            "\"connect_timeout_seconds\": 31",
        ),
    )
    .unwrap();
    let modified = verify_release(&fixture.root).unwrap();
    assert_eq!(
        modified.modified_configurations,
        ["settings.json".to_owned()]
    );
}

#[test]
fn saved_preferences_and_their_lock_preserve_release_validation() {
    let fixture = ReleaseFixture::new("saved-preferences");
    fixture.write_manifest();
    let original = crate::adapters::settings::Settings::load(&fixture.root)
        .unwrap()
        .preferences();
    crate::adapters::settings::Settings::save_preferences(
        &fixture.root,
        original,
        crate::application::UserPreferences {
            detailed_diagnostics: !original.detailed_diagnostics,
            ..original
        },
    )
    .unwrap();
    assert!(fixture.root.join("settings.lock").is_file());
    let verified = verify_release(&fixture.root).unwrap();
    assert_eq!(verified.modified_configurations, ["settings.json"]);
    fs::write(fixture.root.join("other.lock"), b"").unwrap();
    assert!(matches!(
        verify_release(&fixture.root),
        Err(ReleaseError::FileSetMismatch { .. })
    ));
}

#[test]
fn fixed_specs_record_actual_mixed_binary_architectures() {
    let architecture = |path: &str| {
        RELEASE_FILE_SPECS
            .iter()
            .find(|specification: &&ReleaseFileSpec<'_>| specification.path == path)
            .map(|specification: &ReleaseFileSpec<'_>| specification.architecture)
            .unwrap()
    };

    assert_eq!(
        architecture("runtime/adb/adb.exe"),
        ReleaseArchitecture::X86
    );
    assert_eq!(
        architecture("runtime/adb/AdbWinApi.dll"),
        ReleaseArchitecture::X86
    );
    assert_eq!(
        architecture("runtime/inject/azlw-loader-x86_64"),
        ReleaseArchitecture::X86_64
    );
    let agent = RELEASE_FILE_SPECS
        .iter()
        .find(|specification: &&ReleaseFileSpec<'_>| {
            specification.path == "runtime/inject/libazlw-agent-x86_64.so"
        })
        .unwrap();
    assert_eq!(agent.version, "0.11.0");
    assert_eq!(agent.validator, ReleaseFileValidator::AgentVersion);
}

#[test]
fn rejects_release_with_outdated_agent_marker() {
    let fixture: ReleaseFixture = ReleaseFixture::new("outdated-agent");
    fs::write(
        fixture.root.join("runtime/inject/libazlw-agent-x86_64.so"),
        b"fixture\nAZLW_AGENT_VERSION=0.7.6\0",
    )
    .unwrap();
    fixture.write_manifest();

    let error = verify_release(&fixture.root).unwrap_err();

    assert_eq!(error.code(), "RUNTIME_INCOMPATIBLE");
    assert_eq!(error.stage(), "release.validate_agent_version");
    assert!(error.to_string().contains("实际 0.7.6"));
}

#[test]
fn rejects_immutable_tampering_and_unregistered_files() {
    let fixture: ReleaseFixture = ReleaseFixture::new("tamper");
    fixture.write_manifest();
    fs::write(fixture.root.join("runtime/adb/adb.exe"), b"tampered").unwrap();
    assert!(verify_release(&fixture.root).is_err());

    let fixture: ReleaseFixture = ReleaseFixture::new("unexpected");
    fixture.write_manifest();
    fs::write(fixture.root.join("debug.pdb"), b"unregistered").unwrap();
    let error: String = verify_release(&fixture.root).unwrap_err().to_string();
    assert!(error.contains("debug.pdb"));
}

#[test]
fn modified_configuration_must_still_pass_its_schema() {
    let fixture: ReleaseFixture = ReleaseFixture::new("invalid-config");
    fixture.write_manifest();
    fs::write(
        fixture.root.join("settings.json"),
        SETTINGS.replace("\"schema_version\": 1", "\"schema_version\": 2"),
    )
    .unwrap();

    let error: String = verify_release(&fixture.root).unwrap_err().to_string();
    assert!(error.contains("settings.json schema 1"));
}

#[test]
fn reports_valid_workbook_layout_change() {
    let fixture: ReleaseFixture = ReleaseFixture::new("valid-workbook-layout");
    fixture.write_manifest();
    let workbook_path: PathBuf = fixture.root.join("workbook-layout.xlsx");
    edit_text_cell_atomically(&workbook_path, "工作表设置", "C2", "舰队配装计划").unwrap();

    let modified = verify_release(&fixture.root).unwrap();

    assert_eq!(
        modified.modified_configurations,
        ["workbook-layout.xlsx".to_owned()]
    );
}

#[test]
fn modified_workbook_layout_must_match_projection_contract() {
    let fixture: ReleaseFixture = ReleaseFixture::new("invalid-workbook-layout");
    fixture.write_manifest();
    let workbook_path: PathBuf = fixture.root.join("workbook-layout.xlsx");
    edit_text_cell_atomically(&workbook_path, "字段设置", "B2", "unknown_field").unwrap();

    let error: ReleaseError = verify_release(&fixture.root).unwrap_err();

    match error {
        ReleaseError::WorkbookLayout { path, source } => {
            assert_eq!(path, "workbook-layout.xlsx");
            assert_eq!(source.code(), AppErrorCode::LayoutInvalid);
        }
        other => panic!("应返回严格布局校验错误，实际为 {other}"),
    }
}

#[test]
fn settings_cannot_select_non_executable_manifest_file() {
    let fixture: ReleaseFixture = ReleaseFixture::new("adb-reference");
    fixture.write_manifest();
    fs::write(
        fixture.root.join("settings.json"),
        SETTINGS.replace(
            "\"adb_path\": \"\"",
            "\"adb_path\": \"runtime/adb/NOTICE.txt\"",
        ),
    )
    .unwrap();

    let error: String = verify_release(&fixture.root).unwrap_err().to_string();
    assert!(error.contains("自定义 ADB 必须是清单中强制校验的 executable"));
}

#[test]
fn auto_settings_allow_a_missing_adb_hint_to_fall_back() {
    let fixture: ReleaseFixture = ReleaseFixture::new("missing-adb-hint");
    fixture.write_manifest();
    fs::write(
        fixture.root.join("settings.json"),
        SETTINGS.replace(
            "\"adb_path\": \"\"",
            "\"adb_path\": \"runtime/removed-adb/adb.exe\"",
        ),
    )
    .unwrap();

    let report = verify_release(&fixture.root).unwrap();

    assert_eq!(report.modified_configurations, ["settings.json".to_owned()]);
}

#[test]
fn manifest_file_entries_must_keep_stable_order() {
    let fixture: ReleaseFixture = ReleaseFixture::new("manifest-order");
    fixture.write_manifest();
    let manifest_path: PathBuf = fixture.root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let files: &mut Vec<serde_json::Value> = manifest["files"].as_array_mut().unwrap();
    files.swap(0, 1);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let error: String = verify_release(&fixture.root).unwrap_err().to_string();
    assert!(error.contains("严格递增排序"));
}

#[test]
fn manifest_path_boundary_errors_keep_the_manifest_error_classification() {
    let fixture: ReleaseFixture = ReleaseFixture::new("manifest-path");
    fixture.write_manifest();
    let manifest_path: PathBuf = fixture.root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"][0]["path"] = serde_json::Value::String("../outside.exe".to_owned());
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let error: ReleaseError = verify_release(&fixture.root).unwrap_err();

    assert_eq!(error.code(), "MANIFEST_INVALID");
    assert_eq!(error.stage(), "release.validate_manifest");
    assert_eq!(
        error.context().get("field").map(String::as_str),
        Some("files[0].path")
    );
}

#[test]
fn manifest_cannot_register_file_outside_fixed_release_closure() {
    let fixture: ReleaseFixture = ReleaseFixture::new("registered-extra");
    fs::write(fixture.root.join("debug.pdb"), b"debug symbols").unwrap();
    let tool_root: ToolRoot = ToolRoot::open(&fixture.root).unwrap();
    let mut entries: Vec<ReleaseFileEntry> = RELEASE_FILE_SPECS
        .iter()
        .map(|definition: &ReleaseFileSpec<'_>| {
            ReleaseFileEntry::from_file(&tool_root, *definition).unwrap()
        })
        .collect();
    entries.push(
        ReleaseFileEntry::from_file(
            &tool_root,
            ReleaseFileSpec::new(
                "debug.pdb",
                "调试符号",
                ReleaseFileKind::License,
                "0.1.0",
                ReleaseArchitecture::Independent,
                ReleaseFilePolicy::Immutable,
                ReleaseFileValidator::None,
            ),
        )
        .unwrap(),
    );
    let manifest: ReleaseManifest = ReleaseManifest::new(entries);
    fs::write(
        fixture.root.join("manifest.json"),
        manifest.to_pretty_bytes().unwrap(),
    )
    .unwrap();

    let error: String = verify_release(&fixture.root).unwrap_err().to_string();
    assert!(error.contains("未在固定发布闭包定义"));
}

#[test]
fn packed_executable_extracts_sidecars_into_the_resource_directory() {
    let fixture = ReleaseFixture::new("payload-extract");
    let source_root = ToolRoot::open(&fixture.root).unwrap();
    let source_exe = fixture.parent.join("AzurLaneWorkbook.exe");
    fs::write(&source_exe, b"unpacked-exe").unwrap();
    embed_sidecar_payload(&source_root, &source_exe).unwrap();

    let empty_parent = fixture.parent.join("empty-copy");
    fs::create_dir_all(&empty_parent).unwrap();
    let copied_exe = empty_parent.join("AzurLaneWorkbook.exe");
    fs::copy(&source_exe, &copied_exe).unwrap();
    let resource_root =
        crate::adapters::tool_root::open_or_create_resource_root(&empty_parent).unwrap();
    ensure_extracted_release(&resource_root, &copied_exe).unwrap();
    verify_release(resource_root.as_path()).unwrap();
    assert!(!empty_parent.join("settings.json").exists());
    assert!(resource_root.as_path().join("settings.json").is_file());

    fs::write(
        resource_root.as_path().join("settings.json"),
        b"{\"schema_version\":1}",
    )
    .unwrap();
    ensure_extracted_release(&resource_root, &copied_exe).unwrap();
    assert_eq!(
        fs::read(resource_root.as_path().join("settings.json")).unwrap(),
        b"{\"schema_version\":1}"
    );
}

#[test]
fn release_errors_expose_stable_code_stage_and_context() {
    let error = ReleaseError::InvalidManifest {
        field: "schema_version".to_owned(),
        message: "版本不匹配".to_owned(),
    };
    assert_eq!(error.code(), "MANIFEST_INVALID");
    assert_eq!(error.stage(), "release.validate_manifest");
    assert_eq!(
        error.context().get("field").map(String::as_str),
        Some("schema_version")
    );

    let missing = ReleaseError::Io {
        stage: "release.locate_manifest",
        path: PathBuf::from("/tmp/manifest.json"),
        source: std::io::Error::from(std::io::ErrorKind::NotFound),
    };
    assert_eq!(missing.code(), "MANIFEST_INVALID");
    assert_eq!(missing.stage(), "release.locate_manifest");
    assert_eq!(
        missing.context().get("path").map(String::as_str),
        Some("manifest.json")
    );
}

struct ReleaseFixture {
    parent: PathBuf,
    root: PathBuf,
}

impl ReleaseFixture {
    fn new(label: &str) -> Self {
        let home: PathBuf = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let parent: PathBuf = home
            .join("suzushiro/scratch/azlw-release-tests")
            .join(format!("{label}-{}", unique_suffix()));
        let root: PathBuf = parent.join("AzurLaneWorkbook");
        for directory in [
            "runtime/adb",
            "runtime/inject",
            "runtime/resources/profiles",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        for (path, bytes) in [
            ("runtime/adb/adb.exe", b"adb".as_slice()),
            ("runtime/adb/AdbWinApi.dll", b"api".as_slice()),
            ("runtime/adb/NOTICE.txt", b"notice".as_slice()),
            (
                "runtime/adb/source.properties",
                b"Pkg.Revision=37.0.1\n".as_slice(),
            ),
            ("runtime/inject/azlw-loader-x86_64", b"loader".as_slice()),
            ("runtime/inject/libazlw-agent-x86_64.so", AGENT_FIXTURE),
        ] {
            fs::write(root.join(path), bytes).unwrap();
        }
        fs::write(root.join("settings.json"), SETTINGS).unwrap();
        fs::write(
            root.join("runtime/resources/profiles/default.json"),
            PROFILE,
        )
        .unwrap();
        fs::write(root.join("workbook-layout.xlsx"), WORKBOOK_LAYOUT).unwrap();
        Self { parent, root }
    }

    fn write_manifest(&self) {
        let tool_root: ToolRoot = ToolRoot::open(&self.root).unwrap();
        let entries: Vec<ReleaseFileEntry> = RELEASE_FILE_SPECS
            .iter()
            .map(|definition: &ReleaseFileSpec<'_>| {
                ReleaseFileEntry::from_file(&tool_root, *definition).unwrap()
            })
            .collect();
        let manifest: ReleaseManifest = ReleaseManifest::new(entries);
        fs::write(
            self.root.join("manifest.json"),
            manifest.to_pretty_bytes().unwrap(),
        )
        .unwrap();
    }
}

impl Drop for ReleaseFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

fn unique_suffix() -> String {
    let mut bytes: [u8; 16] = [0; 16];
    getrandom::fill(&mut bytes).unwrap();
    format!("{}-{:032x}", std::process::id(), u128::from_le_bytes(bytes))
}

#[test]
fn concurrent_first_launch_extracts_one_complete_release() {
    let fixture = ReleaseFixture::new("concurrent-extraction");
    let source = ToolRoot::open(&fixture.root).unwrap();
    let executable = fixture.parent.join("AzurLaneWorkbook.exe");
    fs::write(&executable, b"fixture-exe").unwrap();
    embed_sidecar_payload(&source, &executable).unwrap();
    let target = fixture.parent.join("concurrent-target");
    fs::create_dir(&target).unwrap();
    let root = ToolRoot::open(&target).unwrap();
    let barrier = std::sync::Barrier::new(8);
    let failures = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = &root;
                let executable = &executable;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    ensure_extracted_release(root, executable).map_err(|error| error.to_string())
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|thread| thread.join().unwrap().err())
            .collect::<Vec<_>>()
    });
    assert!(
        failures.is_empty(),
        "concurrent startup failed: {failures:#?}"
    );
    verify_release(&target).unwrap();
}

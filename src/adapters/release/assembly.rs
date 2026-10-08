//! 从已验证构建产物装配并发布固定闭包的 Windows 单目录发行包。

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::thread;
#[cfg(target_os = "windows")]
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

use super::super::tool_root::{RESOURCE_DIRECTORY, ToolRoot, ToolRootError};
use super::payload::embed_sidecar_payload;
use super::{
    RELEASE_FILE_SPECS, ReleaseError, ReleaseFileEntry, ReleaseFileSpec, ReleaseManifest,
    ReleaseVerificationReport, verify_release,
};
use suzushiro_content_digest::sha256_file;

const EXECUTABLE_NAME: &str = "AzurLaneWorkbook.exe";
const MANIFEST_NAME: &str = "manifest.json";
const SETTINGS_BYTES: &[u8] = include_bytes!("../../../settings.json");
const WORKBOOK_LAYOUT_BYTES: &[u8] = include_bytes!("../../../workbook-layout.xlsx");
const MAXIMUM_STAGING_ATTEMPTS: usize = 16;

/// 发布装配所需的最终目录、宿主程序和已经建立的 runtime 根目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseAssemblyOptions {
    output_directory: PathBuf,
    executable: PathBuf,
    runtime_root: PathBuf,
}

impl ReleaseAssemblyOptions {
    /// 建立不依赖当前工作目录推断产物位置的显式装配选项。
    pub fn new(
        output_directory: impl Into<PathBuf>,
        executable: impl Into<PathBuf>,
        runtime_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            output_directory: output_directory.into(),
            executable: executable.into(),
            runtime_root: runtime_root.into(),
        }
    }
}

/// 装配完成后可供构建流水线留存的稳定发布摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseAssemblyReport {
    /// 已经发布且可直接移动的最终目录绝对路径。
    pub output_directory: PathBuf,
    /// 清单和当前程序共同声明的产品版本。
    pub product_version: String,
    /// 包含 `manifest.json` 在内的最终普通文件数量。
    pub file_count: usize,
    /// 启动时必须同时匹配长度与 SHA-256 的文件数量。
    pub immutable_file_count: usize,
    /// 所有最终普通文件的总字节数。
    pub total_bytes: u64,
    /// 可用于发布流水线独立留存的清单 SHA-256。
    pub manifest_sha256: String,
}

/// 输入产物、受控写入、发布校验或失败清理没有满足装配契约。
#[derive(Debug, Error)]
pub enum ReleaseAssemblyError {
    #[error("装配参数 {field} 的路径 {path} 无效: {message}")]
    InvalidInput {
        field: &'static str,
        path: PathBuf,
        message: String,
    },
    #[error("发布目标已经存在，拒绝覆盖: {0}")]
    OutputExists(PathBuf),
    #[error("{stage} 访问 {path} 失败: {source}")]
    Io {
        stage: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("建立发布暂存目录名时操作系统随机源不可用: {0}")]
    RandomSource(#[from] getrandom::Error),
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error(
        "装配失败后清理暂存目录 {staging_directory} 失败；原操作: {operation}; 清理错误: {cleanup}"
    )]
    CleanupFailed {
        staging_directory: PathBuf,
        operation: Box<ReleaseAssemblyError>,
        #[source]
        cleanup: ToolRootError,
    },
}

/// 在目标同级暂存目录完成装配和反向校验，随后无覆盖地发布最终目录。
pub fn assemble_release(
    options: ReleaseAssemblyOptions,
) -> Result<ReleaseAssemblyReport, ReleaseAssemblyError> {
    let destination: OutputDestination = OutputDestination::new(&options.output_directory)?;
    let (staging_relative, staging_root): (PathBuf, ToolRoot) =
        create_staging_directory(&destination.parent_root)?;
    let assembled: AssembledRelease = match populate_staging(&options, &staging_root) {
        Ok(assembled) => assembled,
        Err(operation) => {
            return cleanup_after_error(&destination.parent_root, &staging_relative, operation);
        }
    };

    if let Err(source) = publish_directory(staging_root.as_path(), &destination.output_path) {
        let operation: ReleaseAssemblyError = if source.kind() == io::ErrorKind::AlreadyExists {
            ReleaseAssemblyError::OutputExists(destination.output_path)
        } else {
            ReleaseAssemblyError::Io {
                stage: "release_assembly.publish_directory",
                path: destination.output_path,
                source,
            }
        };
        return cleanup_after_error(&destination.parent_root, &staging_relative, operation);
    }

    Ok(ReleaseAssemblyReport {
        output_directory: destination.output_path,
        product_version: assembled.verification.product_version,
        file_count: assembled.verification.checked_files + 1,
        immutable_file_count: assembled.verification.immutable_files,
        total_bytes: assembled.total_bytes,
        manifest_sha256: assembled.manifest_sha256,
    })
}

struct OutputDestination {
    parent_root: ToolRoot,
    output_path: PathBuf,
}

impl OutputDestination {
    fn new(requested: &Path) -> Result<Self, ReleaseAssemblyError> {
        let output_name: &OsStr =
            requested
                .file_name()
                .ok_or_else(|| ReleaseAssemblyError::InvalidInput {
                    field: "output_directory",
                    path: requested.to_path_buf(),
                    message: "必须包含最终目录名".to_owned(),
                })?;
        let requested_parent: &Path = requested
            .parent()
            .filter(|parent: &&Path| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_root: ToolRoot = ToolRoot::open(requested_parent)?;
        let normalized_name: PathBuf =
            parent_root.validated_relative_path(Path::new(output_name))?;
        if normalized_name.components().count() != 1 {
            return Err(ReleaseAssemblyError::InvalidInput {
                field: "output_directory",
                path: requested.to_path_buf(),
                message: "最终目录名必须是单个普通路径段".to_owned(),
            });
        }
        let output_path: PathBuf = parent_root.as_path().join(&normalized_name);
        match fs::symlink_metadata(&output_path) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err(ReleaseAssemblyError::OutputExists(output_path)),
            Err(source) => {
                return Err(ReleaseAssemblyError::Io {
                    stage: "release_assembly.inspect_output",
                    path: output_path,
                    source,
                });
            }
        }
        Ok(Self {
            parent_root,
            output_path,
        })
    }
}

struct AssembledRelease {
    verification: ReleaseVerificationReport,
    total_bytes: u64,
    manifest_sha256: String,
}

fn create_staging_directory(
    parent_root: &ToolRoot,
) -> Result<(PathBuf, ToolRoot), ReleaseAssemblyError> {
    for _attempt in 0..MAXIMUM_STAGING_ATTEMPTS {
        let mut random: [u8; 16] = [0; 16];
        getrandom::fill(&mut random)?;
        let relative: PathBuf = PathBuf::from(format!(
            ".azlw-release-{}-{:032x}",
            std::process::id(),
            u128::from_le_bytes(random)
        ));
        let path: PathBuf = parent_root.as_path().join(&relative);
        match fs::create_dir(&path) {
            Ok(()) => match ToolRoot::open(&path) {
                Ok(root) => return Ok((relative, root)),
                Err(error) => {
                    return cleanup_after_error(parent_root, &relative, error.into());
                }
            },
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ReleaseAssemblyError::Io {
                    stage: "release_assembly.create_staging",
                    path,
                    source,
                });
            }
        }
    }
    Err(ReleaseAssemblyError::InvalidInput {
        field: "output_directory",
        path: parent_root.as_path().to_path_buf(),
        message: format!("连续 {MAXIMUM_STAGING_ATTEMPTS} 次暂存目录名冲突"),
    })
}

fn populate_staging(
    options: &ReleaseAssemblyOptions,
    staging_root: &ToolRoot,
) -> Result<AssembledRelease, ReleaseAssemblyError> {
    let executable: PathBuf = validated_executable(&options.executable)?;
    let runtime_root: ToolRoot = ToolRoot::open(&options.runtime_root)?;
    staging_root.ensure_directory(Path::new(RESOURCE_DIRECTORY))?;
    let resource_root = ToolRoot::open(&staging_root.as_path().join(RESOURCE_DIRECTORY))?;

    for specification in RELEASE_FILE_SPECS {
        let relative_path: &Path = Path::new(specification.path());
        if let Some(parent) = relative_path
            .parent()
            .filter(|parent: &&Path| !parent.as_os_str().is_empty())
        {
            resource_root.ensure_directory(parent)?;
        }
        match specification.path() {
            "settings.json" => write_file(&resource_root, relative_path, SETTINGS_BYTES)?,
            "workbook-layout.xlsx" => {
                write_file(&resource_root, relative_path, WORKBOOK_LAYOUT_BYTES)?;
            }
            path if path.starts_with("runtime/") => {
                let source: PathBuf = runtime_root.existing_file(relative_path)?;
                copy_file(&source, &resource_root, relative_path)?;
            }
            path => {
                return Err(ReleaseAssemblyError::InvalidInput {
                    field: "release_file_specs",
                    path: PathBuf::from(path),
                    message: "没有对应的装配来源".to_owned(),
                });
            }
        }
    }

    copy_file(&executable, staging_root, Path::new(EXECUTABLE_NAME))?;
    embed_sidecar_payload(
        &resource_root,
        &staging_root.as_path().join(EXECUTABLE_NAME),
    )?;

    let entries: Vec<ReleaseFileEntry> = RELEASE_FILE_SPECS
        .iter()
        .map(|specification: &ReleaseFileSpec<'_>| {
            ReleaseFileEntry::from_file(&resource_root, *specification)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let payload_bytes: u64 = entries.iter().map(ReleaseFileEntry::size_bytes).sum();
    let manifest: ReleaseManifest = ReleaseManifest::new(entries);
    let manifest_bytes: Vec<u8> = manifest.to_pretty_bytes()?;
    write_file(&resource_root, Path::new(MANIFEST_NAME), &manifest_bytes)?;

    let verification: ReleaseVerificationReport = verify_release(resource_root.as_path())?;
    let manifest_path: PathBuf = resource_root.existing_file(Path::new(MANIFEST_NAME))?;
    let manifest_sha256: String =
        sha256_file(&manifest_path).map_err(|source| ReleaseAssemblyError::Io {
            stage: "release_assembly.hash_manifest",
            path: manifest_path,
            source,
        })?;
    Ok(AssembledRelease {
        verification,
        total_bytes: payload_bytes + manifest_bytes.len() as u64,
        manifest_sha256,
    })
}

fn validated_executable(path: &Path) -> Result<PathBuf, ReleaseAssemblyError> {
    if path.file_name() != Some(OsStr::new(EXECUTABLE_NAME)) {
        return Err(ReleaseAssemblyError::InvalidInput {
            field: "executable",
            path: path.to_path_buf(),
            message: format!("文件名必须是 {EXECUTABLE_NAME}"),
        });
    }
    let parent: &Path = path
        .parent()
        .filter(|parent: &&Path| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let root: ToolRoot = ToolRoot::open(parent)?;
    root.existing_file(Path::new(EXECUTABLE_NAME))
        .map_err(Into::into)
}

fn copy_file(
    source: &Path,
    destination_root: &ToolRoot,
    destination_relative: &Path,
) -> Result<(), ReleaseAssemblyError> {
    let destination: PathBuf = destination_root.prepare_new_file(destination_relative)?;
    let mut reader: File = File::open(source).map_err(|source_error| ReleaseAssemblyError::Io {
        stage: "release_assembly.open_source",
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let mut writer: File = create_new_file(&destination)?;
    io::copy(&mut reader, &mut writer).map_err(|source_error| ReleaseAssemblyError::Io {
        stage: "release_assembly.copy_file",
        path: destination.clone(),
        source: source_error,
    })?;
    sync_file(writer, &destination)
}

fn write_file(
    destination_root: &ToolRoot,
    destination_relative: &Path,
    bytes: &[u8],
) -> Result<(), ReleaseAssemblyError> {
    let destination: PathBuf = destination_root.prepare_new_file(destination_relative)?;
    let mut file: File = create_new_file(&destination)?;
    file.write_all(bytes)
        .map_err(|source| ReleaseAssemblyError::Io {
            stage: "release_assembly.write_file",
            path: destination.clone(),
            source,
        })?;
    sync_file(file, &destination)
}

fn create_new_file(path: &Path) -> Result<File, ReleaseAssemblyError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| ReleaseAssemblyError::Io {
            stage: "release_assembly.create_file",
            path: path.to_path_buf(),
            source,
        })
}

fn sync_file(file: File, path: &Path) -> Result<(), ReleaseAssemblyError> {
    file.sync_all().map_err(|source| ReleaseAssemblyError::Io {
        stage: "release_assembly.sync_file",
        path: path.to_path_buf(),
        source,
    })
}

fn cleanup_after_error<T>(
    parent_root: &ToolRoot,
    staging_relative: &Path,
    operation: ReleaseAssemblyError,
) -> Result<T, ReleaseAssemblyError> {
    match parent_root.remove_directory_if_exists(staging_relative) {
        Ok(_) => Err(operation),
        Err(cleanup) => Err(ReleaseAssemblyError::CleanupFailed {
            staging_directory: parent_root.as_path().join(staging_relative),
            operation: Box::new(operation),
            cleanup,
        }),
    }
}

#[cfg(target_os = "windows")]
fn is_transient_directory_move(error: &io::Error) -> bool {
    // 5 是拒绝访问，32 是共享冲突。新写入的程序文件常被系统短暂占用。
    matches!(error.raw_os_error(), Some(5 | 32))
}

#[cfg(target_os = "windows")]
fn publish_directory(staging: &Path, output: &Path) -> Result<(), io::Error> {
    const ATTEMPTS: u32 = 8;
    let mut delay = Duration::from_millis(200);
    for attempt in 1..=ATTEMPTS {
        match move_directory_once(staging, output) {
            Ok(()) => return Ok(()),
            Err(source) => {
                let source_exists = staging.try_exists()?;
                let output_exists = output.try_exists()?;
                if !source_exists && output_exists {
                    return Ok(());
                }
                if output_exists
                    || !source_exists
                    || !is_transient_directory_move(&source)
                    || attempt == ATTEMPTS
                {
                    return Err(source);
                }
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(4000));
            }
        }
    }
    unreachable!("目录发布循环要么返回成功，要么返回最后一次错误")
}

#[cfg(target_os = "windows")]
fn move_directory_once(staging: &Path, output: &Path) -> Result<(), io::Error> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let staging: Vec<u16> = staging.as_os_str().encode_wide().chain(Some(0)).collect();
    let output: Vec<u16> = output.as_os_str().encode_wide().chain(Some(0)).collect();
    // 不设置替换标志，目标已存在时保持失败。
    if unsafe { MoveFileExW(staging.as_ptr(), output.as_ptr(), 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Linux/Android 使用内核原子 no-replace，消除存在性检查与目录改名之间的竞态。
#[cfg(any(target_os = "linux", target_os = "android"))]
fn publish_directory(staging: &Path, output: &Path) -> Result<(), io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let staging = CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "发布暂存目录路径包含空字节"))?;
    let output = CString::new(output.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "发布目标目录路径包含空字节"))?;

    // 两个路径均由当前进程持有至调用返回，且 CString 保证传给 libc 的指针以 NUL 结尾。
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            staging.as_ptr(),
            libc::AT_FDCWD,
            output.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_uint,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let source = io::Error::last_os_error();
        if matches!(
            source.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
        ) {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("当前文件系统不支持原子目录 no-overwrite 发布: {source}"),
            ))
        } else {
            Err(source)
        }
    }
}

/// 没有已验证原子 no-replace 原语的平台不得退回到检查后改名。
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "android")))]
fn publish_directory(_staging: &Path, _output: &Path) -> Result<(), io::Error> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "当前平台没有已验证的原子目录 no-overwrite 发布原语",
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use std::io;
    use std::path::{Path, PathBuf};
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use std::sync::{Arc, Barrier};
    #[cfg(any(target_os = "linux", target_os = "android"))]
    use std::thread::{self, JoinHandle};

    #[cfg(any(target_os = "linux", target_os = "android"))]
    use super::publish_directory;
    use super::{ReleaseAssemblyOptions, WORKBOOK_LAYOUT_BYTES, assemble_release};
    use crate::adapters::release::verify_release;

    const PROFILE: &str = include_str!("../../../runtime/resources/profiles/default.json");
    const AGENT_FIXTURE: &[u8] = b"fixture\nAZLW_AGENT_VERSION=0.11.0\0";

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_directory_move_retries_only_brief_access_and_sharing_failures() {
        assert!(super::is_transient_directory_move(
            &std::io::Error::from_raw_os_error(5)
        ));
        assert!(super::is_transient_directory_move(
            &std::io::Error::from_raw_os_error(32)
        ));
        assert!(!super::is_transient_directory_move(
            &std::io::Error::from_raw_os_error(183)
        ));
    }

    #[test]
    fn assembles_exact_release_and_produces_stable_manifest() {
        let fixture: AssemblyFixture = AssemblyFixture::new("exact");
        let first: PathBuf = fixture.output_root.join("first");
        let second: PathBuf = fixture.output_root.join("second");

        let first_report = assemble_release(fixture.options(&first)).unwrap();
        let second_report = assemble_release(fixture.options(&second)).unwrap();

        assert_eq!(first_report.file_count, 10);
        assert_eq!(first_report.manifest_sha256, second_report.manifest_sha256);
        assert!(first.join("AzurLaneWorkbook.exe").is_file());
        assert!(!first.join("settings.json").exists());
        assert_eq!(
            fs::read(first.join(".suzushiro/workbook-layout.xlsx")).unwrap(),
            WORKBOOK_LAYOUT_BYTES
        );
        assert_eq!(
            fs::read(first.join(".suzushiro/workbook-layout.xlsx")).unwrap(),
            fs::read(second.join(".suzushiro/workbook-layout.xlsx")).unwrap()
        );
        assert_eq!(
            verify_release(&first.join(".suzushiro"))
                .unwrap()
                .checked_files,
            9
        );
        assert_eq!(
            verify_release(&second.join(".suzushiro"))
                .unwrap()
                .checked_files,
            9
        );
    }

    #[test]
    fn rejects_existing_output_without_modifying_it() {
        let fixture: AssemblyFixture = AssemblyFixture::new("existing");
        let output: PathBuf = fixture.output_root.join("release");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("keep.txt"), b"keep").unwrap();

        let error: String = assemble_release(fixture.options(&output))
            .unwrap_err()
            .to_string();

        assert!(error.contains("拒绝覆盖"));
        assert_eq!(fs::read(output.join("keep.txt")).unwrap(), b"keep");
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn concurrent_publish_never_replaces_the_winning_directory() {
        let fixture = AssemblyFixture::new("publish-race");

        for iteration in 0..64 {
            let output = fixture.output_root.join(format!("release-{iteration}"));
            let first_staging = fixture.output_root.join(format!("first-{iteration}"));
            let second_staging = fixture.output_root.join(format!("second-{iteration}"));
            fs::create_dir(&first_staging).unwrap();
            fs::create_dir(&second_staging).unwrap();
            fs::write(first_staging.join("winner.txt"), b"first").unwrap();
            fs::write(second_staging.join("winner.txt"), b"second").unwrap();

            let barrier = Arc::new(Barrier::new(3));
            let first = spawn_publish("first", first_staging, output.clone(), barrier.clone());
            let second = spawn_publish("second", second_staging, output.clone(), barrier.clone());
            barrier.wait();

            let outcomes = [first.join().unwrap(), second.join().unwrap()];
            let winner = outcomes
                .iter()
                .find(|(_, _, result)| result.is_ok())
                .unwrap_or_else(|| panic!("必须有一个发布者成功，实际结果: {outcomes:?}"));
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|(_, _, result)| result.is_ok())
                    .count(),
                1
            );
            assert_eq!(
                fs::read(output.join("winner.txt")).unwrap(),
                winner.0.as_bytes()
            );

            for (_, staging, result) in outcomes {
                match result {
                    Ok(()) => assert!(!staging.exists()),
                    Err(error) => {
                        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
                        assert!(staging.exists());
                    }
                }
            }
        }
    }

    #[test]
    fn source_failure_removes_unpublished_staging_directory() {
        let fixture: AssemblyFixture = AssemblyFixture::new("cleanup");
        let output: PathBuf = fixture.output_root.join("release");
        fs::remove_file(
            fixture
                .runtime_root
                .join("runtime/inject/libazlw-agent-x86_64.so"),
        )
        .unwrap();

        assert!(assemble_release(fixture.options(&output)).is_err());
        assert!(!output.exists());
        assert_no_staging_directory(&fixture.output_root);
    }

    #[test]
    fn assembles_updated_adb_revision_from_source_metadata() {
        let fixture: AssemblyFixture = AssemblyFixture::new("adb-revision");
        let output: PathBuf = fixture.output_root.join("release");
        fs::write(
            fixture.runtime_root.join("runtime/adb/source.properties"),
            b"Pkg.Revision=38.0.0\n",
        )
        .unwrap();

        assemble_release(fixture.options(&output)).unwrap();
        verify_release(&output.join(".suzushiro")).unwrap();
        let manifest = fs::read_to_string(output.join(".suzushiro/manifest.json")).unwrap();
        assert!(manifest.contains("38.0.0"));
        assert_no_staging_directory(&fixture.output_root);
    }

    #[test]
    fn outdated_agent_version_removes_unpublished_staging_directory() {
        let fixture: AssemblyFixture = AssemblyFixture::new("agent-version");
        let output: PathBuf = fixture.output_root.join("release");
        fs::write(
            fixture
                .runtime_root
                .join("runtime/inject/libazlw-agent-x86_64.so"),
            b"fixture\nAZLW_AGENT_VERSION=0.7.6\0",
        )
        .unwrap();

        let error: String = assemble_release(fixture.options(&output))
            .unwrap_err()
            .to_string();

        assert!(error.contains("期望 0.11.0，实际 0.7.6"));
        assert!(!output.exists());
        assert_no_staging_directory(&fixture.output_root);
    }

    struct AssemblyFixture {
        parent: PathBuf,
        executable: PathBuf,
        runtime_root: PathBuf,
        output_root: PathBuf,
    }

    impl AssemblyFixture {
        fn new(label: &str) -> Self {
            let parent: PathBuf = test_scratch_root()
                .join("azlw-assembly-tests")
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
            fs::write(&executable, b"windows executable").unwrap();
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
                    PROFILE.as_bytes(),
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

        fn options(&self, output: &Path) -> ReleaseAssemblyOptions {
            ReleaseAssemblyOptions::new(output, &self.executable, &self.runtime_root)
        }
    }

    impl Drop for AssemblyFixture {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.parent)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!(
                    "清理发布装配测试目录 {} 失败: {error}",
                    self.parent.display()
                );
            }
        }
    }

    fn assert_no_staging_directory(output_root: &Path) {
        let remaining: Vec<String> = fs::read_dir(output_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            remaining
                .iter()
                .all(|name: &String| !name.starts_with(".azlw-release-"))
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn test_scratch_root() -> PathBuf {
        std::env::var_os("AZLW_TEST_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("suzushiro/scratch"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn test_scratch_root() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE")
            .join("suzushiro/scratch")
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn spawn_publish(
        label: &'static str,
        staging: PathBuf,
        output: PathBuf,
        barrier: Arc<Barrier>,
    ) -> JoinHandle<(&'static str, PathBuf, Result<(), io::Error>)> {
        thread::spawn(move || {
            barrier.wait();
            let result = publish_directory(&staging, &output);
            (label, staging, result)
        })
    }

    fn unique_suffix() -> String {
        let mut bytes: [u8; 16] = [0; 16];
        getrandom::fill(&mut bytes).unwrap();
        format!("{}-{:032x}", std::process::id(), u128::from_le_bytes(bytes))
    }
}

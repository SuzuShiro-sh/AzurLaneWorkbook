//! 负责 Native 测试的主机选项校验和构建工具调用。

#[cfg(target_os = "windows")]
use std::fs;
use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use suzushiro_host_command::{NativeOutput, run_native};

use super::contracts::NativeTestRunnerError;
#[cfg(target_os = "windows")]
use super::contracts::{
    NativeTestHostCommandEvidence, NativeTestRunnerOptions, NativeTestToolchain,
};
#[cfg(target_os = "windows")]
use crate::adapters::tool_root::{ToolRoot, has_link_semantics};
#[cfg(target_os = "windows")]
use suzushiro_content_digest::sha256_bytes;

#[cfg(target_os = "windows")]
pub(super) struct ValidatedOptions {
    pub(super) repository_root: ToolRoot,
    pub(super) tool_root: ToolRoot,
    pub(super) cmake_executable: PathBuf,
    pub(super) ctest_executable: PathBuf,
    pub(super) ninja_executable: PathBuf,
    pub(super) ndk_toolchain: PathBuf,
    pub(super) build_relative: PathBuf,
    pub(super) build_root: ToolRoot,
    pub(super) serial: String,
}

#[cfg(target_os = "windows")]
pub(super) fn validate_options(
    options: NativeTestRunnerOptions,
) -> Result<ValidatedOptions, NativeTestRunnerError> {
    let NativeTestToolchain {
        cmake_executable,
        ctest_executable,
        ninja_executable,
        ndk_toolchain,
    } = options.toolchain;
    let repository_root = ToolRoot::open(&options.repository_root).map_err(|error| {
        NativeTestRunnerError::InvalidPath {
            message: format!("仓库根目录无效: {error}"),
        }
    })?;
    repository_root
        .existing_file(Path::new("native/CMakeLists.txt"))
        .map_err(|error| NativeTestRunnerError::InvalidPath {
            message: format!("仓库缺少 Native CMake 入口: {error}"),
        })?;
    let tool_root =
        ToolRoot::open(&options.tool_root).map_err(|error| NativeTestRunnerError::InvalidPath {
            message: format!("Native 测试工具根目录无效: {error}"),
        })?;
    let build_relative = repository_root
        .validated_relative_path(&options.build_relative)
        .map_err(|error| NativeTestRunnerError::InvalidOptions {
            message: format!("Native 构建目录必须是仓库内相对路径: {error}"),
        })?;
    if !build_relative.starts_with("target") || build_relative.components().count() < 2 {
        return Err(NativeTestRunnerError::InvalidOptions {
            message: "Native 构建目录必须位于仓库 target 的直接或间接子目录".to_owned(),
        });
    }
    if options.serial.trim() != options.serial || options.serial.is_empty() {
        return Err(NativeTestRunnerError::InvalidOptions {
            message: "ADB serial 不能为空或包含首尾空白".to_owned(),
        });
    }
    let build_directory = repository_root
        .ensure_directory(&build_relative)
        .map_err(|error| NativeTestRunnerError::InvalidPath {
            message: format!("建立受控 Native 构建目录失败: {error}"),
        })?;
    let build_root =
        ToolRoot::open(&build_directory).map_err(|error| NativeTestRunnerError::InvalidPath {
            message: format!("打开受控 Native 构建目录失败: {error}"),
        })?;
    Ok(ValidatedOptions {
        repository_root,
        tool_root,
        cmake_executable: validate_external_file(&cmake_executable, "CMake")?,
        ctest_executable: validate_external_file(&ctest_executable, "CTest")?,
        ninja_executable: validate_external_file(&ninja_executable, "Ninja")?,
        ndk_toolchain: validate_external_file(&ndk_toolchain, "Android NDK toolchain")?,
        build_relative,
        build_root,
        serial: options.serial,
    })
}

#[cfg(target_os = "windows")]
fn validate_external_file(path: &Path, label: &str) -> Result<PathBuf, NativeTestRunnerError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| NativeTestRunnerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if has_link_semantics(&metadata) || !metadata.is_file() {
        return Err(NativeTestRunnerError::InvalidPath {
            message: format!("{label} 必须是普通文件且不能是链接: {}", path.display()),
        });
    }
    fs::canonicalize(path).map_err(|source| NativeTestRunnerError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(target_os = "windows")]
pub(super) struct HostCommandResult {
    pub(super) evidence: NativeTestHostCommandEvidence,
    pub(super) stdout: String,
}

#[cfg(target_os = "windows")]
pub(super) fn run_host_checked(
    executable: &Path,
    arguments: &[String],
    stage: &'static str,
) -> Result<HostCommandResult, NativeTestRunnerError> {
    let output: NativeOutput = run_native(&path_text(executable, stage)?, arguments, stage)
        .map_err(|error| NativeTestRunnerError::HostCommand {
            stage,
            message: error.to_string(),
        })?;
    if output.exit_code != 0 {
        return Err(NativeTestRunnerError::HostCommand {
            stage,
            message: format!(
                "退出码为 {}，stdout={:?}，stderr={:?}",
                output.exit_code, output.stdout, output.stderr
            ),
        });
    }
    Ok(HostCommandResult {
        evidence: NativeTestHostCommandEvidence {
            stage,
            exit_code: output.exit_code,
            stdout_sha256: sha256_bytes(output.stdout.as_bytes()),
            stderr_sha256: sha256_bytes(output.stderr.as_bytes()),
        },
        stdout: output.stdout,
    })
}

#[cfg(target_os = "windows")]
pub(super) fn path_text(path: &Path, label: &str) -> Result<String, NativeTestRunnerError> {
    windows_command_path_text(path, label)
}

/// 传给可能调用 `cmd.exe` 的工具时，把本地盘符扩展路径恢复为普通绝对路径。
#[cfg(any(target_os = "windows", test))]
pub(super) fn windows_command_path_text(
    path: &Path,
    label: &str,
) -> Result<String, NativeTestRunnerError> {
    let text = path
        .to_str()
        .ok_or_else(|| NativeTestRunnerError::InvalidPath {
            message: format!("{label} 不是有效 Unicode 路径: {}", path.display()),
        })?;
    let Some(local_path) = text.strip_prefix(r"\\?\") else {
        return Ok(text.to_owned());
    };
    let bytes = local_path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return Ok(local_path.to_owned());
    }
    Err(NativeTestRunnerError::InvalidPath {
        message: format!("{label} 不能使用 Windows 扩展 UNC 或设备路径: {text}"),
    })
}

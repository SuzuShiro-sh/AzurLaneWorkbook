//! 校验随包 ADB 闭包，并收敛独立服务的所有权与清理结果。

use std::fs;
use std::path::Path;

#[cfg(target_os = "windows")]
use super::PortableCleanupEvidence;
use super::{
    AdbBundleInspection, AdbContractError, BundleFileEvidence, PortableProbeError, adb_failure,
};
use super::{AdbFallbackError, PortableMode, PortableProbeOptions};
use suzushiro_adb::AdbBundle;
#[cfg(target_os = "windows")]
use suzushiro_adb::{AdbShutdownEvidence, IsolatedAdbError, OwnedAdbServer};
use suzushiro_content_digest::sha256_file as shared_sha256_file;

const MAX_BUNDLE_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// 读取固定闭包摘要，所有文件必须受限大小并位于工具根目录。
fn inspect_loaded_bundle(bundle: &AdbBundle) -> Result<AdbBundleInspection, PortableProbeError> {
    let mut files: Vec<BundleFileEvidence> = Vec::with_capacity(4);
    for path in bundle.files() {
        let metadata: fs::Metadata =
            fs::metadata(path).map_err(|source| PortableProbeError::Io {
                stage: "portable.read_bundle_metadata",
                path: path.to_path_buf(),
                source,
            })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BUNDLE_FILE_BYTES {
            return Err(adb_failure(
                "adb.bundle",
                AdbContractError {
                    message: format!(
                        "随包文件 {} 必须是 1 至 {MAX_BUNDLE_FILE_BYTES} 字节的普通文件",
                        path.display()
                    ),
                },
            ));
        }
        let relative: &Path = path.strip_prefix(bundle.tool_root()).map_err(|_| {
            adb_failure(
                "adb.bundle",
                AdbContractError {
                    message: format!("随包文件越过工具根目录: {}", path.display()),
                },
            )
        })?;
        files.push(BundleFileEvidence {
            relative_path: relative.to_string_lossy().replace('\\', "/"),
            size_bytes: metadata.len(),
            sha256: sha256_file(path)?,
        });
    }
    Ok(AdbBundleInspection {
        revision: bundle.revision().to_owned(),
        state_root: bundle.state_root().to_path_buf(),
        files,
    })
}

/// 流式计算单个随包文件摘要，避免按文件大小分配内存。
fn sha256_file(path: &Path) -> Result<String, PortableProbeError> {
    shared_sha256_file(path).map_err(|source| PortableProbeError::Io {
        stage: "portable.hash_bundle_file",
        path: path.to_path_buf(),
        source,
    })
}

/// 关闭一次便携操作持有的 ADB，并把操作错误与清理错误分别保留。
#[cfg(target_os = "windows")]
pub(super) fn finish_portable_operation<T>(
    mut server: OwnedAdbServer,
    operation: Result<T, PortableProbeError>,
) -> Result<(T, PortableCleanupEvidence), PortableProbeError> {
    let shutdown_result: Result<AdbShutdownEvidence, IsolatedAdbError> = server.shutdown();
    match (operation, shutdown_result) {
        (Ok(value), Ok(shutdown)) => Ok((
            value,
            PortableCleanupEvidence {
                adb_process_stopped: shutdown.process_stopped,
                adb_port_released: shutdown.port_released,
                adb_temporary_root_removed: shutdown.temporary_root_removed,
            },
        )),
        (Err(operation), Ok(_)) => Err(operation),
        (Ok(_), Err(adb_error)) => Err(adb_failure("adb.cleanup", adb_error)),
        (Err(operation), Err(adb_error)) => Err(PortableProbeError::OperationAndAdbCleanup {
            operation: operation.to_string(),
            adb: adb_error.to_string(),
        }),
    }
}

/// 自动模式把加载和内容检查作为同一提示门禁，手工模式只接受指定闭包。
pub(super) fn load_and_inspect_configured_adb_bundle(
    options: &PortableProbeOptions,
    related: Option<&crate::adapters::RelatedLogSink>,
) -> Result<(AdbBundle, AdbBundleInspection), PortableProbeError> {
    let Some(relative_path) = &options.adb_relative_path_hint else {
        return load_and_inspect_adb_bundle(&options.tool_root, None, related);
    };
    match load_and_inspect_adb_bundle(&options.tool_root, Some(relative_path), related) {
        Ok(loaded) => Ok(loaded),
        Err(configured_error) if options.mode == PortableMode::Auto => {
            load_and_inspect_adb_bundle(&options.tool_root, None, related).map_err(|fallback| {
                adb_failure(
                    "adb.bundle",
                    AdbFallbackError {
                        configured_path: relative_path.clone(),
                        configured: configured_error.to_string(),
                        fallback: Box::new(fallback),
                    },
                )
            })
        }
        Err(source) => Err(source),
    }
}

/// 加载一个工具根目录内的 ADB 闭包，并在返回前完成大小、文件类型和摘要检查。
fn load_and_inspect_adb_bundle(
    tool_root: &Path,
    executable_relative_path: Option<&Path>,
    related: Option<&crate::adapters::RelatedLogSink>,
) -> Result<(AdbBundle, AdbBundleInspection), PortableProbeError> {
    let bundle: AdbBundle = match executable_relative_path {
        Some(path) => crate::adapters::device::adb_config::load_adb_bundle_from_executable(
            tool_root, path, related,
        ),
        None => crate::adapters::device::adb_config::load_adb_bundle(tool_root, related),
    }
    .map_err(|source| adb_failure("adb.bundle", source))?;
    let inspection: AdbBundleInspection = inspect_loaded_bundle(&bundle)?;
    Ok((bundle, inspection))
}

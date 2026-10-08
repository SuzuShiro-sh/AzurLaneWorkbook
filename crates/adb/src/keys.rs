//! 在受控目录内建立和校验 ADB 密钥。
use crate::environment::windows_path;
use crate::{AdbBundle, IsolatedAdbError};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};
use suzushiro_host_command::{NativeOutput, run_native_with_policy};
const MAX_KEY_BYTES: u64 = 64 * 1024;
/// 首次运行时用随包 ADB 创建工具内密钥对，既有半成品会明确失败。
#[cfg(target_os = "windows")]
pub(crate) fn prepare_key(bundle: &AdbBundle) -> Result<(), IsolatedAdbError> {
    let private_relative_path = bundle.state_relative.join("keys/adbkey");
    let private_relative = private_relative_path.as_path();
    let public_relative_path = bundle.state_relative.join("keys/adbkey.pub");
    let public_relative = public_relative_path.as_path();
    let private_exists: bool = validated_optional_file(bundle, private_relative)?;
    let public_exists: bool = validated_optional_file(bundle, public_relative)?;
    if private_exists && public_exists {
        validate_key_file(&bundle.tool_root.existing_file(private_relative)?)?;
        validate_key_file(&bundle.tool_root.existing_file(public_relative)?)?;
        return Ok(());
    }
    if private_exists || public_exists {
        return Err(IsolatedAdbError::InvalidBundle {
            path: bundle.key_root.clone(),
            message: "ADB 密钥对只有一个文件，拒绝覆盖未知状态".to_owned(),
        });
    }

    let nonce: u128 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary_key_relative: PathBuf = bundle
        .state_relative
        .join("keys")
        .join(format!("adbkey.new-{nonce}"));
    let temporary_public_relative: PathBuf = append_path_suffix(&temporary_key_relative, ".pub");
    let temporary_key: PathBuf = bundle.tool_root.prepare_new_file(&temporary_key_relative)?;
    let temporary_public: PathBuf = bundle
        .tool_root
        .prepare_new_file(&temporary_public_relative)?;
    let executable: String = windows_path(&bundle.executable)?;
    let temporary_key_text: String = windows_path(&temporary_key)?;
    let output: NativeOutput = match run_native_with_policy(
        &executable,
        &["keygen".to_owned(), temporary_key_text],
        "adb.generate_key",
        bundle.process_policy(),
    ) {
        Ok(output) => output,
        Err(error) => {
            let operation: IsolatedAdbError = IsolatedAdbError::NativeCommand(error);
            return Err(key_error_after_cleanup(
                bundle,
                operation,
                &[
                    (&temporary_key_relative, None),
                    (&temporary_public_relative, None),
                ],
            ));
        }
    };
    if output.exit_code != 0 {
        let operation: IsolatedAdbError = IsolatedAdbError::CommandStatus {
            stage: "adb.generate_key",
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        };
        return Err(key_error_after_cleanup(
            bundle,
            operation,
            &[
                (&temporary_key_relative, None),
                (&temporary_public_relative, None),
            ],
        ));
    }
    let (private_file, public_file) = match validate_key_file(&temporary_key)
        .and_then(|private| validate_key_file(&temporary_public).map(|public| (private, public)))
    {
        Ok(files) => files,
        Err(operation) => {
            return Err(key_error_after_cleanup(
                bundle,
                operation,
                &[
                    (&temporary_key_relative, None),
                    (&temporary_public_relative, None),
                ],
            ));
        }
    };
    if let Err(error) =
        bundle
            .tool_root
            .rename_new_file(&public_file, &temporary_public_relative, public_relative)
    {
        let operation: IsolatedAdbError = error.into();
        return Err(key_error_after_cleanup(
            bundle,
            operation,
            &[
                (&temporary_key_relative, Some(&private_file)),
                (&temporary_public_relative, Some(&public_file)),
            ],
        ));
    }
    if let Err(error) =
        bundle
            .tool_root
            .rename_new_file(&private_file, &temporary_key_relative, private_relative)
    {
        let operation: IsolatedAdbError = error.into();
        return Err(key_error_after_cleanup(
            bundle,
            operation,
            &[
                (&temporary_key_relative, Some(&private_file)),
                (public_relative, Some(&public_file)),
            ],
        ));
    }
    Ok(())
}

/// 检查可选密钥路径；既有条目必须是工具内普通文件，缺失路径必须可排他新建。
#[cfg(target_os = "windows")]
fn validated_optional_file(
    bundle: &AdbBundle,
    relative_path: &Path,
) -> Result<bool, IsolatedAdbError> {
    let path: PathBuf = bundle.tool_root.as_path().join(relative_path);
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            bundle.tool_root.existing_file(relative_path)?;
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            bundle.tool_root.prepare_new_file(relative_path)?;
            Ok(false)
        }
        Err(source) => Err(IsolatedAdbError::Io {
            stage: "adb.inspect_optional_key",
            path,
            source,
        }),
    }
}

/// 在操作系统原生路径后追加固定后缀，不经有损的显示文本往返。
#[cfg(target_os = "windows")]
fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

/// 密钥必须是受限大小的非空普通文件。
#[cfg(target_os = "windows")]
fn validate_key_file(path: &Path) -> Result<fs::File, IsolatedAdbError> {
    let file = fs::File::open(path).map_err(|source| IsolatedAdbError::Io {
        stage: "adb.open_key",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata: fs::Metadata = file.metadata().map_err(|source| IsolatedAdbError::Io {
        stage: "adb.read_key_metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_KEY_BYTES {
        return Err(IsolatedAdbError::InvalidBundle {
            path: path.to_path_buf(),
            message: format!("密钥必须是 1 至 {MAX_KEY_BYTES} 字节的普通文件"),
        });
    }
    Ok(file)
}

/// 删除当前密钥流程创建的已知路径，并在双重失败时保留全部上下文。
#[cfg(target_os = "windows")]
fn key_error_after_cleanup(
    bundle: &AdbBundle,
    operation: IsolatedAdbError,
    staged_relative_paths: &[(&Path, Option<&fs::File>)],
) -> IsolatedAdbError {
    let mut cleanup_failures: Vec<String> = Vec::new();
    for (relative_path, expected_file) in staged_relative_paths {
        if let Err(error) = bundle
            .tool_root
            .remove_file_if_exists(relative_path, *expected_file)
        {
            cleanup_failures.push(error.to_string());
        }
    }
    if cleanup_failures.is_empty() {
        operation
    } else {
        IsolatedAdbError::KeyStagingCleanup {
            operation: operation.to_string(),
            cleanup: cleanup_failures.join(" | "),
        }
    }
}

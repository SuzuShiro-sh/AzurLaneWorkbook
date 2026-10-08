//! 构造随包 ADB 进程的显式环境。
use crate::IsolatedAdbError;
use std::{env, path::Path};
use suzushiro_host_command::NativeCommandPolicy;
/// 只保留 ADB 运行所需的系统根目录和工具内可写位置。
#[cfg(target_os = "windows")]
pub(crate) fn create_process_policy(
    executable: &Path,
    home_root: &Path,
    roaming_root: &Path,
    local_root: &Path,
    temporary_root: &Path,
    key_path: &Path,
) -> Result<NativeCommandPolicy, IsolatedAdbError> {
    let working_directory: &Path =
        executable
            .parent()
            .ok_or_else(|| IsolatedAdbError::InvalidBundle {
                path: executable.to_path_buf(),
                message: "adb.exe 缺少父目录".to_owned(),
            })?;
    let system_root: String =
        env::var("SystemRoot").map_err(|source| IsolatedAdbError::HostEnvironment {
            name: "SystemRoot",
            message: source.to_string(),
        })?;
    let home: String = windows_path(home_root)?;
    let roaming: String = windows_path(roaming_root)?;
    let local: String = windows_path(local_root)?;
    let temporary: String = windows_path(temporary_root)?;
    let key: String = windows_path(key_path)?;

    Ok(NativeCommandPolicy::isolated(
        windows_path(working_directory)?,
        vec![
            ("SystemRoot".to_owned(), system_root.clone()),
            ("WINDIR".to_owned(), system_root),
            ("HOME".to_owned(), home.clone()),
            ("USERPROFILE".to_owned(), home.clone()),
            ("ANDROID_USER_HOME".to_owned(), home.clone()),
            ("ANDROID_SDK_HOME".to_owned(), home),
            ("APPDATA".to_owned(), roaming),
            ("LOCALAPPDATA".to_owned(), local),
            ("ADB_VENDOR_KEYS".to_owned(), key),
            ("TEMP".to_owned(), temporary.clone()),
            ("TMP".to_owned(), temporary),
        ],
    ))
}

/// Windows 子进程只接受有效 Unicode 原生路径。
#[cfg(target_os = "windows")]
pub(crate) fn windows_path(path: &Path) -> Result<String, IsolatedAdbError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| IsolatedAdbError::InvalidBundle {
            path: path.to_path_buf(),
            message: "Windows 原生路径不是有效 Unicode".to_owned(),
        })
}

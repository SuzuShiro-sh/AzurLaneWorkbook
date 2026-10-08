//! 同目录 XLSX 临时文件验证和平台原子替换。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::XlsxError;
use super::editor::{TextCellEditEvidence, edit_text_cell_to_new_file};
use super::package::cleanup_created_file;

const TEMPORARY_NONCE_BYTES: usize = 16;

/// 已验证编辑经过同目录原子替换后的内部结果。
pub struct AtomicWorkbookEdit<T> {
    pub value: T,
    pub replacement_method: &'static str,
    pub temporary_file_removed: bool,
}

/// 将已经写完并验证的同目录普通 XLSX 临时文件原子发布或替换到最终路径。
pub fn replace_validated_workbook(
    temporary_path: &Path,
    workbook_path: &Path,
) -> Result<(), XlsxError> {
    let _lock = lock_workbook(workbook_path)?;
    replace_validated_workbook_locked(temporary_path, workbook_path)
}

fn replace_validated_workbook_locked(
    temporary_path: &Path,
    workbook_path: &Path,
) -> Result<(), XlsxError> {
    validate_replacement_paths(temporary_path, workbook_path)?;
    replace_file(temporary_path, workbook_path)?;
    if temporary_path.exists() {
        return Err(XlsxError::InvalidPath {
            path: temporary_path.to_path_buf(),
            message: "原子替换成功后临时文件仍然存在".to_owned(),
        });
    }
    Ok(())
}

/// 原位工作簿编辑通过临时文件验证并完成替换后的证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AtomicTextCellEditEvidence {
    pub edit: TextCellEditEvidence,
    pub replacement_method: String,
    pub temporary_file_removed: bool,
}

/// 通过同目录临时工作簿验证后原子替换现有目标文件。
pub fn edit_text_cell_atomically(
    workbook_path: &Path,
    sheet_name: &str,
    cell_reference: &str,
    replacement_value: &str,
    allow_external: fn(&super::package::PackageRelationship) -> bool,
) -> Result<AtomicTextCellEditEvidence, XlsxError> {
    let atomic = edit_workbook_atomically(workbook_path, |temporary_path| {
        edit_text_cell_to_new_file(
            workbook_path,
            temporary_path,
            sheet_name,
            cell_reference,
            replacement_value,
            allow_external,
        )
    })?;
    Ok(AtomicTextCellEditEvidence {
        edit: atomic.value,
        replacement_method: atomic.replacement_method.to_owned(),
        temporary_file_removed: atomic.temporary_file_removed,
    })
}

/// 统一执行“新临时文件写入并验证、权限复制、同目录原子替换”的工作簿编辑流程。
///
/// 编辑闭包必须以排他方式建立临时文件，并负责清理自己在失败前建立的文件；闭包成功
/// 返回后，本函数接管临时文件的权限复制、替换和清理责任。
pub fn edit_workbook_atomically<T, E: From<XlsxError> + std::fmt::Display>(
    workbook_path: &Path,
    edit: impl FnOnce(&Path) -> Result<T, E>,
) -> Result<AtomicWorkbookEdit<T>, E> {
    edit_workbook_atomically_with_pre_publish(workbook_path, edit, |_| Ok(()))
}

/// 在最终替换前再次执行调用方提供的源文件身份核对。
pub fn edit_workbook_atomically_with_pre_publish<T, E: From<XlsxError> + std::fmt::Display>(
    workbook_path: &Path,
    edit: impl FnOnce(&Path) -> Result<T, E>,
    pre_publish: impl FnOnce(&Path) -> Result<(), E>,
) -> Result<AtomicWorkbookEdit<T>, E> {
    let original_permissions: std::fs::Permissions =
        validate_existing_workbook_path(workbook_path)?;
    let temporary_path: PathBuf = temporary_workbook_path(workbook_path)?;
    let value = edit(&temporary_path)?;

    if let Err(source) = std::fs::set_permissions(&temporary_path, original_permissions) {
        let operation = XlsxError::Io {
            stage: "复制原工作簿权限",
            path: temporary_path.clone(),
            source,
        };
        return Err(cleanup_created_file(&temporary_path, operation).into());
    }
    // 锁文件独立于被替换的工作簿，核对与发布必须持有同一个跨进程锁。
    let _lock = match lock_workbook(workbook_path) {
        Ok(lock) => lock,
        Err(operation) => return Err(cleanup_created_file(&temporary_path, operation).into()),
    };
    if let Err(operation) = pre_publish(workbook_path) {
        return Err(cleanup_created_file(&temporary_path, operation));
    }
    if let Err(operation) = replace_validated_workbook_locked(&temporary_path, workbook_path) {
        return Err(cleanup_created_file(&temporary_path, operation).into());
    }
    let temporary_file_removed = !temporary_path.exists();
    if !temporary_file_removed {
        return Err(XlsxError::InvalidPath {
            path: temporary_path,
            message: "原子替换成功后临时文件仍然存在".to_owned(),
        }
        .into());
    }
    Ok(AtomicWorkbookEdit {
        value,
        replacement_method: replacement_method(),
        temporary_file_removed,
    })
}

fn lock_workbook(path: &Path) -> Result<std::fs::File, XlsxError> {
    let error = |source| XlsxError::Io {
        stage: "锁定工作簿发布",
        path: path.to_path_buf(),
        source,
    };
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let root = suzushiro_controlled_root::ControlledRoot::open(parent)
        .map_err(|source| error(std::io::Error::other(source)))?;
    let name = path
        .file_name()
        .ok_or_else(|| error(std::io::Error::other("工作簿缺少文件名")))?;
    let mut lock_name = OsString::from(".");
    lock_name.push(name);
    lock_name.push(".write.lock");
    root.lock_file(Path::new(&lock_name), std::time::Duration::from_secs(5))
        .map_err(error)
}

/// 原子发布只接受同一普通目录下的普通 `.xlsx` 临时文件和非链接目标。
fn validate_replacement_paths(
    temporary_path: &Path,
    workbook_path: &Path,
) -> Result<(), XlsxError> {
    let temporary_metadata =
        std::fs::symlink_metadata(temporary_path).map_err(|source| XlsxError::Io {
            stage: "读取原子替换临时文件元数据",
            path: temporary_path.to_path_buf(),
            source,
        })?;
    let valid_extension = |path: &Path| {
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("xlsx"))
    };
    if !temporary_metadata.is_file()
        || temporary_metadata.file_type().is_symlink()
        || !valid_extension(temporary_path)
        || !valid_extension(workbook_path)
        || temporary_path.parent().is_none()
        || temporary_path.parent() != workbook_path.parent()
        || workbook_path.file_name().is_none()
    {
        return Err(XlsxError::InvalidPath {
            path: workbook_path.to_path_buf(),
            message: "原子替换要求同目录普通 .xlsx 临时文件和最终路径".to_owned(),
        });
    }
    match std::fs::symlink_metadata(workbook_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(XlsxError::InvalidPath {
            path: workbook_path.to_path_buf(),
            message: "原子替换目标必须缺失或为普通 .xlsx 文件".to_owned(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(XlsxError::Io {
            stage: "读取原子替换目标元数据",
            path: workbook_path.to_path_buf(),
            source,
        }),
    }
}

/// 确认原位更新目标是已存在的普通 XLSX 文件而不是符号链接。
fn validate_existing_workbook_path(path: &Path) -> Result<std::fs::Permissions, XlsxError> {
    let metadata: std::fs::Metadata =
        std::fs::symlink_metadata(path).map_err(|source: std::io::Error| XlsxError::Io {
            stage: "读取原位更新目标元数据",
            path: path.to_path_buf(),
            source,
        })?;
    let extension: Option<&str> = path
        .extension()
        .and_then(|value: &std::ffi::OsStr| value.to_str());
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || !extension.is_some_and(|value: &str| value.eq_ignore_ascii_case("xlsx"))
        || path.parent().is_none()
        || path.file_name().is_none()
    {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "原位更新目标必须是已存在的普通 .xlsx 文件".to_owned(),
        });
    }
    Ok(metadata.permissions())
}

/// 使用操作系统随机源构造目标同目录且保持 XLSX 扩展名的临时路径。
fn temporary_workbook_path(workbook_path: &Path) -> Result<PathBuf, XlsxError> {
    let parent: &Path = workbook_path
        .parent()
        .ok_or_else(|| XlsxError::InvalidPath {
            path: workbook_path.to_path_buf(),
            message: "工作簿缺少父目录".to_owned(),
        })?;
    let file_name: &std::ffi::OsStr =
        workbook_path
            .file_name()
            .ok_or_else(|| XlsxError::InvalidPath {
                path: workbook_path.to_path_buf(),
                message: "工作簿缺少文件名".to_owned(),
            })?;
    let mut nonce_bytes: [u8; TEMPORARY_NONCE_BYTES] = [0; TEMPORARY_NONCE_BYTES];
    getrandom::fill(&mut nonce_bytes).map_err(XlsxError::RandomSource)?;
    let nonce: u128 = u128::from_le_bytes(nonce_bytes);
    let mut temporary_name: OsString = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".xlsx-toolkit-{nonce:032x}.tmp.xlsx"));
    let temporary_path: PathBuf = parent.join(temporary_name);
    if temporary_path.exists() {
        return Err(XlsxError::InvalidPath {
            path: temporary_path,
            message: "随机临时文件名发生冲突".to_owned(),
        });
    }
    Ok(temporary_path)
}

/// 非 Windows 平台依赖同目录 rename 的原子替换语义。
#[cfg(not(windows))]
fn replace_file(temporary_path: &Path, workbook_path: &Path) -> Result<(), XlsxError> {
    std::fs::rename(temporary_path, workbook_path).map_err(|source: std::io::Error| XlsxError::Io {
        stage: "原子替换工作簿",
        path: workbook_path.to_path_buf(),
        source,
    })
}

/// Windows 使用同卷替换和写穿透标记，锁冲突映射为明确业务错误。
#[cfg(windows)]
fn replace_file(temporary_path: &Path, workbook_path: &Path) -> Result<(), XlsxError> {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temporary_wide: Vec<u16> = null_terminated_wide_path(temporary_path)?;
    let workbook_wide: Vec<u16> = null_terminated_wide_path(workbook_path)?;
    let flags: u32 = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    // 两个 UTF-16 路径缓冲区均以 NUL 结尾，并在系统调用返回前保持有效。
    let result: i32 =
        unsafe { MoveFileExW(temporary_wide.as_ptr(), workbook_wide.as_ptr(), flags) };
    if result != 0 {
        return Ok(());
    }

    let source: std::io::Error = std::io::Error::last_os_error();
    let raw_error: Option<i32> = source.raw_os_error();
    let direct_lock: bool = raw_error == Some(ERROR_SHARING_VIOLATION as i32)
        || raw_error == Some(ERROR_LOCK_VIOLATION as i32);
    let delete_share_lock: bool = if raw_error == Some(ERROR_ACCESS_DENIED as i32) {
        target_denies_delete_sharing(&workbook_wide).map_err(
            |diagnostic_source: std::io::Error| XlsxError::Io {
                stage: "诊断工作簿删除共享权",
                path: workbook_path.to_path_buf(),
                source: diagnostic_source,
            },
        )?
    } else {
        false
    };
    if direct_lock || delete_share_lock {
        return Err(XlsxError::WorkbookLocked {
            path: workbook_path.to_path_buf(),
            source,
        });
    }
    Err(XlsxError::Io {
        stage: "原子替换工作簿",
        path: workbook_path.to_path_buf(),
        source,
    })
}

/// 请求 DELETE 访问权以区分共享锁冲突和 ACL 权限拒绝。
#[cfg(windows)]
fn target_denies_delete_sharing(workbook_wide: &[u16]) -> Result<bool, std::io::Error> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, HANDLE,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    // 路径缓冲区在调用期间有效，安全属性和模板句柄按只读诊断要求置空。
    let handle: HANDLE = unsafe {
        CreateFileW(
            workbook_wide.as_ptr(),
            DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        // 有效句柄只用于访问权诊断，调用后立即关闭。
        let close_result: i32 = unsafe { CloseHandle(handle) };
        if close_result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        return Ok(false);
    }

    let source: std::io::Error = std::io::Error::last_os_error();
    match source.raw_os_error() {
        Some(value)
            if value == ERROR_SHARING_VIOLATION as i32 || value == ERROR_LOCK_VIOLATION as i32 =>
        {
            Ok(true)
        }
        Some(value) if value == ERROR_ACCESS_DENIED as i32 => Ok(false),
        _ => Err(source),
    }
}

/// Windows 路径转换为无内嵌 NUL 的终止 UTF-16 缓冲区。
#[cfg(windows)]
fn null_terminated_wide_path(path: &Path) -> Result<Vec<u16>, XlsxError> {
    use std::os::windows::ffi::OsStrExt;

    let mut encoded: Vec<u16> = path.as_os_str().encode_wide().collect();
    if encoded.contains(&0) {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "Windows 路径包含 NUL".to_owned(),
        });
    }
    encoded.push(0);
    Ok(encoded)
}

/// 返回当前平台实际采用的替换机制名称。
#[cfg(windows)]
fn replacement_method() -> &'static str {
    "MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH)"
}

/// 返回当前平台实际采用的替换机制名称。
#[cfg(not(windows))]
fn replacement_method() -> &'static str {
    "rename(same-directory)"
}

//! 通过操作系统默认程序打开受控的 XLSX 工作簿，不参与工作簿内容写入。

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::adapters::tool_root::ToolRoot;
use crate::application::{
    AppError, AppErrorCode, WorkbookOpenPort, WorkbookOpenReport, WorkbookRef,
};

/// 使用受控工具根目录把工作簿交给系统默认程序。
pub(crate) struct XlsxWorkbookOpenPort {
    tool_root: ToolRoot,
}

impl XlsxWorkbookOpenPort {
    /// 固定工具根目录，打开时不接受任意宿主路径。
    pub(crate) fn new(tool_root: ToolRoot) -> Self {
        Self { tool_root }
    }
}

impl WorkbookOpenPort for XlsxWorkbookOpenPort {
    fn open_workbook(&self, workbook: &WorkbookRef) -> Result<WorkbookOpenReport, AppError> {
        let relative_path = workbook.relative_path();
        let path = self
            .tool_root
            .existing_file(relative_path)
            .map_err(|source| map_open_error(relative_path, source))?;
        let launcher =
            launch_workbook(&path).map_err(|source| map_open_error(relative_path, source))?;
        Ok(WorkbookOpenReport::new(
            relative_path.to_string_lossy().replace('\\', "/"),
            launcher,
        ))
    }
}

/// 默认程序启动失败或目标路径不满足工作簿边界。
#[derive(Debug, Error)]
pub enum WorkbookOpenError {
    /// 目标必须是既有的普通 XLSX 文件。
    #[error("工作簿路径 {path} 无效: {message}")]
    InvalidPath { path: PathBuf, message: String },
    /// 读取目标元数据失败。
    #[error("读取工作簿元数据失败: {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Windows ShellExecuteW 返回了失败状态。
    #[cfg(target_os = "windows")]
    #[error("Windows 默认程序未能打开工作簿: {message}")]
    Windows { message: String },
    /// Unix 默认打开器进程未能启动。
    #[cfg(unix)]
    #[error("启动系统工作簿打开器 {launcher} 失败: {source}")]
    UnixLauncher {
        launcher: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// 当前平台没有已实现的默认工作簿打开器。
    #[cfg(not(any(target_os = "windows", unix)))]
    #[error("当前平台没有可用的工作簿默认打开器")]
    UnsupportedPlatform,
}

/// 校验并调用系统默认程序打开一个工具目录内的工作簿。
///
/// 端口已经通过 `ToolRoot` 重新解析引用；这里再次检查普通文件和扩展名，
/// 避免路径在校验与启动之间被替换为链接或目录。
fn launch_workbook(path: &Path) -> Result<&'static str, WorkbookOpenError> {
    validate_open_target(path)?;
    launch_default_application(path)
}

fn map_open_error(
    relative_path: &Path,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    AppError::from_source(
        "workbook.open",
        AppErrorCode::WorkbookOpenFailed,
        "未能调用系统默认程序打开工作簿",
        source,
    )
    .with_context("path", relative_path.to_string_lossy().replace('\\', "/"))
}

fn validate_open_target(path: &Path) -> Result<(), WorkbookOpenError> {
    let metadata: fs::Metadata =
        fs::symlink_metadata(path).map_err(|source| WorkbookOpenError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkbookOpenError::InvalidPath {
            path: path.to_path_buf(),
            message: "目标必须是既有的普通文件，不能是符号链接或目录".to_owned(),
        });
    }
    let is_xlsx: bool = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx"));
    if !is_xlsx {
        return Err(WorkbookOpenError::InvalidPath {
            path: path.to_path_buf(),
            message: "目标必须使用 .xlsx 扩展名".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn launch_default_application(path: &Path) -> Result<&'static str, WorkbookOpenError> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb: Vec<u16> = "open".encode_utf16().chain(once(0)).collect();
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        return Err(WorkbookOpenError::Windows {
            message: format!("ShellExecuteW 返回错误码 {}", result as isize),
        });
    }
    Ok("windows-shell")
}

#[cfg(unix)]
pub(crate) fn launch_default_application(path: &Path) -> Result<&'static str, WorkbookOpenError> {
    #[cfg(target_os = "macos")]
    const LAUNCHER: &str = "open";
    #[cfg(not(target_os = "macos"))]
    const LAUNCHER: &str = "xdg-open";

    Command::new(LAUNCHER)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| LAUNCHER)
        .map_err(|source| WorkbookOpenError::UnixLauncher {
            launcher: LAUNCHER,
            source,
        })
}

#[cfg(not(any(target_os = "windows", unix)))]
pub(crate) fn launch_default_application(_path: &Path) -> Result<&'static str, WorkbookOpenError> {
    Err(WorkbookOpenError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{WorkbookOpenError, validate_open_target};

    #[test]
    fn rejects_missing_target_before_starting_a_launcher() {
        let error = validate_open_target(Path::new("missing.xlsx")).unwrap_err();
        assert!(matches!(error, WorkbookOpenError::Io { .. }));
    }

    #[test]
    fn rejects_non_xlsx_and_symlink_targets() {
        let directory = TempDirectory::new();
        let text_path = directory.path().join("plan.txt");
        fs::write(&text_path, b"fixture").unwrap();
        let error = validate_open_target(&text_path).unwrap_err();
        assert!(matches!(error, WorkbookOpenError::InvalidPath { .. }));

        #[cfg(unix)]
        {
            let link = directory.path().join("plan.xlsx");
            std::os::unix::fs::symlink(&text_path, &link).unwrap();
            let error = validate_open_target(&link).unwrap_err();
            assert!(matches!(error, WorkbookOpenError::InvalidPath { .. }));
        }
    }

    struct TempDirectory {
        path: PathBuf,
    }

    impl TempDirectory {
        fn new() -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .expect("测试环境必须提供 HOME 或 USERPROFILE");
            let path = PathBuf::from(home).join(format!(
                "suzushiro/scratch/azlw-open-unit-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

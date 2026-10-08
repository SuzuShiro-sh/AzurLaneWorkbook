//! 新工作簿目标路径校验。
use crate::XlsxError;
use std::path::Path;
/// 拒绝缺少父目录、目录目标或非 XLSX 扩展名。
pub fn validate_new_xlsx_destination(path: &Path) -> Result<(), XlsxError> {
    let extension: Option<&str> = path
        .extension()
        .and_then(|value: &std::ffi::OsStr| value.to_str());
    if !extension.is_some_and(|value: &str| value.eq_ignore_ascii_case("xlsx")) {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "扩展名必须是 .xlsx".to_owned(),
        });
    }
    let parent: &Path = path.parent().ok_or_else(|| XlsxError::InvalidPath {
        path: path.to_path_buf(),
        message: "缺少父目录".to_owned(),
    })?;
    if !parent.is_dir() {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "父目录必须已经存在".to_owned(),
        });
    }
    if path.is_dir() {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "目标不能是目录".to_owned(),
        });
    }
    if path.exists() {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "目标文件已经存在".to_owned(),
        });
    }
    Ok(())
}

//! 以只读权限查询进程映像，为安装发现与监听归属提供共同证据。
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
const MAX_PROCESS_IMAGE_PATH_UNITS: usize = 32_768;

pub fn query_process_image_path(process_id: u32) -> Result<PathBuf, String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err(unsafe { GetLastError() }.to_string());
    }
    let result = (|| {
        let mut buffer: Vec<u16> = vec![0; MAX_PROCESS_IMAGE_PATH_UNITS];
        let mut length = u32::try_from(buffer.len()).expect("process path buffer must fit u32");
        if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0
        {
            return Err(unsafe { GetLastError() }.to_string());
        }
        let length =
            usize::try_from(length).map_err(|_| "进程映像路径长度无法转换为 usize".to_owned())?;
        if length == 0 || length > buffer.len() {
            return Err(format!("进程映像路径长度无效: {length}"));
        }
        buffer.truncate(length);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    })();
    let close_error = if unsafe { CloseHandle(process) } == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    match (result, close_error) {
        (Ok(value), None) => Ok(value),
        (Ok(_), Some(error)) => Err(format!("进程句柄关闭失败: {error}")),
        (Err(message), None) => Err(message),
        (Err(message), Some(error)) => Err(format!("{message}；进程句柄关闭失败: {error}")),
    }
}

//! 提供 Windows 原生界面子模块共享的 Win32 文本与错误转换。

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

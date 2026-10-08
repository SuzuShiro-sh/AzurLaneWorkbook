//! 运行 AzurLaneWorkbook 的单窗口桌面界面。

use thiserror::Error;

use super::gui_controller::GuiTaskFactory;

/// 原生窗口标题。
pub const WINDOW_TITLE: &str = "铃白的碧蓝工具箱";

/// 在当前线程运行 egui 桌面窗口。
#[cfg(target_os = "windows")]
pub fn run_native_gui(task_factory: GuiTaskFactory) -> Result<(), NativeGuiError> {
    windows::run(task_factory)
}

/// 非 Windows 目标保留明确编译契约，但不伪装原生窗口能力。
#[cfg(not(target_os = "windows"))]
pub fn run_native_gui(_task_factory: GuiTaskFactory) -> Result<(), NativeGuiError> {
    Err(NativeGuiError::UnsupportedPlatform)
}

/// 在 GUI 入口建立失败时展示最后一条可读错误。
#[cfg(target_os = "windows")]
pub fn show_error_dialog(message: &str) {
    windows::show_error_dialog(message);
}

/// 非 Windows 开发目标把入口错误写入标准错误流。
#[cfg(not(target_os = "windows"))]
pub fn show_error_dialog(message: &str) {
    eprintln!("{message}");
}

/// 桌面界面运行失败。
#[derive(Debug, Error)]
pub enum NativeGuiError {
    #[cfg(not(target_os = "windows"))]
    #[error("原生窗口只支持 Windows")]
    UnsupportedPlatform,
    #[cfg(target_os = "windows")]
    #[error("原生窗口消息循环失败: {0}")]
    MessageLoop(String),
}

#[cfg(target_os = "windows")]
mod windows;

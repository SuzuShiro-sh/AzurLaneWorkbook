//! 将界面动作交给控制器，并通过 egui 通知重绘。
use super::super::WINDOW_TITLE;
use super::state::WindowState;
use super::win32::wide;
use crate::interfaces::gui_controller::GuiAction;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONWARNING, MB_OK, MB_YESNO, MessageBoxW,
};

pub(super) fn start_initialization(state: &mut WindowState) -> Result<(), String> {
    let context = state.context.clone();
    state
        .controller
        .start_initialization(move || context.request_repaint())
        .map_err(|error| error.to_string())
}

pub(super) fn start_action(state: &mut WindowState, action: GuiAction) -> Result<(), String> {
    if action == GuiAction::ExecutePlan && !confirm_execution(state)? {
        return Ok(());
    }
    let context = state.context.clone();
    state
        .controller
        .start(action, move || context.request_repaint())
        .map_err(|error| error.to_string())
}

pub(super) fn open_selected_diagnostic(state: &mut WindowState) -> Result<(), String> {
    let context = state.context.clone();
    state
        .controller
        .start_diagnostic_open(move || context.request_repaint())
        .map_err(|error| error.to_string())
}

pub(super) fn show_failure_detail(state: &WindowState) {
    let view = state.controller.view();
    let Some(detail) = view.last_failure_detail() else {
        return;
    };
    show_warning_dialog(state.window, detail);
}

pub(super) fn confirm_execution(state: &WindowState) -> Result<bool, String> {
    let view = state.controller.view();
    let workbook = view
        .selected_workbook()
        .ok_or_else(|| "尚未选择工作簿".to_owned())?;
    let message = wide(&format!(
        "将按工作簿 {workbook} 执行计划。\n\n程序会先复核当前游戏状态并备份工作簿；执行后会保存历史并写回结果。确认继续吗？"
    ));
    let title = wide("确认执行计划");
    let result = unsafe {
        MessageBoxW(
            state.window,
            message.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONWARNING,
        )
    };
    Ok(result == IDYES)
}

pub(super) fn show_warning_dialog(parent: HWND, message: &str) {
    let title = wide(WINDOW_TITLE);
    let message = wide(message);
    unsafe {
        MessageBoxW(
            parent,
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

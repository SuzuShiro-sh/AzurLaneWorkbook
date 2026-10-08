//! 保存界面控制器、对话框所属窗口和关闭状态。

use crate::interfaces::gui_controller::{GuiController, GuiTaskFactory};
use eframe::egui;
use windows_sys::Win32::Foundation::HWND;

pub(super) struct WindowState {
    pub(super) window: HWND,
    pub(super) controller: GuiController,
    pub(super) context: egui::Context,
    pub(super) closing: bool,
    pub(super) settings_open: bool,
    pub(super) settings_draft: Option<crate::application::UserPreferences>,
    pub(super) settings_original: Option<crate::application::UserPreferences>,
}
impl WindowState {
    pub(super) fn new(factory: GuiTaskFactory, context: egui::Context) -> Self {
        Self {
            window: std::ptr::null_mut(),
            controller: GuiController::new(factory),
            context,
            closing: false,
            settings_open: false,
            settings_draft: None,
            settings_original: None,
        }
    }
    pub(super) fn request_close(&mut self) {
        self.closing = true;
        self.controller.request_cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::gui_controller::{GuiOperationOutput, GuiTaskOutput};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    #[test]
    fn close_cancels_and_reaps_active_task() {
        let completed = Arc::new(AtomicBool::new(false));
        let worker_completed = completed.clone();
        let factory = GuiTaskFactory::from_handler(move |_, _, context| {
            while !context.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            worker_completed.store(true, Ordering::Release);
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("已清理")),
                None,
            ))
        });
        let mut state = WindowState::new(factory, egui::Context::default());
        super::super::actions::start_initialization(&mut state).unwrap();
        state.request_close();
        assert!(state.closing);
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.controller.view().is_running() {
            assert!(Instant::now() < deadline, "后台任务未回收");
            state.controller.drain_events().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(completed.load(Ordering::Acquire));
    }
}

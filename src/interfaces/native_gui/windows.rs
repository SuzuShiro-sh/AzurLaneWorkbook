//! 用 egui 绘制界面，直接接收后台任务通知并在关闭前完成回收。

use super::{NativeGuiError, WINDOW_TITLE};
use crate::interfaces::gui_controller::GuiTaskFactory;
use eframe::egui;
use std::ptr::null_mut;
use std::time::Duration;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, MB_ICONERROR, MB_OK, MessageBoxW,
};
mod actions;
mod loading_bar;
mod state;
mod theme;
mod view;
mod win32;
use state::WindowState;
use win32::wide;
const CLIENT_WIDTH: f32 = 980.0;
const CLIENT_HEIGHT: f32 = 640.0;

pub(super) fn run(task_factory: GuiTaskFactory) -> Result<(), NativeGuiError> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([CLIENT_WIDTH, CLIENT_HEIGHT])
            .with_min_inner_size([CLIENT_WIDTH, CLIENT_HEIGHT])
            .with_max_inner_size([CLIENT_WIDTH, CLIENT_HEIGHT])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_title(WINDOW_TITLE)
            .with_icon(view::window_icon()),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(GuiApp {
                state: WindowState::new(task_factory, cc.egui_ctx.clone()),
                initialized: false,
                background: view::load_background(&cc.egui_ctx),
                loading_bar: loading_bar::load(&cc.egui_ctx),
            }))
        }),
    )
    .map_err(|error| NativeGuiError::MessageLoop(error.to_string()))
}

pub(super) fn show_error_dialog(message: &str) {
    let title = wide(WINDOW_TITLE);
    let message = wide(message);
    unsafe {
        MessageBoxW(
            null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

struct GuiApp {
    state: WindowState,
    initialized: bool,
    background: Option<view::GuiBackground>,
    loading_bar: loading_bar::LoadingBarAssets,
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::apply_visuals(ctx);
        if !self.initialized {
            self.state.window = current_process_window().unwrap_or(null_mut());
            if let Err(error) = actions::start_initialization(&mut self.state) {
                show_error_dialog(&error);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.initialized = true;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            self.state.request_close();
        }
        if let Err(error) = self.state.controller.drain_events() {
            show_error_dialog(&error.to_string());
            self.state.request_close();
        }
        if self.state.closing {
            if self.state.controller.view().is_running() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        } else {
            view::draw(
                ctx,
                &mut self.state,
                self.background.as_ref(),
                Some(&self.loading_bar),
            );
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.state.request_close();
        while self.state.controller.view().is_running() {
            if let Err(error) = self.state.controller.drain_events() {
                show_error_dialog(&error.to_string());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct WindowQuery {
    process_id: u32,
    window: HWND,
}

fn current_process_window() -> Option<HWND> {
    let mut query = WindowQuery {
        process_id: unsafe { GetCurrentProcessId() },
        window: null_mut(),
    };
    unsafe {
        EnumWindows(
            Some(collect_title_window),
            (&mut query as *mut WindowQuery) as isize,
        );
    }
    (!query.window.is_null()).then_some(query.window)
}

unsafe extern "system" fn collect_title_window(window: HWND, lparam: isize) -> i32 {
    let query = unsafe { &mut *(lparam as *mut WindowQuery) };
    let mut owner = 0_u32;
    unsafe { GetWindowThreadProcessId(window, &mut owner) };
    if owner != query.process_id {
        return 1;
    }
    let mut buffer = [0_u16; 512];
    let length = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length > 0 {
        let title = String::from_utf16_lossy(&buffer[..length as usize]);
        if title == WINDOW_TITLE {
            query.window = window;
            return 0;
        }
    }
    1
}

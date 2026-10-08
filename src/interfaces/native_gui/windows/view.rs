//! 用整窗背景图和半透明卡片绘制业务控件。

use eframe::egui::{
    self, AboveOrBelow, Align, Align2, Color32, ColorImage, ComboBox, CornerRadius, FontId, Frame,
    Grid, Id, Layout, Margin, PopupCloseBehavior, Rect, RichText, Sense, Stroke, TextWrapMode,
    TextureHandle, TextureOptions, Ui, UiBuilder, Vec2, WidgetText,
    epaint::{RectShape, StrokeKind},
    pos2, vec2,
};

use crate::interfaces::gui_controller::GuiAction;

use super::actions::{
    open_selected_diagnostic, show_failure_detail, show_warning_dialog, start_action,
};
use super::loading_bar::LoadingBarAssets;
use super::state::WindowState;
use super::theme::{
    ACCENT, CLUSTER_FILL, CLUSTER_STROKE, PANEL_FILL, PANEL_SHADOW, PANEL_STROKE, PRIMARY_BUTTON,
    TEXT, TEXT_HINT, TEXT_MUTED, UNAVAILABLE_FILL, UNAVAILABLE_STROKE, UNAVAILABLE_TEXT,
    WINDOW_SCRIM,
};

const BACKGROUND_PNG: &[u8] = include_bytes!("../../../../assets/gui/background.png");
const ICON_PNG: &[u8] = include_bytes!("../../../../assets/gui/icon.png");
const WINDOW_ICON_SIZE: u32 = 256;
const ACTION_BUTTON_MIN: Vec2 = Vec2::new(96.0, 36.0);
const ACTION_CLUSTER_PAD: f32 = 4.0;
const ACTION_ROW_HEIGHT: f32 = ACTION_BUTTON_MIN.y + ACTION_CLUSTER_PAD * 2.0;
const OPEN_LOG_BUTTON_WIDTH: f32 = 112.0;
const PANEL_CORNER: u8 = 14;
const COMBO_POPUP_HEIGHT: f32 = 200.0;
const DIAGNOSTIC_POPUP_ROWS: f32 = 6.0;
const COPY_FEEDBACK_SECS: f64 = 1.6;

enum UiCommand {
    OpenSettings,
    Action(GuiAction),
    OpenDiagnostic,
    FailureDetail,
    SelectInstance(usize),
    SelectWorkbook(usize),
    SelectDiagnostic(usize),
}

pub(super) struct GuiBackground {
    image: TextureHandle,
    blur: TextureHandle,
}

#[derive(Clone, Copy)]
struct GlassPaint<'a> {
    blur: &'a TextureHandle,
    image_rect: Rect,
}

pub(super) fn window_icon() -> egui::IconData {
    let image = image::load_from_memory(ICON_PNG)
        .expect("窗口图标应能解码")
        .into_rgba8();
    let resized = image::imageops::resize(
        &image,
        WINDOW_ICON_SIZE,
        WINDOW_ICON_SIZE,
        image::imageops::FilterType::Triangle,
    );
    egui::IconData {
        rgba: resized.into_raw(),
        width: WINDOW_ICON_SIZE,
        height: WINDOW_ICON_SIZE,
    }
}

pub(super) fn load_background(ctx: &eframe::egui::Context) -> Option<GuiBackground> {
    let image = image::load_from_memory(BACKGROUND_PNG).ok()?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    let sharp = ctx.load_texture("gui-background", color_image, TextureOptions::LINEAR);
    let blur = load_blur_texture(ctx, &image);
    Some(GuiBackground { image: sharp, blur })
}

fn load_blur_texture(ctx: &eframe::egui::Context, image: &image::RgbaImage) -> TextureHandle {
    let (width, height) = image.dimensions();
    let small_w = (width / 4).max(1);
    let small_h = (height / 4).max(1);
    let small = image::imageops::resize(
        image,
        small_w,
        small_h,
        image::imageops::FilterType::Triangle,
    );
    let blurred = image::imageops::blur(&small, 5.0);
    let size = [blurred.width() as usize, blurred.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, blurred.as_raw());
    ctx.load_texture("gui-background-blur", color_image, TextureOptions::LINEAR)
}

pub(super) fn draw(
    ctx: &eframe::egui::Context,
    state: &mut WindowState,
    background: Option<&GuiBackground>,
    loading_bar: Option<&LoadingBarAssets>,
) {
    let mut commands = Vec::new();
    egui::CentralPanel::default()
        .frame(Frame::NONE)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            paint_background(ui, rect, background);
            let image_rect = background.map(|item| cover_rect(rect, item.image.size_vec2()));
            let glass = match (background, image_rect) {
                (Some(background), Some(image_rect)) => Some(GlassPaint {
                    blur: &background.blur,
                    image_rect,
                }),
                _ => None,
            };
            let content = rect.shrink(20.0);
            ui.allocate_new_ui(UiBuilder::new().max_rect(content), |ui| {
                ui.set_max_size(content.size());
                ui.set_width(content.width());
                draw_shell(ui, state, &mut commands, glass, loading_bar);
            });
        });
    apply_commands(state, commands);
    draw_settings(ctx, state);
}

fn setting_help(response: egui::Response, text: &str) {
    if response.hovered() {
        egui::show_tooltip_at_pointer(&response.ctx, response.layer_id, response.id, |ui| {
            ui.label(text);
        });
    }
}

fn draw_settings(ctx: &egui::Context, state: &mut WindowState) {
    let mut open = state.settings_open;
    let mut save = None;
    if open {
        egui::Window::new("设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .default_size(vec2(640.0, 420.0))
            .min_size(vec2(560.0, 320.0))
            .show(ctx, |ui| {
                let view = state.controller.view();
                if state.settings_draft.is_none() {
                    state.settings_draft = view.preferences();
                    state.settings_original = view.preferences();
                } else if !view.is_running() && state.settings_draft == view.preferences() {
                    // 保存成功后，以已保存的草稿作为下一次编辑的基线。
                    state.settings_original = view.preferences();
                }
                match state.settings_draft.as_mut() {
                    Some(preferences) => {
                        let response = ui.add_enabled(
                            !view.is_running(),
                            egui::Checkbox::new(&mut preferences.ship_acquisition_enabled, "启用获取方式列"),
                        );
                        setting_help(response, "开启后，同步时按更新策略读取获取方式并生成该列。\n点击保存后，下次同步生成生效。\n关闭时不生成该列，也不查询 BWiki。");
                        ui.add_enabled_ui(!view.is_running(), |ui| {
                            use crate::application::AcquisitionUpdatePolicy;
                            let policy_response = egui::ComboBox::from_label("获取方式更新策略")
                                .selected_text(match preferences.acquisition_update_policy {
                                    AcquisitionUpdatePolicy::UseCache => "使用缓存",
                                    AcquisitionUpdatePolicy::Refresh => "每次同步更新",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut preferences.acquisition_update_policy, AcquisitionUpdatePolicy::UseCache, "使用缓存");
                                    ui.selectable_value(&mut preferences.acquisition_update_policy, AcquisitionUpdatePolicy::Refresh, "每次同步更新");
                                }).response;
                            setting_help(policy_response, "使用缓存：只读取已有缓存。没有缓存时单元格写“资料未缓存，待更新”，本次同步不联网。\n每次同步更新：本次同步等待在线更新，失败时沿用已有缓存。\n仅在启用获取方式列时生效。");
                            setting_help(ui.checkbox(&mut preferences.detailed_diagnostics, "详细诊断日志"), "记录逐项进度等诊断明细。关闭后仍保留关键阶段、失败与清理证据。");
                            setting_help(ui.checkbox(&mut preferences.unload_after_sync, "同步完成后卸载代理"), "同步结束后卸载本工具代理；关闭时保留代理以便后续 GUI 或 CLI 复用。\n点击保存后，下次同步生效。");
                        });
                    }
                    None => {
                        ui.label("设置尚未读取");
                    }
                }
                let dirty = state.settings_draft.is_some()
                    && state.settings_draft != state.settings_original;
                ui.add_space((ui.available_height() - 60.0).max(0.0));
                ui.separator();
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(dirty && !view.is_running(), egui::Button::new("保存").min_size(ACTION_BUTTON_MIN)).clicked() {
                            save = state.settings_original.zip(state.settings_draft);
                        }
                        ui.label(if dirty && !view.is_running() && view.last_failure_detail().is_none() { "有未保存的修改" } else { view.message() });
                    });
                    if let Some(detail) = view.last_failure_detail() {
                        ui.collapsing("错误详情", |ui| { ui.label(detail); });
                    }
                });
            });
    }
    state.settings_open = open;
    if let Some((original, preferences)) = save
        && let Err(error) = start_action(
            state,
            GuiAction::SaveSettings {
                original,
                preferences,
            },
        )
    {
        show_warning_dialog(state.window, &error);
    }
}

fn draw_shell(
    ui: &mut Ui,
    state: &WindowState,
    commands: &mut Vec<UiCommand>,
    glass: Option<GlassPaint<'_>>,
    loading_bar: Option<&LoadingBarAssets>,
) {
    let view = state.controller.view();
    let controls = view.controls();
    ui.set_width(ui.available_width());
    card(ui, glass, |ui| {
        ui.horizontal(|ui| {
            let ready = view.shows_ready_indicator();
            let dot = if ready {
                Color32::from_rgb(110, 220, 140)
            } else if view.is_running() {
                ACCENT
            } else {
                Color32::from_rgb(255, 186, 90)
            };
            let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 5.0, dot);
            ui.label(RichText::new(view.status()).size(18.0).color(TEXT).strong());
        });
        ui.add(
            egui::Label::new(
                RichText::new(wrap_anywhere(view.message()))
                    .size(13.0)
                    .color(TEXT_MUTED),
            )
            .wrap(),
        );
        if view.is_running()
            && let Some(loading_bar) = loading_bar
        {
            ui.spacing_mut().item_spacing.y = 2.0;
            super::loading_bar::show(
                ui,
                loading_bar,
                view.progress_percent()
                    .map(|value| f32::from(value) / 100.0),
            );
        }
    });
    ui.add_space(12.0);
    ui.columns(2, |columns| {
        card(&mut columns[0], glass, |ui| {
            ui.label(RichText::new("模拟器 实例").size(13.0).color(TEXT_MUTED));
            let selected = view.selected_instance_index().is_some();
            let instance_text = view
                .selected_instance_index()
                .and_then(|index| {
                    view.instances()
                        .get(index)
                        .map(|item| item.label().to_owned())
                })
                .unwrap_or_else(|| "尚未选择实例".to_owned());
            ui.add_enabled_ui(controls.instance_selection_enabled, |ui| {
                let current = view.selected_instance_index();
                constrained_combo(
                    ui,
                    "gui-instances",
                    combo_selected_text(&instance_text, !selected),
                    COMBO_POPUP_HEIGHT,
                    |ui| {
                        for (index, item) in view.instances().iter().enumerate() {
                            if ui
                                .selectable_label(current == Some(index), item.label())
                                .clicked()
                            {
                                commands.push(UiCommand::SelectInstance(index));
                            }
                        }
                    },
                );
            });
            if ui
                .add_enabled(
                    !view.is_running() && view.selected_instance_available(),
                    egui::Button::new(format!("代理：{}", view.agent_status().unwrap_or("未查询")))
                        .truncate(),
                )
                .on_hover_text("查询代理状态")
                .clicked()
            {
                commands.push(UiCommand::Action(GuiAction::AgentStatus));
            }
        });
        card(&mut columns[1], glass, |ui| {
            ui.label(RichText::new("工作簿").size(13.0).color(TEXT_MUTED));
            let selected = view.selected_workbook().is_some();
            let workbook_text = view
                .selected_workbook()
                .unwrap_or("尚未选择工作簿")
                .to_owned();
            ui.add_enabled_ui(controls.workbook_selection_enabled, |ui| {
                let current = view.selected_workbook_index();
                constrained_combo(
                    ui,
                    "gui-workbooks",
                    combo_selected_text(&workbook_text, !selected),
                    COMBO_POPUP_HEIGHT,
                    |ui| {
                        for (index, name) in view.workbooks().iter().enumerate() {
                            if ui.selectable_label(current == Some(index), name).clicked() {
                                commands.push(UiCommand::SelectWorkbook(index));
                            }
                        }
                    },
                );
            });
        });
    });
    ui.add_space(12.0);
    card(ui, glass, |ui| {
        ui.horizontal(|ui| {
            ui.set_height(ACTION_ROW_HEIGHT);
            button_cluster(ui, |ui| {
                if action_button(
                    ui,
                    "同步并生成",
                    controls.synchronize_enabled,
                    Some(PRIMARY_BUTTON),
                )
                .clicked()
                {
                    commands.push(UiCommand::Action(GuiAction::SynchronizeAndGenerate));
                }
                unavailable_action_button(ui, "检查计划");
                unavailable_action_button(ui, "执行计划");
                push_action_button(
                    ui,
                    "打开工作簿",
                    controls.open_enabled,
                    UiCommand::Action(GuiAction::OpenWorkbook),
                    commands,
                );
            });
            ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), ACTION_ROW_HEIGHT),
                Layout::right_to_left(Align::Center),
                |ui| {
                    button_cluster(ui, |ui| {
                        push_action_button(
                            ui,
                            "刷新信息",
                            controls.refresh_enabled,
                            UiCommand::Action(GuiAction::Refresh),
                            commands,
                        );
                        push_action_button(
                            ui,
                            "查看错误详情",
                            controls.failure_detail_enabled,
                            UiCommand::FailureDetail,
                            commands,
                        );
                        push_action_button(
                            ui,
                            "设置",
                            !view.is_running(),
                            UiCommand::OpenSettings,
                            commands,
                        );
                        push_action_button(
                            ui,
                            "卸载",
                            !view.is_running() && view.selected_instance_available(),
                            UiCommand::Action(GuiAction::UnloadAgent),
                            commands,
                        );
                    });
                },
            );
        });
    });
    ui.add_space(12.0);
    let log_height = ui.available_height().max(0.0);
    let (log_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), log_height), Sense::hover());
    ui.allocate_new_ui(UiBuilder::new().max_rect(log_rect), |ui| {
        ui.set_width(log_rect.width());
        card(ui, glass, |ui| {
            ui.label(RichText::new("历史与日志").size(13.0).color(TEXT_MUTED));
            ui.horizontal(|ui| {
                ui.set_height(combo_closed_height());
                ui.spacing_mut().item_spacing.y = 0.0;
                let spacing = ui.spacing().item_spacing.x;
                let combo_width =
                    (ui.available_width() - OPEN_LOG_BUTTON_WIDTH - spacing).max(160.0);
                let diagnostic_selected = view.selected_diagnostic().is_some();
                let diagnostic_text = view
                    .selected_diagnostic()
                    .map(|item| item.label().to_owned())
                    .unwrap_or_else(|| "刷新信息后可查看历史与日志".to_owned());
                ui.scope(|ui| {
                    ui.set_max_width(combo_width);
                    ui.set_width(combo_width);
                    ui.add_enabled_ui(controls.diagnostic_selection_enabled, |ui| {
                        diagnostic_combo(
                            ui,
                            combo_selected_text(&diagnostic_text, !diagnostic_selected),
                            combo_width,
                            view.diagnostics(),
                            view.selected_diagnostic_index(),
                            commands,
                        );
                    });
                });
                push_action_button(
                    ui,
                    "打开日志",
                    controls.diagnostic_open_enabled,
                    UiCommand::OpenDiagnostic,
                    commands,
                );
            });
            ui.add_space(8.0);
            let detail_height = ui.available_height();
            egui::ScrollArea::vertical()
                .max_height(detail_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width());
                    if let Some(item) = view.selected_diagnostic() {
                        draw_diagnostic_detail(ui, item);
                    } else {
                        ui.add(
                            egui::Label::new(
                                RichText::new("刷新信息后可查看历史与日志元数据")
                                    .size(12.0)
                                    .color(TEXT_MUTED),
                            )
                            .wrap(),
                        );
                    }
                });
        });
    });
}

fn push_action_button(
    ui: &mut Ui,
    label: &str,
    enabled: bool,
    command: UiCommand,
    commands: &mut Vec<UiCommand>,
) {
    if action_button(ui, label, enabled, None).clicked() {
        commands.push(command);
    }
}

fn unavailable_action_button(ui: &mut Ui, label: &str) {
    let button = egui::Button::new(RichText::new(label).color(UNAVAILABLE_TEXT))
        .fill(UNAVAILABLE_FILL)
        .stroke(Stroke::new(1.0, UNAVAILABLE_STROKE))
        .corner_radius(10)
        .truncate()
        .min_size(ACTION_BUTTON_MIN)
        .sense(Sense::hover());
    ui.add(button).on_hover_text("功能尚未完善，暂不可用");
}

fn action_button(ui: &mut Ui, label: &str, enabled: bool, fill: Option<Color32>) -> egui::Response {
    let text_color = if fill.is_some() { Color32::WHITE } else { TEXT };
    let mut button = egui::Button::new(RichText::new(label).color(text_color))
        .corner_radius(10)
        .truncate()
        .min_size(ACTION_BUTTON_MIN);
    if let Some(fill) = fill {
        button = button.fill(fill);
    }
    ui.add_enabled(enabled, button)
}

fn button_cluster(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(CLUSTER_FILL)
        .stroke(Stroke::new(1.0, CLUSTER_STROKE))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(8, ACTION_CLUSTER_PAD as i8))
        .show(ui, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.set_height(ACTION_BUTTON_MIN.y);
                ui.set_max_height(ACTION_BUTTON_MIN.y);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                add(ui);
            });
        });
}

fn apply_commands(state: &mut WindowState, commands: Vec<UiCommand>) {
    for command in commands {
        let result = match command {
            UiCommand::OpenSettings => {
                state.settings_open = true;
                state.settings_draft = None;
                state.settings_original = None;
                start_action(state, GuiAction::LoadSettings)
            }
            UiCommand::Action(action) => start_action(state, action),
            UiCommand::OpenDiagnostic => open_selected_diagnostic(state),
            UiCommand::FailureDetail => {
                show_failure_detail(state);
                Ok(())
            }
            UiCommand::SelectInstance(index) => state
                .controller
                .select_instance(index)
                .map_err(|error| error.to_string())
                .and_then(|()| {
                    if state.controller.view().selected_instance_available() {
                        start_action(state, GuiAction::AgentStatus)
                    } else {
                        Ok(())
                    }
                }),
            UiCommand::SelectWorkbook(index) => state
                .controller
                .select_workbook(index)
                .map_err(|error| error.to_string()),
            UiCommand::SelectDiagnostic(index) => state
                .controller
                .select_diagnostic(index)
                .map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            show_warning_dialog(state.window, &error);
        }
    }
}

fn diagnostic_combo(
    ui: &mut Ui,
    selected_text: impl Into<WidgetText>,
    width: f32,
    items: &[crate::interfaces::gui_controller::GuiDiagnosticItem],
    current: Option<usize>,
    commands: &mut Vec<UiCommand>,
) {
    let button_id = Id::new("gui-diagnostics");
    let popup_id = button_id.with("popup");
    let button = combo_select_button(ui, button_id, selected_text, width);
    if button.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    if !ui.memory(|memory| memory.is_popup_open(popup_id)) {
        return;
    }
    let inner_width = diagnostic_popup_inner_width(
        button.rect.width(),
        Frame::popup(ui.style()).total_margin().sum().x,
    );
    let spacing = ui.spacing();
    let row_height = spacing.interact_size.y.max(28.0);
    let row_stride = row_height + spacing.item_spacing.y;
    let height = diagnostic_popup_max_height(
        row_stride,
        ui.ctx().screen_rect().bottom() - button.rect.bottom() - 8.0,
    );
    if height <= 0.0 {
        return;
    }
    egui::popup::popup_above_or_below_widget(
        ui,
        popup_id,
        &button,
        AboveOrBelow::Below,
        PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(inner_width);
            ui.set_max_width(inner_width);
            ui.set_min_height(height);
            ui.set_max_height(height);
            ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
            egui::ScrollArea::vertical()
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .show_rows(ui, row_height, items.len(), |ui, rows| {
                    ui.set_max_width(inner_width);
                    ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                    for index in rows {
                        let item = &items[index];
                        if ui
                            .selectable_label(current == Some(index), item.label())
                            .clicked()
                        {
                            commands.push(UiCommand::SelectDiagnostic(index));
                        }
                    }
                });
        },
    );
}

fn diagnostic_popup_max_height(row_height: f32, space_below: f32) -> f32 {
    (row_height * DIAGNOSTIC_POPUP_ROWS).min(space_below.max(0.0))
}

fn diagnostic_popup_inner_width(button_width: f32, frame_margin_x: f32) -> f32 {
    (button_width - frame_margin_x).max(0.0)
}

fn combo_closed_height() -> f32 {
    ACTION_BUTTON_MIN.y
}

fn combo_select_button(
    ui: &mut Ui,
    id: Id,
    selected_text: impl Into<WidgetText>,
    width: f32,
) -> egui::Response {
    let height = combo_closed_height();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    let visuals = if ui.memory(|memory| memory.is_popup_open(id.with("popup"))) {
        ui.visuals().widgets.open
    } else {
        *ui.style().interact(&response)
    };
    ui.painter().rect(
        rect,
        visuals.corner_radius,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    let icon_size = Vec2::splat(ui.spacing().icon_width);
    let inner = rect.shrink2(ui.spacing().button_padding);
    let icon_rect = Align2::RIGHT_CENTER.align_size_within_rect(icon_size, inner);
    let tri = Rect::from_center_size(
        icon_rect.center(),
        vec2(icon_rect.width() * 0.7, icon_rect.height() * 0.45),
    );
    ui.painter().add(egui::Shape::convex_polygon(
        vec![tri.left_top(), tri.right_top(), tri.center_bottom()],
        visuals.fg_stroke.color,
        Stroke::NONE,
    ));
    let wrap_width = (inner.width() - icon_size.x - ui.spacing().icon_spacing).max(24.0);
    let galley = selected_text.into().into_galley(
        ui,
        Some(TextWrapMode::Truncate),
        wrap_width,
        egui::TextStyle::Button,
    );
    let text_rect = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), inner);
    ui.painter()
        .galley(text_rect.min, galley, visuals.text_color());
    response
}

fn constrained_combo(
    ui: &mut Ui,
    id: &'static str,
    selected_text: impl Into<WidgetText>,
    popup_height: f32,
    add_contents: impl FnOnce(&mut Ui),
) {
    let width = ui.available_width();
    ui.scope(|ui| {
        ui.set_max_width(width);
        ui.set_height(combo_closed_height());
        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
        ui.spacing_mut().combo_height = popup_height;
        ui.spacing_mut().interact_size.y = combo_closed_height();
        ComboBox::from_id_salt(id)
            .selected_text(selected_text)
            .width(width)
            .truncate()
            .show_ui(ui, add_contents);
    });
}

fn combo_selected_text(text: &str, placeholder: bool) -> RichText {
    if placeholder {
        RichText::new(format!("▸  {text}")).color(TEXT_HINT)
    } else {
        RichText::new(text).color(TEXT)
    }
}

/// 诊断详情行。哈希行单独绘制，不靠标签文本判断。
enum DiagnosticDetailRow {
    Text { label: &'static str, value: String },
    Hash { label: &'static str, value: String },
}

fn draw_diagnostic_detail(
    ui: &mut Ui,
    item: &crate::interfaces::gui_controller::GuiDiagnosticItem,
) {
    let source = item.source();
    let mut rows = vec![
        DiagnosticDetailRow::Text {
            label: "类型",
            value: item.kind_label().to_owned(),
        },
        DiagnosticDetailRow::Text {
            label: "相对路径",
            value: source.relative_path().to_owned(),
        },
        DiagnosticDetailRow::Text {
            label: "大小",
            value: format!("{} 字节", source.size_bytes()),
        },
    ];
    if let Some(status) = item.status_label() {
        rows.insert(
            1,
            DiagnosticDetailRow::Text {
                label: "状态",
                value: status.to_owned(),
            },
        );
    }
    if let Some(workbook) = item.workbook_name() {
        let index = if item.status_label().is_some() { 2 } else { 1 };
        rows.insert(
            index,
            DiagnosticDetailRow::Text {
                label: "工作簿",
                value: workbook.to_owned(),
            },
        );
    }
    rows.push(DiagnosticDetailRow::Hash {
        label: "SHA-256",
        value: source.file_sha256().to_owned(),
    });
    Grid::new("diagnostic-detail")
        .num_columns(2)
        .min_col_width(72.0)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            for row in rows {
                match row {
                    DiagnosticDetailRow::Text { label, value } => {
                        ui.label(RichText::new(label).size(12.0).color(TEXT_MUTED));
                        ui.add(
                            egui::Label::new(
                                RichText::new(wrap_anywhere(&value)).size(12.0).color(TEXT),
                            )
                            .wrap(),
                        );
                    }
                    DiagnosticDetailRow::Hash { label, value } => {
                        ui.label(RichText::new(label).size(12.0).color(TEXT_MUTED));
                        draw_hash_value(ui, &value);
                    }
                }
                ui.end_row();
            }
        });
}

fn draw_hash_value(ui: &mut Ui, hash: &str) {
    let feedback_id = Id::new("diagnostic-copy-until");
    let now = ui.input(|input| input.time);
    let copied = ui
        .ctx()
        .data(|data| data.get_temp::<f64>(feedback_id))
        .is_some_and(|until| now < until);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add(
            egui::Label::new(
                RichText::new(truncate_sha256(hash))
                    .font(FontId::monospace(12.0))
                    .color(TEXT),
            )
            .sense(Sense::hover()),
        )
        .on_hover_text(hash);
        let caption = copy_button_caption(copied);
        let fill = if copied {
            Color32::from_rgba_unmultiplied(120, 176, 255, 70)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 12)
        };
        let color = if copied { TEXT } else { TEXT_HINT };
        let copy = ui
            .add(
                egui::Button::new(RichText::new(caption).size(11.0).color(color))
                    .small()
                    .fill(fill)
                    .stroke(Stroke::new(1.0, PANEL_STROKE))
                    .corner_radius(6)
                    .min_size(Vec2::new(52.0, 22.0)),
            )
            .on_hover_text(if copied {
                "已复制到剪贴板"
            } else {
                "复制完整 SHA-256"
            });
        if copy.clicked() {
            ui.ctx().copy_text(hash.to_owned());
            ui.ctx()
                .data_mut(|data| data.insert_temp(feedback_id, now + COPY_FEEDBACK_SECS));
        }
    });
}

fn copy_button_caption(copied: bool) -> &'static str {
    if copied { "已复制" } else { "复制" }
}

fn truncate_sha256(value: &str) -> String {
    if value.chars().count() <= 12 {
        return value.to_owned();
    }
    let prefix: String = value.chars().take(4).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(6)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}...{suffix}")
}

fn wrap_anywhere(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_mul(3));
    for character in text.chars() {
        out.push(character);
        if character != '\n' {
            out.push('\u{200B}');
        }
    }
    out
}

fn card(ui: &mut Ui, glass: Option<GlassPaint<'_>>, add: impl FnOnce(&mut Ui)) {
    let glass_idx = ui.painter().add(egui::Shape::Noop);
    let response = Frame::new()
        .fill(Color32::TRANSPARENT)
        .inner_margin(Margin::same(12))
        .corner_radius(PANEL_CORNER)
        .show(ui, |ui| {
            let inner = ui.available_width();
            ui.set_min_width(inner);
            ui.set_max_width(inner);
            add(ui);
        })
        .response;
    paint_glass(ui, glass_idx, response.rect, glass);
}

fn paint_glass(
    ui: &Ui,
    shape_idx: egui::layers::ShapeIdx,
    rect: Rect,
    glass: Option<GlassPaint<'_>>,
) {
    let corner = CornerRadius::same(PANEL_CORNER);
    let mut shapes = Vec::with_capacity(3);
    shapes.push(egui::Shape::from(PANEL_SHADOW.as_shape(rect, corner)));
    if let Some(glass) = glass {
        shapes.push(egui::Shape::Rect(
            RectShape::filled(rect, corner, Color32::WHITE)
                .with_texture(glass.blur.id(), texture_uv(rect, glass.image_rect)),
        ));
    }
    shapes.push(egui::Shape::Rect(RectShape::new(
        rect,
        corner,
        PANEL_FILL,
        Stroke::new(1.0, PANEL_STROKE),
        StrokeKind::Inside,
    )));
    ui.painter().set(shape_idx, egui::Shape::Vec(shapes));
}

fn paint_background(ui: &Ui, rect: Rect, background: Option<&GuiBackground>) {
    let painter = ui.painter_at(rect);
    let Some(background) = background else {
        painter.rect_filled(rect, 0.0, Color32::from_rgb(12, 10, 28));
        return;
    };
    let image_rect = cover_rect(rect, background.image.size_vec2());
    if image_rect.width() <= 0.0 || image_rect.height() <= 0.0 {
        painter.rect_filled(rect, 0.0, Color32::from_rgb(12, 10, 28));
        return;
    }
    painter.image(
        background.image.id(),
        image_rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    painter.rect_filled(rect, 0.0, WINDOW_SCRIM);
}

fn cover_rect(window: Rect, texture_size: Vec2) -> Rect {
    if texture_size.x <= 0.0 || texture_size.y <= 0.0 {
        return window;
    }
    let scale = (window.width() / texture_size.x).max(window.height() / texture_size.y);
    Rect::from_center_size(window.center(), texture_size * scale)
}

fn texture_uv(target: Rect, image_rect: Rect) -> Rect {
    if image_rect.width() <= 0.0 || image_rect.height() <= 0.0 {
        return Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    }
    Rect::from_min_max(
        pos2(
            (target.min.x - image_rect.min.x) / image_rect.width(),
            (target.min.y - image_rect.min.y) / image_rect.height(),
        ),
        pos2(
            (target.max.x - image_rect.min.x) / image_rect.width(),
            (target.max.y - image_rect.min.y) / image_rect.height(),
        ),
    )
}

#[cfg(test)]
mod tests {
    use eframe::egui::{Rect, Vec2, pos2};

    #[test]
    fn wrap_anywhere_breaks_inside_ascii_paths() {
        let wrapped = super::wrap_anywhere("打开：file.xlsx");
        assert!(wrapped.contains('\u{200B}'));
        assert!(
            wrapped
                .chars()
                .filter(|c| *c != '\u{200B}')
                .eq("打开：file.xlsx".chars())
        );
    }

    #[test]
    fn window_icon_is_square_rgba() {
        let icon = super::window_icon();
        assert_eq!(icon.width, super::WINDOW_ICON_SIZE);
        assert_eq!(icon.height, super::WINDOW_ICON_SIZE);
        assert_eq!(icon.width % 4, 0);
        assert_eq!(icon.height % 4, 0);
        assert_eq!(
            icon.rgba.len(),
            super::WINDOW_ICON_SIZE as usize * super::WINDOW_ICON_SIZE as usize * 4
        );
    }

    #[test]
    fn background_png_decodes() {
        let image = image::load_from_memory(super::BACKGROUND_PNG)
            .expect("背景图应能解码")
            .into_rgba8();
        assert!(image.width() > 0 && image.height() > 0);
        assert_eq!(
            image.as_raw().len(),
            image.width() as usize * image.height() as usize * 4
        );
        let small = image::imageops::resize(
            &image,
            (image.width() / 4).max(1),
            (image.height() / 4).max(1),
            image::imageops::FilterType::Triangle,
        );
        let blurred = image::imageops::blur(&small, 5.0);
        assert!(blurred.width() > 0 && blurred.height() > 0);
    }

    #[test]
    fn truncate_sha256_keeps_head_and_tail() {
        let hash = "d14d0123456789abcdef0123456789abcdef0123456789abcdef4b3b14";
        assert_eq!(super::truncate_sha256(hash), "d14d...4b3b14");
        assert_eq!(super::truncate_sha256("short"), "short");
    }

    #[test]
    fn cover_rect_fills_window() {
        let window = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(100.0, 50.0));
        let covered = super::cover_rect(window, Vec2::new(10.0, 10.0));
        assert_eq!(covered.height(), 100.0);
        assert_eq!(covered.width(), 100.0);
        assert_eq!(covered.center(), window.center());
    }

    #[test]
    fn copy_button_caption_shows_copied_feedback() {
        assert_eq!(super::copy_button_caption(false), "复制");
        assert_eq!(super::copy_button_caption(true), "已复制");
    }

    #[test]
    fn action_row_matches_button_height_and_cluster_padding() {
        assert_eq!(
            super::ACTION_ROW_HEIGHT,
            super::ACTION_BUTTON_MIN.y + super::ACTION_CLUSTER_PAD * 2.0
        );
    }

    #[test]
    fn closed_combo_matches_action_button_height() {
        assert_eq!(super::combo_closed_height(), super::ACTION_BUTTON_MIN.y);
    }

    #[test]
    fn diagnostic_popup_stays_within_space_below() {
        assert_eq!(super::DIAGNOSTIC_POPUP_ROWS, 6.0);
        assert_eq!(super::diagnostic_popup_max_height(36.0, 400.0), 216.0);
        assert_eq!(super::diagnostic_popup_max_height(36.0, 80.0), 80.0);
        assert_eq!(super::diagnostic_popup_max_height(36.0, -4.0), 0.0);
    }

    #[test]
    fn diagnostic_popup_inner_width_matches_closed_combo() {
        assert_eq!(super::diagnostic_popup_inner_width(400.0, 16.0), 384.0);
        assert_eq!(super::diagnostic_popup_inner_width(12.0, 16.0), 0.0);
    }

    #[test]
    fn texture_uv_maps_card_inside_cover() {
        let image = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(100.0, 100.0));
        let card = Rect::from_min_size(pos2(25.0, 25.0), Vec2::new(50.0, 50.0));
        let uv = super::texture_uv(card, image);
        assert!((uv.min.x - 0.25).abs() < f32::EPSILON);
        assert!((uv.max.x - 0.75).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod rendering_tests {
    use super::*;
    use crate::interfaces::gui_controller::{GuiOperationOutput, GuiTaskFactory, GuiTaskOutput};
    #[test]
    fn draws_widgets_from_controller_state() {
        let context = egui::Context::default();
        let factory = GuiTaskFactory::from_handler(|_, _, _| {
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("就绪")),
                None,
            ))
        });
        let mut state = WindowState::new(factory, context.clone());
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(980.0, 640.0))),
                ..Default::default()
            },
            |context| draw(context, &mut state, None, None),
        );
        assert!(!output.shapes.is_empty());
        assert!(!state.controller.view().is_running());
    }

    fn frame(
        context: &egui::Context,
        state: &mut WindowState,
        events: Vec<egui::Event>,
    ) -> Vec<(String, Rect)> {
        let time = context.input(|input| input.time) + 0.25;
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(980.0, 640.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |context| draw(context, state, None, None),
        );
        output
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::epaint::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .collect()
    }

    fn finish_task(state: &mut WindowState) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while state.controller.view().is_running() {
            assert!(std::time::Instant::now() < deadline);
            state.controller.drain_events().unwrap();
            std::thread::yield_now();
        }
    }

    fn click(context: &egui::Context, state: &mut WindowState, position: egui::Pos2) {
        for pressed in [true, false] {
            frame(
                context,
                state,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn settings_edits_require_save_and_help_only_appears_while_hovered() {
        use crate::interfaces::gui_controller::GuiOperation;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let saved = Arc::new(AtomicBool::new(false));
        let stored = saved.clone();
        let context = egui::Context::default();
        let factory = GuiTaskFactory::from_handler(move |operation, _, _| {
            if let GuiOperation::SaveSettings {
                original,
                preferences: enabled,
            } = operation
            {
                assert_eq!(
                    original.ship_acquisition_enabled,
                    stored.load(Ordering::Relaxed)
                );
                stored.store(enabled.ship_acquisition_enabled, Ordering::Relaxed);
            }
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("设置已读取").with_preferences(
                    crate::application::UserPreferences {
                        ship_acquisition_enabled: stored.load(Ordering::Relaxed),
                        ..Default::default()
                    },
                )),
                None,
            ))
        });
        let mut state = WindowState::new(factory, context.clone());
        frame(&context, &mut state, vec![]);
        let controls = frame(&context, &mut state, vec![]);
        let position = |texts: &[(String, Rect)], label: &str| {
            texts
                .iter()
                .find(|(text, _)| text == label)
                .unwrap()
                .1
                .center()
        };
        assert!(position(&controls, "设置").x > position(&controls, "查看错误详情").x);
        assert!(position(&controls, "卸载").x > position(&controls, "设置").x);
        click(&context, &mut state, position(&controls, "设置"));
        finish_task(&mut state);
        frame(&context, &mut state, vec![]);
        let texts = frame(
            &context,
            &mut state,
            vec![egui::Event::PointerMoved(pos2(0.0, 0.0))],
        );
        assert!(state.settings_open);
        assert!(
            !texts
                .iter()
                .any(|(text, _)| text.contains("开启后，同步时"))
        );
        let checkbox = position(&texts, "启用获取方式列");
        assert!(position(&texts, "保存").y - checkbox.y > 300.0);
        for _ in 0..2 {
            frame(
                &context,
                &mut state,
                vec![egui::Event::PointerMoved(checkbox)],
            );
        }
        let hovered = frame(&context, &mut state, vec![]);
        assert!(
            hovered
                .iter()
                .any(|(text, _)| text.contains("开启后，同步时"))
        );
        frame(
            &context,
            &mut state,
            vec![egui::Event::PointerMoved(pos2(0.0, 0.0))],
        );
        let away = frame(&context, &mut state, vec![]);
        assert!(!away.iter().any(|(text, _)| text.contains("开启后，同步时")));
        click(&context, &mut state, checkbox);
        assert_eq!(
            state.settings_draft.map(|p| p.ship_acquisition_enabled),
            Some(true)
        );
        assert!(!state.controller.view().is_running());
        assert!(!saved.load(Ordering::Relaxed));
        let texts = frame(&context, &mut state, vec![]);
        click(&context, &mut state, position(&texts, "保存"));
        finish_task(&mut state);
        assert!(saved.load(Ordering::Relaxed));
        assert_eq!(
            state
                .controller
                .view()
                .preferences()
                .map(|p| p.ship_acquisition_enabled),
            Some(true)
        );
        let texts = frame(&context, &mut state, vec![]);
        assert_eq!(
            state.settings_original,
            state.controller.view().preferences()
        );
        click(&context, &mut state, position(&texts, "启用获取方式列"));
        let texts = frame(&context, &mut state, vec![]);
        click(&context, &mut state, position(&texts, "保存"));
        finish_task(&mut state);
        assert!(!saved.load(Ordering::Relaxed));
        state.settings_draft = Some(crate::application::UserPreferences::default());
        state.settings_open = false;
        apply_commands(&mut state, vec![UiCommand::OpenSettings]);
        finish_task(&mut state);
        frame(&context, &mut state, vec![]);
        assert_eq!(
            state.settings_draft.map(|p| p.ship_acquisition_enabled),
            Some(false)
        );
    }
}

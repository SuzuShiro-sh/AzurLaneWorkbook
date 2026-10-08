//! 主界面的颜色、字体和 egui 视觉。

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, Style, Theme,
    ThemePreference, Visuals, epaint::Shadow,
};

const GUI_FONT: &[u8] = include_bytes!("../../../../assets/gui/fonts/NotoSansSC-Regular.ttf");

pub(super) const PANEL_FILL: Color32 = Color32::from_rgba_premultiplied(8, 12, 24, 224);
pub(super) const PANEL_STROKE: Color32 = Color32::from_rgba_premultiplied(40, 40, 40, 40);
pub(super) const PANEL_SHADOW: Shadow = Shadow {
    offset: [0, 8],
    blur: 18,
    spread: 0,
    color: Color32::from_black_alpha(88),
};
pub(super) const ACCENT: Color32 = Color32::from_rgb(120, 176, 255);
pub(super) const TEXT: Color32 = Color32::from_rgb(244, 248, 255);
pub(super) const TEXT_MUTED: Color32 = Color32::from_rgb(214, 226, 242);
pub(super) const TEXT_HINT: Color32 = Color32::from_rgb(228, 236, 248);
pub(super) const PRIMARY_BUTTON: Color32 = Color32::from_rgb(72, 140, 255);
pub(super) const WIDGET_FILL: Color32 = Color32::from_rgba_premultiplied(16, 24, 40, 235);
pub(super) const CLUSTER_FILL: Color32 = Color32::from_rgba_premultiplied(10, 10, 10, 10);
pub(super) const CLUSTER_STROKE: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
pub(super) const WINDOW_SCRIM: Color32 = Color32::from_black_alpha(72);
pub(super) const DISABLED_FADE: Color32 = Color32::from_rgb(148, 162, 196);
pub(super) const UNAVAILABLE_FILL: Color32 = Color32::from_rgba_premultiplied(56, 32, 20, 190);
pub(super) const UNAVAILABLE_STROKE: Color32 = Color32::from_rgb(210, 150, 88);
pub(super) const UNAVAILABLE_TEXT: Color32 = Color32::from_rgb(240, 208, 168);

pub(super) fn install(ctx: &eframe::egui::Context) {
    install_gui_font(ctx);
    apply_visuals(ctx);
}

pub(super) fn apply_visuals(ctx: &eframe::egui::Context) {
    ctx.set_theme(ThemePreference::Dark);
    let style = ui_style();
    ctx.set_style_of(Theme::Dark, style.clone());
    ctx.set_style_of(Theme::Light, style);
}

fn ui_style() -> Style {
    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = Color32::from_rgba_premultiplied(9, 13, 24, 220);
    visuals.override_text_color = Some(TEXT);
    visuals.widgets.inactive.bg_fill = WIDGET_FILL;
    visuals.widgets.inactive.weak_bg_fill = WIDGET_FILL;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, PANEL_STROKE);
    visuals.widgets.open.bg_fill = WIDGET_FILL;
    visuals.widgets.open.weak_bg_fill = WIDGET_FILL;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.bg_fill = Color32::from_rgba_premultiplied(49, 68, 107, 235);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgba_premultiplied(49, 68, 107, 235);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.active.bg_fill = Color32::from_rgba_premultiplied(48, 72, 125, 235);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(37, 64, 120, 220);
    visuals.widgets.noninteractive.bg_fill = WIDGET_FILL;
    visuals.widgets.noninteractive.weak_bg_fill = WIDGET_FILL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, DISABLED_FADE);
    visuals.extreme_bg_color = Color32::from_rgb(12, 18, 32);
    visuals.window_corner_radius = CornerRadius::same(14);
    visuals.menu_corner_radius = CornerRadius::same(10);
    let mut style = Style {
        visuals,
        ..Style::default()
    };
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style
}

fn install_gui_font(ctx: &eframe::egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "gui-sans".to_owned(),
        FontData::from_static(GUI_FONT).into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "gui-sans".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "gui-sans".to_owned());
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_gui_font_is_truetype() {
        assert!(super::GUI_FONT.len() > 1024);
        assert!(
            super::GUI_FONT.starts_with(&[0x00, 0x01, 0x00, 0x00])
                || super::GUI_FONT.starts_with(b"OTTO")
        );
    }
}

//! 任务进行中的进度条：圆角青蓝填充与沿进度跑步的 Q 版小人。

use std::time::Duration;

use eframe::egui::{
    self, Color32, ColorImage, Rect, Sense, TextureHandle, TextureOptions, Ui, pos2, vec2,
};

const ANIMA_PNG: [&[u8]; 14] = [
    include_bytes!("../../../../assets/gui/loading/loading_anima_01.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_02.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_03.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_04.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_05.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_06.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_07.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_08.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_09.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_10.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_11.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_12.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_13.png"),
    include_bytes!("../../../../assets/gui/loading/loading_anima_14.png"),
];

const CHIBI_HEIGHT: f32 = 40.0;
const CHIBI_ASPECT: f32 = 88.0 / 117.0;
const BAR_HEIGHT: f32 = 10.0;
const WIDGET_HEIGHT: f32 = CHIBI_HEIGHT + BAR_HEIGHT * 0.35;
const FRAME_RATE: f64 = 30.0;
const INDETERMINATE_SPEED: f64 = 0.6;
const TRACK_COLOR: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 38);
const FILL_COLOR: Color32 = Color32::from_rgb(70, 196, 255);
const REPAINT: Duration = Duration::from_millis(33);
const UV: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

pub(super) struct LoadingBarAssets {
    anima: [TextureHandle; 14],
}

pub(super) fn load(ctx: &egui::Context) -> LoadingBarAssets {
    LoadingBarAssets {
        anima: std::array::from_fn(|index| {
            load_png(ctx, &format!("gui-loading-anima-{index}"), ANIMA_PNG[index])
        }),
    }
}

pub(super) fn show(ui: &mut Ui, assets: &LoadingBarAssets, progress: Option<f32>) {
    ui.ctx().request_repaint_after(REPAINT);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, WIDGET_HEIGHT), Sense::hover());
    let time = ui.input(|input| input.time);
    let value = progress
        .map(|percent| percent.clamp(0.0, 1.0))
        .unwrap_or_else(|| ((time * INDETERMINATE_SPEED) % 1.0) as f32);
    paint(ui, assets, rect, value, time);
}

fn paint(ui: &Ui, assets: &LoadingBarAssets, rect: Rect, progress: f32, time: f64) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    let bar = Rect::from_min_max(
        pos2(rect.left(), rect.bottom() - BAR_HEIGHT),
        pos2(rect.right(), rect.bottom()),
    );
    let rounding = BAR_HEIGHT * 0.5;
    painter.rect_filled(bar, rounding, TRACK_COLOR);

    let fill_width = (bar.width() * progress).max(0.0);
    if fill_width > 0.0 {
        let fill = Rect::from_min_max(bar.min, pos2(bar.left() + fill_width, bar.bottom()));
        painter.rect_filled(fill, rounding, FILL_COLOR);
    }

    let chibi_height = CHIBI_HEIGHT.min(rect.height());
    let chibi_width = chibi_height * CHIBI_ASPECT;
    let travel = (bar.width() - chibi_width).max(0.0);
    let chibi = Rect::from_min_size(
        pos2(
            bar.left() + travel * progress,
            bar.center().y - chibi_height + 2.0,
        ),
        vec2(chibi_width, chibi_height),
    );
    let frame = ((time * FRAME_RATE).floor() as usize) % assets.anima.len();
    painter.image(assets.anima[frame].id(), chibi, UV, Color32::WHITE);
}

fn load_png(ctx: &egui::Context, name: &str, bytes: &[u8]) -> TextureHandle {
    let image = image::load_from_memory(bytes)
        .unwrap_or_else(|error| panic!("{name} 应能解码: {error}"))
        .into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color_image = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    ctx.load_texture(name, color_image, TextureOptions::LINEAR)
}

#[cfg(test)]
mod tests {
    #[test]
    fn loading_pngs_decode() {
        for bytes in super::ANIMA_PNG {
            let image = image::load_from_memory(bytes)
                .expect("加载条素材应能解码")
                .into_rgba8();
            assert!(image.width() > 0 && image.height() > 0);
            assert_eq!(
                image.as_raw().len(),
                image.width() as usize * image.height() as usize * 4
            );
        }
    }
}

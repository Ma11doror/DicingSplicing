pub mod editor;
pub mod export;
pub mod flash;
pub mod keys;
pub mod framing;
pub mod picture;
pub mod plugins;
pub mod settings;
pub mod sources;
pub mod start;
pub mod thumbnails;
pub mod timeline;
pub mod viewer;

use crate::core::edit::{Curve, Edge};
use egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, TextStyle, Ui};
use std::path::PathBuf;

pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "webm", "avi", "m4v"];
pub const SOUND_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"];
/// A GIF of one frame is an image, an animated one a video: ffprobe tells which.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif"];

/// How dots of a grip are apart, and how big they are.
const GRIP_STEP: f32 = 4.0;
const GRIP_DOT_RADIUS: f32 = 1.1;

/// The mark of anything that can be dragged, the same everywhere: dots, `across` by `down`. Six,
/// three in two rows, on a handle of its own; one row of three where there is little room, along
/// the longer side. Drawn, as the font may have no such characters.
pub fn grip_dots(painter: &Painter, center: Pos2, across: usize, down: usize, color: Color32) {
    let offset = |i: usize, count: usize| (i as f32 - (count as f32 - 1.0) / 2.0) * GRIP_STEP;
    for column in 0..across {
        for row in 0..down {
            painter.circle_filled(pos2(center.x + offset(column, across), center.y + offset(row, down)), GRIP_DOT_RADIUS, color);
        }
    }
}

/// An arrow up or down along `at.x`, the same everywhere: a line and a head, from 5 points on one
/// side of `at` to 6 on the other, the head's side. Drawn, as the font has no arrows.
pub fn paint_arrow(painter: &Painter, at: Pos2, up: bool, color: Color32) {
    let dir = if up { -1.0 } else { 1.0 };
    painter.line_segment([pos2(at.x, at.y - 5.0 * dir), pos2(at.x, at.y + 2.0 * dir)], Stroke::new(1.5, color));
    let head = vec![pos2(at.x - 3.5, at.y + dir), pos2(at.x + 3.5, at.y + dir), pos2(at.x, at.y + 6.0 * dir)];
    painter.add(Shape::convex_polygon(head, color, Stroke::NONE));
}

/// How big a curve's little graph is, in a list of curves.
const CURVE_ICON: [f32; 2] = [28.0, 16.0];

/// A fade at `edge` going by `curve`, drawn into `rect`: how much shows, from the left to the right.
pub fn paint_curve(painter: &Painter, rect: Rect, curve: Curve, edge: Edge, color: Color32) {
    let steps = 24;
    let points = (0..=steps)
        .map(|i| {
            let x = i as f64 / steps as f64;
            pos2(rect.left() + rect.width() * x as f32, rect.bottom() - rect.height() * curve.level(edge, x) as f32)
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(1.5, color)));
}

/// A curve in a list to choose from: its graph and its name, highlighted if it is the `selected` one.
pub fn curve_choice(ui: &mut Ui, curve: Curve, edge: Edge, selected: bool) -> Response {
    let visuals = ui.visuals().clone();
    let color = if selected { visuals.selection.stroke.color } else { visuals.text_color() };
    let text = ui.painter().layout_no_wrap(curve.name().to_owned(), TextStyle::Button.resolve(ui.style()), color);
    let (pad, gap) = (ui.spacing().button_padding, ui.spacing().item_spacing.x);
    let size = vec2(pad.x * 2.0 + CURVE_ICON[0] + gap + text.size().x, CURVE_ICON[1].max(text.size().y) + pad.y * 2.0);
    let (rect, response) = ui.allocate_exact_size(vec2(size.x.max(ui.available_width().min(240.0)), size.y), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 3.0, visuals.selection.bg_fill);
        } else if response.hovered() {
            painter.rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
        }
        let icon = Rect::from_min_size(pos2(rect.left() + pad.x, rect.center().y - CURVE_ICON[1] / 2.0), vec2(CURVE_ICON[0], CURVE_ICON[1]));
        painter.rect_stroke(icon, 2.0, Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
        paint_curve(painter, icon.shrink(2.0), curve, edge, visuals.text_color());
        painter.galley(pos2(icon.right() + gap, rect.center().y - text.size().y / 2.0), text, color);
    }
    response
}

/// Asks for a video file.
pub fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a video")
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_file()
}

/// Every kind of file a source can be: videos, sound files and images.
fn media_extensions() -> Vec<&'static str> {
    VIDEO_EXTENSIONS.iter().chain(SOUND_EXTENSIONS).chain(IMAGE_EXTENSIONS).copied().collect()
}

/// Asks for a file a source can be: a video, sound only, or an image.
pub fn pick_media() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a video, a sound file or an image")
        .add_filter("Video, sound and images", &media_extensions())
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("Sound", SOUND_EXTENSIONS)
        .add_filter("Images", IMAGE_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_file()
}

/// Asks for files sources can be: any number of videos, sound files and images at once.
pub fn pick_media_files() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose videos, sound files or images")
        .add_filter("Video, sound and images", &media_extensions())
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("Sound", SOUND_EXTENSIONS)
        .add_filter("Images", IMAGE_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_files()
        .unwrap_or_default()
}

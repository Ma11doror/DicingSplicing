//! The source view with the output frames on top of it, and the output preview.

use crate::core::preset::{self, SafeZone};
use crate::media::geometry::{self, Mask, Outline, Transform};
use egui::epaint::Vertex;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Mesh, Painter, Pos2, Rect, Sense, Shape, Stroke, TextureHandle};
use std::f32::consts::{FRAC_PI_2, TAU};

/// Room around the picture for the rotation arrows, in screen pixels.
const MARGIN: f32 = 34.0;
const ARROW_RADIUS: f32 = 11.0;
const ARROW_GRAB: f32 = 16.0;
/// How far the arrows stand off a circle, and off a rectangle's corner along each of its axes.
const ARROW_OFF_CIRCLE: f32 = 18.0;
const ARROW_OFF_CORNER: f32 = 13.0;
const SIDE_GRAB: f32 = 7.0;
const TOP_RADIUS: f32 = 5.0;
const SNAP_DISTANCE: f64 = 8.0;
const SNAP_LINE: Color32 = Color32::from_rgb(255, 200, 40);
/// Room for the lock in front of the main frame's label.
const LABEL_LOCK_ROOM: f32 = 15.0;
const WHEEL_ZOOM_SPEED: f64 = 0.003;

/// A frame to draw on the source.
#[derive(Clone)]
pub struct FrameOverlay {
    pub outline: Outline,
    pub color: Color32,
    pub label: String,
    pub thirds: bool,
    pub center: bool,
    pub safe: Option<SafeZone>,
    /// For the main frame: dragging a side changes the proportions instead of keeping them.
    pub free_ratio: bool,
}

/// The part of the main frame under the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    /// Everything that is not a handle: dragging moves the frame.
    Body,
    /// A side of the frame; the direction points outwards in output coordinates.
    Side([f64; 2]),
    Arrow(usize),
    Top,
}

#[derive(Clone, Copy)]
enum SourceDrag {
    Move { start: Transform, press: [f64; 2] },
    Rotate { start: Transform, arrow: usize, press_angle: f64 },
    /// `grab` is how far the pointer was from the side when pressed, so that the side does not jump to it.
    Resize { start: Transform, side: [f64; 2], grab: f64 },
    /// Like `Resize`, changing the proportions of `outline`, the frame's shape when the drag began.
    Reshape { start: Transform, outline: Outline, side: [f64; 2], grab: f64 },
}

impl SourceDrag {
    fn part(&self) -> Part {
        match *self {
            Self::Move { .. } => Part::Body,
            Self::Rotate { arrow, .. } => Part::Arrow(arrow),
            Self::Resize { side, .. } | Self::Reshape { side, .. } => Part::Side(side),
        }
    }
}

/// The picture on the screen, and the main frame's handles on it.
struct Stage {
    image: Rect,
    scale: f64,
    w: f64,
    h: f64,
    outline: Outline,
}

impl Stage {
    fn screen(&self, p: [f64; 2]) -> Pos2 {
        pos2(self.image.left() + (p[0] * self.scale) as f32, self.image.top() + (p[1] * self.scale) as f32)
    }

    fn source(&self, p: Pos2) -> [f64; 2] {
        [(p.x - self.image.left()) as f64 / self.scale, (p.y - self.image.top()) as f64 / self.scale]
    }

    fn arrows(&self, t: &Transform) -> [Pos2; 4] {
        let [fw, fh] = t.size(self.w, self.h, self.outline);
        [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]].map(|[x, y]: [f64; 2]| {
            let offset = match self.outline.mask {
                Mask::Circle => {
                    let d = (fw / 2.0 + ARROW_OFF_CIRCLE as f64 / self.scale) * std::f64::consts::FRAC_1_SQRT_2;
                    [x * d, y * d]
                }
                Mask::Rect => {
                    let off = ARROW_OFF_CORNER as f64 / self.scale;
                    [x * (fw / 2.0 + off), y * (fh / 2.0 + off)]
                }
            };
            self.screen(t.output_to_source(self.w, self.h, offset))
        })
    }

    fn top(&self, t: &Transform) -> Pos2 {
        let [_, fh] = t.size(self.w, self.h, self.outline);
        self.screen(t.output_to_source(self.w, self.h, [0.0, -fh / 2.0]))
    }

    fn part_at(&self, t: &Transform, p: Pos2) -> Part {
        if let Some(i) = self.arrows(t).iter().position(|a| a.distance(p) <= ARROW_GRAB) {
            return Part::Arrow(i);
        }
        if self.top(t).distance(p) <= TOP_RADIUS + 3.0 {
            return Part::Top;
        }
        let [fw, fh] = t.size(self.w, self.h, self.outline);
        let (half_w, half_h) = (fw / 2.0, fh / 2.0);
        let [u, v] = t.source_to_output(self.w, self.h, self.source(p));
        let grab = SIDE_GRAB as f64 / self.scale;
        match self.outline.mask {
            Mask::Circle => {
                let d = u.hypot(v);
                if d > 0.0 && (d - half_w).abs() <= grab {
                    return Part::Side([u / d, v / d]);
                }
            }
            Mask::Rect => {
                let (from_x, from_y) = ((u.abs() - half_w).abs(), (v.abs() - half_h).abs());
                let near_x = from_x <= grab && v.abs() <= half_h + grab;
                let near_y = from_y <= grab && u.abs() <= half_w + grab;
                if near_x && (!near_y || from_x <= from_y) {
                    return Part::Side([u.signum(), 0.0]);
                }
                if near_y {
                    return Part::Side([0.0, v.signum()]);
                }
            }
        }
        Part::Body
    }

    /// The pointer's distance from the center, along `side`, in source pixels.
    fn along(&self, t: &Transform, side: [f64; 2], p: Pos2) -> f64 {
        let [u, v] = t.source_to_output(self.w, self.h, self.source(p));
        u * side[0] + v * side[1]
    }

    fn begin_drag(&self, t: Transform, press: Pos2, reshape: bool) -> SourceDrag {
        // The top mark sits on a side: a drag from it moves that side, only a click resets the angle.
        let part = match self.part_at(&t, press) {
            Part::Top => Part::Side([0.0, -1.0]),
            part => part,
        };
        match part {
            Part::Arrow(arrow) => {
                let press_angle = screen_angle(self.screen(t.center(self.w, self.h)), press);
                SourceDrag::Rotate { start: t, arrow, press_angle }
            }
            Part::Side(side) => {
                let [fw, fh] = t.size(self.w, self.h, self.outline);
                let half = if self.outline.mask == Mask::Circle || side[0] != 0.0 { fw } else { fh } / 2.0;
                let grab = half - self.along(&t, side, press);
                if reshape && self.outline.mask == Mask::Rect {
                    SourceDrag::Reshape { start: t, outline: self.outline, side, grab }
                } else {
                    SourceDrag::Resize { start: t, side, grab }
                }
            }
            _ => SourceDrag::Move { start: t, press: self.source(press) },
        }
    }

    fn cursor(&self, t: &Transform, part: Part, dragging: bool) -> CursorIcon {
        match part {
            Part::Body => CursorIcon::Move,
            Part::Top => CursorIcon::PointingHand,
            Part::Arrow(_) if dragging => CursorIcon::Grabbing,
            Part::Arrow(_) => CursorIcon::Grab,
            Part::Side(side) => {
                let center = self.screen(t.center(self.w, self.h));
                let outwards = self.screen(t.output_to_source(self.w, self.h, side)) - center;
                let eighths = (outwards.y.atan2(outwards.x) / TAU * 8.0).round() as i32;
                match eighths.rem_euclid(4) {
                    0 => CursorIcon::ResizeHorizontal,
                    1 => CursorIcon::ResizeNwSe,
                    2 => CursorIcon::ResizeVertical,
                    _ => CursorIcon::ResizeNeSw,
                }
            }
        }
    }
}

/// The pointer's direction from `center`, in degrees clockwise on the screen.
fn screen_angle(center: Pos2, p: Pos2) -> f64 {
    ((p.y - center.y) as f64).atan2((p.x - center.x) as f64).to_degrees()
}

/// The target nearest to `value` within `tolerance`, as (value there, where to draw its line).
fn snap_axis(value: f64, targets: [(f64, f64); 3], tolerance: f64) -> (f64, Option<f64>) {
    let nearest = targets
        .into_iter()
        .filter(|(target, _)| (target - value).abs() <= tolerance)
        .min_by(|a, b| (a.0 - value).abs().total_cmp(&(b.0 - value).abs()));
    match nearest {
        Some((target, line)) => (target, Some(line)),
        None => (value, None),
    }
}

#[derive(Default)]
pub struct SourceView {
    drag: Option<SourceDrag>,
    /// Snap lines of the current move: x and y in source pixels.
    snapped: [Option<f64>; 2],
}

impl SourceView {
    /// The whole source frame with the output frames on it; the first of `frames` is the main one.
    ///
    /// Drag to move the frame (it snaps to the center and edges of the picture, Alt: no snapping),
    /// drag a side to resize it (Ctrl: the opposite side stays; it keeps the proportions unless the
    /// main frame has a free ratio, Shift: the other way round), drag an arrow to rotate it (Shift:
    /// 15° steps), click the top mark for 0°, wheel to zoom. A `note` dims the frame and says why.
    /// Returns the new proportions while they are being changed.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        texture: Option<&TextureHandle>,
        size: [f64; 2],
        frames: &[FrameOverlay],
        note: Option<&str>,
        t: &mut Transform,
    ) -> Option<f64> {
        let [w, h] = size;
        let area = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let margin = MARGIN as f64;
        let scale = ((area.width() as f64 - 2.0 * margin) / w).min((area.height() as f64 - 2.0 * margin) / h).max(0.01);
        let image = Rect::from_center_size(area.center(), vec2((w * scale) as f32, (h * scale) as f32));
        let mut stage = Stage { image, scale, w, h, outline: frames[0].outline };
        let modifiers = ui.input(|i| i.modifiers);
        let mut aspect = None;

        // Input first, so that the overlay shows this frame's result.
        if response.drag_started() {
            let press = ui.input(|i| i.pointer.press_origin());
            let reshape = frames[0].free_ratio != modifiers.shift;
            self.drag = press.map(|p| stage.begin_drag(*t, p, reshape));
        }
        self.snapped = [None; 2];
        if let (true, Some(drag), Some(p)) = (response.dragged(), self.drag, response.interact_pointer_pos()) {
            match drag {
                SourceDrag::Move { start, press } => {
                    let [x, y] = stage.source(p);
                    let center = start.center(w, h);
                    let (mut cx, mut cy) = (center[0] + x - press[0], center[1] + y - press[1]);
                    if !modifiers.alt {
                        let [ex, ey] = stage.outline.extents(start.size(w, h, stage.outline), start.angle_deg);
                        let tolerance = SNAP_DISTANCE / scale;
                        (cx, self.snapped[0]) = snap_axis(cx, [(w / 2.0, w / 2.0), (ex, 0.0), (w - ex, w)], tolerance);
                        (cy, self.snapped[1]) = snap_axis(cy, [(h / 2.0, h / 2.0), (ey, 0.0), (h - ey, h)], tolerance);
                    }
                    *t = Transform { cx: cx / w, cy: cy / h, ..start };
                }
                SourceDrag::Rotate { start, press_angle, .. } => {
                    // The frame turns with the pointer, so the picture in it turns the other way.
                    // Starting from `start` each time gives back the zoom that the turn took away.
                    let turned = screen_angle(stage.screen(start.center(w, h)), p) - press_angle;
                    let mut angle = start.angle_deg - turned;
                    if modifiers.shift {
                        angle = (angle / 15.0).round() * 15.0;
                    }
                    *t = Transform { angle_deg: geometry::normalize_angle(angle), ..start };
                }
                SourceDrag::Resize { start, side, grab } => {
                    let extent = stage.along(&start, side, p) + grab;
                    *t = start.resized(w, h, stage.outline, side, extent, modifiers.command);
                }
                SourceDrag::Reshape { start, outline, side, grab } => {
                    let extent = stage.along(&start, side, p) + grab;
                    let snap = |a: f64| if modifiers.alt { a } else { preset::snap_aspect(a) };
                    let (next, new_aspect) = start.reshaped(w, h, outline, side, extent, modifiers.command, snap);
                    *t = next;
                    stage.outline = Outline { aspect: new_aspect, mask: outline.mask };
                    aspect = Some(new_aspect);
                }
            }
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        if response.clicked() {
            let on_top = response.interact_pointer_pos().is_some_and(|p| stage.part_at(t, p) == Part::Top);
            if on_top {
                t.angle_deg = 0.0;
            }
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            t.zoom *= (scroll as f64 * WHEEL_ZOOM_SPEED).exp();
        }
        *t = t.clamped(w, h, stage.outline);

        let hot = self.drag.map(|drag| drag.part()).or_else(|| response.hover_pos().map(|p| stage.part_at(t, p)));
        if let (Some(part), true) = (hot, response.hovered() || self.drag.is_some()) {
            ui.ctx().set_cursor_icon(stage.cursor(t, part, self.drag.is_some()));
        }

        let painter = ui.painter_at(area);
        match texture {
            Some(texture) => {
                let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
                painter.image(texture.id(), image, uv, Color32::WHITE);
            }
            None => {
                painter.rect_filled(image, 0.0, Color32::from_gray(28));
                painter.text(image.center(), Align2::CENTER_CENTER, "Preparing frames…", FontId::proportional(16.0), Color32::GRAY);
            }
        }
        if let Some(note) = note {
            painter.rect_filled(image, 0.0, Color32::from_black_alpha(140));
            let at = pos2(image.center().x, image.top() + 10.0);
            painter.text(at, Align2::CENTER_TOP, note, FontId::proportional(15.0), Color32::from_white_alpha(200));
        }

        let main = FrameOverlay { outline: stage.outline, ..frames[0].clone() };
        let [fw, fh] = t.size(w, h, stage.outline);
        let center = stage.screen(t.center(w, h));
        let corners = t.corners(w, h, stage.outline).map(|p| stage.screen(p));
        let clipped = painter.with_clip_rect(image);
        let shade = Color32::from_black_alpha(160);
        match stage.outline.mask {
            Mask::Circle => shade_outside_circle(&clipped, image, center, (fw / 2.0 * scale) as f32, shade),
            Mask::Rect => shade_outside_rect(&clipped, &corners, shade),
        }
        for frame in &frames[1..] {
            paint_reference(&painter, &stage, t, frame, frame.outline.fit(fw, fh));
        }
        paint_snap_lines(&painter, &stage, self.snapped);
        paint_main(&painter, &stage, t, &main, hot);

        if let (Some(drag), Some(p)) = (self.drag, response.interact_pointer_pos()) {
            let text = match drag {
                SourceDrag::Move { .. } => None,
                SourceDrag::Rotate { .. } => Some(format!("{:.1}°", t.angle_deg)),
                SourceDrag::Resize { .. } => Some(format!("×{:.2}", t.zoom)),
                SourceDrag::Reshape { .. } => aspect.map(|a| preset::Ratio::from_aspect(a).label()),
            };
            if let Some(text) = text {
                paint_label(&painter, p + vec2(16.0, 16.0), text, Color32::WHITE, None);
            }
        }
        aspect
    }
}

fn paint_main(painter: &Painter, stage: &Stage, t: &Transform, frame: &FrameOverlay, hot: Option<Part>) {
    let (w, h) = (stage.w, stage.h);
    let [fw, fh] = t.size(w, h, frame.outline);
    let corners = t.corners(w, h, frame.outline).map(|p| stage.screen(p));
    let center = stage.screen(t.center(w, h));
    let backing = Stroke::new(3.5, Color32::from_black_alpha(110));
    let width = if matches!(hot, Some(Part::Side(_))) { 3.0 } else { 2.0 };
    let line = Stroke::new(width, frame.color);

    paint_guides(painter, stage, t, frame, [fw, fh]);
    match frame.outline.mask {
        Mask::Circle => {
            painter.add(Shape::closed_line(corners.to_vec(), Stroke::new(1.0, Color32::from_white_alpha(90))));
            let radius = (fw / 2.0 * stage.scale) as f32;
            painter.circle_stroke(center, radius, backing);
            painter.circle_stroke(center, radius, line);
        }
        Mask::Rect => {
            painter.add(Shape::closed_line(corners.to_vec(), backing));
            painter.add(Shape::closed_line(corners.to_vec(), line));
        }
    }
    for (i, arrow) in stage.arrows(t).into_iter().enumerate() {
        paint_arrow(painter, arrow, center, frame.color, hot == Some(Part::Arrow(i)));
    }
    let top_width = if hot == Some(Part::Top) { 3.0 } else { 2.0 };
    painter.circle(stage.top(t), TOP_RADIUS, Color32::from_gray(16), Stroke::new(top_width, frame.color));

    let label_at = match frame.outline.mask {
        Mask::Circle => center - vec2((fw / 2.0 * stage.scale) as f32, (fh / 2.0 * stage.scale) as f32 + 20.0),
        Mask::Rect => corners[0] + vec2(3.0, 3.0),
    };
    paint_label(painter, label_at, frame.label.clone(), frame.color, Some(frame.free_ratio));
}

/// The color of a lock: an open one should catch the eye.
pub fn lock_color(free: bool) -> Color32 {
    if free {
        Color32::from_rgb(255, 205, 60)
    } else {
        Color32::from_rgb(235, 235, 235)
    }
}

/// A padlock `size` pixels tall around `center`; `open` lifts its shackle and swings it aside.
pub fn paint_lock(painter: &Painter, center: Pos2, size: f32, open: bool, color: Color32) {
    const STEPS: usize = 10;
    let body = Rect::from_center_size(center + vec2(0.0, size * 0.22), vec2(size * 0.72, size * 0.52));
    painter.rect_filled(body, size * 0.1, color);

    let radius = size * 0.2;
    let (arch_x, rise) = if open { (body.right() - radius * 0.2, size * 0.28) } else { (center.x, size * 0.16) };
    let arch_y = body.top() - rise;
    let left = pos2(arch_x - radius, body.top());
    let mut shackle = vec![left];
    for i in 0..=STEPS {
        let (sin, cos) = (std::f32::consts::PI * i as f32 / STEPS as f32).sin_cos();
        shackle.push(pos2(arch_x - radius * cos, arch_y - radius * sin));
    }
    // A closed shackle goes back into the body, an open one ends in the air.
    let right_end = if open { arch_y + size * 0.06 } else { body.top() };
    shackle.push(pos2(arch_x + radius, right_end));
    painter.add(Shape::line(shackle, Stroke::new((size * 0.12).max(1.3), color)));
}

/// A frame for comparison: dashed, inside the main frame, with the same center and angle.
fn paint_reference(painter: &Painter, stage: &Stage, t: &Transform, frame: &FrameOverlay, size: [f64; 2]) {
    let [fw, fh] = size;
    let at = |u: f64, v: f64| stage.screen(t.output_to_source(stage.w, stage.h, [u, v]));
    let (rx, ry) = (fw / 2.0, fh / 2.0);
    let stroke = Stroke::new(1.3, frame.color);
    let line: Vec<Pos2> = match frame.outline.mask {
        Mask::Circle => (0..=72)
            .map(|i| {
                let (sin, cos) = (i as f64 / 72.0 * std::f64::consts::TAU).sin_cos();
                at(rx * cos, rx * sin)
            })
            .collect(),
        Mask::Rect => vec![at(-rx, -ry), at(rx, -ry), at(rx, ry), at(-rx, ry), at(-rx, -ry)],
    };
    painter.extend(Shape::dashed_line(&line, stroke, 6.0, 5.0));
    paint_guides(painter, stage, t, frame, size);
    paint_label(painter, at(-rx, -ry) + vec2(3.0, 3.0), frame.label.clone(), frame.color, None);
}

fn paint_guides(painter: &Painter, stage: &Stage, t: &Transform, frame: &FrameOverlay, size: [f64; 2]) {
    let [fw, fh] = size;
    let at = |u: f64, v: f64| stage.screen(t.output_to_source(stage.w, stage.h, [u, v]));
    let (rx, ry) = (fw / 2.0, fh / 2.0);
    if frame.thirds {
        let stroke = Stroke::new(1.0, frame.color.gamma_multiply(0.7));
        for k in [-1.0 / 6.0, 1.0 / 6.0] {
            painter.line_segment([at(k * fw, -ry), at(k * fw, ry)], stroke);
            painter.line_segment([at(-rx, k * fh), at(rx, k * fh)], stroke);
        }
    }
    if frame.center {
        let arm = 8.0 / stage.scale;
        let stroke = Stroke::new(1.2, frame.color);
        painter.line_segment([at(-arm, 0.0), at(arm, 0.0)], stroke);
        painter.line_segment([at(0.0, -arm), at(0.0, arm)], stroke);
    }
    if let Some(safe) = frame.safe {
        let (left, right) = (-rx + safe.left * fw, rx - safe.right * fw);
        let (top, bottom) = (-ry + safe.top * fh, ry - safe.bottom * fh);
        let line = [at(left, top), at(right, top), at(right, bottom), at(left, bottom), at(left, top)];
        painter.extend(Shape::dashed_line(&line, Stroke::new(1.0, frame.color), 3.0, 3.0));
        painter.text(at(left, top) + vec2(4.0, 3.0), Align2::LEFT_TOP, "safe", FontId::proportional(11.0), frame.color);
    }
}

fn paint_snap_lines(painter: &Painter, stage: &Stage, snapped: [Option<f64>; 2]) {
    let stroke = Stroke::new(1.0, SNAP_LINE);
    if let Some(x) = snapped[0] {
        painter.line_segment([stage.screen([x, 0.0]), stage.screen([x, stage.h])], stroke);
    }
    if let Some(y) = snapped[1] {
        painter.line_segment([stage.screen([0.0, y]), stage.screen([stage.w, y])], stroke);
    }
}

/// A rounded arrow at `at`, bulging away from `center`.
fn paint_arrow(painter: &Painter, at: Pos2, center: Pos2, color: Color32, hot: bool) {
    const STEPS: usize = 12;
    const SPAN: f32 = 1.05;
    let outwards = at - center;
    let facing = outwards.y.atan2(outwards.x);
    let on_arc = |angle: f32| at + ARROW_RADIUS * vec2(angle.cos(), angle.sin());
    let arc: Vec<Pos2> = (0..=STEPS).map(|i| on_arc(facing - SPAN + 2.0 * SPAN * i as f32 / STEPS as f32)).collect();
    let tip = on_arc(facing + SPAN);
    let along = facing + SPAN + FRAC_PI_2;
    let barb = |turn: f32| tip + 6.0 * vec2((along + turn).cos(), (along + turn).sin());
    let head = vec![barb(2.6), tip, barb(-2.6)];

    let backing = Stroke::new(4.0, Color32::from_black_alpha(130));
    let line = Stroke::new(if hot { 3.0 } else { 2.0 }, color);
    for stroke in [backing, line] {
        painter.add(Shape::line(arc.clone(), stroke));
        painter.add(Shape::line(head.clone(), stroke));
    }
}

/// A small tag with `text`, and in front of it a lock if one is given: open or closed.
fn paint_label(painter: &Painter, at: Pos2, text: String, color: Color32, lock: Option<bool>) {
    let room = if lock.is_some() { LABEL_LOCK_ROOM } else { 0.0 };
    let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), color);
    let rect = Rect::from_min_size(at, galley.size() + vec2(8.0 + room, 3.0));
    painter.rect_filled(rect, 3.0, Color32::from_black_alpha(190));
    if let Some(open) = lock {
        paint_lock(painter, pos2(at.x + 10.0, rect.center().y), 12.0, open, lock_color(open));
    }
    painter.galley(at + vec2(4.0 + room, 1.0), galley, color);
}

#[derive(Default)]
pub struct OutputView;

impl OutputView {
    /// What the output looks like: the transformed picture under the frame's mask, `box_size` pixels
    /// large. Dragging moves the picture, the wheel zooms.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        texture: Option<&TextureHandle>,
        size: [f64; 2],
        outline: Outline,
        t: &mut Transform,
        box_size: egui::Vec2,
    ) {
        let [w, h] = size;
        let (rect, response) = ui.allocate_exact_size(box_size, Sense::drag());

        if response.dragged() {
            // The picture follows the pointer, so the sampled area moves the opposite way.
            let d = response.drag_delta();
            let source_per_pixel = t.size(w, h, outline)[0] / box_size.x as f64;
            let [cx, cy] = t.output_to_source(w, h, [-d.x as f64 * source_per_pixel, -d.y as f64 * source_per_pixel]);
            t.cx = cx / w;
            t.cy = cy / h;
        }
        if response.hovered() {
            // Taken from the input so that the panel's scroll area does not scroll as well.
            let scroll = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta).y);
            t.zoom *= (scroll as f64 * WHEEL_ZOOM_SPEED).exp();
            ui.ctx().set_cursor_icon(if response.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
        }
        *t = t.clamped(w, h, outline);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::BLACK);
        if let Some(texture) = texture {
            // The output corners sample the turned frame on the source.
            let mut mesh = Mesh::with_texture(texture.id());
            let screen = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
            for (pos, [x, y]) in screen.into_iter().zip(t.corners(w, h, outline)) {
                let uv = pos2((x / w) as f32, (y / h) as f32);
                mesh.vertices.push(Vertex { pos, uv, color: Color32::WHITE });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(Shape::mesh(mesh));
        }
        let border = Stroke::new(1.0, ui.visuals().weak_text_color());
        match outline.mask {
            Mask::Circle => {
                shade_outside_circle(&painter, rect, rect.center(), box_size.x / 2.0, ui.visuals().panel_fill);
                painter.circle_stroke(rect.center(), box_size.x / 2.0, border);
            }
            Mask::Rect => {
                painter.rect_stroke(rect, 0.0, border);
            }
        }
    }
}

/// Covers everything in `area` outside the circle with `color`.
fn shade_outside_circle(painter: &Painter, area: Rect, center: Pos2, radius: f32, color: Color32) {
    // A ring wide enough to reach every corner of the area. Circle strokes in egui are drawn
    // outside the radius, so the ring starts exactly at the circle's edge.
    let width = area.size().length() + 2.0 * center.distance(area.center()) + 2.0;
    painter.circle_stroke(center, radius, Stroke::new(width, color));
}

/// Covers everything the painter can reach outside the rectangle with `color`. The outside is cut
/// into four pinwheel blades, one per side, so that no part of it is covered twice.
fn shade_outside_rect(painter: &Painter, corners: &[Pos2; 4], color: Color32) {
    let reach = painter.clip_rect().size().length() * 4.0;
    let direction = |i: usize| (corners[(i + 1) % 4] - corners[i]).normalized();
    let mut mesh = Mesh::default();
    for i in 0..4 {
        let (from, to) = (corners[i], corners[(i + 1) % 4]);
        let first = mesh.vertices.len() as u32;
        mesh.colored_vertex(from, color);
        mesh.colored_vertex(to, color);
        mesh.colored_vertex(to + direction(i) * reach, color);
        mesh.colored_vertex(from + direction((i + 3) % 4) * reach, color);
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first, first + 2, first + 3);
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 1920.0;
    const H: f64 = 1080.0;

    fn stage(aspect: f64, mask: Mask) -> Stage {
        let image = Rect::from_min_size(pos2(100.0, 50.0), vec2(960.0, 540.0));
        Stage { image, scale: 0.5, w: W, h: H, outline: Outline { aspect, mask } }
    }

    #[test]
    fn finds_the_parts_of_a_rectangle() {
        // 9:16 at zoom 1: 303.75 px wide on the screen, centered at 580, 320.
        let s = stage(9.0 / 16.0, Mask::Rect);
        let t = Transform::default();
        let (left, right, top, bottom) = (580.0 - 151.875, 580.0 + 151.875, 50.0, 590.0);

        assert_eq!(s.part_at(&t, pos2(580.0, 320.0)), Part::Body);
        assert_eq!(s.part_at(&t, pos2(100.0, 60.0)), Part::Body);
        assert_eq!(s.part_at(&t, pos2(right, 320.0)), Part::Side([1.0, 0.0]));
        assert_eq!(s.part_at(&t, pos2(left - 4.0, 200.0)), Part::Side([-1.0, 0.0]));
        assert_eq!(s.part_at(&t, pos2(500.0, bottom)), Part::Side([0.0, 1.0]));
        assert_eq!(s.part_at(&t, pos2(580.0, top)), Part::Top);
        assert_eq!(s.part_at(&t, pos2(left - 13.0, top - 13.0)), Part::Arrow(0));
        assert_eq!(s.part_at(&t, pos2(right + 13.0, bottom + 13.0)), Part::Arrow(2));
    }

    #[test]
    fn finds_the_parts_of_a_turned_rectangle() {
        let s = stage(1.0, Mask::Rect);
        let t = Transform { zoom: 2.0, angle_deg: 45.0, ..Default::default() };
        // Half the side is 135 px on the screen; the output's right side is turned up by 45°.
        let [x, y] = t.output_to_source(W, H, [270.0, 0.0]);
        assert_eq!(s.part_at(&t, s.screen([x, y])), Part::Side([1.0, 0.0]));
        let [x, y] = t.output_to_source(W, H, [270.0 + 26.0, 270.0 + 26.0]);
        assert_eq!(s.part_at(&t, s.screen([x, y])), Part::Arrow(2));
    }

    #[test]
    fn finds_the_parts_of_a_circle() {
        let s = stage(1.0, Mask::Circle);
        let t = Transform { zoom: 2.0, ..Default::default() };
        // Radius 135 px on the screen around 580, 320.
        assert_eq!(s.part_at(&t, pos2(580.0, 320.0)), Part::Body);
        assert_eq!(s.part_at(&t, pos2(580.0 + 135.0, 320.0)), Part::Side([1.0, 0.0]));
        assert_eq!(s.part_at(&t, pos2(580.0, 320.0 - 135.0)), Part::Top);
        let d = (135.0 + ARROW_OFF_CIRCLE) * std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(s.part_at(&t, pos2(580.0 + d, 320.0 + d)), Part::Arrow(2));
    }

    #[test]
    fn dragging_starts_the_right_gesture() {
        let s = stage(9.0 / 16.0, Mask::Rect);
        let t = Transform { zoom: 2.0, ..Default::default() };
        let right = 580.0 + 151.875 / 2.0;
        match s.begin_drag(t, pos2(right + 3.0, 320.0), false) {
            SourceDrag::Resize { side, grab, .. } => {
                assert_eq!(side, [1.0, 0.0]);
                assert!((grab + 6.0).abs() < 1e-3, "{grab}");
            }
            _ => panic!("expected a resize"),
        }
        assert!(matches!(s.begin_drag(t, pos2(right, 320.0), true), SourceDrag::Reshape { .. }));
        assert!(matches!(s.begin_drag(t, pos2(580.0, 320.0), false), SourceDrag::Move { .. }));

        // A circle has no proportions to change.
        let s = stage(1.0, Mask::Circle);
        assert!(matches!(s.begin_drag(t, pos2(580.0 + 135.0, 320.0), true), SourceDrag::Resize { .. }));
    }

    #[test]
    fn snaps_to_the_nearest_target_within_reach() {
        let targets = [(960.0, 960.0), (300.0, 0.0), (1620.0, 1920.0)];
        assert_eq!(snap_axis(950.0, targets, 16.0), (960.0, Some(960.0)));
        assert_eq!(snap_axis(310.0, targets, 16.0), (300.0, Some(0.0)));
        assert_eq!(snap_axis(800.0, targets, 16.0), (800.0, None));
    }
}

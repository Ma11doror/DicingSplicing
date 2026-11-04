//! The timeline: a ruler and tracks of clips that slide under a fixed playhead in the center.
//! Dragging scrubs, the wheel zooms. The export range is edited on the ruler, the selected clip
//! is trimmed by the handles on its edges.

use crate::core::edit::{ClipId, Sequence, Span};
use crate::media::timecode;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Rounding, Sense, Shape, Stroke};

const LABEL_HEIGHT: f32 = 26.0;
const RULER_HEIGHT: f32 = 32.0;
/// Bottom of the ruler, where the export range is drawn; dragging it moves the range.
const STRIP_HEIGHT: f32 = 12.0;
const TRACK_HEIGHT: f32 = 44.0;
const CLIP_MARGIN: f32 = 4.0;
const HANDLE_WIDTH: f32 = 12.0;
const ANCHOR_WIDTH: f32 = 9.0;
/// How far outside a handle or an anchor it can still be grabbed.
const GRAB_MARGIN: f32 = 6.0;
const SNAP_DISTANCE: f32 = 8.0;
/// Dragging an edge this close to a side of the timeline scrolls it, at up to this many px/s.
const AUTOSCROLL_ZONE: f32 = 48.0;
const AUTOSCROLL_SPEED: f32 = 900.0;
/// Minimum distance between labelled ticks.
const MAJOR_TICK_SPACING: f64 = 90.0;

const PLAYHEAD_COLOR: Color32 = Color32::from_rgb(255, 86, 64);
const EXPORT_COLOR: Color32 = Color32::from_rgb(64, 150, 240);
const CLIP_COLOR: Color32 = Color32::from_rgb(72, 88, 128);
const HANDLE_COLOR: Color32 = Color32::from_rgb(255, 200, 40);
/// Laid over everything outside the export range.
const OUTSIDE_EXPORT_TINT: Color32 = Color32::from_rgba_premultiplied(56, 56, 56, 110);

pub struct Timeline {
    /// Pixels per second. Set from the bar width on first show.
    pps: Option<f32>,
    drag: Option<Drag>,
    /// Speed of the bar after a fling, in seconds of timeline per second.
    fling: f64,
    /// Where the dragged edge (or the export start, when moving the range) is relative to the
    /// pointer, in seconds: the edge keeps its distance to where it was grabbed.
    grab: f64,
    /// Where the drag started, to scroll only when the pointer heads for a side.
    press_x: f32,
    /// Where the dragged edge has snapped to.
    snapped: Option<f64>,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Scrub { start_time: f64, start_x: f32 },
    ExportStart,
    ExportEnd,
    ExportMove,
    ClipStart(ClipId),
    ClipEnd(ClipId),
}

#[derive(Default)]
pub struct TimelineResponse {
    /// The user moved the playhead.
    pub seeked: bool,
    /// The user grabbed an edge or the export range.
    pub edit_started: bool,
    /// While an edge is dragged: the time whose frame the preview should show instead of the playhead's.
    pub peek: Option<f64>,
}

/// Time to screen mapping: `pos` is under `center_x`.
#[derive(Clone, Copy)]
struct View {
    center_x: f32,
    pps: f32,
    pos: f64,
}

impl View {
    fn x(self, t: f64) -> f32 {
        self.center_x + ((t - self.pos) as f32) * self.pps
    }

    fn t(self, x: f32) -> f64 {
        self.pos + ((x - self.center_x) / self.pps) as f64
    }
}

struct Layout {
    rect: Rect,
    /// The ruler and the tracks.
    body: Rect,
    ruler: Rect,
    strip: Rect,
}

impl Layout {
    fn new(rect: Rect) -> Self {
        let body = Rect::from_min_max(pos2(rect.left(), rect.top() + LABEL_HEIGHT), rect.max);
        let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER_HEIGHT));
        let strip = Rect::from_min_max(pos2(ruler.left(), ruler.bottom() - STRIP_HEIGHT), ruler.max);
        Self { rect, body, ruler, strip }
    }

    /// The first track is at the bottom.
    fn row(&self, track: usize) -> Rect {
        let bottom = self.body.bottom() - track as f32 * TRACK_HEIGHT;
        Rect::from_x_y_ranges(self.body.x_range(), bottom - TRACK_HEIGHT..=bottom)
    }

    fn block(&self, track: usize, span: Span, view: View) -> Rect {
        let row = self.row(track).shrink2(vec2(0.0, CLIP_MARGIN));
        Rect::from_x_y_ranges(view.x(span.start)..=view.x(span.end), row.y_range())
    }
}

impl Timeline {
    /// `scale` is the pixels per second to start with, if they are known from before.
    pub fn new(scale: Option<f32>) -> Self {
        Self { pps: scale, drag: None, fling: 0.0, grab: 0.0, press_x: 0.0, snapped: None }
    }

    pub fn scale(&self) -> Option<f32> {
        self.pps
    }

    /// Draws the timeline and applies input to the playhead and the sequence.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        position: &mut f64,
        sequence: &mut Sequence,
        selected: Option<ClipId>,
        fps: f64,
    ) -> TimelineResponse {
        let end = sequence.end();
        let frame = 1.0 / fps;
        let width = ui.available_width();
        let height = LABEL_HEIGHT + RULER_HEIGHT + TRACK_HEIGHT * sequence.tracks.len().max(1) as f32;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let layout = Layout::new(rect);
        let mut out = TimelineResponse::default();

        // From "half the bar holds the whole timeline" to "a frame is 60 px wide".
        let max_pps = (fps * 60.0) as f32;
        let min_pps = (width * 0.5 / end.max(0.1) as f32).min(max_pps);
        let mut pps = self.pps.unwrap_or(width / 40.0).clamp(min_pps, max_pps);
        let mut pos = *position;

        // Wheel (or pinch, or ctrl+wheel) zooms; horizontal scrolling scrubs.
        if response.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            pps = (pps * (scroll.y * 0.004).exp() * zoom).clamp(min_pps, max_pps);
            if scroll.x != 0.0 {
                pos -= (scroll.x / pps) as f64;
                out.seeked = true;
                self.fling = 0.0;
            }
        }
        let view = |pos: f64| View { center_x: rect.center().x, pps, pos };

        if response.drag_started() {
            self.fling = 0.0;
            if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos()) {
                let drag = hit(p, &layout, view(pos), sequence, selected).unwrap_or(Drag::Scrub { start_time: pos, start_x: p.x });
                if let Some(edge) = edge_time(sequence, drag) {
                    self.grab = edge - view(pos).t(p.x);
                    out.edit_started = true;
                }
                self.press_x = p.x;
                self.drag = Some(drag);
            }
        }
        if let (true, Some(drag), Some(p)) = (response.dragged(), self.drag, response.interact_pointer_pos()) {
            if let Drag::Scrub { start_time, start_x } = drag {
                pos = start_time - ((p.x - start_x) / pps) as f64;
                out.seeked = true;
            } else {
                let speed = autoscroll_speed(p.x, self.press_x, rect.left(), rect.right());
                if speed != 0.0 {
                    pos = (pos + (speed * ui.input(|i| i.stable_dt).min(0.1) / pps) as f64).clamp(0.0, end);
                    out.seeked = true;
                    ui.ctx().request_repaint();
                }
                let snap_range = if ui.input(|i| i.modifiers.alt) { 0.0 } else { (SNAP_DISTANCE / pps) as f64 };
                self.snapped = edit(sequence, drag, view(pos).t(p.x) + self.grab, pos, snap_range, frame);
            }
        }
        if response.drag_stopped() {
            if let Some(Drag::Scrub { .. }) = self.drag {
                let velocity = ui.input(|i| i.pointer.velocity().x);
                if velocity.abs() > 150.0 {
                    self.fling = -(velocity / pps) as f64;
                }
            }
            self.drag = None;
            self.snapped = None;
        }
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                pos = view(pos).t(p.x);
                out.seeked = true;
                self.fling = 0.0;
            }
        }
        if self.drag.is_none() && self.fling != 0.0 {
            let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
            pos += self.fling * dt;
            self.fling *= (-4.0 * dt).exp();
            if (self.fling * pps as f64).abs() < 20.0 {
                self.fling = 0.0;
            }
            out.seeked = true;
            ui.ctx().request_repaint();
        }
        let clamped = pos.clamp(0.0, end);
        if clamped != pos {
            self.fling = 0.0;
        }
        pos = clamped;
        let view = view(pos);

        let hovered = response.hover_pos().and_then(|p| hit(p, &layout, view, sequence, selected));
        if response.hover_pos().is_some() {
            let icon = match self.drag.or(hovered) {
                Some(Drag::Scrub { .. }) => CursorIcon::Grabbing,
                Some(Drag::ExportMove) if self.drag.is_some() => CursorIcon::Grabbing,
                Some(Drag::ExportMove) | None => CursorIcon::Grab,
                Some(_) => CursorIcon::ResizeHorizontal,
            };
            ui.ctx().set_cursor_icon(icon);
        }

        out.peek = match self.drag {
            Some(Drag::ExportStart) => Some(sequence.export.start),
            Some(Drag::ExportEnd) => Some(sequence.export.end - frame),
            Some(Drag::ClipStart(id)) => Some(sequence.clip(id).start),
            Some(Drag::ClipEnd(id)) => Some(sequence.clip(id).end() - frame),
            _ => None,
        };
        *position = pos;
        self.pps = Some(pps);
        self.paint(ui, &layout, view, sequence, selected, self.drag.or(hovered), fps);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        ui: &egui::Ui,
        layout: &Layout,
        view: View,
        sequence: &Sequence,
        selected: Option<ClipId>,
        active: Option<Drag>,
        fps: f64,
    ) {
        let painter = ui.painter_at(layout.rect);
        let visuals = ui.visuals();
        let tick_color = visuals.weak_text_color();
        let body = layout.body;
        let across = |x: f32| [pos2(x, body.top()), pos2(x, body.bottom())];
        let x_span = |from: f32, to: f32| from.max(body.left())..=to.min(body.right());

        painter.rect_filled(body, 3.0, visuals.extreme_bg_color);
        painter.hline(body.x_range(), layout.ruler.bottom(), visuals.widgets.noninteractive.bg_stroke);

        // Before the start and after the end of the timeline.
        let outside = Color32::from_black_alpha(110);
        let (x_start, x_end) = (view.x(0.0), view.x(sequence.end()));
        if x_start > body.left() {
            painter.rect_filled(Rect::from_x_y_ranges(x_span(body.left(), x_start), body.y_range()), 0.0, outside);
        }
        if x_end < body.right() {
            painter.rect_filled(Rect::from_x_y_ranges(x_span(x_end, body.right()), body.y_range()), 0.0, outside);
        }

        // Ruler.
        let (major, minor) = tick_steps(view.pps, fps);
        let t_from = view.t(body.left()).max(0.0);
        let t_to = view.t(body.right()).min(sequence.end());
        let ticks_bottom = layout.strip.top();
        for t in ticks(t_from, t_to, minor) {
            let x = view.x(t);
            painter.line_segment([pos2(x, ticks_bottom - 5.0), pos2(x, ticks_bottom)], Stroke::new(1.0, tick_color));
        }
        for t in ticks(t_from, t_to, major) {
            let x = view.x(t);
            painter.line_segment([pos2(x, body.top() + 14.0), pos2(x, ticks_bottom)], Stroke::new(1.0, tick_color));
            let label = if major < 1.0 { timecode::format(t, fps) } else { timecode::format_minutes(t) };
            painter.text(pos2(x + 3.0, body.top() + 2.0), Align2::LEFT_TOP, label, FontId::monospace(10.0), tick_color);
        }

        // Clips. The selected one shows where its source would go untrimmed, a frame and handles.
        for (id, clip) in sequence.clips() {
            let block = layout.block(id.track, clip.span(), view);
            let is_selected = selected == Some(id);
            if is_selected {
                let ghost = layout.block(id.track, clip.source_span(), view);
                painter.rect_filled(ghost, 4.0, CLIP_COLOR.gamma_multiply(0.25));
                painter.rect_stroke(ghost, 4.0, Stroke::new(1.0, CLIP_COLOR.gamma_multiply(0.7)));
            }
            painter.rect_filled(block, 4.0, CLIP_COLOR);
            let text = format!("{}  ·  {}", clip.name, timecode::format(clip.span().len(), fps));
            let text_x = block.left().max(body.left()) + if is_selected { HANDLE_WIDTH + 6.0 } else { 6.0 };
            painter.with_clip_rect(block.shrink(2.0)).text(
                pos2(text_x, block.center().y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(12.0),
                Color32::from_white_alpha(220),
            );
            if is_selected {
                painter.rect_stroke(block, 4.0, Stroke::new(2.0, HANDLE_COLOR));
                let left = Rect::from_x_y_ranges(block.left()..=block.left() + HANDLE_WIDTH, block.y_range());
                let right = Rect::from_x_y_ranges(block.right() - HANDLE_WIDTH..=block.right(), block.y_range());
                let left_rounding = Rounding { nw: 4.0, sw: 4.0, ..Rounding::ZERO };
                let right_rounding = Rounding { ne: 4.0, se: 4.0, ..Rounding::ZERO };
                for (handle, rounding, drag) in [(left, left_rounding, Drag::ClipStart(id)), (right, right_rounding, Drag::ClipEnd(id))] {
                    let color = if active == Some(drag) { Color32::WHITE } else { HANDLE_COLOR };
                    painter.rect_filled(handle, rounding, color);
                    grip(&painter, handle.center(), 8.0);
                }
            }
        }

        // Everything on the timeline outside the export range is greyed out.
        let export = sequence.export;
        let (x_in, x_out) = (view.x(export.start), view.x(export.end));
        if x_in > x_start {
            painter.rect_filled(Rect::from_x_y_ranges(x_span(x_start, x_in), body.y_range()), 0.0, OUTSIDE_EXPORT_TINT);
        }
        if x_out < x_end {
            painter.rect_filled(Rect::from_x_y_ranges(x_span(x_out, x_end), body.y_range()), 0.0, OUTSIDE_EXPORT_TINT);
        }

        // The export range: a strip with its length, and an anchor at each edge.
        let strip = Rect::from_x_y_ranges(x_in..=x_out, layout.strip.y_range());
        let moving = active == Some(Drag::ExportMove);
        painter.rect_filled(strip, 0.0, EXPORT_COLOR.gamma_multiply(if moving { 1.0 } else { 0.75 }));
        if strip.width() > 90.0 {
            let length = timecode::format(export.len(), fps);
            let at = strip.intersect(body).center();
            painter.text(at, Align2::CENTER_CENTER, length, FontId::monospace(10.0), Color32::WHITE);
        }
        // Below the tick labels.
        let tabs = layout.ruler.top() + 14.0..=layout.ruler.bottom();
        let start_tab = Rect::from_x_y_ranges(x_in..=x_in + ANCHOR_WIDTH, tabs.clone());
        let end_tab = Rect::from_x_y_ranges(x_out - ANCHOR_WIDTH..=x_out, tabs);
        for (x, tab, drag) in [(x_in, start_tab, Drag::ExportStart), (x_out, end_tab, Drag::ExportEnd)] {
            let color = if active == Some(drag) { Color32::WHITE } else { EXPORT_COLOR };
            painter.line_segment(across(x), Stroke::new(2.0, color));
            painter.rect_filled(tab, 2.0, color);
            grip(&painter, tab.center(), 10.0);
        }

        if let Some(t) = self.snapped {
            painter.line_segment(across(view.x(t)), Stroke::new(1.0, Color32::from_white_alpha(160)));
        }

        // Playhead: fixed in the center, with an arrow and the current time above the bar.
        let center_x = view.center_x;
        painter.line_segment(across(center_x), Stroke::new(2.0, PLAYHEAD_COLOR));
        let arrow = vec![
            pos2(center_x - 7.0, body.top() - 9.0),
            pos2(center_x + 7.0, body.top() - 9.0),
            pos2(center_x, body.top()),
        ];
        painter.add(Shape::convex_polygon(arrow, PLAYHEAD_COLOR, Stroke::NONE));
        painter.text(
            pos2(center_x, layout.rect.top()),
            Align2::CENTER_TOP,
            timecode::format(view.pos, fps),
            FontId::monospace(13.0),
            visuals.strong_text_color(),
        );
    }
}

/// What a press at `p` grabs, if not the bar itself.
fn hit(p: Pos2, layout: &Layout, view: View, sequence: &Sequence, selected: Option<ClipId>) -> Option<Drag> {
    if layout.ruler.contains(p) {
        let export = sequence.export;
        let (x_in, x_out) = (view.x(export.start), view.x(export.end));
        return match nearer_edge(p.x, x_in, x_out, ANCHOR_WIDTH) {
            Some(true) => Some(Drag::ExportStart),
            Some(false) => Some(Drag::ExportEnd),
            None if layout.strip.contains(p) && p.x > x_in && p.x < x_out => Some(Drag::ExportMove),
            None => None,
        };
    }
    let id = selected?;
    let block = layout.block(id.track, sequence.clip(id).span(), view);
    if !block.y_range().contains(p.y) {
        return None;
    }
    nearer_edge(p.x, block.left(), block.right(), HANDLE_WIDTH).map(|start| if start { Drag::ClipStart(id) } else { Drag::ClipEnd(id) })
}

/// Whether `x` grabs the start (true) or the end (false) of a span drawn from `start` to `end`
/// with handles `width` wide just inside its edges.
fn nearer_edge(x: f32, start: f32, end: f32, width: f32) -> Option<bool> {
    let reach = width / 2.0 + GRAB_MARGIN;
    if end - start >= 2.0 * width {
        let (to_start, to_end) = ((x - start - width / 2.0).abs(), (x - end + width / 2.0).abs());
        return (to_start.min(to_end) <= reach).then_some(to_start <= to_end);
    }
    // Too narrow for two handles: the side of the middle decides, so that both edges can be
    // pulled apart.
    let middle = (start + end) / 2.0;
    ((x - middle).abs() <= reach + (end - start) / 2.0).then_some(x < middle)
}

/// The time a drag moves: the dragged edge, or the export start when moving the range.
fn edge_time(sequence: &Sequence, drag: Drag) -> Option<f64> {
    match drag {
        Drag::Scrub { .. } => None,
        Drag::ExportStart | Drag::ExportMove => Some(sequence.export.start),
        Drag::ExportEnd => Some(sequence.export.end),
        Drag::ClipStart(id) => Some(sequence.clip(id).start),
        Drag::ClipEnd(id) => Some(sequence.clip(id).end()),
    }
}

/// Moves an edge (or the export start, when moving the range) to `t`. Returns the snap target used.
fn edit(sequence: &mut Sequence, drag: Drag, t: f64, playhead: f64, snap_range: f64, frame: f64) -> Option<f64> {
    let targets = snap_targets(sequence, drag, playhead);
    // Snapped to a target, or else to the nearest frame.
    let place = |t: f64| match snap(t, &targets, snap_range) {
        Some(target) => (target, Some(target)),
        None => ((t / frame).round() * frame, None),
    };
    let bounds = sequence.export_bounds(frame);
    match drag {
        Drag::Scrub { .. } => None,
        Drag::ExportStart => {
            let (t, snapped) = place(t);
            sequence.export.resize_start(t, bounds);
            snapped
        }
        Drag::ExportEnd => {
            let (t, snapped) = place(t);
            sequence.export.resize_end(t, bounds);
            snapped
        }
        Drag::ExportMove => {
            // Either edge may snap; the nearer target wins.
            let len = sequence.export.len();
            let start = t;
            let by_start = snap(start, &targets, snap_range).map(|s| (s, s - start));
            let by_end = snap(start + len, &targets, snap_range).map(|e| (e, e - len - start));
            let best = [by_start, by_end].into_iter().flatten().min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
            let (start, snapped) = match best {
                Some((target, shift)) => (start + shift, Some(target)),
                None => ((start / frame).round() * frame, None),
            };
            sequence.export.move_to(start, bounds);
            snapped
        }
        Drag::ClipStart(id) => {
            let (t, snapped) = place(t);
            sequence.trim_start(id, t, frame);
            snapped
        }
        Drag::ClipEnd(id) => {
            let (t, snapped) = place(t);
            sequence.trim_end(id, t, frame);
            snapped
        }
    }
}

/// Times a dragged edge sticks to: the timeline start, the playhead, clip edges and the export
/// range, except the edge being dragged.
fn snap_targets(sequence: &Sequence, drag: Drag, playhead: f64) -> Vec<f64> {
    let mut targets = vec![0.0, playhead];
    for (id, clip) in sequence.clips() {
        if drag != Drag::ClipStart(id) {
            targets.push(clip.start);
        }
        if drag != Drag::ClipEnd(id) {
            targets.push(clip.end());
        }
    }
    let moving = drag == Drag::ExportMove;
    if !moving && drag != Drag::ExportStart {
        targets.push(sequence.export.start);
    }
    if !moving && drag != Drag::ExportEnd {
        targets.push(sequence.export.end);
    }
    targets
}

/// The target nearest to `t`, if it is within `range`.
fn snap(t: f64, targets: &[f64], range: f64) -> Option<f64> {
    targets
        .iter()
        .copied()
        .filter(|target| (target - t).abs() <= range)
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// Pixels per second to scroll while an edge pressed at `press_x` is dragged at `x`; negative to
/// the left. Only towards a side the pointer has moved to, so that grabbing an edge that is
/// already near a side does not scroll.
fn autoscroll_speed(x: f32, press_x: f32, left: f32, right: f32) -> f32 {
    let depth = if x < left + AUTOSCROLL_ZONE && x < press_x {
        x - left - AUTOSCROLL_ZONE
    } else if x > right - AUTOSCROLL_ZONE && x > press_x {
        x - right + AUTOSCROLL_ZONE
    } else {
        0.0
    };
    (depth / AUTOSCROLL_ZONE).clamp(-1.0, 1.0) * AUTOSCROLL_SPEED
}

/// Two short vertical lines, the "grab here" mark on handles.
fn grip(painter: &egui::Painter, center: Pos2, height: f32) {
    let stroke = Stroke::new(1.0, Color32::from_black_alpha(150));
    for dx in [-1.5, 1.5] {
        let x = center.x + dx;
        painter.line_segment([pos2(x, center.y - height / 2.0), pos2(x, center.y + height / 2.0)], stroke);
    }
}

/// Labelled and unlabelled tick steps, in seconds, for the current zoom.
fn tick_steps(pps: f32, fps: f64) -> (f64, f64) {
    let frame = 1.0 / fps;
    let mut steps = vec![frame, 2.0 * frame, 5.0 * frame, 10.0 * frame, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0];
    steps.extend([60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0]);
    steps.retain(|&s| s >= frame * 0.999);
    steps.sort_by(f64::total_cmp);
    let px = |s: f64| s * pps as f64;
    let major = steps.iter().copied().find(|&s| px(s) >= MAJOR_TICK_SPACING).unwrap_or(3600.0);
    let minor = [major / 10.0, major / 5.0, major / 2.0]
        .into_iter()
        .find(|&s| px(s) >= 8.0 && s >= frame * 0.999)
        .unwrap_or(major);
    (major, minor)
}

fn ticks(from: f64, to: f64, step: f64) -> impl Iterator<Item = f64> {
    let first = (from / step).ceil() as i64;
    let last = (to / step).floor() as i64;
    (first..=last).take(2000).map(move |k| k as f64 * step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Clip;

    const CLIP: ClipId = ClipId { track: 0, index: 0 };

    #[test]
    fn tick_steps_follow_the_zoom() {
        // 10 px per second: labels every 10 s.
        assert_eq!(tick_steps(10.0, 30.0).0, 10.0);
        // 100 px per second: labels every second, minor ticks every 0.1 s.
        assert_eq!(tick_steps(100.0, 30.0), (1.0, 0.1));
        // 1800 px per second (60 px per frame): labels every 2 frames, minor ticks every frame.
        let (major, minor) = tick_steps(1800.0, 30.0);
        assert!((major - 2.0 / 30.0).abs() < 1e-12 && (minor - 1.0 / 30.0).abs() < 1e-12);
    }

    #[test]
    fn grabs_the_nearer_edge() {
        // Handles 12 px wide inside 100..300: centers at 106 and 294, grabbable 12 px around them.
        assert_eq!(nearer_edge(95.0, 100.0, 300.0, 12.0), Some(true));
        assert_eq!(nearer_edge(117.0, 100.0, 300.0, 12.0), Some(true));
        assert_eq!(nearer_edge(150.0, 100.0, 300.0, 12.0), None);
        assert_eq!(nearer_edge(305.0, 100.0, 300.0, 12.0), Some(false));
        // A span narrower than two handles: the left half pulls the start, the right half the end.
        assert_eq!(nearer_edge(99.0, 100.0, 104.0, 12.0), Some(true));
        assert_eq!(nearer_edge(103.0, 100.0, 104.0, 12.0), Some(false));
        assert_eq!(nearer_edge(130.0, 100.0, 104.0, 12.0), None);
    }

    #[test]
    fn snaps_to_the_nearest_target_in_range() {
        let targets = [0.0, 10.0, 10.5];
        assert_eq!(snap(10.3, &targets, 0.25), Some(10.5));
        assert_eq!(snap(10.2, &targets, 0.25), Some(10.0));
        assert_eq!(snap(5.0, &targets, 0.25), None);
        assert_eq!(snap(10.2, &targets, 0.0), None);
    }

    #[test]
    fn dragged_edges_snap_or_land_on_frames() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        let frame = 0.04;
        // Near the playhead at 12.03: sticks to it.
        assert_eq!(edit(&mut sequence, Drag::ExportStart, 12.1, 12.03, 0.1, frame), Some(12.03));
        assert_eq!(sequence.export.start, 12.03);
        // Nothing near: the nearest frame.
        assert_eq!(edit(&mut sequence, Drag::ClipEnd(CLIP), 20.33, 5.0, 0.1, frame), None);
        assert!((sequence.clip(CLIP).end() - 20.32).abs() < 1e-9);
        // The export end sticks to the trimmed clip end.
        assert_eq!(edit(&mut sequence, Drag::ExportEnd, 20.4, 5.0, 0.1, frame), Some(sequence.clip(CLIP).end()));
        // Moving the range keeps its length; here its start sticks to the timeline start.
        let len = sequence.export.len();
        assert_eq!(edit(&mut sequence, Drag::ExportMove, 0.05, 30.0, 0.1, frame), Some(0.0));
        assert_eq!(sequence.export.start, 0.0);
        assert!((sequence.export.len() - len).abs() < 1e-9);
    }

    #[test]
    fn autoscrolls_only_near_the_sides() {
        assert_eq!(autoscroll_speed(500.0, 400.0, 0.0, 1000.0), 0.0);
        assert_eq!(autoscroll_speed(0.0, 400.0, 0.0, 1000.0), -AUTOSCROLL_SPEED);
        assert_eq!(autoscroll_speed(1000.0 - AUTOSCROLL_ZONE / 2.0, 400.0, 0.0, 1000.0), AUTOSCROLL_SPEED / 2.0);
        assert_eq!(autoscroll_speed(1200.0, 400.0, 0.0, 1000.0), AUTOSCROLL_SPEED);
        // Grabbed near the right side and pulled away from it.
        assert_eq!(autoscroll_speed(980.0, 995.0, 0.0, 1000.0), 0.0);
    }
}

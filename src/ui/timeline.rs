//! The timeline: a ruler and tracks of clips that slide under a fixed playhead in the center.
//! The ruler counts from the export start, so that it shows the time of the resulting file.
//!
//! One axis with things on it, and the cursor tells what a drag will do. The tape moves when dragged
//! by the ruler or an empty place, a clip when dragged by its body. The edges of the selected
//! clip trim it, the anchors on the ruler move the edges of the export range, the strip between
//! them moves all of it. The wheel zooms.

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
/// The tape goes no further than this much of what is on it stays on the screen.
const KEEP_IN_SIGHT: f32 = 40.0;
/// A gap in the export range gets its label when it is this wide.
const GAP_LABEL_WIDTH: f32 = 64.0;

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
    /// The dragged edge has stopped at a limit and no longer follows the pointer. The tape waits
    /// for it then, instead of carrying it away from the pointer.
    stuck: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Scrub { start_time: f64, start_x: f32 },
    ExportStart,
    ExportEnd,
    ExportMove,
    ClipStart(ClipId),
    ClipEnd(ClipId),
    ClipMove(ClipId),
}

impl Drag {
    /// The clip this drag is about.
    fn clip(self) -> Option<ClipId> {
        match self {
            Drag::ClipStart(id) | Drag::ClipEnd(id) | Drag::ClipMove(id) => Some(id),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct TimelineResponse {
    /// The user moved the playhead.
    pub seeked: bool,
    /// The user grabbed an edge, a clip or the export range.
    pub edit_started: bool,
    /// The user pressed on this clip: it becomes the selected one.
    pub selected: Option<ClipId>,
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
        Self { pps: scale, drag: None, fling: 0.0, grab: 0.0, press_x: 0.0, snapped: None, stuck: false }
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
        let frame = 1.0 / fps;
        let width = ui.available_width();
        let height = LABEL_HEIGHT + RULER_HEIGHT + TRACK_HEIGHT * sequence.tracks.len().max(1) as f32;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let layout = Layout::new(rect);
        let mut out = TimelineResponse::default();

        // From "half the bar holds everything there is" to "a frame is 60 px wide".
        let max_pps = (fps * 60.0) as f32;
        let min_pps = (width * 0.5 / sequence.reach().len().max(0.1) as f32).min(max_pps);
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
                out.selected = drag.clip();
                self.press_x = p.x;
                self.stuck = false;
                self.drag = Some(drag);
            }
        }
        if let (true, Some(drag), Some(p)) = (response.dragged(), self.drag, response.interact_pointer_pos()) {
            if let Drag::Scrub { start_time, start_x } = drag {
                pos = start_time - ((p.x - start_x) / pps) as f64;
                out.seeked = true;
            } else {
                let speed = if self.stuck { 0.0 } else { autoscroll_speed(p.x, self.press_x, rect.left(), rect.right()) };
                if speed != 0.0 {
                    pos += (speed * ui.input(|i| i.stable_dt).min(0.1) / pps) as f64;
                    out.seeked = true;
                    ui.ctx().request_repaint();
                }
                let snap_range = if ui.input(|i| i.modifiers.alt) { 0.0 } else { (SNAP_DISTANCE / pps) as f64 };
                let wanted = view(pos).t(p.x) + self.grab;
                self.snapped = edit(sequence, drag, wanted, pos, snap_range, frame);
                self.stuck = edge_time(sequence, drag).is_some_and(|edge| (edge - wanted).abs() > snap_range + frame);
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
                // A click on a clip only selects it; anywhere else it goes to that time.
                match hit(p, &layout, view(pos), sequence, selected).and_then(Drag::clip) {
                    Some(id) => out.selected = Some(id),
                    None => {
                        pos = view(pos).t(p.x);
                        out.seeked = true;
                    }
                }
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
        let clamped = keep_in_sight(pos, sequence.reach(), width, pps);
        if clamped != pos {
            self.fling = 0.0;
        }
        pos = clamped;
        let view = view(pos);

        let hovered = response.hover_pos().and_then(|p| hit(p, &layout, view, sequence, selected));
        if response.hover_pos().is_some() {
            ui.ctx().set_cursor_icon(cursor(self.drag.or(hovered)));
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

        // Ruler: it counts from the export start, and travels with it.
        let export = sequence.export;
        let zero = export.start;
        let (major, minor) = tick_steps(view.pps, fps);
        let t_from = view.t(body.left()) - zero;
        let t_to = view.t(body.right()) - zero;
        let ticks_bottom = layout.strip.top();
        for t in ticks(t_from, t_to, minor) {
            let x = view.x(zero + t);
            painter.line_segment([pos2(x, ticks_bottom - 5.0), pos2(x, ticks_bottom)], Stroke::new(1.0, tick_color));
        }
        for t in ticks(t_from, t_to, major) {
            let x = view.x(zero + t);
            painter.line_segment([pos2(x, body.top() + 14.0), pos2(x, ticks_bottom)], Stroke::new(1.0, tick_color));
            let label = if major < 1.0 { timecode::format(t) } else { timecode::format_minutes(t) };
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
            let text = format!("{}  ·  {}", clip.name, timecode::format(clip.span().len()));
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

        // Where the export range has no clip, the file will be black: the bottom row says so.
        for gap in sequence.gaps() {
            let (from, to) = (view.x(gap.start).max(body.left()), view.x(gap.end).min(body.right()));
            if to - from >= GAP_LABEL_WIDTH {
                let at = pos2((from + to) / 2.0, layout.row(0).center().y);
                painter.text(at, Align2::CENTER_CENTER, "black", FontId::proportional(12.0), tick_color);
            }
        }

        // Everything outside the export range is greyed out.
        let (x_in, x_out) = (view.x(export.start), view.x(export.end));
        if x_in > body.left() {
            painter.rect_filled(Rect::from_x_y_ranges(x_span(body.left(), x_in), body.y_range()), 0.0, OUTSIDE_EXPORT_TINT);
        }
        if x_out < body.right() {
            painter.rect_filled(Rect::from_x_y_ranges(x_span(x_out, body.right()), body.y_range()), 0.0, OUTSIDE_EXPORT_TINT);
        }

        // The export range: a strip with its length, and an anchor at each edge.
        let strip = Rect::from_x_y_ranges(x_in..=x_out, layout.strip.y_range());
        let moving = active == Some(Drag::ExportMove);
        painter.rect_filled(strip, 0.0, EXPORT_COLOR.gamma_multiply(if moving { 1.0 } else { 0.75 }));
        if strip.width() > 90.0 {
            let length = timecode::format(export.len());
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
        let in_file = export.contains(view.pos);
        let time = painter.text(
            pos2(center_x, layout.rect.top()),
            Align2::CENTER_TOP,
            timecode::format(view.pos - zero),
            FontId::monospace(13.0),
            if in_file { visuals.strong_text_color() } else { tick_color },
        );
        if !in_file {
            let at = pos2(time.right() + 8.0, time.center().y);
            painter.text(at, Align2::LEFT_CENTER, "outside the export", FontId::proportional(11.0), tick_color);
        }
    }
}

/// What a press at `p` grabs, if not the tape itself.
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
    // Edges only on the selected clip: the others show no handles to grab.
    if let Some(id) = selected {
        let block = layout.block(id.track, sequence.clip(id).span(), view);
        if block.y_range().contains(p.y) {
            if let Some(start) = nearer_edge(p.x, block.left(), block.right(), HANDLE_WIDTH) {
                return Some(if start { Drag::ClipStart(id) } else { Drag::ClipEnd(id) });
            }
        }
    }
    sequence.clips().find(|&(id, clip)| layout.block(id.track, clip.span(), view).contains(p)).map(|(id, _)| Drag::ClipMove(id))
}

/// What the pointer looks like over `target`: nothing grabbed means the tape.
fn cursor(target: Option<Drag>) -> CursorIcon {
    match target {
        None => CursorIcon::Grab,
        Some(Drag::Scrub { .. }) => CursorIcon::Grabbing,
        Some(Drag::ClipMove(_) | Drag::ExportMove) => CursorIcon::Move,
        Some(Drag::ClipStart(_) | Drag::ClipEnd(_) | Drag::ExportStart | Drag::ExportEnd) => CursorIcon::ResizeHorizontal,
    }
}

/// Where the playhead may be so that something of `reach` stays on a bar `width` wide.
fn keep_in_sight(pos: f64, reach: Span, width: f32, pps: f32) -> f64 {
    let margin = ((width / 2.0 - KEEP_IN_SIGHT).max(0.0) / pps) as f64;
    pos.max(reach.start - margin).min(reach.end + margin)
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

/// The time a drag moves: the dragged edge, or the start of what is moved whole.
fn edge_time(sequence: &Sequence, drag: Drag) -> Option<f64> {
    match drag {
        Drag::Scrub { .. } => None,
        Drag::ExportStart | Drag::ExportMove => Some(sequence.export.start),
        Drag::ExportEnd => Some(sequence.export.end),
        Drag::ClipStart(id) | Drag::ClipMove(id) => Some(sequence.clip(id).start),
        Drag::ClipEnd(id) => Some(sequence.clip(id).end()),
    }
}

/// Moves an edge (or the start of what is moved whole) to `t`. Returns the snap target used.
fn edit(sequence: &mut Sequence, drag: Drag, t: f64, playhead: f64, snap_range: f64, frame: f64) -> Option<f64> {
    let targets = snap_targets(sequence, drag, playhead);
    // Snapped to a target, or else to the nearest frame.
    let place = |t: f64| match snap(t, &targets, snap_range) {
        Some(target) => (target, Some(target)),
        None => ((t / frame).round() * frame, None),
    };
    // Something moved whole may snap by either edge; the nearer target wins.
    let place_whole = |start: f64, len: f64| {
        let by_start = snap(start, &targets, snap_range).map(|s| (s, s - start));
        let by_end = snap(start + len, &targets, snap_range).map(|e| (e, e - len - start));
        let best = [by_start, by_end].into_iter().flatten().min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
        match best {
            Some((target, shift)) => (start + shift, Some(target)),
            None => ((start / frame).round() * frame, None),
        }
    };
    match drag {
        Drag::Scrub { .. } => None,
        Drag::ExportStart => {
            let (t, snapped) = place(t);
            sequence.export.resize_start(t, frame);
            snapped
        }
        Drag::ExportEnd => {
            let (t, snapped) = place(t);
            sequence.export.resize_end(t, frame);
            snapped
        }
        Drag::ExportMove => {
            let (start, snapped) = place_whole(t, sequence.export.len());
            sequence.export.move_to(start);
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
        Drag::ClipMove(id) => {
            let (start, snapped) = place_whole(t, sequence.clip(id).span().len());
            sequence.move_clip(id, start);
            snapped
        }
    }
}

/// Times a dragged edge sticks to: the playhead, clip edges and the export range, except what is
/// being dragged and what moves along with it.
fn snap_targets(sequence: &Sequence, drag: Drag, playhead: f64) -> Vec<f64> {
    let mut targets = vec![playhead];
    for (id, clip) in sequence.clips() {
        if drag != Drag::ClipStart(id) && drag != Drag::ClipMove(id) {
            targets.push(clip.start);
        }
        if drag != Drag::ClipEnd(id) && drag != Drag::ClipMove(id) {
            targets.push(clip.end());
        }
    }
    // An export range that fits the clips goes with the edge that is being trimmed.
    let fitted = sequence.export_fits();
    let moving = drag == Drag::ExportMove;
    if !moving && drag != Drag::ExportStart && !(fitted && matches!(drag, Drag::ClipStart(_))) {
        targets.push(sequence.export.start);
    }
    if !moving && drag != Drag::ExportEnd && !(fitted && matches!(drag, Drag::ClipEnd(_))) {
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
        // Moving the range keeps its length; here its start sticks to the clip start.
        let len = sequence.export.len();
        assert_eq!(edit(&mut sequence, Drag::ExportMove, 0.05, 30.0, 0.1, frame), Some(0.0));
        assert_eq!(sequence.export.start, 0.0);
        assert!((sequence.export.len() - len).abs() < 1e-9);
    }

    /// A bar 1000 px wide: 10 px per second, with 40 s at x = 500.
    fn view() -> View {
        View { center_x: 500.0, pps: 10.0, pos: 40.0 }
    }

    fn layout() -> Layout {
        // Label 26, ruler 32, then one track of 44: clips are drawn from y = 62 to y = 98.
        Layout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 102.0)))
    }

    #[test]
    fn a_press_grabs_what_the_cursor_is_over() {
        // A clip from 30 s to 50 s (x = 400..600), the export range from 20 s to 60 s (x = 300..700).
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 20.0, ..Clip::new("a".into(), 60.0) });
        sequence.export = Span { start: 20.0, end: 60.0 };
        let (layout, view) = (layout(), view());
        let at = |x: f32, y: f32, selected| hit(pos2(x, y), &layout, view, &sequence, selected);
        let (track, ruler, strip) = (80.0, 40.0, 52.0);
        let selected = Some(CLIP);

        // The clip: its body moves it, its edges trim it, if it is the selected one.
        assert!(matches!(at(500.0, track, selected), Some(Drag::ClipMove(CLIP))));
        assert!(matches!(at(405.0, track, selected), Some(Drag::ClipStart(CLIP))));
        assert!(matches!(at(595.0, track, selected), Some(Drag::ClipEnd(CLIP))));
        assert!(matches!(at(405.0, track, None), Some(Drag::ClipMove(CLIP))));
        // Next to the clip there is only the tape, as above and under it.
        assert!(at(350.0, track, selected).is_none());
        assert!(at(500.0, 60.0, selected).is_none());
        assert!(at(500.0, 100.0, selected).is_none());
        // The ruler: the anchors, the strip between them, and the tape.
        assert!(matches!(at(304.0, ruler, selected), Some(Drag::ExportStart)));
        assert!(matches!(at(696.0, ruler, selected), Some(Drag::ExportEnd)));
        assert!(matches!(at(500.0, strip, selected), Some(Drag::ExportMove)));
        assert!(at(500.0, ruler, selected).is_none());
        assert!(at(100.0, strip, selected).is_none());
    }

    #[test]
    fn a_clip_is_found_on_its_own_track() {
        // The upper track holds a clip where the lower one has a gap.
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 5.0, ..Clip::new("a".into(), 60.0) });
        sequence.tracks.push(crate::core::edit::Track { clips: vec![Clip { start: 38.0, source_out: 5.0, ..Clip::new("b".into(), 60.0) }] });
        let upper = ClipId { track: 1, index: 0 };
        let layout = Layout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 146.0)));
        let at = |x: f32, y: f32| hit(pos2(x, y), &layout, view(), &sequence, Some(CLIP));
        // Lower track: y = 106..142, x = 400..450. Upper track: y = 62..98, x = 480..530.
        assert!(matches!(at(505.0, 80.0), Some(Drag::ClipMove(id)) if id == upper));
        assert!(at(425.0, 80.0).is_none());
        assert!(matches!(at(425.0, 124.0), Some(Drag::ClipMove(CLIP))));
        assert!(matches!(at(405.0, 124.0), Some(Drag::ClipStart(CLIP))));
        assert!(at(505.0, 124.0).is_none());
        // Only the selected clip has edges.
        assert!(matches!(at(485.0, 80.0), Some(Drag::ClipMove(id)) if id == upper));
    }

    #[test]
    fn the_cursor_tells_what_a_drag_will_do() {
        let tape = Drag::Scrub { start_time: 0.0, start_x: 0.0 };
        assert_eq!(cursor(None), CursorIcon::Grab);
        assert_eq!(cursor(Some(tape)), CursorIcon::Grabbing);
        assert_eq!(cursor(Some(Drag::ClipMove(CLIP))), CursorIcon::Move);
        assert_eq!(cursor(Some(Drag::ExportMove)), CursorIcon::Move);
        for edge in [Drag::ClipStart(CLIP), Drag::ClipEnd(CLIP), Drag::ExportStart, Drag::ExportEnd] {
            assert_eq!(cursor(Some(edge)), CursorIcon::ResizeHorizontal);
        }
    }

    #[test]
    fn a_dragged_clip_goes_anywhere_and_snaps_by_either_edge() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        let frame = 0.04;
        sequence.export.end = 25.0;
        let export = sequence.export;

        // Free on both sides of the timeline's zero. The export range stays where it was.
        assert_eq!(edit(&mut sequence, Drag::ClipMove(CLIP), -7.0, 100.0, 0.1, frame), None);
        assert!((sequence.clip(CLIP).start + 7.0).abs() < 1e-9);
        assert_eq!(edit(&mut sequence, Drag::ClipMove(CLIP), 300.0, 100.0, 0.1, frame), None);
        assert!((sequence.clip(CLIP).start - 300.0).abs() < 1e-9);
        assert_eq!(sequence.export, export);

        // By its start, to the export start; by its end, to the export end.
        assert_eq!(edit(&mut sequence, Drag::ClipMove(CLIP), 0.05, 100.0, 0.1, frame), Some(0.0));
        assert_eq!(sequence.clip(CLIP).start, 0.0);
        assert_eq!(edit(&mut sequence, Drag::ClipMove(CLIP), 15.03, 100.0, 0.1, frame), Some(25.0));
        assert!((sequence.clip(CLIP).end() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn a_dragged_thing_does_not_stick_to_itself() {
        // The clip fits the export range: both edges of each are at the same times.
        let sequence = Sequence::new(Clip { start: 2.0, source_out: 8.0, ..Clip::new("a".into(), 30.0) });
        let export = Span { start: 2.0, end: 10.0 };
        assert_eq!(sequence.export, export);
        let targets = |drag| snap_targets(&sequence, drag, 100.0);
        // Moved whole, the clip has only the export range to stick to.
        assert_eq!(targets(Drag::ClipMove(CLIP)), [100.0, 2.0, 10.0]);
        // A trimmed edge takes the export edge along, and has the other edges to stick to.
        assert_eq!(targets(Drag::ClipStart(CLIP)), [100.0, 10.0, 10.0]);
        assert_eq!(targets(Drag::ClipEnd(CLIP)), [100.0, 2.0, 2.0]);
        // The same goes for the edges of the range.
        assert_eq!(targets(Drag::ExportStart), [100.0, 2.0, 10.0, 10.0]);
        assert_eq!(targets(Drag::ExportMove), [100.0, 2.0, 10.0]);
    }

    #[test]
    fn a_slow_trim_of_a_fitting_clip_does_not_jump() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        let frame = 0.04;
        let mut t = 0.0;
        while t < 1.0 {
            t += frame;
            assert_eq!(edit(&mut sequence, Drag::ClipStart(CLIP), t, 100.0, 0.2, frame), None);
            assert!((sequence.clip(CLIP).start - t).abs() < frame, "at {t}");
            assert_eq!(sequence.export.start, sequence.clip(CLIP).start);
        }
    }

    #[test]
    fn the_tape_stops_when_only_a_little_of_it_is_left_to_see() {
        // 1000 px at 10 px per second: 46 s from the center to KEEP_IN_SIGHT of a side.
        let reach = Span { start: 0.0, end: 12.0 };
        assert_eq!(keep_in_sight(5.0, reach, 1000.0, 10.0), 5.0);
        assert_eq!(keep_in_sight(-30.0, reach, 1000.0, 10.0), -30.0);
        assert_eq!(keep_in_sight(-100.0, reach, 1000.0, 10.0), -46.0);
        assert_eq!(keep_in_sight(100.0, reach, 1000.0, 10.0), 58.0);
        // A bar too narrow to keep anything aside: the playhead stays on what there is.
        assert_eq!(keep_in_sight(100.0, reach, 60.0, 10.0), 12.0);
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

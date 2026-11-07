//! The timeline: a ruler and a row for each track, with clips that slide under a fixed playhead in
//! the center. The ruler counts from the export start, so that it shows the time of the resulting
//! file.
//!
//! One axis with things on it, and the cursor tells what a drag will do. The tape moves when dragged
//! by the ruler or an empty place, a clip when dragged by its body, also into another row. The edges
//! of the selected clip trim it, the anchors on the ruler move the edges of the export range, the
//! strip between them moves all of it. Dragged with the right or the middle button, it is always
//! the tape. A click on the ruler takes the playhead there; in the rows it picks a clip, or nothing.
//! The wheel zooms; over the headers or the scroll bar left of them it scrolls the rows. Dragged with
//! the right or the middle button, or the left one on an empty place in the rows, the tape also
//! moves the rows up and down. The rows give a little past their ends and spring back: there is
//! nothing more there. A row moves by dragging its name.
//! With the Blade tool, a click on a clip cuts it there, with Shift every clip there. A right click
//! on a clip opens its menu: fades and their curves, a cut there, delete. The selected clip's fades are dragged by
//! the dots on its top edge.
//!
//! A row is a header and lanes: a lane for each thing its clips show. Folded, there is the picture;
//! unfolded, also the effects of each clip and its sound as a graph. A clip covers all the lanes of
//! its row, and is grabbed, cut and trimmed by any of them.

use crate::core::edit::{Clip, ClipId, Curve, Edge, Effect, SourceId, Sequence, Span, Track, TrackId};
use crate::media::geometry::Transform;
use crate::media::timecode;
use crate::media::waveform::Peaks;
use crate::ui::{curve_choice, grip_dots};
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Mesh, PointerButton, Pos2, Rect, RichText, Rounding, Sense, Shape, Stroke};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

const LABEL_HEIGHT: f32 = 26.0;
const RULER_HEIGHT: f32 = 32.0;
/// Bottom of the ruler, where the export range is drawn; dragging it moves the range.
const STRIP_HEIGHT: f32 = 12.0;
const PICTURE_LANE_HEIGHT: f32 = 44.0;
const EFFECTS_LANE_HEIGHT: f32 = 24.0;
const SOUND_LANE_HEIGHT: f32 = 52.0;
/// Left of the rows: their scroll bar, and the name of each track and what can be done with it.
const HEADER_WIDTH: f32 = 218.0;
/// The scroll bar and the gap after it, at the left of the headers.
const SCROLL_BAR_SPACE: f32 = SCROLL_BAR_WIDTH + 4.0;
/// How far the rows go past their ends before they stop, how much of a push they give way to
/// there, and how fast they spring back, per second.
const OVERSCROLL: f32 = 56.0;
const OVERSCROLL_GIVE: f32 = 0.35;
const SPRING: f32 = 14.0;
/// How long after the last push the rows spring back: the wheel's steps come apart.
const SPRING_DELAY: f64 = 0.12;
/// The six dots a row is dragged by, left of its name.
const ROW_GRIP_SIZE: [f32; 2] = [14.0, 16.0];
/// The triangle that folds and unfolds a row.
const FOLD_SIZE: [f32; 2] = [16.0, 16.0];
/// When the rows need more than this, they scroll.
/// The scroll bar of the rows, over their right edge, and the shortest its thumb gets.
const SCROLL_BAR_WIDTH: f32 = 10.0;
const SCROLL_THUMB_MIN: f32 = 24.0;
const CLIP_MARGIN: f32 = 4.0;
const HANDLE_WIDTH: f32 = 12.0;
const ANCHOR_WIDTH: f32 = 9.0;
/// How far outside a handle or an anchor it can still be grabbed.
const GRAB_MARGIN: f32 = 6.0;
const SNAP_DISTANCE: f32 = 8.0;
/// Dragging an edge this close to a side of the timeline scrolls it, at up to this many px/s.
const AUTOSCROLL_ZONE: f32 = 48.0;
const AUTOSCROLL_SPEED: f32 = 900.0;
/// Dragging a clip this close to the top or the bottom of the rows scrolls them, at this many px/s.
const ROW_AUTOSCROLL_ZONE: f32 = 12.0;
const ROW_AUTOSCROLL_SPEED: f32 = 240.0;
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
const MISSING_COLOR: Color32 = Color32::from_rgb(230, 80, 70);
const CUT_COLOR: Color32 = Color32::from_rgb(255, 120, 60);
/// A fade's slope over a clip, and the part of the clip it darkens.
const FADE_LINE: Color32 = Color32::from_rgb(255, 200, 40);
const FADE_SHADE: Color32 = Color32::from_black_alpha(90);
/// The handle a fade's length is dragged by, on the clip's top edge.
const FADE_HANDLE_SIZE: [f32; 2] = [18.0, 10.0];
const CHIP_COLOR: Color32 = Color32::from_rgb(96, 80, 150);
const WAVE_COLOR: Color32 = Color32::from_rgb(90, 200, 160);
/// Under the effects and the sound of a clip, darker than its picture lane.
const LANE_SHADE: Color32 = Color32::from_black_alpha(70);
/// Laid over everything outside the export range.
const OUTSIDE_EXPORT_TINT: Color32 = Color32::from_rgba_premultiplied(56, 56, 56, 110);

/// What a click on a clip does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tool {
    /// Picks it; dragging moves it, its edges trim it.
    #[default]
    Select,
    /// Cuts it in two where it was clicked.
    Blade,
}

/// A part of the Output tab that an effect's plaque leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Frame,
    Sound,
    Effects,
}

/// What the graph of a source's sound can be.
#[derive(Clone)]
pub enum SourceSound {
    /// The source has no sound.
    Silent,
    /// The graph, once it is made.
    Graph(Arc<OnceLock<Peaks>>),
}

/// Where a cut would go, under the pointer with the Blade tool.
#[derive(Clone, Copy)]
struct Cut {
    time: f64,
    track: usize,
    /// Shift is held: every clip there is cut.
    all: bool,
}

pub struct Timeline {
    pub tool: Tool,
    /// Pixels per second. Set from the bar width on first show.
    pps: Option<f32>,
    /// The tracks whose rows show their clips' effects and sound.
    expanded: BTreeSet<TrackId>,
    cut: Option<Cut>,
    /// How far the rows are scrolled down, in pixels.
    scroll: f32,
    drag: Option<Drag>,
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
    /// Where a source dragged from the list, or a clip with no room in the row under the pointer,
    /// would go.
    ghost: Option<Ghost>,
    /// A dragged clip has no room in the row under the pointer: let go, it goes on a new row right
    /// above that one, starting here.
    new_row: Option<(usize, f64)>,
    /// The track whose row is being dragged by its name.
    row_drag: Option<TrackId>,
    /// The pointer is over the ruler here: a click takes the playhead to it.
    ruler_hover: Option<f32>,
    /// Where the thumb of the rows' scroll bar was grabbed, from its top.
    thumb_grab: f32,
    /// The clip whose menu is open, and the time it was opened at: a cut from the menu goes there.
    menu: Option<(ClipId, f64)>,
    /// The rows' scroll bar as it is drawn: the bar, its thumb, and whether the pointer is on it.
    scroll_bar: Option<(Rect, Rect, bool)>,
    /// While the tape is dragged over the rows: where the drag started and how far they were
    /// scrolled then.
    pan: Option<(f32, f32)>,
    /// When the rows were last pushed: they spring back a little after.
    pushed_at: f64,
}

/// The rows' scroll after a push by `delta`: past an end they give way less, and no further than
/// `OVERSCROLL`.
fn push(scroll: f32, delta: f32, max: f32) -> f32 {
    let outward = (scroll < 0.0 && delta < 0.0) || (scroll > max && delta > 0.0);
    (scroll + if outward { delta * OVERSCROLL_GIVE } else { delta }).clamp(-OVERSCROLL, max + OVERSCROLL)
}

/// Where the rows are when the pointer would take them to `raw`: past an end, only part of the way.
fn rubber(raw: f32, max: f32) -> f32 {
    if raw < 0.0 {
        (raw * OVERSCROLL_GIVE).max(-OVERSCROLL)
    } else if raw > max {
        max + ((raw - max) * OVERSCROLL_GIVE).min(OVERSCROLL)
    } else {
        raw
    }
}

/// The rows `dt` seconds on as they spring back within `0..=max`. At the end they stop.
fn settle(scroll: f32, max: f32, dt: f32) -> f32 {
    let target = scroll.clamp(0.0, max.max(0.0));
    let next = scroll + (target - scroll) * (1.0 - (-SPRING * dt).exp());
    if (next - target).abs() < 0.5 {
        target
    } else {
        next
    }
}

/// The scroll bar of the rows: where its thumb is for a scroll, and the scroll for where the thumb is.
#[derive(Debug, Clone, Copy)]
struct RowsBar {
    track: Rect,
    thumb_height: f32,
    max_scroll: f32,
}

impl RowsBar {
    /// For rows `total` pixels tall, of which `shown` are in sight.
    fn new(track: Rect, shown: f32, total: f32) -> Self {
        let thumb_height = (track.height() * shown / total).max(SCROLL_THUMB_MIN).min(track.height());
        Self { track, thumb_height, max_scroll: (total - shown).max(0.0) }
    }

    /// How far the thumb can go.
    fn travel(&self) -> f32 {
        (self.track.height() - self.thumb_height).max(1.0)
    }

    fn thumb(&self, scroll: f32) -> Rect {
        let top = self.track.top() + scroll / self.max_scroll.max(1.0) * self.travel();
        Rect::from_x_y_ranges(self.track.x_range(), top..=top + self.thumb_height)
    }

    /// The scroll that puts the thumb's top at `top`.
    fn scroll_at(&self, top: f32) -> f32 {
        ((top - self.track.top()) / self.travel() * self.max_scroll).clamp(0.0, self.max_scroll)
    }
}

/// Where a clip let go over the rows goes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    /// Into this track, where there is room for it.
    Into(usize),
    /// On a new track right above this one, which has no room for it there.
    Above(usize),
    /// On a new track under all the others.
    Bottom,
}

/// Something dragged over the rows, drawn where it would go.
struct Ghost {
    place: Place,
    span: Span,
    name: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    Scrub { start_time: f64, start_x: f32 },
    ExportStart,
    ExportEnd,
    ExportMove,
    ClipStart(ClipId),
    ClipEnd(ClipId),
    ClipMove(ClipId),
    /// The length of a clip's fade, by its handle.
    Fade(ClipId, Edge),
}

impl Drag {
    /// The clip this drag is about.
    fn clip(self) -> Option<ClipId> {
        match self {
            Drag::ClipStart(id) | Drag::ClipEnd(id) | Drag::ClipMove(id) | Drag::Fade(id, _) => Some(id),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct TimelineResponse {
    /// The user moved the playhead.
    pub seeked: bool,
    /// The user grabbed an edge, a clip or the export range, cut a clip, or changed a track.
    pub edit_started: bool,
    /// While an edge is dragged: the time whose frame the preview should show instead of the playhead's.
    pub peek: Option<f64>,
    /// A plaque of an effect was clicked: the part of the Output tab it stands for.
    pub open: Option<Section>,
}

/// What the timeline needs to know besides the edit.
pub struct TimelineEnv<'a> {
    pub fps: f64,
    /// Sources whose file is not there: their clips are marked.
    pub missing: &'a BTreeSet<SourceId>,
    /// Sources that can be dropped on the timeline, with what their clip is called and how long it is.
    pub placeable: &'a BTreeMap<SourceId, (String, f64)>,
    /// The graph of each source's sound, for unfolded rows.
    pub sounds: &'a BTreeMap<SourceId, SourceSound>,
}

/// What a track's menu or the buttons over the rows asked for, done once the rows are drawn.
#[derive(Clone, Copy)]
enum TrackAction {
    Move { track: usize, up: bool },
    Delete(usize),
    /// An empty track at this place among the tracks.
    Add(usize),
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

/// A strip of a row for one thing its clips show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Picture,
    Effects,
    Sound,
}

impl Lane {
    fn height(self) -> f32 {
        match self {
            Lane::Picture => PICTURE_LANE_HEIGHT,
            Lane::Effects => EFFECTS_LANE_HEIGHT,
            Lane::Sound => SOUND_LANE_HEIGHT,
        }
    }
}

/// The lanes of a track's row, top to bottom: the picture, and when it is unfolded the effects and
/// the sound.
fn lanes_of(track: &Track, expanded: &BTreeSet<TrackId>) -> Vec<Lane> {
    if expanded.contains(&track.id) {
        vec![Lane::Picture, Lane::Effects, Lane::Sound]
    } else {
        vec![Lane::Picture]
    }
}

fn row_height(track: &Track, expanded: &BTreeSet<TrackId>) -> f32 {
    lanes_of(track, expanded).into_iter().map(Lane::height).sum()
}

/// A plaque for something done to a clip, in the effects lane.
#[derive(Debug, Clone, PartialEq)]
struct Chip {
    text: String,
    section: Section,
}

/// What is done to a clip, as plaques: its framing if it is not the plain one, the mirror, the bars,
/// a volume other than 100 %, mute. Effects to come join them here.
fn effects_of(clip: &Clip) -> Vec<Chip> {
    let (t, plain) = (clip.transform, Transform::default());
    let chip = |text: String, section| Chip { text, section };
    let mut chips = Vec::new();
    for effect in &clip.effects {
        match effect {
            Effect::Fade(fade) => {
                let name = if fade.edge == Edge::In { "Fade in" } else { "Fade out" };
                let mut text = format!("{name} {:.1} s", fade.seconds);
                if fade.curve != Curve::Linear {
                    text += &format!(" · {}", fade.curve.name());
                }
                chips.push(chip(text, Section::Effects));
            }
            Effect::Color(_) => chips.push(chip("Color".to_owned(), Section::Effects)),
        }
    }
    if (t.cx, t.cy, t.zoom, t.angle_deg) != (plain.cx, plain.cy, plain.zoom, plain.angle_deg) {
        let mut text = "Frame".to_owned();
        if (t.zoom - 1.0).abs() > 1e-6 {
            text += &format!(" ×{:.2}", t.zoom);
        }
        if t.angle_deg.abs() > 1e-6 {
            text += &format!(" {:.0}°", t.angle_deg);
        }
        chips.push(chip(text, Section::Frame));
    }
    if t.mirror {
        chips.push(chip("Mirror".to_owned(), Section::Frame));
    }
    if t.bars {
        chips.push(chip("Bars".to_owned(), Section::Frame));
    }
    if clip.muted {
        chips.push(chip("Muted".to_owned(), Section::Sound));
    } else if (clip.volume - 1.0).abs() > 1e-6 {
        chips.push(chip(format!("Volume {:.0} %", clip.volume * 100.0), Section::Sound));
    }
    chips
}

/// Where the plaques go in `area`, left to right from `from`, as many as fit.
fn place_chips(ui: &egui::Ui, area: Rect, from: f32, chips: Vec<Chip>) -> Vec<(Rect, Chip)> {
    let mut x = from.max(area.left()) + 4.0;
    let mut placed = Vec::new();
    for chip in chips {
        let width = ui.fonts(|fonts| fonts.layout_no_wrap(chip.text.clone(), FontId::proportional(11.0), Color32::WHITE).size().x) + 12.0;
        if x + width > area.right() - 4.0 {
            break;
        }
        placed.push((Rect::from_min_size(pos2(x, area.center().y - 8.0), vec2(width, 16.0)), chip));
        x += width + 4.0;
    }
    placed
}

/// Where a cut at `t` falls: on the playhead, a clip's edge or the export range's when one is within
/// `range`, or else on a frame.
fn cut_time(sequence: &Sequence, t: f64, playhead: f64, range: f64, frame: f64) -> f64 {
    let mut targets = vec![playhead, sequence.export.start, sequence.export.end];
    for (_, clip) in sequence.clips() {
        targets.extend([clip.start, clip.end()]);
    }
    snap(t, &targets, range).unwrap_or((t / frame).round() * frame)
}

/// Where a track's row is: its header on the left, its lanes on the timeline.
struct Row {
    track: usize,
    header: Rect,
    /// The part on the timeline, all of its lanes.
    body: Rect,
    lanes: Vec<(Lane, Rect)>,
}

impl Row {
    fn lane(&self, lane: Lane) -> Option<Rect> {
        self.lanes.iter().find(|(kind, _)| *kind == lane).map(|(_, rect)| *rect)
    }

    /// Where a clip of this row is drawn and grabbed: across its lanes.
    fn block(&self, span: Span, view: View) -> Rect {
        let top = self.lanes.first().map_or(self.body.top(), |(_, lane)| lane.top());
        let bottom = self.lanes.last().map_or(self.body.bottom(), |(_, lane)| lane.bottom());
        Rect::from_x_y_ranges(view.x(span.start)..=view.x(span.end), top + CLIP_MARGIN..=bottom - CLIP_MARGIN)
    }
}

struct Layout {
    rect: Rect,
    /// Right of the headers: the ruler and the rows' lanes.
    body: Rect,
    ruler: Rect,
    strip: Rect,
    /// Where the rows can be seen, headers included. They scroll inside it.
    rows_area: Rect,
    /// The top track first: it is the top layer of the picture.
    rows: Vec<Row>,
}

impl Layout {
    fn new(rect: Rect, sequence: &Sequence, expanded: &BTreeSet<TrackId>, scroll: f32) -> Self {
        let left = rect.left() + HEADER_WIDTH;
        let body = Rect::from_min_max(pos2(left, rect.top() + LABEL_HEIGHT), rect.max);
        let ruler = Rect::from_min_size(body.min, vec2(body.width(), RULER_HEIGHT));
        let strip = Rect::from_min_max(pos2(ruler.left(), ruler.bottom() - STRIP_HEIGHT), ruler.max);
        let rows_area = Rect::from_min_max(pos2(rect.left(), ruler.bottom()), rect.max);
        let header_left = rect.left() + SCROLL_BAR_SPACE;
        let mut y = rows_area.top() - scroll;
        let mut rows = Vec::new();
        for (track, clips) in sequence.tracks.iter().enumerate().rev() {
            let top = y;
            let mut lanes = Vec::new();
            for lane in lanes_of(clips, expanded) {
                lanes.push((lane, Rect::from_x_y_ranges(left..=rect.right(), y..=y + lane.height())));
                y += lane.height();
            }
            let header = Rect::from_x_y_ranges(header_left..=left, top..=y);
            let body = Rect::from_x_y_ranges(left..=rect.right(), top..=y);
            rows.push(Row { track, header, body, lanes });
        }
        Self { rect, body, ruler, strip, rows_area, rows }
    }

    /// How tall the rows of `sequence` are together.
    fn rows_height(sequence: &Sequence, expanded: &BTreeSet<TrackId>) -> f32 {
        sequence.tracks.iter().map(|track| row_height(track, expanded)).sum()
    }

    fn row(&self, track: usize) -> Option<&Row> {
        self.rows.iter().find(|row| row.track == track)
    }

    /// The row at the height `y`, if it is in sight there.
    fn row_at(&self, y: f32) -> Option<&Row> {
        if !self.rows_area.y_range().contains(y) {
            return None;
        }
        self.rows.iter().find(|row| row.body.y_range().contains(y))
    }

    fn block(&self, id: ClipId, span: Span, view: View) -> Rect {
        self.row(id.track).map_or(Rect::NOTHING, |row| row.block(span, view))
    }
}

impl Timeline {
    /// `scale` is the pixels per second to start with, if they are known from before.
    pub fn new(scale: Option<f32>, expanded: &[TrackId]) -> Self {
        Self {
            tool: Tool::default(),
            pps: scale,
            expanded: expanded.iter().copied().collect(),
            cut: None,
            scroll: 0.0,
            drag: None,
            grab: 0.0,
            press_x: 0.0,
            snapped: None,
            stuck: false,
            ghost: None,
            new_row: None,
            row_drag: None,
            ruler_hover: None,
            thumb_grab: 0.0,
            menu: None,
            scroll_bar: None,
            pan: None,
            pushed_at: 0.0,
        }
    }

    pub fn scale(&self) -> Option<f32> {
        self.pps
    }

    /// The tracks whose rows are unfolded.
    pub fn expanded(&self) -> impl Iterator<Item = TrackId> + '_ {
        self.expanded.iter().copied()
    }

    /// Folds or unfolds a track's row.
    fn toggle(&mut self, track: TrackId) {
        if !self.expanded.remove(&track) {
            self.expanded.insert(track);
        }
    }

    /// Draws the timeline and applies input to the playhead, the sequence and the selection.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        position: &mut f64,
        sequence: &mut Sequence,
        selected: &mut Option<ClipId>,
        env: &TimelineEnv,
    ) -> TimelineResponse {
        let fps = env.fps;
        let frame = 1.0 / fps;
        let width = ui.available_width();
        // All the height it is given: the rows take what the ruler leaves, whether they fill it or not.
        let height = ui.available_height().max(LABEL_HEIGHT + RULER_HEIGHT + PICTURE_LANE_HEIGHT);
        let shown_rows = height - LABEL_HEIGHT - RULER_HEIGHT;
        let rows_height = Layout::rows_height(sequence, &self.expanded);
        let max_scroll = (rows_height - shown_rows).max(0.0);
        self.scroll = self.scroll.clamp(-OVERSCROLL, max_scroll + OVERSCROLL);
        let now = ui.input(|i| i.time);
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let mut layout = Layout::new(rect, sequence, &self.expanded, self.scroll);
        let mut out = TimelineResponse::default();
        let body_width = layout.body.width();

        // From "half the bar holds everything there is" to "a frame is 60 px wide".
        let max_pps = (fps * 60.0) as f32;
        let min_pps = (body_width * 0.5 / sequence.reach().len().max(0.1) as f32).min(max_pps);
        let mut pps = self.pps.unwrap_or(body_width / 40.0).clamp(min_pps, max_pps);
        let mut pos = *position;
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over_headers = pointer.is_some_and(|p| layout.rows_area.contains(p) && p.x < layout.body.left());

        // Wheel (or pinch, or ctrl+wheel) zooms; horizontal scrolling scrubs. Over the headers the
        // wheel scrolls the rows instead, a little past their ends too.
        if over_headers {
            let wheel = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta).y);
            if wheel != 0.0 {
                self.scroll = push(self.scroll, -wheel, max_scroll);
                self.pushed_at = now;
                layout = Layout::new(rect, sequence, &self.expanded, self.scroll);
            }
        } else if response.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            pps = (pps * (scroll.y * 0.004).exp() * zoom).clamp(min_pps, max_pps);
            if scroll.x != 0.0 {
                pos -= (scroll.x / pps) as f64;
                out.seeked = true;
            }
        }
        // A scroll bar at the left of the headers, when the rows are taller than the room they have.
        // Its thumb is dragged; a click beside it turns a page; the wheel over it scrolls.
        self.scroll_bar = None;
        if rows_height > shown_rows {
            let track = Rect::from_x_y_ranges(rect.left()..=rect.left() + SCROLL_BAR_WIDTH, layout.rows_area.y_range());
            let bar = RowsBar::new(track, shown_rows, rows_height);
            let response = ui.interact(track, ui.id().with("rows scroll bar"), Sense::click_and_drag());
            if let (true, Some(p)) = (response.drag_started(), response.interact_pointer_pos()) {
                let thumb = bar.thumb(self.scroll);
                // By the thumb, it keeps where it was grabbed; elsewhere its middle comes to the pointer.
                self.thumb_grab = if thumb.y_range().contains(p.y) { p.y - thumb.top() } else { bar.thumb_height / 2.0 };
            }
            if let (true, Some(p)) = (response.dragged(), response.interact_pointer_pos()) {
                self.scroll = bar.scroll_at(p.y - self.thumb_grab);
                self.pushed_at = now;
            }
            if let (true, Some(p)) = (response.clicked(), response.interact_pointer_pos()) {
                let thumb = bar.thumb(self.scroll);
                let page = if p.y < thumb.top() { -shown_rows } else if p.y > thumb.bottom() { shown_rows } else { 0.0 };
                self.scroll = (self.scroll + page).clamp(0.0, bar.max_scroll);
            }
            if response.hovered() {
                let wheel = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta).y);
                if wheel != 0.0 {
                    self.scroll = push(self.scroll, -wheel, bar.max_scroll);
                    self.pushed_at = now;
                }
            }
            let hot = response.hovered() || response.dragged();
            response.on_hover_text("Scroll the tracks. The wheel over their names does it too, and a drag with the right button");
            layout = Layout::new(rect, sequence, &self.expanded, self.scroll);
            self.scroll_bar = Some((track, bar.thumb(self.scroll.clamp(0.0, bar.max_scroll)), hot));
        }

        // The rows scroll up and down only: the playhead stays where it is.
        let center_x = layout.body.center().x;
        let view = |pos: f64| View { center_x, pps, pos };
        let snap_range = if ui.input(|i| i.modifiers.alt) { 0.0 } else { (SNAP_DISTANCE / pps) as f64 };

        if response.drag_started() {
            let press = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos());
            // The right and the middle button always take the tape, whatever is under them.
            let tape = response.drag_started_by(PointerButton::Secondary) || response.drag_started_by(PointerButton::Middle);
            // The headers are not the tape: nothing to drag there.
            if let Some(p) = press.filter(|p| p.x >= layout.body.left()) {
                // The Blade cuts with a click: a drag in the rows is the tape.
                let blade = self.tool == Tool::Blade && !layout.ruler.contains(p);
                let grabbed = if tape || blade { None } else { hit(p, &layout, view(pos), sequence, *selected) };
                let drag = grabbed.unwrap_or(Drag::Scrub { start_time: pos, start_x: p.x });
                // The tape over the rows takes them along up and down.
                self.pan = (grabbed.is_none() && layout.rows_area.contains(p)).then_some((p.y, self.scroll));
                if let Some(edge) = edge_time(sequence, drag) {
                    self.grab = edge - view(pos).t(p.x);
                    out.edit_started = true;
                }
                if let Some(id) = drag.clip() {
                    *selected = Some(id);
                }
                self.press_x = p.x;
                self.stuck = false;
                self.drag = Some(drag);
            }
        }
        if let (true, Some(mut drag), Some(p)) = (response.dragged(), self.drag, response.interact_pointer_pos()) {
            if let Drag::Scrub { start_time, start_x } = drag {
                pos = start_time - ((p.x - start_x) / pps) as f64;
                out.seeked = true;
                if let Some((start_y, start_scroll)) = self.pan {
                    self.scroll = rubber(start_scroll - (p.y - start_y), max_scroll);
                    self.pushed_at = now;
                    layout = Layout::new(rect, sequence, &self.expanded, self.scroll);
                }
            } else {
                let speed = if self.stuck { 0.0 } else { autoscroll_speed(p.x, self.press_x, layout.body.left(), layout.body.right()) };
                if speed != 0.0 {
                    pos += (speed * ui.input(|i| i.stable_dt).min(0.1) / pps) as f64;
                    out.seeked = true;
                    ui.ctx().request_repaint();
                }
                let wanted = view(pos).t(p.x) + self.grab;
                // A clip dragged over another row goes there, if there is room for it. If not, it
                // will go on a new row right above that one when let go.
                self.new_row = None;
                if let Drag::ClipMove(id) = drag {
                    let row_speed = row_autoscroll_speed(p.y, layout.rows_area);
                    if row_speed != 0.0 && rows_height > shown_rows {
                        let dt = ui.input(|i| i.stable_dt).min(0.1);
                        self.scroll = (self.scroll + row_speed * dt).clamp(0.0, max_scroll);
                        layout = Layout::new(rect, sequence, &self.expanded, self.scroll);
                        ui.ctx().request_repaint();
                    }
                    let target = layout.row_at(p.y.clamp(layout.rows_area.top() + 1.0, layout.rows_area.bottom() - 1.0)).map(|row| row.track);
                    if let Some(track) = target.filter(|&track| track != id.track) {
                        match sequence.move_clip_to_track(id, track, wanted) {
                            Some(moved) => {
                                drag = Drag::ClipMove(moved);
                                self.drag = Some(drag);
                                *selected = Some(moved);
                            }
                            None => self.new_row = Some((track, (wanted / frame).round() * frame)),
                        }
                    }
                }
                self.snapped = edit(sequence, drag, wanted, pos, snap_range, frame);
                self.stuck = edge_time(sequence, drag).is_some_and(|edge| (edge - wanted).abs() > snap_range + frame);
            }
        }
        if response.drag_stopped() {
            // A clip let go where there was no room: on a new row right above that one.
            if let (Some(Drag::ClipMove(id)), Some((above, start))) = (self.drag, self.new_row.take()) {
                let track = sequence.add_track(above + 1);
                let id = ClipId { track: after_insert(id.track, track), ..id };
                *selected = sequence.move_clip_to_track(id, track, start).or(Some(id));
            }
            self.drag = None;
            self.snapped = None;
            self.pan = None;
        }
        // Past an end and let go: back to it, a little after the last push.
        let held = self.pan.is_some() || now - self.pushed_at < SPRING_DELAY;
        if !held && !(0.0..=max_scroll).contains(&self.scroll) {
            self.scroll = settle(self.scroll, max_scroll, ui.input(|i| i.stable_dt).min(0.1));
            layout = Layout::new(rect, sequence, &self.expanded, self.scroll);
            ui.ctx().request_repaint();
        } else if held && !(0.0..=max_scroll).contains(&self.scroll) {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SPRING_DELAY));
        }
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos().filter(|p| p.x >= layout.body.left()) {
                if layout.ruler.contains(p) {
                    // The ruler is where the playhead goes: straight to the time clicked.
                    pos = view(pos).t(p.x);
                    out.seeked = true;
                } else if layout.rows_area.contains(p) {
                    let clicked = hit(p, &layout, view(pos), sequence, *selected).and_then(Drag::clip);
                    match (self.tool, clicked) {
                        // The Blade cuts the clip there, with Shift every clip there. The part after
                        // the cut is what is picked: it is what gets moved or removed next.
                        (Tool::Blade, Some(id)) => {
                            let t = cut_time(sequence, view(pos).t(p.x), pos, snap_range, frame);
                            if ui.input(|i| i.modifiers.shift) {
                                *selected = sequence.split_all(t, frame).into_iter().find(|part| part.track == id.track);
                            } else if let Some(part) = sequence.split(id, t, frame) {
                                *selected = Some(part);
                            }
                            out.edit_started = true;
                        }
                        // A click picks the clip under it, or nothing; the playhead stays. A plaque
                        // of an effect also opens what it stands for.
                        (_, clicked) => {
                            *selected = clicked;
                            out.open = clicked.and_then(|id| self.chip_at(ui, &layout, view(pos), sequence, id, p)).map(|chip| chip.section);
                        }
                    }
                }
            }
        }
        // A right click on a clip opens its menu.
        if response.secondary_clicked() {
            let clicked = response.interact_pointer_pos().filter(|p| layout.rows_area.contains(*p) && p.x >= layout.body.left());
            self.menu = clicked.and_then(|p| Some((hit(p, &layout, view(pos), sequence, *selected).and_then(Drag::clip)?, view(pos).t(p.x))));
            if let Some((id, _)) = self.menu {
                *selected = Some(id);
            }
        }
        if let Some((id, at)) = self.menu.filter(|(id, _)| sequence.has(*id)) {
            let mut changed = false;
            // An effect added here is set up in the Clip tab: it is shown there.
            let mut added = false;
            // In this egui it gives something back only as the menu closes: whether it is still
            // open is asked separately.
            response.context_menu(|ui| {
                let clip = sequence.clip_mut(id);
                for (edge, name) in [(Edge::In, "fade in"), (Edge::Out, "fade out")] {
                    let present = clip.fade(edge).is_some();
                    let label = if present { format!("Remove {name}") } else { format!("Add {name}") };
                    if ui.button(label).clicked() {
                        if present {
                            clip.remove_fade(edge);
                        } else {
                            added = clip.add_fade(edge);
                        }
                        changed = true;
                        ui.close_menu();
                    }
                }
                let colored = clip.color().is_some();
                if ui.button(if colored { "Remove color" } else { "Add color" }).on_hover_text("Exposure, contrast, saturation and temperature of this clip").clicked() {
                    if colored {
                        clip.remove_color();
                    } else {
                        added = clip.add_color();
                    }
                    changed = true;
                    ui.close_menu();
                }
                // How each fade the clip has goes.
                for (edge, name) in [(Edge::In, "Fade in curve"), (Edge::Out, "Fade out curve")] {
                    let Some(now) = clip.fade(edge).map(|fade| fade.curve) else { continue };
                    ui.menu_button(name, |ui| {
                        for curve in Curve::ALL {
                            if curve_choice(ui, curve, edge, curve == now).clicked() {
                                clip.set_fade_curve(edge, curve);
                                changed = true;
                                ui.close_menu();
                            }
                        }
                    });
                }
                ui.separator();
                if ui.button("Split here").on_hover_text("Cut the clip where the menu was opened").clicked() {
                    let t = cut_time(sequence, at, pos, snap_range, frame);
                    if let Some(part) = sequence.split(id, t, frame) {
                        *selected = Some(part);
                    }
                    changed = true;
                    ui.close_menu();
                }
                if ui.button("Duplicate").on_hover_text("A copy right after it (Ctrl+D)").clicked() {
                    *selected = Some(sequence.duplicate(id));
                    changed = true;
                    ui.close_menu();
                }
                if ui.button("Delete").clicked() {
                    sequence.remove_clip(id);
                    *selected = None;
                    changed = true;
                    ui.close_menu();
                }
                let ripple = ui.button("Delete and close the gap").on_hover_text("The clips after it on this track move back by its length (Shift+Delete)");
                if ripple.clicked() {
                    sequence.ripple_delete(id);
                    *selected = None;
                    changed = true;
                    ui.close_menu();
                }
            });
            if !response.context_menu_opened() {
                self.menu = None;
            }
            out.edit_started |= changed;
            if added {
                out.open = Some(Section::Effects);
            }
        }
        // A double click on a clip folds or unfolds its row.
        if response.double_clicked() && self.tool == Tool::Select {
            let clicked = response.interact_pointer_pos().filter(|p| layout.rows_area.contains(*p) && p.x >= layout.body.left());
            if let Some(id) = clicked.and_then(|p| hit(p, &layout, view(pos), sequence, *selected)).and_then(Drag::clip) {
                self.toggle(sequence.tracks[id.track].id);
                out.edit_started = true;
            }
        }
        pos = keep_in_sight(pos, sequence.reach(), body_width, pps);
        let view = view(pos);
        self.ruler_hover = pointer.filter(|p| self.drag.is_none() && layout.ruler.contains(*p)).map(|p| p.x);

        let hover = response.hover_pos().filter(|p| p.x >= layout.body.left());
        let hovered = hover.and_then(|p| hit(p, &layout, view, sequence, *selected));
        // With the Blade, a line shows where a click would cut.
        self.cut = None;
        if let (Tool::Blade, Some(p), None) = (self.tool, hover, self.drag) {
            if let Some(id) = hovered.and_then(Drag::clip).filter(|_| layout.rows_area.contains(p)) {
                let time = cut_time(sequence, view.t(p.x), pos, snap_range, frame);
                self.cut = Some(Cut { time, track: id.track, all: ui.input(|i| i.modifiers.shift) });
            }
        }
        let on_chip = match (self.tool, hover, hovered.and_then(Drag::clip)) {
            (Tool::Select, Some(p), Some(id)) => self.chip_at(ui, &layout, view, sequence, id, p).is_some(),
            _ => false,
        };
        if hover.is_some() {
            let icon = match (self.cut, on_chip) {
                (Some(_), _) => CursorIcon::Crosshair,
                (None, true) if self.drag.is_none() => CursorIcon::PointingHand,
                _ => cursor(self.drag.or(hovered)),
            };
            ui.ctx().set_cursor_icon(icon);
        }

        // A source dragged from the list: where it would go, and there it goes when let go.
        let dragged_source = |source: &SourceId| Some((*source, env.placeable.get(source)?));
        self.ghost = response.dnd_hover_payload::<SourceId>().and_then(|source| dragged_source(&source)).and_then(|(_, (name, len))| {
            // While a source is dragged, the timeline is not the hovered widget: the pointer says where it is.
            let (place, start) = drop_place(&layout, view, sequence, pointer.filter(|p| rect.contains(*p))?, *len, frame);
            Some(Ghost { place, span: Span { start, end: start + len }, name: name.clone() })
        });
        if let Some((source, (name, len))) = response.dnd_release_payload::<SourceId>().and_then(|source| dragged_source(&source)) {
            if let Some(p) = pointer {
                let (place, start) = drop_place(&layout, view, sequence, p, *len, frame);
                let clip = Clip { start, ..Clip::of(source, name.clone(), *len) };
                if let Some(id) = put(sequence, place, clip) {
                    *selected = Some(id);
                    out.edit_started = true;
                }
            }
            self.ghost = None;
        }
        if let (Some(Drag::ClipMove(id)), Some((above, start))) = (self.drag, self.new_row) {
            let clip = sequence.clip(id);
            self.ghost = Some(Ghost { place: Place::Above(above), span: Span { start, end: start + clip.span().len() }, name: clip.name.clone() });
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
        self.paint(ui, &layout, view, sequence, *selected, self.drag.or(hovered), env);
        if self.headers(ui, &layout, sequence, selected) {
            out.edit_started = true;
            self.drag = None;
        }
        out
    }

    /// The plaque of an effect of clip `id` under `p`, if there is one there.
    fn chip_at(&self, ui: &egui::Ui, layout: &Layout, view: View, sequence: &Sequence, id: ClipId, p: Pos2) -> Option<Chip> {
        let row = layout.row(id.track)?;
        let clip = sequence.clip(id);
        let lane = row.lane(Lane::Effects)?;
        let area = Rect::from_x_y_ranges(row.block(clip.span(), view).x_range(), lane.y_range());
        let from = area.left().max(layout.body.left());
        place_chips(ui, area, from, effects_of(clip)).into_iter().find(|(rect, _)| rect.contains(p)).map(|(_, chip)| chip)
    }

    /// The buttons over the headers and the header of each row: its name, Hide, Mute and a menu.
    /// Returns whether a track changed.
    fn headers(&mut self, ui: &mut egui::Ui, layout: &Layout, sequence: &mut Sequence, selected: &mut Option<ClipId>) -> bool {
        let count = sequence.tracks.len();
        let mut changed = false;
        let mut action = None;

        // Over the headers: a new empty track under all the others, above or under the selected clip's.
        let area = Rect::from_x_y_ranges(layout.rect.left() + 8.0..=layout.body.left() - 8.0, layout.ruler.y_range());
        let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::left_to_right(egui::Align::Center)));
        buttons.spacing_mut().item_spacing.x = 4.0;
        if add_track_button(&mut buttons, None, true).on_hover_text("Add an empty track under all the others").clicked() {
            action = Some(TrackAction::Add(0));
        }
        let near = selected.map(|id| id.track);
        let above = add_track_button(&mut buttons, Some(true), near.is_some());
        if above.on_hover_text("Add an empty track above the selected clip's track").on_disabled_hover_text("Select a clip first").clicked() {
            action = near.map(|track| TrackAction::Add(track + 1));
        }
        let below = add_track_button(&mut buttons, Some(false), near.is_some());
        if below.on_hover_text("Add an empty track under the selected clip's track").on_disabled_hover_text("Select a clip first").clicked() {
            action = near.map(TrackAction::Add);
        }

        let mut fold = None;
        for row in &layout.rows {
            let visible = row.header.intersect(layout.rows_area);
            if visible.height() <= 0.0 {
                continue;
            }
            // The controls stand by the picture lane; the other lanes have their names under them.
            let picture = row.lane(Lane::Picture).unwrap_or(row.body);
            let area = Rect::from_x_y_ranges(row.header.x_range(), picture.y_range()).shrink2(vec2(8.0, 0.0));
            let id = sequence.tracks[row.track].id;
            let expanded = self.expanded.contains(&id);
            // Named after the track and not its place: a row dragged by its name keeps the drag.
            let builder = egui::UiBuilder::new().id_salt(("track header", id)).max_rect(area).layout(egui::Layout::left_to_right(egui::Align::Center));
            let mut header = ui.new_child(builder);
            header.set_clip_rect(visible);
            header.spacing_mut().item_spacing.x = 4.0;
            let track = &mut sequence.tracks[row.track];
            // The triangle folds and unfolds the row: pointing right folded, down unfolded.
            let (fold_rect, folding) = header.allocate_exact_size(FOLD_SIZE.into(), Sense::click());
            let color = if folding.hovered() { header.visuals().strong_text_color() } else { header.visuals().text_color() };
            let c = fold_rect.center();
            let points = if expanded {
                vec![pos2(c.x - 4.5, c.y - 2.5), pos2(c.x + 4.5, c.y - 2.5), pos2(c.x, c.y + 3.5)]
            } else {
                vec![pos2(c.x - 2.5, c.y - 4.5), pos2(c.x + 3.5, c.y), pos2(c.x - 2.5, c.y + 4.5)]
            };
            header.painter().add(Shape::convex_polygon(points, color, Stroke::NONE));
            let hint = if expanded { "Fold: only the picture" } else { "Unfold: the effects and the sound of the clips" };
            if folding.on_hover_text(hint).clicked() {
                fold = Some(id);
            }
            // Numbered from the top, as they are seen. Dragged up or down by its dots or its name, the
            // row moves.
            let (grip_rect, grip) = header.allocate_exact_size(ROW_GRIP_SIZE.into(), Sense::drag());
            let name = header.add(egui::Label::new(RichText::new(format!("Track {}", count - row.track)).small()).sense(Sense::drag()));
            let handle = grip.union(name);
            if handle.drag_started() {
                self.row_drag = Some(id);
            }
            let held = self.row_drag == Some(id);
            if handle.hovered() || held {
                header.ctx().set_cursor_icon(if held { CursorIcon::Grabbing } else { CursorIcon::Grab });
            }
            let dots = if handle.hovered() || held { header.visuals().strong_text_color() } else { header.visuals().weak_text_color() };
            grip_dots(header.painter(), grip_rect.center(), 3, 2, dots);
            handle.on_hover_text("Drag up or down to put the track elsewhere");
            let hide = header.add(egui::Button::new(RichText::new("Hide").small()).selected(track.hidden));
            if hide.on_hover_text("Leave this track's picture out: what is under it shows").clicked() {
                track.hidden = !track.hidden;
                changed = true;
            }
            let mute = header.add(egui::Button::new(RichText::new("Mute").small()).selected(track.muted));
            if mute.on_hover_text("Leave this track's sound out").clicked() {
                track.muted = !track.muted;
                changed = true;
            }
            header.menu_button(RichText::new("...").small(), |ui| {
                let up = ui.add_enabled(row.track + 1 < count, egui::Button::new("Move up"));
                if up.on_hover_text("Above the track over it: its picture covers that one's").clicked() {
                    action = Some(TrackAction::Move { track: row.track, up: true });
                    ui.close_menu();
                }
                let down = ui.add_enabled(row.track > 0, egui::Button::new("Move down"));
                if down.on_hover_text("Under the track below it").clicked() {
                    action = Some(TrackAction::Move { track: row.track, up: false });
                    ui.close_menu();
                }
                if ui.button("Delete track").on_hover_text("With its clips. Undo brings it back").clicked() {
                    action = Some(TrackAction::Delete(row.track));
                    ui.close_menu();
                }
            });
            // The names of the lanes of an unfolded row, by the lanes.
            let painter = ui.painter_at(visible);
            for (lane, rect) in &row.lanes {
                let name = match lane {
                    Lane::Picture => continue,
                    Lane::Effects => "effects",
                    Lane::Sound => "sound",
                };
                let at = pos2(row.header.left() + 8.0 + FOLD_SIZE[0] + 4.0, rect.center().y);
                painter.text(at, Align2::LEFT_CENTER, name, FontId::proportional(11.0), ui.visuals().weak_text_color());
            }
        }
        if let Some(id) = fold {
            self.toggle(id);
            changed = true;
        }
        // A row dragged by its name goes wherever the pointer is, row by row.
        let pointer = ui.input(|i| i.pointer.primary_down().then(|| i.pointer.hover_pos()).flatten());
        match (self.row_drag, pointer) {
            (Some(dragged), Some(p)) => {
                let from = sequence.tracks.iter().position(|track| track.id == dragged);
                let to = layout.row_at(p.y.clamp(layout.rows_area.top() + 1.0, layout.rows_area.bottom() - 1.0)).map(|row| row.track);
                if let (Some(from), Some(to)) = (from, to) {
                    if from != to {
                        sequence.move_track_to(from, to);
                        if let Some(id) = selected.as_mut() {
                            id.track = after_move(id.track, from, to);
                        }
                        changed = true;
                    }
                }
            }
            _ => self.row_drag = None,
        }

        match action {
            Some(TrackAction::Add(index)) => {
                let index = sequence.add_track(index);
                if let Some(id) = selected.as_mut() {
                    id.track = after_insert(id.track, index);
                }
            }
            Some(TrackAction::Move { track, up }) => {
                let to = sequence.move_track(track, up);
                if let Some(id) = selected.as_mut() {
                    if id.track == track {
                        id.track = to;
                    } else if id.track == to {
                        id.track = track;
                    }
                }
            }
            Some(TrackAction::Delete(track)) => {
                sequence.remove_track(track);
                *selected = selected.and_then(|id| match id.track {
                    t if t == track => None,
                    t if t > track => Some(ClipId { track: t - 1, ..id }),
                    _ => Some(id),
                });
            }
            None => {}
        }
        changed || action.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(&self, ui: &egui::Ui, layout: &Layout, view: View, sequence: &Sequence, selected: Option<ClipId>, active: Option<Drag>, env: &TimelineEnv) {
        let painter = ui.painter_at(layout.rect);
        let visuals = ui.visuals();
        let tick_color = visuals.weak_text_color();
        let body = layout.body;
        let across = |x: f32| [pos2(x, body.top()), pos2(x, body.bottom())];
        let x_span = |from: f32, to: f32| from.max(body.left())..=to.min(body.right());
        let rows_painter = painter.with_clip_rect(layout.rows_area.intersect(body));
        let headers_painter = painter.with_clip_rect(layout.rows_area);
        let separator = visuals.widgets.noninteractive.bg_stroke;

        painter.rect_filled(body, 3.0, visuals.extreme_bg_color);
        painter.hline(body.x_range(), layout.ruler.bottom(), separator);
        for row in &layout.rows {
            headers_painter.rect_filled(row.header, 0.0, visuals.faint_bg_color);
            headers_painter.hline(layout.rect.x_range(), row.body.bottom(), separator);
        }
        if layout.rows.is_empty() {
            let at = layout.rows_area.center();
            painter.text(at, Align2::CENTER_CENTER, "No tracks. Add a video from Sources", FontId::proportional(13.0), tick_color);
        }

        // Ruler: it counts from the export start, and travels with it.
        let export = sequence.export;
        let zero = export.start;
        let (major, minor) = tick_steps(view.pps, env.fps);
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
            let Some(row) = layout.row(id.track) else { continue };
            let block = row.block(clip.span(), view);
            let is_selected = selected == Some(id);
            let track = &sequence.tracks[id.track];
            let missing = env.missing.contains(&clip.source);
            if is_selected {
                let ghost = row.block(clip.source_span(), view);
                rows_painter.rect_filled(ghost, 4.0, CLIP_COLOR.gamma_multiply(0.25));
                rows_painter.rect_stroke(ghost, 4.0, Stroke::new(1.0, CLIP_COLOR.gamma_multiply(0.7)));
            }
            let fill = if track.hidden { CLIP_COLOR.gamma_multiply(0.45) } else { CLIP_COLOR };
            rows_painter.rect_filled(block, 4.0, fill);
            let inside = rows_painter.with_clip_rect(block.shrink(2.0).intersect(layout.rows_area).intersect(body));
            let text_x = block.left().max(body.left()) + if is_selected { HANDLE_WIDTH + 6.0 } else { 6.0 };
            let weak = Color32::from_white_alpha(110);
            for &(lane, lane_rect) in &row.lanes {
                let area = Rect::from_x_y_ranges(block.x_range(), lane_rect.y_range()).intersect(block);
                match lane {
                    Lane::Picture => {
                        let mut text = format!("{}  ·  {}", clip.name, timecode::format(clip.span().len()));
                        if missing {
                            text += "  ·  file missing";
                        }
                        let color = if missing { MISSING_COLOR } else { Color32::from_white_alpha(if track.hidden { 120 } else { 220 }) };
                        inside.text(pos2(text_x, area.center().y), Align2::LEFT_CENTER, text, FontId::proportional(12.0), color);
                    }
                    Lane::Effects => {
                        inside.rect_filled(area, 0.0, LANE_SHADE);
                        let from = text_x - 6.0;
                        let chips = place_chips(ui, area, from, effects_of(clip));
                        if chips.is_empty() && effects_of(clip).is_empty() {
                            inside.text(pos2(text_x, area.center().y), Align2::LEFT_CENTER, "no effects", FontId::proportional(11.0), weak);
                        }
                        for (rect, chip) in chips {
                            inside.rect_filled(rect, 4.0, CHIP_COLOR);
                            inside.text(rect.center(), Align2::CENTER_CENTER, chip.text, FontId::proportional(11.0), Color32::WHITE);
                        }
                    }
                    Lane::Sound => {
                        inside.rect_filled(area, 0.0, LANE_SHADE);
                        let muted = clip.muted || track.muted;
                        match env.sounds.get(&clip.source) {
                            Some(SourceSound::Silent) => {
                                inside.text(pos2(text_x, area.center().y), Align2::LEFT_CENTER, "no sound", FontId::proportional(11.0), weak);
                            }
                            Some(SourceSound::Graph(graph)) => match graph.get() {
                                Some(peaks) => paint_sound(&inside, area.intersect(body), clip, view, peaks, muted),
                                None => {
                                    inside.text(pos2(text_x, area.center().y), Align2::LEFT_CENTER, "reading the sound…", FontId::proportional(11.0), weak);
                                }
                            },
                            None => {}
                        }
                    }
                }
            }
            // A fade is its curve from the clip's corner, how much shows from the bottom up, and the
            // part it hides darkened.
            let (fade_in, fade_out) = clip.fade_lengths();
            for (edge, from, to) in [(Edge::In, clip.start, clip.start + fade_in), (Edge::Out, clip.end() - fade_out, clip.end())] {
                let (Some(fade), x0, x1) = (clip.fade(edge), view.x(from), view.x(to)) else { continue };
                if x1 - x0 < 1.0 {
                    continue;
                }
                let steps = ((x1 - x0) / 3.0).ceil().clamp(1.0, 64.0) as usize;
                let points: Vec<Pos2> = (0..=steps)
                    .map(|i| {
                        let x = i as f64 / steps as f64;
                        pos2(x0 + (x1 - x0) * x as f32, block.bottom() - block.height() * fade.curve.level(edge, x) as f32)
                    })
                    .collect();
                let mut shade = Mesh::default();
                for pair in points.windows(2) {
                    let first = shade.vertices.len() as u32;
                    for p in [pair[0], pos2(pair[0].x, block.top()), pair[1], pos2(pair[1].x, block.top())] {
                        shade.colored_vertex(p, FADE_SHADE);
                    }
                    shade.add_triangle(first, first + 1, first + 2);
                    shade.add_triangle(first + 1, first + 3, first + 2);
                }
                inside.add(shade);
                inside.add(Shape::line(points, Stroke::new(1.5, FADE_LINE)));
            }
            if missing {
                rows_painter.rect_stroke(block.shrink(1.0), 4.0, Stroke::new(2.0, MISSING_COLOR));
            }
            if is_selected {
                rows_painter.rect_stroke(block, 4.0, Stroke::new(2.0, HANDLE_COLOR));
                let left = Rect::from_x_y_ranges(block.left()..=block.left() + HANDLE_WIDTH, block.y_range());
                let right = Rect::from_x_y_ranges(block.right() - HANDLE_WIDTH..=block.right(), block.y_range());
                let left_rounding = Rounding { nw: 4.0, sw: 4.0, ..Rounding::ZERO };
                let right_rounding = Rounding { ne: 4.0, se: 4.0, ..Rounding::ZERO };
                for (handle, rounding, drag) in [(left, left_rounding, Drag::ClipStart(id)), (right, right_rounding, Drag::ClipEnd(id))] {
                    let color = if active == Some(drag) { Color32::WHITE } else { HANDLE_COLOR };
                    rows_painter.rect_filled(handle, rounding, color);
                    grip_dots(&rows_painter, handle.center(), 1, 3, Color32::from_black_alpha(150));
                }
                // The fades' lengths are dragged by these, a row of three dots each.
                for edge in [Edge::In, Edge::Out] {
                    if let Some(handle) = fade_handle(block, clip, edge, view) {
                        let color = if active == Some(Drag::Fade(id, edge)) { Color32::WHITE } else { HANDLE_COLOR };
                        rows_painter.rect_filled(handle, 3.0, color);
                        grip_dots(&rows_painter, handle.center(), 3, 1, Color32::from_black_alpha(150));
                    }
                }
            }
        }

        // With the Blade: where a click would cut, across the row, or every row with Shift.
        if let Some(cut) = self.cut {
            let x = view.x(cut.time);
            let rows: Vec<&Row> = if cut.all { layout.rows.iter().collect() } else { layout.row(cut.track).into_iter().collect() };
            for row in rows {
                rows_painter.line_segment([pos2(x, row.body.top()), pos2(x, row.body.bottom())], Stroke::new(2.0, CUT_COLOR));
            }
        }

        // Where the export range shows no clip, the file will be black: the bottom row says so.
        if let Some(bottom) = layout.row(0) {
            for gap in sequence.gaps() {
                let (from, to) = (view.x(gap.start).max(body.left()), view.x(gap.end).min(body.right()));
                if to - from >= GAP_LABEL_WIDTH {
                    let picture = bottom.lane(Lane::Picture).unwrap_or(bottom.body);
                    let at = pos2((from + to) / 2.0, picture.center().y);
                    rows_painter.text(at, Align2::CENTER_CENTER, "black", FontId::proportional(12.0), tick_color);
                }
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
        painter.rect_filled(strip.intersect(body), 0.0, EXPORT_COLOR.gamma_multiply(if moving { 1.0 } else { 0.75 }));
        if strip.width() > 90.0 {
            let length = timecode::format(export.len());
            let at = strip.intersect(body).center();
            painter.text(at, Align2::CENTER_CENTER, length, FontId::monospace(10.0), Color32::WHITE);
        }
        // Below the tick labels.
        let tabs = layout.ruler.top() + 14.0..=layout.ruler.bottom();
        let start_tab = Rect::from_x_y_ranges(x_in..=x_in + ANCHOR_WIDTH, tabs.clone());
        let end_tab = Rect::from_x_y_ranges(x_out - ANCHOR_WIDTH..=x_out, tabs);
        let body_painter = painter.with_clip_rect(body);
        for (x, tab, drag) in [(x_in, start_tab, Drag::ExportStart), (x_out, end_tab, Drag::ExportEnd)] {
            let color = if active == Some(drag) { Color32::WHITE } else { EXPORT_COLOR };
            body_painter.line_segment(across(x), Stroke::new(2.0, color));
            body_painter.rect_filled(tab, 2.0, color);
            grip_dots(&body_painter, tab.center(), 1, 3, Color32::from_black_alpha(150));
        }

        if let Some(t) = self.snapped {
            body_painter.line_segment(across(view.x(t)), Stroke::new(1.0, Color32::from_white_alpha(160)));
        }

        // Where a click on the ruler would take the playhead: a mark at the pointer, with its time.
        if let Some(x) = self.ruler_hover {
            let top = layout.ruler.top();
            let stroke = Stroke::new(2.5, HANDLE_COLOR);
            body_painter.line_segment([pos2(x - 6.0, top + 1.0), pos2(x, top + 7.0)], stroke);
            body_painter.line_segment([pos2(x, top + 7.0), pos2(x + 6.0, top + 1.0)], stroke);
            body_painter.line_segment([pos2(x, top + 7.0), pos2(x, body.bottom())], Stroke::new(1.0, HANDLE_COLOR.gamma_multiply(0.45)));
            // Not over the playhead's own time.
            if (x - view.center_x).abs() > 80.0 {
                let label = timecode::format(view.t(x) - zero);
                painter.text(pos2(x, layout.rect.top() + 4.0), Align2::CENTER_TOP, label, FontId::monospace(11.0), HANDLE_COLOR);
            }
        }

        // Where a dragged source would go: into a row, or a new row under the others.
        if let Some(ghost) = &self.ghost {
            let x = view.x(ghost.span.start)..=view.x(ghost.span.end);
            let new_track = Some(format!("{}  ·  new track", ghost.name));
            let (block, label) = match ghost.place {
                // The dragged name is at the pointer already.
                Place::Into(track) => (layout.row(track).map_or(Rect::NOTHING, |row| row.block(ghost.span, view)), None),
                // Across the border between that row and the one above it.
                Place::Above(track) => {
                    let top = layout.row(track).map_or(layout.rows_area.top(), |row| row.body.top());
                    let band = (top - PICTURE_LANE_HEIGHT / 4.0).max(layout.rows_area.top())..=top + PICTURE_LANE_HEIGHT / 4.0;
                    (Rect::from_x_y_ranges(x, band), new_track)
                }
                Place::Bottom => {
                    let bottom = layout.rows_area.bottom();
                    (Rect::from_x_y_ranges(x, bottom - PICTURE_LANE_HEIGHT / 2.0..=bottom - 2.0), new_track)
                }
            };
            rows_painter.rect_filled(block, 4.0, HANDLE_COLOR.gamma_multiply(0.35));
            rows_painter.rect_stroke(block, 4.0, Stroke::new(1.5, HANDLE_COLOR));
            if let Some(label) = label {
                let at = pos2(block.left().max(body.left()) + 6.0, block.center().y);
                rows_painter.text(at, Align2::LEFT_CENTER, label, FontId::proportional(12.0), Color32::WHITE);
            }
        }

        // The rows' scroll bar, with the dots of a thing that is dragged on its thumb.
        if let Some((track, thumb, hot)) = self.scroll_bar {
            painter.rect_filled(track, 3.0, Color32::from_white_alpha(22));
            let thumb = thumb.shrink2(vec2(2.0, 1.0));
            painter.rect_filled(thumb, 3.0, Color32::from_gray(if hot { 190 } else { 130 }));
            if thumb.height() >= SCROLL_THUMB_MIN - 2.0 {
                grip_dots(&painter, thumb.center(), 1, 3, Color32::from_black_alpha(170));
            }
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
    if !layout.rows_area.contains(p) || p.x < layout.body.left() {
        return None;
    }
    // Edges and fades only on the selected clip: the others show no handles to grab.
    if let Some(id) = selected.filter(|&id| sequence.has(id)) {
        let block = layout.block(id, sequence.clip(id).span(), view);
        for edge in [Edge::In, Edge::Out] {
            if fade_handle(block, sequence.clip(id), edge, view).is_some_and(|handle| handle.expand(2.0).contains(p)) {
                return Some(Drag::Fade(id, edge));
            }
        }
        if block.y_range().contains(p.y) {
            if let Some(start) = nearer_edge(p.x, block.left(), block.right(), HANDLE_WIDTH) {
                return Some(if start { Drag::ClipStart(id) } else { Drag::ClipEnd(id) });
            }
        }
    }
    sequence.clips().find(|&(id, clip)| layout.block(id, clip.span(), view).contains(p)).map(|(id, _)| Drag::ClipMove(id))
}

/// Where the handle of the fade at `edge` of a clip drawn as `block` is, if it has that fade: on the
/// top edge, where the fade ends.
fn fade_handle(block: Rect, clip: &Clip, edge: Edge, view: View) -> Option<Rect> {
    clip.fade(edge)?;
    let (fade_in, fade_out) = clip.fade_lengths();
    let x = match edge {
        Edge::In => view.x(clip.start + fade_in),
        Edge::Out => view.x(clip.end() - fade_out),
    };
    // Kept off the corners, where the edges are trimmed. A clip too narrow for that on screen has
    // none: its fades are set in the Clip tab, or after zooming in.
    let (lo, hi) = (block.left() + HANDLE_WIDTH + FADE_HANDLE_SIZE[0] / 2.0, block.right() - HANDLE_WIDTH - FADE_HANDLE_SIZE[0] / 2.0);
    if lo > hi {
        return None;
    }
    let x = x.clamp(lo, hi);
    Some(Rect::from_center_size(pos2(x, block.top() + FADE_HANDLE_SIZE[1] / 2.0 + 1.0), FADE_HANDLE_SIZE.into()))
}

/// A clip's sound as a graph in `area`: for each pixel, the loudest the source gets in the time it
/// stands for, times the clip's volume and its fades. Pale when the clip or its track is muted.
fn paint_sound(painter: &egui::Painter, area: Rect, clip: &Clip, view: View, peaks: &Peaks, muted: bool) {
    let color = if muted { WAVE_COLOR.gamma_multiply(0.35) } else { WAVE_COLOR };
    let (middle, half) = (area.center().y, area.height() / 2.0 - 3.0);
    let mut x = area.left().floor();
    while x < area.right() {
        let (from, to) = (clip.source_time(view.t(x)), clip.source_time(view.t(x + 1.0)));
        let fade = clip.sound_fade(view.t(x + 0.5)) as f32;
        let level = (peaks.level(from, to) * clip.volume as f32 * fade).min(1.0);
        let height = (level * half).max(0.5);
        painter.line_segment([pos2(x + 0.5, middle - height), pos2(x + 0.5, middle + height)], Stroke::new(1.0, color));
        x += 1.0;
    }
}

/// Where a source `len` seconds long dropped at `p` goes: its start at the pointer, on the frame
/// grid. In the row under the pointer if there is room for it around its middle, pulled in to fit;
/// on a new row right above that one if not; under all the rows when it is not over one.
fn drop_place(layout: &Layout, view: View, sequence: &Sequence, p: Pos2, len: f64, frame: f64) -> (Place, f64) {
    let start = (view.t(p.x) / frame).round() * frame;
    let Some(row) = layout.row_at(p.y) else { return (Place::Bottom, start) };
    match sequence.tracks[row.track].gap_at(start + len / 2.0).filter(|gap| gap.len() + 1e-6 >= len) {
        Some(gap) => (Place::Into(row.track), start.max(gap.start).min(gap.end - len)),
        None => (Place::Above(row.track), start),
    }
}

/// Puts a clip where `place` says. Returns where it is.
fn put(sequence: &mut Sequence, place: Place, clip: Clip) -> Option<ClipId> {
    match place {
        Place::Into(track) => sequence.place_clip(Some(track), clip),
        Place::Above(track) => {
            let track = sequence.add_track(track + 1);
            sequence.place_clip(Some(track), clip)
        }
        Place::Bottom => sequence.place_clip(None, clip),
    }
}

/// Where track `t` is once an empty track has been put at `index`.
fn after_insert(t: usize, index: usize) -> usize {
    if t >= index {
        t + 1
    } else {
        t
    }
}

/// Where track `t` is once the track at `from` has moved to `to`.
fn after_move(t: usize, from: usize, to: usize) -> usize {
    if t == from {
        to
    } else if from < to && t > from && t <= to {
        t - 1
    } else if to < from && t >= to && t < from {
        t + 1
    } else {
        t
    }
}

/// A small button with a plus, and an arrow up or down after it when `arrow` says which. They are
/// drawn: the font has no arrows.
fn add_track_button(ui: &mut egui::Ui, arrow: Option<bool>, enabled: bool) -> egui::Response {
    let width = if arrow.is_some() { 34.0 } else { 24.0 };
    let response = ui.add_enabled(enabled, egui::Button::new("").min_size(vec2(width, 20.0)));
    let color = if response.enabled() { ui.style().interact(&response).text_color() } else { ui.visuals().weak_text_color() };
    let painter = ui.painter();
    let center = response.rect.center();
    let stroke = Stroke::new(1.5, color);
    let plus = if arrow.is_some() { center.x - 6.0 } else { center.x };
    painter.line_segment([pos2(plus - 4.0, center.y), pos2(plus + 4.0, center.y)], stroke);
    painter.line_segment([pos2(plus, center.y - 4.0), pos2(plus, center.y + 4.0)], stroke);
    if let Some(up) = arrow {
        let (x, dir) = (center.x + 7.0, if up { -1.0 } else { 1.0 });
        painter.line_segment([pos2(x, center.y - 5.0 * dir), pos2(x, center.y + 2.0 * dir)], stroke);
        let head = vec![pos2(x - 3.5, center.y + 1.0 * dir), pos2(x + 3.5, center.y + 1.0 * dir), pos2(x, center.y + 6.0 * dir)];
        painter.add(Shape::convex_polygon(head, color, Stroke::NONE));
    }
    response
}

/// Pixels per second to scroll the rows while a clip is dragged at height `y`; negative is up.
fn row_autoscroll_speed(y: f32, rows: Rect) -> f32 {
    if y < rows.top() + ROW_AUTOSCROLL_ZONE {
        -ROW_AUTOSCROLL_SPEED
    } else if y > rows.bottom() - ROW_AUTOSCROLL_ZONE {
        ROW_AUTOSCROLL_SPEED
    } else {
        0.0
    }
}

/// What the pointer looks like over `target`: nothing grabbed means the tape.
fn cursor(target: Option<Drag>) -> CursorIcon {
    match target {
        None => CursorIcon::Grab,
        Some(Drag::Scrub { .. }) => CursorIcon::Grabbing,
        Some(Drag::ClipMove(_) | Drag::ExportMove) => CursorIcon::Move,
        Some(Drag::ClipStart(_) | Drag::ClipEnd(_) | Drag::ExportStart | Drag::ExportEnd | Drag::Fade(..)) => CursorIcon::ResizeHorizontal,
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
        Drag::Fade(id, edge) => {
            let clip = sequence.clip(id);
            let (fade_in, fade_out) = clip.fade_lengths();
            Some(if edge == Edge::In { clip.start + fade_in } else { clip.end() - fade_out })
        }
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
        Drag::Fade(id, edge) => {
            let (t, snapped) = place(t);
            let clip = sequence.clip_mut(id);
            let seconds = if edge == Edge::In { t - clip.start } else { clip.end() - t };
            clip.set_fade_length(edge, seconds);
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

    /// Label 26, ruler 32, then the rows: with one track, its clips are drawn from y = 62 to 98.
    fn layout(sequence: &Sequence, height: f32) -> Layout {
        Layout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, height)), sequence, &BTreeSet::new(), 0.0)
    }

    #[test]
    fn a_press_grabs_what_the_cursor_is_over() {
        // A clip from 30 s to 50 s (x = 400..600), the export range from 20 s to 60 s (x = 300..700).
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 20.0, ..Clip::new("a".into(), 60.0) });
        sequence.export = Span { start: 20.0, end: 60.0 };
        let (layout, view) = (layout(&sequence, 102.0), view());
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
        // The headers are left of the timeline: nothing there to grab.
        assert!(at(100.0, track, selected).is_none());
    }

    #[test]
    fn a_clip_is_found_on_its_own_track() {
        // The upper track holds a clip where the lower one has a gap.
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 5.0, ..Clip::new("a".into(), 60.0) });
        sequence.tracks.push(crate::core::edit::Track { id: 1, clips: vec![Clip { start: 38.0, source_out: 5.0, ..Clip::new("b".into(), 60.0) }], ..Default::default() });
        let upper = ClipId { track: 1, index: 0 };
        let layout = layout(&sequence, 146.0);
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
    fn rows_go_from_the_top_track_down_and_scroll() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        for _ in 0..5 {
            sequence.add_below(Clip::new("b".into(), 1.0));
        }
        let folded = BTreeSet::new();
        assert_eq!(Layout::rows_height(&sequence, &folded), 6.0 * PICTURE_LANE_HEIGHT);
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 58.0 + 4.0 * PICTURE_LANE_HEIGHT));
        let layout = Layout::new(rect, &sequence, &folded, 0.0);
        assert_eq!(layout.rows.iter().map(|row| row.track).collect::<Vec<_>>(), [5, 4, 3, 2, 1, 0]);
        assert_eq!(layout.row_at(60.0).map(|row| row.track), Some(5));
        assert_eq!(layout.row_at(110.0).map(|row| row.track), Some(4));
        // The fifth row is under the bottom of the area: not in sight.
        assert_eq!(layout.row_at(58.0 + 4.0 * PICTURE_LANE_HEIGHT + 10.0).map(|row| row.track), None);
        assert_eq!(layout.rows[0].header.right(), HEADER_WIDTH);
        assert_eq!(layout.rows[0].lanes.iter().map(|(lane, _)| *lane).collect::<Vec<_>>(), [Lane::Picture]);

        // Scrolled by two rows, the third one is at the top.
        let layout = Layout::new(rect, &sequence, &folded, 2.0 * PICTURE_LANE_HEIGHT);
        assert_eq!(layout.row_at(60.0).map(|row| row.track), Some(3));
        assert!(layout.row_at(30.0).is_none());
    }

    #[test]
    fn a_clip_is_grabbed_in_its_row_wherever_that_is() {
        // A clip at 38..43 s on the top track, one at 30..35 s under it; the rows scrolled by 20 px.
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 5.0, ..Clip::new("a".into(), 60.0) });
        sequence.tracks.push(crate::core::edit::Track { id: 1, clips: vec![Clip { start: 38.0, source_out: 5.0, ..Clip::new("b".into(), 60.0) }], ..Default::default() });
        let layout = Layout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 100.0)), &sequence, &BTreeSet::new(), 20.0);
        let at = |x: f32, y: f32| hit(pos2(x, y), &layout, view(), &sequence, None);
        let upper = ClipId { track: 1, index: 0 };
        // Upper row: 38..82, its clips 42..78. Lower row: 82..126, seen down to 100.
        assert!(matches!(at(505.0, 60.0), Some(Drag::ClipMove(id)) if id == upper));
        assert!(matches!(at(425.0, 95.0), Some(Drag::ClipMove(CLIP))));
        // Above the area the rows scrolled out of, and below its bottom, there is nothing.
        assert!(at(505.0, 40.0).is_none());
        assert!(at(425.0, 110.0).is_none());
    }

    #[test]
    fn a_dropped_source_goes_into_a_row_with_room_or_a_new_one() {
        // One row with clips at 30..35 s and 50..60 s (x = 400..450 and 600..700), 10 px per second.
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 5.0, ..Clip::new("a".into(), 60.0) });
        sequence.tracks[0].clips.push(Clip { start: 50.0, source_out: 10.0, ..Clip::new("b".into(), 60.0) });
        let layout = layout(&sequence, 102.0);
        let at = |x: f32, len: f64| drop_place(&layout, view(), &sequence, pos2(x, 80.0), len, 0.04);
        // Into the gap of 15 s between them, pulled in to fit.
        let (place, start) = at(510.0, 8.0);
        assert_eq!(place, Place::Into(0));
        assert!((start - 41.0).abs() < 1e-9, "{start}");
        let (place, start) = at(550.0, 8.0);
        assert_eq!(place, Place::Into(0));
        assert!((start - 42.0).abs() < 1e-9, "{start}");
        // Too long for the gap, or over a clip: a new row right above, where it was dropped.
        assert_eq!(at(510.0, 20.0).0, Place::Above(0));
        let (place, start) = at(420.0, 2.0);
        assert_eq!(place, Place::Above(0));
        assert!((start - 32.0).abs() < 1e-9, "{start}");
        // Not over a row, on the ruler: a new row under the others.
        assert_eq!(drop_place(&layout, view(), &sequence, pos2(510.0, 40.0), 2.0, 0.04).0, Place::Bottom);

        // Put there: above means between that row and the one over it.
        let mut sequence = sequence.clone();
        sequence.add_below(Clip::new("c".into(), 1.0));
        let id = put(&mut sequence, Place::Above(0), Clip { start: 32.0, ..Clip::of(1, "d".into(), 2.0) }).unwrap();
        assert_eq!(id, ClipId { track: 1, index: 0 });
        assert_eq!(sequence.tracks.iter().map(|track| track.clips.len()).collect::<Vec<_>>(), [1, 1, 2]);
    }

    #[test]
    fn tracks_keep_track_of_each_other_when_one_comes_or_moves() {
        assert_eq!([0, 1, 2, 3].map(|t| after_insert(t, 2)), [0, 1, 3, 4]);
        assert_eq!([0, 1, 2, 3].map(|t| after_insert(t, 0)), [1, 2, 3, 4]);
        // The track at 1 goes to 3: the ones it passes come down by one.
        assert_eq!([0, 1, 2, 3, 4].map(|t| after_move(t, 1, 3)), [0, 3, 1, 2, 4]);
        // And back up.
        assert_eq!([0, 1, 2, 3, 4].map(|t| after_move(t, 3, 1)), [0, 2, 3, 1, 4]);
        assert_eq!([0, 1, 2].map(|t| after_move(t, 1, 1)), [0, 1, 2]);
    }

    #[test]
    fn the_scroll_bar_thumb_goes_with_the_rows() {
        // Rows 400 px tall, 100 of them in sight, a bar from y = 50 to 150.
        let bar = RowsBar::new(Rect::from_min_max(pos2(990.0, 50.0), pos2(1000.0, 150.0)), 100.0, 400.0);
        assert_eq!(bar.max_scroll, 300.0);
        assert_eq!(bar.thumb_height, 25.0);
        assert_eq!(bar.thumb(0.0).top(), 50.0);
        assert_eq!(bar.thumb(300.0).bottom(), 150.0);
        assert_eq!(bar.thumb(150.0).top(), 87.5);
        // And back: where the thumb is put says the scroll, and no further than the rows go.
        assert_eq!(bar.scroll_at(87.5), 150.0);
        assert_eq!(bar.scroll_at(10.0), 0.0);
        assert_eq!(bar.scroll_at(500.0), 300.0);
        // Many rows: the thumb stays big enough to grab.
        assert_eq!(RowsBar::new(Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 100.0)), 100.0, 5000.0).thumb_height, SCROLL_THUMB_MIN);
    }

    #[test]
    fn the_rows_give_a_little_past_their_ends_and_spring_back() {
        // Rows 300 px taller than their room.
        assert_eq!(push(100.0, 50.0, 300.0), 150.0, "inside, as far as pushed");
        assert_eq!(push(290.0, 40.0, 300.0), 330.0, "from inside, the push takes it past");
        assert!((push(310.0, 40.0, 300.0) - 324.0).abs() < 1e-4, "past the end, it gives way less");
        assert_eq!(push(300.0 + OVERSCROLL, 500.0, 300.0), 300.0 + OVERSCROLL, "and no further than this");
        assert_eq!(push(0.0, -500.0, 0.0), -OVERSCROLL, "rows that fit give way too");
        // Dragged: past an end only part of the way.
        assert_eq!(rubber(150.0, 300.0), 150.0);
        assert!((rubber(-40.0, 300.0) + 14.0).abs() < 1e-4);
        assert_eq!(rubber(10_000.0, 300.0), 300.0 + OVERSCROLL);
        // Let go, they come back to the end, and stop there.
        let mut scroll = 300.0 + OVERSCROLL;
        for _ in 0..60 {
            scroll = settle(scroll, 300.0, 1.0 / 60.0);
        }
        assert_eq!(scroll, 300.0);
        assert_eq!(settle(-20.0, 0.0, 1.0), 0.0);
        assert_eq!(settle(120.0, 300.0, 0.016), 120.0, "inside, nothing moves");
    }

    #[test]
    fn an_unfolded_row_has_the_effects_and_the_sound_under_its_picture() {
        // Two tracks; the lower one, id 0, is unfolded.
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 5.0, ..Clip::new("a".into(), 60.0) });
        sequence.tracks.push(crate::core::edit::Track { id: 1, clips: vec![Clip { start: 38.0, source_out: 5.0, ..Clip::new("b".into(), 60.0) }], ..Default::default() });
        let unfolded = BTreeSet::from([0]);
        let tall = PICTURE_LANE_HEIGHT + EFFECTS_LANE_HEIGHT + SOUND_LANE_HEIGHT;
        assert_eq!(Layout::rows_height(&sequence, &unfolded), PICTURE_LANE_HEIGHT + tall);
        let layout = Layout::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 400.0)), &sequence, &unfolded, 0.0);
        let lower = layout.row(0).unwrap();
        assert_eq!(lower.lanes.iter().map(|(lane, _)| *lane).collect::<Vec<_>>(), [Lane::Picture, Lane::Effects, Lane::Sound]);
        assert_eq!(layout.row(1).unwrap().lanes.len(), 1);
        // The lower row starts under the upper one; its clip covers all three lanes and is grabbed by any.
        let top = 58.0 + PICTURE_LANE_HEIGHT;
        assert_eq!((lower.body.top(), lower.body.bottom()), (top, top + tall));
        let at = |y: f32| hit(pos2(425.0, y), &layout, view(), &sequence, None);
        for y in [top + 20.0, top + PICTURE_LANE_HEIGHT + 10.0, top + tall - 10.0] {
            assert!(matches!(at(y), Some(Drag::ClipMove(CLIP))), "at {y}");
        }
    }

    #[test]
    fn plaques_say_what_is_done_to_a_clip() {
        let mut clip = Clip::new("a".into(), 10.0);
        assert!(effects_of(&clip).is_empty());
        clip.transform = Transform { cx: 0.7, zoom: 1.4, angle_deg: 20.0, mirror: true, ..Transform::default() };
        clip.volume = 0.8;
        let texts: Vec<String> = effects_of(&clip).into_iter().map(|chip| chip.text).collect();
        assert_eq!(texts, ["Frame ×1.40 20°", "Mirror", "Volume 80 %"]);
        assert_eq!(effects_of(&clip)[2].section, Section::Sound);
        clip.muted = true;
        assert_eq!(effects_of(&clip).last().map(|chip| chip.text.as_str()), Some("Muted"));
        // Only moved: the frame is there, without numbers.
        let moved = Clip { transform: Transform { cy: 0.3, ..Transform::default() }, ..Clip::new("b".into(), 1.0) };
        assert_eq!(effects_of(&moved), [Chip { text: "Frame".into(), section: Section::Frame }]);
    }

    #[test]
    fn a_fade_is_dragged_by_its_handle_to_whole_frames() {
        // A clip 30..50 s (x = 400..600), selected, with a fade in of 1 s and a fade out of 1 s.
        let mut sequence = Sequence::new(Clip { start: 30.0, source_out: 20.0, ..Clip::new("a".into(), 60.0) });
        sequence.clip_mut(CLIP).add_fade(Edge::In);
        sequence.clip_mut(CLIP).add_fade(Edge::Out);
        let layout = layout(&sequence, 102.0);
        let block = layout.block(CLIP, sequence.clip(CLIP).span(), view());
        // The handles sit on the top edge where the fades end, but keep off the corners.
        let handle_in = fade_handle(block, sequence.clip(CLIP), Edge::In, view()).unwrap();
        assert_eq!(handle_in.center().x, 400.0 + HANDLE_WIDTH + FADE_HANDLE_SIZE[0] / 2.0);
        assert!(matches!(hit(handle_in.center(), &layout, view(), &sequence, Some(CLIP)), Some(Drag::Fade(CLIP, Edge::In))));
        // Not on a clip that is not selected.
        assert!(matches!(hit(handle_in.center(), &layout, view(), &sequence, None), Some(Drag::ClipMove(CLIP))));
        // None on a clip narrower than its trim handles and a fade handle together.
        let narrow = Rect::from_x_y_ranges(400.0..=440.0, block.y_range());
        assert_eq!(fade_handle(narrow, sequence.clip(CLIP), Edge::In, view()), None);

        // Dragged to 33.01 s: 3 s, on the frame. The fade out leaves no more than 19 s for it.
        edit(&mut sequence, Drag::Fade(CLIP, Edge::In), 33.01, 100.0, 0.0, 0.04);
        assert!((sequence.clip(CLIP).fade_lengths().0 - 3.0).abs() < 1e-9);
        edit(&mut sequence, Drag::Fade(CLIP, Edge::In), 80.0, 100.0, 0.0, 0.04);
        assert!((sequence.clip(CLIP).fade_lengths().0 - 19.0).abs() < 1e-9);
        edit(&mut sequence, Drag::Fade(CLIP, Edge::Out), 45.0, 100.0, 0.0, 0.04);
        assert!((sequence.clip(CLIP).fade_lengths().1 - 1.0).abs() < 1e-9, "no room left for more");
        assert_eq!(cursor(Some(Drag::Fade(CLIP, Edge::Out))), CursorIcon::ResizeHorizontal);
    }

    #[test]
    fn plaques_show_the_fades() {
        let mut clip = Clip::new("a".into(), 10.0);
        clip.add_fade(Edge::Out);
        clip.set_fade_length(Edge::Out, 0.5);
        assert_eq!(effects_of(&clip), [Chip { text: "Fade out 0.5 s".into(), section: Section::Effects }]);
        clip.set_fade_curve(Edge::Out, Curve::EaseInOut);
        assert_eq!(effects_of(&clip)[0].text, "Fade out 0.5 s · Ease in-out");
        clip.add_color();
        assert_eq!(effects_of(&clip)[1], Chip { text: "Color".into(), section: Section::Effects });
    }

    #[test]
    fn a_cut_lands_on_what_is_near_or_on_a_frame() {
        // A clip 30..50 s, the playhead at 42 s; 0.1 s is near enough.
        let sequence = Sequence::new(Clip { start: 30.0, source_out: 20.0, ..Clip::new("a".into(), 60.0) });
        let cut = |t: f64| cut_time(&sequence, t, 42.0, 0.1, 0.04);
        assert_eq!(cut(42.05), 42.0);
        assert_eq!(cut(49.95), 50.0);
        assert!((cut(37.013) - 37.0).abs() < 1e-9);
        assert!((cut(37.03) - 37.04).abs() < 1e-9);
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

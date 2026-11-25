//! The preview: the result at the playhead, its frame upright in the middle of the view and the
//! clips' pictures where they are on it. The selected clip's picture is drawn whole, faint past the
//! frame, with handles that move, resize and turn it. The view fits the frame with all of that
//! picture around it, or is zoomed and moved by hand.

use crate::core::preset::{self, SafeZone};
use crate::media::geometry::{self, Mask, Outline, Placement, Transform, MAX_ASPECT, MIN_ASPECT};
use crate::ui::flash;
use egui::epaint::Vertex;
use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Mesh, Painter, PointerButton, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Vec2};
use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

/// Behind the frame, where the result is not.
pub const BACKGROUND: Color32 = Color32::from_gray(16);
/// Room around what the view fits: for the arrows that turn the picture.
const MARGIN: f32 = 34.0;
const ARROW_RADIUS: f32 = 11.0;
const ARROW_GRAB: f32 = 13.0;
/// How far the arrows stand off the picture's corners, along each of its sides.
const ARROW_OFF_CORNER: f32 = 14.0;
/// How far from a handle of the selected picture a press takes it, and how large the handles are.
const HANDLE_GRAB: f32 = 9.0;
const HANDLE_SIZE: f32 = 7.0;
/// A side shorter than this on the screen has no handle in its middle: its corners are enough, and
/// it leaves the picture something to be moved by.
const SIDE_HANDLE_MIN_LENGTH: f32 = 4.0 * HANDLE_GRAB;
/// How far from a side of the frame a press takes it, while the frame's proportions can change.
const FRAME_SIDE_GRAB: f32 = 7.0;
const SNAP_DISTANCE: f64 = 8.0;
const SNAP_LINE: Color32 = Color32::from_rgb(255, 200, 40);
/// Room for the lock in front of the main frame's label.
const LABEL_LOCK_ROOM: f32 = 15.0;
const WHEEL_SPEED: f64 = 0.003;
/// How fast the wheel zooms the view while it is switched to Zoom: as Ctrl+wheel does.
const WHEEL_ZOOM_SPEED: f64 = 1.0 / 200.0;
/// The selected picture past the frame shows this faint.
const PAST_OPACITY: f32 = 0.35;
/// The pointer goes this far from where it was pressed before a gesture changes anything: a press
/// held still is no drag.
const DRAG_THRESHOLD: f32 = 3.0;
/// The view holds still this long after the wheel, as it does during a drag.
const WHEEL_HOLD_SECONDS: f64 = 0.5;
/// How fast the view eases to what fits: of the way left, all but e to the minus this goes in a second.
const EASE_RATE: f64 = 12.0;
/// How far back the view goes and how close it comes, against the frame filling it.
const MIN_VIEW_ZOOM: f64 = 0.1;
const MAX_VIEW_ZOOM: f64 = 32.0;
/// Over the selected clip's picture when it is not seen at the playhead.
const VEIL: Color32 = Color32::from_black_alpha(150);
const TAG_BACKGROUND: Color32 = Color32::from_black_alpha(190);
/// The strip under the preview with what the result is and how close the view is.
const TAG_STRIP: f32 = 24.0;
/// The dot above the middle of the selected picture's top side: it marks the picture's top, a click
/// on it turns the picture upright, the pointer resting on it opens the menu. How far it stands off
/// the side, and how large it is.
const TOP_OFF: f32 = 20.0;
const TOP_RADIUS: f32 = 5.0;
/// How far the menu's items stand from where it opens, and how large they are.
const MENU_RADIUS: f32 = 54.0;
const MENU_ITEM_RADIUS: f32 = 20.0;
/// In the fan off the dot, the items are this far apart, in radians.
const MENU_FAN_STEP: f32 = 0.91;
/// The pointer rests on the dot this long before the menu opens: reaching for the top side should
/// not open it. And it may wander off this long before the menu closes.
const MENU_OPEN_DELAY: f64 = 0.2;
const MENU_CLOSE_DELAY: f64 = 0.3;
const MENU_FADE_SECONDS: f32 = 0.1;

/// What the selected picture's menu offers.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MenuItem {
    Fit,
    Fill,
    Bars,
    Reset,
}

impl MenuItem {
    const ALL: [Self; 4] = [Self::Fit, Self::Fill, Self::Bars, Self::Reset];

    fn label(self) -> &'static str {
        match self {
            Self::Fit => "Fit",
            Self::Fill => "Fill",
            Self::Bars => "Bars",
            Self::Reset => "Reset",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Fit => "The whole picture in the frame: around it the clips under this one show, or black",
            Self::Fill => "The picture covers the whole frame, turned as it is",
            Self::Bars => "Black around the picture: the clips under this one are hidden there",
            Self::Reset => "Back to the start: covering the frame, centered, no turn, no mirror, no bars",
        }
    }

    /// What it makes of a picture `w`×`h` placed by `t`, and whether the menu stays open after it: it
    /// does for Bars, to be turned back.
    fn apply(self, t: Transform, w: f64, h: f64, outline: Outline) -> (Transform, bool) {
        match self {
            Self::Fit => (t.showing_whole_picture(w, h, outline), false),
            Self::Fill => (t.filling_frame(w, h, outline), false),
            Self::Bars => (Transform { bars: !t.bars, ..t }, true),
            Self::Reset => (Transform::default(), false),
        }
    }
}

/// The selected picture's menu: whether it is open, and what opens and closes it.
#[derive(Debug, Default)]
struct Menu {
    open: bool,
    /// Opened by its key around this point, all round it; else it fans out of the dot.
    at: Option<Pos2>,
    /// Since when the pointer rests on the closed menu's dot.
    resting_since: Option<f64>,
    /// Since when the pointer is away from the open menu.
    away_since: Option<f64>,
    /// After an action or a drag the pointer is still on the dot: the menu opens only once it has left.
    wait_for_leaving: bool,
    /// Its key was pressed: it opens, or closes if it was open.
    asked: bool,
}

impl Menu {
    /// Follows the pointer: `on_mark` and `near` say where it is, `near` meaning near the open menu.
    /// Returns how long to wait until the next change is due, if one is coming.
    fn update(&mut self, now: f64, on_mark: bool, near: bool, dragging: bool) -> Option<f64> {
        if dragging {
            self.close();
            return None;
        }
        if self.open {
            if near {
                self.away_since = None;
                return None;
            }
            let since = *self.away_since.get_or_insert(now);
            if now - since >= MENU_CLOSE_DELAY {
                *self = Self::default();
                return None;
            }
            return Some(MENU_CLOSE_DELAY - (now - since));
        }
        if !on_mark {
            *self = Self::default();
            return None;
        }
        if self.wait_for_leaving {
            return None;
        }
        let since = *self.resting_since.get_or_insert(now);
        if now - since >= MENU_OPEN_DELAY {
            *self = Self { open: true, ..Self::default() };
            return None;
        }
        Some(MENU_OPEN_DELAY - (now - since))
    }

    fn close(&mut self) {
        *self = Self { wait_for_leaving: true, ..Self::default() };
    }
}

/// Where `count` items of a menu go around `origin`: in a fan facing away from `from`, the middle of
/// what the menu is of, or all round it without one, the first at the top. Moved as a whole to stay
/// inside `area`. Returns where the menu opens from, and its items.
fn menu_layout(origin: Pos2, from: Option<Pos2>, count: usize, area: Rect) -> (Pos2, Vec<Pos2>) {
    let angle = |i: usize| match from {
        Some(from) => {
            let outwards = origin - from;
            let facing = if outwards.length() < 1.0 { -FRAC_PI_2 } else { outwards.y.atan2(outwards.x) };
            facing + (i as f32 - (count as f32 - 1.0) / 2.0) * MENU_FAN_STEP
        }
        None => -FRAC_PI_2 + i as f32 * TAU / count.max(1) as f32,
    };
    let items: Vec<Pos2> = (0..count).map(|i| origin + MENU_RADIUS * vec2(angle(i).cos(), angle(i).sin())).collect();
    let inner = area.shrink(MENU_ITEM_RADIUS + 4.0);
    let (lo, hi) = items.iter().fold((origin, origin), |(lo, hi), item| (lo.min(*item), hi.max(*item)));
    let shift = vec2((inner.left() - lo.x).max(0.0) + (inner.right() - hi.x).min(0.0), (inner.top() - lo.y).max(0.0) + (inner.bottom() - hi.y).min(0.0));
    (origin + shift, items.into_iter().map(|item| item + shift).collect())
}

/// A frame to draw on the result.
#[derive(Clone)]
pub struct FrameOverlay {
    pub outline: Outline,
    pub color: Color32,
    pub label: String,
    pub thirds: bool,
    pub center: bool,
    pub safe: Option<SafeZone>,
    /// For the main frame: dragging a side changes the proportions.
    pub free_ratio: bool,
}

/// A clip's picture on the result.
pub struct Layer {
    /// Its frame; none until it is decoded.
    pub texture: Option<TextureHandle>,
    pub size: [f64; 2],
    pub transform: Transform,
    /// How much of it shows: less while it fades.
    pub opacity: f32,
    /// It is a clip at the playhead, which can be picked and moved; not a picture kept on the
    /// screen until that clip's frame is there.
    pub live: bool,
    /// The selected clip when it is not seen at the playhead, and why: it is drawn over the others,
    /// veiled, with this note.
    pub apart: Option<&'static str>,
    /// Which clip it is, the same from one frame to the next: a gesture keeps to its picture while
    /// the pictures under it come and go. None for one that can't be taken.
    pub key: Option<u64>,
}

/// What the preview shows.
pub struct Scene<'a> {
    /// The pictures, the lowest first.
    pub layers: &'a [Layer],
    /// Which of them is the selected clip's.
    pub selected: Option<usize>,
    /// The main frame first, then those shown for comparison.
    pub frames: &'a [FrameOverlay],
    /// The result's size in its own pixels.
    pub pixels: [f64; 2],
    /// The result plays: it is shown as it is, without handles.
    pub playing: bool,
    /// At the bottom left: what the result is.
    pub caption: &'a str,
    /// In the middle of the frame, when there is nothing to see and why.
    pub empty: Option<&'a str>,
}

/// What the plain wheel does over the preview. Ctrl+wheel and a pinch zoom the view either way.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum WheelMode {
    /// The selected picture's Scale, from its anchor.
    #[default]
    Scale,
    /// The view's Zoom, at the pointer.
    Zoom,
}

impl WheelMode {
    fn name(self) -> &'static str {
        match self {
            Self::Scale => "Scale",
            Self::Zoom => "Zoom",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Scale => "Wheel: Scale. The wheel makes the selected picture larger or smaller from its anchor. Ctrl+wheel or a pinch zooms the view. Z switches",
            Self::Zoom => "Wheel: Zoom. The wheel zooms the view in and out at the pointer, as Ctrl+wheel and a pinch do; the picture's size stays. Z switches",
        }
    }
}

/// What the pointer did on the preview this frame.
#[derive(Debug, Default)]
pub struct CanvasChange {
    /// The layer pressed or clicked on: its clip is to be picked.
    pub picked: Option<usize>,
    /// A layer changed by hand, and how it is now.
    pub changed: Option<(usize, Transform)>,
    /// It was moved, not resized or turned: its clip gets the anchor of where it is.
    pub moved: bool,
    /// The frame's new proportions, while a side of it is dragged.
    pub aspect: Option<f64>,
    /// A gesture began: playback stops for it.
    pub began: bool,
}

/// How the view looks at the result: screen pixels per pixel of the result, and the point of the
/// result in the middle of the view, from the frame's middle.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Look {
    scale: f64,
    at: [f64; 2],
}

impl Look {
    /// A step of `dt` seconds on the way to `to`; there at once when it is nearly there.
    fn eased(self, to: Look, dt: f64) -> Look {
        let k = 1.0 - (-EASE_RATE * dt).exp();
        let scale = (self.scale.ln() + (to.scale.ln() - self.scale.ln()) * k).exp();
        let at = [self.at[0] + (to.at[0] - self.at[0]) * k, self.at[1] + (to.at[1] - self.at[1]) * k];
        let near = (scale / to.scale - 1.0).abs() < 1e-3 && (at[0] - to.at[0]).hypot(at[1] - to.at[1]) * to.scale < 0.5;
        if near {
            to
        } else {
            Look { scale, at }
        }
    }

    /// Zoomed by `factor` with the point of the result under `pointer` staying there: no farther
    /// back or closer than the view goes against `frame_fit`, the scale at which the frame fills it.
    /// `middle` is the middle of the view.
    fn zoomed_at(self, middle: Pos2, pointer: Pos2, factor: f64, frame_fit: f64) -> Look {
        let scale = (self.scale * factor).clamp(frame_fit * MIN_VIEW_ZOOM, frame_fit * MAX_VIEW_ZOOM);
        let d = pointer - middle;
        let (dx, dy) = (f64::from(d.x), f64::from(d.y));
        let point = [self.at[0] + dx / self.scale, self.at[1] + dy / self.scale];
        Look { scale, at: [point[0] - dx / scale, point[1] - dy / scale] }
    }
}

/// The view that fits `area`: the frame of `pixels` in its middle, with all of `picture` around it,
/// its corners in the result's pixels from the frame's middle, and room for the arrows.
fn fit_look(area: Rect, pixels: [f64; 2], picture: Option<[[f64; 2]; 4]>) -> Look {
    let mut half = [pixels[0] / 2.0, pixels[1] / 2.0];
    for corner in picture.into_iter().flatten() {
        for (half, value) in half.iter_mut().zip(corner) {
            *half = half.max(value.abs());
        }
    }
    let room = |side: f32| f64::from((side - 2.0 * MARGIN).max(20.0));
    Look { scale: (room(area.width()) / (2.0 * half[0])).min(room(area.height()) / (2.0 * half[1])), at: [0.0, 0.0] }
}

/// What the view keeps from one frame to the next.
#[derive(Default)]
struct View {
    /// Zoomed or moved by hand; None: Fit.
    zoomed: Option<Look>,
    /// As it was shown last: in Fit it eases to what fits.
    shown: Option<Look>,
    /// The area's size last time: a new one is followed at once.
    area: Option<Vec2>,
    /// It holds still until then, after the wheel.
    hold_until: f64,
}

impl View {
    /// What to show on the way to `target`: at once when zoomed by hand or when the area changed,
    /// as it was while something holds it, else a step closer.
    fn next(&mut self, target: Look, area: Vec2, now: f64, dt: f64, held: bool) -> Look {
        let resized = self.area.replace(area).is_some_and(|before| before != area);
        let held = held || now < self.hold_until;
        let next = match self.shown {
            Some(shown) if !resized && self.zoomed.is_none() && held => shown,
            Some(shown) if !resized && self.zoomed.is_none() => shown.eased(target, dt),
            _ => target,
        };
        self.shown = Some(next);
        next
    }
}

/// The result on the screen.
#[derive(Debug, Clone, Copy)]
struct Board {
    /// Where the frame's middle is.
    center: Pos2,
    /// Screen pixels per pixel of the result.
    scale: f64,
    outline: Outline,
    pixels: [f64; 2],
}

impl Board {
    fn of(area: Rect, look: Look, outline: Outline, pixels: [f64; 2]) -> Self {
        let center = area.center() - vec2((look.at[0] * look.scale) as f32, (look.at[1] * look.scale) as f32);
        Self { center, scale: look.scale, outline, pixels }
    }

    fn frame(&self) -> Rect {
        Rect::from_center_size(self.center, vec2((self.pixels[0] * self.scale) as f32, (self.pixels[1] * self.scale) as f32))
    }

    /// A point of the result, in its pixels from the frame's middle, on the screen.
    fn screen(&self, p: [f64; 2]) -> Pos2 {
        self.center + vec2((p[0] * self.scale) as f32, (p[1] * self.scale) as f32)
    }

    /// The other way round.
    fn result(&self, p: Pos2) -> [f64; 2] {
        let d = p - self.center;
        [f64::from(d.x) / self.scale, f64::from(d.y) / self.scale]
    }

    fn placement(&self, layer: &Layer) -> Placement {
        let [w, h] = layer.size;
        layer.transform.placement(w, h, self.outline, self.pixels)
    }

    /// The corners of the layer's picture on the screen, going round it from the picture's top left.
    fn corners(&self, layer: &Layer) -> [Pos2; 4] {
        corners_px(layer, self.outline, self.pixels).map(|p| self.screen(p))
    }

    fn in_frame(&self, p: Pos2) -> bool {
        let frame = self.frame();
        match self.outline.mask {
            Mask::Rect => frame.contains(p),
            Mask::Circle => p.distance(frame.center()) <= frame.width() / 2.0,
        }
    }

    /// Whether `p` is on the layer's picture; one with bars has all of the frame too.
    fn hits(&self, layer: &Layer, p: Pos2) -> bool {
        if layer.transform.bars && self.in_frame(p) {
            return true;
        }
        let uv = self.uv(layer, p);
        (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)
    }

    /// Where `p` on the screen is in the layer's picture: from 0 to 1 across it and down.
    fn uv(&self, layer: &Layer, p: Pos2) -> Pos2 {
        let [w, h] = layer.size;
        let t = &layer.transform;
        let scale = t.picture_scale(w, h, self.outline, self.pixels);
        let [u, v] = self.result(p);
        let [x, y] = t.output_to_source(w, h, [u / scale, v / scale]);
        pos2((x / w) as f32, (y / h) as f32)
    }
}

/// The corners of a layer's picture in the result's pixels from the frame's middle, going round it
/// from the picture's top left.
fn corners_px(layer: &Layer, outline: Outline, pixels: [f64; 2]) -> [[f64; 2]; 4] {
    let [w, h] = layer.size;
    let t = &layer.transform;
    let scale = t.picture_scale(w, h, outline, pixels);
    [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]].map(|p| {
        let [u, v] = t.source_to_output(w, h, p);
        [u * scale, v * scale]
    })
}

/// A handle of the selected picture: by the number of its corner, going round the picture from its
/// top left, or of the side that goes from that corner to the next.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Handle {
    Corner(usize),
    /// The middle of a side.
    Side(usize),
    /// The arrow outside a corner, which turns the picture.
    Arrow(usize),
    /// The dot above the middle of its top side: the menu, and upright.
    Top,
}

/// The handles of a picture on the screen.
struct Handles {
    corners: [Pos2; 4],
    /// None for a side too short to have one.
    sides: [Option<Pos2>; 4],
    arrows: [Pos2; 4],
    /// The dot that marks its top, turned with it.
    top: Pos2,
    center: Pos2,
}

impl Handles {
    fn of(corners: [Pos2; 4]) -> Self {
        let along = (corners[1] - corners[0]).normalized();
        let down = (corners[3] - corners[0]).normalized();
        let out = [-along - down, along - down, along + down, down - along];
        Self {
            corners,
            sides: std::array::from_fn(|i| {
                let (from, to) = (corners[i], corners[(i + 1) % 4]);
                (from.distance(to) >= SIDE_HANDLE_MIN_LENGTH).then(|| from.lerp(to, 0.5))
            }),
            arrows: std::array::from_fn(|i| corners[i] + out[i] * ARROW_OFF_CORNER),
            top: corners[0].lerp(corners[1], 0.5) - down * TOP_OFF,
            center: corners[0].lerp(corners[2], 0.5),
        }
    }

    /// The handle at `p`: the dot first, then a corner, the middle of a side, an arrow.
    fn at(&self, p: Pos2) -> Option<Handle> {
        if self.top.distance(p) <= TOP_RADIUS + 3.0 {
            return Some(Handle::Top);
        }
        if let Some(i) = self.corners.iter().position(|corner| corner.distance(p) <= HANDLE_GRAB) {
            return Some(Handle::Corner(i));
        }
        if let Some(i) = self.sides.iter().position(|side| side.is_some_and(|side| side.distance(p) <= HANDLE_GRAB)) {
            return Some(Handle::Side(i));
        }
        self.arrows.iter().position(|arrow| arrow.distance(p) <= ARROW_GRAB).map(Handle::Arrow)
    }

    /// Which way the handle pulls the picture on the screen: for the cursor.
    fn direction(&self, handle: Handle) -> Vec2 {
        match handle {
            Handle::Corner(i) => self.corners[i] - self.corners[(i + 2) % 4],
            Handle::Side(i) => self.corners[i].lerp(self.corners[(i + 1) % 4], 0.5) - self.center,
            Handle::Arrow(i) => self.arrows[i] - self.center,
            Handle::Top => self.top - self.center,
        }
    }
}

/// The side of the upright `frame` at `p`, pointing out of it.
fn frame_side(frame: Rect, p: Pos2) -> Option<Vec2> {
    let across = (frame.top() - FRAME_SIDE_GRAB..=frame.bottom() + FRAME_SIDE_GRAB).contains(&p.y);
    let along = (frame.left() - FRAME_SIDE_GRAB..=frame.right() + FRAME_SIDE_GRAB).contains(&p.x);
    let sides = [
        (vec2(-1.0, 0.0), (p.x - frame.left()).abs(), across),
        (vec2(1.0, 0.0), (p.x - frame.right()).abs(), across),
        (vec2(0.0, -1.0), (p.y - frame.top()).abs(), along),
        (vec2(0.0, 1.0), (p.y - frame.bottom()).abs(), along),
    ];
    sides.into_iter().filter(|&(_, distance, within)| within && distance <= FRAME_SIDE_GRAB).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(side, ..)| side)
}

/// What a press at a point takes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    /// A side of the frame, pointing out of it, while its proportions can change.
    FrameSide(Vec2),
    Handle(Handle),
    /// A picture, by its layer.
    Picture(usize),
}

#[derive(Debug, Clone, Copy)]
enum Drag {
    /// The picture follows the pointer.
    Move { layer: usize, start: Transform },
    /// The point `fixed` stays, the opposite corner or side; the press was at `press`. Both in the
    /// result's pixels from the frame's middle.
    Resize { layer: usize, start: Transform, handle: Handle, fixed: [f64; 2], press: [f64; 2] },
    /// Around `pivot` on the screen, from the pointer's direction there when pressed.
    Turn { layer: usize, start: Transform, handle: Handle, pivot: Pos2, press_angle: f64 },
    /// A side of the frame, `side` pointing out of it. `grab` is how far inside the side the press
    /// was, `across` how long the frame's other side is on the screen: it stays so.
    Reshape { side: Vec2, grab: f32, across: f32 },
    /// The view, by the right or the middle button, from how it was.
    Pan { from: Look },
}

impl Drag {
    /// The layer the gesture is about, if it is about one.
    fn layer(&self) -> Option<usize> {
        match *self {
            Self::Move { layer, .. } | Self::Resize { layer, .. } | Self::Turn { layer, .. } => Some(layer),
            Self::Reshape { .. } | Self::Pan { .. } => None,
        }
    }

    /// The same gesture about the layer at `to`: where its picture is in the list now.
    fn with_layer(mut self, to: usize) -> Self {
        if let Self::Move { layer, .. } | Self::Resize { layer, .. } | Self::Turn { layer, .. } = &mut self {
            *layer = to;
        }
        self
    }

    fn begin(target: Target, press: Pos2, board: &Board, layers: &[Layer], selected: Option<usize>) -> Option<Self> {
        match target {
            Target::FrameSide(side) => {
                let frame = board.frame();
                let (half, across) = if side.x != 0.0 { (frame.width() / 2.0, frame.height()) } else { (frame.height() / 2.0, frame.width()) };
                Some(Self::Reshape { side, grab: half - (press - frame.center()).dot(side), across })
            }
            Target::Handle(handle) => {
                let layer = selected?;
                let start = layers[layer].transform;
                let corners = corners_px(&layers[layer], board.outline, board.pixels);
                let middle = |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
                let press_px = board.result(press);
                match handle {
                    Handle::Corner(i) => Some(Self::Resize { layer, start, handle, fixed: corners[(i + 2) % 4], press: press_px }),
                    Handle::Side(i) => Some(Self::Resize { layer, start, handle, fixed: middle(corners[(i + 2) % 4], corners[(i + 3) % 4]), press: press_px }),
                    // The dot stands on the top side: dragged, it moves that side; a click turns the
                    // picture upright.
                    Handle::Top => Some(Self::Resize { layer, start, handle, fixed: middle(corners[2], corners[3]), press: press_px }),
                    Handle::Arrow(_) => {
                        // A picture that follows the frame turns around the frame's middle, any other in its place.
                        let pivot = board.screen(if start.anchor.is_frame() { [0.0, 0.0] } else { board.placement(&layers[layer]).center });
                        Some(Self::Turn { layer, start, handle, pivot, press_angle: screen_angle(pivot, press) })
                    }
                }
            }
            Target::Picture(layer) => Some(Self::Move { layer, start: layers[layer].transform }),
        }
    }
}

/// The picture moved by `shift` on the screen from how it was at the start; with `snap`, its middle
/// and its edges onto the frame's. Returns it, and where it snapped: a line down and a line across,
/// on the screen.
fn moved(board: &Board, layer: &Layer, start: Transform, shift: Vec2, snap: bool) -> (Transform, [Option<f32>; 2]) {
    let ([w, h], outline, pixels) = (layer.size, board.outline, board.pixels);
    let place = start.placement(w, h, outline, pixels);
    let mut center = [place.center[0] + f64::from(shift.x) / board.scale, place.center[1] + f64::from(shift.y) / board.scale];
    let mut lines = [None; 2];
    if snap {
        let reach = place.reach();
        for axis in 0..2 {
            let half = pixels[axis] / 2.0;
            let targets = [(0.0, 0.0), (reach[axis] - half, -half), (half - reach[axis], half)];
            let (value, line) = snap_axis(center[axis], targets, SNAP_DISTANCE / board.scale);
            center[axis] = value;
            let middle = if axis == 0 { board.center.x } else { board.center.y };
            lines[axis] = line.map(|line| middle + (line * board.scale) as f32);
        }
    }
    (start.placed(w, h, outline, pixels, center, place.size[0]).clamped(w, h, outline), lines)
}

/// The picture grown or shrunk from `fixed` as the pointer went from `press` to `to`, all in the
/// result's pixels: by how far it went along the line from `fixed` to `press`.
fn resized(board: &Board, layer: &Layer, start: Transform, fixed: [f64; 2], press: [f64; 2], to: [f64; 2]) -> Transform {
    let [w, h] = layer.size;
    let along = [press[0] - fixed[0], press[1] - fixed[1]];
    let reach = |p: [f64; 2]| (p[0] - fixed[0]) * along[0] + (p[1] - fixed[1]) * along[1];
    let factor = (reach(to) / reach(press).max(1e-9)).max(1e-3);
    start.scaled_around(w, h, board.outline, board.pixels, factor, fixed).clamped(w, h, board.outline)
}

/// The picture turned by as much as the pointer went round `pivot` from `press_angle`; with
/// `steps`, to a multiple of 15°.
fn turned(board: &Board, layer: &Layer, start: Transform, pivot: Pos2, press_angle: f64, p: Pos2, steps: bool) -> Transform {
    let [w, h] = layer.size;
    let mut angle = start.angle_deg + screen_angle(pivot, p) - press_angle;
    if steps {
        angle = (angle / 15.0).round() * 15.0;
    }
    start.turned(w, h, geometry::normalize_angle(angle)).clamped(w, h, board.outline)
}

/// The frame's proportions with its `side` at the pointer: both sides of it move alike around its
/// middle, at `center`, and the other side stays `across` long. With `snap`, near a known ratio it
/// takes that one.
fn reshaped(center: Pos2, side: Vec2, grab: f32, across: f32, p: Pos2, snap: bool) -> f64 {
    let half = f64::from(((p - center).dot(side) + grab).max(2.0));
    let aspect = if side.x != 0.0 { 2.0 * half / f64::from(across) } else { f64::from(across) / (2.0 * half) };
    let aspect = aspect.clamp(MIN_ASPECT, MAX_ASPECT);
    if snap {
        preset::snap_aspect(aspect)
    } else {
        aspect
    }
}

#[derive(Default)]
pub struct Canvas {
    drag: Option<Drag>,
    /// Where the pointer went down for the gesture, and whether it has gone far enough from there
    /// for the gesture to change anything.
    press: Pos2,
    under_way: bool,
    /// Where the moved picture snapped: a line down and a line across, on the screen.
    snapped: [Option<f32>; 2],
    view: View,
    /// The key of the picture the gesture is about.
    held: Option<u64>,
    menu: Menu,
    wheel: WheelMode,
}

impl Canvas {
    /// Switches what the plain wheel does: the picture's Scale or the view's Zoom. What its key does.
    pub fn toggle_wheel(&mut self) {
        self.wheel = match self.wheel {
            WheelMode::Scale => WheelMode::Zoom,
            WheelMode::Zoom => WheelMode::Scale,
        };
    }

    /// Opens the selected picture's menu all round the pointer, or out of its dot while the pointer
    /// is not over the preview; closes it when it is open. What its key does.
    pub fn toggle_menu(&mut self) {
        self.menu.asked = true;
    }

    /// Closes the menu, if it is open: what Esc does.
    pub fn close_menu(&mut self) {
        if self.menu.open {
            self.menu.close();
        }
    }

    /// The scene, in all the room left. A press on a picture picks its clip and dragging moves it:
    /// its middle and edges snap to the frame's (Alt: no snapping). The selected picture is drawn
    /// whole, faint past the frame, with its handles: a corner or the middle of a side resizes it,
    /// the opposite one staying; an arrow at a corner turns it (Shift: 15° steps). The wheel
    /// changes its size from its anchor. The dot above its top side turns it upright when clicked
    /// and opens its menu when the pointer rests on it: Fit, Fill, Bars, Reset; `toggle_menu` opens
    /// it round the pointer. While the frame's proportions can change, with the lock open or with
    /// Shift, dragging a side of the frame changes them. Ctrl+wheel and a pinch zoom the view at the
    /// pointer, the right and the middle button move it, the tag at the bottom right takes it back to
    /// Fit. A press held still changes nothing.
    pub fn show(&mut self, ui: &mut egui::Ui, scene: &Scene) -> CanvasChange {
        let room = ui.available_rect_before_wrap();
        let area = Rect::from_min_max(room.min, pos2(room.max.x, (room.max.y - TAG_STRIP).max(room.min.y + 40.0)));
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let strip = ui.allocate_rect(Rect::from_min_max(pos2(room.min.x, area.max.y), room.max), Sense::hover()).rect;
        let main = &scene.frames[0];
        let (outline, pixels, layers) = (main.outline, scene.pixels, scene.layers);
        let selected = scene.selected.filter(|&i| i < layers.len());
        let editing = !scene.playing;
        let (modifiers, now, dt) = ui.input(|i| (i.modifiers, i.time, f64::from(i.stable_dt.min(0.1))));
        let mut change = CanvasChange::default();

        // The view first. During a gesture it holds still; while a side of the frame is dragged, the
        // frame's other side keeps its length on the screen.
        let frame_fit = fit_look(area, pixels, None).scale;
        let target = self.view.zoomed.unwrap_or_else(|| fit_look(area, pixels, selected.map(|i| corners_px(&layers[i], outline, pixels))));
        let mut look = self.view.next(target, area.size(), now, dt, self.drag.is_some());
        if let Some(Drag::Reshape { side, across, .. }) = self.drag {
            look.scale = f64::from(across) / pixels[usize::from(side.x != 0.0)];
            self.view.shown = Some(look);
        }
        if look != target && self.drag.is_none() {
            let wait = self.view.hold_until - now;
            if wait > 0.0 {
                ui.ctx().request_repaint_after(Duration::from_secs_f64(wait));
            } else {
                ui.ctx().request_repaint();
            }
        }
        let board = Board::of(area, look, outline, pixels);
        let frame = board.frame();

        let reshapable = editing && outline.mask == Mask::Rect && (main.free_ratio || modifiers.shift);
        let handles = selected.filter(|&i| editing && layers[i].live).map(|i| Handles::of(board.corners(&layers[i])));
        // In the frame, the topmost picture; past it only the selected one is there to be taken.
        let picture_at = |p: Pos2| {
            if board.in_frame(p) {
                layers.iter().rposition(|layer| layer.live && board.hits(layer, p))
            } else {
                selected.filter(|&i| editing && layers[i].live && board.hits(&layers[i], p))
            }
        };
        let target_at = |p: Pos2| {
            if let Some(side) = frame_side(frame, p).filter(|_| reshapable) {
                return Some(Target::FrameSide(side));
            }
            if let Some(handle) = handles.as_ref().and_then(|handles| handles.at(p)) {
                return Some(Target::Handle(handle));
            }
            picture_at(p).map(Target::Picture)
        };

        // The selected picture's menu: out of its dot, or all round the pointer when its key asked.
        let mark = handles.as_ref().map(|handles| (handles.top, handles.center));
        if std::mem::take(&mut self.menu.asked) {
            if self.menu.open {
                self.menu.close();
            } else if mark.is_some() {
                self.menu = Menu { open: true, at: response.hover_pos(), ..Menu::default() };
            }
        }
        if mark.is_none() {
            self.menu = Menu::default();
        }
        let layout_of = |menu: &Menu| {
            mark.map(|(top, center)| match menu.at {
                Some(at) => menu_layout(at, None, MenuItem::ALL.len(), area),
                None => menu_layout(top, Some(center), MenuItem::ALL.len(), area),
            })
        };
        let menu_visibility = ui.ctx().animate_bool_with_time(ui.id().with("picture menu"), self.menu.open, MENU_FADE_SECONDS);
        let menu_shown = self.menu.open && menu_visibility > 0.5;
        let layout = layout_of(&self.menu);
        let item_at = |p: Pos2| layout.as_ref().filter(|_| menu_shown).and_then(|(_, items)| items.iter().position(|item| item.distance(p) <= MENU_ITEM_RADIUS));

        // The picture a gesture is about is found again by its key: once a picture that covered the
        // frame no longer does, the pictures under it come into the list, and its place there moves.
        if let (Some(drag), Some(key)) = (self.drag.filter(|drag| drag.layer().is_some()), self.held) {
            self.drag = layers.iter().position(|layer| layer.key == Some(key)).map(|i| drag.with_layer(i));
        }
        let press = ui.input(|i| i.pointer.press_origin());
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(press) = press.filter(|press| item_at(*press).is_none()) {
                self.drag = target_at(press).and_then(|target| Drag::begin(target, press, &board, layers, selected));
                self.held = self.drag.and_then(|drag| drag.layer()).and_then(|i| layers[i].key);
                (self.press, self.under_way) = (press, false);
                change.began = self.drag.is_some();
                if let Some(Drag::Move { layer, .. }) = self.drag {
                    change.picked = Some(layer);
                }
            }
        } else if response.drag_started_by(PointerButton::Secondary) || response.drag_started_by(PointerButton::Middle) {
            if let Some(press) = press {
                self.drag = Some(Drag::Pan { from: look });
                (self.press, self.under_way) = (press, false);
            }
        }
        self.snapped = [None; 2];
        if let (Some(drag), Some(p), true) = (self.drag, response.interact_pointer_pos(), response.dragged()) {
            self.under_way |= p.distance(self.press) >= DRAG_THRESHOLD;
            if self.under_way {
                match drag {
                    Drag::Move { layer, start } => {
                        let (next, lines) = moved(&board, &layers[layer], start, p - self.press, !modifiers.alt);
                        self.snapped = lines;
                        change.changed = Some((layer, next));
                        change.moved = true;
                    }
                    Drag::Resize { layer, start, fixed, press, .. } => {
                        change.changed = Some((layer, resized(&board, &layers[layer], start, fixed, press, board.result(p))));
                    }
                    Drag::Turn { layer, start, pivot, press_angle, .. } => {
                        change.changed = Some((layer, turned(&board, &layers[layer], start, pivot, press_angle, p, modifiers.shift)));
                    }
                    Drag::Reshape { side, grab, across } => {
                        change.aspect = Some(reshaped(board.center, side, grab, across, p, !modifiers.alt));
                    }
                    Drag::Pan { from } => {
                        let shift = p - self.press;
                        let at = [from.at[0] - f64::from(shift.x) / from.scale, from.at[1] - f64::from(shift.y) / from.scale];
                        self.view.zoomed = Some(Look { at, ..from });
                        ui.ctx().request_repaint();
                    }
                }
            }
        }
        if response.drag_stopped() {
            // A zoomed view stays as the frame's side left it.
            if matches!(self.drag, Some(Drag::Reshape { .. })) && self.view.zoomed.is_some() {
                self.view.zoomed = self.view.shown;
            }
            self.drag = None;
            self.under_way = false;
            ui.ctx().request_repaint();
        }
        if let (true, Some(p)) = (response.clicked(), response.interact_pointer_pos()) {
            match (item_at(p), selected) {
                (Some(item), Some(layer)) => {
                    let ([w, h], t) = (layers[layer].size, layers[layer].transform);
                    let (next, stays) = MenuItem::ALL[item].apply(t, w, h, outline);
                    change.changed = Some((layer, next.clamped(w, h, outline)));
                    if !stays {
                        self.menu.close();
                    }
                }
                _ => match (target_at(p), selected) {
                    (Some(Target::Handle(Handle::Top)), Some(layer)) => {
                        let ([w, h], t) = (layers[layer].size, layers[layer].transform);
                        change.changed = Some((layer, t.turned(w, h, 0.0).clamped(w, h, outline)));
                    }
                    (Some(Target::Picture(i)), _) => change.picked = Some(i),
                    _ => {}
                },
            }
        }
        let pointer = response.hover_pos().filter(|_| response.hovered());
        if response.hovered() {
            // Taken from the input, so that nothing else scrolls with it.
            let scroll = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta).y);
            let wheeled = selected.filter(|&i| layers[i].live).or_else(|| pointer.and_then(picture_at));
            if let Some(i) = wheeled.filter(|_| scroll != 0.0 && self.drag.is_none() && self.wheel == WheelMode::Scale) {
                let [w, h] = layers[i].size;
                let factor = (f64::from(scroll) * WHEEL_SPEED).exp();
                change.changed = Some((i, layers[i].transform.scaled(w, h, outline, pixels, factor).clamped(w, h, outline)));
                change.picked = Some(i);
                change.began = true;
                self.view.hold_until = now + WHEEL_HOLD_SECONDS;
            }
            let mut zoom = f64::from(ui.input(|i| i.zoom_delta()));
            if self.wheel == WheelMode::Zoom {
                zoom *= (f64::from(scroll) * WHEEL_ZOOM_SPEED).exp();
            }
            if let Some(pointer) = pointer.filter(|_| zoom != 1.0) {
                self.view.zoomed = Some(self.view.zoomed.unwrap_or(look).zoomed_at(area.center(), pointer, zoom, frame_fit));
                ui.ctx().request_repaint();
            }
        }

        // The menu follows the pointer: resting on the dot opens it, going away closes it. From here
        // on it goes by where it is now.
        if let (Some((top, _)), Some((origin, _))) = (mark, &layout) {
            let on_mark = pointer.is_some_and(|p| p.distance(top) <= TOP_RADIUS + 3.0);
            let near = pointer.is_some_and(|p| p.distance(*origin) <= MENU_RADIUS + MENU_ITEM_RADIUS + 8.0 || p.distance(top) <= TOP_RADIUS + 8.0);
            if let Some(wait) = self.menu.update(now, on_mark, near, self.drag.is_some()) {
                ui.ctx().request_repaint_after(Duration::from_secs_f64(wait));
            }
        }
        let layout = layout_of(&self.menu);
        let hovered_item = pointer.and_then(|p| layout.as_ref().filter(|_| menu_shown && self.menu.open).and_then(|(_, items)| items.iter().position(|item| item.distance(p) <= MENU_ITEM_RADIUS)));

        let hovered = pointer.filter(|_| self.drag.is_none() && hovered_item.is_none()).and_then(target_at);
        if response.hovered() || self.drag.is_some() {
            let icon = match self.drag {
                Some(Drag::Move { .. } | Drag::Turn { .. } | Drag::Pan { .. }) => CursorIcon::Grabbing,
                Some(Drag::Resize { fixed, press, .. }) => resize_cursor(vec2((press[0] - fixed[0]) as f32, (press[1] - fixed[1]) as f32)),
                Some(Drag::Reshape { side, .. }) => resize_cursor(side),
                None if hovered_item.is_some() => CursorIcon::PointingHand,
                None => match hovered {
                    Some(Target::FrameSide(side)) => resize_cursor(side),
                    Some(Target::Handle(Handle::Arrow(_))) => CursorIcon::Grab,
                    Some(Target::Handle(Handle::Top)) => CursorIcon::PointingHand,
                    Some(Target::Handle(handle)) => handles.as_ref().map_or(CursorIcon::Default, |handles| resize_cursor(handles.direction(handle))),
                    Some(Target::Picture(_)) => CursorIcon::Move,
                    None => CursorIcon::Default,
                },
            };
            ui.ctx().set_cursor_icon(icon);
        }
        if let Some(item) = hovered_item {
            egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), ui.id().with("picture menu hint"), |ui| {
                ui.label(MenuItem::ALL[item].hint());
            });
        }

        let painter = ui.painter_at(area);
        // The selected picture whole and faint: what of it is past the frame shows so.
        if let Some(layer) = selected.map(|i| &layers[i]).filter(|_| editing) {
            if let Some(texture) = &layer.texture {
                painter.add(placed_picture(texture, frame, layer.size, &layer.transform, outline, Color32::WHITE.gamma_multiply(PAST_OPACITY)));
            }
        }
        // The result, in the frame. A circle cuts the pictures to itself: past it, around the circle
        // and in the corners of its square alike, the selected picture shows faint.
        let inside = painter.with_clip_rect(frame.intersect(area));
        let circle = (outline.mask == Mask::Circle).then(|| circle_points(frame.center(), frame.width() / 2.0));
        let cut = circle.as_deref();
        paint_area(&inside, frame, cut, Color32::BLACK);
        for layer in layers.iter().filter(|layer| layer.apart.is_none()) {
            paint_layer(&inside, &board, layer, cut);
        }
        let apart = selected.filter(|&i| editing && layers[i].apart.is_some());
        if let Some(layer) = apart.map(|i| &layers[i]) {
            if let Some(texture) = &layer.texture {
                inside.add(picture_shape(&board, layer, texture, Color32::WHITE, cut));
            }
            let quad = clockwise(board.corners(layer));
            let veil = cut.map_or_else(|| quad.to_vec(), |circle| clip_to_quad(circle, quad));
            if veil.len() >= 3 {
                inside.add(Shape::convex_polygon(veil, VEIL, Stroke::NONE));
            }
        }

        let hot_side = match (self.drag, hovered) {
            (Some(Drag::Reshape { side, .. }), _) | (None, Some(Target::FrameSide(side))) => Some(side),
            _ => None,
        };
        for reference in &scene.frames[1..] {
            paint_reference(&painter, frame, reference);
        }
        paint_main(&painter, frame, main, hot_side);
        paint_snap_lines(&painter, frame, self.snapped);

        // The picture a press would pick, and the selected one with its handles.
        if let Some(Target::Picture(i)) = hovered.filter(|_| editing) {
            if Some(i) != selected {
                painter.add(Shape::closed_line(board.corners(&layers[i]).to_vec(), Stroke::new(1.0, Color32::from_white_alpha(140))));
            }
        }
        if let Some(handles) = &handles {
            let hot = match (self.drag, hovered) {
                (Some(Drag::Resize { handle, .. } | Drag::Turn { handle, .. }), _) | (None, Some(Target::Handle(handle))) => Some(handle),
                _ => None,
            };
            paint_handles(&painter, handles, hot.or(self.menu.open.then_some(Handle::Top)));
        }
        if let (Some(drag), Some(p), true) = (self.drag, response.interact_pointer_pos(), self.under_way) {
            let text = match drag {
                Drag::Resize { layer, .. } => change.changed.map(|(_, t)| {
                    let [w, h] = layers[layer].size;
                    format!("Scale {:.1}%", 100.0 * t.picture_scale(w, h, outline, pixels))
                }),
                Drag::Turn { .. } => change.changed.map(|(_, t)| format!("{:.1}°", t.angle_deg)),
                Drag::Reshape { .. } => change.aspect.map(|aspect| preset::Ratio::from_aspect(aspect).label()),
                Drag::Move { .. } | Drag::Pan { .. } => None,
            };
            if let Some(text) = text {
                paint_label(&painter, p + vec2(16.0, 16.0), text, Color32::WHITE, None);
            }
        }

        if let (Some((origin, items)), true) = (&layout, menu_visibility > 0.0) {
            let bars = selected.is_some_and(|i| layers[i].transform.bars);
            paint_menu(&painter, *origin, items, menu_visibility, hovered_item, bars);
        }

        let words = FontId::proportional(15.0);
        if let Some(note) = apart.and_then(|i| layers[i].apart) {
            let at = pos2(frame.center().x, frame.top().max(area.top()) + 10.0);
            painter.text(at, Align2::CENTER_TOP, note, words.clone(), Color32::from_white_alpha(210));
        }
        if let Some(text) = scene.empty {
            painter.text(frame.center(), Align2::CENTER_CENTER, text, words, Color32::GRAY);
        } else if selected.is_some_and(|i| editing && layers[i].texture.is_none()) {
            painter.text(frame.center(), Align2::CENTER_CENTER, "Preparing frames…", words, Color32::GRAY);
        }

        let painter = ui.painter_at(strip);
        paint_tag(&painter, Align2::LEFT_CENTER.anchor_size(strip.left_center() + vec2(8.0, 0.0), tag_size(&painter, scene.caption)), scene.caption, ui.visuals().weak_text_color(), TAG_BACKGROUND);
        let zoomed = self.view.zoomed.is_some();
        let zoom_text = if zoomed { format!("Zoom {:.0}%", look.scale / frame_fit * 100.0) } else { "Fit".to_owned() };
        let tag = Align2::RIGHT_CENTER.anchor_size(strip.right_center() - vec2(8.0, 0.0), tag_size(&painter, &zoom_text));
        let tag_response = ui.interact(tag, ui.id().with("zoom tag"), if zoomed { Sense::click() } else { Sense::hover() });
        let fill = if zoomed && tag_response.hovered() { Color32::from_gray(70) } else { TAG_BACKGROUND };
        paint_tag(&painter, tag, &zoom_text, Color32::WHITE, fill);
        // What the wheel does, a switch of two in front of the tag: each says what it does.
        let mut right = tag.left() - 8.0;
        for mode in [WheelMode::Zoom, WheelMode::Scale] {
            let rect = Align2::RIGHT_CENTER.anchor_size(pos2(right, strip.center().y), tag_size(&painter, mode.name()));
            let response = ui.interact(rect, ui.id().with(("wheel mode", mode.name())), Sense::click());
            let on = self.wheel == mode;
            let fill = match (on, response.hovered()) {
                (true, _) => ui.visuals().selection.bg_fill,
                (false, true) => Color32::from_gray(70),
                (false, false) => TAG_BACKGROUND,
            };
            paint_tag(&painter, rect, mode.name(), Color32::WHITE, fill);
            if response.on_hover_text(mode.hint()).clicked() {
                self.wheel = mode;
            }
            right = rect.left() - 2.0;
        }
        painter.text(pos2(right - 4.0, strip.center().y), Align2::RIGHT_CENTER, "Wheel", FontId::proportional(12.0), ui.visuals().weak_text_color());
        let hint = if zoomed {
            "Click: back to Fit, the frame in the middle with all of the selected picture around it"
        } else {
            "Fit: the frame in the middle with all of the selected picture around it. Ctrl+wheel or a pinch zooms in at the pointer, the right or the middle button moves the view"
        };
        if tag_response.on_hover_text(hint).clicked() {
            self.view.zoomed = None;
            ui.ctx().request_repaint();
        }
        change
    }
}

/// The cursor for pulling something `towards` a direction on the screen.
fn resize_cursor(towards: Vec2) -> CursorIcon {
    let eighths = (towards.y.atan2(towards.x) / TAU * 8.0).round() as i32;
    match eighths.rem_euclid(4) {
        0 => CursorIcon::ResizeHorizontal,
        1 => CursorIcon::ResizeNwSe,
        2 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeNeSw,
    }
}

/// The pointer's direction from `center`, in degrees clockwise on the screen.
fn screen_angle(center: Pos2, p: Pos2) -> f64 {
    f64::from(p.y - center.y).atan2(f64::from(p.x - center.x)).to_degrees()
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

/// The corners in clockwise order on the screen, as a filled shape wants them: a mirrored picture's
/// go round the other way.
fn clockwise(mut corners: [Pos2; 4]) -> [Pos2; 4] {
    let area: f32 = (0..4).map(|i| corners[i].x * corners[(i + 1) % 4].y - corners[(i + 1) % 4].x * corners[i].y).sum();
    if area < 0.0 {
        corners.reverse();
    }
    corners
}

/// The points of a circle on the screen, clockwise: close enough to it that the edge of a picture cut
/// to them is within a pixel of the circle drawn, however close the view comes.
fn circle_points(center: Pos2, radius: f32) -> Vec<Pos2> {
    const STEPS: usize = 256;
    (0..STEPS)
        .map(|i| {
            let (sin, cos) = (i as f32 / STEPS as f32 * TAU).sin_cos();
            center + radius * vec2(cos, sin)
        })
        .collect()
}

/// The part of the convex `subject` inside the convex `quad`, whose corners go clockwise on the
/// screen. Nothing for a quad of no size.
fn clip_to_quad(subject: &[Pos2], quad: [Pos2; 4]) -> Vec<Pos2> {
    let cross = |a: Vec2, b: Vec2| a.x * b.y - a.y * b.x;
    let area: f32 = (0..4).map(|i| cross(quad[i].to_vec2(), quad[(i + 1) % 4].to_vec2())).sum();
    if area.abs() < 1e-3 {
        return Vec::new();
    }
    let mut points = subject.to_vec();
    for i in 0..4 {
        let (a, side) = (quad[i], quad[(i + 1) % 4] - quad[i]);
        let mut kept = Vec::with_capacity(points.len() + 1);
        for (j, &p) in points.iter().enumerate() {
            let q = points[(j + 1) % points.len()];
            let (from, to) = (cross(side, p - a), cross(side, q - a));
            if from >= 0.0 {
                kept.push(p);
            }
            // Where the edge from p to q crosses the side.
            if (from >= 0.0) != (to >= 0.0) {
                kept.push(p + (q - p) * (from / (from - to)));
            }
        }
        points = kept;
        if points.is_empty() {
            break;
        }
    }
    points
}

/// All of `frame` filled with `color`, or only the circle `cut` out of it.
fn paint_area(painter: &Painter, frame: Rect, cut: Option<&[Pos2]>, color: Color32) {
    match cut {
        Some(circle) => {
            painter.add(Shape::convex_polygon(circle.to_vec(), color, Stroke::NONE));
        }
        None => {
            painter.rect_filled(frame, 0.0, color);
        }
    }
}

/// A layer on the result: while it fades it is that much of itself over what is under it, and one
/// with bars darkens all of the frame that much first. In a circle `cut`, only what is in it.
fn paint_layer(painter: &Painter, board: &Board, layer: &Layer, cut: Option<&[Pos2]>) {
    let opacity = layer.opacity.clamp(0.0, 1.0);
    if layer.transform.bars {
        paint_area(painter, board.frame(), cut, Color32::from_black_alpha((opacity * 255.0).round() as u8));
    }
    let Some(texture) = &layer.texture else { return };
    let level = (opacity * 255.0).round() as u8;
    let tint = match (opacity < 1.0, layer.transform.bars) {
        (false, _) => Color32::WHITE,
        (true, true) => Color32::from_rgba_premultiplied(level, level, level, 0),
        (true, false) => Color32::WHITE.gamma_multiply(opacity),
    };
    painter.add(picture_shape(board, layer, texture, tint, cut));
}

/// A layer's picture where it is on the result, tinted with `tint`: all of it, or what of it is in
/// the circle `cut`.
fn picture_shape(board: &Board, layer: &Layer, texture: &TextureHandle, tint: Color32, cut: Option<&[Pos2]>) -> Shape {
    let Some(circle) = cut else {
        return placed_picture(texture, board.frame(), layer.size, &layer.transform, board.outline, tint);
    };
    let points = clip_to_quad(circle, clockwise(board.corners(layer)));
    let mut mesh = Mesh::with_texture(texture.id());
    for &pos in &points {
        mesh.vertices.push(Vertex { pos, uv: board.uv(layer, pos), color: tint });
    }
    for i in 2..points.len() {
        mesh.add_triangle(0, i as u32 - 1, i as u32);
    }
    Shape::mesh(mesh)
}

/// The whole picture of a source, placed where its frame sees it in `rect`, tinted with `color`.
fn placed_picture(texture: &TextureHandle, rect: Rect, size: [f64; 2], t: &Transform, outline: Outline, color: Color32) -> Shape {
    let [w, h] = size;
    let pixels_per_source = f64::from(rect.width()) / t.size(w, h, outline)[0];
    let mut mesh = Mesh::with_texture(texture.id());
    for [x, y] in [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]] {
        let [u, v] = t.source_to_output(w, h, [x, y]);
        let pos = rect.center() + vec2((u * pixels_per_source) as f32, (v * pixels_per_source) as f32);
        let uv = pos2((x / w) as f32, (y / h) as f32);
        mesh.vertices.push(Vertex { pos, uv, color });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    Shape::mesh(mesh)
}

/// The main frame: its outline, the side `hot` thicker, its guides, its label with the lock.
fn paint_main(painter: &Painter, frame: Rect, overlay: &FrameOverlay, hot: Option<Vec2>) {
    paint_guides(painter, frame, overlay);
    let backing = Stroke::new(3.5, Color32::from_black_alpha(110));
    let line = Stroke::new(2.0, overlay.color);
    match overlay.outline.mask {
        Mask::Circle => {
            painter.rect_stroke(frame, 0.0, Stroke::new(1.0, Color32::from_white_alpha(60)));
            painter.circle_stroke(frame.center(), frame.width() / 2.0, backing);
            painter.circle_stroke(frame.center(), frame.width() / 2.0, line);
        }
        Mask::Rect => {
            painter.rect_stroke(frame, 0.0, backing);
            painter.rect_stroke(frame, 0.0, line);
            if let Some(side) = hot {
                painter.line_segment(side_ends(frame, side), Stroke::new(4.0, overlay.color));
            }
        }
    }
    let label_at = match overlay.outline.mask {
        Mask::Circle => frame.left_top() - vec2(0.0, 20.0),
        Mask::Rect => frame.left_top() + vec2(3.0, 3.0),
    };
    paint_label(painter, label_at, overlay.label.clone(), overlay.color, Some(overlay.free_ratio));
}

/// The ends of the side of `frame` that `side` points out of.
fn side_ends(frame: Rect, side: Vec2) -> [Pos2; 2] {
    if side.x > 0.0 {
        [frame.right_top(), frame.right_bottom()]
    } else if side.x < 0.0 {
        [frame.left_top(), frame.left_bottom()]
    } else if side.y < 0.0 {
        [frame.left_top(), frame.right_top()]
    } else {
        [frame.left_bottom(), frame.right_bottom()]
    }
}

/// A frame for comparison: dashed, inside the main frame, with the same middle.
fn paint_reference(painter: &Painter, frame: Rect, overlay: &FrameOverlay) {
    let [w, h] = overlay.outline.fit(f64::from(frame.width()), f64::from(frame.height()));
    let rect = Rect::from_center_size(frame.center(), vec2(w as f32, h as f32));
    let line: Vec<Pos2> = match overlay.outline.mask {
        Mask::Circle => (0..=72)
            .map(|i| {
                let (sin, cos) = (i as f32 / 72.0 * TAU).sin_cos();
                rect.center() + rect.width() / 2.0 * vec2(cos, sin)
            })
            .collect(),
        Mask::Rect => vec![rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()],
    };
    painter.extend(Shape::dashed_line(&line, Stroke::new(1.3, overlay.color), 6.0, 5.0));
    paint_guides(painter, rect, overlay);
    paint_label(painter, rect.left_top() + vec2(3.0, 3.0), overlay.label.clone(), overlay.color, None);
}

fn paint_guides(painter: &Painter, rect: Rect, overlay: &FrameOverlay) {
    if overlay.thirds {
        let stroke = Stroke::new(1.0, overlay.color.gamma_multiply(0.7));
        for k in [1.0 / 3.0, 2.0 / 3.0] {
            let (x, y) = (rect.left() + rect.width() * k, rect.top() + rect.height() * k);
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], stroke);
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        }
    }
    if overlay.center {
        let (middle, arm) = (rect.center(), 8.0);
        let stroke = Stroke::new(1.2, overlay.color);
        painter.line_segment([middle - vec2(arm, 0.0), middle + vec2(arm, 0.0)], stroke);
        painter.line_segment([middle - vec2(0.0, arm), middle + vec2(0.0, arm)], stroke);
    }
    if let Some(safe) = overlay.safe {
        let (w, h) = (rect.width(), rect.height());
        let inner = Rect::from_min_max(rect.min + vec2(safe.left as f32 * w, safe.top as f32 * h), rect.max - vec2(safe.right as f32 * w, safe.bottom as f32 * h));
        let line = [inner.left_top(), inner.right_top(), inner.right_bottom(), inner.left_bottom(), inner.left_top()];
        painter.extend(Shape::dashed_line(&line, Stroke::new(1.0, overlay.color), 3.0, 3.0));
        painter.text(inner.left_top() + vec2(4.0, 3.0), Align2::LEFT_TOP, "safe", FontId::proportional(11.0), overlay.color);
    }
}

fn paint_snap_lines(painter: &Painter, frame: Rect, snapped: [Option<f32>; 2]) {
    let line = Stroke::new(1.0, SNAP_LINE);
    if let Some(x) = snapped[0] {
        painter.line_segment([pos2(x, frame.top()), pos2(x, frame.bottom())], line);
    }
    if let Some(y) = snapped[1] {
        painter.line_segment([pos2(frame.left(), y), pos2(frame.right(), y)], line);
    }
}

/// The selected picture's outline, the squares at its corners and in the middles of its sides, and
/// the arrows at its corners; `hot` larger.
fn paint_handles(painter: &Painter, handles: &Handles, hot: Option<Handle>) {
    let outline = handles.corners.to_vec();
    painter.add(Shape::closed_line(outline.clone(), Stroke::new(3.0, Color32::from_black_alpha(110))));
    painter.add(Shape::closed_line(outline, Stroke::new(1.5, flash::COLOR)));
    let square = |at: Pos2, hot: bool| {
        let size = if hot { HANDLE_SIZE + 3.0 } else { HANDLE_SIZE };
        painter.rect(Rect::from_center_size(at, vec2(size, size)), 1.0, Color32::WHITE, Stroke::new(1.5, flash::COLOR));
    };
    for (i, &corner) in handles.corners.iter().enumerate() {
        square(corner, hot == Some(Handle::Corner(i)));
    }
    for (i, side) in handles.sides.iter().enumerate() {
        if let Some(side) = *side {
            square(side, hot == Some(Handle::Side(i)));
        }
    }
    for (i, &arrow) in handles.arrows.iter().enumerate() {
        paint_arrow(painter, arrow, handles.center, flash::COLOR, hot == Some(Handle::Arrow(i)));
    }
    let width = if hot == Some(Handle::Top) { 3.0 } else { 2.0 };
    painter.circle(handles.top, TOP_RADIUS, Color32::from_gray(16), Stroke::new(width, flash::COLOR));
}

/// The menu's items, sliding out of `origin` as `visibility` goes from 0 to 1. Bars stands out while
/// the picture has bars.
fn paint_menu(painter: &Painter, origin: Pos2, items: &[Pos2], visibility: f32, hovered: Option<usize>, bars: bool) {
    for (i, (item, at)) in MenuItem::ALL.into_iter().zip(items).enumerate() {
        let on = item == MenuItem::Bars && bars;
        let fill = match (on, hovered == Some(i)) {
            (true, _) => flash::COLOR.gamma_multiply(0.75),
            (false, true) => Color32::from_gray(70),
            (false, false) => Color32::from_gray(24),
        };
        let at = origin.lerp(*at, visibility);
        let stroke = Stroke::new(if hovered == Some(i) { 2.0 } else { 1.5 }, flash::COLOR.gamma_multiply(visibility));
        painter.circle(at, MENU_ITEM_RADIUS, fill.gamma_multiply(visibility), stroke);
        let text = if on { Color32::BLACK } else { Color32::WHITE };
        painter.text(at, Align2::CENTER_CENTER, item.label(), FontId::proportional(12.0), text.gamma_multiply(visibility));
    }
}

/// The color of a lock: an open one should catch the eye.
pub fn lock_color(free: bool) -> Color32 {
    if free {
        flash::COLOR
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

/// How large a tag with `text` is.
fn tag_size(painter: &Painter, text: &str) -> Vec2 {
    painter.layout_no_wrap(text.to_owned(), FontId::proportional(12.0), Color32::WHITE).size() + vec2(8.0, 4.0)
}

/// A tag at the edge of the view: `text` on `fill`.
fn paint_tag(painter: &Painter, rect: Rect, text: &str, color: Color32, fill: Color32) {
    painter.rect_filled(rect, 3.0, fill);
    painter.text(rect.left_center() + vec2(4.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(12.0), color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::geometry::Anchor;
    use egui::{Event, Modifiers, MouseWheelUnit};

    const W: f64 = 1920.0;
    const H: f64 = 1080.0;
    const WIDE: Outline = Outline { aspect: 16.0 / 9.0, mask: Mask::Rect };
    const PIXELS: [f64; 2] = [1920.0, 1080.0];

    fn layer(transform: Transform) -> Layer {
        Layer { texture: None, size: [W, H], transform, opacity: 1.0, live: true, apart: None, key: None }
    }

    /// A quarter of the frame across, 60 px in from its right edge and 30 px down from its top.
    fn corner_picture(anchor: Anchor) -> Transform {
        Transform { anchor, ..Default::default() }.placed(W, H, WIDE, PIXELS, [600.0, -300.0], 480.0)
    }

    /// The result half its size on the screen, its middle at 500, 300.
    fn board() -> Board {
        Board { center: pos2(500.0, 300.0), scale: 0.5, outline: WIDE, pixels: PIXELS }
    }

    fn assert_near(a: [f64; 2], b: [f64; 2]) {
        assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6, "{a:?} != {b:?}");
    }

    #[test]
    fn finds_the_handles_of_the_selected_picture() {
        // On the screen from 680, 82.5 to 920, 217.5.
        let handles = Handles::of(board().corners(&layer(corner_picture(Anchor::TopRight))));
        assert_eq!(handles.at(pos2(681.0, 83.0)), Some(Handle::Corner(0)));
        assert_eq!(handles.at(pos2(918.0, 216.0)), Some(Handle::Corner(2)));
        assert_eq!(handles.at(pos2(800.0, 84.0)), Some(Handle::Side(0)));
        assert_eq!(handles.at(pos2(921.0, 150.0)), Some(Handle::Side(1)));
        assert_eq!(handles.at(pos2(680.0 - 14.0, 82.5 - 14.0)), Some(Handle::Arrow(0)));
        assert_eq!(handles.at(pos2(920.0 + 14.0, 217.5 + 14.0)), Some(Handle::Arrow(2)));
        assert_eq!(handles.at(pos2(800.0, 150.0)), None, "inside, it is moved");
        // A small picture has only its corners: in the middle of its sides it is moved.
        let small = Transform { anchor: Anchor::Center, ..Default::default() }.placed(W, H, WIDE, PIXELS, [0.0, 0.0], 60.0);
        let handles = Handles::of(board().corners(&layer(small)));
        assert!(handles.sides.iter().all(Option::is_none));
    }

    #[test]
    fn the_handles_of_a_turned_and_mirrored_picture_point_out_of_it() {
        let t = Transform { angle_deg: 30.0, mirror: true, ..corner_picture(Anchor::TopRight) };
        let corners = board().corners(&layer(t));
        let handles = Handles::of(corners);
        for (i, (&arrow, corner)) in handles.arrows.iter().zip(corners).enumerate() {
            assert!(arrow.distance(handles.center) > corner.distance(handles.center) + 10.0, "{i}");
            assert_eq!(handles.at(arrow), Some(Handle::Arrow(i)));
        }
        assert!(matches!(resize_cursor(handles.direction(Handle::Side(1))), CursorIcon::ResizeHorizontal | CursorIcon::ResizeNwSe | CursorIcon::ResizeNeSw));
    }

    #[test]
    fn a_picture_is_hit_on_itself_and_one_with_bars_anywhere_in_the_frame() {
        let board = board();
        let small = layer(corner_picture(Anchor::TopRight));
        assert!(board.hits(&small, pos2(800.0, 150.0)));
        assert!(!board.hits(&small, pos2(500.0, 300.0)));
        let barred = layer(Transform { bars: true, ..corner_picture(Anchor::TopRight) });
        assert!(board.hits(&barred, pos2(500.0, 300.0)));
        assert!(!board.hits(&barred, pos2(10.0, 300.0)), "past the frame only its picture");
    }

    /// Twice the area of a polygon, by the shoelace.
    fn area(points: &[Pos2]) -> f32 {
        (0..points.len()).map(|i| points[i].x * points[(i + 1) % points.len()].y - points[(i + 1) % points.len()].x * points[i].y).sum::<f32>() / 2.0
    }

    #[test]
    fn a_picture_is_cut_to_a_circle() {
        let square = |x0: f32, y0: f32, x1: f32, y1: f32| [pos2(x0, y0), pos2(x1, y0), pos2(x1, y1), pos2(x0, y1)];
        // A square cut by another: their overlap, whichever way the cutting one goes round.
        let cut = clip_to_quad(&square(0.0, 0.0, 10.0, 10.0), square(5.0, 5.0, 15.0, 15.0));
        assert!((area(&cut) - 25.0).abs() < 1e-3, "{cut:?}");
        assert!(cut.iter().all(|p| (5.0..=10.0).contains(&p.x) && (5.0..=10.0).contains(&p.y)));
        let mut backwards = square(5.0, 5.0, 15.0, 15.0);
        backwards.reverse();
        assert!((area(&clip_to_quad(&square(0.0, 0.0, 10.0, 10.0), clockwise(backwards))) - 25.0).abs() < 1e-3);

        // A picture over all of the circle leaves it whole; one past its side, a part.
        let circle = circle_points(pos2(100.0, 100.0), 50.0);
        let whole = area(&circle);
        assert!((area(&clip_to_quad(&circle, square(0.0, 0.0, 200.0, 200.0))) - whole).abs() < 1e-2);
        let half = area(&clip_to_quad(&circle, square(100.0, 0.0, 300.0, 300.0)));
        assert!((half - whole / 2.0).abs() < 1.0, "{half} of {whole}");
        assert!(clip_to_quad(&circle, square(200.0, 200.0, 300.0, 300.0)).is_empty());
        assert!(clip_to_quad(&circle, [pos2(100.0, 100.0); 4]).is_empty(), "a picture of no size");
    }

    #[test]
    fn a_point_of_a_picture_is_found_in_it() {
        let board = board();
        let picture = layer(corner_picture(Anchor::TopRight));
        // On the screen from 680, 82.5 to 920, 217.5.
        let uv = board.uv(&picture, pos2(680.0, 82.5));
        assert!(uv.x.abs() < 1e-4 && uv.y.abs() < 1e-4, "{uv:?}");
        let uv = board.uv(&picture, pos2(800.0, 150.0));
        assert!((uv.x - 0.5).abs() < 1e-4 && (uv.y - 0.5).abs() < 1e-4, "{uv:?}");
        // Mirrored in its place: its left edge on the screen is the right one of the picture.
        let mirrored = layer(corner_picture(Anchor::TopRight).flipped(W, H));
        assert!((board.uv(&mirrored, pos2(680.0, 82.5)).x - 1.0).abs() < 1e-4);
    }

    #[test]
    fn the_frame_takes_a_press_near_a_side() {
        let frame = board().frame();
        assert_eq!(frame_side(frame, pos2(frame.right() + 3.0, 300.0)), Some(vec2(1.0, 0.0)));
        assert_eq!(frame_side(frame, pos2(400.0, frame.top() - 5.0)), Some(vec2(0.0, -1.0)));
        assert_eq!(frame_side(frame, pos2(500.0, 300.0)), None);
        assert_eq!(frame_side(frame, pos2(frame.right() + 3.0, frame.bottom() + 30.0)), None);
    }

    #[test]
    fn fit_has_the_frame_in_the_middle_and_all_of_the_picture_around_it() {
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0));
        let room = [1000.0 - 2.0 * f64::from(MARGIN), 600.0 - 2.0 * f64::from(MARGIN)];
        let frame_only = fit_look(area, PIXELS, None);
        assert_eq!(frame_only.at, [0.0, 0.0]);
        assert!((frame_only.scale - (room[0] / W).min(room[1] / H)).abs() < 1e-12);

        // Its right half past the frame's right edge: the view is as wide as twice that, the frame
        // still in its middle.
        let past = layer(Transform { anchor: Anchor::Right, ..Default::default() }.placed(W, H, WIDE, PIXELS, [960.0, 0.0], 1920.0));
        let look = fit_look(area, PIXELS, Some(corners_px(&past, WIDE, PIXELS)));
        assert_eq!(look.at, [0.0, 0.0]);
        assert!((look.scale - (room[0] / (2.0 * W)).min(room[1] / H)).abs() < 1e-12);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let middle = pos2(500.0, 300.0);
        let look = Look { scale: 0.5, at: [100.0, -40.0] };
        let pointer = pos2(700.0, 200.0);
        let under = |look: Look| [look.at[0] + 200.0 / look.scale, look.at[1] - 100.0 / look.scale];
        let closer = look.zoomed_at(middle, pointer, 2.0, 0.5);
        assert!((closer.scale - 1.0).abs() < 1e-12);
        assert_near(under(closer), under(look));
        // No farther back and no closer than the view goes.
        assert!((look.zoomed_at(middle, pointer, 1e-6, 0.5).scale - 0.5 * MIN_VIEW_ZOOM).abs() < 1e-12);
        assert!((look.zoomed_at(middle, pointer, 1e6, 0.5).scale - 0.5 * MAX_VIEW_ZOOM).abs() < 1e-12);
    }

    #[test]
    fn the_view_eases_to_what_fits_and_holds_still_during_a_gesture() {
        let mut view = View::default();
        let area = vec2(1000.0, 600.0);
        let first = Look { scale: 0.5, at: [0.0, 0.0] };
        assert_eq!(view.next(first, area, 0.0, 0.016, false), first);
        let wider = Look { scale: 0.25, ..first };
        assert_eq!(view.next(wider, area, 0.1, 0.016, true), first, "held");
        let step = view.next(wider, area, 0.2, 0.016, false);
        assert!(step.scale < 0.5 && step.scale > 0.25, "{step:?}");
        for i in 0..100 {
            view.next(wider, area, 0.3 + f64::from(i) * 0.016, 0.016, false);
        }
        assert_eq!(view.shown, Some(wider));
        // A new size of the area is followed at once.
        assert_eq!(view.next(first, vec2(800.0, 600.0), 2.0, 0.016, false), first);
    }

    #[test]
    fn moving_snaps_the_edges_to_the_frames_unless_told_not_to() {
        let board = board();
        let start = corner_picture(Anchor::TopRight);
        let picture = layer(start);
        // 10 px on the screen are 20 of the result.
        let (next, lines) = moved(&board, &picture, start, vec2(10.0, 0.0), true);
        assert_near(next.placement(W, H, WIDE, PIXELS).center, [620.0, -300.0]);
        assert_eq!(lines, [None, None]);
        // Its right edge 4 px from the frame's: it goes there, and the line shows it.
        let (next, lines) = moved(&board, &picture, start, vec2(58.0, 0.0), true);
        assert_near(next.placement(W, H, WIDE, PIXELS).center, [720.0, -300.0]);
        assert_eq!(lines[0], Some(980.0));
        let (next, _) = moved(&board, &picture, start, vec2(58.0, 0.0), false);
        assert_near(next.placement(W, H, WIDE, PIXELS).center, [716.0, -300.0]);
    }

    #[test]
    fn a_corner_resizes_the_picture_with_the_opposite_one_staying() {
        let board = board();
        let start = corner_picture(Anchor::TopRight);
        let picture = layer(start);
        // From its bottom left corner a quarter farther out, along its diagonal.
        let corners = corners_px(&picture, WIDE, PIXELS);
        let next = resized(&board, &picture, start, corners[1], corners[3], [240.0, -97.5]);
        let place = next.placement(W, H, WIDE, PIXELS);
        assert_near(place.size, [600.0, 337.5]);
        assert_near(corners_px(&layer(next), WIDE, PIXELS)[1], corners[1]);
        assert_eq!(next.anchor, Anchor::TopRight, "the anchor is set by moving only");
    }

    #[test]
    fn a_side_resizes_the_picture_from_the_opposite_side() {
        let board = board();
        let start = corner_picture(Anchor::BottomLeft);
        let picture = layer(start);
        let corners = corners_px(&picture, WIDE, PIXELS);
        // The right side's middle pulled 120 px to the right: half as wide again, the left side's
        // middle where it was.
        let left = [(corners[3][0] + corners[0][0]) / 2.0, (corners[3][1] + corners[0][1]) / 2.0];
        let right = [(corners[1][0] + corners[2][0]) / 2.0, (corners[1][1] + corners[2][1]) / 2.0];
        let next = resized(&board, &picture, start, left, right, [right[0] + 240.0, right[1]]);
        assert_near(next.placement(W, H, WIDE, PIXELS).size, [720.0, 405.0]);
        let after = corners_px(&layer(next), WIDE, PIXELS);
        assert_near([(after[3][0] + after[0][0]) / 2.0, (after[3][1] + after[0][1]) / 2.0], left);
    }

    #[test]
    fn the_arrows_turn_a_pinned_picture_in_its_place() {
        let board = board();
        let start = corner_picture(Anchor::TopRight);
        let picture = layer(start);
        let pivot = board.screen(start.placement(W, H, WIDE, PIXELS).center);
        let press = pivot + vec2(100.0, 0.0);
        // A quarter turn clockwise on the screen.
        let next = turned(&board, &picture, start, pivot, screen_angle(pivot, press), pivot + vec2(0.0, 100.0), false);
        assert!((next.angle_deg - 90.0).abs() < 1e-9, "{}", next.angle_deg);
        assert_near(next.placement(W, H, WIDE, PIXELS).center, [600.0, -300.0]);
        // With Shift, by 15°.
        let near = pivot + 100.0 * vec2(20f32.to_radians().cos(), 20f32.to_radians().sin());
        assert!((turned(&board, &picture, start, pivot, 0.0, near, true).angle_deg - 15.0).abs() < 1e-9);
    }

    #[test]
    fn a_side_of_the_frame_changes_its_proportions() {
        let center = pos2(500.0, 300.0);
        // The frame 960×540 on the screen; its right side pulled to 600 px from its middle.
        let aspect = reshaped(center, vec2(1.0, 0.0), 0.0, 540.0, pos2(1100.0, 300.0), false);
        assert!((aspect - 1200.0 / 540.0).abs() < 1e-9);
        // Near 16:9 it takes 16:9; the top side makes it taller.
        assert_eq!(reshaped(center, vec2(1.0, 0.0), 0.0, 540.0, pos2(500.0 + 481.0, 300.0), true), 16.0 / 9.0);
        let tall = reshaped(center, vec2(0.0, -1.0), 0.0, 960.0, pos2(500.0, 300.0 - 480.0), false);
        assert!((tall - 1.0).abs() < 1e-9);
    }

    fn overlay() -> FrameOverlay {
        FrameOverlay { outline: WIDE, color: Color32::WHITE, label: "16:9".into(), thirds: false, center: false, safe: None, free_ratio: false }
    }

    /// The canvas in a window 1000×700, one frame at `time` with these events.
    fn run(ctx: &egui::Context, canvas: &mut Canvas, scene: &Scene, time: f64, events: Vec<Event>, modifiers: Modifiers) -> CanvasChange {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0))), time: Some(time), events, modifiers, ..Default::default() };
        let mut change = CanvasChange::default();
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| change = canvas.show(ui, scene));
        });
        change
    }

    fn button(at: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    #[test]
    fn a_press_held_still_changes_nothing_and_a_drag_moves_the_picture() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let layers = [layer(Transform::default())];
        let frames = [overlay()];
        let scene = Scene { layers: &layers, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        let at = pos2(500.0, 350.0);
        let none = Modifiers::NONE;
        let mut changes = vec![run(&ctx, &mut canvas, &scene, 0.0, vec![], none), run(&ctx, &mut canvas, &scene, 0.1, vec![Event::PointerMoved(at)], none)];
        changes.push(run(&ctx, &mut canvas, &scene, 0.2, vec![button(at, true)], none));
        // Two seconds without moving: long enough for egui to call it a drag.
        for i in 0..20 {
            changes.push(run(&ctx, &mut canvas, &scene, 0.3 + f64::from(i) * 0.1, vec![Event::PointerMoved(at)], none));
        }
        changes.push(run(&ctx, &mut canvas, &scene, 2.4, vec![button(at, false)], none));
        assert!(changes.iter().all(|change| change.changed.is_none()), "{changes:?}");

        // Now it moves 40 px to the right.
        run(&ctx, &mut canvas, &scene, 3.0, vec![button(at, true)], none);
        let mut last = None;
        for i in 1..=8 {
            let change = run(&ctx, &mut canvas, &scene, 3.0 + f64::from(i) * 0.02, vec![Event::PointerMoved(at + vec2(5.0 * i as f32, 0.0))], none);
            last = change.changed.or(last);
            assert!(change.changed.is_none() || change.moved);
        }
        let (_, moved) = last.expect("it moved");
        let scale = (1000.0 - 2.0 * f64::from(MARGIN)) / W;
        let center = moved.placement(W, H, WIDE, PIXELS).center;
        assert!((center[0] - 40.0 / scale).abs() < 1e-3 && center[1].abs() < 1e-9, "{center:?}");
    }

    #[test]
    fn a_gesture_keeps_to_its_picture_when_the_pictures_under_it_come_into_sight() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let frames = [overlay()];
        let top = |transform| Layer { key: Some(7), ..layer(transform) };
        // The top picture covers the frame, so the one under it is not in the list; made smaller,
        // it no longer covers it, and the one under it comes first.
        let covering = [top(Transform::default())];
        let smaller = Transform { zoom: 0.8, ..Transform::default() };
        let both = [Layer { key: Some(3), ..layer(Transform::default()) }, top(smaller)];
        let before = Scene { layers: &covering, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        let after = Scene { layers: &both, selected: Some(1), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        // Its bottom right corner, as the view fits the frame in 1000×676 with room for the arrows.
        let scale = (1000.0 - 2.0 * MARGIN) / W as f32;
        let corner = pos2(500.0 + 960.0 * scale, 338.0 + 540.0 * scale);
        let none = Modifiers::NONE;
        run(&ctx, &mut canvas, &before, 0.0, vec![Event::PointerMoved(corner)], none);
        run(&ctx, &mut canvas, &before, 0.1, vec![button(corner, true)], none);
        let first = run(&ctx, &mut canvas, &before, 0.15, vec![Event::PointerMoved(corner - vec2(20.0, 11.0))], none);
        assert_eq!(first.changed.map(|(i, _)| i), Some(0));
        for i in 2..6 {
            let change = run(&ctx, &mut canvas, &after, 0.1 + f64::from(i) * 0.05, vec![Event::PointerMoved(corner - vec2(10.0 * i as f32, 5.6 * i as f32))], none);
            assert_eq!(change.changed.map(|(i, _)| i), Some(1), "the top picture, now second in the list");
        }
    }

    #[test]
    fn the_dot_stands_off_the_middle_of_the_pictures_top_side() {
        let board = board();
        // Upright: above the middle of its top side, which is at 800, 82.5.
        let handles = Handles::of(board.corners(&layer(corner_picture(Anchor::TopRight))));
        assert!((handles.top - pos2(800.0, 82.5 - TOP_OFF)).length() < 1e-3, "{:?}", handles.top);
        assert_eq!(handles.at(handles.top), Some(Handle::Top));
        assert_eq!(handles.at(pos2(800.0, 84.0)), Some(Handle::Side(0)), "the side keeps its own handle");
        // Turned a quarter round clockwise, its top faces right.
        let turned = Handles::of(board.corners(&layer(corner_picture(Anchor::TopRight).turned(W, H, 90.0))));
        let away = turned.top - turned.center;
        assert!(away.x > 0.0 && away.y.abs() < 1e-3, "{away:?}");
    }

    #[test]
    fn the_menu_fans_out_away_from_the_picture() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let (origin, items) = menu_layout(pos2(400.0, 200.0), Some(pos2(400.0, 400.0)), 4, area);
        assert_eq!(origin, pos2(400.0, 200.0));
        assert!(items.iter().all(|item| item.y < 200.0), "{items:?}");
        // From left to right, as the picture's top is up.
        assert!(items.windows(2).all(|pair| pair[0].x < pair[1].x), "{items:?}");
    }

    #[test]
    fn opened_by_its_key_the_menu_goes_all_round_the_pointer() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let (origin, items) = menu_layout(pos2(400.0, 300.0), None, 4, area);
        assert_eq!(origin, pos2(400.0, 300.0));
        // The first at the top, then round clockwise.
        assert!((items[0] - pos2(400.0, 300.0 - MENU_RADIUS)).length() < 1e-3, "{items:?}");
        assert!(items[1].x > 400.0 && items[2].y > 300.0 && items[3].x < 400.0, "{items:?}");
    }

    #[test]
    fn the_menu_stays_inside_the_area_and_its_items_do_not_touch() {
        let area = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 500.0));
        let marks = [pos2(500.0, 52.0), pos2(102.0, 300.0), pos2(898.0, 548.0), pos2(500.0, 300.0), pos2(101.0, 51.0)];
        for mark in marks {
            let fans = (0..16).map(|turn| {
                let (sin, cos) = (turn as f32 / 16.0 * TAU).sin_cos();
                Some(mark + 200.0 * vec2(cos, sin))
            });
            for from in fans.chain([None]) {
                let (_, items) = menu_layout(mark, from, MenuItem::ALL.len(), area);
                for (i, item) in items.iter().enumerate() {
                    assert!(area.shrink(MENU_ITEM_RADIUS).contains(*item), "{mark:?} from {from:?}: {item:?}");
                    for other in &items[i + 1..] {
                        assert!(item.distance(*other) > 2.0 * MENU_ITEM_RADIUS, "{mark:?} from {from:?}: {item:?} and {other:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_menu_opens_after_a_rest_and_closes_after_leaving() {
        let mut menu = Menu::default();
        // Passing over the dot does nothing.
        assert!(menu.update(0.0, true, true, false).is_some());
        assert_eq!(menu.update(0.1, false, false, false), None);
        assert!(!menu.open);

        // Resting on it does.
        menu.update(1.0, true, true, false);
        assert!(!menu.open);
        menu.update(1.0 + MENU_OPEN_DELAY + 0.01, true, true, false);
        assert!(menu.open);

        // Moving towards an item keeps it, a short trip outside too.
        menu.update(2.0, false, true, false);
        assert!(menu.update(3.0, false, false, false).is_some());
        menu.update(3.1, false, true, false);
        assert!(menu.open);

        // A longer one does not.
        menu.update(4.0, false, false, false);
        menu.update(4.0 + MENU_CLOSE_DELAY + 0.01, false, false, false);
        assert!(!menu.open);

        // Starting a drag closes it at once.
        menu.open = true;
        menu.update(5.0, true, true, true);
        assert!(!menu.open);
    }

    #[test]
    fn after_an_action_or_a_drag_the_menu_waits_for_the_pointer_to_leave_the_dot() {
        let mut menu = Menu { open: true, ..Menu::default() };
        menu.close();
        assert_eq!(menu.update(10.5, true, true, false), None);
        assert_eq!(menu.update(11.0, true, true, false), None);
        assert!(!menu.open);

        // A drag ending on the dot, as when pulling the top side by it.
        menu.update(11.1, true, true, true);
        assert_eq!(menu.update(11.2, true, true, false), None);
        assert_eq!(menu.update(11.4, true, true, false), None);
        assert!(!menu.open);

        menu.update(11.5, false, false, false);
        menu.update(12.0, true, true, false);
        menu.update(12.0 + MENU_OPEN_DELAY + 0.01, true, true, false);
        assert!(menu.open);
    }

    #[test]
    fn menu_items_do_what_they_say() {
        let outline = Outline { aspect: 9.0 / 16.0, mask: Mask::Rect };
        let start = Transform { cx: 0.4, zoom: 1.5, angle_deg: 10.0, mirror: true, ..Transform::default() };
        assert_eq!(MenuItem::Fit.apply(start, W, H, outline), (start.showing_whole_picture(W, H, outline), false));
        assert_eq!(MenuItem::Fill.apply(start, W, H, outline), (start.filling_frame(W, H, outline), false));
        // Bars keeps the menu open, so that it can be turned back.
        let (barred, stays) = MenuItem::Bars.apply(start, W, H, outline);
        assert!(barred.bars && stays);
        assert!(!MenuItem::Bars.apply(barred, W, H, outline).0.bars);
        assert_eq!(MenuItem::Reset.apply(start, W, H, outline), (Transform::default(), false));
    }

    /// The view the canvas fits in a window 1000×700 to `layers`, the last of them selected.
    fn fitted_board(layers: &[Layer]) -> Board {
        let area = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0 - TAG_STRIP));
        let picture = layers.last().map(|layer| corners_px(layer, WIDE, PIXELS));
        Board::of(area, fit_look(area, PIXELS, picture), WIDE, PIXELS)
    }

    #[test]
    fn a_click_on_the_dot_turns_the_picture_upright() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let layers = [Layer { key: Some(1), ..layer(corner_picture(Anchor::TopRight).turned(W, H, 30.0)) }];
        let frames = [overlay()];
        let scene = Scene { layers: &layers, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        run(&ctx, &mut canvas, &scene, 0.0, vec![], Modifiers::NONE);
        let dot = Handles::of(fitted_board(&layers).corners(&layers[0])).top;
        run(&ctx, &mut canvas, &scene, 0.5, vec![Event::PointerMoved(dot), button(dot, true)], Modifiers::NONE);
        let change = run(&ctx, &mut canvas, &scene, 0.55, vec![button(dot, false)], Modifiers::NONE);
        let (_, upright) = change.changed.expect("turned upright");
        assert!(upright.angle_deg.abs() < 1e-9, "{}", upright.angle_deg);
        assert_eq!(upright.anchor, Anchor::TopRight);
    }

    #[test]
    fn its_key_opens_the_menu_round_the_pointer_and_an_item_does_its_part() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let start = corner_picture(Anchor::TopRight);
        let layers = [Layer { key: Some(1), ..layer(start) }];
        let frames = [overlay()];
        let scene = Scene { layers: &layers, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        let at = pos2(300.0, 400.0);
        run(&ctx, &mut canvas, &scene, 0.0, vec![Event::PointerMoved(at)], Modifiers::NONE);
        canvas.toggle_menu();
        run(&ctx, &mut canvas, &scene, 0.1, vec![], Modifiers::NONE);
        assert!(canvas.menu.open && canvas.menu.at == Some(at), "{:?}", canvas.menu);

        // Fit is the first, at the top, once the menu has slid out.
        let fit = at - vec2(0.0, MENU_RADIUS);
        run(&ctx, &mut canvas, &scene, 0.4, vec![Event::PointerMoved(fit), button(fit, true)], Modifiers::NONE);
        let change = run(&ctx, &mut canvas, &scene, 0.45, vec![button(fit, false)], Modifiers::NONE);
        assert_eq!(change.changed.map(|(_, t)| t), Some(start.showing_whole_picture(W, H, WIDE).clamped(W, H, WIDE)));
        assert!(!canvas.menu.open, "an action closes it");

        // The key opens it, and closes it again.
        canvas.toggle_menu();
        run(&ctx, &mut canvas, &scene, 1.0, vec![], Modifiers::NONE);
        assert!(canvas.menu.open);
        canvas.toggle_menu();
        run(&ctx, &mut canvas, &scene, 1.1, vec![], Modifiers::NONE);
        assert!(!canvas.menu.open);
    }

    #[test]
    fn switched_to_zoom_the_wheel_zooms_the_view_and_leaves_the_picture_as_it_is() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let layers = [Layer { key: Some(1), ..layer(corner_picture(Anchor::TopRight)) }];
        let frames = [overlay()];
        let scene = Scene { layers: &layers, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        let at = pos2(300.0, 400.0);
        run(&ctx, &mut canvas, &scene, 0.0, vec![Event::PointerMoved(at)], Modifiers::NONE);
        canvas.toggle_wheel();
        assert_eq!(canvas.wheel, WheelMode::Zoom);
        let wheel = Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, 5.0), modifiers: Modifiers::NONE };
        let change = run(&ctx, &mut canvas, &scene, 0.1, vec![wheel], Modifiers::NONE);
        assert!(change.changed.is_none());
        let fit = fit_look(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0 - TAG_STRIP)), PIXELS, None).scale;
        assert!(canvas.view.zoomed.is_some_and(|look| look.scale > fit), "{:?}", canvas.view.zoomed);
        canvas.toggle_wheel();
        assert_eq!(canvas.wheel, WheelMode::Scale);
    }

    #[test]
    fn a_click_picks_the_topmost_picture_under_the_pointer() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let layers = [layer(Transform::default()), layer(corner_picture(Anchor::TopRight))];
        let frames = [overlay()];
        let scene = Scene { layers: &layers, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        run(&ctx, &mut canvas, &scene, 0.0, vec![], Modifiers::NONE);
        let scale = (1000.0 - 2.0 * f64::from(MARGIN)) / W;
        let click = |at: Pos2, time: f64, canvas: &mut Canvas| {
            run(&ctx, canvas, &scene, time, vec![Event::PointerMoved(at), button(at, true)], Modifiers::NONE);
            run(&ctx, canvas, &scene, time + 0.05, vec![button(at, false)], Modifiers::NONE).picked
        };
        let small = pos2(500.0 + (600.0 * scale) as f32, 350.0 - (300.0 * scale) as f32);
        assert_eq!(click(small, 1.0, &mut canvas), Some(1));
        assert_eq!(click(pos2(300.0, 400.0), 2.0, &mut canvas), Some(0));
    }

    #[test]
    fn the_wheel_resizes_the_selected_picture_from_its_anchor_and_ctrl_zooms_the_view() {
        let ctx = egui::Context::default();
        let mut canvas = Canvas::default();
        let start = corner_picture(Anchor::TopRight);
        let layers = [layer(start)];
        let frames = [overlay()];
        let scene = Scene { layers: &layers, selected: Some(0), frames: &frames, pixels: PIXELS, playing: false, caption: "", empty: None };
        let at = pos2(300.0, 400.0);
        run(&ctx, &mut canvas, &scene, 0.0, vec![Event::PointerMoved(at)], Modifiers::NONE);
        let wheel = |modifiers| Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, 5.0), modifiers };
        let change = run(&ctx, &mut canvas, &scene, 0.1, vec![wheel(Modifiers::NONE)], Modifiers::NONE);
        let (_, grown) = change.changed.expect("resized");
        assert!(!change.moved);
        let (before, after) = (start.placement(W, H, WIDE, PIXELS), grown.placement(W, H, WIDE, PIXELS));
        assert_near(after.offsets(Anchor::TopRight, PIXELS), before.offsets(Anchor::TopRight, PIXELS));
        assert!(after.size[0] > before.size[0]);

        let change = run(&ctx, &mut canvas, &scene, 1.0, vec![wheel(Modifiers::CTRL)], Modifiers::CTRL);
        assert!(change.changed.is_none());
        run(&ctx, &mut canvas, &scene, 1.1, vec![], Modifiers::NONE);
        assert!(canvas.view.zoomed.is_some_and(|look| look.scale > fit_look(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0)), PIXELS, None).scale));
    }
}

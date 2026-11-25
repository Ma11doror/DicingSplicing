//! Placement of the output frame on the source frame.
//!
//! Coordinates are source pixels with y pointing down. The output is the source rotated
//! clockwise by `angle_deg` around the center of the frame, cut to the frame, then mirrored
//! if `mirror` is on. The same math drives the preview (texture coordinates) and the ffmpeg
//! filters.
//!
//! The frame is the whole result, and the picture lies on it anywhere, as large as the zoom makes
//! it: covering it, inside it, or partly past its edges. Where the picture leaves the frame empty,
//! the clips under it show, or black; with bars that part is black and hides them.

use serde::{Deserialize, Serialize};

pub const MAX_ZOOM: f64 = 10.0;
/// The frame can be larger than the picture: this makes it ten times the largest frame inside, the
/// picture a tenth of it across.
pub const MIN_ZOOM: f64 = 0.1;
/// How narrow or wide a frame can get.
pub const MIN_ASPECT: f64 = 0.1;
pub const MAX_ASPECT: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mask {
    Rect,
    Circle,
}

/// The output's shape: the proportions of its rectangle (width / height) and what is cut out of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outline {
    pub aspect: f64,
    pub mask: Mask,
}

impl Outline {
    /// The largest rectangle of these proportions inside `w`×`h`.
    pub fn fit(&self, w: f64, h: f64) -> [f64; 2] {
        if self.aspect > w / h {
            [w, w / self.aspect]
        } else {
            [h * self.aspect, h]
        }
    }

    /// How far from its center a frame of this size reaches along the source's axes, when turned
    /// by `angle_deg`. A circle does not change with rotation: square corners outside it are
    /// never visible.
    pub fn extents(&self, size: [f64; 2], angle_deg: f64) -> [f64; 2] {
        let [fw, fh] = size;
        match self.mask {
            Mask::Circle => [fw / 2.0, fh / 2.0],
            Mask::Rect => {
                let (sin, cos) = abs_sin_cos(angle_deg);
                [(fw * cos + fh * sin) / 2.0, (fw * sin + fh * cos) / 2.0]
            }
        }
    }

    /// The zoom at which the frame, turned by `angle_deg`, fits inside the source.
    fn min_zoom(&self, w: f64, h: f64, angle_deg: f64) -> f64 {
        let [ex, ey] = self.extents(self.fit(w, h), angle_deg);
        (2.0 * ex / w).max(2.0 * ey / h).max(1.0)
    }
}

fn abs_sin_cos(angle_deg: f64) -> (f64, f64) {
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    (sin.abs(), cos.abs())
}

/// Where a picture stays on the frame when the frame's proportions change: in another output, or in
/// the export to another format at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Anchor {
    /// It follows the frame: the frame keeps its middle on the picture, as frames always did, and
    /// what covers the frame covers it in other proportions too.
    #[default]
    Frame,
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// The points of the frame, by rows from the top left.
    pub const POINTS: [Self; 9] = [Self::TopLeft, Self::Top, Self::TopRight, Self::Left, Self::Center, Self::Right, Self::BottomLeft, Self::Bottom, Self::BottomRight];

    pub fn is_frame(&self) -> bool {
        *self == Self::Frame
    }

    /// Which way from the frame's middle the point is: -1, 0 or 1 across and down. None for `Frame`.
    pub fn sides(self) -> Option<[f64; 2]> {
        let i = Self::POINTS.iter().position(|point| *point == self)?;
        Some([(i % 3) as f64 - 1.0, (i / 3) as f64 - 1.0])
    }

    /// The point of the frame's 3×3 grid that `at` is in: -1 to 1 across and down from the frame's
    /// middle to its edges.
    pub fn of_place(at: [f64; 2]) -> Self {
        let third = |v: f64| if v < -1.0 / 3.0 { 0 } else if v > 1.0 / 3.0 { 2 } else { 1 };
        Self::POINTS[third(at[1]) * 3 + third(at[0])]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Frame => "Frame",
            Self::TopLeft => "Top left",
            Self::Top => "Top",
            Self::TopRight => "Top right",
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
            Self::BottomLeft => "Bottom left",
            Self::Bottom => "Bottom",
            Self::BottomRight => "Bottom right",
        }
    }
}

/// Where the picture is on the frame, in output pixels: its middle from the frame's middle, and its
/// width and height as it is drawn, before it is turned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub center: [f64; 2],
    pub size: [f64; 2],
    pub angle_deg: f64,
}

impl Placement {
    /// Half of what the turned picture spans across and down.
    pub fn reach(&self) -> [f64; 2] {
        let (sin, cos) = abs_sin_cos(self.angle_deg);
        let [w, h] = self.size;
        [(w * cos + h * sin) / 2.0, (w * sin + h * cos) / 2.0]
    }

    /// How far the picture is from what `anchor` names, on a frame of `pixels`: from the frame's left
    /// or right edge to the picture's, inwards, or from the frame's middle to the picture's, to the
    /// right; the same down. `Frame` counts from the middle.
    pub fn offsets(&self, anchor: Anchor, pixels: [f64; 2]) -> [f64; 2] {
        let sides = anchor.sides().unwrap_or([0.0, 0.0]);
        let reach = self.reach();
        std::array::from_fn(|i| match sides[i] {
            side if side < 0.0 => self.center[i] - reach[i] + pixels[i] / 2.0,
            side if side > 0.0 => pixels[i] / 2.0 - (self.center[i] + reach[i]),
            _ => self.center[i],
        })
    }

    /// The middle that puts the picture `offsets` from what `anchor` names: see `offsets`.
    pub fn center_for(&self, anchor: Anchor, pixels: [f64; 2], offsets: [f64; 2]) -> [f64; 2] {
        let sides = anchor.sides().unwrap_or([0.0, 0.0]);
        let reach = self.reach();
        std::array::from_fn(|i| match sides[i] {
            side if side < 0.0 => offsets[i] + reach[i] - pixels[i] / 2.0,
            side if side > 0.0 => pixels[i] / 2.0 - reach[i] - offsets[i],
            _ => offsets[i],
        })
    }

    /// The point that stays where it is when the picture grows or shrinks, from the frame's middle:
    /// what `anchor` names on the picture, the edge or the middle its offsets count from; for `Frame`,
    /// the frame's middle.
    pub fn fixed_point(&self, anchor: Anchor) -> [f64; 2] {
        let Some(sides) = anchor.sides() else { return [0.0, 0.0] };
        let reach = self.reach();
        std::array::from_fn(|i| self.center[i] + sides[i] * reach[i])
    }

    /// The same picture `factor` times as large, the point `fixed` staying where it is.
    pub fn scaled_around(&self, factor: f64, fixed: [f64; 2]) -> Self {
        Self { center: std::array::from_fn(|i| fixed[i] + (self.center[i] - fixed[i]) * factor), size: self.size.map(|side| side * factor), ..*self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Center of the frame, as a fraction of the source width and height.
    pub cx: f64,
    pub cy: f64,
    /// 1 means the largest frame of its proportions inside the source.
    pub zoom: f64,
    /// Clockwise rotation of the picture in the output, in degrees.
    pub angle_deg: f64,
    /// The output is flipped left to right.
    #[serde(default)]
    pub mirror: bool,
    /// Where the frame reaches past the picture, it is black and hides what is under the clip.
    /// Without bars that part is empty: what is under the clip shows there.
    #[serde(default)]
    pub bars: bool,
    /// Where the picture stays when the frame's proportions change.
    #[serde(default, skip_serializing_if = "Anchor::is_frame")]
    pub anchor: Anchor,
}

impl Default for Transform {
    fn default() -> Self {
        Self { cx: 0.5, cy: 0.5, zoom: 1.0, angle_deg: 0.0, mirror: false, bars: false, anchor: Anchor::Frame }
    }
}

impl Transform {
    /// Width and height of the frame in source pixels.
    pub fn size(&self, w: f64, h: f64, outline: Outline) -> [f64; 2] {
        let [fw, fh] = outline.fit(w, h);
        [fw / self.zoom, fh / self.zoom]
    }

    pub fn center(&self, w: f64, h: f64) -> [f64; 2] {
        [self.cx * w, self.cy * h]
    }

    /// Maps an offset from the output center (in source pixels) to a source point.
    pub fn output_to_source(&self, w: f64, h: f64, offset: [f64; 2]) -> [f64; 2] {
        let [cx, cy] = self.center(w, h);
        let (sin, cos) = self.angle_deg.to_radians().sin_cos();
        let [u, v] = offset;
        let u = if self.mirror { -u } else { u };
        [cx + u * cos + v * sin, cy - u * sin + v * cos]
    }

    /// Maps a source point to an offset from the output center.
    pub fn source_to_output(&self, w: f64, h: f64, point: [f64; 2]) -> [f64; 2] {
        let [cx, cy] = self.center(w, h);
        let (sin, cos) = self.angle_deg.to_radians().sin_cos();
        let (x, y) = (point[0] - cx, point[1] - cy);
        let u = x * cos - y * sin;
        [if self.mirror { -u } else { u }, x * sin + y * cos]
    }

    /// Source points of the output corners: top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self, w: f64, h: f64, outline: Outline) -> [[f64; 2]; 4] {
        let [fw, fh] = self.size(w, h, outline);
        let (rx, ry) = (fw / 2.0, fh / 2.0);
        [[-rx, -ry], [rx, -ry], [rx, ry], [-rx, ry]].map(|offset| self.output_to_source(w, h, offset))
    }

    /// The whole picture in the middle of the frame. Around it the frame is empty, or black with bars.
    pub fn showing_whole_picture(&self, w: f64, h: f64, outline: Outline) -> Self {
        let [fw, fh] = outline.fit(w, h);
        let zoom = match outline.mask {
            // All the corners of the picture have to be inside the circle.
            Mask::Circle => fw / w.hypot(h),
            Mask::Rect => {
                let (sin, cos) = abs_sin_cos(self.angle_deg);
                (fw / (w * cos + h * sin)).min(fh / (w * sin + h * cos))
            }
        };
        Self { cx: 0.5, cy: 0.5, zoom: zoom.max(MIN_ZOOM), anchor: Anchor::Frame, ..*self }
    }

    /// The largest centered frame inside the picture, turned as it is: the picture covers it all.
    pub fn filling_frame(&self, w: f64, h: f64, outline: Outline) -> Self {
        Self { cx: 0.5, cy: 0.5, zoom: outline.min_zoom(w, h, self.angle_deg), bars: false, anchor: Anchor::Frame, ..*self }
    }

    /// Keeps the numbers usable: the zoom in its range, and the frame within reach of the picture.
    /// Nothing else is held: the picture may be smaller than the frame, past its edges, or turned
    /// with corners of the frame left empty, and what it leaves empty shows what is under the clip.
    pub fn clamped(mut self, w: f64, h: f64, outline: Outline) -> Self {
        let usable = |v: f64, default: f64| if v.is_finite() { v } else { default };
        self.zoom = usable(self.zoom, 1.0).clamp(MIN_ZOOM, MAX_ZOOM);
        self.angle_deg = usable(self.angle_deg, 0.0);
        // Up to one and a half frames past the picture's edges: far enough for it to leave the frame.
        let [fw, fh] = self.size(w, h, outline);
        let reach = fw.max(fh) * 1.5;
        let (rx, ry) = ((reach + w / 2.0) / w, (reach + h / 2.0) / h);
        // A picture of no size, as sound has, has no reach to keep to.
        let reach = |v: f64, r: f64| if r.is_finite() { usable(v, 0.5).clamp(0.5 - r, 0.5 + r) } else { usable(v, 0.5) };
        self.cx = reach(self.cx, rx);
        self.cy = reach(self.cy, ry);
        self
    }

    /// Whether the picture covers all of the frame, so that nothing under the clip shows.
    pub fn covers(&self, w: f64, h: f64, outline: Outline) -> bool {
        // A thousandth of a pixel: what rounding leaves of a frame that fits exactly.
        const SLACK: f64 = 1e-3;
        let inside = |[x, y]: [f64; 2]| (-SLACK..=w + SLACK).contains(&x) && (-SLACK..=h + SLACK).contains(&y);
        match outline.mask {
            Mask::Rect => self.corners(w, h, outline).into_iter().all(inside),
            Mask::Circle => {
                let [cx, cy] = self.center(w, h);
                let r = self.size(w, h, outline)[0] / 2.0;
                inside([cx - r, cy - r]) && inside([cx + r, cy + r])
            }
        }
    }

    /// How large the picture is on a frame of `pixels`: the frame's pixels per pixel of the picture,
    /// 1 at its own size. Turning does not change it.
    pub fn picture_scale(&self, w: f64, h: f64, outline: Outline, pixels: [f64; 2]) -> f64 {
        pixels[0] / self.size(w, h, outline)[0]
    }

    /// Where the picture is on a frame of `pixels`, in those pixels.
    pub fn placement(&self, w: f64, h: f64, outline: Outline, pixels: [f64; 2]) -> Placement {
        let scale = self.picture_scale(w, h, outline, pixels);
        let [u, v] = self.source_to_output(w, h, [w / 2.0, h / 2.0]);
        Placement { center: [u * scale, v * scale], size: [w * scale, h * scale], angle_deg: self.angle_deg }
    }

    /// The picture `factor` times as large, from its anchor: its offsets from what the anchor names
    /// stay. One that follows the frame grows around the frame's middle.
    pub fn scaled(&self, w: f64, h: f64, outline: Outline, pixels: [f64; 2], factor: f64) -> Self {
        let fixed = self.placement(w, h, outline, pixels).fixed_point(self.anchor);
        self.scaled_around(w, h, outline, pixels, factor, fixed)
    }

    /// The picture `factor` times as large, with the point `fixed` of a frame of `pixels` staying
    /// where it is: from the frame's middle, in those pixels. No larger or smaller than a picture
    /// gets: the zoom stays in its range.
    #[allow(clippy::too_many_arguments)]
    pub fn scaled_around(&self, w: f64, h: f64, outline: Outline, pixels: [f64; 2], factor: f64, fixed: [f64; 2]) -> Self {
        // The picture grows with the zoom.
        let factor = factor.clamp(MIN_ZOOM / self.zoom, MAX_ZOOM / self.zoom);
        let grown = self.placement(w, h, outline, pixels).scaled_around(factor, fixed);
        self.placed(w, h, outline, pixels, grown.center, grown.size[0])
    }

    /// The picture with its middle `center` from the frame's middle and `width` across, both in the
    /// pixels of a frame of `pixels`; turned and mirrored as it is.
    #[allow(clippy::too_many_arguments)]
    pub fn placed(&self, w: f64, h: f64, outline: Outline, pixels: [f64; 2], center: [f64; 2], width: f64) -> Self {
        let scale = width / w;
        let zoom = outline.fit(w, h)[0] * scale / pixels[0];
        Self { zoom, ..*self }.with_picture_at(w, h, [center[0] / scale, center[1] / scale])
    }

    /// The same frame moved so that the picture's middle is `offset` from its middle: in source
    /// pixels, along the output's axes.
    fn with_picture_at(&self, w: f64, h: f64, offset: [f64; 2]) -> Self {
        let [x, y] = Self { cx: 0.0, cy: 0.0, ..*self }.output_to_source(w, h, offset);
        Self { cx: (w / 2.0 - x) / w, cy: (h / 2.0 - y) / h, ..*self }
    }

    /// The picture's middle from the frame's middle, in source pixels along the output's axes.
    fn picture_offset(&self, w: f64, h: f64) -> [f64; 2] {
        self.source_to_output(w, h, [w / 2.0, h / 2.0])
    }

    /// The same picture on a frame of other proportions, `to` instead of `from`: as far from the point
    /// its anchor names and as large, against the frame's shorter side. A picture that follows the
    /// frame stays as it is.
    pub fn moved_to(&self, w: f64, h: f64, from: Outline, to: Outline) -> Self {
        let Some([ax, ay]) = self.anchor.sides() else { return *self };
        let [fw, fh] = self.size(w, h, from);
        let [u, v] = self.picture_offset(w, h);
        let (dx, dy) = (u - ax * fw / 2.0, v - ay * fh / 2.0);
        let fit = to.fit(w, h);
        let next = Self { zoom: fit[0].min(fit[1]) / fw.min(fh), ..*self };
        let [nw, nh] = next.size(w, h, to);
        next.with_picture_at(w, h, [ax * nw / 2.0 + dx, ay * nh / 2.0 + dy])
    }

    /// With the anchor of where the picture is now: `Frame` while it covers the frame, else the point
    /// of the frame's 3×3 grid its middle is in.
    pub fn anchored(&self, w: f64, h: f64, outline: Outline) -> Self {
        if self.covers(w, h, outline) {
            return Self { anchor: Anchor::Frame, ..*self };
        }
        let [fw, fh] = self.size(w, h, outline);
        let [u, v] = self.picture_offset(w, h);
        Self { anchor: Anchor::of_place([u / (fw / 2.0), v / (fh / 2.0)]), ..*self }
    }

    /// Turned to `angle_deg`: a picture that follows the frame with all of it, around the frame's
    /// middle; any other in its place, around its own.
    pub fn turned(&self, w: f64, h: f64, angle_deg: f64) -> Self {
        let next = Self { angle_deg, ..*self };
        if self.anchor.is_frame() {
            return next;
        }
        next.with_picture_at(w, h, self.picture_offset(w, h))
    }

    /// Mirrored, or back: a picture that follows the frame with all of it, any other in its place.
    pub fn flipped(&self, w: f64, h: f64) -> Self {
        let next = Self { mirror: !self.mirror, ..*self };
        if self.anchor.is_frame() {
            return next;
        }
        next.with_picture_at(w, h, self.picture_offset(w, h))
    }
}

/// Brings an angle into (-180°, 180°].
pub fn normalize_angle(deg: f64) -> f64 {
    let deg = deg.rem_euclid(360.0);
    if deg > 180.0 {
        deg - 360.0
    } else {
        deg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 1920.0;
    const H: f64 = 1080.0;
    const CIRCLE: Outline = Outline { aspect: 1.0, mask: Mask::Circle };
    const VERTICAL: Outline = Outline { aspect: 9.0 / 16.0, mask: Mask::Rect };
    const WIDE: Outline = Outline { aspect: 16.0 / 9.0, mask: Mask::Rect };

    fn assert_close(a: [f64; 2], b: [f64; 2]) {
        assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6, "{a:?} != {b:?}");
    }

    #[test]
    fn default_is_the_largest_centered_frame() {
        let c = Transform::default().corners(W, H, CIRCLE);
        assert_close(c[0], [420.0, 0.0]);
        assert_close(c[1], [1500.0, 0.0]);
        assert_close(c[2], [1500.0, 1080.0]);
        assert_close(c[3], [420.0, 1080.0]);

        assert_close(Transform::default().size(W, H, VERTICAL), [607.5, 1080.0]);
        assert_close(Transform::default().size(W, H, WIDE), [1920.0, 1080.0]);
        let cinema = Outline { aspect: 21.0 / 9.0, mask: Mask::Rect };
        assert_close(Transform::default().size(W, H, cinema), [1920.0, 1920.0 * 9.0 / 21.0]);
    }

    #[test]
    fn rotation_turns_the_picture_clockwise() {
        let t = Transform { angle_deg: 90.0, ..Default::default() };
        let c = t.corners(W, H, CIRCLE);
        // Turned clockwise, the source's bottom-left lands in the output's top-left,
        // and the source's top-left in the output's top-right.
        assert_close(c[0], [420.0, 1080.0]);
        assert_close(c[1], [420.0, 0.0]);
    }

    #[test]
    fn zoom_shrinks_the_frame_around_the_center() {
        let t = Transform { cx: 0.25, zoom: 2.0, ..Default::default() };
        let c = t.corners(W, H, CIRCLE);
        assert_close(c[0], [480.0 - 270.0, 540.0 - 270.0]);
        assert_close(c[2], [480.0 + 270.0, 540.0 + 270.0]);
    }

    #[test]
    fn source_and_output_mappings_are_inverse() {
        for mirror in [false, true] {
            let t = Transform { cx: 0.3, cy: 0.6, zoom: 2.5, angle_deg: 33.3, mirror, ..Default::default() };
            for offset in [[10.0, -20.0], [0.0, 0.0], [-100.0, 55.0]] {
                let p = t.output_to_source(W, H, offset);
                assert_close(t.source_to_output(W, H, p), offset);
            }
        }
    }

    #[test]
    fn mirror_swaps_left_and_right() {
        let plain = Transform::default().corners(W, H, VERTICAL);
        let mirrored = Transform { mirror: true, ..Default::default() }.corners(W, H, VERTICAL);
        assert_close(mirrored[0], plain[1]);
        assert_close(mirrored[1], plain[0]);
        assert_close(mirrored[2], plain[3]);
    }

    #[test]
    fn clamping_keeps_only_the_zoom_in_range_and_the_frame_within_reach() {
        // A frame past the picture's corner, turned, larger than the picture: all of it stays.
        let t = Transform { cx: 0.0, cy: 0.0, zoom: 0.5, angle_deg: 45.0, ..Default::default() };
        assert_eq!(t.clamped(W, H, CIRCLE), t);
        assert_eq!(Transform { zoom: 50.0, ..Default::default() }.clamped(W, H, CIRCLE).zoom, MAX_ZOOM);
        assert_eq!(Transform { zoom: 0.01, ..Default::default() }.clamped(W, H, CIRCLE).zoom, MIN_ZOOM);
        // What is not a number becomes the default; a frame far away comes back within reach, which
        // is still far enough for the picture to be out of it.
        let wild = Transform { cx: f64::NAN, cy: 1e9, zoom: f64::INFINITY, angle_deg: f64::NAN, ..Default::default() }.clamped(W, H, WIDE);
        assert_eq!((wild.cx, wild.zoom, wild.angle_deg), (0.5, 1.0, 0.0));
        let [_, fh] = wild.size(W, H, WIDE);
        assert!(wild.cy < 10.0 && (wild.cy - 0.5) * H > fh / 2.0 + H / 2.0, "{}", wild.cy);
    }

    #[test]
    fn a_picture_of_no_size_is_left_as_it_is() {
        let t = Transform { cx: 0.3, ..Default::default() };
        assert_eq!(t.clamped(0.0, 0.0, WIDE), t);
    }

    #[test]
    fn a_picture_covers_the_frame_only_while_the_frame_is_inside_it() {
        for outline in [WIDE, VERTICAL, CIRCLE] {
            assert!(Transform::default().covers(W, H, outline), "{outline:?}");
        }
        assert!(!Transform { zoom: 0.9, ..Default::default() }.covers(W, H, WIDE));
        assert!(!Transform { angle_deg: 10.0, ..Default::default() }.covers(W, H, WIDE), "the corners of the frame are past the picture");
        assert!(!Transform { cx: 0.6, ..Default::default() }.covers(W, H, WIDE));
        // A circle has no corners to lose.
        assert!(Transform { angle_deg: 33.0, ..Default::default() }.covers(W, H, CIRCLE));
        // Fill makes any picture cover the frame again, turned too.
        for outline in [VERTICAL, WIDE, CIRCLE, Outline { aspect: 10.0, mask: Mask::Rect }] {
            for angle_deg in [0.0, 12.0, 45.0, -80.0, 180.0] {
                let t = Transform { cx: 0.1, zoom: 0.4, angle_deg, bars: true, ..Default::default() }.filling_frame(W, H, outline);
                assert!(t.covers(W, H, outline) && !t.bars && (t.cx, t.cy) == (0.5, 0.5), "{outline:?} at {angle_deg}°");
            }
        }
    }

    #[test]
    fn anchors_are_the_points_of_a_three_by_three_grid() {
        assert_eq!(Anchor::Frame.sides(), None);
        assert_eq!(Anchor::TopLeft.sides(), Some([-1.0, -1.0]));
        assert_eq!(Anchor::Right.sides(), Some([1.0, 0.0]));
        assert_eq!(Anchor::Bottom.sides(), Some([0.0, 1.0]));
        assert_eq!(Anchor::of_place([0.9, -0.8]), Anchor::TopRight);
        assert_eq!(Anchor::of_place([0.2, 0.3]), Anchor::Center);
        assert_eq!(Anchor::of_place([-0.5, 0.0]), Anchor::Left);
        assert_eq!(Anchor::of_place([0.0, 2.0]), Anchor::Bottom);
        for point in Anchor::POINTS {
            let [x, y] = point.sides().unwrap();
            assert_eq!(Anchor::of_place([x * 0.9, y * 0.9]), point);
        }
    }

    #[test]
    fn a_picture_is_placed_in_output_pixels() {
        let pixels = [1920.0, 1080.0];
        for (mirror, angle_deg) in [(false, 0.0), (true, 30.0), (false, -75.0)] {
            let t = Transform { cx: 0.2, cy: 0.7, zoom: 0.4, angle_deg, mirror, ..Default::default() };
            let p = t.placement(W, H, WIDE, pixels);
            // The picture, 1920 px of a frame 4800 source pixels across, is 768 px wide on it.
            assert_close(p.size, [768.0, 432.0]);
            let back = t.placed(W, H, WIDE, pixels, p.center, p.size[0]);
            assert_close([back.cx, back.cy], [t.cx, t.cy]);
            assert!((back.zoom - t.zoom).abs() < 1e-12);
            let moved = t.placed(W, H, WIDE, pixels, [100.0, -50.0], 384.0).placement(W, H, WIDE, pixels);
            assert_close(moved.center, [100.0, -50.0]);
            assert_close(moved.size, [384.0, 216.0]);
        }
    }

    #[test]
    fn offsets_count_inwards_from_the_anchor() {
        let pixels = [1920.0, 1080.0];
        let p = Placement { center: [600.0, -300.0], size: [400.0, 200.0], angle_deg: 0.0 };
        // Its right edge is 160 px from the frame's, its top 140 px from the frame's top.
        assert_close(p.offsets(Anchor::TopRight, pixels), [160.0, 140.0]);
        assert_close(p.offsets(Anchor::Center, pixels), [600.0, -300.0]);
        assert_close(p.offsets(Anchor::Frame, pixels), [600.0, -300.0]);
        assert_close(p.offsets(Anchor::BottomLeft, pixels), [1360.0, 740.0]);
        for anchor in Anchor::POINTS {
            assert_close(p.center_for(anchor, pixels, p.offsets(anchor, pixels)), p.center);
        }
        // Turned, it reaches farther: the offsets are to what it spans.
        let turned = Placement { angle_deg: 90.0, ..p };
        assert_close(turned.offsets(Anchor::TopRight, pixels), [260.0, 40.0]);
    }

    #[test]
    fn scaling_keeps_the_picture_at_what_its_anchor_names() {
        let pixels = [1920.0, 1080.0];
        for anchor in Anchor::POINTS {
            for (angle_deg, mirror) in [(0.0, false), (25.0, true), (-90.0, false)] {
                let t = Transform { zoom: 0.3, angle_deg, mirror, anchor, ..Default::default() }.placed(W, H, WIDE, pixels, [500.0, -200.0], 480.0);
                let before = t.placement(W, H, WIDE, pixels);
                let grown = t.scaled(W, H, WIDE, pixels, 1.5);
                let after = grown.placement(W, H, WIDE, pixels);
                assert_close(after.offsets(anchor, pixels), before.offsets(anchor, pixels));
                assert_close(after.size, before.size.map(|side| side * 1.5));
                assert_eq!((grown.anchor, grown.angle_deg, grown.mirror), (anchor, angle_deg, mirror), "{anchor:?}");
            }
        }
    }

    #[test]
    fn a_picture_that_follows_the_frame_grows_around_the_frames_middle() {
        let pixels = [1920.0, 1080.0];
        let t = Transform { cx: 0.4, cy: 0.55, zoom: 1.2, angle_deg: 10.0, ..Default::default() };
        let grown = t.scaled(W, H, WIDE, pixels, 2.0);
        // The point of the picture in the frame's middle stays there.
        assert_close([grown.cx, grown.cy], [t.cx, t.cy]);
        assert!((grown.zoom - t.zoom * 2.0).abs() < 1e-12);
        assert_close(grown.placement(W, H, WIDE, pixels).center, t.placement(W, H, WIDE, pixels).center.map(|v| v * 2.0));
        assert_eq!(grown.anchor, Anchor::Frame);
    }

    #[test]
    fn scaling_stops_where_the_zoom_does() {
        let pixels = [1920.0, 1080.0];
        let t = Transform { anchor: Anchor::TopLeft, ..Default::default() }.placed(W, H, WIDE, pixels, [-660.0, -370.0], 600.0);
        let before = t.placement(W, H, WIDE, pixels);
        for factor in [1e-6, 1e6] {
            let next = t.scaled(W, H, WIDE, pixels, factor);
            assert!(next.zoom > MIN_ZOOM - 1e-9 && next.zoom < MAX_ZOOM + 1e-9, "{}", next.zoom);
            assert_close(next.placement(W, H, WIDE, pixels).offsets(Anchor::TopLeft, pixels), before.offsets(Anchor::TopLeft, pixels));
        }
    }

    #[test]
    fn the_scale_counts_the_pictures_own_pixels() {
        // A new clip covers the frame: 4K on 1920×1080 is half its size, 720p one and a half.
        let pixels = [1920.0, 1080.0];
        assert!((Transform::default().picture_scale(3840.0, 2160.0, WIDE, pixels) - 0.5).abs() < 1e-12);
        assert!((Transform::default().picture_scale(1280.0, 720.0, WIDE, pixels) - 1.5).abs() < 1e-12);
        // Turned, it is as large as it was.
        assert_eq!(Transform { angle_deg: 30.0, ..Default::default() }.picture_scale(W, H, WIDE, pixels), 1.0);
    }

    #[test]
    fn a_pinned_picture_keeps_its_corner_and_its_size_against_the_frame() {
        // Frames 1000 px on their shorter side.
        let pixels = |o: Outline| if o.aspect >= 1.0 { [1000.0 * o.aspect, 1000.0] } else { [1000.0, 1000.0 / o.aspect] };
        let start = Transform { zoom: 0.3, angle_deg: 8.0, anchor: Anchor::TopRight, ..Default::default() };
        let p = start.placement(W, H, WIDE, pixels(WIDE));
        // 50 px in from the top right corner.
        let start = start.placed(W, H, WIDE, pixels(WIDE), p.center_for(Anchor::TopRight, pixels(WIDE), [50.0, 50.0]), p.size[0]);
        for to in [VERTICAL, CIRCLE, WIDE, Outline { aspect: 21.0 / 9.0, mask: Mask::Rect }] {
            let moved = start.moved_to(W, H, WIDE, to);
            let q = moved.placement(W, H, to, pixels(to));
            assert_close(q.offsets(Anchor::TopRight, pixels(to)), [50.0, 50.0]);
            assert_close(q.size, p.size);
            assert_eq!((moved.angle_deg, moved.anchor), (8.0, Anchor::TopRight));
        }
        // One that follows the frame is not moved.
        let follows = Transform { zoom: 0.3, ..Default::default() };
        assert_eq!(follows.moved_to(W, H, WIDE, VERTICAL), follows);
    }

    #[test]
    fn a_picture_gets_the_anchor_of_where_it_is() {
        let pixels = [1920.0, 1080.0];
        assert_eq!(Transform { cx: 0.45, zoom: 1.2, ..Default::default() }.anchored(W, H, WIDE).anchor, Anchor::Frame);
        let small = Transform { zoom: 0.25, anchor: Anchor::Left, ..Default::default() };
        let at = |center| small.placed(W, H, WIDE, pixels, center, 480.0).anchored(W, H, WIDE).anchor;
        assert_eq!(at([0.0, 0.0]), Anchor::Center);
        assert_eq!(at([700.0, -400.0]), Anchor::TopRight);
        assert_eq!(at([-700.0, 0.0]), Anchor::Left);
        assert_eq!(at([0.0, 500.0]), Anchor::Bottom);
    }

    #[test]
    fn a_pinned_picture_turns_and_mirrors_in_its_place() {
        let pixels = [1920.0, 1080.0];
        let t = Transform { zoom: 0.25, anchor: Anchor::TopRight, ..Default::default() }.placed(W, H, WIDE, pixels, [700.0, -350.0], 480.0);
        for next in [t.turned(W, H, 30.0), t.flipped(W, H), t.turned(W, H, -90.0).flipped(W, H)] {
            assert_close(next.placement(W, H, WIDE, pixels).center, [700.0, -350.0]);
        }
        // One that follows the frame turns and mirrors with all of it.
        let whole = Transform { cx: 0.6, ..Default::default() };
        assert_eq!(whole.turned(W, H, 30.0), Transform { angle_deg: 30.0, ..whole });
        assert_eq!(whole.flipped(W, H), Transform { mirror: true, ..whole });
    }

    #[test]
    fn the_frame_may_leave_the_picture() {
        let t = Transform { cx: 0.05, cy: 0.1, zoom: 0.5, ..Default::default() }.clamped(W, H, VERTICAL);
        assert_eq!((t.cx, t.cy, t.zoom), (0.05, 0.1, 0.5));

        // Its center past the picture's edges too; smaller than a tenth it does not get.
        let t = Transform { cx: -1.0, cy: 2.0, zoom: 0.01, ..Default::default() }.clamped(W, H, VERTICAL);
        assert_eq!((t.cx, t.cy, t.zoom), (-1.0, 2.0, MIN_ZOOM));
    }

    #[test]
    fn the_whole_picture_fits_the_frame() {
        let cases = [(VERTICAL, 0.0), (WIDE, 0.0), (VERTICAL, 30.0), (CIRCLE, 0.0), (CIRCLE, 40.0), (Outline { aspect: 1.6, mask: Mask::Rect }, -75.0)];
        for (outline, angle_deg) in cases {
            let t = Transform { angle_deg, ..Default::default() }.showing_whole_picture(W, H, outline);
            assert_eq!(t.clamped(W, H, outline), t);
            let [fw, fh] = t.size(W, H, outline);
            for corner in [[0.0, 0.0], [W, 0.0], [W, H], [0.0, H]] {
                let [u, v] = t.source_to_output(W, H, corner);
                let inside = match outline.mask {
                    Mask::Circle => u.hypot(v) <= fw / 2.0 + 1e-6,
                    Mask::Rect => u.abs() <= fw / 2.0 + 1e-6 && v.abs() <= fh / 2.0 + 1e-6,
                };
                assert!(inside, "{outline:?} at {angle_deg}°: corner at {u}, {v} in {fw}×{fh}");
            }
        }

        // 16:9 in 16:9 is exactly the picture.
        assert!((Transform::default().showing_whole_picture(W, H, WIDE).zoom - 1.0).abs() < 1e-9);
    }

    #[test]
    fn filling_the_frame_removes_the_bars_and_follows_the_frame() {
        let whole = Transform { bars: true, anchor: Anchor::Left, ..Default::default() }.showing_whole_picture(W, H, VERTICAL);
        assert_eq!((whole.bars, whole.anchor), (true, Anchor::Frame), "Fit leaves the bars as they are");
        assert_eq!(whole.filling_frame(W, H, VERTICAL), Transform::default());
        // Turned, the frame still fits the picture, so it is smaller.
        let t = Transform { angle_deg: 30.0, ..Default::default() }.filling_frame(W, H, VERTICAL);
        assert!(t.zoom > 1.0 && t.covers(W, H, VERTICAL));
    }

    #[test]
    fn normalizes_angles() {
        assert_eq!(normalize_angle(270.0), -90.0);
        assert_eq!(normalize_angle(-190.0), 170.0);
        assert_eq!(normalize_angle(180.0), 180.0);
        assert_eq!(normalize_angle(-180.0), 180.0);
        assert_eq!(normalize_angle(12.5), 12.5);
    }
}

//! Placement of the output frame on the source frame.
//!
//! Coordinates are source pixels with y pointing down. The output is the source rotated
//! clockwise by `angle_deg` around the center of the frame, cut to the frame, then mirrored
//! if `mirror` is on. The same math drives the preview (texture coordinates) and, later,
//! the ffmpeg filters.

use serde::{Deserialize, Serialize};

pub const MAX_ZOOM: f64 = 10.0;
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
}

impl Default for Transform {
    fn default() -> Self {
        Self { cx: 0.5, cy: 0.5, zoom: 1.0, angle_deg: 0.0, mirror: false }
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

    /// Moves a side of the frame to `extent` from the center, keeping the proportions. `side` points
    /// outwards from the output center. The center stays, or with `keep_opposite` the opposite side.
    pub fn resized(&self, w: f64, h: f64, outline: Outline, side: [f64; 2], extent: f64, keep_opposite: bool) -> Self {
        let [fw, fh] = self.size(w, h, outline);
        let half = if outline.mask == Mask::Circle || is_horizontal(side) { fw } else { fh } / 2.0;
        let wanted = if keep_opposite { (extent + half) / 2.0 } else { extent }.max(1.0);
        let zoom = (self.zoom * half / wanted).max(outline.min_zoom(w, h, self.angle_deg)).min(MAX_ZOOM);
        let mut out = Self { zoom, ..*self };
        if keep_opposite {
            out.shift_along(self, w, h, side, half * self.zoom / zoom - half);
        }
        out
    }

    /// Moves a side of the frame to `extent` from the center and changes the proportions: the other
    /// dimension stays. The side stops where the turned frame no longer fits in the source. Returns
    /// the new placement and the proportions, passed through `snap` on the way.
    #[allow(clippy::too_many_arguments)]
    pub fn reshaped(
        &self,
        w: f64,
        h: f64,
        outline: Outline,
        side: [f64; 2],
        extent: f64,
        keep_opposite: bool,
        snap: impl Fn(f64) -> f64,
    ) -> (Self, f64) {
        let [fw, fh] = self.size(w, h, outline);
        let horizontal = is_horizontal(side);
        let (moving, other) = if horizontal { (fw, fh) } else { (fh, fw) };
        let half = moving / 2.0;
        let wanted = 2.0 * if keep_opposite { (extent + half) / 2.0 } else { extent };

        let (sin, cos) = abs_sin_cos(self.angle_deg);
        let (to_x, to_y) = if horizontal { (cos, sin) } else { (sin, cos) };
        let room = |free: f64, share: f64| if share > 1e-9 { free / share } else { f64::INFINITY };
        let longest = room(w - other * to_y, to_x).min(room(h - other * to_x, to_y));
        let moved = wanted.min(longest).max(1.0);

        let aspect = if horizontal { moved / other } else { other / moved };
        let aspect = snap(aspect.clamp(MIN_ASPECT, MAX_ASPECT));
        let moved = if horizontal { other * aspect } else { other / aspect };
        let fit = Outline { aspect, mask: outline.mask }.fit(w, h);
        let zoom = if horizontal { fit[1] } else { fit[0] } / other;

        let mut out = Self { zoom, ..*self };
        if keep_opposite {
            out.shift_along(self, w, h, side, moved / 2.0 - half);
        }
        (out, aspect)
    }

    /// Puts the center `shift` along `direction` (in output coordinates) from the center of `from`.
    fn shift_along(&mut self, from: &Self, w: f64, h: f64, direction: [f64; 2], shift: f64) {
        let [cx, cy] = from.output_to_source(w, h, [direction[0] * shift, direction[1] * shift]);
        self.cx = cx / w;
        self.cy = cy / h;
    }

    /// Keeps the zoom in range and the frame inside the source: a turned rectangle needs a higher
    /// zoom to fit, so that the output never has empty corners.
    pub fn clamped(mut self, w: f64, h: f64, outline: Outline) -> Self {
        let min_zoom = outline.min_zoom(w, h, self.angle_deg);
        self.zoom = self.zoom.max(min_zoom).min(MAX_ZOOM.max(min_zoom));
        let [ex, ey] = outline.extents(self.size(w, h, outline), self.angle_deg);
        self.cx = self.cx.max(ex / w).min(1.0 - ex / w);
        self.cy = self.cy.max(ey / h).min(1.0 - ey / h);
        self
    }
}

fn is_horizontal(side: [f64; 2]) -> bool {
    side[0].abs() > side[1].abs()
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
            let t = Transform { cx: 0.3, cy: 0.6, zoom: 2.5, angle_deg: 33.3, mirror };
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
    fn clamp_keeps_the_circle_inside() {
        let t = Transform { cx: 0.0, cy: 0.0, zoom: 0.5, angle_deg: 45.0, mirror: false }.clamped(W, H, CIRCLE);
        assert_eq!(t.zoom, 1.0);
        assert!((t.cx - 540.0 / W).abs() < 1e-12);
        assert_eq!(t.cy, 0.5);
        assert_eq!(t.angle_deg, 45.0);
        assert_eq!(Transform { zoom: 50.0, ..Default::default() }.clamped(W, H, CIRCLE).zoom, MAX_ZOOM);
    }

    #[test]
    fn clamp_keeps_every_corner_of_a_turned_rectangle_inside() {
        let outlines = [VERTICAL, WIDE, Outline { aspect: 1.6, mask: Mask::Rect }, Outline { aspect: 10.0, mask: Mask::Rect }];
        for outline in outlines {
            for angle_deg in [0.0, 7.5, 30.0, 45.0, 90.0, 133.0, -60.0, 180.0] {
                for (cx, cy) in [(0.5, 0.5), (0.0, 0.0), (1.0, 0.3)] {
                    let t = Transform { cx, cy, zoom: 1.0, angle_deg, mirror: false }.clamped(W, H, outline);
                    for [x, y] in t.corners(W, H, outline) {
                        let inside = (-1e-6..=W + 1e-6).contains(&x) && (-1e-6..=H + 1e-6).contains(&y);
                        assert!(inside, "{outline:?} at {angle_deg}°: corner {x}, {y}");
                    }
                }
            }
        }
    }

    #[test]
    fn clamp_does_not_touch_an_upright_frame_that_fits() {
        let t = Transform { cx: 0.2, cy: 0.5, zoom: 1.0, angle_deg: 0.0, mirror: false }.clamped(W, H, VERTICAL);
        assert_eq!(t.zoom, 1.0);
        assert!((t.cx - 0.2).abs() < 1e-12);
    }

    #[test]
    fn resizing_moves_the_side_around_the_center() {
        let start = Transform { zoom: 2.0, ..Default::default() };
        let half = start.size(W, H, VERTICAL)[0] / 2.0;
        let t = start.resized(W, H, VERTICAL, [1.0, 0.0], half * 1.5, false);
        assert!((t.size(W, H, VERTICAL)[0] / 2.0 - half * 1.5).abs() < 1e-6);
        assert_eq!((t.cx, t.cy), (start.cx, start.cy));

        // Pulling past the largest frame stops at zoom 1, pushing to nothing at the maximum zoom.
        assert_eq!(start.resized(W, H, VERTICAL, [1.0, 0.0], 1e6, false).zoom, 1.0);
        assert_eq!(start.resized(W, H, VERTICAL, [1.0, 0.0], 0.0, false).zoom, MAX_ZOOM);
    }

    #[test]
    fn resizing_can_keep_the_opposite_side() {
        let start = Transform { zoom: 2.0, angle_deg: 30.0, ..Default::default() };
        let [fw, _] = start.size(W, H, VERTICAL);
        let opposite = start.output_to_source(W, H, [-fw / 2.0, 0.0]);

        let t = start.resized(W, H, VERTICAL, [1.0, 0.0], fw, true);
        let [new_fw, _] = t.size(W, H, VERTICAL);
        assert!((new_fw - fw * 1.5).abs() < 1e-6);
        assert_close(t.output_to_source(W, H, [-new_fw / 2.0, 0.0]), opposite);
    }

    #[test]
    fn resizing_a_circle_works_along_any_direction() {
        let start = Transform { zoom: 2.0, ..Default::default() };
        let t = start.resized(W, H, CIRCLE, [0.6, 0.8], 135.0, false);
        assert!((t.size(W, H, CIRCLE)[0] - 270.0).abs() < 1e-6);
    }

    #[test]
    fn reshaping_changes_the_proportions_and_keeps_the_other_side() {
        let start = Transform { zoom: 2.0, ..Default::default() };
        let [fw, fh] = start.size(W, H, VERTICAL);

        let (t, aspect) = start.reshaped(W, H, VERTICAL, [1.0, 0.0], fw, false, |a| a);
        let new = Outline { aspect, mask: Mask::Rect };
        assert_close(t.size(W, H, new), [fw * 2.0, fh]);
        assert!((aspect - 2.0 * fw / fh).abs() < 1e-9);

        let (t, aspect) = start.reshaped(W, H, VERTICAL, [0.0, -1.0], fh / 4.0, false, |a| a);
        let new = Outline { aspect, mask: Mask::Rect };
        assert_close(t.size(W, H, new), [fw, fh / 2.0]);
    }

    #[test]
    fn reshaping_applies_the_snap_and_stops_at_the_source() {
        let start = Transform { zoom: 2.0, ..Default::default() };
        let [fw, fh] = start.size(W, H, VERTICAL);
        let (t, aspect) = start.reshaped(W, H, VERTICAL, [1.0, 0.0], fw, false, |_| 1.6);
        assert_eq!(aspect, 1.6);
        assert_close(t.size(W, H, Outline { aspect, mask: Mask::Rect }), [fh * 1.6, fh]);

        // Pulled far past the picture: as wide as the source, the height is not touched.
        let (t, aspect) = start.reshaped(W, H, VERTICAL, [1.0, 0.0], 1e6, false, |a| a);
        assert_close(t.size(W, H, Outline { aspect, mask: Mask::Rect }), [W, fh]);
    }

    #[test]
    fn reshaping_can_keep_the_opposite_side() {
        let start = Transform { zoom: 2.0, ..Default::default() };
        let [fw, _] = start.size(W, H, VERTICAL);
        let opposite = start.output_to_source(W, H, [-fw / 2.0, 0.0]);
        let (t, aspect) = start.reshaped(W, H, VERTICAL, [1.0, 0.0], fw, true, |a| a);
        let [new_fw, _] = t.size(W, H, Outline { aspect, mask: Mask::Rect });
        assert!((new_fw - fw * 1.5).abs() < 1e-6);
        assert_close(t.output_to_source(W, H, [-new_fw / 2.0, 0.0]), opposite);
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

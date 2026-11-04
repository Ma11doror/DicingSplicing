//! Output frames: the formats a result can have. Everything that depends on the output format
//! reads it from here.

use crate::media::geometry::{Mask, Outline, MAX_ASPECT, MIN_ASPECT};
use serde::{Deserialize, Serialize};

/// Proportions of a frame, as the two numbers people write them with.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ratio {
    pub w: f64,
    pub h: f64,
}

impl Ratio {
    pub const fn new(w: f64, h: f64) -> Self {
        Self { w, h }
    }

    /// The nearest thing to `aspect` that reads well: a catalog ratio if it is one, else `x:1` or `1:x`.
    pub fn from_aspect(aspect: f64) -> Self {
        known_ratio(aspect, 1e-6).unwrap_or_else(|| {
            if aspect >= 1.0 {
                Self::new(round2(aspect), 1.0)
            } else {
                Self::new(1.0, round2(1.0 / aspect))
            }
        })
    }

    pub fn aspect(self) -> f64 {
        (self.w / self.h).clamp(MIN_ASPECT, MAX_ASPECT)
    }

    pub fn swapped(self) -> Self {
        Self::new(self.h, self.w)
    }

    pub fn label(self) -> String {
        format!("{}:{}", round2(self.w), round2(self.h))
    }

    /// Output size in pixels with the given long side. Both sides are even, as yuv420p needs.
    pub fn pixels(self, long_side: u32) -> [u32; 2] {
        let long = long_side as f64;
        let aspect = self.aspect();
        let (w, h) = if aspect >= 1.0 { (long, long / aspect) } else { (long * aspect, long) };
        [even(w), even(h)]
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn even(v: f64) -> u32 {
    (v / 2.0).round() as u32 * 2
}

/// Parts of a frame that the app showing the video covers with its own interface, as fractions
/// of the frame's width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SafeZone {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetId {
    TelegramCircle,
    Vertical,
    Landscape,
    Tablet,
    Photo,
    Classic,
    Square,
    Post,
    Cinema,
    Custom,
}

#[derive(Debug)]
pub struct Preset {
    pub id: PresetId,
    pub name: &'static str,
    /// For `Custom` only the starting value: the current one is in `Output`.
    pub ratio: Ratio,
    pub mask: Mask,
    /// Long side of the output in pixels, unless the export settings say otherwise.
    pub long_side: u32,
    /// The frame's color on the source view.
    pub color: [u8; 3],
    pub safe: Option<SafeZone>,
}

const fn rect(id: PresetId, name: &'static str, ratio: Ratio, long_side: u32, color: [u8; 3]) -> Preset {
    Preset { id, name, ratio, mask: Mask::Rect, long_side, color, safe: None }
}

/// The circle is 400 px, as in tgCircleGen. It is saved as a square: Telegram shows it as a circle.
///
/// The vertical safe zone is approximate: the apps change their interface and guides disagree on the
/// numbers, so it only shows roughly where captions and buttons go.
pub const PRESETS: [Preset; 10] = [
    Preset {
        id: PresetId::TelegramCircle,
        name: "Telegram circle",
        ratio: Ratio::new(1.0, 1.0),
        mask: Mask::Circle,
        long_side: 400,
        color: [255, 255, 255],
        safe: None,
    },
    Preset {
        safe: Some(SafeZone { top: 0.14, bottom: 0.35, left: 0.06, right: 0.06 }),
        ..rect(PresetId::Vertical, "Reels, Shorts, TikTok", Ratio::new(9.0, 16.0), 1920, [237, 147, 177])
    },
    rect(PresetId::Landscape, "Landscape", Ratio::new(16.0, 9.0), 1920, [240, 153, 123]),
    rect(PresetId::Tablet, "Tablet, phone", Ratio::new(16.0, 10.0), 1920, [250, 199, 117]),
    rect(PresetId::Photo, "Photo", Ratio::new(3.0, 2.0), 1620, [197, 180, 120]),
    rect(PresetId::Classic, "Classic", Ratio::new(4.0, 3.0), 1440, [133, 183, 235]),
    rect(PresetId::Square, "Square", Ratio::new(1.0, 1.0), 1080, [93, 202, 165]),
    rect(PresetId::Post, "Instagram post", Ratio::new(4.0, 5.0), 1350, [175, 169, 236]),
    rect(PresetId::Cinema, "Cinema", Ratio::new(21.0, 9.0), 2520, [120, 200, 220]),
    rect(PresetId::Custom, "Custom", Ratio::new(16.0, 10.0), 1920, [151, 196, 89]),
];

/// A catalog ratio, either way up, within `tolerance` of `aspect`.
fn known_ratio(aspect: f64, tolerance: f64) -> Option<Ratio> {
    PRESETS
        .iter()
        .filter(|p| p.id != PresetId::Custom)
        .flat_map(|p| [p.ratio, p.ratio.swapped()])
        .find(|r| (r.w / r.h / aspect).ln().abs() < tolerance)
}

/// Pulls proportions set with the mouse onto a catalog ratio when they are about 1% from it.
pub fn snap_aspect(aspect: f64) -> f64 {
    known_ratio(aspect, 0.01).map_or(aspect, |r| r.w / r.h)
}

pub fn get(id: PresetId) -> &'static Preset {
    PRESETS.iter().find(|p| p.id == id).unwrap_or(&PRESETS[0])
}

/// How a frame is shown. These are view settings: they do not change the result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameOptions {
    /// Shown for comparison while another frame is the main one.
    pub shown: bool,
    pub thirds: bool,
    pub center: bool,
    pub safe: bool,
}

impl FrameOptions {
    pub fn for_preset(preset: &Preset) -> Self {
        let round = preset.mask == Mask::Circle;
        Self { shown: false, thirds: !round, center: round, safe: false }
    }
}

/// What the edit is made for: the main frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub preset: PresetId,
    /// The proportions of the `Custom` frame, also while another frame is the main one.
    pub custom: Ratio,
}

impl Default for Output {
    fn default() -> Self {
        Self { preset: PresetId::TelegramCircle, custom: get(PresetId::Custom).ratio }
    }
}

impl Output {
    pub fn preset(&self) -> &'static Preset {
        get(self.preset)
    }

    pub fn ratio_of(&self, preset: &Preset) -> Ratio {
        if preset.id == PresetId::Custom {
            self.custom
        } else {
            preset.ratio
        }
    }

    pub fn outline_of(&self, preset: &Preset) -> Outline {
        Outline { aspect: self.ratio_of(preset).aspect(), mask: preset.mask }
    }

    pub fn ratio(&self) -> Ratio {
        self.ratio_of(self.preset())
    }

    pub fn outline(&self) -> Outline {
        self.outline_of(self.preset())
    }

    /// Output size in pixels.
    pub fn pixels(&self) -> [u32; 2] {
        self.ratio().pixels(self.preset().long_side)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(id: PresetId) -> [u32; 2] {
        Output { preset: id, ..Output::default() }.pixels()
    }

    #[test]
    fn presets_have_the_usual_sizes() {
        assert_eq!(pixels(PresetId::TelegramCircle), [400, 400]);
        assert_eq!(pixels(PresetId::Vertical), [1080, 1920]);
        assert_eq!(pixels(PresetId::Landscape), [1920, 1080]);
        assert_eq!(pixels(PresetId::Tablet), [1920, 1200]);
        assert_eq!(pixels(PresetId::Photo), [1620, 1080]);
        assert_eq!(pixels(PresetId::Classic), [1440, 1080]);
        assert_eq!(pixels(PresetId::Square), [1080, 1080]);
        assert_eq!(pixels(PresetId::Post), [1080, 1350]);
        assert_eq!(pixels(PresetId::Cinema), [2520, 1080]);
    }

    #[test]
    fn every_preset_is_found_by_its_id() {
        for p in &PRESETS {
            assert_eq!(get(p.id).id, p.id);
        }
    }

    #[test]
    fn the_custom_frame_uses_its_own_ratio() {
        let output = Output { preset: PresetId::Custom, custom: Ratio::new(5.0, 4.0) };
        assert_eq!(output.ratio(), Ratio::new(5.0, 4.0));
        assert_eq!(output.outline(), Outline { aspect: 1.25, mask: Mask::Rect });
        assert_eq!(output.pixels(), [1920, 1536]);
        // Other frames keep theirs.
        assert_eq!(output.ratio_of(get(PresetId::Landscape)), Ratio::new(16.0, 9.0));
    }

    #[test]
    fn output_sizes_are_even_and_extreme_ratios_are_limited() {
        let [w, h] = Ratio::new(7.0, 3.0).pixels(1921);
        assert!(w % 2 == 0 && h % 2 == 0, "{w}×{h}");
        assert_eq!(Ratio::new(1000.0, 1.0).aspect(), MAX_ASPECT);
        assert_eq!(Ratio::new(1.0, 1000.0).aspect(), MIN_ASPECT);
    }

    #[test]
    fn ratios_from_the_mouse_read_well() {
        assert_eq!(Ratio::from_aspect(1.6).label(), "16:10");
        assert_eq!(Ratio::from_aspect(9.0 / 16.0).label(), "9:16");
        assert_eq!(Ratio::from_aspect(10.0 / 16.0).label(), "10:16");
        assert_eq!(Ratio::from_aspect(1.623).label(), "1.62:1");
        assert_eq!(Ratio::from_aspect(0.8).label(), "4:5");
        assert_eq!(Ratio::from_aspect(0.55).label(), "1:1.82");
    }

    #[test]
    fn aspects_near_a_known_ratio_snap_to_it() {
        assert_eq!(snap_aspect(1.595), 1.6);
        assert_eq!(snap_aspect(1.7), 1.7);
        assert_eq!(snap_aspect(0.5), 0.5);
    }
}

//! The highlight that shows which controls have just changed or become available.

use egui::{vec2, Color32, Stroke};
use std::f64::consts::TAU;

pub const COLOR: Color32 = Color32::from_rgb(255, 205, 60);
const SECONDS: f64 = 1.2;
const PULSES: f64 = 2.0;

/// How bright the flash is `elapsed` seconds after it began: pulses from 0 to 1 and back.
fn strength(elapsed: f64) -> f32 {
    if !(0.0..SECONDS).contains(&elapsed) {
        return 0.0;
    }
    ((1.0 - (elapsed / SECONDS * PULSES * TAU).cos()) / 2.0) as f32
}

/// A frame for controls that flashed at time `started`, if they did. With `lasting` it keeps its
/// border afterwards. It takes the same room whether it shows anything or not.
pub fn frame(ui: &egui::Ui, started: Option<f64>, lasting: bool) -> egui::Frame {
    let now = ui.input(|i| i.time);
    let running = started.is_some_and(|at| (0.0..SECONDS).contains(&(now - at)));
    if running {
        ui.ctx().request_repaint();
    }
    let brightness = started.map_or(0.0, |at| strength(now - at));
    let border = if lasting || running { Stroke::new(1.0, COLOR.gamma_multiply(0.7)) } else { Stroke::NONE };
    egui::Frame::none().fill(COLOR.gamma_multiply(0.6 * brightness)).stroke(border).rounding(4.0).inner_margin(vec2(4.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flash_pulses_and_ends() {
        assert_eq!(strength(-0.1), 0.0);
        assert!(strength(0.0).abs() < 1e-6);
        assert!((strength(0.3) - 1.0).abs() < 1e-6);
        assert!(strength(0.6).abs() < 1e-6);
        assert!((strength(0.9) - 1.0).abs() < 1e-6);
        assert_eq!(strength(SECONDS), 0.0);
    }
}

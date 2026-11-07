//! A clip's colour on pixels: for the preview, as tables applied to a decoded frame, and for the file,
//! as the ffmpeg filters that take the same steps with the same rounding. What one shows the other
//! writes.
//!
//! First each of red, green and blue on its own: multiplied by its gain (exposure and temperature),
//! spread around the middle by the contrast, kept within 0..255 and rounded (`lutrgb`). Then the
//! saturation mixes each with the others by the weights of Rec. 709 brightness: each part rounded,
//! the sum kept within 0..255 (`colorchannelmixer`).

use crate::core::edit::Color;
use egui::Color32;

/// How much red, green and blue each make of a pixel's brightness (Rec. 709).
const WEIGHTS: [f64; 3] = [0.2126, 0.7152, 0.0722];

/// The numbers as ffmpeg gets them: written with six decimals.
fn six(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

/// The colour, ready to apply.
pub struct Grade {
    /// What each value of each channel becomes before the saturation.
    curves: [[u8; 256]; 3],
    /// `mix[out][from][value]`: how much a channel's value adds to an output channel.
    mix: [[[i32; 256]; 3]; 3],
}

/// What a channel's value `v` becomes with `gain` and the contrast `k`.
fn channel(v: u8, gain: f64, k: f64) -> u8 {
    let x = ((v as f64 / 255.0) * gain - 0.5) * k + 0.5;
    (x.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

/// The saturation as a matrix: `matrix[out][from]`.
fn matrix(saturation: f64) -> [[f64; 3]; 3] {
    let s = saturation;
    let mut m = [[0.0; 3]; 3];
    for (out, row) in m.iter_mut().enumerate() {
        for (from, cell) in row.iter_mut().enumerate() {
            *cell = six(WEIGHTS[from] * (1.0 - s) + if out == from { s } else { 0.0 });
        }
    }
    m
}

impl Grade {
    pub fn new(color: &Color) -> Self {
        let gains = color.gains().map(six);
        let k = six(color.contrast_factor());
        let mut curves = [[0u8; 256]; 3];
        for (c, curve) in curves.iter_mut().enumerate() {
            for (v, out) in curve.iter_mut().enumerate() {
                *out = channel(v as u8, gains[c], k);
            }
        }
        let m = matrix(color.saturation_factor());
        let mut mix = [[[0i32; 256]; 3]; 3];
        for (row, weights) in mix.iter_mut().zip(m) {
            for (table, weight) in row.iter_mut().zip(weights) {
                for (v, part) in table.iter_mut().enumerate() {
                    *part = (v as f64 * weight).round() as i32;
                }
            }
        }
        Self { curves, mix }
    }

    /// Changes opaque pixels in place.
    pub fn apply(&self, pixels: &mut [Color32]) {
        for pixel in pixels {
            let [r, g, b, a] = pixel.to_array();
            let rgb = [self.curves[0][r as usize], self.curves[1][g as usize], self.curves[2][b as usize]].map(usize::from);
            let out = |c: usize| (self.mix[c][0][rgb[0]] + self.mix[c][1][rgb[1]] + self.mix[c][2][rgb[2]]).clamp(0, 255) as u8;
            *pixel = Color32::from_rgba_premultiplied(out(0), out(1), out(2), a);
        }
    }
}

/// The ffmpeg filters for the colour, to follow others in a chain: they start with a comma, take
/// any picture and give back `yuv420p`. Empty if it changes nothing.
pub fn filters(color: &Color) -> String {
    if color.is_neutral() {
        return String::new();
    }
    let gains = color.gains().map(six);
    let k = six(color.contrast_factor());
    let curve = |gain: f64| format!("floor(clip(((val/255)*{gain:.6}-0.5)*{k:.6}+0.5\\,0\\,1)*255+0.5)");
    let m = matrix(color.saturation_factor());
    let names = ["r", "g", "b"];
    let mut mixer = Vec::new();
    for (out, row) in m.iter().enumerate() {
        for (from, cell) in row.iter().enumerate() {
            mixer.push(format!("{}{}={cell:.6}", names[out], names[from]));
        }
    }
    format!(
        ",format=gbrp,lutrgb=r='{}':g='{}':b='{}',colorchannelmixer={},format=yuv420p",
        curve(gains[0]),
        curve(gains[1]),
        curve(gains[2]),
        mixer.join(":")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools;

    fn graded(color: &Color, rgb: [u8; 3]) -> [u8; 3] {
        let mut pixels = [Color32::from_rgb(rgb[0], rgb[1], rgb[2])];
        Grade::new(color).apply(&mut pixels);
        let [r, g, b, _] = pixels[0].to_array();
        [r, g, b]
    }

    #[test]
    fn each_control_does_what_it_says() {
        let neutral = Color::default();
        for rgb in [[0, 0, 0], [12, 130, 250], [255, 255, 255]] {
            assert_eq!(graded(&neutral, rgb), rgb, "a neutral colour changes nothing");
        }
        assert_eq!(filters(&neutral), "");
        assert_eq!(graded(&Color { exposure: 1.0, ..neutral }, [60, 100, 200]), [120, 200, 255], "twice as bright, kept within");
        assert_eq!(graded(&Color { exposure: -1.0, ..neutral }, [60, 100, 200]), [30, 50, 100]);
        assert_eq!(graded(&Color { contrast: -100.0, ..neutral }, [0, 128, 255]), [128, 128, 128], "flat gray");
        let [r, g, b] = graded(&Color { saturation: 0.0, ..neutral }, [200, 40, 40]);
        assert!(r == g && g == b, "black and white: {r} {g} {b}");
        let [r, _, b] = graded(&Color { temperature: 100.0, ..neutral }, [100, 100, 100]);
        assert!(r > 100 && b < 100, "warmer: {r} {b}");
    }

    /// The preview's tables against ffmpeg's filters on every value of every channel, and on colours.
    #[test]
    fn ffmpeg_does_what_the_preview_shows() {
        if tools::check(tools::FFMPEG).is_err() {
            return;
        }
        // A picture of 256 × 4 pixels: gray, red, green and blue ramps; then colours.
        let mut input: Vec<[u8; 3]> = Vec::new();
        for kind in 0..4 {
            for v in 0..=255u8 {
                input.push(match kind {
                    0 => [v, v, v],
                    1 => [v, 30, 200],
                    2 => [90, v, 10],
                    _ => [250, 120, v],
                });
            }
        }
        let bytes: Vec<u8> = input.iter().flatten().copied().collect();
        let colors = [
            Color { exposure: 0.7, contrast: 25.0, saturation: 140.0, temperature: 40.0 },
            Color { exposure: -0.4, contrast: -30.0, saturation: 30.0, temperature: -80.0 },
            Color { exposure: 0.0, contrast: 0.0, saturation: 0.0, temperature: 0.0 },
        ];
        for color in colors {
            let chain = format!("format=rgb24{},format=rgb24", filters(&color).replace(",format=yuv420p", ""));
            let mut child = tools::command(tools::FFMPEG)
                .args(["-v", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "256x4", "-i", "pipe:0", "-vf", &chain, "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let feed = bytes.clone();
            let writer = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &feed));
            let output = child.wait_with_output().unwrap();
            writer.join().unwrap().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.stdout.len(), bytes.len());
            let mut worst = 0;
            for (n, (rgb, got)) in input.iter().zip(output.stdout.chunks_exact(3)).enumerate() {
                let want = graded(&color, *rgb);
                for c in 0..3 {
                    let off = (got[c] as i32 - want[c] as i32).abs();
                    worst = worst.max(off);
                    assert!(off <= 1, "{color:?} pixel {n} {rgb:?}: ffmpeg {got:?}, preview {want:?}");
                }
            }
            eprintln!("{color:?}: at most {worst} apart");
        }
    }
}

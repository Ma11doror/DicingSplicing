//! Source file information from ffprobe.

use crate::core::tools;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    fn parse(s: &str) -> Option<Self> {
        let (num, den) = s.split_once('/')?;
        let (num, den) = (num.parse().ok()?, den.parse().ok()?);
        (num > 0 && den > 0).then_some(Self { num, den })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: f64,
    /// Display size: rotation metadata is applied (ffmpeg auto-rotates when decoding).
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    pub has_audio: bool,
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let output = tools::command(tools::FFPROBE)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .context("failed to run ffprobe")?;
    if !output.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    parse(&output.stdout)
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
    #[serde(default)]
    tags: Tags,
    #[serde(default)]
    disposition: Disposition,
}

#[derive(Deserialize)]
struct SideData {
    rotation: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Tags {
    rotate: Option<String>,
}

#[derive(Deserialize, Default)]
struct Disposition {
    #[serde(default)]
    attached_pic: u8,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

fn parse(json: &[u8]) -> Result<MediaInfo> {
    let probe: ProbeOutput = serde_json::from_slice(json).context("unexpected ffprobe output")?;
    let is = |s: &ProbeStream, kind: &str| s.codec_type.as_deref() == Some(kind);
    // Cover art in some containers is a "video" stream too.
    let video = probe
        .streams
        .iter()
        .find(|s| is(s, "video") && s.disposition.attached_pic == 0)
        .context("the file has no video stream")?;
    let has_audio = probe.streams.iter().any(|s| is(s, "audio"));

    let (w, h) = (
        video.width.context("video stream has no width")?,
        video.height.context("video stream has no height")?,
    );
    let rotation = video
        .side_data_list
        .iter()
        .find_map(|d| d.rotation)
        .or_else(|| video.tags.rotate.as_deref().and_then(|r| r.parse().ok()))
        .unwrap_or(0.0);
    let quarter_turns = (rotation / 90.0).round() as i64;
    let (width, height) = if quarter_turns.rem_euclid(2) == 1 { (h, w) } else { (w, h) };

    let fps = [&video.avg_frame_rate, &video.r_frame_rate]
        .into_iter()
        .find_map(|r| r.as_deref().and_then(Rational::parse))
        .context("can't determine the frame rate")?;
    let duration = probe
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .or(video.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| *d > 0.0)
        .context("can't determine the duration")?;

    Ok(MediaInfo { duration, width, height, fps, has_audio })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_rotation_metadata() {
        let json = br#"{
            "streams": [
                {"codec_type": "video", "width": 1920, "height": 1080,
                 "avg_frame_rate": "30000/1001", "r_frame_rate": "30/1",
                 "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}]},
                {"codec_type": "audio"}
            ],
            "format": {"duration": "12.345000"}
        }"#;
        let info = parse(json).unwrap();
        assert_eq!((info.width, info.height), (1080, 1920));
        assert_eq!(info.fps, Rational { num: 30000, den: 1001 });
        assert!(info.has_audio);
        assert_eq!(info.duration, 12.345);
    }

    #[test]
    fn skips_cover_art_and_missing_rates() {
        let json = br#"{
            "streams": [
                {"codec_type": "video", "width": 600, "height": 600, "disposition": {"attached_pic": 1}},
                {"codec_type": "video", "width": 1280, "height": 720,
                 "avg_frame_rate": "0/0", "r_frame_rate": "25/1", "duration": "3.5"}
            ]
        }"#;
        let info = parse(json).unwrap();
        assert_eq!((info.width, info.height), (1280, 720));
        assert_eq!(info.fps, Rational { num: 25, den: 1 });
        assert!(!info.has_audio);
        assert_eq!(info.duration, 3.5);
    }
}

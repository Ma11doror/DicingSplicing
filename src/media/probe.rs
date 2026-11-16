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

/// What a file is. A file of sound only has no size and no frame rate: 0 by 0 at 0 frames a second.
/// An image has a size and no length: its duration is 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: f64,
    /// Display size: rotation metadata is applied (ffmpeg auto-rotates when decoding).
    pub width: u32,
    pub height: u32,
    pub fps: Rational,
    pub has_audio: bool,
    /// It is an image: one picture, with no length of its own.
    #[serde(default)]
    pub image: bool,
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let mut info = parse(&ffprobe(path, &["-show_format", "-show_streams"])?)?;
    // A photo says how it is turned in its first frame, not in its stream: ffmpeg turns it when it
    // decodes it all the same.
    if info.image {
        let frames = ffprobe(path, &["-select_streams", "V:0", "-show_frames", "-read_intervals", "%+#1"])?;
        (info.width, info.height) = turned(info.width, info.height, first_frame_rotation(&frames));
    }
    Ok(info)
}

/// What ffprobe tells about `path` with `args`, as JSON.
fn ffprobe(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = tools::command(tools::FFPROBE)
        .args(["-v", "error", "-print_format", "json"])
        .args(args)
        .arg(path)
        .output()
        .context("failed to run ffprobe")?;
    if !output.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(output.stdout)
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
    nb_frames: Option<String>,
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
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ProbeFrames {
    #[serde(default)]
    frames: Vec<ProbeFrame>,
}

#[derive(Deserialize)]
struct ProbeFrame {
    #[serde(default)]
    side_data_list: Vec<SideData>,
}

/// How far the first frame in ffprobe's `-show_frames` JSON is turned, in degrees: 0 if it says
/// nothing of it.
fn first_frame_rotation(json: &[u8]) -> f64 {
    let frames: Option<ProbeFrames> = serde_json::from_slice(json).ok();
    let first = frames.and_then(|frames| frames.frames.into_iter().next());
    first.and_then(|frame| frame.side_data_list.iter().find_map(|d| d.rotation)).unwrap_or(0.0)
}

/// The size of a picture of `width` by `height` turned by `rotation` degrees, as it is shown.
fn turned(width: u32, height: u32, rotation: f64) -> (u32, u32) {
    let quarter_turns = (rotation / 90.0).round() as i64;
    if quarter_turns.rem_euclid(2) == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

impl MediaInfo {
    /// It has a picture, and is not sound only.
    pub fn has_video(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

fn parse(json: &[u8]) -> Result<MediaInfo> {
    let probe: ProbeOutput = serde_json::from_slice(json).context("unexpected ffprobe output")?;
    let is = |s: &ProbeStream, kind: &str| s.codec_type.as_deref() == Some(kind);
    let has_audio = probe.streams.iter().any(|s| is(s, "audio"));
    // Cover art in some containers is a "video" stream too.
    let Some(video) = probe.streams.iter().find(|s| is(s, "video") && s.disposition.attached_pic == 0) else {
        // Sound only, a song with its cover too.
        let audio = probe.streams.iter().find(|s| is(s, "audio")).context("the file has neither a picture nor sound")?;
        let duration = probe
            .format
            .as_ref()
            .and_then(|f| f.duration.as_deref())
            .or(audio.duration.as_deref())
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| *d > 0.0)
            .context("can't determine the duration")?;
        return Ok(MediaInfo { duration, width: 0, height: 0, fps: Rational { num: 0, den: 1 }, has_audio, image: false });
    };

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
    let (width, height) = turned(w, h, rotation);

    // One picture: what ffmpeg reads as an image, and a GIF of one frame. An animated GIF is a
    // video. An image's rate is only how ffmpeg would repeat it, its length none.
    let format = probe.format.as_ref().and_then(|f| f.format_name.as_deref()).unwrap_or_default();
    let image = format == "image2" || format.ends_with("_pipe") || (format == "gif" && video.nb_frames.as_deref() == Some("1"));
    let rate = [&video.avg_frame_rate, &video.r_frame_rate].into_iter().find_map(|r| r.as_deref().and_then(Rational::parse));
    if image {
        let fps = rate.unwrap_or(Rational { num: 25, den: 1 });
        return Ok(MediaInfo { duration: 0.0, width, height, fps, has_audio: false, image: true });
    }

    let fps = rate.context("can't determine the frame rate")?;
    let duration = probe
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .or(video.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok())
        .filter(|d| *d > 0.0)
        .context("can't determine the duration")?;

    Ok(MediaInfo { duration, width, height, fps, has_audio, image: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_of_sound_only_has_no_picture() {
        // A song with its cover: the cover is no picture to play.
        let json = br#"{
            "streams": [
                {"codec_type": "audio", "duration": "182.5"},
                {"codec_type": "video", "width": 600, "height": 600, "disposition": {"attached_pic": 1}}
            ],
            "format": {"duration": "182.6"}
        }"#;
        let info = parse(json).unwrap();
        assert_eq!((info.width, info.height, info.has_audio, info.has_video()), (0, 0, true, false));
        assert_eq!(info.duration, 182.6);
        assert!(parse(br#"{"streams": [], "format": {"duration": "3"}}"#).is_err(), "nothing to play");
    }

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

    #[test]
    fn an_image_is_one_picture_with_no_length() {
        // What ffprobe 9 says of a PNG, a JPEG and GIFs: a PNG has no duration, a JPEG one frame's.
        let png = br#"{
            "streams": [{"codec_type": "video", "codec_name": "png", "width": 640, "height": 360,
                         "avg_frame_rate": "25/1", "r_frame_rate": "25/1"}],
            "format": {"format_name": "png_pipe", "nb_streams": 1}
        }"#;
        let info = parse(png).unwrap();
        assert!(info.image && info.has_video() && !info.has_audio);
        assert_eq!((info.width, info.height, info.duration), (640, 360, 0.0));
        let jpeg = br#"{
            "streams": [{"codec_type": "video", "codec_name": "mjpeg", "width": 4000, "height": 3000,
                         "avg_frame_rate": "25/1", "r_frame_rate": "25/1", "duration": "0.040000"}],
            "format": {"format_name": "image2", "duration": "0.040000"}
        }"#;
        let info = parse(jpeg).unwrap();
        assert!(info.image);
        assert_eq!((info.width, info.height, info.duration), (4000, 3000, 0.0));
        let gif = |frames: &str, duration: &str| {
            format!(
                r#"{{"streams": [{{"codec_type": "video", "codec_name": "gif", "width": 320, "height": 180,
                    "avg_frame_rate": "10/1", "r_frame_rate": "10/1", "duration": "{duration}", "nb_frames": "{frames}"}}],
                   "format": {{"format_name": "gif", "duration": "{duration}"}}}}"#
            )
        };
        let still = parse(gif("1", "0.100000").as_bytes()).unwrap();
        assert!(still.image && still.duration == 0.0, "a GIF of one frame is an image");
        let animated = parse(gif("20", "2.000000").as_bytes()).unwrap();
        assert!(!animated.image && animated.duration == 2.0, "an animated GIF is a video");
    }

    #[test]
    fn a_photo_is_turned_as_its_first_frame_says() {
        // A photo taken upright: its EXIF says to turn it, and ffmpeg 9 says so in the first frame.
        let frames = br#"{"frames": [{"media_type": "video", "width": 640, "height": 360, "side_data_list": [
            {"side_data_type": "3x3 displaymatrix", "rotation": -90},
            {"side_data_type": "EXIF metadata", "size": 14}]}]}"#;
        assert_eq!(first_frame_rotation(frames), -90.0);
        assert_eq!(turned(640, 360, -90.0), (360, 640));
        assert_eq!(turned(640, 360, 180.0), (640, 360));
        assert_eq!(first_frame_rotation(br#"{"frames": [{"media_type": "video"}]}"#), 0.0);
        assert_eq!(first_frame_rotation(b"not json"), 0.0);
    }
}

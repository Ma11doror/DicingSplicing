//! Whether a file can go out as a round video.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::process::Command;

/// What a bot may send, by the Bot API.
const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Telegram's limit is a minute. A sound track rarely ends exactly with the picture, so a file
/// exported to be exactly that long can be a little longer.
const MAX_SECONDS: f64 = 60.0;
const SECONDS_TOLERANCE: f64 = 0.5;
/// This was the old script's limit, not Telegram's.
const SHORT_SECONDS: f64 = 10.0;
const MEGABYTE: f64 = 1024.0 * 1024.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub bytes: u64,
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: ProbeFormat,
}

#[derive(Deserialize)]
struct ProbeStream {
    width: u32,
    height: u32,
}

#[derive(Deserialize, Default)]
struct ProbeFormat {
    #[serde(default)]
    duration: Option<String>,
}

impl Video {
    fn from_probe(json: &[u8], bytes: u64) -> Result<Self> {
        let probe: Probe = serde_json::from_slice(json).context("ffprobe says something unexpected")?;
        let stream = probe.streams.first().context("there is no picture in this file")?;
        let duration = probe.format.duration.and_then(|duration| duration.parse().ok()).context("ffprobe doesn't say how long this file is")?;
        Ok(Self { width: stream.width, height: stream.height, duration, bytes })
    }

    /// Asks ffprobe about the file.
    pub fn inspect(file: &Path, ffprobe: &str) -> Result<Self> {
        let bytes = fs::metadata(file).with_context(|| format!("can't read {}", file.display()))?.len();
        let mut command = Command::new(ffprobe);
        command.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height:format=duration", "-of", "json"]).arg(file);
        hide_window(&mut command);
        let output = command.output().with_context(|| format!("can't run {ffprobe}"))?;
        if !output.status.success() {
            bail!("ffprobe can't read this file: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Self::from_probe(&output.stdout, bytes)
    }

    /// The length as Telegram wants it.
    pub fn whole_seconds(&self) -> u32 {
        self.duration.round().max(1.0) as u32
    }

    /// Why this can't be sent.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.width != self.height {
            problems.push(format!("The picture is {}×{}. A circle has to be square.", self.width, self.height));
        }
        if self.duration > MAX_SECONDS + SECONDS_TOLERANCE {
            problems.push(format!("It runs {:.1} s. A circle can't be longer than {MAX_SECONDS:.0} s.", self.duration));
        }
        if self.bytes > MAX_BYTES {
            problems.push(format!("The file is {:.1} MB. A bot can send up to {:.0} MB.", self.bytes as f64 / MEGABYTE, MAX_BYTES as f64 / MEGABYTE));
        }
        problems
    }

    /// What is worth knowing, but doesn't stop anything.
    pub fn warnings(&self) -> Vec<String> {
        if self.duration < SHORT_SECONDS {
            vec![format!("It runs {:.1} s. Telegram allows it, though the old script sent nothing shorter than {SHORT_SECONDS:.0} s.", self.duration)]
        } else {
            Vec::new()
        }
    }

    /// One line about the file.
    pub fn summary(&self, name: &str) -> String {
        let seconds = self.whole_seconds();
        format!("{name} · {}:{:02} · {}×{} · {:.1} MB", seconds / 60, seconds % 60, self.width, self.height, self.bytes as f64 / MEGABYTE)
    }
}

/// No console window flashing for a program that runs in the background.
fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle() -> Video {
        Video { width: 400, height: 400, duration: 30.0, bytes: 1024 }
    }

    #[test]
    fn reads_what_ffprobe_says() {
        let json = br#"{"programs":[],"streams":[{"width":400,"height":400}],"format":{"duration":"42.500000"}}"#;
        assert_eq!(Video::from_probe(json, 7).unwrap(), Video { width: 400, height: 400, duration: 42.5, bytes: 7 });
        assert!(Video::from_probe(br#"{"streams":[],"format":{"duration":"1"}}"#, 7).is_err(), "no picture");
        assert!(Video::from_probe(br#"{"streams":[{"width":1,"height":1}],"format":{}}"#, 7).is_err(), "no length");
        assert!(Video::from_probe(b"not json", 7).is_err());
    }

    #[test]
    fn a_good_circle_has_nothing_against_it() {
        assert!(circle().problems().is_empty());
        assert!(circle().warnings().is_empty());
    }

    #[test]
    fn it_has_to_be_square() {
        let video = Video { width: 1080, height: 1920, ..circle() };
        assert_eq!(video.problems().len(), 1);
        assert!(video.problems()[0].contains("1080×1920"));
    }

    #[test]
    fn a_minute_is_the_limit_with_a_little_room_for_the_sound() {
        assert!(Video { duration: 60.0, ..circle() }.problems().is_empty());
        let exported_for_a_minute = Video { duration: 60.3, ..circle() };
        assert!(exported_for_a_minute.problems().is_empty());
        assert_eq!(exported_for_a_minute.whole_seconds(), 60);
        assert!(!Video { duration: 60.6, ..circle() }.problems().is_empty());
    }

    #[test]
    fn the_file_has_a_limit() {
        assert!(Video { bytes: MAX_BYTES, ..circle() }.problems().is_empty());
        assert!(!Video { bytes: MAX_BYTES + 1, ..circle() }.problems().is_empty());
    }

    #[test]
    fn short_is_only_a_warning() {
        let short = Video { duration: 7.0, ..circle() };
        assert!(short.problems().is_empty());
        assert_eq!(short.warnings().len(), 1);
        assert!(Video { duration: 10.0, ..circle() }.warnings().is_empty());
    }

    #[test]
    fn everything_wrong_is_said_at_once() {
        let video = Video { width: 100, height: 200, duration: 100.0, bytes: MAX_BYTES * 2 };
        assert_eq!(video.problems().len(), 3);
    }

    #[test]
    fn the_length_is_never_zero() {
        assert_eq!(Video { duration: 0.2, ..circle() }.whole_seconds(), 1);
        assert_eq!(Video { duration: 12.6, ..circle() }.whole_seconds(), 13);
    }

    #[test]
    fn one_line_about_the_file() {
        let video = Video { width: 400, height: 400, duration: 72.0, bytes: 3 * 1024 * 1024 };
        assert_eq!(video.summary("a.mp4"), "a.mp4 · 1:12 · 400×400 · 3.0 MB");
    }
}

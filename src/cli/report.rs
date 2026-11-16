//! What ds-tool tells about a project: the clips of Video 1 in order, the other tracks, and what
//! looks wrong. As text for people, and as JSON for programs.

use crate::cli::pieces::{clips, find_video_track, ROUNDING, TRACK};
use crate::core::edit::{speed_text, Sequence, Span};
use crate::core::project::{file_label, Project};
use crate::media::probe::MediaInfo;
use crate::media::timecode::format;
use crate::ui::timeline::track_name;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// The longest name the text shows whole.
const NAME_WIDTH: usize = 40;

/// A clip of Video 1, numbered as the commands number it.
pub struct Line {
    pub number: usize,
    pub name: String,
    pub start: f64,
    pub length: f64,
    pub file: String,
    /// The part of the file it plays.
    pub from: f64,
    pub to: f64,
    /// How fast it plays that part: 2 is twice as fast, so half as long. Set in the editor.
    pub speed: f64,
    /// All of its picture is in the frame, with black bars around it, as Fit makes it.
    pub bars: bool,
}

/// What `show` tells, and every command after a change.
pub struct Summary {
    pub name: String,
    pub frame: String,
    pub ratio: String,
    pub lines: Vec<Line>,
    /// The tracks other than Video 1, top first, with how many clips each has.
    pub others: Vec<(String, usize)>,
    pub export: Span,
    pub warnings: Vec<String>,
}

pub fn summary(project: &Project) -> Summary {
    let sequence = &project.sequence;
    let track = find_video_track(sequence);
    let file_of = |source| project.sources.iter().find(|it| it.id == source).map_or_else(|| "(not among the sources)".to_owned(), |it| it.path.display().to_string());
    let lines = track
        .map(|track| {
            let clips = sequence.tracks[track].clips.iter().enumerate();
            clips
                .map(|(i, clip)| Line {
                    number: i + 1,
                    name: clip.name.clone(),
                    start: clip.start,
                    length: clip.span().len(),
                    file: file_of(clip.source),
                    from: clip.source_in,
                    to: clip.source_out,
                    speed: clip.speed(),
                    bars: clip.transform.bars && clip.transform.zoom < 1.0 - ROUNDING,
                })
                .collect()
        })
        .unwrap_or_default();
    let others = (0..sequence.tracks.len()).rev().filter(|&other| Some(other) != track).map(|other| (track_name(sequence, other), sequence.tracks[other].clips.len())).collect();
    Summary {
        name: project.name.clone(),
        frame: format!("{:?}", sequence.output.preset),
        ratio: sequence.output.ratio().label(),
        lines,
        others,
        export: sequence.export,
        warnings: warnings(sequence, track),
    }
}

/// What may not be meant: black between the clips of Video 1, an export range that leaves clips out
/// or has nothing in a part of it.
fn warnings(sequence: &Sequence, track: Option<usize>) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(track) = track {
        for (i, pair) in sequence.tracks[track].clips.windows(2).enumerate() {
            let gap = pair[1].start - pair[0].end();
            if gap > ROUNDING {
                warnings.push(format!("{} of nothing between clips {} and {}: black in the video", format(gap), i + 1, i + 2));
            }
        }
    }
    if let Some(content) = sequence.content().filter(|_| !sequence.export_fits()) {
        let export = sequence.export;
        warnings.push(format!(
            "the export range is {} to {}, and the clips go from {} to {}: the video gets what is in the range",
            format(export.start),
            format(export.end),
            format(content.start),
            format(content.end)
        ));
    }
    warnings
}

impl Summary {
    /// Where the last clip of Video 1 ends.
    pub fn length(&self) -> f64 {
        self.lines.last().map_or(0.0, |line| line.start + line.length)
    }

    pub fn text(&self) -> String {
        let mut text = format!("{}: frame {} ({}), {TRACK} has {}, {} long.", self.name, self.frame, self.ratio, clips(self.lines.len()), format(self.length()));
        if !self.lines.is_empty() {
            let width = self.lines.iter().map(|line| line.name.chars().count()).max().unwrap_or(0).clamp(4, NAME_WIDTH);
            text.push_str(&format!("\n  {:>3}  {:<8}  {:<8}  {:<width$}  plays", "#", "start", "length", "name"));
            for line in &self.lines {
                let name = shortened(&line.name, width);
                let file = file_label(Path::new(&line.file));
                let at = if line.speed == 1.0 { String::new() } else { format!(", at {}", speed_text(line.speed)) };
                let bars = if line.bars { ", with black bars" } else { "" };
                text.push_str(&format!("\n  {:>3}  {}  {}  {name:<width$}  {file} {} to {}{at}{bars}", line.number, format(line.start), format(line.length), format(line.from), format(line.to)));
            }
        }
        if !self.others.is_empty() {
            let others: Vec<String> = self.others.iter().map(|(name, count)| format!("{name} ({})", clips(*count))).collect();
            text.push_str(&format!("\nOther tracks: {}. ds-tool changes {TRACK} only.", others.join(", ")));
        }
        text.push_str(&format!("\nExport range: {} to {}.", format(self.export.start), format(self.export.end)));
        for warning in &self.warnings {
            text.push_str(&format!("\nWarning: {warning}."));
        }
        text
    }

    /// The same, for programs: times in seconds, files with their whole paths.
    pub fn json(&self) -> Value {
        let clips: Vec<Value> = self
            .lines
            .iter()
            .map(|line| json!({"number": line.number, "name": line.name, "start": line.start, "length": line.length, "file": line.file, "from": line.from, "to": line.to, "speed": line.speed, "bars": line.bars}))
            .collect();
        let others: Vec<Value> = self.others.iter().map(|(name, count)| json!({"name": name, "clips": count})).collect();
        json!({
            "name": self.name,
            "frame": self.frame,
            "ratio": self.ratio,
            "track": TRACK,
            "clips": clips,
            "length": self.length(),
            "export": {"start": self.export.start, "end": self.export.end},
            "other_tracks": others,
            "warnings": self.warnings,
        })
    }
}

/// A name cut to `width` characters, with an ellipsis when it is longer.
fn shortened(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        return name.to_owned();
    }
    let mut short: String = name.chars().take(width - 1).collect();
    short.push('…');
    short
}

/// What is wrong with the project as the files on the disk are now: recordings that are not there or
/// can't be read, clips that play past the end of theirs, clips that overlap. `probe` reads a file.
pub fn problems(project: &Project, probe: &dyn Fn(&Path) -> Result<MediaInfo>) -> Vec<String> {
    let sequence = &project.sequence;
    let mut problems = Vec::new();
    let mut lengths = HashMap::new();
    for source in project.sources.iter().filter(|source| sequence.uses(source.id) > 0) {
        if !source.path.is_file() {
            problems.push(format!("{} is not there", source.path.display()));
            continue;
        }
        match probe(&source.path) {
            Ok(info) => {
                lengths.insert(source.id, info.duration);
            }
            Err(error) => problems.push(format!("{} can't be read: {error:#}", source.path.display())),
        }
    }
    for (track, clips) in sequence.tracks.iter().enumerate().map(|(track, it)| (track, &it.clips)) {
        let at = |i: usize| format!("{}, clip {}", track_name(sequence, track), i + 1);
        for (i, clip) in clips.iter().enumerate() {
            match project.sources.iter().find(|source| source.id == clip.source) {
                None => problems.push(format!("{} plays a source the project does not have", at(i))),
                Some(source) => {
                    if let Some(&length) = lengths.get(&clip.source).filter(|&&length| clip.source_out > length + ROUNDING) {
                        problems.push(format!(
                            "{} plays {} up to {}, and the file is {} long: the editor cuts the clip there",
                            at(i),
                            source.label(),
                            format(clip.source_out),
                            format(length)
                        ));
                    }
                }
            }
            if i > 0 && clip.start < clips[i - 1].end() - ROUNDING {
                problems.push(format!("{} starts before the clip before it ends", at(i)));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::pieces::tests::{piece, video};
    use crate::cli::pieces::{new_project, put};
    use crate::core::preset::PresetId;

    fn lesson() -> Project {
        let mut project = new_project("Lesson 3", PresetId::Landscape);
        put(&mut project, &piece("intro.mp4", None, None, Some("Intro")), &video(12.5), None).unwrap();
        put(&mut project, &piece("install.mp4", Some(12.0), Some(245.0), Some("Installing the program, step by step, with all the settings")), &video(300.0), None).unwrap();
        project
    }

    #[test]
    fn the_text_lists_the_clips_in_order() {
        let text = summary(&lesson()).text();
        let pad = |name: &str| format!("{name:<40}");
        let expected = [
            "Lesson 3: frame Landscape (16:9), Video 1 has 2 clips, 04:05.50 long.".to_owned(),
            format!("    #  start     length    {}  plays", pad("name")),
            format!("    1  00:00.00  00:12.50  {}  intro.mp4 00:00.00 to 00:12.50", pad("Intro")),
            "    2  00:12.50  03:53.00  Installing the program, step by step, w…  install.mp4 00:12.00 to 04:05.00".to_owned(),
            "Export range: 00:00.00 to 04:05.50.".to_owned(),
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn gaps_other_tracks_and_a_narrow_export_range_are_told() {
        let mut project = lesson();
        project.sequence.tracks[0].clips[1].start += 2.0;
        project.sequence.tracks[0].clips[0].transform = crate::media::geometry::Transform { zoom: 0.9, bars: true, ..Default::default() };
        project.sequence.export = Span { start: 0.0, end: 100.0 };
        project.sequence.add_track(0, crate::core::edit::TrackKind::Audio);
        let summary = summary(&project);
        assert_eq!(summary.warnings, [
            "00:02.00 of nothing between clips 1 and 2: black in the video",
            "the export range is 00:00.00 to 01:40.00, and the clips go from 00:00.00 to 04:07.50: the video gets what is in the range"
        ]);
        assert!(summary.text().contains("\nOther tracks: Audio 1 (no clips). ds-tool changes Video 1 only."));
        assert!(summary.text().contains("intro.mp4 00:00.00 to 00:12.50, with black bars\n"));
        let json = summary.json();
        assert_eq!(json["clips"][1]["start"], 14.5);
        assert_eq!((json["clips"][0]["bars"].as_bool(), json["clips"][1]["bars"].as_bool()), (Some(true), Some(false)));
        assert_eq!(json["clips"][1]["file"], "C:/rec/install.mp4");
        assert_eq!(json["other_tracks"][0]["name"], "Audio 1");
        assert_eq!(json["length"], 247.5);
        assert_eq!(json["clips"][0]["speed"], 1.0);
    }

    #[test]
    fn a_clip_at_another_speed_says_so() {
        let mut project = lesson();
        let install = crate::core::edit::ClipId { track: 0, index: 1 };
        assert!(project.sequence.set_speed(install, 2.0));
        let summary = summary(&project);
        assert!(summary.text().contains("install.mp4 00:12.00 to 04:05.00, at ×2\n") || summary.text().ends_with("install.mp4 00:12.00 to 04:05.00, at ×2"), "{}", summary.text());
        assert!(summary.text().contains("  00:12.50  01:56.50  "), "half as long: {}", summary.text());
        assert_eq!(summary.json()["clips"][1]["speed"], 2.0);
    }

    #[test]
    fn problems_are_found_in_the_files_and_the_clips() {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_report_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut project = new_project("Lesson", PresetId::Landscape);
        let (here, gone) = (dir.join("here.mp4"), dir.join("gone.mp4"));
        std::fs::write(&here, "").unwrap();
        for file in [&here, &gone] {
            let piece = crate::cli::pieces::Piece { file: file.clone(), from: None, to: Some(60.0), name: None, fit: false };
            put(&mut project, &piece, &video(60.0), None).unwrap();
        }
        // The file turned out shorter than when it was put in.
        let probe = |_: &Path| -> Result<MediaInfo> { Ok(video(50.0)) };
        assert_eq!(problems(&project, &probe), [
            format!("{} is not there", gone.display()),
            "Video 1, clip 1 plays here.mp4 up to 01:00.00, and the file is 00:50.00 long: the editor cuts the clip there".to_owned(),
        ]);
        project.sequence.tracks[0].clips[1].start = 59.0;
        assert!(problems(&project, &probe).contains(&"Video 1, clip 2 starts before the clip before it ends".to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

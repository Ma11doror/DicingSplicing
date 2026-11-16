//! What ds-tool's commands do to a project: lay pieces of recordings out on Video 1, one after
//! another, and change them there. Each change goes through the editor's model, which keeps its
//! rules: clips in order without overlaps, the clips chained to them along, the export range fitted.

use crate::core::edit::{Clip, ClipId, Sequence, Span, Track, TrackKind};
use crate::core::export::{ExportSettings, FrameRate, EXPORT_PRESETS};
use crate::core::preset::{Output, PresetId};
use crate::core::project::{self, Project, ViewState, VERSION};
use crate::media::geometry::{Mask, Transform};
use crate::media::probe::MediaInfo;
use crate::media::timecode;
use crate::ui::timeline::track_name;
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The track the commands work on: the top video track, the one seen over the others.
pub const TRACK: &str = "Video 1";
/// How far past the end of a recording a piece may be asked to go: what rounding does to lengths.
pub const ROUNDING: f64 = 0.001;
/// How far apart two proportions may be and still be the same, as a ratio's logarithm: about 1%.
const SAME_PROPORTIONS: f64 = 0.01;
/// The frames `--frame` can name, by their names in the project file.
const FRAMES: [PresetId; 9] = [
    PresetId::Landscape,
    PresetId::Vertical,
    PresetId::Square,
    PresetId::Tablet,
    PresetId::Photo,
    PresetId::Classic,
    PresetId::Post,
    PresetId::Cinema,
    PresetId::TelegramCircle,
];
const NO_ROOM: &str = "a clip chained to one of the clips that would move has no room on its own track. Change it in the editor";

/// A piece of a recording, as a command asks for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub file: PathBuf,
    /// Where in the recording it starts and ends; without them, the recording's start and end.
    pub from: Option<f64>,
    pub to: Option<f64>,
    /// What the clip is called on the timeline; without it, the recording's file name.
    pub name: Option<String>,
    /// All of the picture in the frame, with black bars where their proportions differ, as Fit does
    /// in the editor. Without it the picture fills the frame, as a new clip's does there.
    pub fit: bool,
}

/// A frame by its name in the project file, such as Landscape, in any case.
pub fn frame_named(name: &str) -> Result<PresetId> {
    FRAMES
        .into_iter()
        .find(|frame| format!("{frame:?}").eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| anyhow!("there is no frame {name:?}; there are {}", FRAMES.map(|frame| format!("{frame:?}")).join(", ")))
}

/// A project with nothing in it yet: one video track, the frame `frame`, and the export settings of
/// the editor's preset for that frame, or of its YouTube preset when there is none for it.
pub fn new_project(name: &str, frame: PresetId) -> Project {
    let mut output = Output { preset: frame, ..Output::default() };
    let mut settings = ExportSettings::default();
    let preset = EXPORT_PRESETS.iter().find(|preset| preset.frame == Some(frame)).or_else(|| EXPORT_PRESETS.iter().find(|preset| preset.name == "YouTube"));
    if let Some(preset) = preset {
        preset.apply(&mut output, &mut settings);
    }
    output.preset = frame;
    let sequence = Sequence { tracks: vec![Track::default()], export: Span { start: 0.0, end: 0.0 }, output, settings };
    Project { version: VERSION, name: name.to_owned(), sources: Vec::new(), sequence, view: ViewState::default() }
}

/// Where Video 1 is among the tracks, if the project has a video track.
pub fn find_video_track(sequence: &Sequence) -> Option<usize> {
    (0..sequence.tracks.len()).rev().find(|&track| sequence.tracks[track].kind == TrackKind::Video)
}

/// Puts `piece`, which `info` tells about, on Video 1: before clip `before`, or after the last one
/// with None. The clips from there on move later by its length. Returns its number.
pub fn put(project: &mut Project, piece: &Piece, info: &MediaInfo, before: Option<usize>) -> Result<usize> {
    let label = project::file_label(&piece.file);
    if !info.has_video() {
        bail!("{label} has no picture: ds-tool puts videos on {TRACK}");
    }
    let part = part_of(&label, piece.from, piece.to, Span { start: 0.0, end: info.duration }, info.duration)?;
    let track = match find_video_track(&project.sequence) {
        Some(track) => track,
        None => project.sequence.add_track(project.sequence.tracks.len(), TrackKind::Video),
    };
    let index = match before {
        Some(number) => index_of(&project.sequence, track, number)?,
        None => project.sequence.tracks[track].clips.len(),
    };
    // Named after its file: a source without a name would be named after its first clip.
    let (source, _) = project::add_source(&mut project.sources, piece.file.clone(), Some(label.clone()), BTreeMap::new());
    let mut clip = Clip { source_in: part.start, source_out: part.end, ..Clip::of(source, piece.name.clone().unwrap_or(label), info.duration) };
    if piece.fit {
        clip.transform = whole_picture(info, project.sequence.output);
    }
    let id = project.sequence.ripple_insert(track, index, clip).ok_or_else(|| anyhow!("the clips after it can't move: {NO_ROOM}"))?;
    Ok(id.index + 1)
}

/// Takes clip `number` off Video 1; the clips after it move back by its length. Returns the clip.
pub fn delete(project: &mut Project, number: usize) -> Result<Clip> {
    let id = clip_id(&project.sequence, number)?;
    refuse_chained(&project.sequence, id, number)?;
    let clip = project.sequence.clip(id).clone();
    project.sequence.ripple_delete(id);
    Ok(clip)
}

/// Makes clip `from` of Video 1 the clip `to`; the clips stay one after another. Returns the clip.
pub fn reorder(project: &mut Project, from: usize, to: usize) -> Result<Clip> {
    let id = clip_id(&project.sequence, from)?;
    let target = clip_id(&project.sequence, to)?;
    refuse_chained(&project.sequence, id, from)?;
    let clip = project.sequence.clip(id).clone();
    project.sequence.ripple_move(id.track, id.index, target.index).ok_or_else(|| anyhow!("the clips can't move: {NO_ROOM}"))?;
    Ok(clip)
}

/// Makes clip `number` of Video 1 play another part of its recording, from `from` to `to`, either
/// of them as it was when not given. The clips after it follow. Returns the clip before and after.
pub fn retime(project: &mut Project, number: usize, from: Option<f64>, to: Option<f64>) -> Result<(Clip, Clip)> {
    let id = clip_id(&project.sequence, number)?;
    let clip = project.sequence.clip(id).clone();
    let label = project.sources.iter().find(|source| source.id == clip.source).map_or_else(|| clip.name.clone(), |source| source.label());
    let part = part_of(&label, from, to, Span { start: clip.source_in, end: clip.source_out }, clip.source_duration)?;
    refuse_chained(&project.sequence, id, number)?;
    if !project.sequence.ripple_retime(id, part) {
        bail!("the clips after it can't move: {NO_ROOM}");
    }
    Ok((clip, project.sequence.clip(id).clone()))
}

/// Cuts clip `number` of Video 1 in two where its recording is at `at`, as the Blade does in the
/// editor: the clip keeps what is before the cut, and a new clip right after it plays the rest, with
/// the same name, framing and sound. Nothing moves. Returns the clip as it was.
pub fn blade(project: &mut Project, number: usize, at: f64) -> Result<Clip> {
    let id = clip_id(&project.sequence, number)?;
    let clip = project.sequence.clip(id).clone();
    let frame = frame_of(&project.sequence);
    // Where the recording is at `at` on the timeline: a clip at another speed gets there sooner or
    // later. Each part is a frame of the result or more.
    let t = clip.time_of(at);
    if t - clip.start < frame - ROUNDING || clip.end() - t < frame - ROUNDING {
        let label = project.sources.iter().find(|source| source.id == clip.source).map_or_else(|| clip.name.clone(), |source| source.label());
        bail!(
            "clip {number} plays {label} from {} to {}: a cut at {} has to be inside that, a frame or more from either end",
            timecode::format(clip.source_in),
            timecode::format(clip.source_out),
            timecode::format(at)
        );
    }
    refuse_chained(&project.sequence, id, number)?;
    project.sequence.split(id, t, frame).ok_or_else(|| anyhow!("clip {number} can not be cut there"))?;
    Ok(clip)
}

/// How long a frame of the result is: the shortest part the Blade leaves, as in the editor.
fn frame_of(sequence: &Sequence) -> f64 {
    match sequence.settings.frame_rate {
        FrameRate::Fixed(fps) if fps > 0.0 => 1.0 / fps,
        _ => 1.0 / 30.0,
    }
}

/// The framing that shows all of the picture `info` tells about in the frame of `output`, as Fit does
/// in the editor. A picture of the frame's own proportions keeps the framing of a new clip: there are
/// no bars to add.
fn whole_picture(info: &MediaInfo, output: Output) -> Transform {
    let (width, height) = (f64::from(info.width), f64::from(info.height));
    let outline = output.outline();
    if outline.mask == Mask::Rect && (width / height / outline.aspect).ln().abs() < SAME_PROPORTIONS {
        return Transform::default();
    }
    Transform::default().showing_whole_picture(width, height, outline)
}

/// The part of a recording `duration` seconds long that `from` and `to` ask for, each of them as in
/// `default` when not given.
fn part_of(label: &str, from: Option<f64>, to: Option<f64>, default: Span, duration: f64) -> Result<Span> {
    let part = Span { start: from.unwrap_or(default.start), end: to.unwrap_or(default.end) };
    if part.end > duration + ROUNDING {
        bail!("{label} is {} long: it has nothing at {}", timecode::format(duration), timecode::format(part.end));
    }
    let part = Span { end: part.end.min(duration), ..part };
    if part.start >= part.end {
        bail!("the piece of {label} would start at {} and end at {}: its start has to come first", timecode::format(part.start), timecode::format(part.end));
    }
    Ok(part)
}

/// Where clip `number` of Video 1 is, 1 being the first.
fn clip_id(sequence: &Sequence, number: usize) -> Result<ClipId> {
    let track = find_video_track(sequence).ok_or_else(|| anyhow!("the project has no video track"))?;
    Ok(ClipId { track, index: index_of(sequence, track, number)? })
}

fn index_of(sequence: &Sequence, track: usize, number: usize) -> Result<usize> {
    let count = sequence.tracks[track].clips.len();
    match number {
        0 => bail!("clips are numbered from 1"),
        number if number > count => bail!("there is no clip {number}: {TRACK} has {}", clips(count)),
        number => Ok(number - 1),
    }
}

/// How many clips, in words.
pub fn clips(count: usize) -> String {
    match count {
        0 => "no clips".to_owned(),
        1 => "1 clip".to_owned(),
        count => format!("{count} clips"),
    }
}

/// A clip chained to others, as a picture to its detached sound, would be parted from them.
fn refuse_chained(sequence: &Sequence, id: ClipId, number: usize) -> Result<()> {
    if sequence.is_chained(id) {
        let mut tracks: Vec<String> = sequence.group_of(id).into_iter().filter(|&other| other != id).map(|other| track_name(sequence, other.track)).collect();
        tracks.dedup();
        bail!("clip {number} is chained to a clip on {}, and ds-tool would part them. Change it in the editor", tracks.join(", "));
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::core::export::{H264Profile, Rate};
    use crate::media::probe::Rational;

    pub fn video(duration: f64) -> MediaInfo {
        MediaInfo { duration, width: 1920, height: 1080, fps: Rational { num: 30, den: 1 }, has_audio: true }
    }

    pub fn piece(file: &str, from: Option<f64>, to: Option<f64>, name: Option<&str>) -> Piece {
        Piece { file: PathBuf::from(format!("C:/rec/{file}")), from, to, name: name.map(str::to_owned), fit: false }
    }

    /// What Video 1 plays: each clip's name, start and length.
    pub fn laid_out(project: &Project) -> Vec<(String, f64, f64)> {
        let track = find_video_track(&project.sequence).unwrap();
        project.sequence.tracks[track].clips.iter().map(|clip| (clip.name.clone(), clip.start, clip.span().len())).collect()
    }

    fn lesson() -> Project {
        let mut project = new_project("Lesson", PresetId::Landscape);
        put(&mut project, &piece("intro.mp4", None, None, Some("Intro")), &video(12.5), None).unwrap();
        put(&mut project, &piece("install.mp4", Some(12.0), Some(245.0), None), &video(300.0), None).unwrap();
        put(&mut project, &piece("result.mp4", Some(3.5), None, Some("The result")), &video(95.0), None).unwrap();
        project
    }

    #[test]
    fn a_new_project_has_one_empty_video_track_and_settings_for_its_frame() {
        let project = new_project("Lesson", PresetId::Landscape);
        assert_eq!(project.sequence.tracks.len(), 1);
        assert!(project.sequence.tracks[0].clips.is_empty());
        assert_eq!(project.sequence.output.preset, PresetId::Landscape);
        assert_eq!((project.sequence.settings.profile, project.sequence.settings.rate), (H264Profile::High, Rate::Quality(18)), "the YouTube preset");
        let vertical = new_project("Short", frame_named("vertical").unwrap());
        assert_eq!(vertical.sequence.output.preset, PresetId::Vertical);
        let square = new_project("Square", frame_named("Square").unwrap());
        assert_eq!(square.sequence.output.preset, PresetId::Square, "its own frame, with the YouTube settings");
        assert!(frame_named("Wide").unwrap_err().to_string().starts_with("there is no frame \"Wide\"; there are Landscape, Vertical"));
    }

    #[test]
    fn pieces_go_one_after_another() {
        let project = lesson();
        let laid = laid_out(&project);
        assert_eq!(laid, [("Intro".to_owned(), 0.0, 12.5), ("install.mp4".to_owned(), 12.5, 233.0), ("The result".to_owned(), 245.5, 91.5)]);
        assert_eq!(project.sequence.export, Span { start: 0.0, end: 337.0 });
        // Sources keep their files' names, whatever the clips are called.
        assert_eq!(project.sources.iter().map(|source| source.label()).collect::<Vec<_>>(), ["intro.mp4", "install.mp4", "result.mp4"]);
    }

    #[test]
    fn a_piece_put_before_a_clip_moves_it_and_the_rest_on() {
        let mut project = lesson();
        let number = put(&mut project, &piece("intro.mp4", Some(2.0), Some(4.0), Some("Again")), &video(12.5), Some(2)).unwrap();
        assert_eq!(number, 2);
        let laid = laid_out(&project);
        assert_eq!(laid.iter().map(|(name, start, _)| (name.as_str(), *start)).collect::<Vec<_>>(), [("Intro", 0.0), ("Again", 12.5), ("install.mp4", 14.5), ("The result", 247.5)]);
        assert_eq!(project.sources.len(), 3, "the same file is one source");
        assert_eq!(put(&mut project, &piece("a.mp4", None, None, None), &video(5.0), Some(9)).unwrap_err().to_string(), "there is no clip 9: Video 1 has 4 clips");
    }

    #[test]
    fn pieces_that_are_not_in_their_recordings_are_refused() {
        let mut project = lesson();
        let error = |project: &mut Project, piece: Piece, info: MediaInfo| put(project, &piece, &info, None).unwrap_err().to_string();
        assert_eq!(error(&mut project, piece("a.mp4", None, Some(70.0), None), video(60.0)), "a.mp4 is 01:00.00 long: it has nothing at 01:10.00");
        assert_eq!(error(&mut project, piece("a.mp4", Some(30.0), Some(20.0), None), video(60.0)), "the piece of a.mp4 would start at 00:30.00 and end at 00:20.00: its start has to come first");
        let sound = MediaInfo { width: 0, height: 0, ..video(60.0) };
        assert_eq!(error(&mut project, piece("music.m4a", None, None, None), sound), "music.m4a has no picture: ds-tool puts videos on Video 1");
        // A hair past the end is the end: rounding.
        put(&mut project, &piece("a.mp4", None, Some(60.0004), None), &video(60.0), None).unwrap();
        assert_eq!(laid_out(&project).last().unwrap().2, 60.0);
    }

    #[test]
    fn fit_shows_all_of_a_picture_of_other_proportions() {
        let mut project = new_project("Lesson", PresetId::Landscape);
        let fitted = |file: &str| Piece { fit: true, ..piece(file, None, None, None) };
        let sized = |width, height| MediaInfo { width, height, ..video(5.0) };
        put(&mut project, &fitted("screen.mp4"), &sized(1440, 900), None).unwrap();
        put(&mut project, &fitted("phone.mp4"), &sized(1080, 1920), None).unwrap();
        put(&mut project, &fitted("camera.mp4"), &sized(1920, 1080), None).unwrap();
        put(&mut project, &piece("screen.mp4", None, None, None), &sized(1440, 900), None).unwrap();
        let framing: Vec<Transform> = project.sequence.tracks[0].clips.iter().map(|clip| clip.transform).collect();
        assert!(framing[0].bars && (framing[0].zoom - 0.9).abs() < 1e-9, "16:10 in 16:9: bars at the sides");
        assert!(framing[1].bars && (framing[1].zoom - 0.31640625).abs() < 1e-9, "upright in 16:9: wide bars");
        assert_eq!(framing[2], Transform::default(), "16:9 in 16:9: nothing to fit");
        assert_eq!(framing[3], Transform::default(), "without fit: as a new clip in the editor");
        let mut circle = new_project("Circle", PresetId::TelegramCircle);
        put(&mut circle, &fitted("camera.mp4"), &sized(1920, 1080), None).unwrap();
        let round = circle.sequence.tracks[0].clips[0].transform;
        assert!(round.bars && round.zoom < 1.0, "in the circle, all of the picture, corners too");
    }

    #[test]
    fn blade_cuts_a_clip_in_two_and_nothing_moves() {
        let mut project = lesson();
        let cut = blade(&mut project, 2, 60.0).unwrap();
        assert_eq!(cut.name, "install.mp4");
        let laid = laid_out(&project);
        assert_eq!(laid.iter().map(|(name, start, length)| (name.as_str(), *start, *length)).collect::<Vec<_>>(), [
            ("Intro", 0.0, 12.5),
            ("install.mp4", 12.5, 48.0),
            ("install.mp4", 60.5, 185.0),
            ("The result", 245.5, 91.5)
        ]);
        let clips = &project.sequence.tracks[0].clips;
        assert_eq!((clips[1].source_in, clips[1].source_out, clips[2].source_in, clips[2].source_out), (12.0, 60.0, 60.0, 245.0));
        assert_eq!(project.sequence.export, Span { start: 0.0, end: 337.0 });
        // A part out of the middle of a recording: a cut at its other end, and a delete.
        blade(&mut project, 3, 90.0).unwrap();
        delete(&mut project, 3).unwrap();
        let clips = &project.sequence.tracks[0].clips;
        assert_eq!(clips.iter().map(|clip| (clip.start, clip.source_in)).collect::<Vec<_>>(), [(0.0, 0.0), (12.5, 12.0), (60.5, 90.0), (215.5, 3.5)]);
        let edge = "clip 2 plays install.mp4 from 00:12.00 to 01:00.00: a cut at 00:12.00 has to be inside that, a frame or more from either end";
        assert_eq!(blade(&mut project, 2, 12.0).unwrap_err().to_string(), edge);
        assert!(blade(&mut project, 2, 100.0).is_err(), "past the part it plays");
        assert_eq!(blade(&mut project, 9, 1.0).unwrap_err().to_string(), "there is no clip 9: Video 1 has 4 clips");
        let track = find_video_track(&project.sequence).unwrap();
        project.sequence.detach_sound(ClipId { track, index: 0 }).unwrap();
        assert!(blade(&mut project, 1, 5.0).unwrap_err().to_string().starts_with("clip 1 is chained to a clip on Audio 1"));
    }

    #[test]
    fn a_clip_sped_up_in_the_editor_is_cut_and_trimmed_by_the_times_of_its_recording() {
        let mut project = lesson();
        // The editor made the installation twice as fast: 12..245 s of it take 116.5 s.
        let install = ClipId { track: find_video_track(&project.sequence).unwrap(), index: 1 };
        assert!(project.sequence.set_speed(install, 2.0));
        assert_eq!(laid_out(&project)[2], ("The result".to_owned(), 129.0, 91.5));
        // At 1:00 of the recording: 24 s into the clip.
        blade(&mut project, 2, 60.0).unwrap();
        let clips = &project.sequence.tracks[0].clips;
        assert_eq!((clips[1].span().len(), clips[1].source_out, clips[2].start, clips[2].source_in), (24.0, 60.0, 36.5, 60.0));
        assert_eq!(clips[2].speed(), 2.0, "both parts keep the speed");
        let edge = "clip 3 plays install.mp4 from 01:00.00 to 04:05.00: a cut at 01:00.01 has to be inside that, a frame or more from either end";
        assert_eq!(blade(&mut project, 3, 60.01).unwrap_err().to_string(), edge, "less than a frame of the result after its start");
        // Trimmed to 60..100 s of the recording: 20 s at that speed, and the next clip follows.
        retime(&mut project, 3, None, Some(100.0)).unwrap();
        assert_eq!(laid_out(&project)[2..], [("install.mp4".to_owned(), 36.5, 20.0), ("The result".to_owned(), 56.5, 91.5)]);
    }

    #[test]
    fn delete_move_and_trim_keep_the_clips_one_after_another() {
        let mut project = lesson();
        let moved = reorder(&mut project, 3, 1).unwrap();
        assert_eq!(moved.name, "The result");
        assert_eq!(laid_out(&project).iter().map(|(name, start, _)| (name.as_str(), *start)).collect::<Vec<_>>(), [("The result", 0.0), ("Intro", 91.5), ("install.mp4", 104.0)]);
        let (before, after) = retime(&mut project, 2, None, Some(10.0)).unwrap();
        assert_eq!((before.source_out, after.source_out), (12.5, 10.0));
        assert_eq!(laid_out(&project)[2].1, 101.5, "the next clip moved back by 2.5 s");
        let deleted = delete(&mut project, 1).unwrap();
        assert_eq!(deleted.name, "The result");
        assert_eq!(laid_out(&project).iter().map(|(_, start, _)| *start).collect::<Vec<_>>(), [0.0, 10.0]);
        assert_eq!(project.sequence.export, Span { start: 0.0, end: 243.0 });
        assert_eq!(retime(&mut project, 1, Some(11.0), None).unwrap_err().to_string(), "the piece of intro.mp4 would start at 00:11.00 and end at 00:10.00: its start has to come first");
        assert_eq!(delete(&mut project, 3).unwrap_err().to_string(), "there is no clip 3: Video 1 has 2 clips");
        assert_eq!(reorder(&mut project, 1, 0).unwrap_err().to_string(), "clips are numbered from 1");
    }

    #[test]
    fn chained_clips_are_left_to_the_editor() {
        let mut project = lesson();
        let track = find_video_track(&project.sequence).unwrap();
        project.sequence.detach_sound(ClipId { track, index: 1 }).unwrap();
        let refused = "clip 2 is chained to a clip on Audio 1, and ds-tool would part them. Change it in the editor";
        assert_eq!(delete(&mut project, 2).unwrap_err().to_string(), refused);
        assert_eq!(reorder(&mut project, 2, 1).unwrap_err().to_string(), refused);
        assert_eq!(retime(&mut project, 2, Some(20.0), None).unwrap_err().to_string(), refused);
        // The clips around it are free to go.
        delete(&mut project, 3).unwrap();
    }
}

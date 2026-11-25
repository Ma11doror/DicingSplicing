//! Rendering the edit to a file with ffmpeg, always from the original source.

use crate::core::edit::{Clip, ClipId, Curve, Edge, Run, Sequence, Sizes, SourceId, Span, TrackKind};
use crate::core::export::{ExportSettings, FrameRate, Rate, VideoCodec, EVEN_LOUDNESS};
use crate::core::preset::Preset;
use crate::core::tools;
use crate::media::geometry::{Mask, Outline, Transform};
use crate::media::{color, Notify, SourceFile, SourceFiles, OVER_BLACK};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// How far before the start the fast seek lands; decoding covers the rest exactly.
const SEEK_MARGIN: f64 = 3.0;
/// Windows takes command lines of up to 32767 characters: a filter graph longer than this goes to
/// ffmpeg in a file.
const GRAPH_INLINE_LIMIT: usize = 16_000;
/// What Telegram accepts as a video note, from tgCircleGen and the Bot API limit.
const TELEGRAM_MIN_SECONDS: f64 = 10.0;
const TELEGRAM_MAX_SECONDS: f64 = 60.0;
pub const TELEGRAM_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// How many lines of ffmpeg's errors to keep for the message.
const ERROR_LINES: usize = 12;

/// The filters that turn a source frame into an output frame.
fn frame_filter(w: f64, h: f64, t: &Transform, outline: Outline, pixels: [u32; 2]) -> String {
    let [fw, fh] = t.size(w, h, outline);
    let [cx, cy] = t.center(w, h);
    let (crop_w, crop_h) = (fw.round().max(2.0), fh.round().max(2.0));
    let upright = t.angle_deg.abs() < 1e-6;
    // What to cut first: an upright frame as it is, a turned one with a square that holds it at any
    // angle, with its center in the middle.
    let (box_w, box_h) = if upright {
        (crop_w, crop_h)
    } else {
        let side = ((fw.hypot(fh).ceil() as i64 + 2) / 2 * 2) as f64;
        (side, side)
    };
    let x = pull_in((cx - box_w / 2.0).round(), w - box_w);
    let y = pull_in((cy - box_h / 2.0).round(), h - box_h);

    // Where that reaches outside the picture, black is added. It is seen only with bars: otherwise
    // what is outside the picture is outside the frame too, or outside the circle.
    let even_up = |v: f64| (v / 2.0).ceil() * 2.0;
    let (left, top) = (even_up((-x).max(0.0)), even_up((-y).max(0.0)));
    let (pad_w, pad_h) = (even_up((x + left + box_w).max(w) + left), even_up((y + top + box_h).max(h) + top));
    let mut steps = Vec::new();
    if left + top > 0.0 || x + box_w > w || y + box_h > h {
        steps.push(format!("pad={pad_w}:{pad_h}:{left}:{top}:black"));
    }
    steps.push(format!("crop={box_w}:{box_h}:{}:{}:exact=1", x + left, y + top));
    if !upright {
        let angle = t.angle_deg.to_radians();
        steps.push(format!("rotate={angle:.8}:ow={box_w}:oh={box_h}:c=black"));
        // The square's position was rounded: the frame's center is off its middle by a fraction of a
        // pixel. The turn takes that offset along.
        let (dx, dy) = (cx - (x + box_w / 2.0), cy - (y + box_h / 2.0));
        let (sin, cos) = angle.sin_cos();
        let (u, v) = (dx * cos - dy * sin, dx * sin + dy * cos);
        let crop_x = ((box_w - crop_w) / 2.0 + u).round().clamp(0.0, box_w - crop_w);
        let crop_y = ((box_h - crop_h) / 2.0 + v).round().clamp(0.0, box_h - crop_h);
        steps.push(format!("crop={crop_w}:{crop_h}:{crop_x}:{crop_y}:exact=1"));
    }
    if t.mirror {
        steps.push("hflip".to_owned());
    }
    steps.push(format!("scale={}:{}:flags=lanczos", pixels[0], pixels[1]));
    steps.push("setsar=1".to_owned());
    steps.join(",")
}

/// The filters that make a picture `w`×`h` what it is on a frame of `pixels`: the part of it that
/// the frame takes in, cut out and scaled, turned and mirrored, transparent around; and where its
/// top left corner goes on the frame. None when none of it is on the frame.
fn placed_filter(w: f64, h: f64, t: &Transform, outline: Outline, pixels: [u32; 2]) -> Option<(String, [i64; 2])> {
    let [pw, ph] = pixels.map(f64::from);
    let scale = pw / t.size(w, h, outline)[0];
    // The part of the picture around the turned frame, in whole pixels.
    let corners = t.corners(w, h, outline);
    let low = |axis: usize| corners.iter().map(|corner| corner[axis]).fold(f64::INFINITY, f64::min).floor().max(0.0);
    let high = |axis: usize, end: f64| corners.iter().map(|corner| corner[axis]).fold(f64::NEG_INFINITY, f64::max).ceil().min(end);
    let (x0, y0, x1, y1) = (low(0), low(1), high(0, w), high(1, h));
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
        return None;
    }
    let (sw, sh) = (((x1 - x0) * scale).round().max(1.0), ((y1 - y0) * scale).round().max(1.0));
    let mut steps = vec![format!("format=rgba,crop={}:{}:{x0}:{y0}:exact=1,scale={sw}:{sh}:flags=lanczos,setsar=1", x1 - x0, y1 - y0)];
    let (mut ow, mut oh) = (sw, sh);
    if t.angle_deg.abs() > 1e-6 {
        let angle = t.angle_deg.to_radians();
        let (sin, cos) = (angle.sin().abs(), angle.cos().abs());
        let even_up = |v: f64| (v.ceil() / 2.0).ceil() * 2.0;
        (ow, oh) = (even_up(sw * cos + sh * sin), even_up(sw * sin + sh * cos));
        steps.push(format!("rotate={angle:.8}:ow={ow}:oh={oh}:c=none"));
    }
    if t.mirror {
        steps.push("hflip".to_owned());
    }
    // Where the middle of that part is on the frame; turning keeps it in the middle.
    let [u, v] = t.source_to_output(w, h, [(x0 + x1) / 2.0, (y0 + y1) / 2.0]);
    let at = [(pw / 2.0 + u * scale - ow / 2.0).round() as i64, (ph / 2.0 + v * scale - oh / 2.0).round() as i64];
    Some((steps.join(","), at))
}

/// Rounding can put a box that fits the picture a pixel outside of it.
fn pull_in(position: f64, max: f64) -> f64 {
    if (-1.0..0.0).contains(&position) {
        0.0
    } else if position > max && position < max + 1.0 {
        max
    } else {
        position
    }
}

/// A run of the result's frames with one clip at the bottom of what is seen, over black, or black.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Piece {
    first: u64,
    count: u64,
    clip: Option<ClipId>,
}

/// A run of the result's frames in which a clip is seen over what is under it: laid over the pieces,
/// in the order of the tracks. It plays its source at one rate or holds a frame, all of it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layer {
    first: u64,
    count: u64,
    clip: ClipId,
}

/// The result's picture, frame by frame: what is at the bottom of each run of frames, and what is
/// laid over it. A clip seen over others across cuts under it is one layer: one input, however
/// many pieces it lies on. Where one clip gives way to another, a fade begins or ends, or a clip
/// starts or stops holding a frame or playing in slow motion, falls on the frame nearest to where it
/// is on the timeline.
fn pieces(sequence: &Sequence, fps: f64, sizes: Sizes) -> (Vec<Piece>, Vec<Layer>) {
    let export = sequence.export;
    let frame_at = |t: f64| ((t - export.start) * fps).round().max(0.0) as u64;
    let mut cuts = vec![export.start, export.end];
    let seen = sequence.clips().filter(|(id, _)| sequence.tracks[id.track].kind == TrackKind::Video && !sequence.tracks[id.track].hidden);
    for (_, clip) in seen {
        let (fade_in, fade_out) = clip.fade_lengths();
        let runs = clip.runs().into_iter().flat_map(|run| [run.span.start, run.span.end]);
        let edges = [clip.start, clip.end(), clip.start + fade_in, clip.end() - fade_out];
        cuts.extend(edges.into_iter().chain(runs).filter(|&t| export.contains(t)));
    }
    cuts.sort_by(f64::total_cmp);
    // Which run of a clip a moment is in.
    let run_of = |id: ClipId, t: f64| sequence.clip(id).runs().iter().position(|run| run.span.contains(t));
    let mut pieces: Vec<Piece> = Vec::new();
    let mut last_run = None;
    let mut layers: Vec<(Layer, Option<usize>)> = Vec::new();
    // The layers that reach the frame where the last run of frames ended.
    let mut open: Vec<usize> = Vec::new();
    for pair in cuts.windows(2) {
        let (first, end) = (frame_at(pair[0]), frame_at(pair[1]));
        if end <= first {
            continue;
        }
        let middle = (pair[0] + pair[1]) / 2.0;
        let stack = sequence.stack_at(middle, sizes);
        let clip = stack.first().copied();
        let run = clip.map(|id| run_of(id, middle));
        match pieces.last_mut() {
            Some(last) if last.clip == clip && last_run == run => last.count = end - last.first,
            _ => pieces.push(Piece { first, count: end - first, clip }),
        }
        last_run = run;
        let mut going_on = Vec::new();
        for &id in stack.iter().skip(1) {
            let run = run_of(id, middle);
            match open.iter().copied().find(|&i| layers[i].0.clip == id && layers[i].1 == run) {
                Some(i) => {
                    layers[i].0.count = end - layers[i].0.first;
                    going_on.push(i);
                }
                None => {
                    layers.push((Layer { first, count: end - first, clip: id }, run));
                    going_on.push(layers.len() - 1);
                }
            }
        }
        open = going_on;
    }
    if pieces.is_empty() {
        pieces.push(Piece { first: 0, count: frame_at(export.end).max(1), clip: None });
    }
    let mut layers: Vec<Layer> = layers.into_iter().map(|(layer, _)| layer).collect();
    layers.sort_by_key(|layer| (layer.clip.track, layer.first));
    (pieces, layers)
}

/// The size of each source's picture, as its file says.
fn sizes_of(files: &SourceFiles) -> impl Fn(SourceId) -> Option<[f64; 2]> + '_ {
    |source| files.get(&source).map(|file| [file.info.width as f64, file.info.height as f64])
}

/// Every clip seen somewhere in the export range, once each.
fn seen_clips(sequence: &Sequence, files: &SourceFiles) -> Vec<ClipId> {
    let mut seen: Vec<ClipId> = sequence.layers(sequence.export, &sizes_of(files)).into_iter().flat_map(|(_, stack)| stack).collect();
    seen.sort_by_key(|id| (id.track, id.index));
    seen.dedup();
    seen
}

/// The clips whose sound is in the result, each with the parts of the timeline it sounds in and the
/// run of it each part is in: where it plays its source, not where it holds a frame.
fn heard<'a>(sequence: &'a Sequence, files: &SourceFiles) -> Vec<(&'a Clip, Span, Run)> {
    let export = sequence.export;
    sequence
        .clips()
        .filter(|(id, _)| sequence.sounds(*id))
        .filter(|(_, clip)| files.get(&clip.source).is_none_or(|file| file.info.has_audio))
        .flat_map(|(_, clip)| {
            clip.runs().into_iter().filter(|run| !run.is_held()).filter_map(move |run| {
                let (start, end) = (run.span.start.max(export.start), run.span.end.min(export.end));
                (end > start).then_some((clip, Span { start, end }, run))
            })
        })
        .collect()
}

/// The clips that reach into the export range at all, seen or heard.
fn reaching(sequence: &Sequence) -> impl Iterator<Item = (ClipId, &Clip)> {
    let export = sequence.export;
    sequence.clips().filter(move |(_, clip)| clip.span().start < export.end && clip.span().end > export.start)
}

/// The result's frame rate, as ffmpeg takes it and as a number. "As the source" is the highest rate
/// of the clips that are seen, so that none of them loses frames; a still has none to lose, so only
/// stills are 30 frames a second.
fn frame_rate(sequence: &Sequence, files: &SourceFiles) -> (String, f64) {
    match sequence.settings.frame_rate {
        FrameRate::Fixed(fps) => (fps.to_string(), fps),
        FrameRate::Source => {
            let moving = seen_clips(sequence, files).into_iter().map(|id| sequence.clip(id)).filter(|clip| clip.still.is_none());
            let seen = moving.filter_map(|clip| files.get(&clip.source));
            let fastest = seen.map(|file| file.info.fps).max_by(|a, b| a.as_f64().total_cmp(&b.as_f64()));
            fastest.map_or_else(|| ("30".to_owned(), 30.0), |fps| (format!("{}/{}", fps.num, fps.den), fps.as_f64()))
        }
    }
}

/// The result's frame rate.
pub fn output_fps(sequence: &Sequence, files: &SourceFiles) -> f64 {
    frame_rate(sequence, files).1
}

/// The file a clip plays, or why there is none.
fn file_of<'a>(files: &'a SourceFiles, clip: &Clip) -> Result<&'a SourceFile> {
    files.get(&clip.source).with_context(|| format!("The file of {} is not there or can't be read.", clip.name))
}

fn video_args(settings: &ExportSettings) -> Vec<String> {
    let codec = settings.video_codec;
    let mut args = vec!["-c:v".to_owned(), codec.encoder().to_owned()];
    let mut add = |flag: &str, value: String| {
        args.push(flag.to_owned());
        args.push(value);
    };
    match codec {
        VideoCodec::H264 | VideoCodec::H265 => add("-preset", settings.speed.x26x_preset().to_owned()),
        VideoCodec::Vp9 => {
            add("-deadline", "good".to_owned());
            add("-cpu-used", settings.speed.vp9_cpu_used().to_string());
            add("-row-mt", "1".to_owned());
        }
    }
    if codec == VideoCodec::H264 {
        add("-profile:v", settings.profile.arg().to_owned());
    }
    if codec == VideoCodec::H265 && settings.container.has_faststart() {
        // Apple's players need this tag to recognize the stream.
        add("-tag:v", "hvc1".to_owned());
    }
    match settings.rate {
        Rate::Quality(crf) => {
            add("-crf", crf.to_string());
            if codec == VideoCodec::Vp9 {
                // Otherwise VP9 treats the CRF as a limit on top of its default bitrate.
                add("-b:v", "0".to_owned());
            }
        }
        Rate::Bitrate(kbps) => add("-b:v", format!("{kbps}k")),
    }
    add("-pix_fmt", "yuv420p".to_owned());
    args
}

/// A complete ffmpeg command line, without the program itself.
pub struct RenderPlan {
    pub args: Vec<OsString>,
    /// Length of the result in seconds.
    pub total: f64,
    /// The file the filter graph is in, when it is too long for the command line. It goes with the
    /// plan.
    graph_file: Option<PathBuf>,
}

impl RenderPlan {
    fn new(args: Vec<OsString>, total: f64) -> Self {
        Self { args, total, graph_file: None }
    }

    /// Hands the filter graph to ffmpeg: on the command line, or in a file when it is too long for it.
    fn set_graph(&mut self, graph: &[String]) -> Result<()> {
        let graph = graph.join(";");
        if graph.len() <= GRAPH_INLINE_LIMIT {
            self.args.push("-filter_complex".into());
            self.args.push(graph.into());
            return Ok(());
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let file = std::env::temp_dir().join(format!("dicing_splicing_graph_{}_{n}.txt", std::process::id()));
        fs::write(&file, graph).with_context(|| format!("Can't write the filter graph to {}", file.display()))?;
        self.args.push("-/filter_complex".into());
        self.args.push(file.as_os_str().into());
        self.graph_file = Some(file);
        Ok(())
    }
}

impl Drop for RenderPlan {
    fn drop(&mut self) {
        if let Some(file) = &self.graph_file {
            let _ = fs::remove_file(file);
        }
    }
}

/// Adds an input that starts a little before `start`, and not after `latest`: seeking before the
/// input saves decoding everything up to it, and decoding covers the rest exactly. Timestamps then
/// count from where the seek landed. Returns the input's number and where it starts.
fn add_input(args: &mut Vec<OsString>, inputs: &mut usize, path: &Path, start: f64, end: f64, latest: f64) -> (usize, f64) {
    let seek = (start - SEEK_MARGIN).min(latest).max(0.0);
    if seek > 0.0 {
        args.push("-ss".into());
        args.push(format!("{seek:.6}").into());
    }
    // Nothing after the end is needed; a second more keeps the last frame and the last samples.
    args.push("-t".into());
    args.push(format!("{:.6}", end - seek + 1.0).into());
    args.push("-i".into());
    args.push(path.into());
    *inputs += 1;
    (*inputs - 1, seek)
}

/// Adds an input for the frame of `file` seen at its time `frame`: an image's one picture, or the
/// frame of a recording that started at or before `frame`, on its frame grid, and its last frame at
/// its end. Returns the input's number and when, in the input's own time, that frame starts at the
/// earliest.
fn add_frame_input(args: &mut Vec<OsString>, inputs: &mut usize, file: &SourceFile, frame: f64) -> (usize, f64) {
    if file.info.image {
        args.push("-i".into());
        args.push((&file.path).into());
        *inputs += 1;
        return (*inputs - 1, 0.0);
    }
    let fps = file.info.fps.as_f64();
    let last = ((file.info.duration - 0.5 / fps) * fps).floor().max(0.0) / fps;
    let shown = ((frame * fps + 1e-6).floor() / fps).clamp(0.0, last);
    let (input, seek) = add_input(args, inputs, &file.path, shown, shown + 1.0 / fps, f64::INFINITY);
    // Half a frame early: a frame a little off its place on the grid is still that frame.
    (input, (shown - seek - 0.5 / fps).max(0.0))
}

/// A fade of a clip as the filters see it: from source time `at`, for `length` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FadeAt {
    edge: Edge,
    at: f64,
    length: f64,
    curve: Curve,
}

impl FadeAt {
    /// How much of the clip shows, or sounds, at the time `t` of an input that starts at source time
    /// `seek`: the fade's curve as an ffmpeg expression, the same formula as `Curve::ease`.
    fn level(&self, seek: f64) -> String {
        // A fade that started before the input did: its time is added, "t+1" rather than "t--1".
        let x = format!("clip((t{:+.6})/{:.6},0,1)", seek - self.at, self.length);
        let ease = match self.curve {
            Curve::Linear => x,
            Curve::EaseIn => format!("pow({x},2)"),
            Curve::EaseOut => format!("1-pow(1-{x},2)"),
            Curve::EaseInOut => format!("(1-cos(PI*{x}))/2"),
            Curve::StrongEaseIn => format!("pow({x},3)"),
            Curve::StrongEaseOut => format!("1-pow(1-{x},3)"),
            Curve::StrongEaseInOut => format!("if(lt({x},0.5),4*pow({x},3),1-4*pow(1-{x},3))"),
        };
        match self.edge {
            Edge::In => ease,
            Edge::Out => format!("1-({ease})"),
        }
    }

    fn kind(&self) -> &'static str {
        match self.edge {
            Edge::In => "in",
            Edge::Out => "out",
        }
    }

    /// Whether it goes by its formula, for an input that starts at `zero`: a curve, or a fade that
    /// starts before the input does, as one through a frame held does in the part after the hold.
    /// ffmpeg's own fades start at the input's start or later.
    fn by_formula(&self, zero: f64) -> bool {
        self.curve != Curve::Linear || self.at < zero - 1e-6
    }

    /// The sound filter: ffmpeg's own for a straight fade, the formula sample by sample otherwise.
    fn sound_filter(&self, seek: f64) -> String {
        if self.by_formula(seek) {
            format!(",aeval=exprs='val(ch)*({})':c=same", self.level(seek))
        } else {
            format!(",afade=t={}:st={:.6}:d={:.6}", self.kind(), (self.at - seek).max(0.0), self.length)
        }
    }
}

/// The fades of `clip` that reach into `own`, a part of its own time in seconds from its start, for
/// `sound` or the picture: by the time of an input that is at `a + b * c` at the clip's own time `c`,
/// as the filters see them. And the latest a seek may land so that their timestamps are not before
/// the input's start.
fn fade_filters_of(clip: &Clip, own: (f64, f64), (a, b): (f64, f64), sound: bool) -> (Vec<FadeAt>, f64) {
    // In the input's seconds: a clip that plays twice as fast goes through twice as much of its
    // source while it fades.
    let (fade_in, fade_out) = clip.fade_lengths();
    let len = clip.span().len();
    let applies = |edge| clip.fade(edge).is_some_and(|fade| if sound { fade.sound } else { fade.picture });
    let mut fades = Vec::new();
    let mut latest = f64::INFINITY;
    if fade_in > 0.0 && applies(Edge::In) && own.0 < fade_in {
        fades.push(FadeAt { edge: Edge::In, at: a, length: fade_in * b, curve: clip.fade_curve(Edge::In) });
        latest = latest.min(a);
    }
    if fade_out > 0.0 && applies(Edge::Out) && own.1 > len - fade_out {
        let at = a + (len - fade_out) * b;
        fades.push(FadeAt { edge: Edge::Out, at, length: fade_out * b, curve: clip.fade_curve(Edge::Out) });
        latest = latest.min(at);
    }
    (fades, latest)
}

/// Settings for drawing the frames of one clip in the result.
struct Frames<'a> {
    files: &'a SourceFiles,
    sequence: &'a Sequence,
    /// The result's frame rate, as ffmpeg takes it and as a number.
    fps: &'a str,
    rate: f64,
    pixels: [u32; 2],
}

/// How `clip_frames` draws a clip.
struct Look {
    /// Its fades take it into transparency, for what is under it to show through; else into black.
    alpha: bool,
    /// Its fades are in the frames. Without, they are left to what is done with the frames after.
    fades: bool,
    /// Only its picture, as large as it is on the frame and transparent around: these filters make
    /// it so. Without, the whole frame.
    placed: Option<String>,
}

impl Look {
    /// The whole frame, faded into transparency with `alpha`, else into black.
    fn frame(alpha: bool) -> Self {
        Self { alpha, fades: true, placed: None }
    }
}

/// The filters of `fades` in a clip's picture, where its own time `zero` is 0: ffmpeg's `fade` for a
/// straight one, the mask of one by its formula. `transparent`: they take the picture into
/// transparency, else into black. `name` tells the mask's own labels apart.
fn fade_chain(fades: &[FadeAt], zero: f64, transparent: bool, name: &str) -> String {
    let mut fade = String::new();
    if transparent && !fades.is_empty() {
        fade += ",format=yuva420p";
    }
    let mut mask = String::new();
    for f in fades {
        if !f.by_formula(zero) {
            fade += &format!(",fade=t={}:st={:.6}:d={:.6}{}", f.kind(), (f.at - zero).max(0.0), f.length, if transparent { ":alpha=1" } else { "" });
        } else {
            // White, its contrast around mid gray taken from 1 to -1: the mask's value is the level
            // times 255. Brightness stays 0: ffmpeg rounds it to whole percents.
            mask += &format!(",eq=contrast='2*({})-1':eval=frame", f.level(zero));
        }
    }
    if !mask.is_empty() {
        fade += &format!(",split[{name}c][{name}m];[{name}m]alphaextract{mask}[{name}a];[{name}c][{name}a]alphamerge");
    }
    fade
}

/// The filters that make the frames of `clip` for the run of result frames `(first, count)`, ending
/// in `label`, as `look` says: the whole frame, or only its picture where it is on the frame. Its
/// fades take it into black, or into transparency, for what is under it to show through. The frames
/// are all of a part of the clip where it plays its source, or all of one where it holds a frame.
///
/// A straight fade is ffmpeg's `fade`. One by another curve, or one that started before the input
/// did, is a mask of how much shows, made frame by frame by its formula; with nothing under the
/// clip, it is laid over black.
fn clip_frames(frames: &Frames, args: &mut Vec<OsString>, inputs: &mut usize, id: ClipId, (first, count): (u64, u64), look: &Look, label: &str) -> Result<String> {
    let (sequence, rate, fps) = (frames.sequence, frames.rate, frames.fps);
    let clip = sequence.clip(id);
    let file = file_of(frames.files, clip)?;
    // The frames in the clip's own time, from its start, and the run of it they are in.
    let at_first = sequence.export.start + first as f64 / rate;
    let own = (at_first - clip.start, at_first - clip.start + count as f64 / rate);
    let runs = clip.runs();
    let middle = clip.start + (own.0 + own.1) / 2.0;
    let run = runs.iter().find(|run| run.span.contains(middle)).unwrap_or(&runs[runs.len() - 1]);
    let name = label.trim_matches(['[', ']']);
    let frame = match &look.placed {
        Some(placed) => placed.clone(),
        None => frame_filter(file.info.width as f64, file.info.height as f64, &clip.transform, sequence.output.outline(), frames.pixels),
    };
    // The colour comes first: a fade into black ends in black whatever the contrast.
    let colored = clip.graded().map_or_else(String::new, |grade| color::filters(&grade));
    // Where the frames come from, up to their fades; the fades, by the time the frames have there and
    // the input's start in it; and the rest of the way.
    let (source, fades, zero, rest) = match clip.still.or_else(|| run.is_held().then_some(run.source)) {
        // One frame, coloured and framed once, then repeated at the result's rate. The copies are
        // timed by the clip's own time, from where they start in it, for its fades to go by; then
        // from 0, as the result's frames. What is transparent in an image is black.
        Some(at) => {
            let (fades, _) = if look.fades { fade_filters_of(clip, own, (0.0, 1.0), false) } else { (Vec::new(), f64::INFINITY) };
            let (input, at) = add_frame_input(args, inputs, file, at);
            let flat = if file.info.image { format!(",{OVER_BLACK}") } else { String::new() };
            let source = format!(
                "[{input}:V:0]trim=start={at:.6},trim=end_frame=1{flat}{colored},{frame},loop=loop=-1:size=1,settb=AVTB,setpts=(N/({fps})+{:.6})/TB",
                own.0
            );
            (source, fades, 0.0, format!(",setpts=PTS-STARTPTS,fps={fps}:start_time=0,trim=end_frame={count}"))
        }
        // The source's moment at the first frame. Frames before it stay until `fps`, which needs them
        // to know what is on screen at that moment; a source that ends a frame early has its last
        // frame held. The clip goes through its source at its speed: in this run of it, the clip's
        // own time `c` is the moment `a + c * speed` of its source. Fades go by the input's own
        // timestamps, before they are moved to the result's.
        None => {
            let speed = run.rate;
            let start = run.source_time(at_first);
            let end = start + count as f64 / rate * speed;
            let a = run.source - (run.span.start - clip.start) * speed;
            let (fades, latest) = if look.fades { fade_filters_of(clip, own, (a, speed), false) } else { (Vec::new(), f64::INFINITY) };
            let (input, seek) = add_input(args, inputs, &file.path, start, end, latest);
            let (from, to) = (start - seek, end - seek);
            // From the source's moment `start` on, as fast as the clip plays.
            let timed = if speed == 1.0 { format!("PTS{:+.6}/TB", -from) } else { format!("(PTS{:+.6}/TB)/{speed:.6}", -from) };
            let rest = format!(",setpts={timed},fps={fps}:start_time=0,{frame},tpad=stop=-1:stop_mode=clone,trim=end_frame={count}");
            (format!("[{input}:V:0]trim=end={to:.6}{colored}"), fades, seek, rest)
        }
    };
    let masked = fades.iter().any(|fade| fade.by_formula(zero));
    let transparent = look.alpha || masked || look.placed.is_some();
    let format = if transparent { "yuva420p" } else { "yuv420p" };
    let over_black = masked && !look.alpha;
    let out = if over_black { format!("[{name}o]") } else { label.to_owned() };
    let fade = fade_chain(&fades, zero, transparent, name);
    let mut filters = format!("{source}{fade}{rest},format={format}{out}");
    if over_black {
        let [px, py] = frames.pixels;
        filters += &format!(
            ";color=c=black:s={px}x{py}:r={fps},trim=end_frame={count},format=yuv420p,setsar=1[{name}k];\
             [{name}k][{name}o]overlay=format=auto,format=yuv420p{label}"
        );
    }
    Ok(filters)
}

/// The frames of `id` for the run of result frames `(first, count)`, ending in `label`: its picture
/// laid on black where it is on the frame, then faded as a whole by its own time, into black, or
/// with `alpha` into transparency. For a picture that leaves part of the frame empty: at the bottom
/// of what is seen, with black around it, or with black bars over what is under it.
fn canvas_frames(frames: &Frames, args: &mut Vec<OsString>, inputs: &mut usize, id: ClipId, (first, count): (u64, u64), alpha: bool, label: &str) -> Result<String> {
    let (sequence, fps, rate) = (frames.sequence, frames.fps, frames.rate);
    let clip = sequence.clip(id);
    let file = file_of(frames.files, clip)?;
    let [px, py] = frames.pixels;
    let name = label.trim_matches(['[', ']']);
    let black = |tag: &str| format!("color=c=black:s={px}x{py}:r={fps},trim=end_frame={count},format=yuv420p,setsar=1[{name}{tag}]");
    let mut filters = vec![black("k")];
    let placed = placed_filter(file.info.width as f64, file.info.height as f64, &clip.transform, sequence.output.outline(), frames.pixels);
    let laid = match placed {
        Some((chain, [x, y])) => {
            let look = Look { alpha: true, fades: false, placed: Some(chain) };
            filters.push(clip_frames(frames, args, inputs, id, (first, count), &look, &format!("[{name}p]"))?);
            filters.push(format!("[{name}k][{name}p]overlay=x={x}:y={y}:format=auto,format=yuv420p[{name}v]"));
            format!("[{name}v]")
        }
        None => format!("[{name}k]"),
    };
    // Its fades by its own time: these frames start at its moment `own.0`.
    let at_first = sequence.export.start + first as f64 / rate;
    let own = (at_first - clip.start, at_first - clip.start + count as f64 / rate);
    let (fades, _) = fade_filters_of(clip, own, (-own.0, 1.0), false);
    if fades.is_empty() {
        filters.push(format!("{laid}null{label}"));
    } else if alpha {
        filters.push(format!("{laid}setpts=PTS-STARTPTS{},format=yuva420p{label}", fade_chain(&fades, 0.0, true, &format!("{name}f"))));
    } else {
        filters.push(format!("{laid}setpts=PTS-STARTPTS{},format=yuva420p[{name}g]", fade_chain(&fades, 0.0, true, &format!("{name}f"))));
        filters.push(black("b"));
        filters.push(format!("[{name}b][{name}g]overlay=format=auto,format=yuv420p{label}"));
    }
    Ok(filters.join(";"))
}

/// The frames of `layer`, timed to where they are in the result and ending in `[l{n}]`, and where
/// their top left corner goes on the frame. None when none of its picture is on the frame.
fn layer_frames(frames: &Frames, args: &mut Vec<OsString>, inputs: &mut usize, layer: &Layer, n: usize) -> Result<Option<(String, [i64; 2])>> {
    let clip = frames.sequence.clip(layer.clip);
    let file = file_of(frames.files, clip)?;
    let (w, h) = (file.info.width as f64, file.info.height as f64);
    let outline = frames.sequence.output.outline();
    let part = (layer.first, layer.count);
    let raw = format!("[l{n}r]");
    let (filters, at) = if clip.transform.covers(w, h, outline) {
        (clip_frames(frames, args, inputs, layer.clip, part, &Look::frame(true), &raw)?, [0, 0])
    } else if clip.transform.bars {
        (canvas_frames(frames, args, inputs, layer.clip, part, true, &raw)?, [0, 0])
    } else {
        let Some((chain, at)) = placed_filter(w, h, &clip.transform, outline, frames.pixels) else { return Ok(None) };
        (clip_frames(frames, args, inputs, layer.clip, part, &Look { alpha: true, fades: true, placed: Some(chain) }, &raw)?, at)
    };
    // The frames count from the result's start: before and after them, what is under passes on.
    Ok(Some((format!("{filters};{raw}setpts=PTS+{}[l{n}]", layer.first), at)))
}

/// Whether the picture of `id` covers the whole frame, as far as its file says.
fn covers_frame(frames: &Frames, id: ClipId) -> bool {
    let clip = frames.sequence.clip(id);
    let outline = frames.sequence.output.outline();
    frames.files.get(&clip.source).is_none_or(|file| clip.transform.covers(file.info.width as f64, file.info.height as f64, outline))
}

/// The picture of the edit as filters into `[v]`: what is at the bottom, its pieces one after
/// another, and every clip seen over it laid on in its time and place, in the order of the tracks.
fn picture_graph(frames: &Frames, args: &mut Vec<OsString>, inputs: &mut usize, graph: &mut Vec<String>) -> Result<()> {
    let [px, py] = frames.pixels;
    let (pieces, layers) = pieces(frames.sequence, frames.rate, &sizes_of(frames.files));
    // Layers whose picture is not on the frame at all are left out.
    let outline = frames.sequence.output.outline();
    let on_frame = |layer: &&Layer| {
        let clip = frames.sequence.clip(layer.clip);
        let Some(file) = frames.files.get(&clip.source) else { return true };
        let (w, h) = (file.info.width as f64, file.info.height as f64);
        clip.transform.bars || clip.transform.covers(w, h, outline) || placed_filter(w, h, &clip.transform, outline, frames.pixels).is_some()
    };
    let layers: Vec<&Layer> = layers.iter().filter(on_frame).collect();
    let bottom = if layers.is_empty() { "[v]".to_owned() } else { "[bottom]".to_owned() };
    let mut labels = String::new();
    for (n, piece) in pieces.iter().enumerate() {
        let count = piece.count;
        let label = if pieces.len() == 1 { bottom.clone() } else { format!("[p{n}]") };
        match piece.clip {
            None => graph.push(format!("color=c=black:s={px}x{py}:r={},trim=end_frame={count},format=yuv420p,setsar=1{label}", frames.fps)),
            Some(id) if covers_frame(frames, id) => graph.push(clip_frames(frames, args, inputs, id, (piece.first, count), &Look::frame(false), &label)?),
            Some(id) => graph.push(canvas_frames(frames, args, inputs, id, (piece.first, count), false, &label)?),
        }
        labels += &label;
    }
    if pieces.len() > 1 {
        graph.push(format!("{labels}concat=n={}:v=1:a=0{bottom}", pieces.len()));
    }
    let mut under = bottom;
    for (n, layer) in layers.iter().enumerate() {
        let Some((filters, [x, y])) = layer_frames(frames, args, inputs, layer, n)? else { continue };
        graph.push(filters);
        let out = if n + 1 == layers.len() { "[v]".to_owned() } else { format!("[over{n}]") };
        graph.push(format!("{under}[l{n}]overlay=x={x}:y={y}:eof_action=pass:format=auto,format=yuv420p{out}"));
        under = out;
    }
    Ok(())
}

/// What ffmpeg has to do to write the edit to `output`.
///
/// The picture is the pieces of the clips that are seen, one after another, on the result's frame
/// grid: a clip covers the whole frame, so the one on top is all there is to see. The sound is every
/// clip's sound in its place, mixed. Picture and sound go by one clock: a source's moment lands on
/// the same moment of the file in both. Counting a stream from its own first frame instead would
/// put the picture ahead of the sound, by as much as that frame is late.
pub fn plan(files: &SourceFiles, sequence: &Sequence, output: &Path) -> Result<RenderPlan> {
    let settings = &sequence.settings;
    let export = sequence.export;
    let total = export.len();
    let pixels = settings.pixels(sequence.output);
    let (fps, rate) = frame_rate(sequence, files);
    for (_, clip) in reaching(sequence) {
        file_of(files, clip)?;
    }

    let mut args: Vec<OsString> = ["-v", "error", "-nostdin", "-nostats", "-progress", "pipe:1", "-y"].map(OsString::from).to_vec();
    let mut inputs = 0;
    let mut graph = Vec::new();
    picture_graph(&Frames { files, sequence, fps: &fps, rate, pixels }, &mut args, &mut inputs, &mut graph)?;

    let has_sound = settings.audio_codec.encoder().is_some()
        && reaching(sequence).any(|(_, clip)| files.get(&clip.source).is_some_and(|file| file.info.has_audio));
    if has_sound {
        // Brought to one loudness before the fades of the whole, which then stay as they are set.
        let loudness = if settings.even_loudness { format!(",loudnorm=I={EVEN_LOUDNESS}:TP=-1.5:LRA=11,aresample=48000") } else { String::new() };
        let fades = format!("{loudness}{}", fade_filters(settings, total));
        let mut sounds = String::new();
        let heard = heard(sequence, files);
        for (n, (clip, span, run)) in heard.iter().enumerate() {
            let file = file_of(files, clip)?;
            // In this run of the clip, its own time `c` is the moment `a + c * speed` of its source.
            let speed = run.rate;
            let (start, end) = (run.source_time(span.start), run.source_time(span.end));
            let a = run.source - (run.span.start - clip.start) * speed;
            let (clip_fades, latest) = fade_filters_of(clip, (span.start - clip.start, span.end - clip.start), (a, speed), true);
            let (input, seek) = add_input(&mut args, &mut inputs, &file.path, start, end, latest);
            let (from, to) = (start - seek, end - seek);
            let lead = span.start - export.start;
            // The clip's own fades, by the input's timestamps, which `atrim` leaves as they are.
            let fade: String = clip_fades.iter().map(|fade| fade.sound_filter(seek)).collect();
            // At the clip's speed, its pitch kept. `atempo` gives what it hears ahead of time by as
            // much as it holds back, so it hears that much of the source before the part: where the
            // source has less, the rest is made up by placing the sound later.
            let (begin, timed) = if speed == 1.0 {
                (from, format!("asetpts=PTS-{from:.6}/TB+{lead:.6}/TB"))
            } else {
                let (filters, held) = tempo(speed);
                let before = held.min(from).max(0.0);
                let placed = lead + (held - before) / speed;
                (from - before, format!("asetpts=PTS-{:.6}/TB,aresample=48000,{filters},asetpts=PTS+{placed:.6}/TB", from - before))
            };
            // Moved to its place, then filled from 0: adelay only shifts the timestamps, which would
            // leave a hole in the stream instead of silence. The filling also evens out gaps in the
            // source's own sound, so that it stays with the picture.
            let label = if heard.len() == 1 { format!("{fades}[a]") } else { format!("[s{n}]") };
            graph.push(format!(
                "[{input}:a:0]atrim=start={begin:.6}:end={to:.6}{fade},{timed},\
                 aresample=48000:async=1:first_pts=0,aformat=channel_layouts=stereo,volume={:.4},\
                 apad=whole_dur={total:.6},atrim=end={total:.6}{label}",
                clip.gain()
            ));
            sounds += &format!("[s{n}]");
        }
        match heard.len() {
            0 => graph.push(format!("anullsrc=r=48000:cl=stereo:d={total:.6}[a]")),
            1 => {}
            // Added up as they are, like the editor plays them: not divided by how many there are.
            count => graph.push(format!("{sounds}amix=inputs={count}:duration=longest:normalize=0{fades}[a]")),
        }
    }

    let mut plan = RenderPlan::new(args, total);
    plan.set_graph(&graph)?;
    let args = &mut plan.args;
    args.extend(["-map", "[v]"].map(OsString::from));
    if has_sound {
        args.extend(["-map", "[a]"].map(OsString::from));
    }
    args.push("-t".into());
    args.push(format!("{total:.6}").into());
    args.extend(video_args(settings).into_iter().map(OsString::from));
    if let (true, Some(encoder)) = (has_sound, settings.audio_codec.encoder()) {
        args.extend(["-c:a", encoder].map(OsString::from));
        args.push("-b:a".into());
        args.push(format!("{}k", settings.audio_kbps).into());
        args.extend(["-ac", "2", "-ar", "48000"].map(OsString::from));
    }
    if settings.container.has_faststart() {
        args.extend(["-movflags", "+faststart"].map(OsString::from));
    }
    args.push(output.into());
    Ok(plan)
}

/// How much of its input ffmpeg's `atempo` takes in before it gives anything: half of its window,
/// which is 2048 samples at 48 kHz. What it gives is that much ahead of what it was given.
const TEMPO_HOLD: f64 = 1024.0 / 48000.0;

/// ffmpeg's `atempo` for `speed`, on sound at 48 kHz, which keeps the pitch; and how many seconds
/// of its input it holds back, all of the chain together. One `atempo` takes 0.5 to 2 in every
/// ffmpeg there is, so a speed past that is made of several, and each holds back half its window of
/// what it is given: of the input, that is as much times the speed of the ones before it.
pub fn tempo(speed: f64) -> (String, f64) {
    let mut steps = Vec::new();
    let mut left = speed;
    while left > 2.0 + 1e-9 {
        steps.push(2.0);
        left /= 2.0;
    }
    while left < 0.5 - 1e-9 {
        steps.push(0.5);
        left /= 0.5;
    }
    if steps.is_empty() || (left - 1.0).abs() > 1e-9 {
        steps.push(left);
    }
    let (mut held, mut rate) = (0.0, 1.0);
    for step in &steps {
        held += TEMPO_HOLD * rate;
        rate *= step;
    }
    (steps.iter().map(|step| format!("atempo={step:.6}")).collect::<Vec<_>>().join(","), held)
}

fn fade_filters(settings: &ExportSettings, total: f64) -> String {
    let (fade_in, fade_out) = settings.fades;
    let (fade_in, fade_out) = (fade_in.min(total), fade_out.min(total));
    let mut filters = String::new();
    if fade_in > 0.0 {
        filters += &format!(",afade=t=in:st=0:d={fade_in:.6}");
    }
    if fade_out > 0.0 {
        filters += &format!(",afade=t=out:st={:.6}:d={fade_out:.6}", total - fade_out);
    }
    filters
}

/// The edit as the frame `preset` would see it when shown for comparison: the same center and angle,
/// inside the main frame. A picture that stays against a point of the frame is in the same place
/// against that point of this one, as large against its shorter side.
pub fn variant(sequence: &Sequence, files: &SourceFiles, preset: &Preset) -> Sequence {
    let main = sequence.output.outline();
    let outline = sequence.output.outline_of(preset);
    let mut out = sequence.clone();
    out.output.preset = preset.id;
    for clip in out.tracks.iter_mut().flat_map(|track| &mut track.clips) {
        if let Some(file) = files.get(&clip.source) {
            let (w, h) = (file.info.width as f64, file.info.height as f64);
            if !clip.transform.anchor.is_frame() {
                clip.transform = clip.transform.moved_to(w, h, main, outline);
                continue;
            }
            let [fw, fh] = clip.transform.size(w, h, main);
            clip.transform.zoom = outline.fit(w, h)[0] / outline.fit(fw, fh)[0];
        }
    }
    out
}

/// One file to write: the command, and where the result goes once it is complete.
pub struct RenderTask {
    plan: RenderPlan,
    part: PathBuf,
    output: PathBuf,
}

/// Renders the edit to `output`.
pub fn video_task(files: &SourceFiles, sequence: &Sequence, output: &Path) -> Result<RenderTask> {
    let part = part_path(output);
    Ok(RenderTask { plan: plan(files, sequence, &part)?, part, output: output.to_owned() })
}

/// Saves the output's picture at timeline `time` as an image, from the original sources: the frame
/// the edit has there, fades and what shows through them included.
pub fn frame_task(files: &SourceFiles, sequence: &Sequence, time: f64, output: &Path) -> Result<RenderTask> {
    if sequence.shown_at(time).is_none() {
        bail!("There is no picture at the playhead.");
    }
    let (fps, rate) = frame_rate(sequence, files);
    // An edit of one frame, from `time` on.
    let mut moment = sequence.clone();
    moment.export = Span { start: time, end: time + 1.0 / rate };
    let part = part_path(output);
    let mut args: Vec<OsString> = ["-v", "error", "-nostdin", "-nostats", "-progress", "pipe:1", "-y"].map(OsString::from).to_vec();
    let mut graph = Vec::new();
    let frames = Frames { files, sequence: &moment, fps: &fps, rate, pixels: sequence.settings.pixels(sequence.output) };
    picture_graph(&frames, &mut args, &mut 0, &mut graph)?;
    let mut plan = RenderPlan::new(args, 1.0);
    plan.set_graph(&graph)?;
    plan.args.extend(["-map", "[v]", "-frames:v", "1", "-update", "1"].map(OsString::from));
    plan.args.push((&part).into());
    Ok(RenderTask { plan, part, output: output.to_owned() })
}

/// What is worth knowing before the render.
pub fn warnings(sequence: &Sequence, files: &SourceFiles) -> Vec<String> {
    let mut warnings = Vec::new();
    let export = sequence.export;
    for (_, clip) in reaching(sequence) {
        if let Err(error) = file_of(files, clip) {
            let text = format!("{error} The export can't start.");
            if !warnings.contains(&text) {
                warnings.push(text);
            }
        }
    }
    let picture = sequence.picture(export);
    let black: f64 = picture.iter().filter(|(_, shown)| shown.is_none()).map(|(span, _)| span.len()).sum();
    if picture.iter().all(|(_, shown)| shown.is_none()) {
        warnings.push("No clip is seen in the export range: the result is all black.".to_owned());
    } else if black > 0.0 {
        warnings.push(format!("{black:.1} s of the export range show no clip: they are black."));
    }
    let outline = sequence.output.outline();
    if outline.mask == Mask::Circle && !(TELEGRAM_MIN_SECONDS..=TELEGRAM_MAX_SECONDS).contains(&export.len()) {
        warnings.push(format!(
            "A Telegram circle should last {TELEGRAM_MIN_SECONDS:.0} to {TELEGRAM_MAX_SECONDS:.0} s, the export range has {:.1} s.",
            export.len()
        ));
    }
    let seen = seen_clips(sequence, files);
    let [px, _] = sequence.settings.pixels(sequence.output);
    for id in seen {
        let clip = sequence.clip(id);
        let Some(file) = files.get(&clip.source) else { continue };
        let [fw, _] = clip.transform.size(file.info.width as f64, file.info.height as f64, outline);
        let enlarged = px as f64 / fw;
        let text = format!("{} is enlarged {enlarged:.1} times: the frame covers fewer of its pixels than the result has.", clip.name);
        if enlarged > 1.05 && !warnings.contains(&text) {
            warnings.push(text);
        }
    }
    warnings
}

/// How far a line of ffmpeg's `-progress` output says it has come, in seconds.
fn progress_seconds(line: &str) -> Option<f64> {
    // Despite its name, out_time_ms is in microseconds too.
    let value = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="))?;
    Some(value.trim().parse::<f64>().ok()? / 1e6)
}

/// Where ffmpeg writes until it has finished: the same folder, so that renaming is immediate. The
/// real extension stays last, because ffmpeg picks the format by it.
fn part_path(output: &Path) -> PathBuf {
    let stem = output.file_stem().unwrap_or_default().to_string_lossy();
    let extension = output.extension().unwrap_or_default().to_string_lossy();
    output.with_file_name(format!("{stem}.part.{extension}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderStatus {
    /// From 0 to 1.
    Running(f32),
    Done { path: PathBuf, bytes: u64 },
    Failed(String),
    Cancelled,
}

struct Shared {
    status: Mutex<RenderStatus>,
    cancelled: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Shared {
    fn set(&self, status: RenderStatus) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                tools::kill_tree(child);
            }
        }
    }
}

/// A render running in the background. Dropping it cancels the render.
pub struct RenderJob {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl RenderJob {
    /// The file appears at its place only when it is complete.
    pub fn start(task: RenderTask, notify: Notify) -> Self {
        let RenderTask { plan, part, output } = task;
        let shared = Arc::new(Shared { status: Mutex::new(RenderStatus::Running(0.0)), cancelled: AtomicBool::new(false), child: Mutex::new(None) });
        let thread = {
            let shared = shared.clone();
            thread::spawn(move || {
                let result = run(&plan, &shared, &notify);
                let cancelled = shared.cancelled.load(Ordering::Relaxed);
                let status = match result {
                    _ if cancelled => RenderStatus::Cancelled,
                    Ok(()) => match fs::rename(&part, &output).and_then(|()| fs::metadata(&output)) {
                        Ok(meta) => RenderStatus::Done { path: output, bytes: meta.len() },
                        Err(e) => RenderStatus::Failed(format!("Can't save {}: {e}", output.display())),
                    },
                    Err(e) => RenderStatus::Failed(format!("{e:#}")),
                };
                if !matches!(status, RenderStatus::Done { .. }) {
                    let _ = fs::remove_file(&part);
                }
                shared.set(status);
                notify();
            })
        };
        Self { shared, thread: Some(thread) }
    }

    pub fn status(&self) -> RenderStatus {
        self.shared.status.lock().map_or(RenderStatus::Cancelled, |status| status.clone())
    }

    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Relaxed);
        self.shared.kill();
    }
}

impl Drop for RenderJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(plan: &RenderPlan, shared: &Shared, notify: &Notify) -> Result<()> {
    let mut child = tools::command(tools::FFMPEG)
        .args(&plan.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Can't start ffmpeg")?;
    let stdout = child.stdout.take().context("ffmpeg has no stdout")?;
    let stderr = child.stderr.take().context("ffmpeg has no stderr")?;
    if let Ok(mut slot) = shared.child.lock() {
        *slot = Some(child);
    }
    if shared.cancelled.load(Ordering::Relaxed) {
        shared.kill();
    }

    let errors = thread::spawn(move || {
        let mut last = VecDeque::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if last.len() == ERROR_LINES {
                last.pop_front();
            }
            last.push_back(line);
        }
        last.into_iter().collect::<Vec<_>>().join("\n")
    });
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(seconds) = progress_seconds(&line) {
            shared.set(RenderStatus::Running(((seconds / plan.total) as f32).clamp(0.0, 1.0)));
            notify();
        }
    }
    let errors = errors.join().unwrap_or_default();
    let status = match shared.child.lock() {
        Ok(mut slot) => slot.take().map(|mut child| child.wait()),
        Err(_) => None,
    };
    match status {
        Some(Ok(status)) if status.success() => Ok(()),
        _ if errors.is_empty() => anyhow::bail!("ffmpeg stopped with an error"),
        _ => anyhow::bail!("{errors}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::{Clip, Span, FREEZE_LENGTH, SLOW_MOTION_FACTOR, SLOW_MOTION_SECONDS};
    use crate::core::export::{AudioCodec, Container, OutputSize, Speed};
    use crate::core::preset::{get, Output, PresetId, Ratio};
    use crate::core::edit::Track;
    use crate::media::geometry;
    use crate::media::geometry::{Anchor, Transform};
    use crate::media::probe::{self, MediaInfo, Rational};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const W: u32 = 640;
    const H: u32 = 360;

    fn info(duration: f64) -> MediaInfo {
        MediaInfo { duration, width: W, height: H, fps: Rational { num: 25, den: 1 }, has_audio: true, image: false }
    }

    fn sequence(duration: f64) -> Sequence {
        Sequence::new(Clip::new("source".into(), duration))
    }

    fn have_ffmpeg() -> bool {
        let found = tools::check(tools::FFMPEG).is_ok() && tools::check(tools::FFPROBE).is_ok();
        if !found {
            eprintln!("ffmpeg/ffprobe not found, skipping");
        }
        found
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_render_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ffmpeg(args: &[&str]) {
        let output = tools::command(tools::FFMPEG).args(["-v", "error", "-y"]).args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    /// A generated clip: every frame differs, with a tone.
    fn make_source(dir: &Path, seconds: u32) -> PathBuf {
        let path = dir.join("source.mp4");
        ffmpeg(&[
            "-f", "lavfi", "-i", &format!("testsrc2=size={W}x{H}:rate=25:duration={seconds}"),
            "-f", "lavfi", "-i", &format!("sine=frequency=440:duration={seconds}"),
            "-c:v", "libx264", "-crf", "10", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
            path.to_str().unwrap(),
        ]);
        path
    }

    /// Runs the plan to the end, the way a job does, and returns the file.
    /// The file as the only source, with id 0.
    fn files(source: &Path, info: &MediaInfo) -> SourceFiles {
        SourceFiles::from([(0, SourceFile { path: source.to_owned(), info: info.clone() })])
    }

    fn render(source: &Path, info: &MediaInfo, sequence: &Sequence, output: &Path) -> PathBuf {
        render_all(&files(source, info), sequence, output)
    }

    fn render_all(files: &SourceFiles, sequence: &Sequence, output: &Path) -> PathBuf {
        let plan = plan(files, sequence, output).unwrap();
        let result = tools::command(tools::FFMPEG).args(&plan.args).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        output.to_owned()
    }

    /// The frame `time` seconds into a file, decoded from its very start, as RGB.
    fn rgb_at(path: &Path, time: f64, width: u32, height: u32) -> Vec<u8> {
        let filter = format!("trim=start={time:.6},setpts=PTS-STARTPTS");
        let output = tools::command(tools::FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vf", &filter, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout.len(), (width * height * 3) as usize);
        output.stdout
    }

    fn mean_difference(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.len() as f64
    }

    fn mean_level(rgb: &[u8]) -> f64 {
        rgb.iter().map(|v| *v as f64).sum::<f64>() / rgb.len() as f64
    }

    /// Mean difference between the output frame and what the geometry says should be there.
    fn geometry_error(source: &[u8], out: &[u8], px: u32, py: u32, t: &Transform, outline: Outline) -> f64 {
        let (w, h) = (W as f64, H as f64);
        let [fw, fh] = t.size(w, h, outline);
        // How far around the picture's edge the scaling blurs it into the bars: not compared.
        let edge = 2.0 + 2.0 * fw / px as f64;
        let sample = |x: f64, y: f64, channel: usize| -> Option<f64> {
            let (x, y) = (x - 0.5, y - 0.5);
            if x < -edge || y < -edge || x > w + edge || y > h + edge {
                return Some(0.0);
            }
            if x < 0.0 || y < 0.0 || x >= w - 1.0 || y >= h - 1.0 {
                return None;
            }
            let (x0, y0) = (x.floor() as usize, y.floor() as usize);
            let (ax, ay) = (x - x0 as f64, y - y0 as f64);
            let at = |x: usize, y: usize| source[(y * W as usize + x) * 3 + channel] as f64;
            let top = at(x0, y0) * (1.0 - ax) + at(x0 + 1, y0) * ax;
            let bottom = at(x0, y0 + 1) * (1.0 - ax) + at(x0 + 1, y0 + 1) * ax;
            Some(top * (1.0 - ay) + bottom * ay)
        };
        let (mut sum, mut count) = (0.0, 0u64);
        for j in 0..py {
            for i in 0..px {
                let u = (i as f64 + 0.5) / px as f64 - 0.5;
                let v = (j as f64 + 0.5) / py as f64 - 0.5;
                // The edge of a circle is not visible in the result, stay inside of it.
                if outline.mask == Mask::Circle && u.hypot(v) > 0.48 {
                    continue;
                }
                let [x, y] = t.output_to_source(w, h, [u * fw, v * fh]);
                for channel in 0..3 {
                    if let Some(expected) = sample(x, y, channel) {
                        sum += (expected - out[(j as usize * px as usize + i as usize) * 3 + channel] as f64).abs();
                        count += 1;
                    }
                }
            }
        }
        assert!(count > 0);
        sum / count as f64
    }

    #[test]
    fn upright_frames_are_only_cropped() {
        let t = Transform { cx: 0.25, cy: 0.5, zoom: 2.0, ..Transform::default() };
        let outline = Outline { aspect: 1.0, mask: Mask::Rect };
        let filter = frame_filter(1920.0, 1080.0, &t, outline, [400, 400]);
        assert_eq!(filter, "crop=540:540:210:270:exact=1,scale=400:400:flags=lanczos,setsar=1");
    }

    #[test]
    fn turned_frames_are_padded_only_near_the_edges() {
        let outline = Outline { aspect: 1.0, mask: Mask::Circle };
        let middle = Transform { zoom: 4.0, angle_deg: 30.0, ..Transform::default() };
        let filter = frame_filter(1920.0, 1080.0, &middle, outline, [400, 400]);
        assert!(filter.contains("rotate=") && !filter.contains("pad="), "{filter}");

        let corner = Transform { cx: 0.2, cy: 0.5, zoom: 1.0, angle_deg: 30.0, mirror: true, bars: false, anchor: Anchor::Frame };
        let filter = frame_filter(1920.0, 1080.0, &corner, outline, [400, 400]);
        assert!(filter.starts_with("pad="), "{filter}");
        assert!(filter.contains("hflip"), "{filter}");
    }

    /// What is at the bottom of the result's frames, with sources of `W`×`H`.
    fn bottom(s: &Sequence, fps: f64) -> Vec<Piece> {
        pieces(s, fps, &|_| Some([W as f64, H as f64])).0
    }

    #[test]
    fn pieces_follow_the_clips_on_the_frame_grid() {
        let id = ClipId { track: 0, index: 0 };
        let mut s = sequence(10.0);
        assert_eq!(bottom(&s, 30.0), [Piece { first: 0, count: 300, clip: Some(id) }]);

        // The clip trimmed to source 2..4 in place, the range around it: black on both sides.
        s.trim_start(id, 2.0, 0.1);
        s.trim_end(id, 4.0, 0.1);
        s.export = Span { start: 1.0, end: 5.0 };
        let black = |first, count| Piece { first, count, clip: None };
        assert_eq!(bottom(&s, 25.0), [black(0, 25), Piece { first: 25, count: 50, clip: Some(id) }, black(75, 25)]);

        // Out of the range: all black, and the warning says so.
        s.export = Span { start: 6.0, end: 8.0 };
        assert_eq!(bottom(&s, 25.0), [black(0, 50)]);
        assert!(warnings(&s, &files(Path::new("a.mp4"), &info(10.0))).iter().any(|w| w.contains("all black")));
    }

    #[test]
    fn a_clip_on_top_cuts_into_the_one_under_it_on_whole_frames() {
        // Below: 0..10 s. Above: 4.01..6.02 s. The range from -1 s to 12 s, at 30 fps.
        let mut s = sequence(10.0);
        s.tracks.push(Track { id: 1, clips: vec![Clip { start: 4.01, source_out: 2.01, ..Clip::of(1, "b".into(), 5.0) }], ..Track::default() });
        s.export = Span { start: -1.0, end: 12.0 };
        let (below, above) = (Some(ClipId { track: 0, index: 0 }), Some(ClipId { track: 1, index: 0 }));
        let piece = |first, count, clip| Piece { first, count, clip };
        let expected = [piece(0, 30, None), piece(30, 120, below), piece(150, 61, above), piece(211, 119, below), piece(330, 60, None)];
        assert_eq!(bottom(&s, 30.0), expected);
        assert_eq!(bottom(&s, 30.0).iter().map(|p| p.count).sum::<u64>(), 390);

        // Hidden, it is not seen; muted, it is not heard.
        s.tracks[1].hidden = true;
        assert_eq!(bottom(&s, 30.0), [piece(0, 30, None), piece(30, 300, below), piece(330, 60, None)]);
        let files = SourceFiles::from([
            (0, SourceFile { path: "a.mp4".into(), info: info(10.0) }),
            (1, SourceFile { path: "b.mp4".into(), info: info(5.0) }),
        ]);
        assert_eq!(heard(&s, &files).len(), 2);
        s.tracks[0].muted = true;
        assert_eq!(heard(&s, &files).iter().map(|(clip, _, _)| clip.name.as_str()).collect::<Vec<_>>(), ["b"]);
        s.clip_mut(ClipId { track: 1, index: 0 }).muted = true;
        assert!(heard(&s, &files).is_empty());
    }

    #[test]
    fn a_fading_clip_is_laid_over_what_shows_through() {
        // Below: 0..10 s. Above: 4..8 s, fading in over 1 s and out over 2 s. At 10 fps.
        let mut s = sequence(10.0);
        let mut above = Clip { start: 4.0, ..Clip::of(1, "b".into(), 4.0) };
        above.add_fade(Edge::In);
        above.add_fade(Edge::Out);
        above.set_fade_length(Edge::Out, 2.0);
        s.tracks.push(Track { id: 1, clips: vec![above], ..Track::default() });
        let (below, top) = (Some(ClipId { track: 0, index: 0 }), ClipId { track: 1, index: 0 });
        let sizes = |_: SourceId| Some([W as f64, H as f64]);
        let piece = |first, count, clip| Piece { first, count, clip };
        let layer = |first, count| Layer { first, count, clip: top };
        // While it fades, the clip under it is at the bottom and it lies over it.
        let expected = (vec![piece(0, 50, below), piece(50, 10, Some(top)), piece(60, 40, below)], vec![layer(40, 10), layer(60, 20)]);
        assert_eq!(pieces(&s, 10.0, &sizes), expected);

        // Nothing under it: it fades into black, in one piece with the rest of it.
        s.tracks[0].hidden = true;
        assert_eq!(pieces(&s, 10.0, &sizes), (vec![piece(0, 40, None), piece(40, 40, Some(top)), piece(80, 20, None)], vec![]));
        // Only the sound fades: the picture is whole.
        s.tracks[0].hidden = false;
        for effect in &mut s.tracks[1].clips[0].effects {
            if let crate::core::edit::Effect::Fade(fade) = effect {
                fade.picture = false;
            }
        }
        assert_eq!(pieces(&s, 10.0, &sizes), (vec![piece(0, 40, below), piece(40, 40, Some(top)), piece(80, 20, below)], vec![]));
    }

    #[test]
    fn a_picture_smaller_than_the_frame_is_laid_over_the_pieces_in_one_layer() {
        // Below: 0..10 s. Above: 2..8 s, a third of the frame across. At 10 fps.
        let mut s = sequence(10.0);
        s.tracks.push(Track { id: 1, clips: vec![Clip { start: 2.0, ..Clip::of(1, "b".into(), 6.0) }], ..Track::default() });
        let (below, above) = (ClipId { track: 0, index: 0 }, ClipId { track: 1, index: 0 });
        s.clip_mut(above).transform.zoom = 0.33;
        let sizes = |_: SourceId| Some([W as f64, H as f64]);
        let piece = |first, count, clip| Piece { first, count, clip };
        let laid = vec![Layer { first: 20, count: 60, clip: above }];
        assert_eq!(pieces(&s, 10.0, &sizes), (vec![piece(0, 100, Some(below))], laid.clone()));
        // A cut under it does not cut it: one input for all of it.
        assert!(s.split(below, 5.0, 0.1).is_some());
        let after = ClipId { track: 0, index: 1 };
        assert_eq!(pieces(&s, 10.0, &sizes), (vec![piece(0, 50, Some(below)), piece(50, 50, Some(after))], laid));
        // With nothing under it, it is at the bottom, over black.
        s.tracks[0].hidden = true;
        assert_eq!(pieces(&s, 10.0, &sizes), (vec![piece(0, 20, None), piece(20, 60, Some(above)), piece(80, 20, None)], vec![]));
    }

    #[test]
    fn a_long_graph_goes_to_ffmpeg_in_a_file() {
        // 400 pieces: far more filters than a command line holds.
        let mut s = sequence(400.0);
        for k in 1..400 {
            assert!(s.split(ClipId { track: 0, index: k - 1 }, k as f64, 0.1).is_some());
        }
        let file = {
            let plan = plan(&files(Path::new("a.mp4"), &info(400.0)), &s, Path::new("out.mp4")).unwrap();
            let at = plan.args.iter().position(|arg| arg.as_os_str() == "-/filter_complex").expect("the graph in a file");
            let file = PathBuf::from(&plan.args[at + 1]);
            let graph = fs::read_to_string(&file).unwrap();
            assert!(graph.contains("concat=n=400:v=1:a=0[v]"));
            assert!(plan.args.iter().all(|arg| arg.len() < 2000));
            file
        };
        assert!(!file.exists(), "the file goes with the plan");
    }

    #[test]
    fn fades_go_into_the_filters_by_the_inputs_own_time() {
        // A clip of source 2..12 s at 0 s, fading in over 1 s and out over 2 s, a 10 s range at 25 fps.
        let mut s = Sequence::new(Clip { source_in: 2.0, source_out: 12.0, ..Clip::new("a".into(), 30.0) });
        s.clip_mut(ClipId { track: 0, index: 0 }).add_fade(Edge::In);
        s.clip_mut(ClipId { track: 0, index: 0 }).add_fade(Edge::Out);
        s.clip_mut(ClipId { track: 0, index: 0 }).set_fade_length(Edge::Out, 2.0);
        let files = files(Path::new("a.mp4"), &info(30.0));
        let plan = plan(&files, &s, Path::new("out.mp4")).unwrap();
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        // The input starts at 0 s, no later than the fade in: the fades are at 2 s and at 10 s of it.
        assert!(graph.contains("fade=t=in:st=2.000000:d=1.000000,"), "{graph}");
        assert!(graph.contains("fade=t=out:st=10.000000:d=2.000000,"), "{graph}");
        assert!(graph.contains("afade=t=in:st=2.000000:d=1.000000"), "{graph}");
        assert!(!graph.contains("alpha=1"), "nothing under it: into black. {graph}");
    }

    #[test]
    fn a_faster_clip_goes_through_more_of_its_source_and_keeps_its_pitch() {
        // Each atempo holds back half its window, of what the ones before it give.
        let half = 1024.0 / 48000.0;
        for (speed, filters, held) in [
            (2.0, "atempo=2.000000".to_owned(), half),
            (3.0, "atempo=2.000000,atempo=1.500000".to_owned(), 3.0 * half),
            (16.0, ["atempo=2.000000"; 4].join(","), 15.0 * half),
            (0.25, "atempo=0.500000,atempo=0.500000".to_owned(), 1.5 * half),
            (0.3, "atempo=0.500000,atempo=0.600000".to_owned(), 1.5 * half),
            (0.75, "atempo=0.750000".to_owned(), half),
        ] {
            let (made, holds) = tempo(speed);
            assert_eq!(made, filters, "×{speed}");
            assert!((holds - held).abs() < 1e-9, "×{speed}: holds {holds}, not {held}");
        }
        // Source 2..12 s twice as fast, from 0 s: 5 s, fading in over 1 s, which is 2 s of the source.
        let mut s = Sequence::new(Clip { source_in: 2.0, source_out: 12.0, ..Clip::new("a".into(), 30.0) });
        let id = ClipId { track: 0, index: 0 };
        assert!(s.set_speed(id, 2.0));
        s.clip_mut(id).add_fade(Edge::In);
        assert_eq!(s.export, Span { start: 0.0, end: 5.0 });
        let files = files(Path::new("a.mp4"), &info(30.0));
        let plan = plan(&files, &s, Path::new("out.mp4")).unwrap();
        assert_eq!(plan.total, 5.0);
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        assert!(graph.contains("trim=end=12.000000,fade=t=in:st=2.000000:d=2.000000,setpts=(PTS-2.000000/TB)/2.000000,"), "{graph}");
        // The sound starts half of atempo's window early, for what atempo holds back.
        assert!(graph.contains("atrim=start=1.978667:end=12.000000,afade=t=in:st=2.000000:d=2.000000,"), "{graph}");
        assert!(graph.contains("asetpts=PTS-1.978667/TB,aresample=48000,atempo=2.000000,asetpts=PTS+0.000000/TB,"), "{graph}");
    }

    /// The tone of a file's sound from `from` to `to` seconds: how many times a second it goes up
    /// through zero.
    fn tone(path: &Path, from: f64, to: f64) -> f64 {
        let output = tools::command(tools::FFMPEG).args(["-v", "error", "-i"]).arg(path).args(["-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "s16le", "pipe:1"]).output().unwrap();
        let samples: Vec<i16> = output.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let part = &samples[(from * 48000.0) as usize..(to * 48000.0) as usize];
        let rises = part.windows(2).filter(|pair| pair[0] < 0 && pair[1] >= 0).count();
        rises as f64 / (to - from)
    }

    #[test]
    fn a_faster_or_slower_clip_shows_its_source_at_that_speed_and_sounds_at_its_pitch() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("speed");
        let source = make_source(&dir, 12);
        let id = ClipId { track: 0, index: 0 };
        for (factor, from, to, at, moment) in [(2.0, 2.0, 10.0, 1.0, 4.0), (0.5, 2.0, 4.0, 2.0, 3.0)] {
            // The part of the source from..to at that speed: 4 s either way.
            let mut s = Sequence::new(Clip { source_in: from, source_out: to, ..Clip::new("source".into(), 12.0) });
            assert!(s.set_speed(id, factor));
            s.settings.fades = (0.0, 0.0);
            s.settings.size = OutputSize::LongSide(W);
            s.settings.rate = Rate::Quality(10);
            s.settings.frame_rate = FrameRate::Source;
            s.settings.speed = Speed::Veryfast;
            s.output = Output { preset: PresetId::Landscape, ..Output::default() };
            let path = render(&source, &info(12.0), &s, &dir.join(format!("x{factor}.mp4")));
            let result = probe::probe(&path).unwrap();
            assert!((result.duration - 4.0).abs() < 0.1, "×{factor}: duration {}", result.duration);
            // `at` seconds into the file is `moment` of the source, not what plain speed would show.
            let out = rgb_at(&path, at, W, H);
            let right = mean_difference(&out, &rgb_at(&source, moment, W, H));
            let plain = mean_difference(&out, &rgb_at(&source, from + at, W, H));
            assert!(right < 8.0, "×{factor}: differs by {right:.1} from the right moment");
            assert!(right * 1.5 < plain, "×{factor}: right {right:.1}, at plain speed {plain:.1}");
            let pitch = tone(&path, 0.5, 3.5);
            assert!((pitch - 440.0).abs() < 440.0 * 0.03, "×{factor}: the 440 Hz tone is {pitch:.0} Hz");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn picture_and_sound_of_a_faster_clip_stay_together() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("speed_sync");
        let id = ClipId { track: 0, index: 0 };
        let fps = 30.0;
        // A white frame and a beep together every two seconds of the source.
        let source = make_marked_source(&dir, "marked.mp4", "30", 60, 2.0, 0.0);
        let i = probe::probe(&source).unwrap();
        for factor in [2.0, 0.5] {
            let mut s = Sequence::new(Clip { source_in: 1.0, source_out: 17.0, ..Clip::new("source".into(), i.duration) });
            assert!(s.set_speed(id, factor));
            s.settings.fades = (0.0, 0.0);
            s.settings.size = OutputSize::LongSide(W);
            s.settings.rate = Rate::Quality(12);
            s.settings.speed = Speed::Ultrafast;
            s.settings.frame_rate = FrameRate::Fixed(fps);
            s.output = Output { preset: PresetId::Landscape, ..Output::default() };
            let errors = picture_minus_sound(&render(&source, &i, &s, &dir.join(format!("x{factor}.mp4"))), fps);
            assert!(errors.len() >= 3, "×{factor}: {errors:?}");
            for error in errors {
                assert!(error.abs() <= 0.5 / fps + 0.01, "×{factor}: picture minus sound {:.1} ms", error * 1000.0);
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curved_fades_are_a_mask_by_their_formula() {
        // The same clip, fading in with an ease in and out with a strong ease out.
        let mut s = Sequence::new(Clip { source_in: 2.0, source_out: 12.0, ..Clip::new("a".into(), 30.0) });
        let id = ClipId { track: 0, index: 0 };
        s.clip_mut(id).add_fade(Edge::In);
        s.clip_mut(id).add_fade(Edge::Out);
        s.clip_mut(id).set_fade_length(Edge::Out, 2.0);
        s.clip_mut(id).set_fade_curve(Edge::In, Curve::EaseIn);
        s.clip_mut(id).set_fade_curve(Edge::Out, Curve::StrongEaseOut);
        let files = files(Path::new("a.mp4"), &info(30.0));
        let plan = plan(&files, &s, Path::new("out.mp4")).unwrap();
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        let rise = "pow(clip((t-2.000000)/1.000000,0,1),2)";
        let fall = "1-(1-pow(1-clip((t-10.000000)/2.000000,0,1),3))";
        assert!(graph.contains(&format!("eq=contrast='2*({rise})-1':eval=frame")), "{graph}");
        assert!(graph.contains(&format!("eq=contrast='2*({fall})-1':eval=frame")), "{graph}");
        assert!(graph.contains("alphamerge") && graph.contains("color=c=black"), "over black: {graph}");
        assert!(!graph.contains(",fade=t="), "no straight fades of the picture: {graph}");
        assert!(graph.contains(&format!("aeval=exprs='val(ch)*({rise})':c=same")), "{graph}");
        assert!(graph.contains(&format!("aeval=exprs='val(ch)*({fall})':c=same")), "{graph}");
    }

    /// Every curve's formula, run by ffmpeg the way a mask is made, gives what the preview shows.
    #[test]
    fn ffmpeg_follows_every_curve_like_the_preview() {
        if !have_ffmpeg() {
            return;
        }
        for curve in Curve::ALL {
            for edge in [Edge::In, Edge::Out] {
                let fade = FadeAt { edge, at: 0.5, length: 2.0, curve };
                let level = fade.level(0.0);
                let filter = format!("eq=contrast='2*({level})-1':eval=frame");
                let output = tools::command(tools::FFMPEG)
                    .args(["-v", "error", "-f", "lavfi", "-i", "color=c=white:s=8x8:r=10:d=3,format=gray", "-vf", &filter])
                    .args(["-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{curve:?}: {}", String::from_utf8_lossy(&output.stderr));
                let frames: Vec<u8> = output.stdout.chunks_exact(64).map(|frame| frame[0]).collect();
                assert_eq!(frames.len(), 30);
                for (n, &got) in frames.iter().enumerate() {
                    let want = 255.0 * curve.level(edge, (n as f64 / 10.0 - 0.5) / 2.0);
                    assert!((got as f64 - want).abs() <= 2.0, "{curve:?} {edge:?} at frame {n}: {got}, should be {want:.1}");
                }
            }
        }
    }

    #[test]
    fn a_missing_file_stops_the_export_only_if_its_clip_reaches_into_the_range() {
        let mut s = sequence(10.0);
        s.tracks.push(Track { id: 1, clips: vec![Clip { start: 20.0, ..Clip::of(1, "far".into(), 5.0) }], ..Track::default() });
        let only_first = files(Path::new("a.mp4"), &info(10.0));
        assert!(plan(&only_first, &s, Path::new("out.mp4")).is_ok());
        assert!(warnings(&s, &only_first).iter().all(|w| !w.contains("far")));

        s.export.end = 22.0;
        let error = plan(&only_first, &s, Path::new("out.mp4")).err().unwrap().to_string();
        assert!(error.contains("far"), "{error}");
        assert!(warnings(&s, &only_first).iter().any(|w| w.contains("far") && w.contains("can't start")));
    }

    #[test]
    fn the_rate_as_the_source_is_the_highest_of_what_is_seen() {
        let mut s = sequence(10.0);
        s.settings.frame_rate = FrameRate::Source;
        s.tracks.push(Track { id: 1, clips: vec![Clip { start: 2.0, ..Clip::of(1, "b".into(), 1.0) }], ..Track::default() });
        let fast = MediaInfo { fps: Rational { num: 60000, den: 1001 }, ..info(1.0) };
        let mut files = files(Path::new("a.mp4"), &info(10.0));
        files.insert(1, SourceFile { path: "b.mp4".into(), info: fast });
        assert_eq!(frame_rate(&s, &files).0, "60000/1001");
        // Hidden, it does not count.
        s.tracks[1].hidden = true;
        assert_eq!(frame_rate(&s, &files), ("25/1".to_owned(), 25.0));
        s.settings.frame_rate = FrameRate::Fixed(30.0);
        assert_eq!(output_fps(&s, &files), 30.0);
    }

    /// A PNG, 3 s from 2 s, after a video: read once, without a seek, and held.
    fn logo_after_a_video() -> (Sequence, SourceFiles, ClipId) {
        let mut s = Sequence::new(Clip { source_out: 2.0, ..Clip::new("a".into(), 30.0) });
        let logo = s.place_clip(Some(0), Clip { start: 2.0, source_out: 3.0, ..Clip::image(1, "logo".into()) }).unwrap();
        s.clip_mut(logo).add_fade(Edge::In);
        s.settings.frame_rate = FrameRate::Source;
        let mut files = files(Path::new("a.mp4"), &info(30.0));
        let png = MediaInfo { duration: 0.0, has_audio: false, image: true, ..info(0.0) };
        files.insert(1, SourceFile { path: "logo.png".into(), info: png });
        (s, files, logo)
    }

    #[test]
    fn an_image_is_read_once_and_held_and_fades_by_its_own_time() {
        let (mut s, files, logo) = logo_after_a_video();
        let graph = |s: &Sequence| plan(&files, s, Path::new("out.mp4")).unwrap().args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        let all = graph(&s);
        assert!(all.contains("-t 3.000000 -i a.mp4 -i logo.png "), "{all}");
        // Made black where it is transparent and framed once, then held: 75 frames, its fade by
        // its own time.
        assert!(all.contains("[1:V:0]trim=start=0.000000,trim=end_frame=1,format=rgba,premultiply=inplace=1,crop="), "{all}");
        assert!(
            all.contains(
                ",setsar=1,loop=loop=-1:size=1,settb=AVTB,setpts=(N/(25/1)+0.000000)/TB,fade=t=in:st=0.000000:d=1.000000,\
                 setpts=PTS-STARTPTS,fps=25/1:start_time=0,trim=end_frame=75,format=yuv420p[p1]"
            ),
            "{all}"
        );
        // From half way into its fade: the fade goes on from there. Only the image is seen then: as
        // the source is 30 frames a second, the video's rate otherwise.
        assert_eq!(frame_rate(&s, &files).0, "25/1");
        s.export.start = 2.5;
        assert!(graph(&s).contains("setpts=(N/(30)+0.500000)/TB,fade=t=in:st=0.000000:d=1.000000,"), "{}", graph(&s));
        assert_eq!(frame_rate(&s, &files), ("30".to_owned(), 30.0));
        s.export = s.clip(logo).span();
        // No sound of its own: only the video's is heard.
        assert_eq!(heard(&s, &files).len(), 0);
    }

    /// A copy of a JPEG that says, in its EXIF, to turn it by `orientation`: 6 is a quarter turn
    /// clockwise, as a phone held upright writes it.
    fn with_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
        let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        let mut app1 = vec![0xFF, 0xE1];
        app1.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        app1.append(&mut exif);
        [&jpeg[..2], &app1, &jpeg[2..]].concat()
    }

    /// The mean colour of rows `rows` of a frame of `width` pixels, as red, green and blue.
    fn rows_color(rgb: &[u8], width: u32, rows: std::ops::Range<u32>) -> [f64; 3] {
        let part = &rgb[(rows.start * width * 3) as usize..(rows.end * width * 3) as usize];
        let channel = |c: usize| part.iter().skip(c).step_by(3).map(|v| *v as f64).sum::<f64>() / (part.len() / 3) as f64;
        [channel(0), channel(1), channel(2)]
    }

    /// An image in the file: as long as its clip, its picture all along, its fade; black where it is
    /// transparent; a photo upright, as its EXIF says.
    #[test]
    fn an_image_is_in_the_file_as_long_as_its_clip() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("image");
        let png = dir.join("picture.png");
        ffmpeg(&["-f", "lavfi", "-i", &format!("testsrc=size={W}x{H}"), "-frames:v", "1", png.to_str().unwrap()]);
        let info = probe::probe(&png).unwrap();
        assert!(info.image && info.duration == 0.0, "{info:?}");
        let files = files(&png, &info);
        // 3 s, coming in over 1 s, alone: as the source, 30 frames a second.
        let mut s = Sequence::new(Clip { source_out: 3.0, ..Clip::image(0, "picture".into()) });
        let id = ClipId { track: 0, index: 0 };
        s.clip_mut(id).add_fade(Edge::In);
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Source;
        let path = render_all(&files, &s, &dir.join("image.mp4"));
        let result = probe::probe(&path).unwrap();
        assert!((result.duration - 3.0).abs() < 0.05, "duration {}", result.duration);
        assert_eq!((result.width, result.height, result.fps, result.has_audio), (320, 180, Rational { num: 30, den: 1 }, false));
        let scaled = dir.join("scaled.png");
        ffmpeg(&["-i", png.to_str().unwrap(), "-vf", "scale=320:180:flags=lanczos", scaled.to_str().unwrap()]);
        let want = rgb_at(&scaled, 0.0, 320, 180);
        for t in [1.0, 2.0, 2.9] {
            let error = mean_difference(&rgb_at(&path, t, 320, 180), &want);
            assert!(error < 6.0, "at {t} s the picture differs by {error:.1}");
        }
        let (half, full) = (mean_level(&rgb_at(&path, 0.5, 320, 180)), mean_level(&want));
        assert!((half / full - 0.5).abs() < 0.08, "half way into the fade: {half:.1} of {full:.1}");

        // Half transparent red on the left, nothing on the right: over black.
        let clear = dir.join("clear.png");
        ffmpeg(&["-f", "lavfi", "-i", "color=c=red@0.5:s=320x360,format=rgba,pad=640:360:0:0:color=white@0", "-frames:v", "1", clear.to_str().unwrap()]);
        let files = SourceFiles::from([(0, SourceFile { path: clear.clone(), info: probe::probe(&clear).unwrap() })]);
        let path = render_all(&files, &s, &dir.join("clear.mp4"));
        let rgb = rgb_at(&path, 2.0, 320, 180);
        let half_of = |range: std::ops::Range<u32>| {
            let pixels: Vec<[u8; 3]> = rgb.chunks(3).enumerate().filter(|(i, _)| range.contains(&((*i as u32) % 320))).map(|(_, p)| [p[0], p[1], p[2]]).collect();
            [0, 1, 2].map(|c| pixels.iter().map(|p| p[c] as f64).sum::<f64>() / pixels.len() as f64)
        };
        let (left, right) = (half_of(20..140), half_of(180..300));
        assert!((left[0] - 128.0).abs() < 20.0 && left[1] < 20.0 && left[2] < 20.0, "half of red over black: {left:?}");
        assert!(right.iter().all(|c| *c < 20.0), "nothing over black: {right:?}");

        // A photo stored lying on its side, red on the left: upright, red at the top.
        let lying = dir.join("lying.jpg");
        ffmpeg(&["-f", "lavfi", "-i", "color=red:s=64x64[a];color=blue:s=64x64[b];[a][b]hstack", "-frames:v", "1", lying.to_str().unwrap()]);
        let photo = dir.join("photo.jpg");
        fs::write(&photo, with_orientation(&fs::read(&lying).unwrap(), 6)).unwrap();
        let info = probe::probe(&photo).unwrap();
        assert_eq!((info.width, info.height), (64, 128));
        let files = SourceFiles::from([(0, SourceFile { path: photo.clone(), info })]);
        s.output = Output { preset: PresetId::Vertical, ..Output::default() };
        let path = render_all(&files, &s, &dir.join("photo.mp4"));
        let rgb = rgb_at(&path, 2.0, 180, 320);
        let (top, bottom) = (rows_color(&rgb, 180, 20..120), rows_color(&rgb, 180, 200..300));
        assert!(top[0] > 200.0 && top[2] < 60.0, "red at the top: {top:?}");
        assert!(bottom[2] > 200.0 && bottom[0] < 60.0, "blue at the bottom: {bottom:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A source of `frames` frames at 25 a second, up to 100, each of a colour of its own: its
    /// brightness steps up with each frame of a second, its blue with each second. Which of them is
    /// seen can be read off a picture. With a tone.
    fn make_counted_source(dir: &Path, frames: u32) -> PathBuf {
        let path = dir.join("counted.mp4");
        let seconds = frames as f64 / 25.0;
        ffmpeg(&[
            "-f", "lavfi", "-i", &format!("nullsrc=size=160x90:rate=25:duration={seconds},format=yuv420p,geq=lum='20+8*mod(N\\,25)':cb='64+32*floor(N/25)':cr=128"),
            "-f", "lavfi", "-i", &format!("sine=frequency=440:duration={seconds}"),
            "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
            path.to_str().unwrap(),
        ]);
        path
    }

    /// The mean brightness and blue of each frame of a file.
    fn frame_colors(path: &Path) -> Vec<(f64, f64)> {
        let output = tools::command(tools::FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-vf", "scale=16:16:flags=area,format=yuv444p", "-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let mean = |plane: &[u8]| plane.iter().map(|v| *v as f64).sum::<f64>() / plane.len() as f64;
        output.stdout.chunks_exact(768).map(|frame| (mean(&frame[..256]), mean(&frame[256..512]))).collect()
    }

    #[test]
    fn a_frame_held_is_a_piece_of_its_own() {
        // 0..10 s at 10 fps, holding its frame at 4 s for 2 s: played, held, played.
        let mut s = sequence(10.0);
        let id = ClipId { track: 0, index: 0 };
        assert!(s.freeze(id, 4.0, FREEZE_LENGTH));
        let piece = |first, count| Piece { first, count, clip: Some(id) };
        assert_eq!(bottom(&s, 10.0), [piece(0, 40), piece(40, 20), piece(60, 60)]);
    }

    #[test]
    fn a_part_in_slow_motion_is_a_piece_of_its_own() {
        // 0..10 s at 10 fps, a second of it from 4 s half as fast: played, slow, played.
        let mut s = sequence(10.0);
        let id = ClipId { track: 0, index: 0 };
        assert!(s.slow_motion(id, 4.0, SLOW_MOTION_SECONDS, SLOW_MOTION_FACTOR));
        let piece = |first, count| Piece { first, count, clip: Some(id) };
        assert_eq!(bottom(&s, 10.0), [piece(0, 40), piece(40, 20), piece(60, 50)]);
        // Its picture goes through its source at half the speed, its sound too, at its pitch.
        let files = files(Path::new("a.mp4"), &info(10.0));
        let plan = plan(&files, &s, Path::new("out.mp4")).unwrap();
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        assert!(graph.contains("trim=end=4.000000,setpts=(PTS-3.000000/TB)/0.500000,"), "{graph}");
        assert_eq!(graph.matches("atrim=start=").count(), 3, "{graph}");
        assert!(graph.contains("atempo=0.500000"), "{graph}");
    }

    #[test]
    fn a_fade_goes_on_through_a_frame_held_by_its_formula() {
        // Source 0..10 s at 0 s, fading in over 2 s, holding its frame at 0.5 s for 1 s: played
        // 0..0.5 s, held 0.5..1.5 s, played on from 0.5 s of its source.
        let mut s = Sequence::new(Clip { source_out: 10.0, ..Clip::new("a".into(), 30.0) });
        let id = ClipId { track: 0, index: 0 };
        s.clip_mut(id).add_fade(Edge::In);
        s.clip_mut(id).set_fade_length(Edge::In, 2.0);
        assert!(s.freeze(id, 0.5, 1.0));
        s.settings.frame_rate = FrameRate::Fixed(10.0);
        let files = files(Path::new("a.mp4"), &info(30.0));
        let plan = plan(&files, &s, Path::new("out.mp4")).unwrap();
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        // Played: ffmpeg's fade from the input's start. Held: the clip's own time from 0.5 s, the fade
        // from 0. After the hold, the source is at 0.5 s 1.5 s into the clip: the fade started a
        // second before the source did, and goes by its formula.
        assert!(graph.contains("trim=end=0.500000,fade=t=in:st=0.000000:d=2.000000,"), "{graph}");
        assert!(graph.contains("setpts=(N/(10)+0.500000)/TB,fade=t=in:st=0.000000:d=2.000000,"), "{graph}");
        assert!(graph.contains("eq=contrast='2*(clip((t+1.000000)/2.000000,0,1))-1':eval=frame"), "{graph}");
        assert!(graph.contains("alphamerge") && graph.contains("color=c=black"), "over black: {graph}");
        // Heard where it plays, not while it holds its frame: two parts, the second by the formula.
        assert_eq!(graph.matches("atrim=start=").count(), 2, "{graph}");
        assert!(graph.contains("afade=t=in:st=0.000000:d=2.000000"), "{graph}");
        assert!(graph.contains("aeval=exprs='val(ch)*(clip((t+1.000000)/2.000000,0,1))':c=same"), "{graph}");
        // Further into its source, the input is sought, and the fade after the hold starts after it.
        let mut later = Sequence::new(Clip { source_in: 20.0, source_out: 30.0, ..Clip::new("a".into(), 30.0) });
        later.clip_mut(id).add_fade(Edge::In);
        later.clip_mut(id).set_fade_length(Edge::In, 2.0);
        assert!(later.freeze(id, 0.5, 1.0));
        let plan = super::plan(&files, &later, Path::new("out.mp4")).unwrap();
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        assert!(graph.contains("-ss 17.500000 -t 13.500000 -i a.mp4") && graph.contains("fade=t=in:st=1.500000:d=2.000000,"), "{graph}");
        assert!(!graph.contains("eq=contrast"), "{graph}");
    }

    #[test]
    fn a_frame_held_is_seen_for_its_length_and_the_video_goes_on_from_it() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("freeze");
        let source = make_counted_source(&dir, 100);
        let i = MediaInfo { width: 160, height: 90, ..info(4.0) };
        let colors = frame_colors(&source);
        assert_eq!(colors.len(), 100);
        let off = |n: usize, (y, u): (f64, f64)| (colors[n].0 - y).powi(2) + (colors[n].1 - u).powi(2);
        let number = |color: (f64, f64)| (0..colors.len()).min_by(|&a, &b| off(a, color).total_cmp(&off(b, color))).unwrap();
        // Frame 37, and frame 87, far enough in for its input to be sought.
        for held in [37, 87] {
            let mut s = sequence(4.0);
            s.settings.fades = (0.0, 0.0);
            s.settings.size = OutputSize::LongSide(W);
            s.settings.rate = Rate::Quality(1);
            s.settings.frame_rate = FrameRate::Source;
            s.settings.speed = Speed::Veryfast;
            s.output = Output { preset: PresetId::Landscape, ..Output::default() };
            let t = held as f64 / 25.0;
            assert!(s.freeze(ClipId { track: 0, index: 0 }, t, FREEZE_LENGTH));
            assert_eq!(s.export, Span { start: 0.0, end: 6.0 });
            let path = render(&source, &i, &s, &dir.join(format!("freeze{held}.mp4")));
            let shown: Vec<usize> = frame_colors(&path).into_iter().map(number).collect();
            // Up to it, then it for 2 s, then on from it.
            let expected: Vec<usize> = (0..150).map(|k| if k < held { k } else if k < held + 50 { held } else { k - 50 }).collect();
            assert_eq!(shown, expected, "frame {held}");
            // Silent while it is held.
            let (silent, heard) = (sound_level(&path, t + 0.2, t + 1.8), sound_level(&path, t + 2.2, t + 2.5));
            assert!(silent < 50.0 && heard > 1000.0, "frame {held}: {silent:.0} while held, {heard:.0} after");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A second of a counted source half as fast: each of its frames is seen twice, its sound keeps
    /// its pitch, and the picture and the sound stay together through it and after it.
    #[test]
    fn a_part_in_slow_motion_shows_each_frame_twice_at_its_pitch_with_its_sound() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("slow_motion");
        let source = make_counted_source(&dir, 100);
        let i = MediaInfo { width: 160, height: 90, ..info(4.0) };
        let colors = frame_colors(&source);
        assert_eq!(colors.len(), 100);
        let off = |n: usize, (y, u): (f64, f64)| (colors[n].0 - y).powi(2) + (colors[n].1 - u).powi(2);
        let number = |color: (f64, f64)| (0..colors.len()).min_by(|&a, &b| off(a, color).total_cmp(&off(b, color))).unwrap();
        let settle = |s: &mut Sequence, quality: u8, frame_rate: FrameRate| {
            s.settings.fades = (0.0, 0.0);
            s.settings.size = OutputSize::LongSide(W);
            s.settings.rate = Rate::Quality(quality);
            s.settings.frame_rate = frame_rate;
            s.settings.speed = Speed::Veryfast;
            s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        };
        // Frames 25 to 49 half as fast: 2 s instead of 1.
        let mut s = sequence(4.0);
        settle(&mut s, 1, FrameRate::Source);
        assert!(s.slow_motion(ClipId { track: 0, index: 0 }, 1.0, SLOW_MOTION_SECONDS, SLOW_MOTION_FACTOR));
        assert_eq!(s.export, Span { start: 0.0, end: 5.0 });
        let path = render(&source, &i, &s, &dir.join("slow.mp4"));
        let shown: Vec<usize> = frame_colors(&path).into_iter().map(number).collect();
        let expected: Vec<usize> = (0..125).map(|k| if k < 25 { k } else if k < 75 { 25 + (k - 25) / 2 } else { k - 25 }).collect();
        assert_eq!(shown, expected);
        for (from, to) in [(0.1, 0.9), (1.2, 2.8), (3.2, 4.8)] {
            let pitch = tone(&path, from, to);
            assert!((pitch - 440.0).abs() < 440.0 * 0.03, "{from}..{to} s: the 440 Hz tone is {pitch:.0} Hz");
        }
        // A white frame and a beep together every two seconds of the source: 1..17 s of it, with 3.5
        // to 4.5 s, and the mark at 4 s, half as fast. The marks after it are a second later.
        let fps = 30.0;
        let marked = make_marked_source(&dir, "marked.mp4", "30", 60, 2.0, 0.0);
        let i = probe::probe(&marked).unwrap();
        let mut s = Sequence::new(Clip { source_in: 1.0, source_out: 17.0, ..Clip::new("source".into(), i.duration) });
        settle(&mut s, 12, FrameRate::Fixed(fps));
        assert!(s.slow_motion(ClipId { track: 0, index: 0 }, 2.5, SLOW_MOTION_SECONDS, SLOW_MOTION_FACTOR));
        let errors = picture_minus_sound(&render(&marked, &i, &s, &dir.join("slow_marked.mp4")), fps);
        assert!(errors.len() >= 6, "{errors:?}");
        for error in errors {
            assert!(error.abs() <= 0.5 / fps + 0.01, "picture minus sound {:.1} ms", error * 1000.0);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A fade in through a frame held: it goes on rising while the frame is held, and after it, in
    /// the picture and in the sound, as the preview has it.
    #[test]
    fn a_fade_through_a_frame_held_is_seen_and_heard_in_the_result() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("fade_through_hold");
        let gray = make_solid(&dir, "gray.mp4", "0x808080", 25, (320, 180), Some(300));
        let files: SourceFiles = [(0, SourceFile { path: gray.clone(), info: probe::probe(&gray).unwrap() })].into();
        // Gray 0..4 s of its source, fading in over 2 s, its frame at 0.5 s held for 1 s: 0..5 s.
        let mut s = Sequence::new(Clip { source_out: 4.0, ..Clip::of(0, "gray".into(), 10.0) });
        let id = ClipId { track: 0, index: 0 };
        s.clip_mut(id).add_fade(Edge::In);
        s.clip_mut(id).set_fade_length(Edge::In, 2.0);
        assert!(s.freeze(id, 0.5, 1.0));
        assert_eq!(s.export, Span { start: 0.0, end: 5.0 });
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Fixed(25.0);
        let path = render_all(&files, &s, &dir.join("through.mp4"));
        let level = |t: f64| mean_level(&rgb_at(&path, t, 320, 180));
        let clip = s.clip(id);
        for t in [0.24, 1.0, 1.76, 3.0] {
            let want = 128.0 * clip.opacity(t);
            assert!((level(t) - want).abs() < 12.0, "at {t} s: {} instead of {want}", level(t));
        }
        // Silent while held; after it, at the level the fade has got to.
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        let full = rms(3.0, 3.5);
        assert!(rms(0.7, 1.3) < full * 0.05, "held: {} of {full}", rms(0.7, 1.3));
        let after = rms(1.7, 1.8) / full;
        assert!((after - 0.875).abs() < 0.1, "after the hold: {after}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_format_gets_the_right_arguments() {
        let text = |settings: ExportSettings| -> String { video_args(&settings).join(" ") };
        let telegram = text(ExportSettings::default());
        assert!(telegram.contains("libx264") && telegram.contains("baseline") && telegram.contains("-crf 23"), "{telegram}");

        let vp9 = ExportSettings { container: Container::WebM, video_codec: VideoCodec::Vp9, rate: Rate::Quality(30), ..ExportSettings::default() };
        let vp9 = text(vp9);
        assert!(vp9.contains("libvpx-vp9") && vp9.contains("-crf 30 -b:v 0") && !vp9.contains("profile"), "{vp9}");

        let h265 = text(ExportSettings { video_codec: VideoCodec::H265, rate: Rate::Bitrate(4000), ..ExportSettings::default() });
        assert!(h265.contains("libx265") && h265.contains("hvc1") && h265.contains("-b:v 4000k"), "{h265}");
    }

    #[test]
    fn warns_about_what_is_worth_knowing() {
        let i = MediaInfo { width: 1920, height: 1080, ..info(100.0) };
        let mut s = sequence(100.0);
        let i = files(Path::new("a.mp4"), &i);
        assert!(warnings(&s, &i).iter().any(|w| w.contains("Telegram")));
        s.export = Span { start: 0.0, end: 30.0 };
        assert!(warnings(&s, &i).is_empty(), "{:?}", warnings(&s, &i));

        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        s.trim_end(id, 20.0, 0.1);
        assert!(warnings(&s, &i).iter().any(|w| w.contains("10.0 s")));
        s.export = Span { start: 50.0, end: 80.0 };
        assert!(warnings(&s, &i).iter().any(|w| w.contains("all black")));

        // 1080×1920 from a frame of about 300 source pixels.
        let mut s = sequence(30.0);
        s.output.preset = PresetId::Vertical;
        s.clip_mut(id).transform.zoom = 2.0;
        assert!(warnings(&s, &i).iter().any(|w| w.contains("enlarged")));
    }

    #[test]
    fn reads_progress_lines() {
        assert_eq!(progress_seconds("out_time_us=1500000"), Some(1.5));
        assert_eq!(progress_seconds("out_time_ms=2000000"), Some(2.0));
        assert_eq!(progress_seconds("out_time_us=N/A"), None);
        assert_eq!(progress_seconds("frame=12"), None);
    }

    #[test]
    fn the_part_file_keeps_the_extension() {
        assert_eq!(part_path(Path::new("a/clip one.mp4")), Path::new("a/clip one.part.mp4"));
    }

    /// The main risk of the whole export: that the result is not what the preview shows.
    #[test]
    fn the_result_matches_the_geometry() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("geometry");
        let source = make_source(&dir, 1);
        let frame = rgb_at(&source, 0.0, W, H);
        let i = info(1.0);
        let (w, h) = (W as f64, H as f64);

        let rect = |w: f64, h: f64| Ratio::new(w, h);
        let vertical = Outline { aspect: 9.0 / 16.0, mask: Mask::Rect };
        let cases: [(PresetId, Ratio, Transform); 10] = [
            // With bars: the whole picture in a tall frame, then a frame reaching past the corner.
            (PresetId::Custom, rect(9.0, 16.0), Transform::default().showing_whole_picture(w, h, vertical)),
            (PresetId::Custom, rect(16.0, 10.0), Transform { cx: 0.2, cy: 0.75, zoom: 0.7, angle_deg: 20.0, mirror: false, bars: true, anchor: Anchor::Frame }),
            (PresetId::TelegramCircle, rect(1.0, 1.0), Transform { cx: 0.1, cy: 0.5, zoom: 0.8, angle_deg: 0.0, mirror: true, bars: true, anchor: Anchor::Frame }),
            (PresetId::Custom, rect(9.0, 16.0), Transform { cx: 0.3, cy: 0.5, zoom: 1.4, angle_deg: 0.0, mirror: false, bars: false, anchor: Anchor::Frame }),
            (PresetId::Custom, rect(16.0, 10.0), Transform { cx: 0.55, cy: 0.45, zoom: 1.6, angle_deg: 25.0, mirror: false, bars: false, anchor: Anchor::Frame }),
            (PresetId::Custom, rect(9.0, 16.0), Transform { cx: 0.5, cy: 0.5, zoom: 2.2, angle_deg: -70.0, mirror: false, bars: false, anchor: Anchor::Frame }),
            (PresetId::Custom, rect(4.0, 3.0), Transform { cx: 0.5, cy: 0.5, zoom: 2.0, angle_deg: 12.0, mirror: true, bars: false, anchor: Anchor::Frame }),
            (PresetId::Custom, rect(16.0, 9.0), Transform { cx: 0.5, cy: 0.5, zoom: 1.0, angle_deg: 0.0, mirror: true, bars: false, anchor: Anchor::Frame }),
            (PresetId::TelegramCircle, rect(1.0, 1.0), Transform { cx: 0.5, cy: 0.5, zoom: 1.3, angle_deg: 45.0, mirror: false, bars: false, anchor: Anchor::Frame }),
            // The square around this circle reaches outside the picture when turned.
            (PresetId::TelegramCircle, rect(1.0, 1.0), Transform { cx: 0.0, cy: 0.5, zoom: 1.0, angle_deg: 33.0, mirror: true, bars: false, anchor: Anchor::Frame }),
        ];
        for (n, (preset, ratio, transform)) in cases.into_iter().enumerate() {
            let output = Output { preset, custom: ratio };
            let outline = output.outline();
            let t = transform.clamped(w, h, outline);
            let mut s = sequence(1.0);
            s.export = Span { start: 0.0, end: 0.2 };
            s.output = output;
            s.settings.size = OutputSize::LongSide(320);
            s.settings.rate = Rate::Quality(10);
            s.settings.speed = Speed::Veryfast;
            s.settings.audio_codec = AudioCodec::None;
            s.settings.frame_rate = FrameRate::Source;
            s.settings.profile = crate::core::export::H264Profile::High;
            s.clip_mut(crate::core::edit::ClipId { track: 0, index: 0 }).transform = t;

            let path = render(&source, &i, &s, &dir.join(format!("out{n}.mp4")));
            let [px, py] = s.settings.pixels(output);
            let out = rgb_at(&path, 0.0, px, py);

            let error = geometry_error(&frame, &out, px, py, &t, outline);
            // The same comparison with the frame a little to the side: alignment must matter here.
            let [fw, _] = t.size(w, h, outline);
            let shift = (4.0 * fw / px as f64).max(6.0);
            let shifted = Transform { cx: t.cx + shift / w, ..t };
            let wrong = geometry_error(&frame, &out, px, py, &shifted, outline);
            assert!(error < 14.0, "case {n}: error {error:.1} (shifted {wrong:.1})");
            assert!(wrong > error * 1.5, "case {n}: error {error:.1} is no better than shifted {wrong:.1}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_parts_of_the_range_are_black_and_silent() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("gaps");
        let source = make_source(&dir, 6);
        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        let mut s = sequence(6.0);
        s.trim_start(id, 2.0, 0.1);
        s.trim_end(id, 4.0, 0.1);
        s.export = Span { start: 1.0, end: 5.0 };
        s.settings.fades = (0.0, 0.0);
        s.settings.size = OutputSize::LongSide(320);
        s.settings.frame_rate = FrameRate::Source;
        s.settings.speed = Speed::Veryfast;
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };

        let path = render(&source, &info(6.0), &s, &dir.join("gaps.mp4"));
        let result = probe::probe(&path).unwrap();
        assert!((result.duration - 4.0).abs() < 0.1, "duration {}", result.duration);
        assert!(result.has_audio);
        assert_eq!((result.width, result.height), (320, 180));

        assert!(mean_level(&rgb_at(&path, 0.5, 320, 180)) < 25.0, "the start should be black");
        assert!(mean_level(&rgb_at(&path, 3.5, 320, 180)) < 25.0, "the end should be black");
        assert!(mean_level(&rgb_at(&path, 1.5, 320, 180)) > 40.0, "the clip should be there");

        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        assert!(rms(0.2, 0.8) < 5.0, "silence before the clip: {}", rms(0.2, 0.8));
        assert!(rms(3.2, 3.8) < 5.0, "silence after the clip: {}", rms(3.2, 3.8));
        assert!(rms(1.2, 2.8) > 100.0, "sound in the clip: {}", rms(1.2, 2.8));

        // The silence is really there, not a hole in the timestamps that some players close up.
        let decoded = tools::command(tools::FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-f", "s16le", "-ac", "1", "-ar", "48000", "pipe:1"])
            .output()
            .unwrap();
        let seconds = decoded.stdout.len() as f64 / 2.0 / 48000.0;
        assert!((seconds - 4.0).abs() < 0.1, "{seconds} s of samples");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A clip dragged along the timeline, even to before its zero: the file goes by where it stands.
    #[test]
    fn a_moved_clip_has_black_around_it_and_plays_the_same_picture() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("moved");
        let source = make_source(&dir, 6);
        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        let mut s = sequence(6.0);
        // Source 2..4 s, then moved to -3..-1 s, with 1 s of black on each side in the file.
        s.trim_start(id, 2.0, 0.1);
        s.trim_end(id, 4.0, 0.1);
        s.move_clip(id, -3.0);
        s.export = Span { start: -4.0, end: 0.0 };
        s.settings.fades = (0.0, 0.0);
        s.settings.size = OutputSize::LongSide(W);
        s.settings.rate = Rate::Quality(10);
        s.settings.frame_rate = FrameRate::Source;
        s.settings.speed = Speed::Veryfast;
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };

        let path = render(&source, &info(6.0), &s, &dir.join("moved.mp4"));
        let result = probe::probe(&path).unwrap();
        assert!((result.duration - 4.0).abs() < 0.1, "duration {}", result.duration);

        assert!(mean_level(&rgb_at(&path, 0.5, W, H)) < 25.0, "the start should be black");
        assert!(mean_level(&rgb_at(&path, 3.5, W, H)) < 25.0, "the end should be black");

        // One second into the clip is second 3 of the source, not second 1 or 2.
        let out = rgb_at(&path, 2.0, W, H);
        let right = mean_difference(&out, &rgb_at(&source, 3.0, W, H));
        let earlier = mean_difference(&out, &rgb_at(&source, 2.5, W, H));
        assert!(right < 8.0, "differs by {right:.1} from the right moment");
        assert!(right * 1.5 < earlier, "right {right:.1}, 0.5 s earlier {earlier:.1}");

        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        assert!(rms(0.2, 0.8) < 5.0, "silence before the clip: {}", rms(0.2, 0.8));
        assert!(rms(3.2, 3.8) < 5.0, "silence after the clip: {}", rms(3.2, 3.8));
        assert!(rms(1.2, 2.8) > 100.0, "sound in the clip: {}", rms(1.2, 2.8));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A source where a white frame and a short beep happen together every two seconds, encoded like
    /// a usual file. `video_late` starts its picture that much after its sound.
    fn make_marked_source(dir: &Path, name: &str, rate: &str, every: u32, period: f64, video_late: f64) -> PathBuf {
        let path = dir.join(name);
        let picture = format!("color=c=black:s={W}x{H}:r={rate}:d=20,drawbox=enable='eq(mod(n\\,{every})\\,0)':color=white:t=fill,format=yuv420p");
        let sound = format!("aevalsrc='if(lt(mod(t\\,{period:.9})\\,0.01)\\,0.9*sin(2*PI*1000*t)\\,0)':s=48000:d=20");
        let late = video_late.to_string();
        let mut args = Vec::new();
        if video_late > 0.0 {
            args.extend(["-itsoffset", &late]);
        }
        args.extend(["-f", "lavfi", "-i", &picture, "-f", "lavfi", "-i", &sound]);
        args.extend(["-map", "0:v", "-map", "1:a", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "12", "-c:a", "aac", "-b:a", "256k"]);
        args.push(path.to_str().unwrap());
        ffmpeg(&args);
        path
    }

    /// In a rendered file: the seconds of each white frame minus the seconds of its beep. Negative
    /// means the picture comes first, so the sound lags.
    fn picture_minus_sound(path: &Path, fps: f64) -> Vec<f64> {
        let decode = |args: &[&str]| tools::command(tools::FFMPEG).args(["-v", "error", "-i"]).arg(path).args(args).output().unwrap().stdout;
        let picture = decode(&["-vf", "scale=16:16:flags=area,format=gray", "-fps_mode", "passthrough", "-f", "rawvideo", "pipe:1"]);
        let levels: Vec<f64> = picture.chunks_exact(256).map(|frame| frame.iter().map(|v| *v as f64).sum::<f64>() / 256.0).collect();
        let flashes: Vec<f64> = (0..levels.len()).filter(|&i| levels[i] > 128.0 && (i == 0 || levels[i - 1] <= 128.0)).map(|i| i as f64 / fps).collect();

        let sound = decode(&["-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "s16le", "pipe:1"]);
        let samples: Vec<f64> = sound.chunks_exact(2).map(|b| (i16::from_le_bytes([b[0], b[1]]) as f64).abs()).collect();
        let loudest = samples.iter().cloned().fold(0.0, f64::max);
        let mut beeps: Vec<f64> = Vec::new();
        for (i, level) in samples.iter().enumerate() {
            let t = i as f64 / 48000.0;
            if *level > 0.4 * loudest && beeps.last().is_none_or(|last| t - last > 0.25) {
                beeps.push(t);
            }
        }
        assert!(!flashes.is_empty() && !beeps.is_empty(), "flashes {flashes:?}, beeps {beeps:?}");
        flashes.iter().map(|flash| beeps.iter().map(|beep| flash - beep).fold(f64::MAX, |a, b| if b.abs() < a.abs() { b } else { a })).collect()
    }

    /// Picture and sound go by one clock, wherever the export starts. A frame falls on the output's
    /// frame grid, so half a frame is what can't be avoided.
    #[test]
    fn picture_and_sound_stay_together() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("sync");
        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        let fps = 30.0;
        let half_frame = 0.5 / fps + 0.003;
        let render_marked = |source: &Path, name: &str, duration: f64, start: f64, lead: f64| -> Vec<f64> {
            let mut s = sequence(duration);
            s.trim_start(id, start, 0.1);
            s.export = Span { start: start - lead, end: start + 10.0 };
            s.settings.fades = (0.0, 0.0);
            s.settings.size = OutputSize::LongSide(W);
            s.settings.rate = Rate::Quality(12);
            s.settings.speed = Speed::Ultrafast;
            s.settings.frame_rate = FrameRate::Fixed(fps);
            s.output = Output { preset: PresetId::Landscape, ..Output::default() };
            let i = probe::probe(source).unwrap();
            picture_minus_sound(&render(source, &i, &s, &dir.join(name)), fps)
        };

        let thirty = make_marked_source(&dir, "30.mp4", "30", 60, 2.0, 0.0);
        let ntsc = make_marked_source(&dir, "2997.mp4", "30000/1001", 60, 2.002, 0.0);
        let pal = make_marked_source(&dir, "25.mp4", "25", 50, 2.0, 0.0);
        for (source, label) in [(&thirty, "30"), (&ntsc, "29.97"), (&pal, "25")] {
            // A start on a frame, between frames, right before a frame; with black before the clip too.
            for (start, lead) in [(0.0, 0.0), (5.217, 0.0), (5.235, 0.0), (0.0, 1.2345), (5.217, 1.2345), (5.235, 0.7)] {
                let name = format!("{label}_{start}_{lead}.mp4");
                for error in render_marked(source, &name, 20.0, start, lead) {
                    assert!(error.abs() <= half_frame, "{label} fps, from {start}, lead {lead}: picture minus sound {:.1} ms", error * 1000.0);
                }
            }
        }

        // A picture that starts after the sound stays as far after it as it is in the file. Moving it
        // to the sound's start would make the sound lag by all of that.
        // The first mark says nothing: until the picture starts, its first frame is held on screen.
        let late = make_marked_source(&dir, "late.mp4", "30", 60, 2.0, 0.4);
        let errors = render_marked(&late, "late_out.mp4", 20.0, 0.0, 0.0);
        assert!(errors.len() >= 4, "{errors:?}");
        for error in errors.into_iter().skip(1) {
            assert!((error - 0.4).abs() <= half_frame, "picture minus sound {:.1} ms, should stay 400", error * 1000.0);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A source of one colour, with a tone if `tone` is given.
    fn make_solid(dir: &Path, name: &str, color: &str, rate: u32, size: (u32, u32), tone: Option<u32>) -> PathBuf {
        let path = dir.join(name);
        let picture = format!("color=c={color}:s={}x{}:r={rate}:d=10,format=yuv420p", size.0, size.1);
        let mut args = vec!["-f".to_owned(), "lavfi".to_owned(), "-i".to_owned(), picture];
        if let Some(tone) = tone {
            args.extend(["-f", "lavfi", "-i", &format!("sine=frequency={tone}:duration=10")].map(str::to_owned));
            args.extend(["-c:a", "aac", "-shortest"].map(str::to_owned));
        }
        args.extend(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "10", path.to_str().unwrap()].map(str::to_owned));
        ffmpeg(&args.iter().map(String::as_str).collect::<Vec<_>>());
        path
    }

    /// Clips of three sources on three tracks: what is seen is the top one, what is heard is all of them.
    #[test]
    fn clips_of_several_sources_make_one_file() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("several");
        let gray = make_solid(&dir, "gray.mp4", "0x808080", 30, (320, 180), Some(300));
        let red = make_solid(&dir, "red.mp4", "0xff0000", 25, (640, 360), Some(1000));
        let green = make_solid(&dir, "green.mp4", "0x00ff00", 30, (320, 180), None);
        let files: SourceFiles = [(0, &gray), (1, &red), (2, &green)]
            .into_iter()
            .map(|(id, path)| (id, SourceFile { path: path.clone(), info: probe::probe(path).unwrap() }))
            .collect();

        // Gray 0..6 s at the bottom, red 2..4 s over it from its own second 1, green 5.5..7 s on top.
        let mut s = Sequence::new(Clip { source_out: 6.0, ..Clip::of(0, "gray".into(), 10.0) });
        let track = |id, clip| Track { id, clips: vec![clip], ..Track::default() };
        s.tracks.push(track(1, Clip { start: 2.0, source_in: 1.0, source_out: 3.0, ..Clip::of(1, "red".into(), 10.0) }));
        s.tracks.push(track(2, Clip { start: 5.5, source_out: 1.5, ..Clip::of(2, "green".into(), 10.0) }));
        s.export = Span { start: 0.0, end: 7.5 };
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Source;

        let path = render_all(&files, &s, &dir.join("all.mp4"));
        let result = probe::probe(&path).unwrap();
        assert!((result.duration - 7.5).abs() < 0.1, "duration {}", result.duration);
        assert_eq!((result.width, result.height, result.fps), (320, 180, Rational { num: 30, den: 1 }));
        let color = |t: f64| {
            let rgb = rgb_at(&path, t, 320, 180);
            let channel = |c: usize| rgb.iter().skip(c).step_by(3).map(|v| *v as f64).sum::<f64>() / (rgb.len() / 3) as f64;
            [channel(0), channel(1), channel(2)]
        };
        let near = |got: [f64; 3], want: [f64; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 30.0);
        for (t, want) in [(1.0, [128.0, 128.0, 128.0]), (3.0, [255.0, 0.0, 0.0]), (4.5, [128.0, 128.0, 128.0]), (6.5, [0.0, 255.0, 0.0]), (7.2, [0.0, 0.0, 0.0])] {
            assert!(near(color(t), want), "at {t} s: {:?}, should be {want:?}", color(t));
        }
        // The red clip's sound joins the gray one's; after the gray clip, the green one has none.
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        let (alone, together, silent) = (rms(0.5, 1.5), rms(2.5, 3.5), rms(6.2, 6.9));
        assert!(alone > 100.0 && together > alone * 1.3, "alone {alone}, together {together}");
        assert!(silent < 5.0, "after the clips: {silent}");

        // Hidden, the red track shows the gray one; muted, the gray one is not heard.
        s.tracks[1].hidden = true;
        s.tracks[0].muted = true;
        let path = render_all(&files, &s, &dir.join("hidden.mp4"));
        let rgb = rgb_at(&path, 3.0, 320, 180);
        assert!((mean_level(&rgb) - 128.0).abs() < 30.0, "{}", mean_level(&rgb));
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        assert!(rms(0.5, 1.5) < 5.0 && rms(2.5, 3.5) > 100.0, "{} {}", rms(0.5, 1.5), rms(2.5, 3.5));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Pictures smaller than the frame, turned, past its edges, coming in: each lies over what is
    /// under it, in the order of the tracks, where the preview draws it.
    #[test]
    fn pictures_smaller_than_the_frame_lie_over_what_is_under_them() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("layers");
        let paths = [("red.mp4", "0xff0000"), ("blue.mp4", "0x0000ff"), ("green.mp4", "0x00ff00")].map(|(name, color)| make_solid(&dir, name, color, 25, (320, 180), None));
        let solid = MediaInfo { width: 320, height: 180, fps: Rational { num: 25, den: 1 }, has_audio: false, ..info(10.0) };
        let files: SourceFiles = paths.iter().enumerate().map(|(id, path)| (id as SourceId, SourceFile { path: path.clone(), info: solid.clone() })).collect();
        let mut s = Sequence::new(Clip::of(0, "red".into(), 4.0));
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(640);
        s.settings.rate = Rate::Quality(10);
        s.settings.speed = Speed::Veryfast;
        s.settings.audio_codec = AudioCodec::None;
        s.settings.frame_rate = FrameRate::Fixed(25.0);
        s.settings.fades = (0.0, 0.0);
        let (outline, pixels, w, h) = (s.output.outline(), [640.0, 360.0], 320.0, 180.0);
        // Blue: half the frame across, its middle 200 px right of the frame's and 100 px up, so past
        // the top right corner; from 1 s to 3 s, coming in over half a second.
        let mut blue = Clip { start: 1.0, ..Clip::of(1, "blue".into(), 2.0) };
        blue.transform = Transform::default().placed(w, h, outline, pixels, [200.0, -100.0], 320.0);
        blue.add_fade(Edge::In);
        blue.set_fade_length(Edge::In, 0.5);
        // Green: as wide, in the middle, turned by 45°, from 2 s to 4 s, over the blue one.
        let mut green = Clip { start: 2.0, ..Clip::of(2, "green".into(), 2.0) };
        green.transform = Transform { angle_deg: 45.0, ..Transform::default() }.placed(w, h, outline, pixels, [0.0, 0.0], 320.0);
        s.tracks.push(Track { id: 1, clips: vec![blue], ..Track::default() });
        s.tracks.push(Track { id: 2, clips: vec![green], ..Track::default() });
        s.export = Span { start: 0.0, end: 4.0 };

        let path = render_all(&files, &s, &dir.join("layers.mp4"));
        let color_at = |t: f64, x: usize, y: usize| {
            let rgb = rgb_at(&path, t, 640, 360);
            let i = (y * 640 + x) * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        };
        let named = |rgb: [u8; 3]| {
            let colors = [("red", [255, 0, 0]), ("green", [0, 255, 0]), ("blue", [0, 0, 255])];
            colors.iter().find(|(_, want)| rgb.iter().zip(want).all(|(got, want)| (*got as i32 - want).abs() < 60)).map_or("mixed", |(name, _)| *name)
        };
        for (t, x, y, want) in [
            (0.5, 520, 60, "red"),
            (1.75, 520, 60, "blue"),
            // Its part at the frame's corner: the rest of it is past the frame.
            (1.75, 630, 5, "blue"),
            (1.75, 340, 60, "red"),
            (1.75, 520, 190, "red"),
            (2.5, 320, 180, "green"),
            // Where the green one lies over the blue one, and a corner of its box, empty when turned.
            (2.5, 370, 140, "green"),
            (2.5, 490, 20, "blue"),
            (2.5, 160, 40, "red"),
            (3.5, 520, 60, "red"),
            (3.5, 320, 180, "green"),
        ] {
            assert_eq!(named(color_at(t, x, y)), want, "at {t} s, {x}, {y}: {:?}", color_at(t, x, y));
        }
        // Coming in, the blue one is part of the red one under it.
        let [r, _, b] = color_at(1.25, 520, 60);
        assert!((60..200).contains(&r) && (60..200).contains(&b), "{r} {b}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The mean colour of a frame, as red, green and blue.
    fn mean_color(path: &Path, t: f64, width: u32, height: u32) -> [f64; 3] {
        let rgb = rgb_at(path, t, width, height);
        let channel = |c: usize| rgb.iter().skip(c).step_by(3).map(|v| *v as f64).sum::<f64>() / (rgb.len() / 3) as f64;
        [channel(0), channel(1), channel(2)]
    }

    /// A quiet source and a loud one: as they are, far apart; with even loudness, alike.
    #[test]
    fn even_loudness_brings_quiet_and_loud_together() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("loudness");
        let level = |volume: f64, even: bool| {
            let name = format!("tone_{volume}.mp4");
            let source = dir.join(&name);
            if !source.exists() {
                ffmpeg(&[
                    "-f", "lavfi", "-i", "color=c=gray:s=160x90:r=25:d=6",
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=6",
                    "-af", &format!("volume={volume}"), "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest",
                    source.to_str().unwrap(),
                ]);
            }
            let files: SourceFiles = [(0, SourceFile { path: source.clone(), info: probe::probe(&source).unwrap() })].into();
            let mut s = Sequence::new(Clip { source_out: 5.0, ..Clip::of(0, "tone".into(), 6.0) });
            s.output = Output { preset: PresetId::Landscape, ..Output::default() };
            s.settings.size = OutputSize::LongSide(160);
            s.settings.speed = Speed::Ultrafast;
            s.settings.fades = (0.0, 0.0);
            s.settings.even_loudness = even;
            let path = render_all(&files, &s, &dir.join(format!("out_{volume}_{even}.mp4")));
            20.0 * sound_level(&path, 1.0, 4.0).log10()
        };
        let (quiet, loud) = (level(0.02, false), level(0.5, false));
        assert!(loud - quiet > 20.0, "as they are: {quiet:.1} and {loud:.1} dB");
        let (quiet, loud) = (level(0.02, true), level(0.5, true));
        assert!((loud - quiet).abs() < 2.0, "even: {quiet:.1} and {loud:.1} dB");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Sound taken off its picture and moved is heard where it was moved to; the picture is still
    /// seen, and no longer heard. A file of sound only on an audio track is heard, and never seen.
    #[test]
    fn detached_sound_and_sound_files_are_heard_where_they_are() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("detached");
        let gray = make_solid(&dir, "gray.mp4", "0x808080", 25, (320, 180), Some(440));
        let tone = dir.join("tone.m4a");
        ffmpeg(&["-f", "lavfi", "-i", "sine=frequency=880:duration=10", "-c:a", "aac", tone.to_str().unwrap()]);
        let files: SourceFiles = [(0, &gray), (1, &tone)]
            .into_iter()
            .map(|(id, path)| (id, SourceFile { path: path.clone(), info: probe::probe(path).unwrap() }))
            .collect();
        assert!(!files[&1].info.has_video());

        // Gray 0..4 s; its sound detached and moved to 2..6 s. The export range is 0..6 s.
        let mut s = Sequence::new(Clip { source_out: 4.0, ..Clip::of(0, "gray".into(), 10.0) });
        let (picture, sound) = s.detach_sound(ClipId { track: 0, index: 0 }).unwrap();
        s.unchain(sound);
        s.move_clip(sound, 2.0);
        s.export = Span { start: 0.0, end: 6.0 };
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Fixed(25.0);
        assert_eq!(s.clip(picture).span(), Span { start: 0.0, end: 4.0 });
        let path = render_all(&files, &s, &dir.join("moved.mp4"));
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        assert!(rms(0.3, 1.7) < 5.0, "the picture's own place is silent: {}", rms(0.3, 1.7));
        assert!(rms(2.3, 3.7) > 100.0 && rms(4.3, 5.7) > 100.0, "{} {}", rms(2.3, 3.7), rms(4.3, 5.7));
        assert!((mean_level(&rgb_at(&path, 1.0, 320, 180)) - 128.0).abs() < 20.0, "the picture is seen");

        // The sound file on an audio track at 1..2 s, the moved sound gone: heard there, and the
        // picture stays as it was.
        s.remove_clip(sound);
        let track = s.add_track(0, crate::core::edit::TrackKind::Audio);
        s.place_clip(Some(track), Clip { start: 1.0, source_out: 1.0, ..Clip::of(1, "tone".into(), 10.0) });
        let path = render_all(&files, &s, &dir.join("file.mp4"));
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        assert!(rms(1.2, 1.8) > 100.0 && rms(2.5, 3.5) < 5.0, "{} {}", rms(1.2, 1.8), rms(2.5, 3.5));
        assert!((mean_level(&rgb_at(&path, 1.5, 320, 180)) - 128.0).abs() < 20.0, "sound only is never seen");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A clip's colour in the file is the preview's, and comes before its fade: the fade still ends
    /// in black, however flat the contrast makes black.
    #[test]
    fn a_clips_colour_is_in_the_result_as_the_preview_has_it() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("colour");
        let brown = make_solid(&dir, "brown.mp4", "0x806040", 25, (320, 180), None);
        let files: SourceFiles = [(0, SourceFile { path: brown.clone(), info: probe::probe(&brown).unwrap() })].into();
        let color = crate::core::edit::Color { exposure: 0.4, contrast: -60.0, saturation: 160.0, temperature: 40.0 };
        let mut s = Sequence::new(Clip { source_out: 4.0, ..Clip::of(0, "brown".into(), 10.0) });
        let id = ClipId { track: 0, index: 0 };
        s.clip_mut(id).add_color();
        for effect in &mut s.clip_mut(id).effects {
            if let crate::core::edit::Effect::Color(c) = effect {
                *c = color;
            }
        }
        s.clip_mut(id).add_fade(Edge::Out);
        s.clip_mut(id).set_fade_length(Edge::Out, 1.0);
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Fixed(25.0);

        let plan = plan(&files, &s, &dir.join("graph.mp4")).unwrap();
        let graph = plan.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        let (lut, fade) = (graph.find("lutrgb").expect("a colour"), graph.find(",fade=t=out").expect("a fade"));
        assert!(lut < fade, "the colour before the fade: {graph}");

        let path = render_all(&files, &s, &dir.join("graded.mp4"));
        // The source as the file has it, through the preview's tables.
        let source = mean_color(&brown, 1.0, 320, 180).map(|c| c.round() as u8);
        let mut want = [egui::Color32::from_rgb(source[0], source[1], source[2])];
        crate::media::color::Grade::new(&color).apply(&mut want);
        let want = want[0].to_array();
        let got = mean_color(&path, 1.0, 320, 180);
        for c in 0..3 {
            assert!((got[c] - want[c] as f64).abs() < 5.0, "channel {c}: the file has {got:?}, the preview {want:?} (source {source:?})");
        }
        let end = mean_level(&rgb_at(&path, 3.96, 320, 180));
        assert!(end < 8.0, "the fade ends in black: {end}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A clip fades out of black on its own track, and over another clip on a track above it.
    #[test]
    fn fades_are_seen_and_heard_in_the_result() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("fades_of_clips");
        let gray = make_solid(&dir, "gray.mp4", "0x808080", 25, (320, 180), Some(300));
        let red = make_solid(&dir, "red.mp4", "0xff0000", 25, (320, 180), None);
        let files: SourceFiles = [(0, &gray), (1, &red)]
            .into_iter()
            .map(|(id, path)| (id, SourceFile { path: path.clone(), info: probe::probe(path).unwrap() }))
            .collect();

        // Gray 0..4 s alone, fading in over 2 s and out over 2 s.
        let mut s = Sequence::new(Clip { source_out: 4.0, ..Clip::of(0, "gray".into(), 10.0) });
        let id = ClipId { track: 0, index: 0 };
        s.clip_mut(id).add_fade(Edge::In);
        s.clip_mut(id).add_fade(Edge::Out);
        s.clip_mut(id).set_fade_length(Edge::In, 2.0);
        s.clip_mut(id).set_fade_length(Edge::Out, 2.0);
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Fixed(25.0);
        let path = render_all(&files, &s, &dir.join("alone.mp4"));
        let level = |t: f64| mean_level(&rgb_at(&path, t, 320, 180));
        assert!(level(0.0) < 8.0, "starts black: {}", level(0.0));
        assert!((level(1.0) - 64.0).abs() < 12.0, "half way: {}", level(1.0));
        assert!((level(2.0) - 128.0).abs() < 12.0, "whole: {}", level(2.0));
        assert!((level(3.0) - 64.0).abs() < 12.0, "half way out: {}", level(3.0));
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        let (quiet, full) = (rms(0.0, 0.3), rms(1.9, 2.1));
        assert!(quiet < full * 0.3 && rms(0.9, 1.1) > full * 0.35 && rms(0.9, 1.1) < full * 0.65, "{quiet} {} {full}", rms(0.9, 1.1));

        // Red under it, 0..4 s: the gray clip comes in over the red one instead of out of black.
        s.tracks.insert(0, Track { id: 1, clips: vec![Clip { source_out: 4.0, ..Clip::of(1, "red".into(), 10.0) }], ..Track::default() });
        let path = render_all(&files, &s, &dir.join("over.mp4"));
        let color = |t: f64| mean_color(&path, t, 320, 180);
        let near = |got: [f64; 3], want: [f64; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 20.0);
        assert!(near(color(0.0), [255.0, 0.0, 0.0]), "red first: {:?}", color(0.0));
        assert!(near(color(1.0), [191.0, 64.0, 64.0]), "half and half: {:?}", color(1.0));
        assert!(near(color(2.0), [128.0, 128.0, 128.0]), "gray: {:?}", color(2.0));

        // A saved frame is what the file has there.
        let frame = dir.join("frame.png");
        let job = RenderJob::start(frame_task(&files, &s, 1.0, &frame).unwrap(), Arc::new(|| {}));
        assert!(matches!(wait_for_end(&job), RenderStatus::Done { .. }));
        assert!(near(mean_color(&frame, 0.0, 320, 180), [191.0, 64.0, 64.0]), "{:?}", mean_color(&frame, 0.0, 320, 180));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Fades by curves: out of black and into it alone, over another clip, and in the sound; the file
    /// has what the preview's formula says.
    #[test]
    fn curved_fades_are_seen_and_heard_as_the_preview_has_them() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("curved_fades");
        let gray = make_solid(&dir, "gray.mp4", "0x808080", 25, (320, 180), Some(300));
        let red = make_solid(&dir, "red.mp4", "0xff0000", 25, (320, 180), None);
        let files: SourceFiles = [(0, &gray), (1, &red)]
            .into_iter()
            .map(|(id, path)| (id, SourceFile { path: path.clone(), info: probe::probe(path).unwrap() }))
            .collect();

        // Gray 0..4 s alone, in over 2 s with an ease in, out over 2 s with an ease out.
        let mut s = Sequence::new(Clip { source_out: 4.0, ..Clip::of(0, "gray".into(), 10.0) });
        let id = ClipId { track: 0, index: 0 };
        for (edge, curve) in [(Edge::In, Curve::EaseIn), (Edge::Out, Curve::EaseOut)] {
            s.clip_mut(id).add_fade(edge);
            s.clip_mut(id).set_fade_length(edge, 2.0);
            s.clip_mut(id).set_fade_curve(edge, curve);
        }
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(320);
        s.settings.speed = Speed::Ultrafast;
        s.settings.fades = (0.0, 0.0);
        s.settings.frame_rate = FrameRate::Fixed(25.0);
        let path = render_all(&files, &s, &dir.join("alone.mp4"));
        let level = |t: f64| mean_level(&rgb_at(&path, t, 320, 180));
        let full = level(2.0);
        assert!((full - 128.0).abs() < 12.0, "whole: {full}");
        for t in [0.0, 0.5, 1.0, 1.5, 2.5, 3.0, 3.5] {
            let want = full * s.clip(id).opacity(t);
            assert!((level(t) - want).abs() < 6.0, "at {t} s: {}, should be {want:.1}", level(t));
        }
        // The sound goes by the same curves: a quarter of the way in at 1 s, not a half.
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        let (early, loud) = (rms(0.95, 1.05), rms(1.95, 2.05));
        assert!((early / loud - 0.25).abs() < 0.06, "at 1 s: {early} of {loud}");
        assert!((rms(2.95, 3.05) / loud - 0.25).abs() < 0.06, "at 3 s: {} of {loud}", rms(2.95, 3.05));

        // Red under it, 0..4 s, and a strong ease in-out: the gray clip comes in over the red one.
        s.clip_mut(id).set_fade_curve(Edge::In, Curve::StrongEaseInOut);
        s.tracks.insert(0, Track { id: 1, clips: vec![Clip { source_out: 4.0, ..Clip::of(1, "red".into(), 10.0) }], ..Track::default() });
        let path = render_all(&files, &s, &dir.join("over.mp4"));
        let top = ClipId { track: 1, index: 0 };
        for t in [0.5, 1.0, 1.5] {
            let a = s.clip(top).opacity(t);
            let want = [255.0 * (1.0 - a) + 128.0 * a, 128.0 * a, 128.0 * a];
            let got = mean_color(&path, t, 320, 180);
            assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 20.0), "at {t} s: {got:?}, should be {want:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// The first frame with a fast seek against the same moment decoded from the start.
    fn assert_first_frame_matches(source: &Path, i: &MediaInfo, start: f64, dir: &Path) {
        let mut s = sequence(i.duration);
        s.export = Span { start, end: start + 0.4 };
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(W);
        s.settings.rate = Rate::Quality(10);
        s.settings.speed = Speed::Veryfast;
        s.settings.frame_rate = FrameRate::Source;
        s.settings.fades = (0.0, 0.0);
        let path = render(source, i, &s, &dir.join("seek.mp4"));

        let out = rgb_at(&path, 0.0, W, H);
        let right = mean_difference(&out, &rgb_at(source, start, W, H));
        let earlier = mean_difference(&out, &rgb_at(source, start - 0.2, W, H));
        assert!(right < 8.0, "first frame differs by {right:.1} from the right moment");
        assert!(right * 1.5 < earlier, "right {right:.1}, 0.2 s earlier {earlier:.1}");
    }

    #[test]
    fn a_fast_seek_lands_on_the_right_frame() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("seek");
        let source = make_source(&dir, 8);
        assert_first_frame_matches(&source, &info(8.0), 5.2, &dir);
        let _ = fs::remove_dir_all(&dir);
    }

    /// tgCircleGen normalized its downloads because of timestamp trouble, phones record like this.
    #[test]
    fn a_fast_seek_works_with_an_uneven_frame_rate() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("vfr");
        let even = make_source(&dir, 8);
        let uneven = dir.join("uneven.mp4");
        // Drops frames unevenly: the timestamps stay, the gaps between frames differ.
        ffmpeg(&[
            "-i", even.to_str().unwrap(),
            "-vf", "select='not(eq(mod(n\\,5)\\,1))*not(eq(mod(n\\,7)\\,3))'",
            "-fps_mode", "vfr", "-c:v", "libx264", "-crf", "10", "-pix_fmt", "yuv420p", "-c:a", "copy",
            uneven.to_str().unwrap(),
        ]);
        let i = probe::probe(&uneven).unwrap();
        assert_first_frame_matches(&uneven, &i, 5.2, &dir);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fades_are_heard_in_the_result() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("fades");
        let source = make_source(&dir, 4);
        let mut s = sequence(4.0);
        s.settings.fades = (1.0, 1.0);
        s.settings.speed = Speed::Veryfast;
        let path = render(&source, &info(4.0), &s, &dir.join("fades.mp4"));
        let rms = |from: f64, to: f64| sound_level(&path, from, to);
        let (quiet, full, end) = (rms(0.0, 0.2), rms(1.5, 2.5), rms(3.8, 4.0));
        assert!(quiet < full * 0.3, "start {quiet}, middle {full}");
        assert!(end < full * 0.3, "end {end}, middle {full}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn volume_and_mute_reach_the_result() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("volume");
        let source = make_source(&dir, 2);
        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        let level = |name: &str, volume: f64, muted: bool| -> f64 {
            let mut s = sequence(2.0);
            s.settings.fades = (0.0, 0.0);
            s.settings.speed = Speed::Ultrafast;
            s.clip_mut(id).volume = volume;
            s.clip_mut(id).muted = muted;
            let path = render(&source, &info(2.0), &s, &dir.join(name));
            sound_level(&path, 0.5, 1.5)
        };
        let full = level("full.mp4", 1.0, false);
        let half = level("half.mp4", 0.5, false);
        let muted = level("muted.mp4", 1.0, true);
        assert!(full > 100.0, "{full}");
        assert!((half / full - 0.5).abs() < 0.1, "half {half}, full {full}");
        assert!(muted < 5.0, "muted {muted}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_frame_is_the_picture_at_that_moment() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("frame");
        let source = make_source(&dir, 8);
        let i = info(8.0);
        let mut s = sequence(8.0);
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        s.settings.size = OutputSize::LongSide(W);
        let output = dir.join("frame.png");
        let notify: Notify = Arc::new(|| {});

        let job = RenderJob::start(frame_task(&files(&source, &i), &s, 5.2, &output).unwrap(), notify);
        assert!(matches!(wait_for_end(&job), RenderStatus::Done { .. }));
        assert!(!part_path(&output).exists());

        let saved = rgb_at(&output, 0.0, W, H);
        let right = mean_difference(&saved, &rgb_at(&source, 5.2, W, H));
        let earlier = mean_difference(&saved, &rgb_at(&source, 5.0, W, H));
        assert!(right < 8.0 && right * 1.5 < earlier, "right {right:.1}, 0.2 s earlier {earlier:.1}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// What is drawn for comparison is what gets rendered.
    #[test]
    fn a_variant_is_the_frame_shown_inside_the_main_one() {
        let i = MediaInfo { width: 1920, height: 1080, ..info(10.0) };
        let (w, h) = (1920.0, 1080.0);
        let mut s = sequence(10.0);
        s.output = Output { preset: PresetId::Vertical, ..Output::default() };
        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        s.clip_mut(id).transform = Transform { cx: 0.4, cy: 0.5, zoom: 1.5, angle_deg: 20.0, mirror: false, bars: false, anchor: Anchor::Frame };
        let main = s.clip(id).transform;
        let [fw, fh] = main.size(w, h, s.output.outline());

        for preset in [PresetId::Square, PresetId::Landscape, PresetId::TelegramCircle] {
            let v = variant(&s, &files(Path::new("a.mp4"), &i), get(preset));
            let t = v.clip(id).transform;
            let outline = v.output.outline();
            assert_eq!(v.output.preset, preset);
            assert_eq!((t.cx, t.cy, t.angle_deg), (main.cx, main.cy, main.angle_deg));
            let [vw, vh] = t.size(w, h, outline);
            let [ew, eh] = outline.fit(fw, fh);
            assert!((vw - ew).abs() < 1e-6 && (vh - eh).abs() < 1e-6, "{preset:?}: {vw}×{vh}, expected {ew}×{eh}");
        }
    }

    #[test]
    fn a_pinned_picture_stays_in_its_corner_in_other_formats() {
        let i = MediaInfo { width: 1920, height: 1080, ..info(10.0) };
        let (w, h) = (1920.0, 1080.0);
        let mut s = sequence(10.0);
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        let id = ClipId { track: 0, index: 0 };
        let (outline, pixels) = (s.output.outline(), [1920.0, 1080.0]);
        let start = Transform { zoom: 0.3, anchor: Anchor::TopRight, ..Transform::default() };
        let place = start.placement(w, h, outline, pixels);
        s.clip_mut(id).transform = start.placed(w, h, outline, pixels, place.center_for(Anchor::TopRight, pixels, [40.0, 40.0]), place.size[0]);
        for preset in [PresetId::Vertical, PresetId::Square, PresetId::TelegramCircle] {
            let v = variant(&s, &files(Path::new("a.mp4"), &i), get(preset));
            let o = v.output.outline();
            // A frame as long on its shorter side as the main one.
            let pixels = if o.aspect >= 1.0 { [1080.0 * o.aspect, 1080.0] } else { [1080.0, 1080.0 / o.aspect] };
            let moved = v.clip(id).transform.placement(w, h, o, pixels);
            let [x, y] = moved.offsets(Anchor::TopRight, pixels);
            assert!((x - 40.0).abs() < 1e-6 && (y - 40.0).abs() < 1e-6, "{preset:?}: {x}, {y}");
            assert!((moved.size[0] - place.size[0]).abs() < 1e-6, "{preset:?}");
        }
    }

    /// How loud a part of a file's sound is: the RMS level of the samples.
    fn sound_level(path: &Path, from: f64, to: f64) -> f64 {
        let filter = format!("atrim=start={from}:end={to}");
        let output = tools::command(tools::FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-af", &filter, "-f", "s16le", "-ac", "1", "pipe:1"])
            .output()
            .unwrap();
        let samples: Vec<f64> = output.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f64).collect();
        assert!(!samples.is_empty());
        (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn other_formats_render() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("formats");
        let source = make_source(&dir, 1);
        for (name, container, video, audio) in [
            ("a.mov", Container::Mov, VideoCodec::H264, AudioCodec::Aac),
            ("b.webm", Container::WebM, VideoCodec::Vp9, AudioCodec::Opus),
            ("c.mkv", Container::Mkv, VideoCodec::H264, AudioCodec::None),
        ] {
            let mut s = sequence(1.0);
            s.settings = ExportSettings {
                container,
                video_codec: video,
                audio_codec: audio,
                speed: Speed::Ultrafast,
                profile: crate::core::export::H264Profile::High,
                ..s.settings
            };
            let path = render(&source, &info(1.0), &s, &dir.join(name));
            let result = probe::probe(&path).unwrap();
            assert_eq!((result.width, result.height), (400, 400), "{name}");
            assert_eq!(result.has_audio, audio != AudioCodec::None, "{name}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    fn wait_for_end(job: &RenderJob) -> RenderStatus {
        let started = Instant::now();
        loop {
            let status = job.status();
            if !matches!(status, RenderStatus::Running(_)) {
                return status;
            }
            assert!(started.elapsed() < Duration::from_secs(120), "the job does not end");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_job_saves_the_file_only_when_it_is_complete() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("job");
        let source = make_source(&dir, 2);
        let output = dir.join("done.mp4");
        let mut s = sequence(2.0);
        s.settings.speed = Speed::Ultrafast;
        let notify: Notify = Arc::new(|| {});

        let job = RenderJob::start(video_task(&files(&source, &info(2.0)), &s, &output).unwrap(), notify.clone());
        match wait_for_end(&job) {
            RenderStatus::Done { path, bytes } => {
                assert_eq!(path, output);
                assert!(bytes > 0);
            }
            other => panic!("{other:?}"),
        }
        assert!(output.exists());
        assert!(!part_path(&output).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_job_leaves_nothing_behind() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("cancel");
        // Long enough that finishing by itself could not pass for stopping.
        let source = make_source(&dir, 60);
        let output = dir.join("cancelled.mp4");
        let mut s = sequence(60.0);
        s.settings.speed = Speed::Veryslow;
        s.settings.size = OutputSize::LongSide(1920);
        s.output = Output { preset: PresetId::Landscape, ..Output::default() };
        let notify: Notify = Arc::new(|| {});

        let job = RenderJob::start(video_task(&files(&source, &info(60.0)), &s, &output).unwrap(), notify);
        thread::sleep(Duration::from_millis(300));
        let cancelled_at = Instant::now();
        job.cancel();
        assert_eq!(wait_for_end(&job), RenderStatus::Cancelled);
        assert!(cancelled_at.elapsed() < Duration::from_secs(5), "cancelling took {:?}", cancelled_at.elapsed());
        assert!(!output.exists());
        assert!(!part_path(&output).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_job_says_why_and_leaves_nothing_behind() {
        if !have_ffmpeg() {
            return;
        }
        let dir = temp_dir("fail");
        let output = dir.join("failed.mp4");
        let notify: Notify = Arc::new(|| {});
        let job = RenderJob::start(video_task(&files(&dir.join("missing.mp4"), &info(2.0)), &sequence(2.0), &output).unwrap(), notify);
        match wait_for_end(&job) {
            RenderStatus::Failed(message) => assert!(!message.is_empty()),
            other => panic!("{other:?}"),
        }
        assert!(!output.exists());
        assert!(!part_path(&output).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalized_angles_reach_the_filter() {
        let t = Transform { angle_deg: geometry::normalize_angle(-180.0), ..Transform::default() };
        let filter = frame_filter(W as f64, H as f64, &t, Outline { aspect: 1.0, mask: Mask::Circle }, [400, 400]);
        assert!(filter.contains("rotate=3.14159265"), "{filter}");
    }
}

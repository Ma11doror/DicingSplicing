//! Rendering the edit to a file with ffmpeg, always from the original source.

use crate::core::edit::Sequence;
use crate::core::export::{ExportSettings, FrameRate, Rate, VideoCodec};
use crate::core::preset::Preset;
use crate::core::tools;
use crate::media::geometry::{Mask, Outline, Transform};
use crate::media::probe::MediaInfo;
use crate::media::Notify;
use anyhow::{Context, Result};
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

/// The part of the source that plays in the export range, and the emptiness around it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Timing {
    total: f64,
    /// Black and silence before and after the clip.
    lead: f64,
    tail: f64,
    /// Source seconds; `None` if no clip reaches into the range.
    source: Option<(f64, f64)>,
}

/// There is one clip for now. Several clips will need overlay and a sound mix here.
fn timing(sequence: &Sequence) -> Timing {
    let export = sequence.export;
    let total = export.len();
    let clip = sequence.clips().next().map(|(_, clip)| clip);
    let overlap = clip.and_then(|clip| {
        let span = clip.span();
        let (start, end) = (span.start.max(export.start), span.end.min(export.end));
        (end > start).then_some((clip, start, end))
    });
    match overlap {
        Some((clip, start, end)) => Timing {
            total,
            lead: start - export.start,
            tail: export.end - end,
            source: Some((clip.source_time(start), clip.source_time(end))),
        },
        None => Timing { total, lead: total, tail: 0.0, source: None },
    }
}

fn frame_rate_value(settings: &ExportSettings, info: &MediaInfo) -> String {
    match settings.frame_rate {
        FrameRate::Source => format!("{}/{}", info.fps.num, info.fps.den),
        FrameRate::Fixed(fps) => fps.to_string(),
    }
}

fn frame_rate_number(settings: &ExportSettings, info: &MediaInfo) -> f64 {
    match settings.frame_rate {
        FrameRate::Source => info.fps.as_f64(),
        FrameRate::Fixed(fps) => fps,
    }
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
}

/// What ffmpeg has to do to write the edit to `output`.
pub fn plan(source: &Path, info: &MediaInfo, sequence: &Sequence, output: &Path) -> RenderPlan {
    let settings = &sequence.settings;
    let (w, h) = (info.width as f64, info.height as f64);
    let pixels = settings.pixels(sequence.output);
    let fps = frame_rate_value(settings, info);
    let time = timing(sequence);
    let has_sound = info.has_audio && settings.audio_codec.encoder().is_some();
    let clip = sequence.clips().next().map(|(_, clip)| clip);
    let transform = clip.map_or_else(Transform::default, |clip| clip.transform);
    let gain = clip.map_or(1.0, |clip| clip.gain());

    let mut args: Vec<OsString> = ["-v", "error", "-nostdin", "-nostats", "-progress", "pipe:1", "-y"].map(OsString::from).to_vec();
    let mut graph = Vec::new();
    match time.source {
        Some((start, end)) => {
            // Seeking before the input saves decoding everything up to the start. Timestamps then
            // count from the point the seek landed on.
            let seek = (start - SEEK_MARGIN).max(0.0);
            if seek > 0.0 {
                args.push("-ss".into());
                args.push(format!("{seek:.6}").into());
            }
            args.push("-i".into());
            args.push(source.into());
            let (from, to) = (start - seek, end - seek);

            // One clock for picture and sound: the source's moment `from` lands on the file's moment
            // `lead`. Counting each stream from its own first frame instead would put the picture ahead
            // of the sound, by as much as that frame is after `from` or after the sound's start.
            //
            // Black comes in whole frames, the picture takes up the rest of the lead. Frames before the
            // start stay until `fps`, which needs them to know what is on screen at that moment.
            let frame = frame_filter(w, h, &transform, sequence.output.outline(), pixels);
            let rate = frame_rate_number(settings, info);
            let black = (time.lead * rate).round();
            let shift = time.lead - black / rate - from;
            let mut video = format!("[0:V:0]trim=end={to:.6},setpts=PTS{shift:+.6}/TB,fps={fps}:start_time=0,{frame}");
            if black > 0.0 || time.tail > 0.0 {
                video += &format!(",tpad=start={black}:stop_duration={:.6}:color=black", time.tail);
            }
            graph.push(video + ",format=yuv420p[v]");
            if has_sound {
                // Moved by the lead, then filled from 0: adelay only shifts the timestamps, which would
                // leave a hole in the stream instead of silence. The filling also evens out gaps in
                // the source's own sound, so that it stays in sync with the picture.
                let sound = format!(
                    "[0:a:0]atrim=start={from:.6}:end={to:.6},asetpts=PTS-{from:.6}/TB+{:.6}/TB,aresample=async=1:first_pts=0,\
                     volume={gain:.4},apad=whole_dur={:.6},atrim=end={:.6}",
                    time.lead, time.total, time.total
                );
                graph.push(sound + &fade_filters(settings, time.total) + "[a]");
            }
        }
        None => {
            let [px, py] = pixels;
            graph.push(format!("color=c=black:s={px}x{py}:r={fps}:d={:.6},format=yuv420p[v]", time.total));
            if has_sound {
                graph.push(format!("anullsrc=r=48000:cl=stereo:d={:.6}[a]", time.total));
            }
        }
    }

    args.push("-filter_complex".into());
    args.push(graph.join(";").into());
    args.extend(["-map", "[v]"].map(OsString::from));
    if has_sound {
        args.extend(["-map", "[a]"].map(OsString::from));
    }
    args.push("-t".into());
    args.push(format!("{:.6}", time.total).into());
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
    RenderPlan { args, total: time.total }
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
/// inside the main frame.
pub fn variant(sequence: &Sequence, info: &MediaInfo, preset: &Preset) -> Sequence {
    let (w, h) = (info.width as f64, info.height as f64);
    let main = sequence.output.outline();
    let outline = sequence.output.outline_of(preset);
    let mut out = sequence.clone();
    out.output.preset = preset.id;
    for track in &mut out.tracks {
        for clip in &mut track.clips {
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
pub fn video_task(source: &Path, info: &MediaInfo, sequence: &Sequence, output: &Path) -> RenderTask {
    let part = part_path(output);
    RenderTask { plan: plan(source, info, sequence, &part), part, output: output.to_owned() }
}

/// Saves the output's picture at timeline `time` as an image, from the original source.
pub fn frame_task(source: &Path, info: &MediaInfo, sequence: &Sequence, time: f64, output: &Path) -> RenderTask {
    let part = part_path(output);
    let clip = sequence.clip_at(time).map_or_else(|| sequence.clips().next().map(|(_, clip)| clip), |id| Some(sequence.clip(id)));
    let last_frame = (info.duration - 1.0 / info.fps.as_f64()).max(0.0);
    let source_time = clip.map_or(0.0, |clip| clip.source_time(time)).clamp(0.0, last_frame);
    let transform = clip.map_or_else(Transform::default, |clip| clip.transform);
    let filter = frame_filter(info.width as f64, info.height as f64, &transform, sequence.output.outline(), sequence.settings.pixels(sequence.output));

    let mut args: Vec<OsString> = ["-v", "error", "-nostdin", "-nostats", "-progress", "pipe:1", "-y", "-ss"].map(OsString::from).to_vec();
    args.push(format!("{source_time:.6}").into());
    args.push("-i".into());
    args.push(source.into());
    args.push("-vf".into());
    args.push(filter.into());
    args.extend(["-frames:v", "1", "-update", "1"].map(OsString::from));
    args.push((&part).into());
    RenderTask { plan: RenderPlan { args, total: 1.0 }, part, output: output.to_owned() }
}

/// What is worth knowing before the render. None of this stops it.
pub fn warnings(sequence: &Sequence, info: &MediaInfo) -> Vec<String> {
    let mut warnings = Vec::new();
    let export = sequence.export;
    let time = timing(sequence);
    if time.source.is_none() {
        warnings.push("No clip reaches into the export range: the result is all black.".to_owned());
    } else if time.lead > 0.0 || time.tail > 0.0 {
        warnings.push(format!("{:.1} s of the export range have no clip: black and silence.", time.lead + time.tail));
    }
    let outline = sequence.output.outline();
    if outline.mask == Mask::Circle && !(TELEGRAM_MIN_SECONDS..=TELEGRAM_MAX_SECONDS).contains(&export.len()) {
        warnings.push(format!(
            "A Telegram circle should last {TELEGRAM_MIN_SECONDS:.0} to {TELEGRAM_MAX_SECONDS:.0} s, the export range has {:.1} s.",
            export.len()
        ));
    }
    if let Some((_, clip)) = sequence.clips().next() {
        let [fw, _] = clip.transform.size(info.width as f64, info.height as f64, outline);
        let [px, _] = sequence.settings.pixels(sequence.output);
        let enlarged = px as f64 / fw;
        if enlarged > 1.05 {
            warnings.push(format!("The picture is enlarged {enlarged:.1} times: the frame covers fewer source pixels than the result has."));
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
    use crate::core::edit::{Clip, Span};
    use crate::core::export::{AudioCodec, Container, OutputSize, Speed};
    use crate::core::preset::{get, Output, PresetId, Ratio};
    use crate::media::probe::{self, Rational};
    use crate::media::geometry;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const W: u32 = 640;
    const H: u32 = 360;

    fn info(duration: f64) -> MediaInfo {
        MediaInfo { duration, width: W, height: H, fps: Rational { num: 25, den: 1 }, has_audio: true }
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
        let dir = std::env::temp_dir().join(format!("simple_editor_render_{}_{name}", std::process::id()));
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
    fn render(source: &Path, info: &MediaInfo, sequence: &Sequence, output: &Path) -> PathBuf {
        let plan = plan(source, info, sequence, output);
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

        let corner = Transform { cx: 0.2, cy: 0.5, zoom: 1.0, angle_deg: 30.0, mirror: true, bars: false };
        let filter = frame_filter(1920.0, 1080.0, &corner, outline, [400, 400]);
        assert!(filter.starts_with("pad="), "{filter}");
        assert!(filter.contains("hflip"), "{filter}");
    }

    #[test]
    fn timing_follows_the_clip_and_the_export_range() {
        let mut s = sequence(10.0);
        assert_eq!(timing(&s), Timing { total: 10.0, lead: 0.0, tail: 0.0, source: Some((0.0, 10.0)) });

        // The clip is trimmed to source 2..4 in place, the export range goes around it.
        s.trim_start(crate::core::edit::ClipId { track: 0, index: 0 }, 2.0, 0.1);
        s.trim_end(crate::core::edit::ClipId { track: 0, index: 0 }, 4.0, 0.1);
        s.export = Span { start: 1.0, end: 5.0 };
        assert_eq!(timing(&s), Timing { total: 4.0, lead: 1.0, tail: 1.0, source: Some((2.0, 4.0)) });

        s.export = Span { start: 6.0, end: 8.0 };
        assert_eq!(timing(&s), Timing { total: 2.0, lead: 2.0, tail: 0.0, source: None });
    }

    #[test]
    fn timing_goes_by_where_the_clip_stands() {
        let id = crate::core::edit::ClipId { track: 0, index: 0 };
        let mut s = sequence(10.0);
        s.move_clip(id, -3.0);

        // The range starts before the clip and ends inside it.
        s.export = Span { start: -5.0, end: 2.0 };
        assert_eq!(timing(&s), Timing { total: 7.0, lead: 2.0, tail: 0.0, source: Some((0.0, 5.0)) });

        // The range starts inside the clip and ends after it.
        s.export = Span { start: -1.0, end: 12.0 };
        assert_eq!(timing(&s), Timing { total: 13.0, lead: 0.0, tail: 5.0, source: Some((2.0, 10.0)) });

        // The clip is out of the range: all black, and the warning says so.
        s.export = Span { start: 20.0, end: 25.0 };
        assert_eq!(timing(&s), Timing { total: 5.0, lead: 5.0, tail: 0.0, source: None });
        assert!(warnings(&s, &info(10.0)).iter().any(|w| w.contains("all black")));
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
            (PresetId::Custom, rect(16.0, 10.0), Transform { cx: 0.2, cy: 0.75, zoom: 0.7, angle_deg: 20.0, mirror: false, bars: true }),
            (PresetId::TelegramCircle, rect(1.0, 1.0), Transform { cx: 0.1, cy: 0.5, zoom: 0.8, angle_deg: 0.0, mirror: true, bars: true }),
            (PresetId::Custom, rect(9.0, 16.0), Transform { cx: 0.3, cy: 0.5, zoom: 1.4, angle_deg: 0.0, mirror: false, bars: false }),
            (PresetId::Custom, rect(16.0, 10.0), Transform { cx: 0.55, cy: 0.45, zoom: 1.6, angle_deg: 25.0, mirror: false, bars: false }),
            (PresetId::Custom, rect(9.0, 16.0), Transform { cx: 0.5, cy: 0.5, zoom: 2.2, angle_deg: -70.0, mirror: false, bars: false }),
            (PresetId::Custom, rect(4.0, 3.0), Transform { cx: 0.5, cy: 0.5, zoom: 2.0, angle_deg: 12.0, mirror: true, bars: false }),
            (PresetId::Custom, rect(16.0, 9.0), Transform { cx: 0.5, cy: 0.5, zoom: 1.0, angle_deg: 0.0, mirror: true, bars: false }),
            (PresetId::TelegramCircle, rect(1.0, 1.0), Transform { cx: 0.5, cy: 0.5, zoom: 1.3, angle_deg: 45.0, mirror: false, bars: false }),
            // The square around this circle reaches outside the picture when turned.
            (PresetId::TelegramCircle, rect(1.0, 1.0), Transform { cx: 0.0, cy: 0.5, zoom: 1.0, angle_deg: 33.0, mirror: true, bars: false }),
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

        let job = RenderJob::start(frame_task(&source, &i, &s, 5.2, &output), notify);
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
        s.clip_mut(id).transform = Transform { cx: 0.4, cy: 0.5, zoom: 1.5, angle_deg: 20.0, mirror: false, bars: false };
        let main = s.clip(id).transform;
        let [fw, fh] = main.size(w, h, s.output.outline());

        for preset in [PresetId::Square, PresetId::Landscape, PresetId::TelegramCircle] {
            let v = variant(&s, &i, get(preset));
            let t = v.clip(id).transform;
            let outline = v.output.outline();
            assert_eq!(v.output.preset, preset);
            assert_eq!((t.cx, t.cy, t.angle_deg), (main.cx, main.cy, main.angle_deg));
            let [vw, vh] = t.size(w, h, outline);
            let [ew, eh] = outline.fit(fw, fh);
            assert!((vw - ew).abs() < 1e-6 && (vh - eh).abs() < 1e-6, "{preset:?}: {vw}×{vh}, expected {ew}×{eh}");
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

        let job = RenderJob::start(video_task(&source, &info(2.0), &s, &output), notify.clone());
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

        let job = RenderJob::start(video_task(&source, &info(60.0), &s, &output), notify);
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
        let job = RenderJob::start(video_task(&dir.join("missing.mp4"), &info(2.0), &sequence(2.0), &output), notify);
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

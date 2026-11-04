//! The export window: presets on top, the detailed settings under them, then the render itself.

use crate::core::edit::Sequence;
use crate::core::export::{
    AudioCodec, Container, ExportPreset, FrameRate, H264Profile, OutputSize, Rate, Setting, Speed, VideoCodec, EXPORT_PRESETS, PRESET_GROUPS,
};
use crate::core::preset::Preset;
use crate::core::tools;
use crate::media::geometry::Mask;
use crate::media::probe::MediaInfo;
use crate::media::render::{self, RenderJob, RenderStatus, RenderTask};
use crate::media::Notify;
use crate::ui::flash;
use egui::{Color32, RichText};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const WARNING: Color32 = Color32::from_rgb(230, 170, 70);
const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
const COMBO_WIDTH: f32 = 170.0;
const START_CRF: u8 = 23;
const START_KBPS: u32 = 4000;

/// One file to write.
struct Target {
    task: RenderTask,
    /// It is a video for Telegram, where the size of the file has a limit.
    for_telegram: bool,
}

/// The files being written, one after another.
struct Run {
    job: RenderJob,
    for_telegram: bool,
    pending: VecDeque<Target>,
    total: usize,
}

struct Saved {
    path: PathBuf,
    bytes: u64,
    for_telegram: bool,
}

/// How the last run went.
#[derive(Default)]
struct Outcome {
    saved: Vec<Saved>,
    error: Option<String>,
    cancelled: bool,
}

#[derive(Default)]
pub struct ExportWindow {
    open: bool,
    path: Option<PathBuf>,
    /// Also render the frames shown for comparison.
    with_shown: bool,
    run: Option<Run>,
    outcome: Outcome,
    message: Option<String>,
    /// The lines that flash, and since when.
    highlighted: Vec<Setting>,
    highlighted_at: Option<f64>,
    encoders: Option<Vec<String>>,
}

/// One line of the settings, flashing if `started` says so.
fn row(ui: &mut egui::Ui, started: Option<f64>, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.label(label);
    flash::frame(ui, started, false).show(ui, |ui| {
        ui.horizontal(contents);
    });
    ui.end_row();
}

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, id: &str, value: &mut T, options: &[T], label: impl Fn(T) -> &'static str, enabled: impl Fn(T) -> bool) {
    egui::ComboBox::from_id_salt(id).selected_text(label(*value)).width(COMBO_WIDTH).show_ui(ui, |ui| {
        for &option in options {
            ui.add_enabled_ui(enabled(option), |ui| {
                ui.selectable_value(value, option, label(option));
            });
        }
    });
}

fn has(encoders: &[String], encoder: &str) -> bool {
    // An empty list means ffmpeg could not be asked: then let the render itself report it.
    encoders.is_empty() || encoders.iter().any(|e| e == encoder)
}

fn preset_is_available(preset: &ExportPreset, encoders: &[String]) -> bool {
    preset.video_codec.is_none_or(|codec| has(encoders, codec.encoder()))
        && preset.audio_codec.and_then(AudioCodec::encoder).is_none_or(|encoder| has(encoders, encoder))
}

/// A frame's name as it goes into a file name.
fn file_label(name: &str) -> String {
    name.replace(", ", " ")
}

fn suggested_path(source: &Path, sequence: &Sequence) -> PathBuf {
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    let frame = file_label(sequence.output.preset().name);
    source.with_file_name(format!("{stem} - {frame}.{}", sequence.settings.container.extension()))
}

/// Where another frame goes next to `main`. If `main` is named after its frame, this replaces that.
fn variant_path(main: &Path, main_frame: &str, frame: &str) -> PathBuf {
    let stem = main.file_stem().unwrap_or_default().to_string_lossy();
    let suffix = format!(" - {}", file_label(main_frame));
    let base = stem.strip_suffix(suffix.as_str()).unwrap_or(&stem);
    let extension = main.extension().unwrap_or_default().to_string_lossy();
    main.with_file_name(format!("{base} - {}.{extension}", file_label(frame)))
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

fn notifier(ctx: &egui::Context) -> Notify {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

impl ExportWindow {
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn is_busy(&self) -> bool {
        self.run.is_some()
    }

    /// How far the whole run has come, from 0 to 1.
    fn progress(&self) -> Option<f32> {
        let run = self.run.as_ref()?;
        let current = match run.job.status() {
            RenderStatus::Running(progress) => progress,
            _ => 1.0,
        };
        let finished = run.total - run.pending.len() - 1;
        Some((finished as f32 + current) / run.total as f32)
    }

    /// What the button that opens the window should say.
    pub fn button_text(&self) -> String {
        match self.progress() {
            Some(progress) => format!("Exporting {:.0}%", progress * 100.0),
            None => "Export…".to_owned(),
        }
    }

    fn highlight(&mut self, lines: Vec<Setting>, now: f64) {
        self.highlighted = lines;
        self.highlighted_at = Some(now);
    }

    fn started(&self, line: Setting) -> Option<f64> {
        self.highlighted_at.filter(|_| self.highlighted.contains(&line))
    }

    fn start(&mut self, ctx: &egui::Context, targets: Vec<Target>) {
        let mut pending = VecDeque::from(targets);
        let Some(first) = pending.pop_front() else { return };
        self.message = None;
        self.outcome = Outcome::default();
        let total = pending.len() + 1;
        self.run = Some(Run { job: RenderJob::start(first.task, notifier(ctx)), for_telegram: first.for_telegram, pending, total });
    }

    /// Moves on to the next file, or ends the run, once a job has ended.
    fn poll(&mut self, ctx: &egui::Context) {
        let Some(run) = &mut self.run else { return };
        match run.job.status() {
            RenderStatus::Running(_) => return,
            RenderStatus::Done { path, bytes } => {
                self.outcome.saved.push(Saved { path, bytes, for_telegram: run.for_telegram });
                if let Some(next) = run.pending.pop_front() {
                    run.job = RenderJob::start(next.task, notifier(ctx));
                    run.for_telegram = next.for_telegram;
                    return;
                }
            }
            RenderStatus::Failed(error) => self.outcome.error = Some(error),
            RenderStatus::Cancelled => self.outcome.cancelled = true,
        }
        self.run = None;
    }

    /// Saves the output's picture at `time` as an image; the window shows how that goes.
    pub fn save_frame(&mut self, ctx: &egui::Context, sequence: &Sequence, source: &Path, info: &MediaInfo, time: f64) {
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save the frame")
            .add_filter("PNG", &["png"])
            .set_file_name(format!("{stem} - frame {time:.2}s.png"));
        if let Some(folder) = self.path.as_deref().and_then(Path::parent).or(source.parent()) {
            dialog = dialog.set_directory(folder);
        }
        if let Some(path) = dialog.save_file() {
            self.open = true;
            self.start(ctx, vec![Target { task: render::frame_task(source, info, sequence, time, &path), for_telegram: false }]);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, sequence: &mut Sequence, source: &Path, info: &MediaInfo, shown: &[&'static Preset]) {
        self.poll(ctx);
        if !self.open {
            return;
        }
        let encoders = self.encoders.get_or_insert_with(tools::encoders).clone();
        let running = self.run.is_some();
        let mut open = self.open;
        egui::Window::new("Export").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                self.presets(ui, sequence, &encoders);
                ui.separator();
                self.settings(ui, sequence, &encoders);
                ui.separator();
                self.destination(ui, sequence, source, shown);
            });
            for warning in render::warnings(sequence, info) {
                ui.colored_label(WARNING, warning);
            }
            ui.add_space(4.0);
            self.action(ui, sequence, source, info, shown);
        });
        self.open = open;
    }

    fn presets(&mut self, ui: &mut egui::Ui, sequence: &mut Sequence, encoders: &[String]) {
        let now = ui.input(|i| i.time);
        egui::Grid::new("export_presets").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            for group in PRESET_GROUPS {
                ui.label(RichText::new(group).weak());
                ui.horizontal_wrapped(|ui| {
                    ui.set_max_width(440.0);
                    for preset in EXPORT_PRESETS.iter().filter(|p| p.group == group) {
                        let applied = preset.is_applied(sequence.output, &sequence.settings);
                        let lines: Vec<&str> = preset.settings().into_iter().map(Setting::label).collect();
                        let hint = format!("Sets: {}", lines.join(", "));
                        let available = preset_is_available(preset, encoders);
                        let chip = ui.add_enabled(available, egui::SelectableLabel::new(applied, preset.name));
                        if chip.on_hover_text(hint).on_disabled_hover_text("This ffmpeg has no encoder for it").clicked() {
                            let lines = preset.apply(&mut sequence.output, &mut sequence.settings);
                            self.highlight(lines, now);
                        }
                    }
                });
                ui.end_row();
            }
        });
    }

    fn settings(&mut self, ui: &mut egui::Ui, sequence: &mut Sequence, encoders: &[String]) {
        let output = sequence.output;
        let settings = &mut sequence.settings;
        let container = settings.container;
        let has_video_encoder = |codec: VideoCodec| has(encoders, codec.encoder());
        let has_audio_encoder = |codec: AudioCodec| codec.encoder().is_none_or(|encoder| has(encoders, encoder));

        egui::Grid::new("export_settings").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            row(ui, self.started(Setting::Frame), Setting::Frame.label(), |ui| {
                let text = format!("{} · {}", output.preset().name, output.ratio().label());
                ui.label(text).on_hover_text("Choose it in the Frames menu above the picture");
            });
            row(ui, self.started(Setting::Format), Setting::Format.label(), |ui| {
                combo(ui, "container", &mut settings.container, &Container::ALL, Container::label, |_| true);
            });
            row(ui, self.started(Setting::VideoCodec), Setting::VideoCodec.label(), |ui| {
                combo(ui, "video_codec", &mut settings.video_codec, &VideoCodec::ALL, VideoCodec::label, |c| container.allows_video(c) && has_video_encoder(c));
            });
            row(ui, self.started(Setting::Profile), Setting::Profile.label(), |ui| {
                ui.add_enabled_ui(settings.video_codec == VideoCodec::H264, |ui| {
                    combo(ui, "profile", &mut settings.profile, &H264Profile::ALL, H264Profile::label, |_| true);
                });
            });
            row(ui, self.started(Setting::Speed), Setting::Speed.label(), |ui| {
                combo(ui, "speed", &mut settings.speed, &Speed::ALL, Speed::label, |_| true);
            });
            row(ui, self.started(Setting::Quality), Setting::Quality.label(), |ui| quality(ui, &mut settings.rate));
            row(ui, self.started(Setting::Size), Setting::Size.label(), |ui| {
                let [px, py] = settings.pixels(output);
                let mut from_frame = settings.size == OutputSize::FromFrame;
                combo(ui, "size", &mut from_frame, &[true, false], |frame| if frame { "From the frame" } else { "Long side" }, |_| true);
                settings.size = match (from_frame, settings.size) {
                    (true, _) => OutputSize::FromFrame,
                    (false, OutputSize::LongSide(long_side)) => OutputSize::LongSide(long_side),
                    (false, OutputSize::FromFrame) => OutputSize::LongSide(px.max(py)),
                };
                if let OutputSize::LongSide(long_side) = &mut settings.size {
                    ui.add(egui::DragValue::new(long_side).speed(4.0).range(16..=7680).suffix(" px"));
                }
                let [px, py] = settings.pixels(output);
                ui.label(RichText::new(format!("{px}×{py}")).weak());
            });
            row(ui, self.started(Setting::FrameRate), Setting::FrameRate.label(), |ui| {
                let mut source = settings.frame_rate == FrameRate::Source;
                combo(ui, "frame_rate", &mut source, &[true, false], |source| if source { "As source" } else { "Fixed" }, |_| true);
                settings.frame_rate = match (source, settings.frame_rate) {
                    (true, _) => FrameRate::Source,
                    (false, FrameRate::Fixed(fps)) => FrameRate::Fixed(fps),
                    (false, FrameRate::Source) => FrameRate::Fixed(30.0),
                };
                if let FrameRate::Fixed(fps) = &mut settings.frame_rate {
                    ui.add(egui::DragValue::new(fps).speed(0.1).range(1.0..=240.0).max_decimals(3).suffix(" fps"));
                }
            });
            row(ui, self.started(Setting::AudioCodec), Setting::AudioCodec.label(), |ui| {
                combo(ui, "audio_codec", &mut settings.audio_codec, &AudioCodec::ALL, AudioCodec::label, |c| container.allows_audio(c) && has_audio_encoder(c));
            });
            let with_sound = settings.audio_codec != AudioCodec::None;
            row(ui, self.started(Setting::AudioBitrate), Setting::AudioBitrate.label(), |ui| {
                ui.add_enabled(with_sound, egui::DragValue::new(&mut settings.audio_kbps).speed(1.0).range(32..=512).suffix(" kbit/s"));
            });
            row(ui, self.started(Setting::Fades), Setting::Fades.label(), |ui| {
                ui.add_enabled_ui(with_sound, |ui| {
                    let seconds = |value| egui::DragValue::new(value).speed(0.05).range(0.0..=30.0).max_decimals(2).suffix(" s");
                    ui.label("in");
                    ui.add(seconds(&mut settings.fades.0));
                    ui.label("out");
                    ui.add(seconds(&mut settings.fades.1));
                });
            });
        });

        // A new container may not hold the codecs: those change too, and show it.
        let normalized = settings.normalized();
        let forced = settings.differences(&normalized);
        if !forced.is_empty() {
            *settings = normalized;
            self.highlight(forced, ui.input(|i| i.time));
        }
    }

    /// Where the files go, the main one first.
    fn paths(&self, sequence: &Sequence, shown: &[&'static Preset]) -> Vec<PathBuf> {
        let Some(main) = &self.path else { return Vec::new() };
        let mut paths = vec![main.clone()];
        if self.with_shown {
            let main_frame = sequence.output.preset().name;
            paths.extend(shown.iter().map(|preset| variant_path(main, main_frame, preset.name)));
        }
        paths
    }

    fn destination(&mut self, ui: &mut egui::Ui, sequence: &Sequence, source: &Path, shown: &[&'static Preset]) {
        // The file follows the format.
        let extension = sequence.settings.container.extension();
        if let Some(path) = &mut self.path {
            if path.extension().is_none_or(|e| e != extension) {
                path.set_extension(extension);
            }
        }
        ui.horizontal(|ui| {
            if ui.button("File…").clicked() {
                self.choose_file(sequence, source);
            }
            match &self.path {
                Some(path) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    ui.label(name).on_hover_text(path.display().to_string());
                }
                None => {
                    ui.label(RichText::new("Not chosen yet").weak());
                }
            }
        });
        if !shown.is_empty() {
            let ratios: Vec<String> = shown.iter().map(|preset| sequence.output.ratio_of(preset).label()).collect();
            let hint = "Each one in its own file, with the same center and angle, the way it is drawn inside the main frame";
            ui.checkbox(&mut self.with_shown, format!("Also export the frames shown for comparison: {}", ratios.join(", "))).on_hover_text(hint);
        }
        let saved_just_now = |path: &PathBuf| self.outcome.saved.iter().any(|saved| &saved.path == path);
        let replaced = self.paths(sequence, shown).iter().filter(|path| path.exists() && !saved_just_now(path)).count();
        if replaced > 0 {
            let text = if replaced == 1 { "A file exists and will be replaced.".to_owned() } else { format!("{replaced} files exist and will be replaced.") };
            ui.colored_label(WARNING, text);
        }
    }

    fn choose_file(&mut self, sequence: &Sequence, source: &Path) {
        let suggested = self.path.clone().unwrap_or_else(|| suggested_path(source, sequence));
        let container = sequence.settings.container;
        let mut dialog = rfd::FileDialog::new().set_title("Export to").add_filter(container.label(), &[container.extension()]);
        if let Some(folder) = suggested.parent() {
            dialog = dialog.set_directory(folder);
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        if let Some(path) = dialog.save_file() {
            self.path = Some(path);
        }
    }

    fn start_export(&mut self, ctx: &egui::Context, sequence: &Sequence, source: &Path, info: &MediaInfo, shown: &[&'static Preset]) {
        let paths = self.paths(sequence, shown);
        if paths.iter().any(|path| path == source) {
            self.message = Some("The source video can't be replaced with its own export.".to_owned());
            return;
        }
        let circle = |sequence: &Sequence| sequence.output.outline().mask == Mask::Circle;
        let sequences = std::iter::once(sequence.clone()).chain(shown.iter().map(|preset| render::variant(sequence, info, preset)));
        let targets = paths
            .iter()
            .zip(sequences)
            .map(|(path, sequence)| Target { task: render::video_task(source, info, &sequence, path), for_telegram: circle(&sequence) })
            .collect();
        self.start(ctx, targets);
    }

    fn action(&mut self, ui: &mut egui::Ui, sequence: &Sequence, source: &Path, info: &MediaInfo, shown: &[&'static Preset]) {
        if let Some(progress) = self.progress() {
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(progress).desired_width(300.0).show_percentage());
                if ui.button("Cancel").clicked() {
                    if let Some(run) = &mut self.run {
                        run.pending.clear();
                        run.job.cancel();
                    }
                }
            });
            return;
        }

        let mut problem = None;
        for saved in &self.outcome.saved {
            let name = saved.path.file_name().unwrap_or_default().to_string_lossy();
            ui.horizontal(|ui| {
                ui.label(format!("Saved {name}, {:.1} MB", megabytes(saved.bytes)));
                if ui.button("Open").clicked() {
                    problem = tools::open_file(&saved.path).err();
                }
                if ui.button("Show in folder").clicked() {
                    problem = tools::reveal_file(&saved.path).err();
                }
            });
            if saved.for_telegram && saved.bytes > render::TELEGRAM_MAX_BYTES {
                ui.colored_label(WARNING, format!("Telegram's limit for a bot is {:.0} MB.", megabytes(render::TELEGRAM_MAX_BYTES)));
            }
        }
        if let Some(problem) = problem {
            self.message = Some(format!("{problem:#}"));
        }
        if let Some(error) = &self.outcome.error {
            ui.colored_label(ERROR, format!("The export failed:\n{error}"));
        }
        if self.outcome.cancelled {
            ui.label(RichText::new("Cancelled").weak());
        }
        if let Some(message) = &self.message {
            ui.colored_label(ERROR, message);
        }

        if ui.button(RichText::new("Export").strong()).clicked() {
            if self.path.is_none() {
                self.choose_file(sequence, source);
            }
            self.start_export(ui.ctx(), sequence, source, info, shown);
        }
    }
}

/// Constant quality or an average bitrate, each with its number.
fn quality(ui: &mut egui::Ui, rate: &mut Rate) {
    let mut constant = matches!(rate, Rate::Quality(_));
    combo(ui, "rate", &mut constant, &[true, false], |constant| if constant { "Constant quality" } else { "Bitrate" }, |_| true);
    *rate = match (constant, *rate) {
        (true, Rate::Bitrate(_)) => Rate::Quality(START_CRF),
        (false, Rate::Quality(_)) => Rate::Bitrate(START_KBPS),
        (_, rate) => rate,
    };
    match rate {
        Rate::Quality(crf) => {
            ui.add(egui::DragValue::new(crf).speed(0.2).range(0..=63).prefix("CRF ")).on_hover_text("Lower is better and takes a bigger file");
        }
        Rate::Bitrate(kbps) => {
            ui.add(egui::DragValue::new(kbps).speed(10.0).range(100..=200_000).suffix(" kbit/s"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Clip;
    use crate::core::preset::{get, PresetId};

    #[test]
    fn the_suggested_file_goes_next_to_the_source() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let path = suggested_path(Path::new("C:/videos/my clip.mkv"), &sequence);
        assert_eq!(path, Path::new("C:/videos/my clip - Telegram circle.mp4"));
        sequence.settings.container = Container::WebM;
        sequence.output.preset = PresetId::Vertical;
        let path = suggested_path(Path::new("clip.mp4"), &sequence);
        assert_eq!(path, Path::new("clip - Reels Shorts TikTok.webm"));
    }

    #[test]
    fn other_frames_go_next_to_the_main_file() {
        let named_after_frame = Path::new("a/clip - Telegram circle.mp4");
        assert_eq!(variant_path(named_after_frame, "Telegram circle", "Reels, Shorts, TikTok"), Path::new("a/clip - Reels Shorts TikTok.mp4"));
        let own_name = Path::new("a/final.mov");
        assert_eq!(variant_path(own_name, "Telegram circle", "Square"), Path::new("a/final - Square.mov"));
    }

    #[test]
    fn shown_frames_are_exported_only_when_asked() {
        let sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let shown = [get(PresetId::Square), get(PresetId::Landscape)];
        let mut window = ExportWindow::default();
        assert!(window.paths(&sequence, &shown).is_empty());

        window.path = Some(PathBuf::from("final.mp4"));
        assert_eq!(window.paths(&sequence, &shown), [PathBuf::from("final.mp4")]);

        window.with_shown = true;
        let expected = ["final.mp4", "final - Square.mp4", "final - Landscape.mp4"].map(PathBuf::from);
        assert_eq!(window.paths(&sequence, &shown), expected);
    }

    #[test]
    fn presets_need_their_encoders() {
        let webm = EXPORT_PRESETS.iter().find(|p| p.name == "WebM, VP9").unwrap();
        let size = EXPORT_PRESETS.iter().find(|p| p.name == "720p").unwrap();
        let only_x264 = ["libx264".to_owned(), "aac".to_owned()];
        assert!(!preset_is_available(webm, &only_x264));
        assert!(preset_is_available(size, &only_x264));
        assert!(preset_is_available(webm, &[]));
        let no_sound = EXPORT_PRESETS.iter().find(|p| p.name == "No sound").unwrap();
        assert!(preset_is_available(no_sound, &only_x264));
    }
}

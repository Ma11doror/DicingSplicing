//! The export tab: presets on top, the detailed settings under them. At the bottom, always in
//! sight, the render itself and what can be done with its result.

use crate::core::edit::Sequence;
use crate::core::export::{
    AudioCodec, Container, ExportPreset, FrameRate, H264Profile, OutputSize, Rate, Setting, Speed, VideoCodec, EXPORT_PRESETS, PRESET_GROUPS,
};
use crate::core::plugins::{Plugin, PluginKind};
use crate::core::preset::Preset;
use crate::core::settings::Settings;
use crate::core::tools;
use crate::core::workspace::folder_name;
use crate::media::geometry::Mask;
use crate::media::probe::MediaInfo;
use crate::media::render::{self, RenderJob, RenderStatus, RenderTask};
use crate::media::Notify;
use crate::ui::flash;
use crate::ui::plugins::{Offer, PublishEnv, PublishSection};
use egui::{Color32, NumExt, RichText};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const WARNING: Color32 = Color32::from_rgb(230, 170, 70);
const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
const COMBO_WIDTH: f32 = 140.0;
const LABEL_COLUMN: f32 = 76.0;
const COLUMN_GAP: f32 = 12.0;
const CANCEL_WIDTH: f32 = 70.0;
/// The settings keep this much however large what is under them gets.
const MIN_SETTINGS_HEIGHT: f32 = 120.0;
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
    /// What a video is made from. A frame has none: nothing else depends on it.
    made_from: Option<Sequence>,
    /// The videos of this run written so far.
    saved: Vec<Saved>,
}

#[derive(Clone)]
struct Saved {
    path: PathBuf,
    bytes: u64,
    for_telegram: bool,
}

/// The video that is ready, and what it shows.
struct Exported {
    files: Vec<Saved>,
    sequence: Sequence,
    /// Tells this export from the ones before: the same file can be written again.
    version: u64,
}

/// How the last run went, apart from the files it wrote.
#[derive(Default)]
struct Outcome {
    error: Option<String>,
    cancelled: bool,
}

/// What the tab needs from the editor.
pub struct ExportEnv<'a> {
    pub source: &'a Path,
    pub info: &'a MediaInfo,
    pub shown: &'a [&'static Preset],
    /// The playhead, for saving a frame.
    pub position: f64,
    pub plugins: &'a [Plugin],
    /// Nothing can be sent without a workspace, where plugins keep their notes.
    pub publish: Option<PublishEnv<'a>>,
}

#[derive(Default)]
pub struct ExportPanel {
    project_name: String,
    /// The project's folder for results.
    renders: PathBuf,
    /// The file to export to, if the user chose one. Otherwise it goes to `renders`.
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
    exported: Option<Exported>,
    /// How many videos have been written so far.
    exports: u64,
    /// The picture saved last.
    frame: Option<Saved>,
    /// The menus of the plugins that send files, by plugin.
    sections: BTreeMap<String, PublishSection>,
    /// How tall the block under the settings was last time.
    actions_height: f32,
}

/// One line of the settings, flashing if `started` says so.
fn row(ui: &mut egui::Ui, width: f32, started: Option<f64>, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.label(label);
    flash::frame(ui, started, false).show(ui, |ui| {
        ui.set_max_width(width);
        ui.horizontal_wrapped(contents);
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

/// What a plugin can send right now, or why nothing.
fn offer(plugin: &Plugin, running: bool, exported: Option<&Exported>, current: &Sequence) -> Offer {
    if running {
        return Offer::Nothing("Exporting…".to_owned());
    }
    let Some(exported) = exported else {
        return Offer::Nothing("Export the video first".to_owned());
    };
    if exported.sequence != *current {
        return Offer::Nothing("Export again".to_owned());
    }
    let files: Vec<PathBuf> = exported.files.iter().map(|saved| saved.path.clone()).filter(|path| plugin.can_publish(path)).collect();
    if files.is_empty() {
        return Offer::Nothing(format!("Only for {}", plugin.extensions.join(", ")));
    }
    Offer::Files { files, version: exported.version }
}

/// A written file, and how to get to it.
fn saved_line(ui: &mut egui::Ui, saved: &Saved, problem: &mut Option<anyhow::Error>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Show in folder").clicked() {
                *problem = tools::reveal_file(&saved.path).err();
            }
            if ui.button("Open").clicked() {
                *problem = tools::open_file(&saved.path).err();
            }
            ui.label(RichText::new(format!("{:.1} MB", megabytes(saved.bytes))).weak());
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let name = saved.path.file_name().unwrap_or_default().to_string_lossy();
                ui.add(egui::Label::new(format!("Saved {name}")).truncate()).on_hover_text(saved.path.display().to_string());
            });
        });
    });
}

impl ExportPanel {
    pub fn for_project(name: &str, renders: PathBuf, path: Option<PathBuf>, with_shown: bool) -> Self {
        Self { project_name: name.to_owned(), renders, path, with_shown, ..Self::default() }
    }

    /// What a project keeps of this: the file, if the user chose one.
    pub fn saved_path(&self) -> Option<PathBuf> {
        self.path.clone()
    }

    pub fn with_shown(&self) -> bool {
        self.with_shown
    }

    /// Where the main frame goes: the chosen file, or one named after the project and the frame.
    fn main_path(&self, sequence: &Sequence) -> PathBuf {
        self.path.clone().unwrap_or_else(|| {
            let frame = file_label(sequence.output.preset().name);
            self.renders.join(format!("{} - {frame}.{}", folder_name(&self.project_name), sequence.settings.container.extension()))
        })
    }

    /// How far the whole run has come, from 0 to 1.
    fn progress(&self) -> Option<f32> {
        let run = self.run.as_ref()?;
        let current = match run.job.status() {
            RenderStatus::Running(progress) => progress,
            _ => 1.0,
        };
        Some((run.saved.len() as f32 + current) / run.total as f32)
    }

    /// What the tab is called.
    pub fn tab_text(&self) -> String {
        match self.progress() {
            Some(progress) => format!("Export {:.0}%", progress * 100.0),
            None => "Export".to_owned(),
        }
    }

    fn highlight(&mut self, lines: Vec<Setting>, now: f64) {
        self.highlighted = lines;
        self.highlighted_at = Some(now);
    }

    fn started(&self, line: Setting) -> Option<f64> {
        self.highlighted_at.filter(|_| self.highlighted.contains(&line))
    }

    fn start(&mut self, ctx: &egui::Context, targets: Vec<Target>, made_from: Option<Sequence>) {
        let mut pending = VecDeque::from(targets);
        let Some(first) = pending.pop_front() else { return };
        self.message = None;
        self.outcome = Outcome::default();
        self.frame = None;
        let total = pending.len() + 1;
        self.run = Some(Run { job: RenderJob::start(first.task, notifier(ctx)), for_telegram: first.for_telegram, pending, total, made_from, saved: Vec::new() });
    }

    /// From now on the video is these files: the ones it had before may have just been replaced.
    fn video_saved(&mut self, files: Vec<Saved>, sequence: &Sequence) {
        self.exports += 1;
        self.exported = Some(Exported { files, sequence: sequence.clone(), version: self.exports });
    }

    /// Moves on to the next file, or ends the run, once a job has ended.
    fn poll_run(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self.run.take() else { return };
        match run.job.status() {
            RenderStatus::Running(_) => self.run = Some(run),
            RenderStatus::Done { path, bytes } => {
                let saved = Saved { path, bytes, for_telegram: run.for_telegram };
                match &run.made_from {
                    Some(sequence) => {
                        run.saved.push(saved);
                        self.video_saved(run.saved.clone(), sequence);
                    }
                    None => self.frame = Some(saved),
                }
                if let Some(next) = run.pending.pop_front() {
                    run.job = RenderJob::start(next.task, notifier(ctx));
                    run.for_telegram = next.for_telegram;
                    self.run = Some(run);
                }
            }
            RenderStatus::Failed(error) => self.outcome.error = Some(error),
            RenderStatus::Cancelled => self.outcome.cancelled = true,
        }
    }

    /// Called whatever the panel shows: what is running goes on while the other tab is open.
    pub fn poll(&mut self, ctx: &egui::Context, settings: &Settings) {
        self.poll_run(ctx);
        for section in self.sections.values_mut() {
            section.poll(ctx, settings);
        }
    }

    /// Saves the output's picture at `time` as an image.
    fn save_frame(&mut self, ctx: &egui::Context, sequence: &Sequence, source: &Path, info: &MediaInfo, time: f64) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save the frame")
            .add_filter("PNG", &["png"])
            .set_file_name(format!("{} - frame {time:.2}s.png", folder_name(&self.project_name)));
        let _ = fs::create_dir_all(&self.renders);
        if let Some(folder) = self.path.as_deref().and_then(Path::parent).or(Some(&self.renders)) {
            dialog = dialog.set_directory(folder);
        }
        if let Some(path) = dialog.save_file() {
            self.start(ctx, vec![Target { task: render::frame_task(source, info, sequence, time, &path), for_telegram: false }], None);
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, sequence: &mut Sequence, env: &ExportEnv) {
        let encoders = self.encoders.get_or_insert_with(tools::encoders).clone();
        let running = self.run.is_some();

        // The settings take what the block under them leaves.
        let total = ui.available_height();
        let most_for_actions = (total - MIN_SETTINGS_HEIGHT).at_least(MIN_SETTINGS_HEIGHT);
        let settings_height = (total - self.actions_height.at_most(most_for_actions)).at_least(MIN_SETTINGS_HEIGHT);
        let settings = egui::ScrollArea::vertical().id_salt("export_settings").auto_shrink([false, false]).max_height(settings_height).show(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                self.presets(ui, sequence, &encoders);
                ui.separator();
                self.settings(ui, sequence, &encoders);
            });
        });
        ui.separator();
        let actions = egui::ScrollArea::vertical().id_salt("export_actions").auto_shrink([false, true]).max_height(most_for_actions).show(ui, |ui| self.actions(ui, sequence, env));
        // What it needs, not what it got: it can't get more than the settings leave.
        let height = actions.inner_rect.top() - settings.inner_rect.bottom() + actions.content_size.y;
        if (height - self.actions_height).abs() > 0.5 {
            self.actions_height = height;
            ui.ctx().request_repaint();
        }
    }

    fn actions(&mut self, ui: &mut egui::Ui, sequence: &Sequence, env: &ExportEnv) {
        let running = self.run.is_some();
        ui.add_enabled_ui(!running, |ui| self.destination(ui, sequence, env.shown));
        for warning in render::warnings(sequence, env.info) {
            ui.colored_label(WARNING, warning);
        }
        ui.add_space(4.0);
        self.controls(ui, sequence, env);
        if !running {
            self.results(ui, sequence);
        }
        self.publishing(ui, sequence, env);
    }

    fn presets(&mut self, ui: &mut egui::Ui, sequence: &mut Sequence, encoders: &[String]) {
        let now = ui.input(|i| i.time);
        // Not a grid: the chips of a group can take several lines, and a grid row wouldn't grow with them.
        for group in PRESET_GROUPS {
            ui.horizontal_top(|ui| {
                let label = egui::vec2(LABEL_COLUMN, ui.spacing().interact_size.y);
                ui.allocate_ui_with_layout(label, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(LABEL_COLUMN);
                    ui.label(RichText::new(group).weak());
                });
                ui.add_space(COLUMN_GAP - ui.spacing().item_spacing.x);
                ui.horizontal_wrapped(|ui| {
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
            });
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui, sequence: &mut Sequence, encoders: &[String]) {
        let output = sequence.output;
        let settings = &mut sequence.settings;
        let container = settings.container;
        let has_video_encoder = |codec: VideoCodec| has(encoders, codec.encoder());
        let has_audio_encoder = |codec: AudioCodec| codec.encoder().is_none_or(|encoder| has(encoders, encoder));

        // The frame around a line takes a little.
        let width = (ui.available_width() - LABEL_COLUMN - COLUMN_GAP - 8.0).at_least(COMBO_WIDTH);
        egui::Grid::new("export_settings").num_columns(2).spacing([COLUMN_GAP, 4.0]).min_col_width(LABEL_COLUMN).show(ui, |ui| {
            row(ui, width, self.started(Setting::Frame), Setting::Frame.label(), |ui| {
                let text = format!("{} · {}", output.preset().name, output.ratio().label());
                ui.label(text).on_hover_text("Choose it in the Frames menu above the picture");
            });
            row(ui, width, self.started(Setting::Format), Setting::Format.label(), |ui| {
                combo(ui, "container", &mut settings.container, &Container::ALL, Container::label, |_| true);
            });
            row(ui, width, self.started(Setting::VideoCodec), Setting::VideoCodec.label(), |ui| {
                combo(ui, "video_codec", &mut settings.video_codec, &VideoCodec::ALL, VideoCodec::label, |c| container.allows_video(c) && has_video_encoder(c));
            });
            row(ui, width, self.started(Setting::Profile), Setting::Profile.label(), |ui| {
                ui.add_enabled_ui(settings.video_codec == VideoCodec::H264, |ui| {
                    combo(ui, "profile", &mut settings.profile, &H264Profile::ALL, H264Profile::label, |_| true);
                });
            });
            row(ui, width, self.started(Setting::Speed), Setting::Speed.label(), |ui| {
                combo(ui, "speed", &mut settings.speed, &Speed::ALL, Speed::label, |_| true);
            });
            row(ui, width, self.started(Setting::Quality), Setting::Quality.label(), |ui| quality(ui, &mut settings.rate));
            row(ui, width, self.started(Setting::Size), Setting::Size.label(), |ui| {
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
            row(ui, width, self.started(Setting::FrameRate), Setting::FrameRate.label(), |ui| {
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
            row(ui, width, self.started(Setting::AudioCodec), Setting::AudioCodec.label(), |ui| {
                combo(ui, "audio_codec", &mut settings.audio_codec, &AudioCodec::ALL, AudioCodec::label, |c| container.allows_audio(c) && has_audio_encoder(c));
            });
            let with_sound = settings.audio_codec != AudioCodec::None;
            row(ui, width, self.started(Setting::AudioBitrate), Setting::AudioBitrate.label(), |ui| {
                ui.add_enabled(with_sound, egui::DragValue::new(&mut settings.audio_kbps).speed(1.0).range(32..=512).suffix(" kbit/s"));
            });
            row(ui, width, self.started(Setting::Fades), Setting::Fades.label(), |ui| {
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
        let main = self.main_path(sequence);
        let mut paths = vec![main.clone()];
        if self.with_shown {
            let main_frame = sequence.output.preset().name;
            paths.extend(shown.iter().map(|preset| variant_path(&main, main_frame, preset.name)));
        }
        paths
    }

    fn destination(&mut self, ui: &mut egui::Ui, sequence: &Sequence, shown: &[&'static Preset]) {
        // The file follows the format.
        let extension = sequence.settings.container.extension();
        if let Some(path) = &mut self.path {
            if path.extension().is_none_or(|e| e != extension) {
                path.set_extension(extension);
            }
        }
        ui.horizontal(|ui| {
            if ui.button("File…").clicked() {
                self.choose_file(sequence);
            }
            if self.path.is_some() {
                let default = ui.button("Project folder").on_hover_text("Back to the file in the project's own folder");
                if default.clicked() {
                    self.path = None;
                }
            }
            let path = self.main_path(sequence);
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            ui.add(egui::Label::new(name).truncate()).on_hover_text(path.display().to_string());
        });
        if !shown.is_empty() {
            let ratios: Vec<String> = shown.iter().map(|preset| sequence.output.ratio_of(preset).label()).collect();
            let hint = "Each one in its own file, with the same center and angle, the way it is drawn inside the main frame";
            ui.checkbox(&mut self.with_shown, format!("Also export the frames shown for comparison: {}", ratios.join(", "))).on_hover_text(hint);
        }
        let saved_just_now = |path: &PathBuf| self.exported.iter().flat_map(|exported| &exported.files).any(|saved| &saved.path == path);
        let replaced = self.paths(sequence, shown).iter().filter(|path| path.exists() && !saved_just_now(path)).count();
        if replaced > 0 {
            let text = if replaced == 1 { "A file exists and will be replaced.".to_owned() } else { format!("{replaced} files exist and will be replaced.") };
            ui.colored_label(WARNING, text);
        }
    }

    fn choose_file(&mut self, sequence: &Sequence) {
        let suggested = self.main_path(sequence);
        let container = sequence.settings.container;
        let mut dialog = rfd::FileDialog::new().set_title("Export to").add_filter(container.label(), &[container.extension()]);
        if let Some(folder) = suggested.parent() {
            let _ = fs::create_dir_all(folder);
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
        if let Some(Err(error)) = paths.first().and_then(|path| path.parent()).map(fs::create_dir_all) {
            self.message = Some(format!("Can't create the folder for the file: {error}"));
            return;
        }
        let circle = |sequence: &Sequence| sequence.output.outline().mask == Mask::Circle;
        let sequences = std::iter::once(sequence.clone()).chain(shown.iter().map(|preset| render::variant(sequence, info, preset)));
        let targets = paths
            .iter()
            .zip(sequences)
            .map(|(path, sequence)| Target { task: render::video_task(source, info, &sequence, path), for_telegram: circle(&sequence) })
            .collect();
        self.start(ctx, targets, Some(sequence.clone()));
    }

    /// The button that writes the file, or how far that has come.
    fn controls(&mut self, ui: &mut egui::Ui, sequence: &Sequence, env: &ExportEnv) {
        if let Some(progress) = self.progress() {
            ui.horizontal(|ui| {
                let width = ui.available_width() - CANCEL_WIDTH;
                ui.add(egui::ProgressBar::new(progress).desired_width(width).show_percentage());
                if ui.button("Cancel").clicked() {
                    if let Some(run) = &mut self.run {
                        run.pending.clear();
                        run.job.cancel();
                    }
                }
            });
            return;
        }
        ui.horizontal(|ui| {
            let sending = self.sections.values().any(PublishSection::is_sending);
            let export = ui.add_enabled(!sending, egui::Button::new(RichText::new("Export").strong()));
            if export.on_disabled_hover_text("A video is going out, and this would replace it").clicked() {
                self.start_export(ui.ctx(), sequence, env.source, env.info, env.shown);
            }
            let hint = "Save the picture at the playhead as a PNG, in the output's size";
            let frame = ui.add_enabled(sequence.clip_at(env.position).is_some(), egui::Button::new("Save frame…"));
            if frame.on_hover_text(hint).clicked() {
                self.save_frame(ui.ctx(), sequence, env.source, env.info, env.position);
            }
        });
    }

    /// What the last runs left.
    fn results(&mut self, ui: &mut egui::Ui, sequence: &Sequence) {
        let mut problem = None;
        if let Some(exported) = &self.exported {
            for saved in &exported.files {
                saved_line(ui, saved, &mut problem);
                if saved.for_telegram && saved.bytes > render::TELEGRAM_MAX_BYTES {
                    ui.colored_label(WARNING, format!("Telegram's limit for a bot is {:.0} MB.", megabytes(render::TELEGRAM_MAX_BYTES)));
                }
            }
            if exported.sequence != *sequence {
                ui.colored_label(WARNING, "Changed since this export.");
            }
        }
        if let Some(saved) = &self.frame {
            saved_line(ui, saved, &mut problem);
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
    }

    /// One menu for each plugin that sends files somewhere.
    fn publishing(&mut self, ui: &mut egui::Ui, sequence: &Sequence, env: &ExportEnv) {
        let Some(publish) = &env.publish else { return };
        for plugin in env.plugins.iter().filter(|plugin| plugin.kind == PluginKind::Publish) {
            let offer = offer(plugin, self.run.is_some(), self.exported.as_ref(), sequence);
            let section = self.sections.entry(plugin.id.clone()).or_insert_with(|| PublishSection::new(plugin.clone()));
            section.show(ui, &offer, publish);
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

    fn window() -> ExportPanel {
        ExportPanel::for_project("my clip: 1", PathBuf::from("p/renders"), None, false)
    }

    fn plugin(extensions: &[&str]) -> Plugin {
        Plugin {
            id: "p".to_owned(),
            program: PathBuf::from("p"),
            kind: PluginKind::Publish,
            name: "P".to_owned(),
            prompt: String::new(),
            hint: String::new(),
            options: Vec::new(),
            extensions: extensions.iter().map(|&extension| extension.to_owned()).collect(),
        }
    }

    fn saved(path: &str) -> Saved {
        Saved { path: PathBuf::from(path), bytes: 1, for_telegram: false }
    }

    fn nothing(reason: &str) -> Offer {
        Offer::Nothing(reason.to_owned())
    }

    #[test]
    fn nothing_can_be_sent_until_the_video_is_ready() {
        let sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let mut panel = window();
        assert_eq!(offer(&plugin(&["mp4"]), false, panel.exported.as_ref(), &sequence), nothing("Export the video first"));

        panel.video_saved(vec![saved("a.mp4")], &sequence);
        let ready = offer(&plugin(&["mp4"]), false, panel.exported.as_ref(), &sequence);
        assert_eq!(ready, Offer::Files { files: vec![PathBuf::from("a.mp4")], version: 1 });

        // While the file is being replaced it can't go anywhere.
        assert_eq!(offer(&plugin(&["mp4"]), true, panel.exported.as_ref(), &sequence), nothing("Exporting…"));
    }

    #[test]
    fn a_change_after_the_export_stops_it_until_it_is_undone() {
        let sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let mut panel = window();
        panel.video_saved(vec![saved("a.mp4")], &sequence);

        let mut changed = sequence.clone();
        changed.settings.container = Container::WebM;
        assert_eq!(offer(&plugin(&["mp4"]), false, panel.exported.as_ref(), &changed), nothing("Export again"));

        changed.settings.container = sequence.settings.container;
        assert!(matches!(offer(&plugin(&["mp4"]), false, panel.exported.as_ref(), &changed), Offer::Files { .. }));
    }

    #[test]
    fn a_plugin_gets_only_the_files_it_takes_the_main_one_first() {
        let sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let mut panel = window();
        panel.video_saved(vec![saved("a.webm"), saved("b.mp4"), saved("c.MP4")], &sequence);
        let files = vec![PathBuf::from("b.mp4"), PathBuf::from("c.MP4")];
        assert_eq!(offer(&plugin(&["mp4"]), false, panel.exported.as_ref(), &sequence), Offer::Files { files, version: 1 });

        panel.video_saved(vec![saved("a.webm")], &sequence);
        assert_eq!(offer(&plugin(&["mp4", "mov"]), false, panel.exported.as_ref(), &sequence), nothing("Only for mp4, mov"));
        assert!(matches!(offer(&plugin(&[]), false, panel.exported.as_ref(), &sequence), Offer::Files { .. }));
    }

    #[test]
    fn writing_the_same_file_again_is_another_export() {
        let sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let mut panel = window();
        panel.video_saved(vec![saved("a.mp4")], &sequence);
        panel.video_saved(vec![saved("a.mp4")], &sequence);
        assert_eq!(panel.exported.as_ref().map(|exported| exported.version), Some(2));
    }

    #[test]
    fn a_run_that_writes_nothing_leaves_the_video_ready() {
        let sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let info = MediaInfo { duration: 1.0, width: 16, height: 16, fps: crate::media::probe::Rational { num: 25, den: 1 }, has_audio: false };
        let output = std::env::temp_dir().join(format!("simple_editor_export_{}.png", std::process::id()));
        let task = render::frame_task(Path::new("there is no such video.mp4"), &info, &sequence, 0.0, &output);
        let ctx = egui::Context::default();
        let mut panel = window();
        panel.video_saved(vec![saved("a.mp4")], &sequence);

        // Saving a frame is not an export, and this one fails.
        panel.start(&ctx, vec![Target { task, for_telegram: false }], None);
        let started = std::time::Instant::now();
        while panel.run.is_some() {
            assert!(started.elapsed().as_secs() < 30, "the job does not end");
            std::thread::sleep(std::time::Duration::from_millis(10));
            panel.poll(&ctx, &Settings::default());
        }
        assert!(panel.outcome.error.is_some());
        assert!(panel.frame.is_none());
        assert_eq!(panel.exported.as_ref().map(|exported| exported.files.len()), Some(1));
        assert!(matches!(offer(&plugin(&["mp4"]), false, panel.exported.as_ref(), &sequence), Offer::Files { .. }));
    }

    #[test]
    fn the_file_goes_to_the_project_folder_unless_told_otherwise() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 1.0));
        let mut window = window();
        assert_eq!(window.main_path(&sequence), Path::new("p/renders/my clip 1 - Telegram circle.mp4"));
        assert_eq!(window.saved_path(), None);

        // It follows the frame and the format.
        sequence.settings.container = Container::WebM;
        sequence.output.preset = PresetId::Vertical;
        assert_eq!(window.main_path(&sequence), Path::new("p/renders/my clip 1 - Reels Shorts TikTok.webm"));

        window.path = Some(PathBuf::from("d/final.mp4"));
        assert_eq!(window.main_path(&sequence), Path::new("d/final.mp4"));
        assert_eq!(window.saved_path(), Some(PathBuf::from("d/final.mp4")));
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
        let mut window = window();
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

//! The editor: source view, output preview with transform controls, timeline and transport.

use crate::core::edit::{Clip, ClipId, Sequence, Span};
use crate::core::history::History;
use crate::core::plugins::Plugin;
use crate::core::project::{self, Project, Source, ViewState, RENDERS_FOLDER};
use crate::core::settings::Settings;
use crate::core::workspace::Workspace;
use crate::media::audio::{PlayRange, Segment};
use crate::media::cache::MediaCache;
use crate::media::frames::FrameDecoder;
use crate::media::geometry::{self, Transform};
use crate::media::player::Player;
use crate::media::{timecode, Notify};
use crate::ui::export::{ExportEnv, ExportPanel};
use crate::ui::framing::Framing;
use crate::ui::plugins::PublishEnv;
use crate::ui::timeline::Timeline;
use crate::ui::viewer::{OutputView, SourceView};
use egui::{Color32, Key, Modifiers, RichText, TextureOptions};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The output preview's largest height: a tall frame should not push the controls out of the panel.
const OUTPUT_MAX_HEIGHT: f64 = 340.0;
const SIDE_PANEL_WIDTH: f32 = 380.0;
/// The export settings need this much to show without cutting anything.
const SIDE_PANEL_MIN_WIDTH: f32 = 340.0;
const TAB_TEXT_SIZE: f32 = 17.0;
/// How long after a change the project is saved, so that a series of changes is written once.
const AUTOSAVE_SECONDS: f64 = 0.5;
/// How much a source's length may differ from what the project remembers: rounding, not a new file.
const SAME_DURATION: f64 = 1e-3;

#[derive(Clone, Copy, PartialEq)]
enum PlayMode {
    /// From the playhead to the end of the last clip.
    Timeline,
    /// The export range, with the export fades, looped if `loop_export` is on.
    Export,
}

/// What the side panel shows: setting up the result, then writing it.
#[derive(Clone, Copy, Default, PartialEq)]
enum SideTab {
    #[default]
    Output,
    Export,
}

pub struct Editor {
    // Dropped first: stops a cache build that is still running.
    pub cache: MediaCache,
    pub name: String,
    dir: PathBuf,
    sources: Vec<Source>,
    /// The project as it is on the disk.
    saved: Project,
    /// Since when there are changes that are not on the disk.
    changed_since: Option<f64>,
    pub save_error: Option<String>,
    decoder: FrameDecoder,
    player: Player,
    texture: Option<egui::TextureHandle>,
    requested_frame: Option<usize>,
    /// The playhead, in timeline seconds.
    position: f64,
    /// While an edge is dragged on the timeline: the time the previews show instead of the playhead.
    peek: Option<f64>,
    playing: bool,
    play_mode: PlayMode,
    loop_export: bool,
    sequence: Sequence,
    /// The clip whose transform is edited. There is only one clip for now.
    selected: ClipId,
    history: History<Sequence>,
    framing: Framing,
    side_tab: SideTab,
    export: ExportPanel,
    timeline: Timeline,
    source_view: SourceView,
    output_view: OutputView,
}

impl Editor {
    /// Opens the project in the folder `dir`, with the cache of its video.
    pub fn new(cache: MediaCache, project: Project, dir: PathBuf, ctx: &egui::Context) -> Self {
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let decoder = FrameDecoder::new(cache.frames_path(), cache.frames.clone(), notify);
        let player = Player::new(cache.audio.clone());

        let saved = project.clone();
        let Project { name, sources, mut sequence, view, .. } = project;
        for track in &mut sequence.tracks {
            for clip in &mut track.clips {
                if (clip.source_duration - cache.info.duration).abs() > SAME_DURATION {
                    clip.refit_source(cache.info.duration);
                }
            }
        }
        let export = ExportPanel::for_project(&name, dir.join(RENDERS_FOLDER), view.export_path, view.export_shown_frames);
        Self {
            cache,
            name,
            dir,
            sources,
            saved,
            changed_since: None,
            save_error: None,
            decoder,
            player,
            texture: None,
            requested_frame: None,
            position: view.playhead,
            peek: None,
            playing: false,
            play_mode: PlayMode::Timeline,
            loop_export: view.loop_export,
            history: History::new(sequence.clone()),
            sequence,
            selected: ClipId { track: 0, index: 0 },
            framing: Framing::restored(&view.frames, view.ratio_locked),
            side_tab: SideTab::default(),
            export,
            timeline: Timeline::new(view.timeline_scale),
            source_view: SourceView::default(),
            output_view: OutputView,
        }
    }

    /// The project as it is now.
    fn snapshot(&self) -> Project {
        Project {
            version: project::VERSION,
            name: self.name.clone(),
            sources: self.sources.clone(),
            sequence: self.sequence.clone(),
            view: ViewState {
                playhead: self.position,
                timeline_scale: self.timeline.scale(),
                frames: self.framing.saved_frames(),
                ratio_locked: self.framing.ratio_locked(),
                export_path: self.export.saved_path(),
                export_shown_frames: self.export.with_shown(),
                loop_export: self.loop_export,
            },
        }
    }

    fn save(&mut self, project: Project) {
        match project.save(&self.dir) {
            Ok(()) => {
                self.saved = project;
                self.changed_since = None;
                self.save_error = None;
            }
            Err(error) => self.save_error = Some(format!("The project can't be saved: {error:#}")),
        }
    }

    /// Saves a while after a change, once no gesture is going on.
    fn autosave(&mut self, ctx: &egui::Context, now: f64, busy: bool) {
        let mut current = self.snapshot();
        if self.playing {
            // Not every frame just because the playhead moves.
            current.view.playhead = self.saved.view.playhead;
        }
        if current == self.saved {
            self.changed_since = None;
            return;
        }
        let since = *self.changed_since.get_or_insert(now);
        if busy || now - since < AUTOSAVE_SECONDS {
            ctx.request_repaint_after(Duration::from_secs_f64(AUTOSAVE_SECONDS / 2.0));
            return;
        }
        self.save(current);
        if self.save_error.is_some() {
            // Try again in a while: the disk may be back by then.
            self.changed_since = Some(now);
            ctx.request_repaint_after(Duration::from_secs_f64(AUTOSAVE_SECONDS));
        }
    }

    /// Saves what is not saved yet, before the project closes.
    pub fn flush(&mut self) {
        let current = self.snapshot();
        if current != self.saved {
            self.save(current);
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, plugins: &[Plugin], workspace: Option<&Workspace>, settings: &Settings) {
        if self.playing {
            let (t, still_playing) = self.player.poll();
            self.position = t;
            self.playing = still_playing;
        }
        if let Some(image) = self.decoder.poll() {
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("frame", image, TextureOptions::LINEAR)),
            }
        }

        self.handle_keys(ctx);
        self.export.poll(ctx, settings);
        self.timeline_panel(ctx);
        self.side_panel(ctx, plugins, workspace, settings);
        self.source_panel(ctx);

        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input();
        let now = ctx.input(|i| i.time);
        if self.history.track(&self.sequence, now, busy) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.autosave(ctx, now, busy);

        if self.playing {
            self.player.set_range(self.play_range(self.play_mode));
            ctx.request_repaint();
        }
        let frame = self.frame_at(self.clip().source_time(self.shown_time()));
        if self.requested_frame != Some(frame) {
            self.decoder.request(frame);
            self.requested_frame = Some(frame);
        }
    }

    fn fps(&self) -> f64 {
        self.cache.format.fps.as_f64()
    }

    fn frame(&self) -> f64 {
        1.0 / self.fps()
    }

    /// Start of the frame shown at `t`: where I, O and the trim keys put an edge.
    fn frame_start(&self, t: f64) -> f64 {
        timecode::frame_index(t, self.fps()) as f64 / self.fps()
    }

    fn clip(&self) -> &Clip {
        self.sequence.clip(self.selected)
    }

    /// What the previews show: the playhead, or the edge being dragged.
    fn shown_time(&self) -> f64 {
        self.peek.unwrap_or(self.position)
    }

    fn content(&self) -> Span {
        self.sequence.content().unwrap_or(Span { start: 0.0, end: 0.0 })
    }

    /// What plain playback goes through: the clips and the black around them in the export range.
    fn play_span(&self) -> Span {
        self.content().union(self.sequence.export)
    }

    fn frame_size(&self) -> [f64; 2] {
        [self.cache.info.width as f64, self.cache.info.height as f64]
    }

    /// The cached frame at source time `t`.
    fn frame_at(&self, t: f64) -> usize {
        let last = self.cache.frame_count() as i64 - 1;
        timecode::frame_index(t, self.fps()).clamp(0, last) as usize
    }

    fn play_range(&self, mode: PlayMode) -> PlayRange {
        let clip = self.clip();
        let sound = Segment { start: clip.start, end: clip.end(), source_start: clip.source_in, gain: clip.gain() as f32 };
        match mode {
            PlayMode::Timeline => {
                let span = self.play_span();
                PlayRange { start: span.start, end: span.end, looping: false, fades: None, sound }
            }
            PlayMode::Export => PlayRange {
                start: self.sequence.export.start,
                end: self.sequence.export.end,
                looping: self.loop_export,
                fades: Some(self.sequence.settings.fades),
                sound,
            },
        }
    }

    fn play(&mut self) {
        let mode = if self.loop_export { PlayMode::Export } else { PlayMode::Timeline };
        let span = self.play_span();
        match mode {
            PlayMode::Export if !self.sequence.export.contains(self.position) => self.position = self.sequence.export.start,
            PlayMode::Timeline if self.position >= span.end - self.frame() => self.position = span.start,
            _ => {}
        }
        self.start(mode);
    }

    fn play_export(&mut self) {
        self.position = self.sequence.export.start;
        self.start(PlayMode::Export);
    }

    fn start(&mut self, mode: PlayMode) {
        self.play_mode = mode;
        self.player.play(self.position, self.play_range(mode));
        self.playing = true;
    }

    fn pause(&mut self) {
        self.player.pause();
        self.playing = false;
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    fn seek(&mut self, t: f64) {
        self.pause();
        self.position = t;
    }

    fn step(&mut self, frames: i64) {
        let frame = timecode::frame_index(self.position, self.fps()) + frames;
        self.seek(frame as f64 / self.fps());
    }

    fn set_export_start(&mut self, t: f64) {
        let frame = self.frame();
        self.sequence.export.set_start(t, frame);
    }

    fn set_export_end(&mut self, t: f64) {
        let frame = self.frame();
        self.sequence.export.set_end(t, frame);
    }

    fn trim_start(&mut self, t: f64) {
        let frame = self.frame();
        self.sequence.trim_start(self.selected, t, frame);
    }

    fn trim_end(&mut self, t: f64) {
        let frame = self.frame();
        self.sequence.trim_end(self.selected, t, frame);
    }

    fn undo(&mut self) {
        if self.history.undo(&mut self.sequence) {
            self.after_history_step();
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.sequence) {
            self.after_history_step();
        }
    }

    /// What is playing no longer matches the edit.
    fn after_history_step(&mut self) {
        self.pause();
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        // Consumed here, so that a focused button does not also react to Space or the arrows.
        let pressed = |modifiers: Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(modifiers, key));
        let (none, shift) = (Modifiers::NONE, Modifiers::SHIFT);
        let at_playhead = self.frame_start(self.position);

        if pressed(Modifiers::COMMAND | shift, Key::Z) || pressed(Modifiers::COMMAND, Key::Y) {
            self.redo();
        } else if pressed(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if pressed(none, Key::Space) {
            self.toggle_play();
        }
        if pressed(none, Key::K) {
            self.pause();
        }
        if pressed(none, Key::L) && !self.playing {
            self.play();
        }
        if pressed(shift, Key::ArrowLeft) {
            self.seek(self.position - 1.0);
        } else if pressed(none, Key::ArrowLeft) {
            self.step(-1);
        }
        if pressed(shift, Key::ArrowRight) {
            self.seek(self.position + 1.0);
        } else if pressed(none, Key::ArrowRight) {
            self.step(1);
        }
        if pressed(none, Key::I) {
            self.set_export_start(at_playhead);
        }
        if pressed(none, Key::O) {
            self.set_export_end(at_playhead);
        }
        if pressed(none, Key::OpenBracket) {
            self.trim_start(at_playhead);
        }
        if pressed(none, Key::CloseBracket) {
            self.trim_end(at_playhead);
        }
        if pressed(none, Key::ArrowUp) {
            self.seek(self.sequence.export.start);
        }
        if pressed(none, Key::ArrowDown) {
            self.seek(self.sequence.export.end);
        }
        if pressed(none, Key::Home) {
            self.seek(self.content().start);
        }
        if pressed(none, Key::End) {
            self.seek(self.content().end);
        }
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        let fps = self.fps();
        let frame = self.frame();
        egui::TopBottomPanel::bottom("timeline").show(ctx, |ui| {
            ui.add_space(6.0);
            let response = self.timeline.show(ui, &mut self.position, &mut self.sequence, Some(self.selected), fps);
            if (response.seeked || response.edit_started) && self.playing {
                self.pause();
            }
            if let Some(id) = response.selected {
                self.selected = id;
            }
            self.peek = response.peek;
            ui.add_space(4.0);

            // Every time shown is time in the file: it counts from the export start.
            let zero = self.sequence.export.start;

            ui.horizontal(|ui| {
                if ui.button("⏮").on_hover_text("Go to the export start (Up)").clicked() {
                    self.seek(self.sequence.export.start);
                }
                if ui.button("⏪").on_hover_text("Previous frame (Left, Shift+Left for a second)").clicked() {
                    self.step(-1);
                }
                let play = if self.playing { "⏸" } else { "▶" };
                let play_button = egui::Button::new(RichText::new(play).size(16.0)).min_size(egui::vec2(44.0, 0.0));
                if ui.add(play_button).on_hover_text("Play / pause (Space)").clicked() {
                    self.toggle_play();
                }
                if ui.button("⏩").on_hover_text("Next frame (Right, Shift+Right for a second)").clicked() {
                    self.step(1);
                }
                if ui.button("⏭").on_hover_text("Go to the export end (Down)").clicked() {
                    self.seek(self.sequence.export.end);
                }
                ui.separator();
                let mut t = self.position - zero;
                if ui.add(time_value(&mut t, fps)).on_hover_text("Playhead").changed() {
                    self.seek(zero + t);
                }
                ui.separator();
                let looped = ui
                    .toggle_value(&mut self.loop_export, "🔁 Loop export")
                    .on_hover_text("Space plays the export range in a loop, with the fades from the export settings");
                if looped.changed() && self.playing {
                    self.play();
                }
                if ui.button("▶ Play export").on_hover_text("Play the export range with the fades from the export settings").clicked() {
                    self.play_export();
                }
            });

            let at_playhead = self.frame_start(self.position);
            egui::Grid::new("ranges").num_columns(10).spacing([8.0, 4.0]).show(ui, |ui| {
                // The export starts at the zero by definition, so In and Out would be 0 and the length.
                // The columns line up with the row of the clip: the two kinds of Set and the two lengths.
                let export = self.sequence.export;
                ui.label(RichText::new("Export").strong());
                ui.label("");
                ui.label("");
                if ui.button("Set In (I)").on_hover_text("Start the export at the playhead").clicked() {
                    self.set_export_start(at_playhead);
                }
                ui.label("");
                ui.label("");
                if ui.button("Set Out (O)").on_hover_text("End the export at the playhead").clicked() {
                    self.set_export_end(at_playhead);
                }
                ui.label("Length");
                let mut length = export.len();
                if ui.add(time_value(&mut length, fps)).changed() {
                    self.sequence.export.resize_end(export.start + length, frame);
                }
                let fit = ui
                    .add_enabled(!self.sequence.export_fits(), egui::Button::new("Fit to clips"))
                    .on_hover_text("Make the export range exactly what the clips cover")
                    .on_disabled_hover_text("The export range is exactly what the clips cover. Trimming a clip takes it along");
                if fit.clicked() {
                    self.sequence.fit_export();
                }
                ui.end_row();

                let clip = self.clip().span();
                let (source_in, source_out) = (self.clip().source_in, self.clip().source_out);
                ui.label(RichText::new("Clip").strong());
                ui.label("Start");
                let mut start = clip.start - zero;
                if ui.add(time_value(&mut start, fps)).on_hover_text("Where the clip starts in the file. Changing it trims the clip").changed() {
                    self.trim_start(zero + start);
                }
                if ui.button("Set ([)").on_hover_text("Trim the clip's start to the playhead").clicked() {
                    self.trim_start(at_playhead);
                }
                ui.label("End");
                let mut clip_end = clip.end - zero;
                if ui.add(time_value(&mut clip_end, fps)).on_hover_text("Where the clip ends in the file. Changing it trims the clip").changed() {
                    self.trim_end(zero + clip_end);
                }
                if ui.button("Set (])").on_hover_text("Trim the clip's end to the playhead").clicked() {
                    self.trim_end(at_playhead);
                }
                ui.label("Length");
                let mut length = clip.len();
                if ui.add(time_value(&mut length, fps)).changed() {
                    self.trim_end(clip.start + length);
                }
                ui.label(RichText::new(format!("from source {} to {}", timecode::format(source_in), timecode::format(source_out))).weak());
                ui.end_row();
            });
            ui.add_space(6.0);
        });
    }

    fn side_panel(&mut self, ctx: &egui::Context, plugins: &[Plugin], workspace: Option<&Workspace>, settings: &Settings) {
        let shown = self.framing.shown_presets(self.sequence.output);
        egui::SidePanel::right("output").resizable(true).default_width(SIDE_PANEL_WIDTH).min_width(SIDE_PANEL_MIN_WIDTH).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.side_tab, SideTab::Output, RichText::new("Output").size(TAB_TEXT_SIZE));
                ui.selectable_value(&mut self.side_tab, SideTab::Export, RichText::new(self.export.tab_text()).size(TAB_TEXT_SIZE));
            });
            ui.separator();
            match self.side_tab {
                SideTab::Output => {
                    egui::ScrollArea::vertical().show(ui, |ui| self.output_tab(ui));
                }
                SideTab::Export => {
                    let no_meta = BTreeMap::new();
                    let meta = self.sources.first().map_or(&no_meta, |source| &source.meta);
                    let publish = workspace.map(|workspace| PublishEnv { workspace, settings, name: &self.name, meta });
                    let env = ExportEnv { source: &self.cache.source, info: &self.cache.info, shown: &shown, position: self.position, plugins, publish };
                    self.export.show(ui, &mut self.sequence, &env);
                }
            }
        });

        // Whatever the tab: a preset on the other one can change the frame the picture has to fill.
        let (size, outline) = (self.frame_size(), self.sequence.output.outline());
        let t = &mut self.sequence.clip_mut(self.selected).transform;
        t.angle_deg = geometry::normalize_angle(t.angle_deg);
        *t = t.clamped(size[0], size[1], outline);
    }

    fn output_tab(&mut self, ui: &mut egui::Ui) {
        let output = self.sequence.output;
        let settings = self.sequence.settings;
        let outline = output.outline();
        let size = self.frame_size();
        // Between clips there is nothing in the output.
        let texture = self.texture.as_ref().filter(|_| self.sequence.clip_at(self.shown_time()).is_some());
        let t = &mut self.sequence.clip_mut(self.selected).transform;
        let [box_w, box_h] = outline.fit(ui.available_width() as f64, OUTPUT_MAX_HEIGHT);
        let box_size = egui::vec2(box_w as f32, box_h as f32);
        ui.vertical_centered(|ui| {
            self.output_view.show(ui, texture, size, outline, t, box_size);
            let [px_w, px_h] = settings.pixels(output);
            let caption = format!("{} · {} · {px_w}×{px_h}", output.preset().name, output.ratio().label());
            ui.label(RichText::new(caption).weak());
        });
        ui.separator();

        ui.heading("Transform");
        let (mut x, mut y) = (t.cx * 100.0, t.cy * 100.0);
        egui::Grid::new("transform").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("X");
            if ui.add(egui::DragValue::new(&mut x).speed(0.1).fixed_decimals(1).suffix(" %")).changed() {
                t.cx = x / 100.0;
            }
            ui.end_row();
            ui.label("Y");
            if ui.add(egui::DragValue::new(&mut y).speed(0.1).fixed_decimals(1).suffix(" %")).changed() {
                t.cy = y / 100.0;
            }
            ui.end_row();
            ui.label("Zoom");
            ui.add(egui::DragValue::new(&mut t.zoom).speed(0.01).range(geometry::MIN_ZOOM_WITH_BARS..=geometry::MAX_ZOOM).fixed_decimals(2).prefix("×"));
            ui.end_row();
            ui.label("Angle");
            ui.add(egui::DragValue::new(&mut t.angle_deg).speed(0.2).fixed_decimals(1).suffix("°"));
            ui.end_row();
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Center").clicked() {
                t.cx = 0.5;
                t.cy = 0.5;
            }
            if ui.button("−90°").clicked() {
                t.angle_deg -= 90.0;
            }
            if ui.button("+90°").clicked() {
                t.angle_deg += 90.0;
            }
            if ui.button("0°").clicked() {
                t.angle_deg = 0.0;
            }
            let mirror = ui.add(egui::Button::new("Mirror").selected(t.mirror));
            if mirror.on_hover_text("Flip the output left to right").clicked() {
                t.mirror = !t.mirror;
            }
            if ui.button("Reset").clicked() {
                *t = Transform::default();
            }
        });
        ui.label(
            RichText::new(
                "Drag inside the frame to move it (Alt: no snapping). Drag a side to resize it \
                 (Ctrl: the opposite side stays); with the lock open that changes the proportions \
                 (Shift: the opposite of the lock). Drag an arrow to rotate it (Shift: 15° steps). \
                 Rest on the mark at its top for Fit, Fill, Bars and Reset; click it for 0°. Wheel to zoom.",
            )
            .weak()
            .small(),
        );
        ui.separator();

        if self.cache.info.has_audio {
            ui.heading("Sound");
            let clip = self.sequence.clip_mut(self.selected);
            ui.horizontal(|ui| {
                let mut percent = clip.volume * 100.0;
                ui.label("Volume");
                if ui.add(egui::Slider::new(&mut percent, 0.0..=200.0).fixed_decimals(0).suffix(" %")).changed() {
                    clip.volume = percent / 100.0;
                }
                if ui.add(egui::Button::new("Mute").selected(clip.muted)).clicked() {
                    clip.muted = !clip.muted;
                }
            });
            ui.separator();
        }

        ui.heading("Source");
        let info = &self.cache.info;
        egui::Grid::new("source").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("Size");
            ui.label(format!("{}×{}", info.width, info.height));
            ui.end_row();
            ui.label("Frame rate");
            let preview_fps = self.cache.format.fps.as_f64();
            if self.cache.format.fps == info.fps {
                ui.label(format!("{:.2} fps", info.fps.as_f64()));
            } else {
                ui.label(format!("{:.2} fps (preview {:.2})", info.fps.as_f64(), preview_fps));
            }
            ui.end_row();
            ui.label("Duration");
            ui.label(timecode::format(info.duration));
            ui.end_row();
            ui.label("Audio");
            ui.label(if info.has_audio { "yes" } else { "no" });
            ui.end_row();
        });
        if let Some(error) = &self.player.audio_error {
            ui.colored_label(Color32::from_rgb(230, 120, 60), format!("No sound: {error}"));
        }
    }

    fn source_panel(&mut self, ctx: &egui::Context) {
        let size = self.frame_size();
        let note = (!self.clip().span().contains(self.shown_time())).then_some("Outside the clip");
        let frame = egui::Frame::none().fill(Color32::from_gray(16));
        let (mut undo, mut redo) = (false, false);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                self.framing.menu(ui, &mut self.sequence.output);
                self.framing.ratio_controls(ui, &mut self.sequence.output, size[0] / size[1]);
                ui.separator();
                let can_undo = self.history.can_undo(&self.sequence);
                undo = ui.add_enabled(can_undo, egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked();
                let can_redo = self.history.can_redo(&self.sequence);
                redo = ui.add_enabled(can_redo, egui::Button::new("Redo")).on_hover_text("Ctrl+Y or Ctrl+Shift+Z").clicked();
            });

            let frames = self.framing.overlays(self.sequence.output);
            let transform = &mut self.sequence.clip_mut(self.selected).transform;
            let aspect = self.source_view.show(ui, self.texture.as_ref(), size, &frames, note, transform);
            if let Some(aspect) = aspect {
                self.framing.set_dragged_ratio(&mut self.sequence.output, aspect);
            }
        });
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
    }
}

/// A time field: drag to change, click to type `mm:ss.cc`, `mm:ss:ff` or seconds.
fn time_value(value: &mut f64, fps: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .speed(1.0 / fps)
        .custom_formatter(|v, _| timecode::format(v))
        .custom_parser(move |s| timecode::parse(s, fps))
}

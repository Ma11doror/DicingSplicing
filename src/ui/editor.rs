//! The editor: source view, output preview with transform controls, timeline and transport.
//!
//! A project has sources, videos linked from the disk, and clips of them on tracks. One clip is
//! selected at a time: the middle shows its source with its frame, the side panel its transform and
//! sound. The output preview shows what goes into the file: the clip seen at the playhead.

use crate::core::edit::{Clip, ClipId, Sequence, SourceId, Span};
use crate::core::history::History;
use crate::core::plugins::Plugin;
use crate::core::project::{self, Project, Source, ViewState, RENDERS_FOLDER};
use crate::core::settings::Settings;
use crate::core::workspace::Workspace;
use crate::media::audio::{PlayRange, Segment, Sound};
use crate::media::geometry::{self, Transform};
use crate::media::library::{Library, SourceState};
use crate::media::player::Player;
use crate::media::render;
use crate::media::{timecode, Notify, SourceFiles};
use crate::ui::export::{ExportEnv, ExportPanel};
use crate::ui::framing::Framing;
use crate::ui::picture::{frame_at, Picture};
use crate::ui::pick_video;
use crate::ui::plugins::PublishEnv;
use crate::ui::sources::{self, SourceAction, SourceRow};
use crate::ui::timeline::{Timeline, TimelineEnv};
use crate::ui::viewer::{OutputView, SourceView};
use egui::{Color32, Key, Modifiers, RichText, TextureHandle};
use std::collections::{BTreeMap, BTreeSet};
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
const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
/// What a source without its file yet is drawn as: a 16:9 picture.
const NO_SIZE: [f64; 2] = [16.0, 9.0];

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

/// What the output preview drew last. When the clip at the playhead becomes one of another source,
/// this stays on the screen until that source's frame is there, instead of a black flash.
#[derive(Clone, Copy)]
struct Drawn {
    source: SourceId,
    transform: Transform,
    size: [f64; 2],
}

pub struct Editor {
    // Dropped first: stops preparing frames.
    library: Library,
    pub name: String,
    dir: PathBuf,
    sources: Vec<Source>,
    /// Sources removed in this session: an undo can bring back a clip of one of them.
    removed: Vec<Source>,
    /// The project as it is on the disk.
    saved: Project,
    /// Since when there are changes that are not on the disk.
    changed_since: Option<f64>,
    pub save_error: Option<String>,
    player: Player,
    /// The selected clip's source, in the middle.
    center: Picture,
    /// The clip seen at the playhead, when it is not the selected one at the same frame.
    output: Picture,
    /// The output shows the same frame as the middle, and takes it from there.
    output_shares_center: bool,
    drawn: Option<Drawn>,
    /// The playhead, in timeline seconds.
    position: f64,
    /// While an edge is dragged on the timeline: the time the previews show instead of the playhead.
    peek: Option<f64>,
    playing: bool,
    play_mode: PlayMode,
    loop_export: bool,
    sequence: Sequence,
    selected: Option<ClipId>,
    history: History<Sequence>,
    framing: Framing,
    side_tab: SideTab,
    export: ExportPanel,
    timeline: Timeline,
    source_view: SourceView,
    output_view: OutputView,
    /// The result's frame rate: the grid of the timeline. Updated every frame.
    fps: f64,
    /// The files that can be rendered from, as of this frame.
    files: SourceFiles,
    /// Open the Sources list on the next frame: a video has just been added.
    show_sources: bool,
}

impl Editor {
    /// Opens the project in the folder `dir`. Its videos are looked at in the background, and their
    /// frames for the editor prepared in `cache_root`.
    pub fn new(project: Project, dir: PathBuf, cache_root: PathBuf, ctx: &egui::Context) -> Self {
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let mut library = Library::new(cache_root, notify.clone());
        for source in &project.sources {
            library.add(source.id, &source.path);
        }
        let saved = project.clone();
        let Project { name, sources, sequence, view, .. } = project;
        let selected = sequence.shown_at(view.playhead).or_else(|| sequence.clips().next().map(|(id, _)| id));
        let export = ExportPanel::for_project(&name, dir.join(RENDERS_FOLDER), view.export_path, view.export_shown_frames);
        Self {
            library,
            name,
            dir,
            sources,
            removed: Vec::new(),
            saved,
            changed_since: None,
            save_error: None,
            player: Player::new(),
            center: Picture::new("source frame", notify.clone()),
            output: Picture::new("output frame", notify),
            output_shares_center: false,
            drawn: None,
            position: view.playhead,
            peek: None,
            playing: false,
            play_mode: PlayMode::Timeline,
            loop_export: view.loop_export,
            history: History::new(sequence.clone()),
            sequence,
            selected,
            framing: Framing::restored(&view.frames, view.ratio_locked),
            side_tab: SideTab::default(),
            export,
            timeline: Timeline::new(view.timeline_scale),
            source_view: SourceView::default(),
            output_view: OutputView,
            fps: 30.0,
            files: SourceFiles::new(),
            show_sources: false,
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
        self.take_in_media();
        self.files = self.library.files();
        self.fps = render::output_fps(&self.sequence, &self.files);
        if self.playing {
            let (t, still_playing) = self.player.poll();
            self.position = t;
            self.playing = still_playing;
        }
        self.center.poll(ctx);
        self.output.poll(ctx);
        self.check_selection();

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
        self.request_frames();
    }

    /// What the background work on the sources has found. A file that turned out to have another
    /// length than the project remembers was replaced: its clips are fitted to it, in every undo
    /// step too, since this is a fact and not an edit.
    fn take_in_media(&mut self) {
        for (id, info) in self.library.poll() {
            let duration = info.duration;
            self.sequence.refit_source(id, duration);
            self.history.amend(|sequence| {
                sequence.refit_source(id, duration);
            });
        }
    }

    /// A selection that an undo or a removal left without its clip goes.
    fn check_selection(&mut self) {
        if self.selected.is_some_and(|id| !self.sequence.has(id)) {
            self.selected = None;
        }
    }

    /// Asks for the frames the views will need: the selected clip's for the middle, the one seen at
    /// the playhead for the output, unless it is the same frame.
    fn request_frames(&mut self) {
        let t = self.shown_time();
        let center = self.selected.map(|id| self.sequence.clip(id)).map(|clip| (clip.source, clip.source_time(t)));
        let mut center_frame = None;
        if let Some((source, time)) = center {
            if let Some(cache) = self.library.cache(source) {
                center_frame = Some((source, self.center.want(source, time, cache)));
            }
        }
        self.output_shares_center = false;
        if let Some(clip) = self.sequence.shown_at(t).map(|id| self.sequence.clip(id)) {
            if let Some(cache) = self.library.cache(clip.source) {
                let time = clip.source_time(t);
                self.output_shares_center = center_frame == Some((clip.source, frame_at(cache, time)));
                if !self.output_shares_center {
                    self.output.want(clip.source, time, cache);
                }
            }
        }
    }

    fn frame(&self) -> f64 {
        1.0 / self.fps
    }

    /// Start of the frame shown at `t`: where I, O and the trim keys put an edge.
    fn frame_start(&self, t: f64) -> f64 {
        timecode::frame_index(t, self.fps) as f64 / self.fps
    }

    fn clip(&self) -> Option<&Clip> {
        self.selected.map(|id| self.sequence.clip(id))
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

    /// The picture size of a source, once its file has been read.
    fn size_of(&self, source: SourceId) -> Option<[f64; 2]> {
        self.library.info(source).map(|info| [info.width as f64, info.height as f64])
    }

    fn play_range(&self, mode: PlayMode) -> PlayRange {
        let sounds: Vec<Sound> = self
            .sequence
            .clips()
            .filter(|(id, clip)| !self.sequence.tracks[id.track].muted && clip.gain() > 0.0)
            .filter_map(|(_, clip)| {
                let segment = Segment { start: clip.start, end: clip.end(), source_start: clip.source_in, gain: clip.gain() as f32 };
                Some(Sound { segment, pcm: self.library.cache(clip.source)?.audio.clone() })
            })
            .collect();
        let sounds: Arc<[Sound]> = sounds.into();
        match mode {
            PlayMode::Timeline => {
                let span = self.play_span();
                PlayRange { start: span.start, end: span.end, looping: false, fades: None, sounds }
            }
            PlayMode::Export => PlayRange {
                start: self.sequence.export.start,
                end: self.sequence.export.end,
                looping: self.loop_export,
                fades: Some(self.sequence.settings.fades),
                sounds,
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
        let frame = timecode::frame_index(self.position, self.fps) + frames;
        self.seek(frame as f64 / self.fps);
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
        if let Some(id) = self.selected {
            self.sequence.trim_start(id, t, frame);
        }
    }

    fn trim_end(&mut self, t: f64) {
        let frame = self.frame();
        if let Some(id) = self.selected {
            self.sequence.trim_end(id, t, frame);
        }
    }

    fn delete_selected(&mut self) {
        if let Some(id) = self.selected.take() {
            self.pause();
            self.sequence.remove_clip(id);
        }
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

    /// What is playing no longer matches the edit, and a clip that came back may need its source.
    fn after_history_step(&mut self) {
        self.pause();
        self.check_selection();
        let needed: BTreeSet<SourceId> = self.sequence.clips().map(|(_, clip)| clip.source).collect();
        for id in needed {
            if self.sources.iter().any(|source| source.id == id) {
                continue;
            }
            if let Some(i) = self.removed.iter().position(|source| source.id == id) {
                let source = self.removed.remove(i);
                self.library.add(source.id, &source.path);
                self.sources.push(source);
                self.sources.sort_by_key(|source| source.id);
            }
        }
    }

    /// Adds a video to the project's sources, from a dialog, a drop or a plugin. A video that is
    /// there already is not added again. The list opens to show it.
    pub fn add_source(&mut self, path: PathBuf, name: Option<String>, meta: BTreeMap<String, String>) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let (id, new) = project::add_source(&mut self.sources, path.clone(), name, meta);
        if new {
            self.library.add(id, &path);
        }
        self.show_sources = true;
    }

    /// Puts the whole source on a new track under the others, after the last clip, and goes there.
    fn add_to_timeline(&mut self, id: SourceId) {
        let (Some(source), Some(info)) = (self.sources.iter().find(|source| source.id == id), self.library.info(id)) else { return };
        let clip = Clip::of(id, source.label(), info.duration);
        let added = self.sequence.add_below(clip);
        self.selected = Some(added);
        self.seek(self.sequence.clip(added).start);
    }

    /// The source's file is somewhere else now.
    fn locate(&mut self, id: SourceId) {
        let Some(path) = pick_video() else { return };
        let Some(source) = self.sources.iter_mut().find(|source| source.id == id) else { return };
        source.path = path;
        self.center.forget(id);
        self.output.forget(id);
        self.library.add(id, &source.path);
    }

    fn remove_source(&mut self, id: SourceId) {
        if self.sequence.uses(id) > 0 {
            return;
        }
        if let Some(i) = self.sources.iter().position(|source| source.id == id) {
            self.removed.push(self.sources.remove(i));
            self.library.remove(id);
            self.center.forget(id);
            self.output.forget(id);
        }
    }

    /// Sources whose clips can't be shown: the file is not there, can't be read, or the source is gone.
    fn missing(&self) -> BTreeSet<SourceId> {
        let mut missing: BTreeSet<SourceId> = self
            .sources
            .iter()
            .filter(|source| matches!(self.library.state(source.id), SourceState::Missing | SourceState::Failed(_)))
            .map(|source| source.id)
            .collect();
        missing.extend(self.sequence.clips().map(|(_, clip)| clip.source).filter(|id| !self.sources.iter().any(|source| source.id == *id)));
        missing
    }

    /// The project's part of the top bar: its name, its sources and how their preparing goes.
    /// Returns the plugin to bring a video in with, when the user asked for one.
    pub fn top_bar(&mut self, ui: &mut egui::Ui, plugins: &[Plugin]) -> Option<usize> {
        ui.label(RichText::new(&self.name).strong()).on_hover_text(self.dir.display().to_string());
        let rows: Vec<SourceRow> = self
            .sources
            .iter()
            .map(|source| SourceRow { source, state: self.library.state(source.id), info: self.library.info(source.id), uses: self.sequence.uses(source.id) })
            .collect();
        let action = sources::sources_button(ui, &rows, plugins, std::mem::take(&mut self.show_sources));
        if let Some((count, progress)) = self.library.preparing() {
            let more = if count > 1 { format!(", {} more waiting", count - 1) } else { String::new() };
            let text = format!("Preparing frames {:.0}%{more}", progress * 100.0);
            ui.add(egui::ProgressBar::new(progress).desired_width(260.0).text(text));
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        if let Some(error) = &self.save_error {
            ui.colored_label(ERROR, error);
        }
        match action? {
            SourceAction::AddToTimeline(id) => self.add_to_timeline(id),
            SourceAction::Locate(id) => self.locate(id),
            SourceAction::Remove(id) => self.remove_source(id),
            SourceAction::AddVideo => {
                if let Some(path) = pick_video() {
                    self.add_source(path, None, BTreeMap::new());
                }
            }
            SourceAction::Plugin(i) => return Some(i),
        }
        None
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
        if pressed(none, Key::Delete) || pressed(none, Key::Backspace) {
            self.delete_selected();
        }
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        let fps = self.fps;
        let frame = self.frame();
        let missing = self.missing();
        let placeable: BTreeMap<SourceId, (String, f64)> = self
            .sources
            .iter()
            .filter_map(|source| Some((source.id, (source.label(), self.library.info(source.id)?.duration))))
            .collect();
        egui::TopBottomPanel::bottom("timeline").show(ctx, |ui| {
            ui.add_space(6.0);
            let env = TimelineEnv { fps, missing: &missing, placeable: &placeable };
            let response = self.timeline.show(ui, &mut self.position, &mut self.sequence, &mut self.selected, &env);
            if (response.seeked || response.edit_started) && self.playing {
                self.pause();
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

                ui.label(RichText::new("Clip").strong());
                let Some(clip) = self.clip() else {
                    // The same row, unavailable: nothing moves when a clip is picked.
                    let mut nothing = 0.0;
                    ui.label("Start");
                    ui.add_enabled(false, time_value(&mut nothing, fps));
                    ui.add_enabled(false, egui::Button::new("Set ([)"));
                    ui.label("End");
                    ui.add_enabled(false, time_value(&mut nothing, fps));
                    ui.add_enabled(false, egui::Button::new("Set (])"));
                    ui.label("Length");
                    ui.add_enabled(false, time_value(&mut nothing, fps));
                    ui.label(RichText::new("Select a clip on the timeline").weak());
                    ui.end_row();
                    return;
                };
                let span = clip.span();
                let (source_in, source_out) = (clip.source_in, clip.source_out);
                ui.label("Start");
                let mut start = span.start - zero;
                if ui.add(time_value(&mut start, fps)).on_hover_text("Where the clip starts in the file. Changing it trims the clip").changed() {
                    self.trim_start(zero + start);
                }
                if ui.button("Set ([)").on_hover_text("Trim the clip's start to the playhead").clicked() {
                    self.trim_start(at_playhead);
                }
                ui.label("End");
                let mut clip_end = span.end - zero;
                if ui.add(time_value(&mut clip_end, fps)).on_hover_text("Where the clip ends in the file. Changing it trims the clip").changed() {
                    self.trim_end(zero + clip_end);
                }
                if ui.button("Set (])").on_hover_text("Trim the clip's end to the playhead").clicked() {
                    self.trim_end(at_playhead);
                }
                ui.label("Length");
                let mut length = span.len();
                if ui.add(time_value(&mut length, fps)).changed() {
                    self.trim_end(span.start + length);
                }
                ui.label(RichText::new(format!("from source {} to {}", timecode::format(source_in), timecode::format(source_out))).weak())
                    .on_hover_text("Delete removes the clip");
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
                    // What the plugins are told about the result: the video it is mostly made of.
                    let no_meta = BTreeMap::new();
                    let main = self.sequence.main_source();
                    let meta = self.sources.iter().find(|source| Some(source.id) == main).map_or(&no_meta, |source| &source.meta);
                    let publish = workspace.map(|workspace| PublishEnv { workspace, settings, name: &self.name, meta });
                    let env = ExportEnv { files: &self.files, shown: &shown, position: self.position, plugins, publish };
                    self.export.show(ui, &mut self.sequence, &env);
                }
            }
        });

        // Whatever the tab: a preset on the other one can change the frame the pictures have to fill.
        let outline = self.sequence.output.outline();
        let sizes: Vec<Option<[f64; 2]>> = self.sequence.clips().map(|(_, clip)| self.size_of(clip.source)).collect();
        let clips = self.sequence.tracks.iter_mut().flat_map(|track| &mut track.clips);
        for (clip, size) in clips.zip(sizes) {
            if let Some([w, h]) = size {
                let t = &mut clip.transform;
                t.angle_deg = geometry::normalize_angle(t.angle_deg);
                *t = t.clamped(w, h, outline);
            }
        }
    }

    /// The texture with the frame the output shows, the clip it is of if that is still the clip at
    /// the playhead, and how to draw it.
    fn output_picture(&self) -> (Option<&TextureHandle>, Option<ClipId>, Drawn) {
        let Some(id) = self.sequence.shown_at(self.shown_time()) else {
            return (None, None, Drawn { source: 0, transform: Transform::default(), size: NO_SIZE });
        };
        let clip = self.sequence.clip(id);
        let picture = if self.output_shares_center { &self.center } else { &self.output };
        let size = self.size_of(clip.source);
        if let (Some(texture), Some(size)) = (picture.of(clip.source), size) {
            return (Some(texture), Some(id), Drawn { source: clip.source, transform: clip.transform, size });
        }
        // The frame of the new clip is on its way: what was there stays meanwhile. Only when it is
        // just being decoded, not while that video's frames are still being prepared: then the old
        // picture would stand for another clip.
        let ready = self.library.cache(clip.source).is_some_and(|cache| cache.frames.get(frame_at(cache, clip.source_time(self.shown_time()))).is_some());
        if let Some(drawn) = self.drawn.filter(|_| ready) {
            if let Some(texture) = self.output.of(drawn.source).or(self.center.of(drawn.source)) {
                return (Some(texture), None, drawn);
            }
        }
        (None, Some(id), Drawn { source: clip.source, transform: clip.transform, size: size.unwrap_or(NO_SIZE) })
    }

    fn output_tab(&mut self, ui: &mut egui::Ui) {
        let output = self.sequence.output;
        let settings = self.sequence.settings;
        let outline = output.outline();
        let [box_w, box_h] = outline.fit(ui.available_width() as f64, OUTPUT_MAX_HEIGHT);
        let box_size = egui::vec2(box_w as f32, box_h as f32);

        let (texture, live, drawn) = self.output_picture();
        let texture = texture.cloned();
        if texture.is_some() && live.is_some() {
            self.drawn = Some(drawn);
        }
        ui.vertical_centered(|ui| {
            // Dragging or zooming the output edits the clip it shows, and selects it.
            let touched = match live {
                Some(id) => self.output_view.show(ui, texture.as_ref(), drawn.size, outline, &mut self.sequence.clip_mut(id).transform, box_size),
                None => {
                    // What was there before stays only to look at.
                    let mut still = drawn.transform;
                    self.output_view.show(ui, texture.as_ref(), drawn.size, outline, &mut still, box_size);
                    false
                }
            };
            if touched {
                self.selected = live;
            }
            let [px_w, px_h] = settings.pixels(output);
            let caption = format!("{} · {} · {px_w}×{px_h}", output.preset().name, output.ratio().label());
            ui.label(RichText::new(caption).weak());
        });
        ui.separator();

        let Some(id) = self.selected else {
            ui.label(RichText::new("Select a clip on the timeline to set its frame and sound.").weak());
            return;
        };
        let source = self.sequence.clip(id).source;
        let info = self.library.info(source).cloned();
        ui.heading("Transform");
        let t = &mut self.sequence.clip_mut(id).transform;
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

        if info.as_ref().is_some_and(|info| info.has_audio) {
            ui.heading("Sound");
            let clip = self.sequence.clip_mut(id);
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
        let label = self.sources.iter().find(|s| s.id == source).map_or_else(|| "removed from the project".to_owned(), Source::label);
        ui.label(RichText::new(label).strong());
        let Some(info) = info else {
            ui.label(RichText::new("Its file has not been read.").weak());
            return;
        };
        egui::Grid::new("source").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("Size");
            ui.label(format!("{}×{}", info.width, info.height));
            ui.end_row();
            ui.label("Frame rate");
            match self.library.cache(source).map(|cache| cache.format.fps).filter(|preview| *preview != info.fps) {
                Some(preview) => ui.label(format!("{:.2} fps (preview {:.2})", info.fps.as_f64(), preview.as_f64())),
                None => ui.label(format!("{:.2} fps", info.fps.as_f64())),
            };
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
        let selected = self.selected.map(|id| (id, self.sequence.clip(id).source));
        let size = selected.and_then(|(_, source)| self.size_of(source));
        let aspect = size.map_or(NO_SIZE[0] / NO_SIZE[1], |[w, h]| w / h);
        let note = selected.filter(|(id, _)| !self.sequence.clip(*id).span().contains(self.shown_time())).map(|_| "Outside the clip");
        let frame = egui::Frame::none().fill(Color32::from_gray(16));
        let (mut undo, mut redo) = (false, false);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                self.framing.menu(ui, &mut self.sequence.output);
                self.framing.ratio_controls(ui, &mut self.sequence.output, aspect);
                ui.separator();
                let can_undo = self.history.can_undo(&self.sequence);
                undo = ui.add_enabled(can_undo, egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked();
                let can_redo = self.history.can_redo(&self.sequence);
                redo = ui.add_enabled(can_redo, egui::Button::new("Redo")).on_hover_text("Ctrl+Y or Ctrl+Shift+Z").clicked();
            });

            match (selected, size) {
                (Some((id, source)), Some(size)) => {
                    let frames = self.framing.overlays(self.sequence.output);
                    let texture = self.center.of(source);
                    let transform = &mut self.sequence.clip_mut(id).transform;
                    if let Some(aspect) = self.source_view.show(ui, texture, size, &frames, note, transform) {
                        self.framing.set_dragged_ratio(&mut self.sequence.output, aspect);
                    }
                }
                _ => {
                    let text = match selected {
                        Some((_, source)) => match self.library.state(source) {
                            SourceState::Missing => "The file of this clip is not there. Sources: Locate file".to_owned(),
                            SourceState::Failed(error) => format!("The file of this clip can't be read: {error}"),
                            _ => "Opening the video…".to_owned(),
                        },
                        None if self.sequence.clips().next().is_some() => "Select a clip on the timeline".to_owned(),
                        None => "Add a video from Sources, then put it on the timeline".to_owned(),
                    };
                    ui.centered_and_justified(|ui| ui.label(RichText::new(text).weak()));
                }
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

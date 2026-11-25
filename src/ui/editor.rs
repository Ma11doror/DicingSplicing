//! The editor: the preview with the pictures on the frame, the clip's panel, the timeline and the
//! transport.
//!
//! A project has sources, videos linked from the disk, and clips of them on tracks. Clips are picked
//! as in a file manager, one or several; the main one is shown: the side panel shows its timing,
//! transform, sound and effects, and what is set there goes to all the picked clips. The preview in
//! the middle shows what goes into the file at the playhead, every clip seen there where it is on the
//! frame, and the main clip's whole picture with its handles: there pictures are picked, moved,
//! resized and turned.

use crate::core::edit::{speed_text, Clip, ClipId, ClipKey, Color, Copied, Curve, Edge, Effect, Run, Sequence, SourceId, Span, Speed, TrackId, TrackKind, FREEZE_LENGTH, SLOW_MOTION_FACTOR, SLOW_MOTION_SECONDS};
use crate::core::selection::Selection;
use crate::core::history::History;
use crate::core::lock::EditorLock;
use crate::core::plugins::Plugin;
use crate::core::project::{self, Project, Source, ViewState, RENDERS_FOLDER};
use crate::core::settings::Settings;
use crate::core::workspace::Workspace;
use crate::media::audio::{PlayRange, Segment, Sound};
use crate::media::cache::Pcm;
use crate::core::preset::Output;
use crate::media::geometry::{self, Anchor, Outline, Transform};
use crate::media::library::{Library, SourceState};
use crate::media::player::Player;
use crate::media::render;
use crate::media::stretch::{Part, Stretcher};
use crate::media::{timecode, Notify, SourceFiles};
use crate::ui::export::{ExportEnv, ExportPanel};
use crate::ui::flash;
use crate::ui::framing::Framing;
use crate::ui::picture::{frame_at, Picture};
use crate::ui::timeline::track_name;
use crate::ui::{curve_choice, grip_dots, paint_curve, pick_media, pick_media_files};
use crate::ui::plugins::PublishEnv;
use crate::ui::sources::{self, SourceAction, SourceRow, SourcesPanel};
use crate::ui::timeline::{Has, Section, SourceSound, Timeline, TimelineEnv, Tool};
use crate::ui::viewer::{self, Canvas, Layer, Scene};
use egui::{Color32, Key, Modifiers, RichText};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const SIDE_PANEL_WIDTH: f32 = 380.0;
/// The border above the timeline, dragged by its dots.
const PANEL_BORDER_HEIGHT: f32 = 10.0;
/// The timeline before its border is dragged: about four rows. The least it keeps is one row with
/// the ruler and the buttons under it, and the pictures above it keep this much.
const TIMELINE_FIRST_HEIGHT: f32 = 300.0;
const TIMELINE_MIN_HEIGHT: f32 = 200.0;
const PREVIEW_MIN_HEIGHT: f32 = 160.0;
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

/// What the right column shows: the selected clip, or writing the result.
#[derive(Clone, Copy, Default, PartialEq)]
enum SideTab {
    #[default]
    Clip,
    Export,
}

/// What the preview drew last of a track. When its clip at the playhead becomes one of another
/// source, this stays on the screen until that source's frame is there, instead of a flash.
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
    /// What the preview shows of each video track: the frame of its clip seen at the playhead.
    layers: BTreeMap<TrackId, Picture>,
    /// The selected clip's frame, when it is not seen at the playhead: the preview shows it in its
    /// place all the same.
    apart: Picture,
    drawn: BTreeMap<TrackId, Drawn>,
    /// A picture was moved by hand: once the gesture is over, the picked clips get the anchor of where
    /// their pictures are. Resizing and turning keep the anchor.
    anchor_pending: bool,
    notify: Notify,
    /// The playhead, in timeline seconds.
    position: f64,
    /// While an edge is dragged on the timeline: the time the previews show instead of the playhead.
    peek: Option<f64>,
    playing: bool,
    play_mode: PlayMode,
    loop_export: bool,
    sequence: Sequence,
    /// The picked clips. The main one is what the Clip tab, the middle and most keys are about.
    selection: Selection,
    history: History<Sequence>,
    framing: Framing,
    side_tab: SideTab,
    export: ExportPanel,
    timeline: Timeline,
    /// How tall the timeline is, once its border has been dragged.
    timeline_height: Option<f32>,
    canvas: Canvas,
    /// The result's frame rate: the grid of the timeline. Updated every frame.
    fps: f64,
    /// The files that can be rendered from, as of this frame.
    files: SourceFiles,
    /// Open the Sources list on the next frame: a video has just been added.
    show_sources: bool,
    /// How the Sources list shows the sources, and what it keeps for that.
    sources_panel: SourcesPanel,
    /// A part of the Clip tab that a plaque on the timeline led to: it flashes, from then.
    flash: Option<(Section, f64)>,
    /// That part is still to be scrolled into sight.
    scroll_to: Option<Section>,
    /// The clips Ctrl+C copied, each with its track and that track's kind, and what it put into the
    /// system's clipboard to say so: Ctrl+V pastes them only while that text is still there.
    clipboard: Option<(Vec<Copied>, String)>,
    /// The clip whose effect could not change last time it was asked to, and which effect: the clips
    /// after it could not move that far. That effect's block says so.
    effect_refused: Option<(ClipId, usize)>,
    /// The still whose length could not change last time, for the same reason. Its Timing says so.
    length_refused: Option<ClipId>,
    /// The clip whose frame could not be held last time, for the same reason. Its Timing says so.
    freeze_refused: Option<ClipId>,
    /// The clip that could not play slower from the playhead last time, and why. Its Timing says so.
    slow_refused: Option<(ClipId, SlowRefused)>,
    /// The sound of the clips that play faster or slower, for playing them.
    stretcher: Stretcher,
    /// Keeps ds-tool from changing the project while it is open here. Last, so that it goes last.
    _lock: Option<EditorLock>,
}

impl Editor {
    /// Opens the project in the folder `dir`. Its videos are looked at in the background, and their
    /// frames for the editor prepared in `cache_root`.
    pub fn new(project: Project, dir: PathBuf, cache_root: PathBuf, ctx: &egui::Context) -> Self {
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let lock = EditorLock::take(&dir);
        let mut library = Library::new(cache_root, notify.clone());
        for source in &project.sources {
            library.add(source.id, &source.path);
        }
        let saved = project.clone();
        let Project { name, sources, sequence, view, .. } = project;
        let mut selection = Selection::default();
        selection.pick(&sequence, sequence.shown_at(view.playhead).or_else(|| sequence.clips().next().map(|(id, _)| id)));
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
            layers: BTreeMap::new(),
            apart: Picture::new("selected clip frame", notify.clone()),
            drawn: BTreeMap::new(),
            anchor_pending: false,
            notify: notify.clone(),
            position: view.playhead,
            peek: None,
            playing: false,
            play_mode: PlayMode::Timeline,
            loop_export: view.loop_export,
            history: History::new(sequence.clone()),
            sequence,
            selection,
            framing: Framing::restored(&view.frames, view.ratio_locked),
            side_tab: SideTab::default(),
            export,
            timeline: Timeline::new(view.timeline_scale, &view.expanded_tracks),
            timeline_height: view.timeline_height,
            canvas: Canvas::default(),
            fps: 30.0,
            files: SourceFiles::new(),
            show_sources: false,
            sources_panel: SourcesPanel::new(view.sources_view, view.sources_sort, view.sources_size, notify.clone()),
            flash: None,
            scroll_to: None,
            clipboard: None,
            effect_refused: None,
            length_refused: None,
            freeze_refused: None,
            slow_refused: None,
            stretcher: Stretcher::new(notify.clone()),
            _lock: lock,
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
                // Of the tracks that are there: a deleted one may come back with Undo, folded then.
                expanded_tracks: self.timeline.expanded().filter(|id| self.sequence.tracks.iter().any(|track| track.id == *id)).collect(),
                timeline_height: self.timeline_height,
                sources_view: self.sources_panel.view,
                sources_sort: self.sources_panel.sort,
                sources_size: self.sources_panel.size,
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
        self.apart.poll(ctx);
        for picture in self.layers.values_mut() {
            picture.poll(ctx);
        }
        self.check_selection();

        self.handle_keys(ctx);
        self.export.poll(ctx, settings);
        self.side_panel(ctx, plugins, workspace, settings);
        self.timeline_panel(ctx);
        self.preview_panel(ctx);

        if self.anchor_pending && !ctx.input(|i| i.pointer.any_down()) {
            self.anchor_pending = false;
            self.anchor_picked();
        }
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input();
        let now = ctx.input(|i| i.time);
        if self.history.track(&self.sequence, now, busy) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.autosave(ctx, now, busy);
        self.keep_stretched(ctx, now);

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

    /// The main picked clip: the one the Clip tab, the middle and most keys are about.
    fn selected(&self) -> Option<ClipId> {
        self.selection.main(&self.sequence)
    }

    /// Clips that an undo or a removal took away are picked no more.
    fn check_selection(&mut self) {
        self.selection.tidy(&self.sequence);
        // What was refused is said about that clip only, while it stays the main one.
        let selected = self.selected();
        if self.effect_refused.map(|(id, _)| id) != selected {
            self.effect_refused = None;
        }
        if self.length_refused != selected {
            self.length_refused = None;
        }
        if self.freeze_refused != selected {
            self.freeze_refused = None;
        }
        if self.slow_refused.map(|(id, _)| id) != selected {
            self.slow_refused = None;
        }
    }

    /// Asks for the frames the preview will need: of every clip seen at the playhead, and of the
    /// selected clip when it is not one of them.
    fn request_frames(&mut self) {
        let t = self.shown_time();
        let stack = self.sequence.stack_at(t, &sizes(&self.library));
        for &id in &stack {
            let clip = self.sequence.clip(id);
            let track = self.sequence.tracks[id.track].id;
            let Some(cache) = self.library.cache(clip.source) else { continue };
            let notify = &self.notify;
            let picture = self.layers.entry(track).or_insert_with(|| Picture::new("output frame", notify.clone()));
            picture.set_color(clip.graded());
            picture.want(clip.source, clip.picture_time(t), cache);
        }
        if let Some((id, _)) = self.apart_clip(&stack) {
            let clip = self.sequence.clip(id);
            if let Some(cache) = self.library.cache(clip.source) {
                self.apart.set_color(clip.graded());
                self.apart.want(clip.source, clip.picture_time(t), cache);
            }
        }
        // A track that is gone takes its pictures along.
        let tracks: BTreeSet<TrackId> = self.sequence.tracks.iter().map(|track| track.id).collect();
        self.layers.retain(|id, _| tracks.contains(id));
        self.drawn.retain(|id, _| tracks.contains(id));
    }

    /// The selected clip when it is a picture at the playhead that is not seen there, and why: its
    /// track is hidden, or a clip above it covers the frame. Before or after it, it is not in the
    /// result, and the preview does not show it.
    fn apart_clip(&self, stack: &[ClipId]) -> Option<(ClipId, &'static str)> {
        let id = self.selected().filter(|id| self.sequence.tracks[id.track].kind == TrackKind::Video && !stack.contains(id))?;
        if !self.sequence.clip(id).span().contains(self.shown_time()) {
            return None;
        }
        let note = if self.sequence.tracks[id.track].hidden { "Its track is hidden" } else { "Under a clip above it" };
        Some((id, note))
    }

    fn frame(&self) -> f64 {
        1.0 / self.fps
    }

    /// The picked clips whose pictures were moved by hand get the anchor of where they are now.
    fn anchor_picked(&mut self) {
        let outline = self.sequence.output.outline();
        for id in self.selection.ids(&self.sequence) {
            if self.sequence.tracks[id.track].kind != TrackKind::Video {
                continue;
            }
            let Some([w, h]) = self.size_of(self.sequence.clip(id).source) else { continue };
            let t = &mut self.sequence.clip_mut(id).transform;
            *t = t.anchored(w, h, outline);
        }
    }

    /// The output's proportions were those of `before`: the pictures that stay against a point of the
    /// frame move to where that is now. All but `except`, whose frame is being dragged into them.
    fn follow_output(&mut self, before: Output, except: Option<ClipId>) {
        if before.outline() == self.sequence.output.outline() {
            return;
        }
        let kept = except.map(|id| (id, self.sequence.clip(id).transform));
        self.sequence.keep_anchors(before.outline(), &sizes(&self.library));
        if let Some((id, transform)) = kept {
            self.sequence.clip_mut(id).transform = transform;
        }
    }

    /// Start of the frame shown at `t`: where I, O and the trim keys put an edge.
    fn frame_start(&self, t: f64) -> f64 {
        timecode::frame_index(t, self.fps) as f64 / self.fps
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

    /// The picture size of a source, once its file has been read; none for sound only.
    fn size_of(&self, source: SourceId) -> Option<[f64; 2]> {
        sizes(&self.library)(source)
    }

    /// Has the sound of every clip heard at another speed made for playing, or kept: of the whole
    /// clip, or of a part of it in slow motion.
    fn keep_stretched(&mut self, ctx: &egui::Context, now: f64) {
        let mut wanted: Vec<(Part, Pcm)> = Vec::new();
        for (_, clip) in self.sequence.clips().filter(|(id, _)| self.sequence.sounds(*id)) {
            let Some(cache) = self.library.cache(clip.source).filter(|cache| cache.info.has_audio) else { continue };
            for stretch in stretches(clip).into_iter().filter(|stretch| stretch.rate != 1.0) {
                wanted.push((stretch.part(clip), cache.audio.clone()));
            }
        }
        if let Some(wait) = self.stretcher.keep(&wanted, now) {
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }

    fn play_range(&self, mode: PlayMode) -> PlayRange {
        let mut sounds: Vec<Sound> = Vec::new();
        for (_, clip) in self.sequence.clips().filter(|(id, _)| self.sequence.sounds(*id)) {
            let (fade_in, fade_out) = clip.fade_lengths();
            let fades = |edge| clip.fade(edge).is_some_and(|fade| fade.sound);
            for stretch in stretches(clip) {
                // At another speed, its sound is made for it: that sound starts where the stretch does.
                let found = if stretch.rate == 1.0 {
                    self.library.cache(clip.source).map(|cache| (cache.audio.clone(), stretch.from))
                } else {
                    self.stretcher.sound(&stretch.part(clip))
                };
                let Some((pcm, source_start)) = found else { continue };
                // Where the clip plays its source, not where it holds a frame: silent then.
                for run in &stretch.runs {
                    let segment = Segment {
                        start: run.span.start,
                        end: run.span.end,
                        source_start: source_start + (run.source - stretch.from) / stretch.rate,
                        gain: clip.gain() as f32,
                        fade_in: if fades(Edge::In) { fade_in } else { 0.0 },
                        fade_out: if fades(Edge::Out) { fade_out } else { 0.0 },
                        curves: (clip.fade_curve(Edge::In), clip.fade_curve(Edge::Out)),
                        edges: Some((clip.start, clip.end())),
                    };
                    sounds.push(Sound { segment, pcm: pcm.clone() });
                }
            }
        }
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
        // The player plays the timeline now, not the viewer in Sources.
        self.sources_panel.viewer.playing = false;
    }

    /// Stops the player: the timeline, or the viewer in Sources, whichever plays.
    fn pause(&mut self) {
        self.player.pause();
        self.playing = false;
        self.sources_panel.viewer.playing = false;
    }

    /// What the viewer in Sources plays: all of a source, its sound as it is.
    fn source_range(&self, id: SourceId) -> Option<PlayRange> {
        let info = self.library.info(id).filter(|info| !info.image)?;
        let sounds: Arc<[Sound]> = match self.library.cache(id).filter(|cache| cache.info.has_audio) {
            Some(cache) => {
                let segment = Segment { start: 0.0, end: info.duration, gain: 1.0, ..Segment::default() };
                Arc::from([Sound { segment, pcm: cache.audio.clone() }])
            }
            None => Arc::from([]),
        };
        Some(PlayRange { start: 0.0, end: info.duration, looping: false, fades: None, sounds })
    }

    /// Plays the chosen source in the viewer from where it is, from its start once it has reached
    /// its end; the timeline waits.
    fn play_source(&mut self, id: SourceId) {
        let Some(range) = self.source_range(id) else { return };
        self.pause();
        let viewer = &mut self.sources_panel.viewer;
        if viewer.position >= range.end - 1.0 / self.fps {
            viewer.position = 0.0;
        }
        self.player.play(viewer.position, range);
        viewer.playing = true;
    }

    fn pause_source(&mut self) {
        if self.sources_panel.viewer.playing {
            self.pause();
        }
    }

    /// The viewer goes to `t` of the chosen source, and plays on from there if it played.
    fn seek_source(&mut self, id: SourceId, t: f64) {
        let Some(range) = self.source_range(id) else { return };
        let viewer = &mut self.sources_panel.viewer;
        viewer.position = t.clamp(0.0, range.end);
        if viewer.playing {
            self.player.play(viewer.position, range);
        }
    }

    /// While Sources is open, Space, K, L and the arrows are the viewer's: they play the chosen
    /// source and step through it, and the timeline stays as it is.
    fn viewer_keys(&mut self, ctx: &egui::Context) {
        let pressed = |modifiers: Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(modifiers, key));
        let (none, shift) = (Modifiers::NONE, Modifiers::SHIFT);
        let (space, k, l) = (pressed(none, Key::Space), pressed(none, Key::K), pressed(none, Key::L));
        let left = if pressed(shift, Key::ArrowLeft) { Some(-1.0) } else { pressed(none, Key::ArrowLeft).then_some(0.0) };
        let right = if pressed(shift, Key::ArrowRight) { Some(1.0) } else { pressed(none, Key::ArrowRight).then_some(0.0) };
        let Some(id) = self.sources_panel.chosen() else { return };
        let Some(fps) = self.library.info(id).filter(|info| !info.image).map(|info| self.library.cache(id).map_or(info.fps.as_f64(), |cache| cache.format.fps.as_f64())) else { return };
        let playing = self.sources_panel.viewer.playing;
        if (space && playing) || k {
            self.pause_source();
        } else if (space || l) && !playing {
            self.play_source(id);
        }
        // A second with Shift, else a frame.
        let position = self.sources_panel.viewer.position;
        let step = |seconds: f64, frames: i64| if seconds != 0.0 { position + seconds } else { (timecode::frame_index(position, fps) + frames) as f64 / fps };
        if let Some(seconds) = left {
            self.seek_source(id, step(seconds, -1));
        }
        if let Some(seconds) = right {
            self.seek_source(id, step(seconds, 1));
        }
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
        if let Some(id) = self.selected() {
            self.sequence.trim_start(id, t, frame);
        }
    }

    fn trim_end(&mut self, t: f64) {
        let frame = self.frame();
        if let Some(id) = self.selected() {
            self.sequence.trim_end(id, t, frame);
        }
    }

    /// Cuts the selected clip at the playhead, or if it is not there, the clip seen there. The part
    /// after the cut is selected.
    fn split_at_playhead(&mut self) {
        let t = self.frame_start(self.position);
        let target = self.selected().filter(|id| self.sequence.clip(*id).span().contains(t)).or_else(|| self.sequence.shown_at(t));
        if let Some(part) = target.and_then(|id| self.sequence.split(id, t, self.frame())) {
            self.pause();
            self.selection.pick(&self.sequence, Some(part));
        }
    }

    /// Holds the frame seen at the playhead for `FREEZE_LENGTH`: the selected clip's if it is there
    /// and plays a recording, else the one of the clip seen there.
    fn freeze_at_playhead(&mut self) {
        let t = self.frame_start(self.position);
        let moving = |id: &ClipId| {
            let clip = self.sequence.clip(*id);
            self.sequence.tracks[id.track].kind == TrackKind::Video && clip.still.is_none() && clip.span().contains(t)
        };
        let target = self.selected().filter(&moving).or_else(|| self.sequence.shown_at(t).filter(&moving));
        if let Some(id) = target {
            self.freeze(id);
        }
    }

    /// Holds the frame of clip `id` at the playhead for `FREEZE_LENGTH`: the clip is the main one
    /// picked. When the clips after it can't move later, its Timing says why.
    fn freeze(&mut self, id: ClipId) {
        let t = self.frame_start(self.position);
        self.pause();
        self.selection.focus(&self.sequence, id);
        if !self.sequence.freeze(id, t, FREEZE_LENGTH) {
            self.freeze_refused = Some(id);
        }
    }

    /// Plays a second of the recording from the playhead half as fast: the selected clip's if it is
    /// there and plays a recording, a sound clip's too, else the one of the clip seen there.
    fn slow_motion_at_playhead(&mut self) {
        let t = self.frame_start(self.position);
        let moving = |id: &ClipId| {
            let clip = self.sequence.clip(*id);
            clip.still.is_none() && clip.span().contains(t)
        };
        let target = self.selected().filter(&moving).or_else(|| self.sequence.shown_at(t).filter(&moving));
        if let Some(id) = target {
            self.slow_motion(id);
        }
    }

    /// Plays `SLOW_MOTION_SECONDS` of clip `id`'s recording from the playhead at `SLOW_MOTION_FACTOR`:
    /// the clip is the main one picked. When it can't, its Timing says why.
    fn slow_motion(&mut self, id: ClipId) {
        let t = self.frame_start(self.position);
        self.pause();
        self.selection.focus(&self.sequence, id);
        if !self.sequence.slow_motion(id, t, SLOW_MOTION_SECONDS, SLOW_MOTION_FACTOR) {
            let clip = self.sequence.clip(id);
            let why = if clip.slowed().iter().any(|part| part.span.contains(t)) {
                SlowRefused::InOne
            } else if clip.source_time(t) >= clip.source_out - 1e-6 {
                SlowRefused::AtEnd
            } else {
                SlowRefused::NoRoom
            };
            self.slow_refused = Some((id, why));
        }
    }

    /// Deletes the picked clips.
    fn delete_selected(&mut self) {
        let ids = self.selection.ids(&self.sequence);
        if !ids.is_empty() {
            self.pause();
            self.sequence.remove_clips(&ids);
            self.selection.clear();
        }
    }

    /// Deletes the picked clips and closes the gaps they leave on their tracks. No key does it for
    /// now (on Windows Shift+Delete comes as cutting): which one is to be settled later.
    #[allow(dead_code)]
    fn ripple_delete_selected(&mut self) {
        let ids = self.selection.ids(&self.sequence);
        if !ids.is_empty() {
            self.pause();
            self.sequence.ripple_delete_clips(&ids);
            self.selection.clear();
        }
    }

    /// Copies of the picked clips right after them, which are picked then: of several, as far apart
    /// as they are, after the last of them.
    fn duplicate_selected(&mut self) {
        let ids = self.selection.ids(&self.sequence);
        match ids[..] {
            [] => {}
            [id] => {
                self.pause();
                let copy = self.sequence.duplicate(id);
                self.selection.pick(&self.sequence, Some(copy));
            }
            _ => {
                self.pause();
                let copies = self.sequence.duplicate_clips(&ids);
                self.selection.set(&self.sequence, &copies);
            }
        }
    }

    /// Keeps the picked clips for pasting, and says so in the system's clipboard.
    fn copy_selected(&mut self, ctx: &egui::Context) {
        let copied = self.sequence.copied(&self.selection.ids(&self.sequence));
        let note = match &copied[..] {
            [] => return,
            [(clip, _, _)] => format!("DicingSplicing clip: {} ({})", clip.name, timecode::format(clip.span().len())),
            many => format!("DicingSplicing clips: {}", many.len()),
        };
        ctx.copy_text(note.clone());
        self.clipboard = Some((copied, note));
    }

    /// Keeps the picked clips for pasting, and deletes them.
    fn cut_selected(&mut self, ctx: &egui::Context) {
        self.copy_selected(ctx);
        self.delete_selected();
    }

    /// Pastes the copied clips, if the clipboard still has what copying put there, and picks them: at
    /// the playhead, or given `pointer` (Ctrl+Shift+V), at its time. One goes onto the track under
    /// the pointer, else the main picked clip's, else the one it was copied from, the first of them
    /// of its kind; several each onto the one it came from, or with the pointer over a track, those
    /// of its kind from there up, as far apart as they were. Where a track has no room then, the
    /// nearest one above that has takes them.
    fn paste(&mut self, text: &str, pointer: Option<(f64, Option<usize>)>) {
        let Some((copied, _)) = self.clipboard.as_ref().filter(|(_, note)| note == text) else { return };
        let copied = copied.clone();
        let at = pointer.map_or_else(|| self.frame_start(self.position), |(t, _)| t);
        let under = pointer.and_then(|(_, track)| track);
        self.pause();
        match &copied[..] {
            [(clip, from, kind)] => {
                let tracks = &self.sequence.tracks;
                let of_kind = |track: &usize| tracks.get(*track).is_some_and(|it| it.kind == *kind);
                let track = under
                    .filter(of_kind)
                    .or_else(|| self.selected().map(|id| id.track).filter(of_kind))
                    .or_else(|| tracks.iter().position(|it| it.id == *from));
                let pasted = self.sequence.paste(clip.clone(), at, track, *kind);
                self.selection.pick(&self.sequence, Some(pasted));
            }
            _ => {
                let pasted = self.sequence.paste_clips(&copied, at, under);
                self.selection.set(&self.sequence, &pasted);
            }
        }
    }

    /// Does `change` to clip `id` and to the other picked clips, to all of them or to none: false,
    /// with nothing changed, if it can't be done to one of them.
    fn change_picked(&mut self, id: ClipId, change: impl Fn(&mut Sequence, ClipId) -> bool) -> bool {
        let others = self.selection.others(&self.sequence).into_iter().filter(|&other| other != id);
        let keys: Vec<ClipKey> = std::iter::once(id).chain(others).map(|clip| self.sequence.clip(clip).key()).collect();
        let mut next = self.sequence.clone();
        for key in keys {
            if let Some(clip) = next.find(key) {
                if !change(&mut next, clip) {
                    return false;
                }
            }
        }
        self.sequence = next;
        true
    }

    /// The frame of clip `id`, which was `before`, set so in the other picked clips that are seen:
    /// what changed, each keeping the rest of its own.
    fn carry_transform(&mut self, id: ClipId, before: Transform) {
        let after = self.sequence.clip(id).clone();
        if after.transform == before {
            return;
        }
        let was = Clip { transform: before, ..after.clone() };
        for other in self.selection.ids(&self.sequence).into_iter().filter(|&other| other != id) {
            if self.sequence.tracks[other.track].kind == TrackKind::Video {
                self.sequence.clip_mut(other).take_changes(&was, &after, true);
            }
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
    /// there already is not added again. The list opens to show it. Returns whether it is new.
    pub fn add_source(&mut self, path: PathBuf, name: Option<String>, meta: BTreeMap<String, String>) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        let (id, new) = project::add_source(&mut self.sources, path.clone(), name, meta);
        if new {
            self.library.add(id, &path);
        }
        self.sources_panel.note = None;
        // The list opens to show it, and the viewer shows it.
        self.show_sources = true;
        self.sources_panel.choose(id);
        new
    }

    /// Adds files to the sources at once, in the order given. The list says how many of them were
    /// there already.
    pub fn add_sources(&mut self, paths: Vec<PathBuf>) {
        let total = paths.len();
        let mut already = 0;
        for path in paths {
            if !self.add_source(path, None, BTreeMap::new()) {
                already += 1;
            }
        }
        self.sources_panel.note = sources::already_note(total, already);
    }

    /// Puts the whole source on a new track under the others, after the last clip, and goes there.
    fn add_to_timeline(&mut self, id: SourceId) {
        let (Some(source), Some(info)) = (self.sources.iter().find(|source| source.id == id), self.library.info(id)) else { return };
        let clip = Clip::whole(id, source.label(), info);
        // A file of sound only goes onto an audio track.
        let kind = if info.has_video() { TrackKind::Video } else { TrackKind::Audio };
        let added = self.sequence.add_below(clip, kind);
        self.selection.pick(&self.sequence, Some(added));
        self.seek(self.sequence.clip(added).start);
    }

    /// The source's file is somewhere else now.
    fn locate(&mut self, id: SourceId) {
        let Some(path) = pick_media() else { return };
        let Some(source) = self.sources.iter_mut().find(|source| source.id == id) else { return };
        source.path = path;
        self.apart.forget(id);
        self.sources_panel.viewer.forget(id);
        for picture in self.layers.values_mut() {
            picture.forget(id);
        }
        self.library.add(id, &source.path);
    }

    fn remove_source(&mut self, id: SourceId) {
        if self.sequence.uses(id) > 0 {
            return;
        }
        if let Some(i) = self.sources.iter().position(|source| source.id == id) {
            self.removed.push(self.sources.remove(i));
            self.library.remove(id);
            self.apart.forget(id);
            self.sources_panel.viewer.forget(id);
            for picture in self.layers.values_mut() {
                picture.forget(id);
            }
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
        if self.sources_panel.viewer.playing {
            let (t, still_playing) = self.player.poll();
            self.sources_panel.viewer.position = t;
            self.sources_panel.viewer.playing = still_playing;
            ui.ctx().request_repaint();
        }
        // Taken before the list is drawn, so that a button there that has the focus does not take
        // Space as well.
        if self.sources_panel.open && !ui.ctx().wants_keyboard_input() {
            self.viewer_keys(ui.ctx());
        }
        let rows: Vec<SourceRow> = self
            .sources
            .iter()
            .map(|source| SourceRow {
                source,
                state: self.library.state(source.id),
                info: self.library.info(source.id),
                uses: self.sequence.uses(source.id),
                cache: self.library.cache(source.id),
                sound: self.library.sound_graph(source.id),
                parts: self.sequence.used_parts(source.id),
            })
            .collect();
        let action = sources::sources_button(ui, &rows, plugins, std::mem::take(&mut self.show_sources), &mut self.sources_panel);
        // Sources closed: its viewer stops.
        if !self.sources_panel.open {
            self.pause_source();
        }
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
                let files = pick_media_files();
                if !files.is_empty() {
                    self.add_sources(files);
                }
            }
            SourceAction::Plugin(i) => return Some(i),
            SourceAction::PlaySource(id) => self.play_source(id),
            SourceAction::PauseSource => self.pause_source(),
            SourceAction::SeekSource(id, t) => self.seek_source(id, t),
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
        if pressed(Modifiers::COMMAND, Key::D) {
            self.duplicate_selected();
        }
        // With Shift first: without it, Ctrl+A would take Ctrl+Shift+A too.
        if pressed(Modifiers::COMMAND | shift, Key::A) {
            self.selection.clear();
        } else if pressed(Modifiers::COMMAND, Key::A) {
            self.selection.all(&self.sequence);
        }
        // Copying, cutting and pasting come as events of their own, not as keys; pasting only when
        // the system's clipboard has text.
        let copied = ctx.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Copy)));
        let cut = ctx.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Cut)));
        let pasted = ctx.input(|i| i.events.iter().find_map(|event| if let egui::Event::Paste(text) = event { Some(text.clone()) } else { None }));
        if copied {
            self.copy_selected(ctx);
        }
        if cut {
            self.pause();
            self.cut_selected(ctx);
        }
        if let Some(text) = pasted {
            // Ctrl+Shift+V: where the mouse pointer is over the timeline, elsewhere at the playhead.
            // Shift+Insert, the old Windows key for pasting, comes without Ctrl.
            let at_pointer = ctx.input(|i| i.modifiers.command && i.modifiers.shift);
            self.paste(&text, self.timeline.under_pointer.filter(|_| at_pointer));
        }
        if pressed(Modifiers::COMMAND, Key::B) {
            self.split_at_playhead();
        } else if pressed(none, Key::B) {
            self.timeline.tool = Tool::Blade;
        }
        if pressed(none, Key::F) {
            self.freeze_at_playhead();
        }
        if pressed(none, Key::S) {
            self.slow_motion_at_playhead();
        }
        // In the preview: the selected picture's menu round the pointer, and what the wheel does.
        if pressed(none, Key::Q) {
            self.canvas.toggle_menu();
        }
        if pressed(none, Key::Z) {
            self.canvas.toggle_wheel();
        }
        if pressed(none, Key::V) || pressed(none, Key::Escape) {
            self.timeline.tool = Tool::Select;
            self.timeline.cancel_chaining();
            self.canvas.close_menu();
        }
    }

    fn timeline_panel(&mut self, ctx: &egui::Context) {
        let fps = self.fps;
        let frame = self.frame();
        let missing = self.missing();
        let placeable: BTreeMap<SourceId, (Clip, Has)> = self
            .sources
            .iter()
            .filter_map(|source| {
                let info = self.library.info(source.id)?;
                Some((source.id, (Clip::whole(source.id, source.label(), info), Has { picture: info.has_video(), sound: info.has_audio })))
            })
            .collect();
        // The graph of each source's sound: none for a video without sound.
        let sounds: BTreeMap<SourceId, SourceSound> = self
            .sources
            .iter()
            .filter_map(|source| {
                let info = self.library.info(source.id)?;
                let sound = if info.has_audio { SourceSound::Graph(self.library.sound_graph(source.id)?) } else { SourceSound::Silent };
                Some((source.id, sound))
            })
            .collect();
        // As tall as its border was dragged to, leaving the pictures above it some room.
        let most = (ctx.available_rect().height() - PREVIEW_MIN_HEIGHT).max(TIMELINE_MIN_HEIGHT);
        let height = self.timeline_height.unwrap_or(TIMELINE_FIRST_HEIGHT).clamp(TIMELINE_MIN_HEIGHT, most);
        egui::TopBottomPanel::bottom("timeline").exact_height(height).show_separator_line(false).show(ctx, |ui| {
            if let Some(dragged) = panel_border(ui) {
                self.timeline_height = Some((height - dragged).clamp(TIMELINE_MIN_HEIGHT, most));
            }
            // The buttons at the bottom; the timeline takes all the rest.
            egui::TopBottomPanel::bottom("timeline buttons").frame(egui::Frame::none()).show_separator_line(false).show_inside(ui, |ui| {
                ui.add_space(4.0);

                // Every time shown is time in the file: it counts from the export start.
                let zero = self.sequence.export.start;

                // Wraps when the timeline is narrow: the right column takes the window's whole height.
                ui.horizontal_wrapped(|ui| {
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
                    ui.separator();
                    let select = ui.selectable_value(&mut self.timeline.tool, Tool::Select, "Select (V)");
                    select.on_hover_text("A click picks a clip; a clip is dragged by its body, trimmed by its edges");
                    let blade = ui.selectable_value(&mut self.timeline.tool, Tool::Blade, "Blade (B)");
                    blade.on_hover_text("A click on a clip cuts it there; with Shift every clip there. Esc goes back to Select");
                    if ui.button("Split (Ctrl+B)").on_hover_text("Cut the selected clip at the playhead, or the clip seen there").clicked() {
                        self.split_at_playhead();
                    }
                    let freeze = ui.button("Freeze frame (F)").on_hover_text(format!(
                        "Hold the frame at the playhead for {FREEZE_LENGTH} s, of the selected clip or the one seen there: \
                         the rest of it and the clips after it on its track move later"
                    ));
                    if freeze.clicked() {
                        self.freeze_at_playhead();
                    }
                    let slow = ui.button("Tempo (S)").on_hover_text(
                        "Play a second of the recording from the playhead at a tempo of its own, of the selected clip or the one seen \
                         there, its sound at its pitch: half as fast to start with, and its block in Effects makes it slower or faster. \
                         The rest of the clip and the clips after it on its track move along",
                    );
                    if slow.clicked() {
                        self.slow_motion_at_playhead();
                    }
                });

                let at_playhead = self.frame_start(self.position);
                ui.horizontal(|ui| {
                    // The export starts at the zero by definition, so In and Out would be 0 and the length.
                    let export = self.sequence.export;
                    ui.label(RichText::new("Export").strong());
                    if ui.button("Set In (I)").on_hover_text("Start the export at the playhead").clicked() {
                        self.set_export_start(at_playhead);
                    }
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
                });
                ui.add_space(6.0);
            });
            egui::CentralPanel::default().frame(egui::Frame::none()).show_inside(ui, |ui| {
                let env = TimelineEnv { fps, missing: &missing, placeable: &placeable, sounds: &sounds };
                let response = self.timeline.show(ui, &mut self.position, &mut self.sequence, &mut self.selection, &env);
                if (response.seeked || response.edit_started) && self.playing {
                    self.pause();
                }
                self.peek = response.peek;
                if response.refused.is_some() {
                    self.effect_refused = response.refused;
                }
                if let Some(id) = response.freeze {
                    self.freeze(id);
                }
                if let Some(id) = response.slow {
                    self.slow_motion(id);
                }
                if let Some(section) = response.open {
                    self.flash = Some((section, ui.input(|i| i.time)));
                    self.scroll_to = Some(section);
                    self.side_tab = SideTab::Clip;
                    ui.ctx().request_repaint();
                }
            });
        });
    }

    fn side_panel(&mut self, ctx: &egui::Context, plugins: &[Plugin], workspace: Option<&Workspace>, settings: &Settings) {
        let shown = self.framing.shown_presets(self.sequence.output);
        // The whole height of the window, one tab at a time. It keeps the name it had when it showed
        // the result: egui remembers its width by it.
        egui::SidePanel::right("output").resizable(true).default_width(SIDE_PANEL_WIDTH).min_width(SIDE_PANEL_MIN_WIDTH).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.side_tab, SideTab::Clip, RichText::new("Clip").size(TAB_TEXT_SIZE));
                ui.selectable_value(&mut self.side_tab, SideTab::Export, RichText::new(self.export.tab_text()).size(TAB_TEXT_SIZE));
            });
            ui.separator();
            match self.side_tab {
                SideTab::Clip => {
                    egui::ScrollArea::vertical().id_salt("clip tab").auto_shrink([false, false]).show(ui, |ui| self.clip_tab(ui));
                }
                SideTab::Export => {
                    // What the plugins are told about the result: the video it is mostly made of.
                    let no_meta = BTreeMap::new();
                    let main = self.sequence.main_source(&sizes(&self.library));
                    let meta = self.sources.iter().find(|source| Some(source.id) == main).map_or(&no_meta, |source| &source.meta);
                    let publish = workspace.map(|workspace| PublishEnv { workspace, settings, name: &self.name, meta });
                    let env = ExportEnv { files: &self.files, shown: &shown, position: self.position, plugins, publish };
                    let before = self.sequence.output;
                    self.export.show(ui, &mut self.sequence, &env);
                    self.follow_output(before, None);
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

    /// The preview's pictures at the playhead, the lowest first, each with the clip it is of, and the
    /// selected clip's on top when it is not seen there. A clip whose frame is not there yet shows
    /// what its track showed last, while that frame is just being decoded: not while its video's
    /// frames are still being prepared, when the old picture would stand for another clip.
    fn preview_layers(&mut self) -> Vec<(Layer, Option<ClipId>)> {
        let t = self.shown_time();
        let stack = self.sequence.stack_at(t, &sizes(&self.library));
        let mut layers = Vec::new();
        for &id in &stack {
            let clip = self.sequence.clip(id);
            let track = self.sequence.tracks[id.track].id;
            let (opacity, size) = (clip.opacity(t) as f32, self.size_of(clip.source));
            if let (Some(texture), Some(size)) = (self.layers.get(&track).and_then(|picture| picture.of(clip.source)).cloned(), size) {
                self.drawn.insert(track, Drawn { source: clip.source, transform: clip.transform, size });
                layers.push((Layer { texture: Some(texture), size, transform: clip.transform, opacity, live: true, apart: None, key: Some(clip.key()) }, Some(id)));
                continue;
            }
            let ready = self.library.cache(clip.source).is_some_and(|cache| cache.frames.get(frame_at(cache, clip.picture_time(t))).is_some());
            let kept = self.drawn.get(&track).copied().filter(|_| ready).and_then(|drawn| {
                let texture = self.layers.get(&track).and_then(|picture| picture.of(drawn.source))?;
                Some((drawn, texture.clone()))
            });
            layers.push(match kept {
                Some((drawn, texture)) => (Layer { texture: Some(texture), size: drawn.size, transform: drawn.transform, opacity, live: false, apart: None, key: None }, None),
                None => (Layer { texture: None, size: size.unwrap_or(NO_SIZE), transform: clip.transform, opacity, live: true, apart: None, key: Some(clip.key()) }, Some(id)),
            });
        }
        if let Some((id, note)) = self.apart_clip(&stack) {
            let clip = self.sequence.clip(id);
            let texture = self.apart.of(clip.source).cloned();
            let size = self.size_of(clip.source).unwrap_or(NO_SIZE);
            layers.push((Layer { texture, size, transform: clip.transform, opacity: 1.0, live: true, apart: Some(note), key: Some(clip.key()) }, Some(id)));
        }
        layers
    }

    /// The main picked clip: where it is, its frame, its sound, its effects and its source. What is
    /// set here, other than its timing, goes to the other picked clips too.
    fn clip_tab(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected() else {
            ui.label(RichText::new("Select a clip on the timeline to set its timing, frame, sound and effects.").weak());
            return;
        };
        // Every time shown is time in the file: it counts from the export start.
        let zero = self.sequence.export.start;
        let (fps, at_playhead) = (self.fps, self.frame_start(self.position));
        let clip = self.sequence.clip(id);
        let (span, source_in, source_out, name, speed, still) = (clip.span(), clip.source_in, clip.source_out, clip.name.clone(), clip.speed(), clip.still);
        let (holds, slowed) = (clip.holds(), clip.slowed());
        let image = clip.is_image();
        let track = track_name(&self.sequence, id.track);
        let kind = self.sequence.tracks[id.track].kind;
        let detached = self.sequence.clip(id).sound_detached;
        let others: Vec<ClipKey> = self.selection.others(&self.sequence).into_iter().map(|other| self.sequence.clip(other).key()).collect();
        ui.label(RichText::new(name).strong());
        ui.label(RichText::new(format!("{track} · {} to {}", timecode::format(span.start - zero), timecode::format(span.end - zero))).weak());
        if !others.is_empty() {
            let note = format!("{} clips selected: what is set here goes to all of them, Timing to this one only.", others.len() + 1);
            ui.label(RichText::new(note).weak().small());
        }
        ui.separator();

        ui.heading("Timing");
        egui::Grid::new("timing").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Start");
            let mut start = span.start - zero;
            if ui.add(time_value(&mut start, fps)).on_hover_text("Where the clip starts in the file. Changing it trims the clip").changed() {
                self.trim_start(zero + start);
            }
            if ui.button("Set ([)").on_hover_text("Trim the clip's start to the playhead").clicked() {
                self.trim_start(at_playhead);
            }
            ui.end_row();
            ui.label("End");
            let mut end = span.end - zero;
            if ui.add(time_value(&mut end, fps)).on_hover_text("Where the clip ends in the file. Changing it trims the clip").changed() {
                self.trim_end(zero + end);
            }
            if ui.button("Set (])").on_hover_text("Trim the clip's end to the playhead").clicked() {
                self.trim_end(at_playhead);
            }
            ui.end_row();
            ui.label("Length");
            let mut length = span.len();
            // A still is as long as it is made: the clips after it make room, or take what it gave up.
            if still.is_some() {
                let field = ui.add(time_value(&mut length, fps)).on_hover_text("How long it lasts: the clips after it on its track move along");
                if field.changed() {
                    let done = self.sequence.set_length(id, length.max(self.frame()));
                    self.length_refused = (!done).then_some(id);
                }
            } else if ui.add(time_value(&mut length, fps)).changed() {
                self.trim_end(span.start + length);
            }
            ui.end_row();
        });
        let from = match still {
            Some(_) if image => "Its image all along, as long as you make it. Delete removes the clip.".to_owned(),
            Some(at) => format!("The frame of its source at {} all along. Delete removes the clip.", timecode::format(at)),
            None => {
                let mut at = if speed == 1.0 { String::new() } else { format!(", at {}", speed_text(speed)) };
                // Its frames held make it longer than that part.
                let held: f64 = holds.iter().map(|hold| hold.span.len()).sum();
                match holds.len() {
                    0 => {}
                    1 => at += &format!(", a frame of it held {}", timecode::format(held)),
                    count => at += &format!(", {count} frames of it held {} in all", timecode::format(held)),
                }
                // Some of it at a speed of its own.
                match slowed.as_slice() {
                    [] => {}
                    [part] => at += &format!(", {} to {} of it at {}", timecode::format(part.from), timecode::format(part.to), speed_text(part.rate)),
                    parts => at += &format!(", {} parts of it at a tempo of their own", parts.len()),
                }
                format!("From its source {} to {}{at}. Delete removes the clip.", timecode::format(source_in), timecode::format(source_out))
            }
        };
        ui.label(RichText::new(from).weak().small());
        if self.length_refused == Some(id) {
            ui.label(RichText::new("The clips after it can't move that far: one chained to them has no room.").color(ERROR).small());
        }
        if self.freeze_refused == Some(id) {
            let why = format!("No freeze frame: the clips after the playhead can't move {FREEZE_LENGTH} s later, one chained to them has no room.");
            ui.label(RichText::new(why).color(ERROR).small());
        }
        if let Some((_, why)) = self.slow_refused.filter(|(clip, _)| *clip == id) {
            let why = match why {
                SlowRefused::InOne => "No new Tempo: the playhead is on a part at a tempo of its own already. Its block in Effects changes it.",
                SlowRefused::AtEnd => "No new Tempo: its recording ends at the playhead.",
                SlowRefused::NoRoom => "No new Tempo: the clips after the playhead can't move that much later, one chained to them has no room.",
            };
            ui.label(RichText::new(why).color(ERROR).small());
        }
        ui.separator();

        // From here on, what is set goes to the other picked clips too.
        let before = self.sequence.clip(id).clone();
        let source = self.sequence.clip(id).source;
        let info = self.library.info(source).cloned();
        let flash_of = |section: Section| self.flash.filter(|(flashed, _)| *flashed == section).map(|(_, started)| started);
        let (frame_flash, sound_flash, effects_flash) = (flash_of(Section::Frame), flash_of(Section::Sound), flash_of(Section::Effects));
        // A sound clip is not seen: nothing to frame.
        if kind == TrackKind::Video {
        let outline = self.sequence.output.outline();
        let pixels = self.sequence.settings.pixels(self.sequence.output).map(f64::from);
        let size = self.size_of(source);
        let t = &mut self.sequence.clip_mut(id).transform;
        let frame_section = flash::frame(ui, frame_flash, false).show(ui, |ui| {
            ui.heading("Transform");
            match size {
                Some(size) => transform_editor(ui, t, size, outline, pixels),
                None => {
                    ui.label(RichText::new("Its place on the frame is here once its video is open.").weak());
                }
            }
            ui.label(
                RichText::new(
                    "In the preview: drag the picture to move it (Alt: no snapping). A corner or the middle \
                     of a side resizes it, the opposite one staying; an arrow at a corner turns it (Shift: \
                     15° steps). The dot above it: rest on it for Fit, Fill, Bars and Reset, or press Q for \
                     them round the pointer; click it for 0°. The wheel changes its Scale from its anchor, or \
                     zooms the view when switched to Zoom (Z). Ctrl+wheel or a pinch zooms the view, the \
                     right or the middle button moves it.",
                )
                .weak()
                .small(),
            );
        });
        if self.scroll_to == Some(Section::Frame) {
            frame_section.response.scroll_to_me(Some(egui::Align::Center));
            self.scroll_to = None;
        }
        ui.separator();
        }

        // A picture whose sound is a clip of its own has no sound here: the other clip has it.
        if detached {
            let partner = self.sequence.group_of(id).into_iter().find(|&other| self.sequence.tracks[other.track].kind == TrackKind::Audio);
            ui.heading("Sound");
            let text = match partner {
                Some(other) => format!("Detached: it is a clip of its own on {}.", track_name(&self.sequence, other.track)),
                None => "Detached: its sound is a clip of its own.".to_owned(),
            };
            ui.label(RichText::new(text).weak());
            ui.separator();
        } else if info.as_ref().is_some_and(|info| info.has_audio) && still.is_none() {
            let clip = self.sequence.clip_mut(id);
            let sound_section = flash::frame(ui, sound_flash, false).show(ui, |ui| {
                ui.heading("Sound");
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
            });
            if self.scroll_to == Some(Section::Sound) {
                sound_section.response.scroll_to_me(Some(egui::Align::Center));
                self.scroll_to = None;
            }
            ui.separator();
        }

        let refused = self.effect_refused.filter(|(clip, _)| *clip == id).map(|(_, effect)| effect);
        let frame = self.frame();
        let clip = self.sequence.clip_mut(id);
        // A still has no sound to fade.
        let (picture, sound) = (kind == TrackKind::Video, (kind == TrackKind::Audio || !detached) && still.is_none());
        let effects_section = flash::frame(ui, effects_flash, false).show(ui, |ui| effects_editor(ui, clip, picture, sound, refused, fps, frame));
        // A new speed or hold makes the clip longer or shorter, and the clips after it move along;
        // what is chained to it and holds the same frame changes with it.
        if let Some((effect, change)) = effects_section.inner {
            // A speed is set to all the picked clips, but the stills, which show one frame however
            // fast. The frames held and the slow motion are moments of this clip's own recording.
            let done = match change {
                EffectChange::Speed(factor) => self.change_picked(id, |sequence, clip| sequence.clip(clip).still.is_some() || sequence.set_speed(clip, factor)),
                EffectChange::SpeedOff => self.change_picked(id, |sequence, clip| sequence.remove_speed(clip)),
                EffectChange::FreezeFrame(at) => self.sequence.move_freeze(id, effect, at),
                EffectChange::FreezeLength(seconds) => self.sequence.set_freeze_length(id, effect, seconds),
                EffectChange::FreezeOff => self.sequence.remove_freeze(id, effect),
                EffectChange::SlowMotionSpeed(factor) => self.sequence.set_slow_motion_speed(id, effect, factor),
                EffectChange::SlowMotionPart(from, to) => self.sequence.move_slow_motion(id, effect, from, to),
                EffectChange::SlowMotionOff => self.sequence.remove_slow_motion(id, effect),
            };
            self.effect_refused = (!done).then_some((id, effect));
        }
        if self.scroll_to == Some(Section::Effects) {
            effects_section.response.scroll_to_me(Some(egui::Align::Center));
            self.scroll_to = None;
        }
        let after = self.sequence.clip(id).clone();
        if after != before {
            for other in self.sequence.find_all(&others) {
                let seen = self.sequence.tracks[other.track].kind == TrackKind::Video;
                self.sequence.clip_mut(other).take_changes(&before, &after, seen);
            }
        }
        ui.separator();

        ui.heading("Source");
        let label = self.sources.iter().find(|s| s.id == source).map_or_else(|| "removed from the project".to_owned(), Source::label);
        ui.label(RichText::new(label).strong());
        let Some(info) = info else {
            ui.label(RichText::new("Its file has not been read.").weak());
            return;
        };
        egui::Grid::new("source").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            if info.image {
                ui.label("Size");
                ui.label(format!("{}×{}", info.width, info.height));
                ui.end_row();
                ui.label("Picture");
                ui.label("an image: no length, no sound");
                ui.end_row();
                return;
            }
            if info.has_video() {
                ui.label("Size");
                ui.label(format!("{}×{}", info.width, info.height));
                ui.end_row();
                ui.label("Frame rate");
                match self.library.cache(source).map(|cache| cache.format.fps).filter(|preview| *preview != info.fps) {
                    Some(preview) => ui.label(format!("{:.2} fps (preview {:.2})", info.fps.as_f64(), preview.as_f64())),
                    None => ui.label(format!("{:.2} fps", info.fps.as_f64())),
                };
                ui.end_row();
            } else {
                ui.label("Picture");
                ui.label("none: sound only");
                ui.end_row();
            }
            ui.label("Duration");
            ui.label(timecode::format(info.duration));
            ui.end_row();
            ui.label("Audio");
            ui.label(if info.has_audio { "yes" } else { "no" });
            ui.end_row();
        });
    }

    /// The preview in the middle, with the frames menu, the proportions and Undo above it.
    fn preview_panel(&mut self, ctx: &egui::Context) {
        let (layers, ids): (Vec<Layer>, Vec<Option<ClipId>>) = self.preview_layers().into_iter().unzip();
        let selected = self.selected().and_then(|id| ids.iter().position(|&shown| shown == Some(id)));
        // The selected picture's proportions, for the button that gives them to the frame.
        let picture = self.selected().filter(|id| self.sequence.tracks[id.track].kind == TrackKind::Video);
        let size = picture.and_then(|id| self.size_of(self.sequence.clip(id).source)).filter(|[w, h]| *w > 0.0 && *h > 0.0);
        let aspect = size.map_or(NO_SIZE[0] / NO_SIZE[1], |[w, h]| w / h);
        let empty = self.preview_empty();
        let (mut undo, mut redo) = (false, false);
        egui::CentralPanel::default().frame(egui::Frame::none().fill(viewer::BACKGROUND)).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let before = self.sequence.output;
                self.framing.menu(ui, &mut self.sequence.output);
                self.framing.ratio_controls(ui, &mut self.sequence.output, aspect);
                self.follow_output(before, None);
                ui.separator();
                let can_undo = self.history.can_undo(&self.sequence);
                undo = ui.add_enabled(can_undo, egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked();
                let can_redo = self.history.can_redo(&self.sequence);
                redo = ui.add_enabled(can_redo, egui::Button::new("Redo")).on_hover_text("Ctrl+Y or Ctrl+Shift+Z").clicked();
                if let Some(error) = &self.player.audio_error {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(230, 120, 60), "No sound").on_hover_text(error.as_str());
                }
            });

            let output = self.sequence.output;
            let frames = self.framing.overlays(output);
            let [px_w, px_h] = self.sequence.settings.pixels(output);
            let caption = format!("{} · {} · {px_w}×{px_h}", output.preset().name, output.ratio().label());
            let pixels = [f64::from(px_w), f64::from(px_h)];
            let scene = Scene { layers: &layers, selected, frames: &frames, pixels, playing: self.playing, caption: &caption, empty: empty.as_deref() };
            let change = self.canvas.show(ui, &scene);
            if change.began && self.playing {
                self.pause();
            }
            if let Some(id) = change.picked.and_then(|i| ids[i]) {
                if self.selection.contains(&self.sequence, id) {
                    self.selection.focus(&self.sequence, id);
                } else {
                    self.selection.pick(&self.sequence, Some(id));
                }
            }
            if let Some((id, transform)) = change.changed.and_then(|(i, transform)| Some((ids[i]?, transform))) {
                let before = self.sequence.clip(id).transform;
                self.sequence.clip_mut(id).transform = transform;
                // What was done to it goes to the other picked clips that are seen too.
                self.carry_transform(id, before);
                self.anchor_pending |= change.moved;
            }
            if let Some(aspect) = change.aspect {
                // The pictures that stay against a point of the frame follow its new proportions.
                let before = self.sequence.output;
                self.framing.set_dragged_ratio(&mut self.sequence.output, aspect);
                self.follow_output(before, None);
            }
        });
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
    }

    /// What the preview says in the middle when there is nothing to see there.
    fn preview_empty(&self) -> Option<String> {
        if self.sequence.clips().next().is_none() {
            return Some("Add a video from Sources, then put it on the timeline".to_owned());
        }
        let id = self.selected().filter(|id| self.sequence.tracks[id.track].kind == TrackKind::Video)?;
        let source = self.sequence.clip(id).source;
        match self.library.state(source) {
            SourceState::Missing => Some("The file of this clip is not there. Sources: Locate file".to_owned()),
            SourceState::Failed(error) => Some(format!("The file of this clip can't be read: {error}")),
            _ if self.size_of(source).is_none() => Some("Opening the video…".to_owned()),
            _ => None,
        }
    }
}



/// The size of each source's picture, once its file has been read; none for sound only, which has
/// no picture to place.
fn sizes(library: &Library) -> impl Fn(SourceId) -> Option<[f64; 2]> + '_ {
    |source| library.info(source).filter(|info| info.has_video()).map(|info| [info.width as f64, info.height as f64])
}

/// The picture's place on the result, in the result's pixels, its anchor, scale, size and angle,
/// and quick ways to set them: for a picture `w`×`h` on a frame of `outline`, `pixels` large.
fn transform_editor(ui: &mut egui::Ui, t: &mut Transform, [w, h]: [f64; 2], outline: Outline, pixels: [f64; 2]) {
    let place = t.placement(w, h, outline, pixels);
    let mut offsets = place.offsets(t.anchor, pixels);
    let scale = t.picture_scale(w, h, outline, pixels);
    let (mut percent, mut size, mut angle) = (scale * 100.0, place.size, t.angle_deg);
    let (mut moved, mut factor, mut turned) = (false, None, false);
    egui::Grid::new("transform").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Anchor").on_hover_text(
            "Where the picture stays when the frame's proportions change, in other formats too, and what it grows from. \
             Moving it in the preview sets it to where it is",
        );
        ui.horizontal(|ui| {
            let frame = ui.add(egui::Button::new("Frame").selected(t.anchor.is_frame()));
            if frame.on_hover_text("It follows the frame: in other proportions the frame keeps its middle, as frames always did").clicked() {
                t.anchor = Anchor::Frame;
            }
            if let Some(anchor) = anchor_grid(ui, t.anchor) {
                t.anchor = anchor;
            }
        });
        ui.end_row();
        let sides = t.anchor.sides().unwrap_or([0.0, 0.0]);
        let names = [["Left", "X", "Right"], ["Top", "Y", "Bottom"]];
        let hints = [
            ["From the frame's left edge to the picture's", "From the frame's middle to the picture's, to the right", "From the frame's right edge to the picture's"],
            ["From the frame's top edge to the picture's", "From the frame's middle to the picture's, down", "From the frame's bottom edge to the picture's"],
        ];
        for axis in 0..2 {
            let side = (sides[axis] + 1.0) as usize;
            ui.label(names[axis][side]).on_hover_text(format!("{}, in pixels of the result", hints[axis][side]));
            moved |= ui.add(egui::DragValue::new(&mut offsets[axis]).speed(1.0).fixed_decimals(0).suffix(" px")).changed();
            ui.end_row();
        }
        ui.label("Scale").on_hover_text("How large the picture is against its own pixels: at 100% a pixel of it is a pixel of the result. It grows from its anchor");
        if ui.add(egui::DragValue::new(&mut percent).speed(0.5).range(0.1..=100_000.0).fixed_decimals(1).suffix(" %")).changed() {
            factor = Some(percent / (scale * 100.0));
        }
        ui.end_row();
        ui.label("Size").on_hover_text("Its width and height on the result, in its pixels, before it is turned. They change together: the picture keeps its proportions");
        ui.horizontal(|ui| {
            let width = ui.add(egui::DragValue::new(&mut size[0]).speed(1.0).range(1.0..=pixels[0] * 100.0).fixed_decimals(0)).on_hover_text("Width");
            ui.label("×");
            let height = ui.add(egui::DragValue::new(&mut size[1]).speed(1.0).range(1.0..=pixels[1] * 100.0).fixed_decimals(0).suffix(" px")).on_hover_text("Height");
            if width.changed() {
                factor = Some(size[0] / place.size[0]);
            } else if height.changed() {
                factor = Some(size[1] / place.size[1]);
            }
        });
        ui.end_row();
        ui.label("Angle");
        turned = ui.add(egui::DragValue::new(&mut angle).speed(0.2).fixed_decimals(1).suffix("°")).changed();
        ui.end_row();
    });
    if moved {
        *t = t.placed(w, h, outline, pixels, place.center_for(t.anchor, pixels, offsets), place.size[0]);
    } else if let Some(factor) = factor {
        // From its anchor: the offsets stay.
        *t = t.scaled(w, h, outline, pixels, factor);
    } else if turned {
        *t = t.turned(w, h, angle);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button("Center").on_hover_text("The picture in the middle of the frame").clicked() {
            t.cx = 0.5;
            t.cy = 0.5;
            if !t.anchor.is_frame() {
                t.anchor = Anchor::Center;
            }
        }
        if ui.button("Fill").on_hover_text("The picture covers the whole frame, turned as it is").clicked() {
            *t = t.filling_frame(w, h, outline);
        }
        if ui.button("Fit").on_hover_text("The whole picture in the frame: around it the clips under this one show, or black").clicked() {
            *t = t.showing_whole_picture(w, h, outline);
        }
        if ui.button("−90°").clicked() {
            *t = t.turned(w, h, t.angle_deg - 90.0);
        }
        if ui.button("+90°").clicked() {
            *t = t.turned(w, h, t.angle_deg + 90.0);
        }
        if ui.button("0°").clicked() {
            *t = t.turned(w, h, 0.0);
        }
        let mirror = ui.add(egui::Button::new("Mirror").selected(t.mirror));
        if mirror.on_hover_text("Flip the picture left to right: one that follows the frame flips with all of it").clicked() {
            *t = t.flipped(w, h);
        }
        let bars = ui.add(egui::Button::new("Bars").selected(t.bars));
        if bars.on_hover_text("Black around the picture: the clips under this one are hidden there").clicked() {
            t.bars = !t.bars;
        }
        if ui.button("Reset").on_hover_text("Back to the start: covering the frame, centered, no turn, no mirror, no bars").clicked() {
            *t = Transform::default();
        }
    });
}

/// The frame's points in a grid of 3×3, the one `anchor` names filled. Returns the point clicked.
fn anchor_grid(ui: &mut egui::Ui, anchor: Anchor) -> Option<Anchor> {
    const CELL: f32 = 12.0;
    const GAP: f32 = 3.0;
    let side = 3.0 * CELL + 2.0 * GAP;
    let (grid, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let mut clicked = None;
    for (i, point) in Anchor::POINTS.into_iter().enumerate() {
        let at = grid.min + egui::vec2((i % 3) as f32 * (CELL + GAP), (i / 3) as f32 * (CELL + GAP));
        let cell = egui::Rect::from_min_size(at, egui::vec2(CELL, CELL));
        let response = ui.interact(cell, ui.id().with(("anchor", i)), egui::Sense::click());
        let fill = match (point == anchor, response.hovered()) {
            (true, _) => flash::COLOR,
            (false, true) => Color32::from_gray(95),
            (false, false) => Color32::from_gray(45),
        };
        ui.painter().rect(cell, 2.0, fill, egui::Stroke::new(1.0, Color32::from_gray(120)));
        let response = response.on_hover_text(format!("{}: in other proportions it keeps its distance from there and its size against the frame", point.name()));
        if response.clicked() {
            clicked = Some(point);
        }
    }
    clicked
}

/// The border between the panels of the right column: a line with dots in its middle, dragged up and
/// down. Returns how far it was dragged down this frame.
fn panel_border(ui: &mut egui::Ui) -> Option<f32> {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), PANEL_BORDER_HEIGHT), egui::Sense::drag());
    let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
    let visuals = ui.visuals();
    let active = response.hovered() || response.dragged();
    let color = if active { visuals.strong_text_color() } else { visuals.weak_text_color() };
    let line = if active { visuals.widgets.hovered.fg_stroke } else { visuals.widgets.noninteractive.bg_stroke };
    ui.painter().hline(rect.x_range(), rect.center().y, line);
    let dots = egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, PANEL_BORDER_HEIGHT));
    ui.painter().rect_filled(dots, 3.0, visuals.panel_fill);
    grip_dots(ui.painter(), rect.center(), 3, 1, color);
    if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        return Some(response.drag_delta().y);
    }
    None
}

/// A stretch of a clip's source it plays at one speed: its runs that play it, one after another
/// however many frames it holds on the way.
struct Stretch {
    from: f64,
    to: f64,
    rate: f64,
    runs: Vec<Run>,
}

impl Stretch {
    /// Its source's sound, as made for playing at its speed.
    fn part(&self, clip: &Clip) -> Part {
        Part::new(clip.source, self.from, self.to, self.rate)
    }
}

/// The stretches of its source a clip plays, in order: at its speed, and at the speed of each part
/// in slow motion. The sound of one at another speed than 1 is made for it.
fn stretches(clip: &Clip) -> Vec<Stretch> {
    let runs = clip.runs();
    let mut stretches: Vec<Stretch> = Vec::new();
    for (n, run) in runs.iter().enumerate().filter(|(_, run)| !run.is_held()) {
        // It goes on in the source up to where the next run is in it, or the end of what the clip plays.
        let to = runs.get(n + 1).map_or(clip.source_out, |next| next.source);
        match stretches.last_mut() {
            Some(last) if last.rate == run.rate && (last.to - run.source).abs() < 1e-6 => {
                last.to = to;
                last.runs.push(*run);
            }
            _ => stretches.push(Stretch { from: run.source, to, rate: run.rate, runs: vec![*run] }),
        }
    }
    stretches
}

/// Why a clip did not play slower from the playhead.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SlowRefused {
    /// It plays that moment in slow motion already.
    InOne,
    /// Its recording ends there.
    AtEnd,
    /// The clips after it could not move later.
    NoRoom,
}

/// What a block of the Effects asks for, of the effect it is about. The clip gets longer or shorter
/// with it, and the clips after it move along: the sequence does that.
enum EffectChange {
    Speed(f64),
    SpeedOff,
    /// The frame of this moment of the source is to be held instead.
    FreezeFrame(f64),
    FreezeLength(f64),
    FreezeOff,
    SlowMotionSpeed(f64),
    /// The moments of the source it is to play in slow motion instead.
    SlowMotionPart(f64, f64),
    SlowMotionOff,
}

/// The effects of a clip, in order, each with its settings, and a way to add more. `refused`: the
/// last change of that effect could not be made. Times are typed and shown at `fps`; a frame is
/// held a `frame` long at least.
fn effects_editor(ui: &mut egui::Ui, clip: &mut Clip, picture: bool, sound: bool, refused: Option<usize>, fps: f64, frame: f64) -> Option<(usize, EffectChange)> {
    ui.heading("Effects");
    let len = clip.span().len();
    let (fade_in, fade_out) = clip.fade_lengths();
    let (source_in, source_out) = (clip.source_in, clip.source_out);
    // The parts it plays in slow motion, as it plays them: within what it plays.
    let slowed = clip.slowed();
    let mut remove = None;
    let mut change = None;
    let no_room = || RichText::new("The clips after it can't move that far: one chained to them has no room.").color(ERROR);
    for (i, effect) in clip.effects.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match effect {
                Effect::Fade(fade) => {
                    let (name, room) = match fade.edge {
                        Edge::In => ("Fade in", len - fade_out),
                        Edge::Out => ("Fade out", len - fade_in),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(name).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label("Length");
                        let length = egui::DragValue::new(&mut fade.seconds).speed(0.01).range(0.0..=room.max(0.0)).fixed_decimals(2).suffix(" s");
                        ui.add(length).on_hover_text("No longer than the clip leaves after the other fade");
                        if picture && sound {
                            ui.checkbox(&mut fade.picture, "Picture").on_hover_text("The picture comes out of, or goes into, what is under the clip: black if there is nothing");
                            ui.checkbox(&mut fade.sound, "Sound").on_hover_text("The sound rises from, or falls into, silence");
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Curve");
                        let (icon, _) = ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::hover());
                        ui.painter().rect_stroke(icon, 2.0, egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
                        paint_curve(ui.painter(), icon.shrink(2.0), fade.curve, fade.edge, ui.visuals().text_color());
                        egui::ComboBox::from_id_salt(("fade curve", i)).selected_text(fade.curve.name()).width(150.0).show_ui(ui, |ui| {
                            for curve in Curve::ALL {
                                if curve_choice(ui, curve, fade.edge, curve == fade.curve).clicked() {
                                    fade.curve = curve;
                                }
                            }
                        });
                    })
                    .response
                    .on_hover_text("How it goes: an ease in starts slowly, an ease out ends slowly");
                }
                Effect::Color(color) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Color").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                remove = Some(i);
                            }
                            if ui.add_enabled(!color.is_neutral(), egui::Button::new("Reset").small()).clicked() {
                                *color = Color::default();
                            }
                        });
                    });
                    egui::Grid::new(("color", i)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Exposure").on_hover_text("In stops: +1 is twice as bright");
                        ui.add(egui::Slider::new(&mut color.exposure, Color::EXPOSURE).step_by(0.05).fixed_decimals(2).suffix(" EV"));
                        ui.end_row();
                        ui.label("Contrast");
                        ui.add(egui::Slider::new(&mut color.contrast, Color::CONTRAST).step_by(1.0).fixed_decimals(0));
                        ui.end_row();
                        ui.label("Saturation").on_hover_text("0 % is black and white");
                        ui.add(egui::Slider::new(&mut color.saturation, Color::SATURATION).step_by(1.0).fixed_decimals(0).suffix(" %"));
                        ui.end_row();
                        ui.label("Temperature").on_hover_text("Colder, bluer to the left; warmer, redder to the right");
                        ui.add(egui::Slider::new(&mut color.temperature, Color::TEMPERATURE).step_by(1.0).fixed_decimals(0));
                        ui.end_row();
                    });
                }
                Effect::Speed(speed) => {
                    let factor = speed.played();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Speed").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                change = Some((i, EffectChange::SpeedOff));
                            }
                            if ui.add_enabled(factor != 1.0, egui::Button::new("Reset").small()).clicked() {
                                change = Some((i, EffectChange::Speed(1.0)));
                            }
                        });
                    });
                    let mut wanted = factor;
                    let slider = egui::Slider::new(&mut wanted, Speed::MIN..=Speed::MAX).logarithmic(true).fixed_decimals(2).prefix("×");
                    if ui.add(slider).on_hover_text("2 is twice as fast and half as long; 0.5 half as fast. The sound keeps its pitch").changed() {
                        change = Some((i, EffectChange::Speed(wanted)));
                    }
                    ui.horizontal(|ui| {
                        for preset in [0.5, 2.0, 4.0] {
                            if ui.add(egui::Button::new(speed_text(preset)).selected(factor == preset)).clicked() {
                                change = Some((i, EffectChange::Speed(preset)));
                            }
                        }
                    });
                    let note = if refused == Some(i) { no_room() } else { RichText::new("Its length changes with it, and the clips after it on its track move along.").weak() };
                    ui.label(note.small());
                }
                Effect::Freeze(freeze) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Freeze").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                change = Some((i, EffectChange::FreezeOff));
                            }
                        });
                    });
                    egui::Grid::new(("freeze", i)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("Frame");
                        let mut at = freeze.at;
                        let field = ui.add(time_value(&mut at, fps)).on_hover_text("The moment of its source whose frame is held");
                        if field.changed() {
                            change = Some((i, EffectChange::FreezeFrame(at.clamp(source_in, source_out.max(source_in)))));
                        }
                        ui.end_row();
                        ui.label("Length");
                        let mut seconds = freeze.seconds;
                        if ui.add(time_value(&mut seconds, fps)).on_hover_text("How long the frame is held").changed() {
                            change = Some((i, EffectChange::FreezeLength(seconds.max(frame))));
                        }
                        ui.end_row();
                    });
                    let note = if refused == Some(i) {
                        no_room()
                    } else {
                        RichText::new("Held without sound. The rest of the clip and the clips after it on its track move along, and so does its detached sound.").weak()
                    };
                    ui.label(note.small());
                }
                Effect::SlowMotion(part) => {
                    let factor = part.played();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Tempo").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                change = Some((i, EffectChange::SlowMotionOff));
                            }
                        });
                    });
                    let mut wanted = factor;
                    let slider = egui::Slider::new(&mut wanted, Speed::MIN..=Speed::MAX).logarithmic(true).fixed_decimals(2).prefix("×");
                    let hover = "How fast this part of its recording plays, whatever the speed of the rest of the clip: 0.5 is half as fast and twice as long. The sound keeps its pitch";
                    if ui.add(slider).on_hover_text(hover).changed() {
                        change = Some((i, EffectChange::SlowMotionSpeed(wanted)));
                    }
                    ui.horizontal(|ui| {
                        for preset in [0.25, 0.5, 2.0] {
                            if ui.add(egui::Button::new(speed_text(preset)).selected(factor == preset)).clicked() {
                                change = Some((i, EffectChange::SlowMotionSpeed(preset)));
                            }
                        }
                    });
                    let (from, to) = slowed.iter().find(|played| played.effect == i).map_or((part.from, part.to), |played| (played.from, played.to));
                    egui::Grid::new(("slow motion", i)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                        ui.label("From");
                        let mut start = from;
                        if ui.add(time_value(&mut start, fps)).on_hover_text("The moment of its recording where it starts").changed() {
                            change = Some((i, EffectChange::SlowMotionPart(start.clamp(source_in, (to - frame).max(source_in)), to)));
                        }
                        ui.end_row();
                        ui.label("To");
                        let mut end = to;
                        if ui.add(time_value(&mut end, fps)).on_hover_text("The moment of its recording where it ends").changed() {
                            change = Some((i, EffectChange::SlowMotionPart(from, end.clamp((from + frame).min(source_out), source_out))));
                        }
                        ui.end_row();
                    });
                    let note = if refused == Some(i) {
                        no_room()
                    } else {
                        RichText::new("The rest of the clip and the clips after it on its track move along, and so does its detached sound.").weak()
                    };
                    ui.label(note.small());
                }
            }
        });
    }
    if let Some(i) = remove {
        clip.effects.remove(i);
    }
    clip.fit_fades();
    if clip.effects.is_empty() {
        ui.label(RichText::new("No effects. Also from the clip's menu: right click on it.").weak());
    }
    ui.menu_button("Add effect", |ui| {
        for (edge, name) in [(Edge::In, "Fade in"), (Edge::Out, "Fade out")] {
            let add = ui.add_enabled(clip.fade(edge).is_none(), egui::Button::new(name));
            if add.on_disabled_hover_text("The clip has one").clicked() {
                clip.add_fade(edge);
                ui.close_menu();
            }
        }
        if picture {
            let add = ui.add_enabled(clip.color().is_none(), egui::Button::new("Color"));
            if add.on_disabled_hover_text("The clip has one").clicked() {
                clip.add_color();
                ui.close_menu();
            }
        }
        // A still shows one frame however fast.
        if clip.still.is_none() {
            let add = ui.add_enabled(clip.speed_effect().is_none(), egui::Button::new("Speed"));
            if add.on_disabled_hover_text("The clip has one").clicked() {
                clip.add_speed();
                ui.close_menu();
            }
        }
    });
    change
}

/// A time field: drag to change, click to type `mm:ss.cc`, `mm:ss:ff` or seconds.
fn time_value(value: &mut f64, fps: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .speed(1.0 / fps)
        .custom_formatter(|v, _| timecode::format(v))
        .custom_parser(move |s| timecode::parse(s, fps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_of_a_clip_is_made_for_each_stretch_of_one_speed() {
        // 0..10 s of a 30 s source at ×2, with 4..5 s of it in slow motion at ×0.5 and the frame at
        // 7 s held: ×2 up to 4 s, ×0.5 up to 5 s, and ×2 on from there through the frame held.
        let mut sequence = Sequence::new(Clip { source_out: 10.0, ..Clip::new("a".into(), 30.0) });
        let id = ClipId { track: 0, index: 0 };
        assert!(sequence.set_speed(id, 2.0));
        assert!(sequence.slow_motion(id, 2.0, SLOW_MOTION_SECONDS, SLOW_MOTION_FACTOR));
        assert!(sequence.freeze(id, 5.0, FREEZE_LENGTH));
        let found: Vec<(f64, f64, f64, usize)> = stretches(sequence.clip(id)).iter().map(|s| (s.from, s.to, s.rate, s.runs.len())).collect();
        assert_eq!(found, [(0.0, 4.0, 2.0, 1), (4.0, 5.0, 0.5, 1), (5.0, 10.0, 2.0, 2)]);
        // A plain clip is one stretch at its speed.
        let plain = Clip { source_in: 2.0, source_out: 12.0, ..Clip::new("a".into(), 30.0) };
        let found: Vec<(f64, f64, f64, usize)> = stretches(&plain).iter().map(|s| (s.from, s.to, s.rate, s.runs.len())).collect();
        assert_eq!(found, [(2.0, 12.0, 1.0, 1)]);
    }
}

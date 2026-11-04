//! The editor: source view, output preview with transform controls, timeline and transport.

use crate::core::edit::{Bounds, Clip, ClipId, Sequence, Span};
use crate::core::history::History;
use crate::media::audio::{PlayRange, Segment};
use crate::media::cache::MediaCache;
use crate::media::frames::FrameDecoder;
use crate::media::geometry::{self, Transform};
use crate::media::player::Player;
use crate::media::{timecode, Notify};
use crate::ui::export::ExportWindow;
use crate::ui::framing::Framing;
use crate::ui::timeline::Timeline;
use crate::ui::viewer::{OutputView, SourceView};
use egui::{Color32, Key, Modifiers, RichText, TextureOptions};
use std::sync::Arc;
use std::time::Duration;

/// The output preview's largest height: a tall frame should not push the controls out of the panel.
const OUTPUT_MAX_HEIGHT: f64 = 340.0;

#[derive(Clone, Copy, PartialEq)]
enum PlayMode {
    /// From the playhead to the end of the last clip.
    Timeline,
    /// The export range, with the export fades, looped if `loop_export` is on.
    Export,
}

pub struct Editor {
    // Dropped first: stops a cache build that is still running.
    pub cache: MediaCache,
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
    export: ExportWindow,
    timeline: Timeline,
    source_view: SourceView,
    output_view: OutputView,
}

impl Editor {
    pub fn new(cache: MediaCache, ctx: &egui::Context) -> Self {
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let decoder = FrameDecoder::new(cache.frames_path(), cache.frames.clone(), notify);
        let player = Player::new(cache.audio.clone());
        let name = cache.source.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let sequence = Sequence::new(Clip::new(name, cache.info.duration));
        Self {
            cache,
            decoder,
            player,
            texture: None,
            requested_frame: None,
            position: 0.0,
            peek: None,
            playing: false,
            play_mode: PlayMode::Timeline,
            loop_export: false,
            history: History::new(sequence.clone()),
            sequence,
            selected: ClipId { track: 0, index: 0 },
            framing: Framing::default(),
            export: ExportWindow::default(),
            timeline: Timeline::new(),
            source_view: SourceView::default(),
            output_view: OutputView,
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if self.playing {
            let (t, still_playing) = self.player.poll();
            self.position = t.clamp(0.0, self.sequence.end());
            self.playing = still_playing;
        }
        if let Some(image) = self.decoder.poll() {
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("frame", image, TextureOptions::LINEAR)),
            }
        }

        self.handle_keys(ctx);
        self.timeline_panel(ctx);
        self.output_panel(ctx);
        self.source_panel(ctx);
        let shown = self.framing.shown_presets(self.sequence.output);
        self.export.show(ctx, &mut self.sequence, &self.cache.source, &self.cache.info, &shown);

        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input();
        let now = ctx.input(|i| i.time);
        if self.history.track(&self.sequence, now, busy) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

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

    fn export_bounds(&self) -> Bounds {
        self.sequence.export_bounds(self.frame())
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
            PlayMode::Timeline => PlayRange { start: 0.0, end: self.content().end, looping: false, fades: None, sound },
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
        let content = self.content();
        match mode {
            PlayMode::Export if !self.sequence.export.contains(self.position) => self.position = self.sequence.export.start,
            PlayMode::Timeline if self.position >= content.end - self.frame() => self.position = content.start,
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
        self.position = t.clamp(0.0, self.sequence.end());
    }

    fn step(&mut self, frames: i64) {
        let frame = timecode::frame_index(self.position, self.fps()) + frames;
        self.seek(frame as f64 / self.fps());
    }

    fn set_export_start(&mut self, t: f64) {
        let bounds = self.export_bounds();
        self.sequence.export.set_start(t, bounds);
    }

    fn set_export_end(&mut self, t: f64) {
        let bounds = self.export_bounds();
        self.sequence.export.set_end(t, bounds);
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

    /// The timeline may have got shorter, and what is playing no longer matches the edit.
    fn after_history_step(&mut self) {
        self.pause();
        self.position = self.position.clamp(0.0, self.sequence.end());
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
        let end = self.sequence.end();
        egui::TopBottomPanel::bottom("timeline").show(ctx, |ui| {
            ui.add_space(6.0);
            let response = self.timeline.show(ui, &mut self.position, &mut self.sequence, Some(self.selected), fps);
            if (response.seeked || response.edit_started) && self.playing {
                self.pause();
            }
            self.peek = response.peek;
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                if ui.button("⏮").on_hover_text("Go to the export start (↑)").clicked() {
                    self.seek(self.sequence.export.start);
                }
                if ui.button("⏪").on_hover_text("Previous frame (←, Shift+← for a second)").clicked() {
                    self.step(-1);
                }
                let play = if self.playing { "⏸" } else { "▶" };
                let play_button = egui::Button::new(RichText::new(play).size(16.0)).min_size(egui::vec2(44.0, 0.0));
                if ui.add(play_button).on_hover_text("Play / pause (Space)").clicked() {
                    self.toggle_play();
                }
                if ui.button("⏩").on_hover_text("Next frame (→, Shift+→ for a second)").clicked() {
                    self.step(1);
                }
                if ui.button("⏭").on_hover_text("Go to the export end (↓)").clicked() {
                    self.seek(self.sequence.export.end);
                }
                ui.separator();
                let mut t = self.position;
                if ui.add(time_value(&mut t, fps, end)).on_hover_text("Playhead").changed() {
                    self.seek(t);
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
            egui::Grid::new("ranges").num_columns(9).spacing([8.0, 4.0]).show(ui, |ui| {
                let export = self.sequence.export;
                let bounds = self.export_bounds();
                ui.label(RichText::new("Export").strong());
                ui.label("In");
                let mut start = export.start;
                if ui.add(time_value(&mut start, fps, end)).changed() {
                    self.set_export_start(start);
                }
                if ui.button("Set (I)").on_hover_text("Start the export at the playhead").clicked() {
                    self.set_export_start(at_playhead);
                }
                ui.label("Out");
                let mut out = export.end;
                if ui.add(time_value(&mut out, fps, end)).changed() {
                    self.set_export_end(out);
                }
                if ui.button("Set (O)").on_hover_text("End the export at the playhead").clicked() {
                    self.set_export_end(at_playhead);
                }
                ui.label("Length");
                let mut length = export.len();
                if ui.add(time_value(&mut length, fps, end)).changed() {
                    self.sequence.export.resize_end(export.start + length, bounds);
                }
                ui.end_row();

                let clip = self.clip().span();
                ui.label(RichText::new("Clip").strong());
                ui.label("Start");
                let mut start = clip.start;
                if ui.add(time_value(&mut start, fps, end)).changed() {
                    self.trim_start(start);
                }
                if ui.button("Set ([)").on_hover_text("Trim the clip's start to the playhead").clicked() {
                    self.trim_start(at_playhead);
                }
                ui.label("End");
                let mut clip_end = clip.end;
                if ui.add(time_value(&mut clip_end, fps, end)).changed() {
                    self.trim_end(clip_end);
                }
                if ui.button("Set (])").on_hover_text("Trim the clip's end to the playhead").clicked() {
                    self.trim_end(at_playhead);
                }
                ui.label("Length");
                let mut length = clip.len();
                if ui.add(time_value(&mut length, fps, end)).changed() {
                    self.trim_end(clip.start + length);
                }
                ui.end_row();
            });
            ui.add_space(6.0);
        });
    }

    fn output_panel(&mut self, ctx: &egui::Context) {
        let output = self.sequence.output;
        let settings = self.sequence.settings;
        let outline = output.outline();
        let size = self.frame_size();
        // Between clips there is nothing in the output.
        let texture = self.texture.as_ref().filter(|_| self.sequence.clip_at(self.shown_time()).is_some());
        egui::SidePanel::right("output").resizable(true).default_width(380.0).min_width(280.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                let can_save_frame = self.sequence.clip_at(self.position).is_some() && !self.export.is_busy();
                ui.horizontal(|ui| {
                    ui.heading("Output");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(self.export.button_text()).clicked() {
                            self.export.open();
                        }
                        let hint = "Save the picture at the playhead as a PNG, in the output's size";
                        let save_frame = ui.add_enabled(can_save_frame, egui::Button::new("Save frame…"));
                        if save_frame.on_hover_text(hint).clicked() {
                            self.export.save_frame(ctx, &self.sequence, &self.cache.source, &self.cache.info, self.position);
                        }
                    });
                });
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
                t.angle_deg = geometry::normalize_angle(t.angle_deg);
                *t = t.clamped(size[0], size[1], outline);
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
                    ui.label(timecode::format(info.duration, preview_fps));
                    ui.end_row();
                    ui.label("Audio");
                    ui.label(if info.has_audio { "yes" } else { "no" });
                    ui.end_row();
                });
                if let Some(error) = &self.player.audio_error {
                    ui.colored_label(Color32::from_rgb(230, 120, 60), format!("No sound: {error}"));
                }
            });
        });
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

/// A time field: drag to change, click to type `mm:ss:ff`, `mm:ss` or seconds.
fn time_value(value: &mut f64, fps: f64, max: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .speed(1.0 / fps)
        .range(0.0..=max)
        .custom_formatter(move |v, _| timecode::format(v, fps))
        .custom_parser(move |s| timecode::parse(s, fps))
}

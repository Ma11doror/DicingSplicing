//! The frames menu and the ratio controls: which output frame is the main one, which others are
//! shown on the source, and whether the main frame's proportions can change.

use crate::core::preset::{FrameOptions, Output, Preset, PresetId, Ratio, PRESETS};
use crate::media::geometry::Mask;
use crate::ui::flash;
use crate::ui::viewer::{self, FrameOverlay};
use egui::{vec2, Color32, RichText, Sense};

const LOCK_SIZE: f32 = 15.0;

pub struct Framing {
    /// In the order of `PRESETS`.
    options: Vec<FrameOptions>,
    /// Whether the `Custom` frame keeps its proportions. All the other frames always do.
    ratio_locked: bool,
    /// When the proportions were unlocked: the controls flash for a moment after that.
    unlocked_at: Option<f64>,
}

impl Default for Framing {
    fn default() -> Self {
        let options = PRESETS.iter().map(FrameOptions::for_preset).collect();
        Self { options, ratio_locked: false, unlocked_at: None }
    }
}

fn color(preset: &Preset) -> Color32 {
    let [r, g, b] = preset.color;
    Color32::from_rgb(r, g, b)
}

/// The lock as a button; a disabled one is dimmer and does not react.
fn lock_button(ui: &mut egui::Ui, free: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 20.0), sense);
    let visuals = ui.style().interact(&response);
    ui.painter().rect(rect, visuals.rounding, visuals.weak_bg_fill, visuals.bg_stroke);
    let color = viewer::lock_color(free);
    let color = if enabled { color } else { color.gamma_multiply(0.5) };
    viewer::paint_lock(ui.painter(), rect.center(), LOCK_SIZE, free, color);
    response
}

impl Framing {
    /// Comes back to how a project left things. Frames it doesn't mention keep their defaults.
    pub fn restored(frames: &[(PresetId, FrameOptions)], ratio_locked: bool) -> Self {
        let mut framing = Self { ratio_locked, ..Self::default() };
        for (preset, options) in PRESETS.iter().zip(&mut framing.options) {
            if let Some((_, saved)) = frames.iter().find(|(id, _)| *id == preset.id) {
                *options = *saved;
            }
        }
        framing
    }

    /// What a project keeps of this.
    pub fn saved_frames(&self) -> Vec<(PresetId, FrameOptions)> {
        PRESETS.iter().zip(&self.options).map(|(preset, options)| (preset.id, *options)).collect()
    }

    pub fn ratio_locked(&self) -> bool {
        self.ratio_locked
    }

    /// Dragging a side of the main frame changes its proportions.
    pub fn ratio_is_free(&self, output: Output) -> bool {
        output.preset == PresetId::Custom && !self.ratio_locked
    }

    fn unlock(&mut self, now: f64) {
        self.ratio_locked = false;
        self.unlocked_at = Some(now);
    }

    /// Unlocking a frame with fixed proportions makes it `Custom`, starting from the same ratio.
    fn toggle_lock(&mut self, output: &mut Output, now: f64) {
        if output.preset().mask == Mask::Circle {
            return;
        }
        if self.ratio_is_free(*output) {
            self.ratio_locked = true;
            self.unlocked_at = None;
        } else {
            output.custom = output.ratio();
            output.preset = PresetId::Custom;
            self.unlock(now);
        }
    }

    fn select(&mut self, output: &mut Output, id: PresetId, now: f64) {
        if output.preset != id {
            output.preset = id;
            if id == PresetId::Custom {
                self.unlock(now);
            }
        }
    }

    /// New proportions from dragging a side. Shift on a frame with fixed proportions is a one-off:
    /// the frame becomes `Custom`, but stays locked.
    pub fn set_dragged_ratio(&mut self, output: &mut Output, aspect: f64) {
        if output.preset != PresetId::Custom {
            self.ratio_locked = true;
        }
        output.preset = PresetId::Custom;
        output.custom = Ratio::from_aspect(aspect);
    }

    /// The frames shown for comparison.
    pub fn shown_presets(&self, output: Output) -> Vec<&'static Preset> {
        PRESETS.iter().zip(&self.options).filter(|(preset, options)| preset.id != output.preset && options.shown).map(|(preset, _)| preset).collect()
    }

    /// The frames to draw: the main one first, then the ones shown for comparison.
    pub fn overlays(&self, output: Output) -> Vec<FrameOverlay> {
        let free = self.ratio_is_free(output);
        let entries = || PRESETS.iter().zip(&self.options);
        let main = entries()
            .filter(|(preset, _)| preset.id == output.preset)
            .map(|(preset, options)| FrameOverlay { free_ratio: free, ..overlay(output, preset, options) });
        let others = entries()
            .filter(|(preset, options)| preset.id != output.preset && options.shown)
            .map(|(preset, options)| overlay(output, preset, options));
        main.chain(others).collect()
    }

    pub fn menu(&mut self, ui: &mut egui::Ui, output: &mut Output) {
        let now = ui.input(|i| i.time);
        ui.menu_button(format!("Frames: {}", output.preset().name), |ui| {
            egui::Grid::new("frames").num_columns(8).spacing([10.0, 4.0]).show(ui, |ui| {
                for title in ["Main", "Show", "", "Frame", "Ratio", "Thirds", "Cross", "Safe"] {
                    ui.label(RichText::new(title).weak().small());
                }
                ui.end_row();
                for (i, preset) in PRESETS.iter().enumerate() {
                    let is_main = output.preset == preset.id;
                    if ui.radio(is_main, "").on_hover_text("The result has this frame").clicked() {
                        self.select(output, preset.id, now);
                    }
                    let mut shown = self.options[i].shown || is_main;
                    let show = egui::Checkbox::without_text(&mut shown);
                    if ui.add_enabled(!is_main, show).on_hover_text("Show it inside the main frame").changed() {
                        self.options[i].shown = shown;
                    }
                    let (swatch, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    let rounding = if preset.mask == Mask::Circle { 6.0 } else { 2.0 };
                    ui.painter().rect_filled(swatch, rounding, color(preset));
                    ui.label(preset.name);
                    self.menu_ratio(ui, output, preset);
                    let options = &mut self.options[i];
                    ui.add(egui::Checkbox::without_text(&mut options.thirds));
                    ui.add(egui::Checkbox::without_text(&mut options.center));
                    if preset.safe.is_some() {
                        ui.add(egui::Checkbox::without_text(&mut options.safe)).on_hover_text("Roughly where the app's interface covers the video");
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        });
    }

    /// A row's ratio. The `Custom` row stands out, and has the controls when it is the main frame.
    fn menu_ratio(&mut self, ui: &mut egui::Ui, output: &mut Output, preset: &Preset) {
        let custom = preset.id == PresetId::Custom;
        if custom && output.preset == PresetId::Custom {
            self.controls(ui, output, None);
            return;
        }
        let ratio = output.ratio_of(preset).label();
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(LOCK_SIZE, LOCK_SIZE), Sense::hover());
            viewer::paint_lock(ui.painter(), rect.center(), LOCK_SIZE, custom, viewer::lock_color(custom));
            ui.label(if custom { RichText::new(ratio).color(viewer::lock_color(true)) } else { RichText::new(ratio) });
        });
    }

    /// The main frame's lock and ratio, for next to the frames menu.
    pub fn ratio_controls(&mut self, ui: &mut egui::Ui, output: &mut Output, source_aspect: f64) {
        self.controls(ui, output, Some(source_aspect));
    }

    /// With the `source_aspect`, also the buttons that set the ratio at once.
    fn controls(&mut self, ui: &mut egui::Ui, output: &mut Output, source_aspect: Option<f64>) {
        let now = ui.input(|i| i.time);
        let free = self.ratio_is_free(*output);
        flash::frame(ui, self.unlocked_at, free).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Menus draw their buttons flat; these have to look like something to press.
                ui.visuals_mut().button_frame = true;
                let round = output.preset().mask == Mask::Circle;
                let hint = match (round, output.preset == PresetId::Custom, free) {
                    (true, ..) => "A circle is always 1:1",
                    (_, false, _) => "Fixed proportions. Click to unlock them: the frame becomes Custom",
                    (_, _, false) => "Dragging a side keeps the proportions. Click to unlock them",
                    (_, _, true) => "Dragging a side changes the proportions. Click to lock them",
                };
                if lock_button(ui, free, !round).on_hover_text(hint).clicked() {
                    self.toggle_lock(output, now);
                }
                if self.ratio_is_free(*output) {
                    ratio_fields(ui, &mut output.custom, source_aspect);
                } else {
                    ui.label(output.ratio().label());
                }
            });
        });
    }
}

fn ratio_fields(ui: &mut egui::Ui, ratio: &mut Ratio, source_aspect: Option<f64>) {
    let number = |value| {
        egui::DragValue::new(value)
            .speed(0.05)
            .range(0.1..=10000.0)
            .custom_formatter(|v, _| ((v * 100.0).round() / 100.0).to_string())
    };
    ui.add(number(&mut ratio.w));
    ui.label(":");
    ui.add(number(&mut ratio.h));
    if let Some(source_aspect) = source_aspect {
        if ui.button("Swap").on_hover_text("Turn the frame between portrait and landscape").clicked() {
            *ratio = ratio.swapped();
        }
        if ui.button("Source").on_hover_text("The proportions of the source video").clicked() {
            *ratio = Ratio::from_aspect(source_aspect);
        }
    }
}

fn overlay(output: Output, preset: &Preset, options: &FrameOptions) -> FrameOverlay {
    let ratio = output.ratio_of(preset).label();
    FrameOverlay {
        outline: output.outline_of(preset),
        color: color(preset),
        label: if preset.mask == Mask::Circle { format!("{ratio} circle") } else { ratio },
        thirds: options.thirds,
        center: options.center,
        safe: preset.safe.filter(|_| options.safe),
        free_ratio: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(preset: PresetId) -> Output {
        Output { preset, ..Output::default() }
    }

    #[test]
    fn the_main_frame_comes_first_and_others_only_when_shown() {
        let mut framing = Framing::default();
        let labels = |framing: &Framing| -> Vec<String> {
            framing.overlays(output(PresetId::Vertical)).into_iter().map(|o| o.label).collect()
        };
        assert_eq!(labels(&framing), ["9:16"]);

        framing.options[0].shown = true;
        framing.options[1].shown = true;
        framing.options[2].shown = true;
        assert_eq!(labels(&framing), ["9:16", "1:1 circle", "16:9"]);
    }

    #[test]
    fn a_project_gets_its_frames_back() {
        let mut framing = Framing { ratio_locked: true, ..Framing::default() };
        framing.options[1].shown = true;
        framing.options[1].safe = true;
        framing.options[2].thirds = false;

        let restored = Framing::restored(&framing.saved_frames(), framing.ratio_locked());
        assert_eq!(restored.options, framing.options);
        assert!(restored.ratio_locked);

        // A frame the project has not heard of starts as usual.
        let partial = Framing::restored(&framing.saved_frames()[..1], false);
        assert_eq!(partial.options[1], FrameOptions::for_preset(&PRESETS[1]));
    }

    #[test]
    fn options_follow_the_frame() {
        let mut framing = Framing::default();
        let circle = &framing.overlays(Output::default())[0];
        assert!(circle.center && !circle.thirds);

        let vertical = &framing.overlays(output(PresetId::Vertical))[0];
        assert!(vertical.thirds && !vertical.center && vertical.safe.is_none());
        framing.options[1].safe = true;
        assert!(framing.overlays(output(PresetId::Vertical))[0].safe.is_some());
    }

    #[test]
    fn choosing_custom_unlocks_it_and_flashes() {
        let mut framing = Framing { ratio_locked: true, ..Framing::default() };
        let mut out = output(PresetId::Vertical);
        framing.select(&mut out, PresetId::Custom, 5.0);
        assert_eq!(out.preset, PresetId::Custom);
        assert!(framing.ratio_is_free(out));
        assert_eq!(framing.unlocked_at, Some(5.0));

        // Choosing it again changes nothing, and neither does choosing another frame.
        framing.ratio_locked = true;
        framing.select(&mut out, PresetId::Custom, 6.0);
        assert!(!framing.ratio_is_free(out));
        framing.select(&mut out, PresetId::Square, 7.0);
        assert!(!framing.ratio_is_free(out));
        assert_eq!(framing.unlocked_at, Some(5.0));
    }

    #[test]
    fn the_lock_turns_a_fixed_frame_into_custom_with_the_same_ratio() {
        let mut framing = Framing { ratio_locked: true, ..Framing::default() };
        let mut out = output(PresetId::Vertical);
        assert!(!framing.ratio_is_free(out));
        framing.toggle_lock(&mut out, 1.0);
        assert_eq!(out.preset, PresetId::Custom);
        assert_eq!(out.custom, Ratio::new(9.0, 16.0));
        assert!(framing.ratio_is_free(out));

        // Locked again it stays custom, and it keeps its ratio when unlocked once more.
        framing.toggle_lock(&mut out, 2.0);
        assert!(!framing.ratio_is_free(out));
        assert_eq!(out.preset, PresetId::Custom);
        framing.toggle_lock(&mut out, 3.0);
        assert!(framing.ratio_is_free(out));
        assert_eq!(out.custom, Ratio::new(9.0, 16.0));
    }

    #[test]
    fn a_circle_cannot_be_unlocked() {
        let mut framing = Framing::default();
        let mut out = Output::default();
        framing.toggle_lock(&mut out, 1.0);
        assert_eq!(out.preset, PresetId::TelegramCircle);
        assert!(!framing.ratio_is_free(out));
    }

    #[test]
    fn only_the_unlocked_custom_frame_has_a_free_ratio() {
        let framing = Framing::default();
        assert!(!framing.overlays(output(PresetId::Landscape))[0].free_ratio);
        let custom = Output { preset: PresetId::Custom, custom: Ratio::new(5.0, 4.0) };
        let main = &framing.overlays(custom)[0];
        assert!(main.free_ratio);
        assert_eq!(main.label, "5:4");
    }

    #[test]
    fn a_ratio_dragged_on_a_fixed_frame_stays_locked_afterwards() {
        let mut framing = Framing::default();
        let mut out = output(PresetId::Landscape);
        framing.set_dragged_ratio(&mut out, 1.6);
        assert_eq!(out.preset, PresetId::Custom);
        assert_eq!(out.custom.label(), "16:10");
        assert!(!framing.ratio_is_free(out));

        let mut framing = Framing::default();
        let mut out = output(PresetId::Custom);
        framing.set_dragged_ratio(&mut out, 1.25);
        assert!(framing.ratio_is_free(out));
    }

}

//! The Sources button and the list it opens: the project's videos, each with what can be done with
//! it, and ways to add more. A source is only a link to a file somewhere on the disk.

use crate::core::edit::SourceId;
use crate::core::plugins::{Plugin, PluginKind};
use crate::core::project::Source;
use crate::media::library::SourceState;
use crate::media::probe::MediaInfo;
use crate::media::timecode;
use crate::ui::grip_dots;
use egui::{Color32, RichText};

const LIST_WIDTH: f32 = 460.0;
const LIST_HEIGHT: f32 = 380.0;
const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
/// The six dots in front of a source that can be dragged onto the timeline, as on the rows.
const GRIP_SIZE: [f32; 2] = [14.0, 16.0];

/// What the user asked for in the list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceAction {
    AddToTimeline(SourceId),
    Locate(SourceId),
    Remove(SourceId),
    AddVideo,
    /// A plugin that brings in a video, by its place among the plugins.
    Plugin(usize),
}

/// A source as the list shows it.
pub struct SourceRow<'a> {
    pub source: &'a Source,
    pub state: SourceState,
    pub info: Option<&'a MediaInfo>,
    /// How many clips play it.
    pub uses: usize,
}

/// The button, and the list under it while it is open. `open` opens it from outside, as when a
/// video has just been added.
pub fn sources_button(ui: &mut egui::Ui, rows: &[SourceRow], plugins: &[Plugin], open: bool) -> Option<SourceAction> {
    let button = ui.button(format!("Sources ({})", rows.len())).on_hover_text("The videos of this project. Put them on the timeline from here");
    let popup = ui.make_persistent_id("sources_list");
    if button.clicked() || (open && !ui.memory(|memory| memory.is_popup_open(popup))) {
        ui.memory_mut(|memory| memory.toggle_popup(popup));
    }
    let mut action = None;
    egui::popup_below_widget(ui, popup, &button, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_width(LIST_WIDTH);
        egui::ScrollArea::vertical().max_height(LIST_HEIGHT).auto_shrink([false, true]).show(ui, |ui| {
            if rows.is_empty() {
                ui.label(RichText::new("No videos yet.").weak());
            }
            for row in rows {
                if let Some(chosen) = source_row(ui, row) {
                    action = Some(chosen);
                }
                ui.separator();
            }
        });
        ui.horizontal_wrapped(|ui| {
            let add = ui.button("Add video or sound…").on_hover_text("A file from the disk: a video, or sound only, such as music. It is not copied: the project links to it");
            if add.clicked() {
                action = Some(SourceAction::AddVideo);
            }
            for (i, plugin) in plugins.iter().enumerate().filter(|(_, plugin)| plugin.kind == PluginKind::Source) {
                if ui.button(format!("{}…", plugin.name)).on_hover_text(&plugin.hint).clicked() {
                    action = Some(SourceAction::Plugin(i));
                }
            }
        });
    });
    action
}

fn source_row(ui: &mut egui::Ui, row: &SourceRow) -> Option<SourceAction> {
    let id = row.source.id;
    let mut action = None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(LIST_WIDTH - 150.0);
            let path = row.source.path.display().to_string();
            // A source that has been read is dragged onto the timeline by any part of it: the dots,
            // its name, the lines under it.
            if row.info.is_some() {
                let dragged = ui.dnd_drag_source(egui::Id::new(("source", id)), id, |ui| source_text(ui, row, true)).response;
                dragged.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(format!("{path}\nDrag it onto the timeline"));
            } else {
                ui.scope(|ui| source_text(ui, row, false)).response.on_hover_text(path);
            }
        });
        ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
            let add = ui.add_enabled(row.info.is_some(), egui::Button::new("Add to timeline"));
            let add = add.on_hover_text("On a new track under the others, right after the last clip").on_disabled_hover_text("Its file has to be read first");
            if add.clicked() {
                action = Some(SourceAction::AddToTimeline(id));
            }
            if matches!(row.state, SourceState::Missing | SourceState::Failed(_)) && ui.button("Locate file…").clicked() {
                action = Some(SourceAction::Locate(id));
            }
            let remove = ui.add_enabled(row.uses == 0, egui::Button::new("Remove"));
            if remove.on_hover_text("From the project. The file stays where it is").on_disabled_hover_text("It is on the timeline").clicked() {
                action = Some(SourceAction::Remove(id));
            }
        });
    });
    action
}

/// A source's name and what is known about it; with `draggable`, the dots it is dragged by in front.
fn source_text(ui: &mut egui::Ui, row: &SourceRow, draggable: bool) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        if draggable {
            let (grip, _) = ui.allocate_exact_size(egui::vec2(GRIP_SIZE[0], GRIP_SIZE[1]), egui::Sense::hover());
            grip_dots(ui.painter(), grip.center(), 3, 2, ui.visuals().weak_text_color());
        }
        ui.vertical(|ui| {
            ui.label(RichText::new(row.source.label()).strong());
            if let Some(info) = row.info {
                let sound = if info.has_audio { "sound" } else { "no sound" };
                let placed = match row.uses {
                    0 => "not on the timeline".to_owned(),
                    1 => "on the timeline".to_owned(),
                    n => format!("on the timeline {n} times"),
                };
                let details = if info.has_video() {
                    format!("{} · {}×{} · {sound} · {placed}", timecode::format(info.duration), info.width, info.height)
                } else {
                    format!("{} · sound only · {placed}", timecode::format(info.duration))
                };
                ui.label(RichText::new(details).weak().small());
            }
            match &row.state {
                SourceState::Opening => {
                    ui.label(RichText::new("Opening…").weak().small());
                }
                SourceState::Waiting => {
                    ui.label(RichText::new("Its frames wait for another video's").weak().small());
                }
                SourceState::Preparing(progress) => {
                    let sound_only = row.info.is_some_and(|info| !info.has_video());
                    let text = if sound_only { "Reading the sound".to_owned() } else { format!("Preparing frames {:.0}%", progress * 100.0) };
                    ui.add(egui::ProgressBar::new(*progress).desired_width(LIST_WIDTH - 190.0).text(text));
                }
                SourceState::Ready => {}
                SourceState::Missing => {
                    ui.colored_label(ERROR, "The file is not there");
                }
                SourceState::Failed(error) => {
                    ui.colored_label(ERROR, format!("Can't be read: {error}"));
                }
            }
        });
    });
}

//! The Sources button and the list it opens: the project's videos, each with what can be done with
//! it, and ways to add more. A source is only a link to a file somewhere on the disk. The list shows
//! the sources as lines or as thumbnails, in the order the user chose.

use crate::core::edit::SourceId;
use crate::core::plugins::{Plugin, PluginKind};
use crate::core::project::{Source, SourcesOrder, SourcesSort, SourcesView};
use crate::media::cache::MediaCache;
use crate::media::library::SourceState;
use crate::media::probe::MediaInfo;
use crate::media::timecode;
use crate::media::waveform::Peaks;
use crate::media::Notify;
use crate::ui::{grip_dots, paint_arrow};
use crate::ui::thumbnails::{Shown, Thumbnails};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{pos2, vec2, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, TextStyle, Vec2};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

/// The room for the sources until the list's corner is dragged, the same in both views: four
/// thumbnails in a row and the scroll bar, three rows of them.
const DEFAULT_ROOM: [f32; 2] = [640.0, 380.0];
/// The least room: the choices at the top fit in one line, and a thumbnail with the line about it.
const MIN_ROOM: [f32; 2] = [430.0, 170.0];
/// What the screen has to have besides the room for the sources: the frame around the list, the
/// choices above the sources and the buttons under them.
const AROUND_ROOM: [f32; 2] = [16.0, 110.0];
/// The line about the chosen thumbnail under the thumbnails, with the line that parts them.
const CHOSEN_LINE_HEIGHT: f32 = 48.0;
/// The corner the list is made larger or smaller by.
const SIZE_GRIP: f32 = 16.0;
const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
/// The six dots in front of a source that can be dragged onto the timeline, as on the rows.
const GRIP_SIZE: [f32; 2] = [14.0, 16.0];
const TILE_WIDTH: f32 = 144.0;
/// The picture's part of a thumbnail: 16:9, the shape most videos have.
const PICTURE_HEIGHT: f32 = 81.0;
/// The name and a line about the source, under the picture.
const TILE_TEXT_HEIGHT: f32 = 36.0;
const TILE_GAP: f32 = 8.0;
const PICTURE_BACKGROUND: Color32 = Color32::from_gray(18);
const BADGE_BACKGROUND: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 170);
const WAVE_COLOR: Color32 = Color32::from_rgb(110, 170, 210);
/// The line under the picture that shows where in the video the pointer is.
const SCRUB_COLOR: Color32 = Color32::from_rgb(230, 70, 60);
const NO_THUMBNAIL_TEXT: &str = "Click a thumbnail to see what it is. Drag it onto the timeline, or double click it.";
/// The two drawn buttons at the top left that choose how the sources are shown.
const VIEW_BUTTON_WIDTH: f32 = 22.0;
/// The small button at the top right with the arrow that turns the order round.
const ORDER_BUTTON_SIZE: f32 = 18.0;
/// How far apart the rows of the list's picture are: closer, and its dots run together.
const LIST_ICON_STEP: f32 = 5.0;
/// How far apart the squares of the thumbnails' picture are: three points each, and a gap of one.
const THUMBNAILS_ICON_STEP: f32 = 4.0;

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
    /// The editor's frames of it, as far as they are prepared.
    pub cache: Option<&'a MediaCache>,
    /// Its sound, for a source of sound only.
    pub sound: Option<Arc<OnceLock<Peaks>>>,
}

/// What the list keeps from one frame to the next: how it shows the sources and in what order, how
/// big it is, what it said after files were added, the thumbnails and when the files were changed.
pub struct SourcesPanel {
    pub view: SourcesView,
    pub sort: SourcesSort,
    /// After files were added: how many of them were in the project already.
    pub note: Option<String>,
    /// The thumbnail clicked last: the line under the thumbnails is about it.
    selected: Option<SourceId>,
    thumbnails: Thumbnails,
    /// When each source's file was changed, as of the file the source had when it was looked at.
    modified: HashMap<SourceId, (PathBuf, Option<SystemTime>)>,
    /// How much room the sources have, once the list's corner has been dragged.
    pub size: Option<[f32; 2]>,
}

impl SourcesPanel {
    pub fn new(view: SourcesView, sort: SourcesSort, size: Option<[f32; 2]>, notify: Notify) -> Self {
        Self { view, sort, note: None, selected: None, thumbnails: Thumbnails::new(notify), modified: HashMap::new(), size }
    }

    /// When the file was changed. Looked at once for each file a source has.
    fn modified(&mut self, source: &Source) -> Option<SystemTime> {
        let entry = self.modified.entry(source.id).or_insert_with(|| (source.path.clone(), file_date(&source.path)));
        if entry.0 != source.path {
            *entry = (source.path.clone(), file_date(&source.path));
        }
        entry.1
    }

    /// The order to show the rows in, as their places in `rows`.
    fn order(&mut self, rows: &[SourceRow]) -> Vec<usize> {
        let keys: Vec<SortKey> = rows
            .iter()
            .map(|row| SortKey { id: row.source.id, name: row.source.label(), modified: (self.sort.by == SourcesOrder::Modified).then(|| self.modified(row.source)).flatten() })
            .collect();
        sorted(&keys, self.sort)
    }

    /// The choice of how the sources are shown, at the left, and of their order, at the right.
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            view_choice(ui, &mut self.view, SourcesView::List);
            view_choice(ui, &mut self.view, SourcesView::Thumbnails);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: the first one goes rightmost.
                if order_button(ui, self.sort.descending).on_hover_text(direction_label(self.sort)).clicked() {
                    self.sort.descending = !self.sort.descending;
                }
                for by in [SourcesOrder::Modified, SourcesOrder::Added, SourcesOrder::Name] {
                    ui.selectable_value(&mut self.sort.by, by, order_label(by)).on_hover_text(order_hint(by));
                }
                ui.label("Sorting:");
                ui.separator();
            });
        });
    }

    /// The line about the chosen thumbnail and what can be done with it, or how to choose one.
    fn chosen_line(&mut self, ui: &mut egui::Ui, rows: &[SourceRow]) -> Option<SourceAction> {
        let Some(row) = self.selected.and_then(|id| rows.iter().find(|row| row.source.id == id)) else {
            ui.label(RichText::new(NO_THUMBNAIL_TEXT).weak().small());
            return None;
        };
        let mut action = None;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width() - 300.0);
                ui.add(egui::Label::new(RichText::new(row.source.label()).strong()).truncate()).on_hover_text(row.source.path.display().to_string());
                if let Some(details) = details(row, None) {
                    ui.label(RichText::new(details).weak().small());
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: the first one goes rightmost.
                if let Some(chosen) = actions(ui, row) {
                    action = Some(chosen);
                }
            });
        });
        action
    }
}

/// The button, and the list under it while it is open. `open` opens it from outside, as when a
/// video has just been added.
pub fn sources_button(ui: &mut egui::Ui, rows: &[SourceRow], plugins: &[Plugin], open: bool, panel: &mut SourcesPanel) -> Option<SourceAction> {
    let button = ui.button(format!("Sources ({})", rows.len())).on_hover_text("The videos of this project. Put them on the timeline from here");
    let popup = ui.make_persistent_id("sources_list");
    if button.clicked() || (open && !ui.memory(|memory| memory.is_popup_open(popup))) {
        ui.memory_mut(|memory| memory.toggle_popup(popup));
    }
    let mut action = None;
    egui::popup_below_widget(ui, popup, &button, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        panel.thumbnails.poll(ui.ctx());
        panel.thumbnails.keep(|id| rows.iter().any(|row| row.source.id == id));
        let thumbnails = panel.view == SourcesView::Thumbnails;
        // As much as the screen has to the right of the list and below it.
        let most = ui.ctx().screen_rect().max - ui.max_rect().min - Vec2::from(AROUND_ROOM);
        let room = fitted_room(panel.size, most);
        ui.set_width(room.x);
        panel.header(ui);
        ui.separator();
        let order = panel.order(rows);
        // The same room in both views: the list keeps its size when the view changes. Under the
        // thumbnails, the line about the chosen one takes a part of it.
        ui.allocate_ui_with_layout(room, egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(room);
            let chosen_line = thumbnails && !rows.is_empty();
            let height = if chosen_line { room.y - CHOSEN_LINE_HEIGHT } else { room.y };
            egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
                if rows.is_empty() {
                    ui.label(RichText::new("No videos yet.").weak());
                }
                if thumbnails {
                    ui.spacing_mut().item_spacing = vec2(TILE_GAP, TILE_GAP);
                    ui.horizontal_wrapped(|ui| {
                        for &i in &order {
                            if let Some(chosen) = tile(ui, &rows[i], panel) {
                                action = Some(chosen);
                            }
                        }
                    });
                } else {
                    for &i in &order {
                        let changed = (panel.sort.by == SourcesOrder::Modified).then(|| panel.modified(rows[i].source)).flatten();
                        if let Some(chosen) = source_row(ui, &rows[i], changed) {
                            action = Some(chosen);
                        }
                        ui.separator();
                    }
                }
            });
            if chosen_line {
                ui.separator();
                // In the middle of what is left, whether it is one line or two.
                ui.allocate_ui_with_layout(ui.available_size(), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some(chosen) = panel.chosen_line(ui, rows) {
                        action = Some(chosen);
                    }
                });
            }
        });
        if let Some(note) = &panel.note {
            ui.label(RichText::new(note).weak().small());
        }
        ui.horizontal_wrapped(|ui| {
            let hint = "Files from the disk, as many as you like: videos, or sound only, such as music. They are not copied: the project links to them";
            if ui.button("Add video or sound…").on_hover_text(hint).clicked() {
                action = Some(SourceAction::AddVideo);
            }
            for (i, plugin) in plugins.iter().enumerate().filter(|(_, plugin)| plugin.kind == PluginKind::Source) {
                if ui.button(format!("{}…", plugin.name)).on_hover_text(&plugin.hint).clicked() {
                    action = Some(SourceAction::Plugin(i));
                }
            }
        });
        size_grip(ui, panel, room, most);
    });
    action
}

/// What the list says after files were added, when some of them were in the project already: a
/// file is never added twice, and without a word it would look as if nothing happened.
pub fn already_note(total: usize, already: usize) -> Option<String> {
    match (total, already) {
        (_, 0) => None,
        (1, _) => Some("It is in the project already.".to_owned()),
        (total, already) if already >= total => Some("They are all in the project already.".to_owned()),
        (_, 1) => Some("One of them was in the project already.".to_owned()),
        (_, already) => Some(format!("{already} of them were in the project already.")),
    }
}

/// What can be done with a source: put it on the timeline, find its file, take it out of the project.
fn actions(ui: &mut egui::Ui, row: &SourceRow) -> Option<SourceAction> {
    let id = row.source.id;
    let mut action = None;
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
    action
}

fn source_row(ui: &mut egui::Ui, row: &SourceRow, changed: Option<SystemTime>) -> Option<SourceAction> {
    let id = row.source.id;
    let mut action = None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - 150.0);
            let path = row.source.path.display().to_string();
            // A source that has been read is dragged onto the timeline by any part of it: the dots,
            // its name, the lines under it.
            if row.info.is_some() {
                let dragged = ui.dnd_drag_source(egui::Id::new(("source", id)), id, |ui| source_text(ui, row, true, changed)).response;
                dragged.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(format!("{path}\nDrag it onto the timeline"));
            } else {
                ui.scope(|ui| source_text(ui, row, false, changed)).response.on_hover_text(path);
            }
        });
        ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
            if let Some(chosen) = actions(ui, row) {
                action = Some(chosen);
            }
        });
    });
    action
}

/// What is known about a source, in one line; with `changed`, when its file was changed too.
fn details(row: &SourceRow, changed: Option<SystemTime>) -> Option<String> {
    let info = row.info?;
    let sound = if info.has_audio { "sound" } else { "no sound" };
    let mut details = if info.has_video() {
        format!("{} · {}×{} · {sound} · {}", timecode::format(info.duration), info.width, info.height, placed(row.uses))
    } else {
        format!("{} · sound only · {}", timecode::format(info.duration), placed(row.uses))
    };
    if let Some(changed) = changed {
        let age = SystemTime::now().duration_since(changed).map_or(0, |age| age.as_secs());
        details.push_str(&format!(" · changed {}", ago(age)));
    }
    Some(details)
}

fn placed(uses: usize) -> String {
    match uses {
        0 => "not on the timeline".to_owned(),
        1 => "on the timeline".to_owned(),
        n => format!("on the timeline {n} times"),
    }
}

/// How the state of a source that is not simply there is said, if it isn't.
fn state_text(row: &SourceRow) -> Option<(String, bool)> {
    match &row.state {
        SourceState::Opening => Some(("Opening…".to_owned(), false)),
        SourceState::Waiting => Some(("Its frames wait for another video's".to_owned(), false)),
        SourceState::Preparing(_) if row.info.is_some_and(|info| !info.has_video()) => Some(("Reading the sound".to_owned(), false)),
        SourceState::Preparing(progress) => Some((format!("Preparing frames {:.0}%", progress * 100.0), false)),
        SourceState::Ready => None,
        SourceState::Missing => Some(("The file is not there".to_owned(), true)),
        SourceState::Failed(error) => Some((format!("Can't be read: {error}"), true)),
    }
}

/// A source's name and what is known about it; with `draggable`, the dots it is dragged by in front.
fn source_text(ui: &mut egui::Ui, row: &SourceRow, draggable: bool, changed: Option<SystemTime>) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        if draggable {
            let (grip, _) = ui.allocate_exact_size(egui::vec2(GRIP_SIZE[0], GRIP_SIZE[1]), egui::Sense::hover());
            grip_dots(ui.painter(), grip.center(), 3, 2, ui.visuals().weak_text_color());
        }
        ui.vertical(|ui| {
            ui.label(RichText::new(row.source.label()).strong());
            if let Some(details) = details(row, changed) {
                ui.label(RichText::new(details).weak().small());
            }
            match (&row.state, state_text(row)) {
                (SourceState::Preparing(progress), Some((text, _))) => {
                    ui.add(egui::ProgressBar::new(*progress).desired_width(ui.available_width()).text(text));
                }
                (_, Some((text, true))) => {
                    ui.colored_label(ERROR, text);
                }
                (_, Some((text, false))) => {
                    ui.label(RichText::new(text).weak().small());
                }
                (_, None) => {}
            }
        });
    });
}

/// One thumbnail: a frame of the source, its length on it, the name under it. Dragged onto the
/// timeline like a line of the list; a click chooses it, a double click puts it on the timeline.
fn tile(ui: &mut egui::Ui, row: &SourceRow, panel: &mut SourcesPanel) -> Option<SourceAction> {
    let id = row.source.id;
    let readable = row.info.is_some();
    // Its place is taken first: a row of thumbnails wraps only around space taken this way.
    let drawn = ui.allocate_ui(vec2(TILE_WIDTH, PICTURE_HEIGHT + TILE_TEXT_HEIGHT), |ui| {
        if readable {
            ui.dnd_drag_source(egui::Id::new(("source", id)), id, |ui| paint_tile(ui, row, panel)).response
        } else {
            ui.scope(|ui| paint_tile(ui, row, panel)).response
        }
    });
    let response = drawn.inner.interact(Sense::click());

    if response.clicked() {
        panel.selected = Some(id);
    }
    if response.double_clicked() && readable {
        return Some(SourceAction::AddToTimeline(id));
    }
    None
}

fn paint_tile(ui: &mut egui::Ui, row: &SourceRow, panel: &mut SourcesPanel) {
    let (rect, _) = ui.allocate_exact_size(vec2(TILE_WIDTH, PICTURE_HEIGHT + TILE_TEXT_HEIGHT), Sense::hover());
    // Only what is in sight asks for its picture.
    if !ui.is_rect_visible(rect) {
        return;
    }
    let picture = Rect::from_min_size(rect.min, vec2(TILE_WIDTH, PICTURE_HEIGHT));
    let pixels = ui.ctx().pixels_per_point();
    let max = [(TILE_WIDTH * pixels).round() as u32, (PICTURE_HEIGHT * pixels).round() as u32];
    let pointer = ui.ctx().pointer_hover_pos().filter(|pointer| picture.contains(*pointer) && ui.rect_contains_pointer(picture));
    let fraction = pointer.map(|pointer| (pointer.x - picture.left()) / picture.width());

    let painter = ui.painter_at(rect);
    let rounding = 4.0;
    painter.rect_filled(picture, rounding, PICTURE_BACKGROUND);
    let path = &row.source.path;
    let scrubbed = fraction.and_then(|fraction| panel.thumbnails.frame(row.source.id, path, fraction, row.cache, max));
    let shown = scrubbed.or_else(|| panel.thumbnails.poster(row.source.id, path, row.info, row.cache, max));
    let sound_only = row.info.is_some_and(|info| !info.has_video());
    match (shown, sound_only, &row.sound) {
        (Some(shown), _, _) => paint_picture(&painter, picture, shown),
        (None, true, Some(sound)) => {
            if let (Some(peaks), Some(info)) = (sound.get(), row.info) {
                paint_sound(&painter, picture.shrink(4.0), peaks, info.duration);
            }
        }
        _ => {}
    }

    let small = TextStyle::Small.resolve(ui.style());
    if let Some(info) = row.info {
        let text = painter.layout_no_wrap(timecode::format(info.duration), small.clone(), Color32::WHITE);
        let badge = Rect::from_min_size(picture.right_bottom() - text.size() - vec2(8.0, 6.0), text.size() + vec2(6.0, 2.0));
        painter.rect_filled(badge, 3.0, BADGE_BACKGROUND);
        painter.galley(badge.min + vec2(3.0, 1.0), text, Color32::WHITE);
    }
    if let SourceState::Preparing(progress) = row.state {
        let bar = Rect::from_min_size(picture.left_bottom() - vec2(0.0, 3.0), vec2(picture.width() * progress, 3.0));
        painter.rect_filled(bar, 0.0, ui.visuals().selection.bg_fill);
    }
    if let Some(fraction) = fraction.filter(|_| scrubbed.is_some()) {
        let line = Rect::from_min_size(picture.left_bottom() - vec2(0.0, 2.0), vec2(picture.width() * fraction.clamp(0.0, 1.0), 2.0));
        painter.rect_filled(line, 0.0, SCRUB_COLOR);
    }
    let selected = panel.selected == Some(row.source.id);
    let hovered = ui.rect_contains_pointer(rect);
    let stroke = if selected {
        Stroke::new(2.0, ui.visuals().selection.stroke.color)
    } else if hovered {
        Stroke::new(1.0, ui.visuals().widgets.hovered.fg_stroke.color)
    } else {
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    painter.rect_stroke(picture.shrink(0.5), rounding, stroke);

    let name_top = picture.bottom() + 3.0;
    let name = one_line(ui, row.source.label(), TextStyle::Body.resolve(ui.style()), ui.visuals().strong_text_color());
    painter.galley(pos2(rect.left() + 1.0, name_top), name, Color32::PLACEHOLDER);
    let (line, color) = match state_text(row) {
        Some((text, true)) => (text, ERROR),
        Some((text, false)) => (text, ui.visuals().weak_text_color()),
        None => (placed(row.uses), ui.visuals().weak_text_color()),
    };
    let line = one_line(ui, line, small, color);
    painter.galley(pos2(rect.left() + 1.0, name_top + 18.0), line, Color32::PLACEHOLDER);
    // Something to see while there is no picture yet.
    if shown.is_none() && !sound_only && readable_but_waiting(row) {
        painter.text(picture.center(), Align2::CENTER_CENTER, "No frame yet", TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
    }
}

/// A source that will have a picture, once it is made.
fn readable_but_waiting(row: &SourceRow) -> bool {
    !matches!(row.state, SourceState::Missing | SourceState::Failed(_))
}

/// Text cut to the width of a thumbnail, with an ellipsis.
fn one_line(ui: &egui::Ui, text: String, font: egui::FontId, color: Color32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(text, TextFormat { font_id: font, color, ..TextFormat::default() });
    job.wrap = TextWrapping::truncate_at_width(TILE_WIDTH - 2.0);
    ui.fonts(|fonts| fonts.layout_job(job))
}

/// A picture in `area`, whole and in its own shape; what it doesn't fill stays dark.
fn paint_picture(painter: &egui::Painter, area: Rect, shown: Shown) {
    let scale = (area.width() / shown.size.x).min(area.height() / shown.size.y);
    let rect = Rect::from_center_size(area.center(), shown.size * scale);
    painter.image(shown.texture, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

/// The graph of a whole sound file, for its thumbnail.
fn paint_sound(painter: &egui::Painter, area: Rect, peaks: &Peaks, duration: f64) {
    let (middle, half) = (area.center().y, area.height() / 2.0);
    let step = duration / area.width().max(1.0) as f64;
    let mut x = area.left().floor();
    while x < area.right() {
        let from = (x - area.left()) as f64 * step;
        let height = (peaks.level(from, from + step) * half).max(0.5);
        painter.line_segment([pos2(x + 0.5, middle - height), pos2(x + 0.5, middle + height)], Stroke::new(1.0, WAVE_COLOR));
        x += 1.0;
    }
}

fn file_date(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}

fn order_label(by: SourcesOrder) -> &'static str {
    match by {
        SourcesOrder::Name => "Name",
        SourcesOrder::Added => "Date added",
        SourcesOrder::Modified => "Date modified",
    }
}

fn order_hint(by: SourcesOrder) -> &'static str {
    match by {
        SourcesOrder::Name => "By name: numbers in names go by their value, so clip 2 comes before clip 10",
        SourcesOrder::Added => "In the order they came into the project",
        SourcesOrder::Modified => "By when their files were last changed, as in the file explorer",
    }
}

/// Which way the order goes now, as the arrow button says when the pointer is over it.
fn direction_label(sort: SourcesSort) -> &'static str {
    match (sort.by, sort.descending) {
        (SourcesOrder::Name, false) => "A to Z",
        (SourcesOrder::Name, true) => "Z to A",
        (_, false) => "Oldest first",
        (_, true) => "Newest first",
    }
}

/// One of the ways to show the sources: a drawn picture to choose, looking like the words that
/// choose the order.
fn view_choice(ui: &mut egui::Ui, view: &mut SourcesView, this: SourcesView) {
    let (name, hint) = match this {
        SourcesView::List => ("List", "List: names, and what is known about each"),
        SourcesView::Thumbnails => ("Thumbnails", "Thumbnails: a frame of each. Move the pointer across one to run through its video"),
    };
    let selected = *view == this;
    let (rect, response) = ui.allocate_exact_size(vec2(VIEW_BUTTON_WIDTH, ui.spacing().interact_size.y), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, name));
    if ui.is_rect_visible(rect) {
        // As egui draws a word to choose.
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() || response.highlighted() || response.has_focus() {
            ui.painter().rect(rect.expand(visuals.expansion), visuals.rounding, visuals.weak_bg_fill, visuals.bg_stroke);
        }
        match this {
            SourcesView::List => paint_list_icon(ui.painter(), rect.center(), visuals.text_color()),
            SourcesView::Thumbnails => paint_thumbnails_icon(ui.painter(), rect.center(), visuals.text_color()),
        }
    }
    if response.on_hover_text(hint).clicked() {
        *view = this;
    }
}

/// The picture of the list: three dots, each with a line after it. Drawn, as the font has none.
/// The dots are round squares: egui blurs a circle this small over five pixels.
fn paint_list_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let center = painter.round_pos_to_pixel_center(center);
    for row in [-LIST_ICON_STEP, 0.0, LIST_ICON_STEP] {
        let y = center.y + row;
        painter.rect_filled(Rect::from_center_size(pos2(center.x - 5.0, y), vec2(3.0, 3.0)), 1.5, color);
        painter.rect_filled(Rect::from_min_max(pos2(center.x - 1.5, y - 1.0), pos2(center.x + 6.5, y + 1.0)), 1.0, color);
    }
}

/// The picture of the thumbnails: a square cut into three by three.
fn paint_thumbnails_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let center = painter.round_pos_to_pixel_center(center);
    for row in [-THUMBNAILS_ICON_STEP, 0.0, THUMBNAILS_ICON_STEP] {
        for column in [-THUMBNAILS_ICON_STEP, 0.0, THUMBNAILS_ICON_STEP] {
            painter.rect_filled(Rect::from_center_size(center + vec2(column, row), vec2(3.0, 3.0)), 0.5, color);
        }
    }
}

/// The small button that turns the order round: an arrow up while the order goes up, from A or
/// from the oldest, and down while it goes the other way.
fn order_button(ui: &mut egui::Ui, descending: bool) -> egui::Response {
    let response = ui.add(egui::Button::new("").min_size(vec2(ORDER_BUTTON_SIZE, ORDER_BUTTON_SIZE)));
    let up = !descending;
    // The arrow reaches one point further on its head's side: half of that back puts it in the middle.
    let center = ui.painter().round_pos_to_pixel_center(response.rect.center());
    let at = center + vec2(0.0, if up { 0.5 } else { -0.5 });
    paint_arrow(ui.painter(), at, up, ui.style().interact(&response).text_color());
    response
}

/// The room for the sources: as it was left, or as it starts, within what the screen has, and no
/// less than the least.
fn fitted_room(size: Option<[f32; 2]>, most: Vec2) -> Vec2 {
    Vec2::from(size.unwrap_or(DEFAULT_ROOM)).min(most).max(Vec2::from(MIN_ROOM))
}

/// The corner that makes the list larger or smaller, marked with the dots of anything dragged.
fn size_grip(ui: &mut egui::Ui, panel: &mut SourcesPanel, room: Vec2, most: Vec2) {
    let corner = ui.min_rect().right_bottom();
    let rect = Rect::from_min_max(corner - vec2(SIZE_GRIP, SIZE_GRIP), corner);
    let response = ui.interact(rect, ui.id().with("size grip"), Sense::drag());
    let active = response.hovered() || response.dragged();
    let color = if active { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
    grip_dots(ui.painter(), rect.center(), 3, 2, color);
    if active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    if response.dragged() {
        panel.size = Some(fitted_room(Some((room + response.drag_delta()).into()), most).into());
    }
    response.on_hover_text("Drag to make the list larger or smaller");
}

/// How long ago, in words.
fn ago(seconds: u64) -> String {
    let (amount, unit) = match seconds {
        0..=59 => return "just now".to_owned(),
        60..=3_599 => (seconds / 60, "minute"),
        3_600..=86_399 => (seconds / 3_600, "hour"),
        86_400..=5_183_999 => (seconds / 86_400, "day"),
        5_184_000..=63_071_999 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };
    format!("{amount} {unit}{} ago", if amount == 1 { "" } else { "s" })
}

/// What a source is sorted by.
struct SortKey {
    id: SourceId,
    name: String,
    modified: Option<SystemTime>,
}

/// The places of `keys` in the order `sort` asks for. Sources whose date is not known, as when
/// the file is not there, go last either way; what is equal keeps the order of adding.
fn sorted(keys: &[SortKey], sort: SourcesSort) -> Vec<usize> {
    let mut order: Vec<usize> = (0..keys.len()).collect();
    let turn = |ordering: Ordering| if sort.descending { ordering.reverse() } else { ordering };
    order.sort_by(|&a, &b| {
        let (a, b) = (&keys[a], &keys[b]);
        let by = match sort.by {
            SourcesOrder::Name => turn(natural_cmp(&a.name, &b.name)),
            SourcesOrder::Added => turn(a.id.cmp(&b.id)),
            SourcesOrder::Modified => match (a.modified, b.modified) {
                (Some(x), Some(y)) => turn(x.cmp(&y)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        };
        by.then(a.id.cmp(&b.id))
    });
    order
}

/// Names compared as the file explorer does: letters regardless of case, and runs of digits by
/// their value, so that "clip 2" comes before "clip 10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        let ordering = match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (x, y) = (take_digits(&mut a), take_digits(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            (Some(x), Some(y)) => {
                a.next();
                b.next();
                x.to_lowercase().cmp(y.to_lowercase())
            }
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut digits = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        digits.push(c);
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn names_go_as_in_the_file_explorer() {
        let mut names = vec!["clip 10.mp4", "Clip 2.mp4", "clip 1.mp4", "b.mp4", "A.mp4", "clip 02.mp4", "clip.mp4"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["A.mp4", "b.mp4", "clip 1.mp4", "Clip 2.mp4", "clip 02.mp4", "clip 10.mp4", "clip.mp4"]);
        assert_eq!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("track 9", "track 10"), Ordering::Less);
        assert_eq!(natural_cmp("2024-12-01", "2024-02-11"), Ordering::Greater);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
    }

    fn keys() -> Vec<SortKey> {
        let at = |seconds: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
        vec![
            SortKey { id: 0, name: "Test card".into(), modified: at(300) },
            SortKey { id: 1, name: "fractal 10".into(), modified: None },
            SortKey { id: 2, name: "Fractal 9".into(), modified: at(100) },
            SortKey { id: 3, name: "music".into(), modified: at(200) },
        ]
    }

    fn sort(by: SourcesOrder, descending: bool) -> SourcesSort {
        SourcesSort { by, descending }
    }

    #[test]
    fn sources_go_in_the_order_asked_for() {
        let keys = keys();
        assert_eq!(sorted(&keys, sort(SourcesOrder::Added, false)), [0, 1, 2, 3], "as they came");
        assert_eq!(sorted(&keys, sort(SourcesOrder::Added, true)), [3, 2, 1, 0]);
        assert_eq!(sorted(&keys, sort(SourcesOrder::Name, false)), [2, 1, 3, 0]);
        assert_eq!(sorted(&keys, sort(SourcesOrder::Name, true)), [0, 3, 1, 2]);
        // The file that is not there has no date: last, whichever way.
        assert_eq!(sorted(&keys, sort(SourcesOrder::Modified, false)), [2, 3, 0, 1]);
        assert_eq!(sorted(&keys, sort(SourcesOrder::Modified, true)), [0, 3, 2, 1]);
    }

    #[test]
    fn equal_names_keep_the_order_of_adding() {
        let keys = vec![SortKey { id: 5, name: "a".into(), modified: None }, SortKey { id: 1, name: "A".into(), modified: None }];
        assert_eq!(sorted(&keys, sort(SourcesOrder::Name, false)), [1, 0]);
        assert_eq!(sorted(&keys, sort(SourcesOrder::Name, true)), [1, 0]);
    }

    #[test]
    fn the_order_button_says_which_way_it_goes() {
        assert_eq!(direction_label(sort(SourcesOrder::Name, false)), "A to Z");
        assert_eq!(direction_label(sort(SourcesOrder::Name, true)), "Z to A");
        assert_eq!(direction_label(sort(SourcesOrder::Added, false)), "Oldest first");
        assert_eq!(direction_label(sort(SourcesOrder::Modified, true)), "Newest first");
    }

    #[test]
    fn what_was_there_already_is_said() {
        assert_eq!(already_note(3, 0), None);
        assert_eq!(already_note(1, 1).as_deref(), Some("It is in the project already."));
        assert_eq!(already_note(3, 3).as_deref(), Some("They are all in the project already."));
        assert_eq!(already_note(3, 1).as_deref(), Some("One of them was in the project already."));
        assert_eq!(already_note(5, 2).as_deref(), Some("2 of them were in the project already."));
    }

    #[test]
    fn the_list_keeps_its_room_within_the_screen() {
        let screen = vec2(1900.0, 1000.0);
        assert_eq!(fitted_room(None, screen), Vec2::from(DEFAULT_ROOM), "as it starts");
        assert_eq!(fitted_room(Some([800.0, 500.0]), screen), vec2(800.0, 500.0), "as it was left");
        assert_eq!(fitted_room(Some([100.0, 50.0]), screen), Vec2::from(MIN_ROOM), "no less than the least");
        assert_eq!(fitted_room(Some([3000.0, 2000.0]), screen), screen, "no more than the screen has");
        assert_eq!(fitted_room(None, vec2(300.0, 100.0)), Vec2::from(MIN_ROOM), "the least, even on a small screen");
    }

    #[test]
    fn ages_in_words() {
        assert_eq!(ago(10), "just now");
        assert_eq!(ago(120), "2 minutes ago");
        assert_eq!(ago(3_600), "1 hour ago");
        assert_eq!(ago(86_400 * 3), "3 days ago");
        assert_eq!(ago(86_400 * 90), "3 months ago");
        assert_eq!(ago(31_536_000 * 2), "2 years ago");
    }
}

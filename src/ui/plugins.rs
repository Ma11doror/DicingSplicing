//! What plugins show: a window that brings in a video, a menu that sends a finished file somewhere.

use crate::core::plugins::{FieldKind, Fetched, Form, Plugin, PluginRun, PublishTarget, RunStatus};
use crate::core::settings::Settings;
use crate::core::workspace::Workspace;
use crate::media::Notify;
use egui::{Align2, Color32, CursorIcon, Key, NumExt, RichText, Sense, Shape, Stroke, TextStyle};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
const WARNING: Color32 = Color32::from_rgb(230, 170, 70);
const SENT: Color32 = Color32::from_rgb(120, 190, 120);
const WIDTH: f32 = 460.0;
const COLUMN_GAP: f32 = 8.0;
const MIN_FIELD_WIDTH: f32 = 80.0;
const CANCEL_WIDTH: f32 = 90.0;
const CANCELLED_TEXT: &str = "Cancelled: nothing went out.";

fn notifier(ctx: &egui::Context) -> Notify {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

/// A progress bar when it is known how far, a spinner otherwise.
fn progress_line(ui: &mut egui::Ui, fraction: Option<f32>, text: &str, width: f32) {
    let text = if text.is_empty() { "Starting" } else { text };
    match fraction {
        Some(fraction) => {
            ui.add(egui::ProgressBar::new(fraction).desired_width(width).text(text));
        }
        None => {
            ui.spinner();
            ui.label(text);
        }
    }
}

#[derive(Default)]
pub struct SourceWindow {
    plugin: Option<Plugin>,
    input: String,
    run: Option<PluginRun>,
    problem: Option<String>,
    wants_focus: bool,
}

impl SourceWindow {
    pub fn open(&mut self, plugin: Plugin) {
        if self.run.is_none() {
            *self = Self { plugin: Some(plugin), wants_focus: true, ..Self::default() };
        }
    }

    fn start(&mut self, ctx: &egui::Context, workspace: &Workspace, settings: &Settings) {
        let Some(plugin) = &self.plugin else { return };
        let input = self.input.trim();
        if input.is_empty() {
            return;
        }
        let out = workspace.plugin_dir(&plugin.id);
        if let Err(error) = fs::create_dir_all(&out) {
            self.problem = Some(format!("Can't create {}: {error}", out.display()));
            return;
        }
        self.problem = None;
        self.run = Some(PluginRun::fetch(plugin, input, &out, &settings.options_of(&plugin.id), notifier(ctx)));
    }

    /// Returns what the plugin brought in, once it has.
    pub fn show(&mut self, ctx: &egui::Context, workspace: Option<&Workspace>, settings: &Settings) -> Option<Fetched> {
        let plugin = self.plugin.clone()?;

        let status = self.run.as_ref().map(PluginRun::status);
        match &status {
            Some(RunStatus::Done(fetched)) => {
                let fetched = fetched.clone();
                *self = Self::default();
                return Some(fetched);
            }
            Some(RunStatus::Failed(problem)) => {
                self.problem = Some(problem.clone());
                self.run = None;
            }
            Some(RunStatus::Cancelled) => self.run = None,
            _ => {}
        }

        let mut open = true;
        let mut start = false;
        egui::Window::new(&plugin.name).open(&mut open).collapsible(false).resizable(false).default_width(WIDTH).show(ctx, |ui| {
            ui.set_width(WIDTH);
            if let Some(RunStatus::Running { fraction, text }) = status.as_ref().filter(|_| self.run.is_some()) {
                ui.label(RichText::new(&self.input).weak());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    progress_line(ui, *fraction, text, WIDTH - 80.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            if let Some(run) = &self.run {
                                run.cancel();
                            }
                        }
                    });
                });
                return;
            }

            if !plugin.prompt.is_empty() {
                ui.label(&plugin.prompt);
            }
            let edit = ui.add(egui::TextEdit::singleline(&mut self.input).desired_width(f32::INFINITY));
            if std::mem::take(&mut self.wants_focus) {
                edit.request_focus();
            }
            start = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if !plugin.hint.is_empty() {
                ui.label(RichText::new(&plugin.hint).weak().small());
            }
            if let Some(problem) = &self.problem {
                ui.add_space(4.0);
                ui.colored_label(ERROR, problem);
            }
            ui.add_space(4.0);
            start |= ui.add_enabled(!self.input.trim().is_empty(), egui::Button::new(RichText::new("Start").strong())).clicked();
        });
        if start {
            if let Some(workspace) = workspace {
                self.start(ctx, workspace, settings);
            }
        }
        if !open {
            // Dropping the run asks the plugin to stop.
            *self = Self::default();
        }
        None
    }
}

/// What a plugin needs to send a file of this project.
pub struct PublishEnv<'a> {
    pub workspace: &'a Workspace,
    pub settings: &'a Settings,
    pub name: &'a str,
    pub meta: &'a BTreeMap<String, String>,
}

/// What there is to send at the moment.
#[derive(Debug, PartialEq)]
pub enum Offer {
    /// These files, the main one first, of the export number `version`.
    Files { files: Vec<PathBuf>, version: u64 },
    /// Nothing, for this reason.
    Nothing(String),
}

/// The line that opens what a plugin asks, unless `enabled` is off.
fn header(ui: &mut egui::Ui, title: &str, enabled: bool, open: bool, hint: Option<&str>) -> egui::Response {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y + 4.0);
    let (rect, response) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if ui.is_rect_visible(rect) {
        let visuals = if enabled { *ui.style().interact(&response) } else { ui.visuals().widgets.noninteractive };
        let color = if enabled { visuals.text_color() } else { ui.visuals().weak_text_color() };
        let fill = if enabled { visuals.weak_bg_fill } else { Color32::TRANSPARENT };
        let painter = ui.painter();
        painter.rect(rect, visuals.rounding, fill, visuals.bg_stroke);

        let center = egui::pos2(rect.left() + 12.0, rect.center().y);
        let (x, y) = (egui::vec2(4.0, 0.0), egui::vec2(0.0, 4.0));
        let arrow = if open { vec![center - x - y, center + x - y, center + y] } else { vec![center - x - y, center + x, center - x + y] };
        painter.add(Shape::convex_polygon(arrow, color, Stroke::NONE));
        let text = egui::pos2(rect.left() + 24.0, rect.center().y);
        painter.text(text, Align2::LEFT_CENTER, title, TextStyle::Body.resolve(ui.style()), color);
        if let Some(hint) = hint {
            let end = egui::pos2(rect.right() - 8.0, rect.center().y);
            painter.text(end, Align2::RIGHT_CENTER, hint, TextStyle::Small.resolve(ui.style()), ui.visuals().weak_text_color());
        }
    }
    response.on_hover_cursor(if enabled { CursorIcon::PointingHand } else { CursorIcon::NotAllowed })
}

/// How wide the widest label of a form is.
fn labels_width(ui: &egui::Ui, form: &Form) -> f32 {
    let font = TextStyle::Body.resolve(ui.style());
    let width = |label: &String| ui.fonts(|fonts| fonts.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE).size().x);
    form.fields.iter().map(|field| width(&field.label)).fold(0.0, f32::max)
}

/// The menu of a plugin that sends a file. It opens only when there is something to send, and what
/// it asks is up to the plugin.
pub struct PublishSection {
    plugin: Plugin,
    open: bool,
    /// The file picked when there are several. The main one otherwise.
    chosen: Option<PathBuf>,
    target: Option<PublishTarget>,
    /// The export that `target` belongs to.
    version: Option<u64>,
    /// The form has to be made again: it was just opened, whatever was wrong may be fixed by now.
    needs_form: bool,
    form: Option<Form>,
    values: BTreeMap<String, String>,
    preparing: Option<PluginRun>,
    sending: Option<PluginRun>,
    /// What has gone out of this export.
    sent: Vec<String>,
    problem: Option<String>,
    cancelled: bool,
    /// The plugin was asked to stop and is finishing what can't be left halfway.
    stopping: bool,
}

impl PublishSection {
    pub fn new(plugin: Plugin) -> Self {
        Self {
            plugin,
            open: false,
            chosen: None,
            target: None,
            version: None,
            needs_form: false,
            form: None,
            values: BTreeMap::new(),
            preparing: None,
            sending: None,
            sent: Vec::new(),
            problem: None,
            cancelled: false,
            stopping: false,
        }
    }

    pub fn is_sending(&self) -> bool {
        self.sending.is_some()
    }

    /// Makes the form again if the file is not the one it describes. A file on its way stays.
    fn follow(&mut self, ctx: &egui::Context, files: &[PathBuf], version: u64, env: &PublishEnv) {
        if self.is_sending() {
            return;
        }
        let Some(file) = self.chosen.as_ref().filter(|chosen| files.contains(chosen)).or(files.first()).cloned() else { return };
        let other_file = self.version != Some(version) || self.target.as_ref().is_none_or(|target| target.file != file);
        if !other_file && !self.needs_form {
            return;
        }
        if other_file {
            self.sent.clear();
        }
        self.needs_form = false;
        self.version = Some(version);
        self.cancelled = false;
        let data = env.workspace.plugin_data_dir(&self.plugin.id);
        self.problem = fs::create_dir_all(&data).err().map(|error| format!("Can't create {}: {error}", data.display()));
        self.target = Some(PublishTarget { file, data, name: env.name.to_owned(), meta: env.meta.clone() });
        self.prepare(ctx, env.settings);
    }

    fn prepare(&mut self, ctx: &egui::Context, settings: &Settings) {
        if let Some(target) = &self.target {
            self.preparing = Some(PluginRun::prepare(&self.plugin, target, &settings.options_of(&self.plugin.id), notifier(ctx)));
        }
    }

    fn send(&mut self, ctx: &egui::Context, settings: &Settings) {
        if let Some(target) = &self.target {
            self.problem = None;
            self.cancelled = false;
            self.stopping = false;
            self.sending = Some(PluginRun::send(&self.plugin, target, &self.values, &settings.options_of(&self.plugin.id), notifier(ctx)));
        }
    }

    /// Called whatever the panel shows: what a plugin does goes on while the other tab is open.
    pub fn poll(&mut self, ctx: &egui::Context, settings: &Settings) {
        match self.preparing.as_ref().map(PluginRun::status) {
            Some(RunStatus::Prepared(form)) => {
                self.values = if self.values.is_empty() { form.initial_values() } else { form.keep_values(&self.values) };
                self.form = Some(form);
                self.preparing = None;
            }
            Some(RunStatus::Failed(problem)) => {
                // The form that is there describes something else now.
                self.form = None;
                self.problem = Some(problem);
                self.preparing = None;
            }
            Some(RunStatus::Running { .. }) | None => {}
            Some(_) => self.preparing = None,
        }
        match self.sending.as_ref().map(PluginRun::status) {
            Some(RunStatus::Sent(message)) => {
                self.sent.push(message);
                self.sending = None;
                // What the plugin says about this file has changed now.
                self.prepare(ctx, settings);
            }
            Some(RunStatus::Failed(problem)) => {
                self.problem = Some(problem);
                self.sending = None;
            }
            Some(RunStatus::Running { .. }) | None => {}
            Some(_) => {
                self.cancelled = true;
                self.sending = None;
            }
        }
    }

    fn fields(&mut self, ui: &mut egui::Ui, form: &Form) {
        let width = (ui.available_width() - labels_width(ui, form) - COLUMN_GAP).at_least(MIN_FIELD_WIDTH);
        egui::Grid::new(("publish_fields", &self.plugin.id)).num_columns(2).spacing([COLUMN_GAP, 6.0]).show(ui, |ui| {
            for field in &form.fields {
                ui.label(&field.label);
                let value = self.values.entry(field.key.clone()).or_default();
                match field.kind {
                    FieldKind::Text => {
                        ui.add_sized([width, 20.0], egui::TextEdit::singleline(value).hint_text(&field.hint));
                    }
                    FieldKind::Choice => {
                        let shown = field.choices.iter().find(|choice| choice.value == *value).map_or_else(|| value.clone(), |choice| choice.label.clone());
                        egui::ComboBox::from_id_salt(("publish_field", &self.plugin.id, &field.key)).selected_text(shown).width(width).show_ui(ui, |ui| {
                            for choice in &field.choices {
                                ui.selectable_value(value, choice.value.clone(), &choice.label);
                            }
                        });
                    }
                }
                ui.end_row();
            }
        });
    }

    /// Which of the exported files goes, when there are several.
    fn file_choice(&mut self, ui: &mut egui::Ui, files: &[PathBuf]) {
        let name = |path: &Path| path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let current = self.target.as_ref().map_or_else(String::new, |target| name(&target.file));
        ui.horizontal(|ui| {
            ui.label("File");
            let width = ui.available_width() - COLUMN_GAP;
            egui::ComboBox::from_id_salt(("publish_file", &self.plugin.id)).selected_text(current).width(width).show_ui(ui, |ui| {
                for file in files {
                    let selected = self.target.as_ref().is_some_and(|target| &target.file == file);
                    if ui.selectable_label(selected, name(file)).clicked() {
                        self.chosen = Some(file.clone());
                    }
                }
            });
        });
    }

    /// Returns whether the user asked to send.
    fn body(&mut self, ui: &mut egui::Ui, files: &[PathBuf]) -> bool {
        let sending = self.sending.as_ref().map(PluginRun::status);
        let mut send = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_enabled_ui(sending.is_none(), |ui| {
                if files.len() > 1 {
                    self.file_choice(ui, files);
                }
            });
            let Some(form) = self.form.clone() else {
                match &self.problem {
                    Some(problem) => {
                        ui.colored_label(ERROR, problem);
                    }
                    None => {
                        ui.horizontal(|ui| progress_line(ui, None, "Looking at the file", 0.0));
                    }
                }
                return;
            };

            ui.label(RichText::new(&form.summary).weak());
            ui.add_space(4.0);
            ui.add_enabled_ui(sending.is_none(), |ui| self.fields(ui, &form));
            ui.add_space(4.0);
            for warning in form.warnings_for(&self.values) {
                ui.colored_label(WARNING, warning);
            }
            for problem in &form.problems {
                ui.colored_label(ERROR, problem);
            }
            for message in &self.sent {
                ui.colored_label(SENT, message);
            }
            if let Some(problem) = &self.problem {
                ui.colored_label(ERROR, problem);
            }
            if self.cancelled {
                ui.label(RichText::new(CANCELLED_TEXT).weak());
            }
            ui.add_space(4.0);

            if let Some(RunStatus::Running { fraction, text }) = &sending {
                ui.horizontal(|ui| {
                    progress_line(ui, *fraction, text, ui.available_width() - CANCEL_WIDTH);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = if self.stopping { "Stopping…" } else { "Cancel" };
                        if ui.add_enabled(!self.stopping, egui::Button::new(text)).clicked() {
                            if let Some(run) = &self.sending {
                                run.cancel();
                                self.stopping = true;
                            }
                        }
                    });
                });
            } else {
                // Only this button sends: Enter in a field going out to a channel would be a surprise.
                let ready = form.problems.is_empty() && self.preparing.is_none();
                send = ui.add_enabled(ready, egui::Button::new(RichText::new(form.action_text(&self.values)).strong())).clicked();
            }
        });
        send
    }

    pub fn show(&mut self, ui: &mut egui::Ui, offer: &Offer, env: &PublishEnv) {
        // What is on its way stays in sight, whatever happens to the video meanwhile.
        let reason = match offer {
            Offer::Nothing(reason) if !self.is_sending() => Some(reason.as_str()),
            _ => None,
        };
        let open = self.open && reason.is_none();
        let hint = reason.or((self.is_sending() && !open).then_some("Sending…"));
        let name = self.plugin.name.clone();
        if header(ui, &name, reason.is_none(), open, hint).clicked() {
            self.open = !self.open;
            self.needs_form = self.open;
        }
        if reason.is_some() || !self.open {
            return;
        }
        let files: &[PathBuf] = match offer {
            Offer::Files { files, version } => {
                self.follow(ui.ctx(), files, *version, env);
                files
            }
            Offer::Nothing(_) => &[],
        };
        if self.body(ui, files) {
            self.send(ui.ctx(), env.settings);
        }
    }
}

//! The screens before a project is open.

use crate::core::plugins::{Plugin, PluginKind};
use crate::core::project::{Project, PROJECT_FILE};
use egui::{Button, RichText};
use std::path::{Path, PathBuf};

const COLUMN_WIDTH: f32 = 620.0;

/// A line of the recent projects.
pub struct RecentProject {
    pub dir: PathBuf,
    pub name: String,
    /// There is a project in the folder. Trying to open a damaged one says what is wrong with it.
    pub found: bool,
}

impl RecentProject {
    pub fn read(dir: &Path) -> Self {
        let fallback = || dir.file_name().map_or_else(|| dir.display().to_string(), |name| name.to_string_lossy().into_owned());
        let name = Project::name_in(dir).unwrap_or_else(fallback);
        Self { dir: dir.to_owned(), found: dir.join(PROJECT_FILE).is_file(), name }
    }
}

pub enum StartAction {
    ChooseWorkspace,
    NewProject,
    OpenProject,
    OpenPlugin(usize),
    OpenRecent(PathBuf),
    ForgetRecent(PathBuf),
}

/// A centered column in the central panel.
fn column(ctx: &egui::Context, contents: impl FnOnce(&mut egui::Ui)) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let width = ui.available_width().min(COLUMN_WIDTH);
        ui.horizontal_top(|ui| {
            ui.add_space((ui.available_width() - width) / 2.0);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.add_space(32.0);
                contents(ui);
            });
        });
    });
}

pub fn start_screen(
    ctx: &egui::Context,
    workspace: Option<&Path>,
    recent: &[RecentProject],
    waiting_video: Option<&Path>,
    plugins: &[Plugin],
) -> Option<StartAction> {
    let mut action = None;
    column(ctx, |ui| {
        ui.label(RichText::new("DicingSplicing").size(26.0).strong());
        ui.add_space(16.0);
        let Some(workspace) = workspace else {
            ui.label("Choose a folder for your projects. They are kept there, together with the prepared frames of their videos.");
            if let Some(name) = waiting_video.and_then(Path::file_name) {
                ui.label(RichText::new(format!("{} opens right after that.", name.to_string_lossy())).weak());
            }
            ui.add_space(8.0);
            if ui.button("Choose a folder…").clicked() {
                action = Some(StartAction::ChooseWorkspace);
            }
            return;
        };

        ui.horizontal(|ui| {
            if ui.button(RichText::new("New project from a video…").strong()).clicked() {
                action = Some(StartAction::NewProject);
            }
            if ui.button("Open a project…").clicked() {
                action = Some(StartAction::OpenProject);
            }
            for (i, plugin) in plugins.iter().enumerate().filter(|(_, plugin)| plugin.kind == PluginKind::Source) {
                if ui.button(format!("{}…", plugin.name)).clicked() {
                    action = Some(StartAction::OpenPlugin(i));
                }
            }
        });
        ui.label(RichText::new("Or drop a video into this window.").weak());
        ui.add_space(16.0);

        if !recent.is_empty() {
            ui.heading("Recent projects");
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(ui.available_height() - 40.0).show(ui, |ui| {
                for project in recent {
                    ui.horizontal(|ui| {
                        let forget = ui.small_button("✖").on_hover_text("Remove from this list. The project stays on the disk");
                        if forget.clicked() {
                            action = Some(StartAction::ForgetRecent(project.dir.clone()));
                        }
                        if project.found {
                            let open = ui.add(Button::new(project.name.as_str()).frame(false)).on_hover_text(project.dir.display().to_string());
                            if open.clicked() {
                                action = Some(StartAction::OpenRecent(project.dir.clone()));
                            }
                        } else {
                            ui.label(RichText::new(format!("{} (not found)", project.name)).weak()).on_hover_text(project.dir.display().to_string());
                        }
                    });
                }
            });
            ui.add_space(8.0);
        }
        ui.label(RichText::new(format!("Projects are kept in {}", workspace.display())).weak().small());
    });
    action
}

pub fn opening_screen(ctx: &egui::Context, label: &str) {
    column(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Opening {label}…"));
        });
    });
}

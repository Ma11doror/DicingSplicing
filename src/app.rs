//! The window: settings, the workspace, opening projects and hosting the editor.

use crate::core::crash;
use crate::core::plugins::{self, Fetched, Plugin};
use crate::core::project::{self, Project, PROJECT_FILE};
use crate::core::settings::{self, Settings};
use crate::core::tools;
use crate::core::workspace::Workspace;
use crate::media::probe;
use crate::media::Notify;
use crate::ui::editor::Editor;
use crate::ui::keys::KeysWindow;
use crate::ui::plugins::SourceWindow;
use crate::ui::settings::SettingsWindow;
use crate::ui::start::{self, RecentProject, StartAction};
use crate::ui::pick_video;
use anyhow::Result;
use egui::Color32;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
const WARNING: Color32 = Color32::from_rgb(230, 170, 60);

/// The window's title: the editor and its version, which feedback should mention.
pub fn title() -> String {
    format!("Simple Editor {}", crash::VERSION)
}

/// A new project, made in the background: its video has to be read first.
struct Opened {
    project: Project,
    dir: PathBuf,
    cache_root: PathBuf,
}

/// A project to make from a video, in the background.
struct NewProject {
    video: PathBuf,
    workspace: Workspace,
    name: Option<String>,
    meta: BTreeMap<String, String>,
}

impl NewProject {
    /// The project's folder appears only when its video has turned out to open.
    fn run(self) -> Result<Opened> {
        let Self { video, workspace, name, meta } = self;
        let video = std::path::absolute(&video).unwrap_or(video);
        let info = probe::probe(&video)?;
        let name = name.unwrap_or_else(|| video.file_stem().map_or_else(|| "project".to_owned(), |stem| stem.to_string_lossy().into_owned()));
        let dir = workspace.new_project_dir(&name)?;
        let mut project = Project::new(name, video, info.duration);
        if let Some(source) = project.sources.first_mut() {
            source.meta = meta;
        }
        project.save(&dir)?;
        Ok(Opened { project, dir, cache_root: workspace.cache_dir() })
    }
}

pub struct EditorApp {
    settings: Settings,
    settings_on_disk: Settings,
    settings_path: PathBuf,
    settings_window: SettingsWindow,
    workspace: Option<Workspace>,
    editor: Option<Editor>,
    opening: Option<(String, Receiver<Result<Opened>>)>,
    recent: Vec<RecentProject>,
    /// A video to open as soon as there is a workspace.
    waiting_video: Option<PathBuf>,
    plugins: Vec<Plugin>,
    /// The plugins, while they are still being looked for.
    finding_plugins: Option<Receiver<(Vec<Plugin>, Vec<String>)>>,
    source_window: SourceWindow,
    /// What the plugin's window brings in goes into the open project, not a new one.
    fetch_into_editor: bool,
    errors: Vec<String>,
    /// An error report written since the user last looked: something broke last time.
    crash_report: Option<PathBuf>,
    keys_window: KeysWindow,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let settings_path = settings::default_path();
        let (settings, problem) = Settings::load(&settings_path);
        settings.apply_programs();
        let mut errors: Vec<String> = problem.into_iter().collect();
        errors.extend([tools::FFMPEG, tools::FFPROBE].into_iter().filter_map(|tool| tools::check(tool).err()).map(|e| format!("{e:#}")));

        // A folder that is gone (say, an unplugged drive) is not created again: that would look as
        // if all the projects had disappeared.
        let workspace = settings.workspace.as_deref().and_then(|root| {
            if !root.is_dir() {
                errors.push(format!("The projects folder {} is not there. Is it on a drive that's not connected?", root.display()));
                return None;
            }
            match Workspace::open_or_create(root) {
                Ok(workspace) => Some(workspace),
                Err(error) => {
                    errors.push(format!("{error:#}"));
                    None
                }
            }
        });

        // In the background: a plugin is a program that has to be run to find out what it is.
        let (found, finding_plugins) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        thread::spawn(move || {
            let _ = found.send(plugins::discover(&plugins::default_folders()));
            ctx.request_repaint();
        });

        let mut app = Self {
            settings_on_disk: settings.clone(),
            settings,
            settings_path,
            settings_window: SettingsWindow::default(),
            workspace,
            editor: None,
            opening: None,
            recent: Vec::new(),
            waiting_video: None,
            plugins: Vec::new(),
            finding_plugins: Some(finding_plugins),
            source_window: SourceWindow::default(),
            fetch_into_editor: false,
            errors,
            crash_report: None,
            keys_window: KeysWindow::default(),
        };
        app.crash_report = crash::newest_report(&crash::logs_folder()).filter(|report| {
            let name = report.file_name().map(|name| name.to_string_lossy().into_owned());
            name != app.settings.crash_seen
        });
        app.refresh_recent();
        // `simple_editor <video>`: also what "Open with" and dropping a file on the exe do.
        if let Some(path) = std::env::args_os().nth(1) {
            app.open_path(PathBuf::from(path), &cc.egui_ctx);
        }
        app
    }

    fn refresh_recent(&mut self) {
        self.recent = self.settings.recent.iter().map(|dir| RecentProject::read(dir)).collect();
    }

    fn start_opening(&mut self, label: String, job: NewProject, ctx: &egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        thread::spawn(move || {
            let _ = sender.send(job.run());
            notify();
        });
        self.opening = Some((label, receiver));
    }

    fn show_project(&mut self, project: Project, dir: PathBuf, cache_root: PathBuf, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — {}", project.name, title())));
        self.settings.remember_project(&dir);
        self.editor = Some(Editor::new(project, dir, cache_root, ctx));
    }

    fn close_project(&mut self, ctx: &egui::Context) {
        if let Some(mut editor) = self.editor.take() {
            editor.flush();
            self.errors.extend(editor.save_error.take());
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title()));
        self.refresh_recent();
    }

    /// Opens whatever was dropped or asked for: a project, or a video.
    fn open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if path.is_dir() {
            self.open_project(path, ctx);
        } else if path.file_name().is_some_and(|name| name == PROJECT_FILE) {
            let dir = path.parent().map_or_else(PathBuf::new, Path::to_owned);
            self.open_project(dir, ctx);
        } else {
            self.open_video(path, ctx);
        }
    }

    /// Opens the project of this video, making one if there is none yet.
    fn open_video(&mut self, video: PathBuf, ctx: &egui::Context) {
        let Some(workspace) = &self.workspace else {
            self.waiting_video = Some(video);
            self.close_project(ctx);
            return;
        };
        match project::find_by_video(&workspace.projects_dir(), &video) {
            Some(dir) => self.open_project(dir, ctx),
            None => self.new_project(video, None, BTreeMap::new(), ctx),
        }
    }

    /// Opens what a plugin brought in, like any other video.
    fn open_fetched(&mut self, fetched: Fetched, ctx: &egui::Context) {
        let Fetched { path, name, meta } = fetched;
        let existing = self.workspace.as_ref().and_then(|workspace| project::find_by_video(&workspace.projects_dir(), &path));
        match existing {
            Some(dir) => self.open_project(dir, ctx),
            None => self.new_project(path, name, meta, ctx),
        }
    }

    fn new_project(&mut self, video: PathBuf, name: Option<String>, meta: BTreeMap<String, String>, ctx: &egui::Context) {
        let Some(workspace) = self.workspace.clone() else { return };
        self.close_project(ctx);
        let label = name.clone().unwrap_or_else(|| project::file_label(&video));
        self.start_opening(label, NewProject { video, workspace, name, meta }, ctx);
    }

    fn poll_plugins(&mut self) {
        let Some(receiver) = &self.finding_plugins else { return };
        if let Ok((plugins, problems)) = receiver.try_recv() {
            self.plugins = plugins;
            self.errors.extend(problems);
            self.finding_plugins = None;
        }
    }

    fn open_project(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.close_project(ctx);
        let project = match Project::load(&dir) {
            Ok(project) => project,
            Err(error) => {
                self.errors.push(format!("{error:#}"));
                return;
            }
        };
        // The project's own workspace, if it is in one: its videos' frames are prepared there.
        if let Some(workspace) = Workspace::containing(&dir) {
            self.settings.workspace = Some(workspace.root.clone());
            self.workspace = Some(workspace);
        }
        let Some(cache_root) = self.workspace.as_ref().map(Workspace::cache_dir) else {
            self.errors.push(format!("{} is not in a projects folder. Choose one first.", dir.display()));
            return;
        };
        // Its videos are looked at in the editor: one that is not there is marked, not in the way.
        self.show_project(project, dir, cache_root, ctx);
    }

    fn choose_workspace(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Folder for your projects");
        if let Some(current) = &self.workspace {
            dialog = dialog.set_directory(&current.root);
        }
        let Some(root) = dialog.pick_folder() else { return };
        match Workspace::open_or_create(&root) {
            Ok(workspace) => {
                self.settings.workspace = Some(root);
                self.workspace = Some(workspace);
            }
            Err(error) => {
                self.errors.push(format!("{error:#}"));
                return;
            }
        }
        if let Some(video) = self.waiting_video.take() {
            self.open_video(video, ctx);
        }
    }

    fn poll_opening(&mut self, ctx: &egui::Context) {
        let result = match &self.opening {
            Some((_, receiver)) => receiver.try_recv(),
            None => return,
        };
        if let Err(TryRecvError::Empty) = result {
            return;
        }
        let Some((label, _)) = self.opening.take() else { return };
        match result {
            Ok(Ok(Opened { project, dir, cache_root })) => self.show_project(project, dir, cache_root, ctx),
            Ok(Err(error)) => {
                self.errors.push(format!("Can't open {label}: {error:#}"));
                self.refresh_recent();
            }
            Err(_) => {}
        }
    }

    fn handle_start_action(&mut self, action: StartAction, ctx: &egui::Context) {
        match action {
            StartAction::ChooseWorkspace => self.choose_workspace(ctx),
            StartAction::NewProject => {
                if let Some(video) = pick_video() {
                    self.new_project(video, None, BTreeMap::new(), ctx);
                }
            }
            StartAction::OpenPlugin(i) => {
                if let Some(plugin) = self.plugins.get(i) {
                    self.source_window.open(plugin.clone());
                    self.fetch_into_editor = false;
                }
            }
            StartAction::OpenProject => {
                let mut dialog = rfd::FileDialog::new().set_title("Open a project");
                if let Some(workspace) = &self.workspace {
                    dialog = dialog.set_directory(workspace.projects_dir());
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.open_project(dir, ctx);
                }
            }
            StartAction::OpenRecent(dir) => self.open_project(dir, ctx),
            StartAction::ForgetRecent(dir) => {
                self.settings.forget_project(&dir);
                self.refresh_recent();
            }
        }
    }

    fn top_panel(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut open_settings = false;
        let mut open_keys = false;
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if let Some(editor) = &mut self.editor {
                    close = ui.button("◀ Projects").on_hover_text("Close this project. It is saved").clicked();
                    if let Some(plugin) = editor.top_bar(ui, &self.plugins).and_then(|i| self.plugins.get(i)) {
                        self.source_window.open(plugin.clone());
                        self.fetch_into_editor = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_settings = ui.button("Settings").clicked();
                    open_keys = ui.button("Keys").on_hover_text("What the keys and the mouse do (F1)").clicked();
                });
            });
            if let Some(report) = self.crash_report.clone() {
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("✖").on_hover_text("Don't remind about this one").clicked() {
                        self.settings.crash_seen = report.file_name().map(|name| name.to_string_lossy().into_owned());
                        self.crash_report = None;
                    }
                    ui.colored_label(WARNING, "Last time something went wrong in the editor. Its report helps to fix it:");
                    if ui.small_button("Show the report").on_hover_text(report.display().to_string()).clicked() {
                        crash::show_in_folder(&report);
                    }
                });
            }
            let mut dismissed = None;
            for (i, error) in self.errors.iter().enumerate() {
                // Errors name long paths: they wrap, and the button stays in sight.
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("✖").clicked() {
                        dismissed = Some(i);
                    }
                    ui.colored_label(ERROR, error);
                });
            }
            if let Some(i) = dismissed {
                self.errors.remove(i);
            }
            ui.add_space(4.0);
        });
        if close {
            self.close_project(ctx);
        }
        if open_settings {
            self.settings_window.open();
        }
        if open_keys || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1)) {
            self.keys_window.toggle();
        }
        self.keys_window.show(ctx);
    }

    fn save_settings(&mut self) {
        if self.settings != self.settings_on_disk {
            match self.settings.save(&self.settings_path) {
                Ok(()) => self.settings_on_disk = self.settings.clone(),
                Err(error) => {
                    // Once: the next change tries again.
                    self.settings_on_disk = self.settings.clone();
                    self.errors.push(format!("The settings can't be saved: {error:#}"));
                }
            }
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_opening(ctx);
        self.poll_plugins();
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));

        self.top_panel(ctx);
        let workspace_root = self.workspace.as_ref().map(|workspace| workspace.root.clone());
        if self.settings_window.show(ctx, &mut self.settings, workspace_root.as_deref(), &self.plugins) {
            self.choose_workspace(ctx);
        }
        if let Some(fetched) = self.source_window.show(ctx, self.workspace.as_ref(), &self.settings) {
            match &mut self.editor {
                Some(editor) if self.fetch_into_editor => editor.add_source(fetched.path, fetched.name, fetched.meta),
                _ => self.open_fetched(fetched, ctx),
            }
        }

        if let Some(path) = dropped {
            // A video dropped on an open project joins its sources; a project opens as it is.
            let is_project = path.is_dir() || path.file_name().is_some_and(|name| name == PROJECT_FILE);
            match &mut self.editor {
                Some(editor) if !is_project => editor.add_source(path, None, BTreeMap::new()),
                _ => self.open_path(path, ctx),
            }
        }
        if let Some(editor) = &mut self.editor {
            editor.ui(ctx, &self.plugins, self.workspace.as_ref(), &self.settings);
        } else if let Some((label, _)) = &self.opening {
            start::opening_screen(ctx, label);
        } else {
            let action = start::start_screen(ctx, workspace_root.as_deref(), &self.recent, self.waiting_video.as_deref(), &self.plugins);
            if let Some(action) = action {
                self.handle_start_action(action, ctx);
            }
        }
        self.save_settings();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(editor) = &mut self.editor {
            editor.flush();
        }
        self.save_settings();
    }
}

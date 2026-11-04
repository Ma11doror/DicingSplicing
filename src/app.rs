//! The window: settings, the workspace, opening projects and hosting the editor.

use crate::core::plugins::{self, Fetched, Plugin};
use crate::core::project::{self, Project, PROJECT_FILE};
use crate::core::settings::{self, Settings};
use crate::core::tools;
use crate::core::workspace::Workspace;
use crate::media::cache::{CacheStatus, MediaCache};
use crate::media::Notify;
use crate::ui::editor::Editor;
use crate::ui::plugins::SourceWindow;
use crate::ui::settings::SettingsWindow;
use crate::ui::start::{self, MissingAction, RecentProject, StartAction};
use anyhow::{Context, Result};
use egui::{Color32, RichText};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "webm", "avi", "m4v"];
const ERROR: Color32 = Color32::from_rgb(230, 80, 70);
const TITLE: &str = "Simple Editor";

/// What opening in the background comes to.
struct Opened {
    cache: MediaCache,
    project: Project,
    dir: PathBuf,
}

/// What to open in the background.
enum OpenJob {
    Existing { project: Box<Project>, dir: PathBuf, cache_root: PathBuf },
    New { video: PathBuf, workspace: Workspace, name: Option<String>, meta: BTreeMap<String, String> },
}

impl OpenJob {
    fn run(self, notify: Notify) -> Result<Opened> {
        match self {
            Self::Existing { project, dir, cache_root } => {
                let source = project.sources.first().context("the project has no video")?;
                let cache = MediaCache::open_in(&cache_root, &source.path, notify)?;
                Ok(Opened { cache, project: *project, dir })
            }
            // The project's folder appears only when its video has turned out to open.
            Self::New { video, workspace, name, meta } => {
                let video = std::path::absolute(&video).unwrap_or(video);
                let cache = MediaCache::open_in(&workspace.cache_dir(), &video, notify)?;
                let name = name.unwrap_or_else(|| video.file_stem().map_or_else(|| "project".to_owned(), |stem| stem.to_string_lossy().into_owned()));
                let dir = workspace.new_project_dir(&name)?;
                let mut project = Project::new(name, video, cache.info.duration);
                if let Some(source) = project.sources.first_mut() {
                    source.meta = meta;
                }
                project.save(&dir)?;
                Ok(Opened { cache, project, dir })
            }
        }
    }
}

/// A project whose video has to be found first.
struct Missing {
    project: Box<Project>,
    dir: PathBuf,
    cache_root: PathBuf,
}

pub struct EditorApp {
    settings: Settings,
    settings_on_disk: Settings,
    settings_path: PathBuf,
    settings_window: SettingsWindow,
    workspace: Option<Workspace>,
    editor: Option<Editor>,
    opening: Option<(String, Receiver<Result<Opened>>)>,
    missing: Option<Missing>,
    recent: Vec<RecentProject>,
    /// A video to open as soon as there is a workspace.
    waiting_video: Option<PathBuf>,
    plugins: Vec<Plugin>,
    /// The plugins, while they are still being looked for.
    finding_plugins: Option<Receiver<(Vec<Plugin>, Vec<String>)>>,
    source_window: SourceWindow,
    errors: Vec<String>,
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
            missing: None,
            recent: Vec::new(),
            waiting_video: None,
            plugins: Vec::new(),
            finding_plugins: Some(finding_plugins),
            source_window: SourceWindow::default(),
            errors,
        };
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

    fn start_opening(&mut self, label: String, job: OpenJob, ctx: &egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        thread::spawn(move || {
            let _ = sender.send(job.run(notify.clone()));
            notify();
        });
        self.opening = Some((label, receiver));
    }

    fn close_project(&mut self, ctx: &egui::Context) {
        if let Some(mut editor) = self.editor.take() {
            editor.flush();
            self.errors.extend(editor.save_error.take());
        }
        self.missing = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(TITLE.to_owned()));
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
        let label = name.clone().unwrap_or_else(|| file_name(&video));
        self.start_opening(label, OpenJob::New { video, workspace, name, meta }, ctx);
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
        let source_exists = project.sources.first().is_some_and(|source| source.path.is_file());
        let project = Box::new(project);
        if source_exists {
            self.start_opening(project.name.clone(), OpenJob::Existing { project, dir, cache_root }, ctx);
        } else {
            self.missing = Some(Missing { project, dir, cache_root });
        }
    }

    fn locate_missing_video(&mut self, ctx: &egui::Context) {
        let Some(video) = pick_video() else { return };
        let Some(Missing { mut project, dir, cache_root }) = self.missing.take() else { return };
        if let Some(source) = project.sources.first_mut() {
            source.path = video;
        }
        if let Err(error) = project.save(&dir) {
            self.errors.push(format!("{error:#}"));
        }
        self.start_opening(project.name.clone(), OpenJob::Existing { project, dir, cache_root }, ctx);
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
            Ok(Ok(Opened { cache, project, dir })) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — {TITLE}", project.name)));
                self.settings.remember_project(&dir);
                self.editor = Some(Editor::new(cache, project, dir, ctx));
            }
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
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if let Some(editor) = &self.editor {
                    close = ui.button("◀ Projects").on_hover_text("Close this project. It is saved").clicked();
                    ui.label(RichText::new(&editor.name).strong()).on_hover_text(editor.cache.source.display().to_string());
                    match editor.cache.status() {
                        CacheStatus::Building(progress) => {
                            let text = format!("Preparing frames {:.0}%", progress * 100.0);
                            ui.add(egui::ProgressBar::new(progress).desired_width(220.0).text(text));
                            ctx.request_repaint_after(Duration::from_millis(250));
                        }
                        CacheStatus::Failed(error) => {
                            ui.colored_label(ERROR, format!("Preparing failed: {error}"));
                        }
                        CacheStatus::Ready => {}
                    }
                    if let Some(error) = &editor.save_error {
                        ui.colored_label(ERROR, error);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_settings = ui.button("Settings").clicked();
                });
            });
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
            self.open_fetched(fetched, ctx);
        }

        if let Some(path) = dropped {
            self.open_path(path, ctx);
        }
        if let Some(editor) = &mut self.editor {
            editor.ui(ctx, &self.plugins, self.workspace.as_ref(), &self.settings);
        } else if let Some((label, _)) = &self.opening {
            start::opening_screen(ctx, label);
        } else if let Some(missing) = &self.missing {
            let path = missing.project.sources.first().map_or_else(PathBuf::new, |source| source.path.clone());
            match start::missing_source_screen(ctx, &missing.project.name, &path) {
                Some(MissingAction::Locate) => self.locate_missing_video(ctx),
                Some(MissingAction::Back) => self.close_project(ctx),
                None => {}
            }
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

fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a video")
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_file()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

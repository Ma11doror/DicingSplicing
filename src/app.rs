//! The window: opening a source and hosting the editor.

use crate::core::tools;
use crate::media::cache::{CacheStatus, MediaCache};
use crate::media::Notify;
use crate::ui::editor::Editor;
use egui::{Color32, RichText};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "webm", "avi", "m4v"];

pub struct EditorApp {
    editor: Option<Editor>,
    opening: Option<(PathBuf, Receiver<anyhow::Result<MediaCache>>)>,
    errors: Vec<String>,
}

impl EditorApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let errors = [tools::FFMPEG, tools::FFPROBE]
            .into_iter()
            .filter_map(|tool| tools::check(tool).err())
            .map(|e| format!("{e:#}"))
            .collect();
        let mut app = Self { editor: None, opening: None, errors };
        // `simple_editor <video>`: also what "Open with" and dropping a file on the exe do.
        if let Some(path) = std::env::args_os().nth(1) {
            app.open(PathBuf::from(path), &cc.egui_ctx);
        }
        app
    }

    fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.editor = None;
        let (sender, receiver) = mpsc::channel();
        let notify: Notify = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let source = path.clone();
        thread::spawn(move || {
            let _ = sender.send(MediaCache::open(&source, notify.clone()));
            notify();
        });
        self.opening = Some((path, receiver));
    }

    fn poll_opening(&mut self, ctx: &egui::Context) {
        let result = match &self.opening {
            Some((_, receiver)) => receiver.try_recv(),
            None => return,
        };
        if let Err(TryRecvError::Empty) = result {
            return;
        }
        let Some((path, _)) = self.opening.take() else { return };
        match result {
            Ok(Ok(cache)) => {
                let title = format!("{} — Simple Editor", file_name(&path));
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
                self.editor = Some(Editor::new(cache, ctx));
            }
            Ok(Err(e)) => self.errors.push(format!("Can't open {}: {e:#}", path.display())),
            Err(_) => {}
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_opening(ctx);
        let mut open_path = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("📂 Open video…").clicked() {
                    open_path = pick_video();
                }
                if let Some((path, _)) = &self.opening {
                    ui.spinner();
                    ui.label(format!("Opening {}…", file_name(path)));
                }
                if let Some(editor) = &self.editor {
                    ui.label(RichText::new(file_name(&editor.cache.source)).strong());
                    match editor.cache.status() {
                        CacheStatus::Building(progress) => {
                            let text = format!("Preparing frames {:.0}%", progress * 100.0);
                            ui.add(egui::ProgressBar::new(progress).desired_width(220.0).text(text));
                            ctx.request_repaint_after(Duration::from_millis(250));
                        }
                        CacheStatus::Failed(error) => {
                            ui.colored_label(Color32::from_rgb(230, 80, 70), format!("Preparing failed: {error}"));
                        }
                        CacheStatus::Ready => {}
                    }
                }
            });
            let mut dismissed = None;
            for (i, error) in self.errors.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(230, 80, 70), error);
                    if ui.small_button("✖").clicked() {
                        dismissed = Some(i);
                    }
                });
            }
            if let Some(i) = dismissed {
                self.errors.remove(i);
            }
            ui.add_space(4.0);
        });

        if let Some(path) = open_path {
            self.open(path, ctx);
        }
        match &mut self.editor {
            Some(editor) => editor.ui(ctx),
            None => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Open a video or drop it into the window").size(18.0).weak());
                    });
                });
            }
        }
    }
}

fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open video")
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_file()
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}


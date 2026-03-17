use crate::core::manager::SessionManager;
use crate::core::project::ProjectStatus;
use crate::core::tasks;
use eframe::egui;
use std::process::Command; // Added for opening files

pub struct EditorApp {
    manager: SessionManager,
}

impl EditorApp {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        let mut manager = SessionManager::new();
        // Create an empty project on startup
        manager.create_new_project();
        Self { manager }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // --- Header ---
            ui.horizontal(|ui| {
                if ui.button("➕ New Project").clicked() {
                    self.manager.create_new_project();
                }
                ui.label(format!("Projects: {}", self.manager.projects.len()));
            });
            ui.separator();

            // --- Active Project View ---
            if let Some(project) = self.manager.get_active_project() {
                // Get a snapshot of status to avoid holding the lock for too long
                let status_snapshot = { project.status.lock().unwrap().clone() };

                // Enable inputs only if not processing
                let is_enabled = matches!(
                    status_snapshot,
                    ProjectStatus::Idle | ProjectStatus::Ready | ProjectStatus::Error(_)
                );

                ui.add_enabled_ui(is_enabled, |ui| {
                    let mut data = project.data.lock().unwrap();
                    ui.heading("Video Settings");
                    ui.label("YouTube URL:");
                    ui.text_edit_singleline(&mut data.url);
                    ui.add_space(10.0);
                    ui.add(egui::Slider::new(&mut data.start_time, 0..=300).text("Start (s)"));
                    ui.add(egui::Slider::new(&mut data.duration, 10..=60).text("Duration (s)"));
                });

                ui.add_space(20.0);

                // --- Action Buttons ---
                match status_snapshot {
                    ProjectStatus::Idle | ProjectStatus::Error(_) => {
                        if ui.button("⬇ Download Video").clicked() {
                            tasks::start_download(project.clone_project_reference());
                        }
                        if let ProjectStatus::Error(e) = status_snapshot {
                            ui.colored_label(egui::Color32::RED, format!("Error: {}", e));
                        }
                    }
                    ProjectStatus::Downloading(_) => {
                        ui.spinner();
                        ui.label("Downloading... Please wait.");
                    }
                    ProjectStatus::Ready => {
                        ui.colored_label(egui::Color32::GREEN, "✅ Video Downloaded");

                        let path_str = {
                            let data = project.data.lock().unwrap();
                            data.source_path.clone().unwrap_or_default()
                        };
                        ui.monospace(format!("File: {}", path_str));

                        ui.horizontal(|ui| {
                            if ui.button("▶ Play Source").clicked() {
                                #[cfg(target_os = "windows")]
                                Command::new("cmd")
                                    .args(["/C", "start", "", &path_str])
                                    .spawn()
                                    .ok();
                            }

                            ui.separator();

                            // Trigger Processing
                            if ui.button("✂ Make Circle Video").clicked() {
                                tasks::start_processing(project.clone_project_reference());
                            }
                        });
                    }
                    ProjectStatus::Processing => {
                        ui.spinner();
                        ui.label("Processing Video (FFmpeg)...");
                    }
                    ProjectStatus::Done(result_path) => {
                        ui.colored_label(egui::Color32::GREEN, "✨ Circle Video Ready!");
                        ui.monospace(&result_path);

                        ui.horizontal(|ui| {
                            if ui.button("▶ Play Result").clicked() {
                                #[cfg(target_os = "windows")]
                                Command::new("cmd")
                                    .args(["/C", "start", "", &result_path])
                                    .spawn()
                                    .ok();
                            }

                            if ui.button("📂 Show in Folder").clicked() {
                                #[cfg(target_os = "windows")]
                                Command::new("explorer")
                                    .arg("/select,")
                                    .arg(&result_path)
                                    .spawn()
                                    .ok();
                            }
                        });

                        ui.separator();

                        // Option to re-do (reset to Ready state)
                        if ui.button("↩ Adjust Settings").clicked() {
                            let mut s = project.status.lock().unwrap();
                            *s = ProjectStatus::Ready;
                        }
                    }
                    _ => {}
                }
            } else {
                ui.label("No active project.");
            }
        });
    }
}

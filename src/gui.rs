use crate::core::manager::SessionManager;
use crate::core::project::ProjectStatus;
use crate::core::tasks;
use eframe::egui;
use std::process::Command; // Added for opening files

pub struct EditorApp {
    manager: SessionManager,
    texture: Option<egui::TextureHandle>,
    last_texture_id: Option<uuid::Uuid>,
}

impl EditorApp {
    pub fn new(_cc: &eframe::CreationContext) -> Self {
        egui_extras::install_image_loaders(&_cc.egui_ctx);

        let mut manager = SessionManager::new();
        manager.create_new_project();
        Self {
            manager,
            texture: None,
            last_texture_id: None,
        }
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

            if let Some(project) = self.manager.get_active_project() {
                let status_snapshot = { project.status.lock().unwrap().clone() };

                // --- Texture Loading Logic ---
                // Check if background task produced a new frame
                {
                    let mut frame_lock = project.preview_frame.lock().unwrap();
                    if let Some(img) = frame_lock.take() {
                        self.texture = Some(ctx.load_texture(
                            "preview_frame",
                            img,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }

                // --- Two Column Layout ---
                ui.columns(2, |columns| {
                    let ui_settings = &mut columns[0];
                    let ui_preview = &mut columns[1];

                    // === COLUMN 1: Controls ===
                    ui_settings.heading("Settings");

                    let is_enabled = matches!(
                        status_snapshot,
                        ProjectStatus::Idle | ProjectStatus::Ready | ProjectStatus::Error(_)
                    );

                    ui_settings.add_enabled_ui(is_enabled, |ui| {
                        let mut data = project.data.lock().unwrap();
                        ui.label("YouTube URL:");
                        ui.text_edit_singleline(&mut data.url);

                        ui.add_space(10.0);
                        ui.label("Time Range:");

                        // Update preview when slider is dragged
                        if ui
                            .add(egui::Slider::new(&mut data.start_time, 0..=300).text("Start (s)"))
                            .changed()
                        {
                            tasks::update_preview(project.clone_project_reference());
                        }
                        ui.add(egui::Slider::new(&mut data.duration, 10..=60).text("Duration (s)"));
                    });

                    ui_settings.add_space(20.0);

                    // Action Buttons
                    match status_snapshot {
                        ProjectStatus::Idle | ProjectStatus::Error(_) => {
                            if ui_settings.button("⬇ Download Video").clicked() {
                                tasks::start_download(project.clone_project_reference());
                            }
                            if let ProjectStatus::Error(e) = status_snapshot {
                                ui_settings
                                    .colored_label(egui::Color32::RED, format!("Error: {}", e));
                            }
                        }
                        ProjectStatus::Downloading(_) => {
                            ui_settings.spinner();
                            ui_settings.label("Downloading...");
                        }
                        ProjectStatus::Ready => {
                            if ui_settings.button("✂ Render Circle").clicked() {
                                tasks::start_processing(project.clone_project_reference());
                            }
                        }
                        ProjectStatus::Done(path) => {
                            ui_settings.colored_label(egui::Color32::GREEN, "Done!");
                            if ui_settings.button("Open File").clicked() {
                                #[cfg(target_os = "windows")]
                                Command::new("explorer").arg(&path).spawn().ok();
                            }
                            if ui_settings.button("Back to Edit").clicked() {
                                *project.status.lock().unwrap() = ProjectStatus::Ready;
                            }
                        }
                        _ => {}
                    }

                    // === COLUMN 2: Visual Preview ===
                    ui_preview.heading("Preview");

                    if let Some(texture) = &self.texture {
                        // Calculate display size preserving aspect ratio
                        let available_w = ui_preview.available_width();
                        let aspect = texture.aspect_ratio();
                        let display_h = available_w / aspect;

                        // Reserve space for image
                        let (rect, _response) = ui_preview.allocate_exact_size(
                            egui::vec2(available_w, display_h),
                            egui::Sense::drag(), // Prepare for drag interaction
                        );

                        let painter = ui_preview.painter_at(rect);

                        // 1. Draw Video Frame
                        painter.image(
                            texture.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );

                        // 2. Calculate Crop Box (Center Square)
                        let min_side = rect.width().min(rect.height());
                        let box_size = min_side;
                        let center = rect.center();
                        let crop_rect =
                            egui::Rect::from_center_size(center, egui::vec2(box_size, box_size));

                        // 3. Draw Mask (Darken outside)
                        let mask_color = egui::Color32::from_black_alpha(200);

                        // Top area
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                rect.min,
                                egui::pos2(rect.max.x, crop_rect.min.y),
                            ),
                            0.0,
                            mask_color,
                        );
                        // Bottom area
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(rect.min.x, crop_rect.max.y),
                                rect.max,
                            ),
                            0.0,
                            mask_color,
                        );
                        // Left area
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(rect.min.x, crop_rect.min.y),
                                egui::pos2(crop_rect.min.x, crop_rect.max.y),
                            ),
                            0.0,
                            mask_color,
                        );
                        // Right area
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(crop_rect.max.x, crop_rect.min.y),
                                egui::pos2(rect.max.x, crop_rect.max.y),
                            ),
                            0.0,
                            mask_color,
                        );

                        // 4. Draw Border
                        painter.rect_stroke(
                            crop_rect,
                            0.0,
                            egui::Stroke::new(2.0, egui::Color32::WHITE),
                        );
                    } else {
                        ui_preview.label("No preview available.");
                        // Auto-trigger preview if ready but texture missing
                        if matches!(status_snapshot, ProjectStatus::Ready) && self.texture.is_none()
                        {
                            tasks::update_preview(project.clone_project_reference());
                        }
                    }
                });
            } else {
                ui.label("No active project.");
            }
        });
    }
}

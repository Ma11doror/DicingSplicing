#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod core;
mod gui;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Initialize Tokio runtime
    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    let _enter = rt.enter(); // Keep runtime active for the duration of the app

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 450.0])
            .with_min_inner_size([300.0, 200.0]),
        ..Default::default()
    };

    eframe::run_native(
        "TG Circle Editor",
        native_options,
        Box::new(|cc| Ok(Box::new(gui::EditorApp::new(cc)))),
    )
}

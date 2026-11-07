#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod core;
mod media;
mod ui;

fn main() -> eframe::Result<()> {
    // Before anything can go wrong: without a console, the report is all that is left of it.
    core::crash::install(core::crash::logs_folder());
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::title())
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "Simple Editor",
        native_options,
        Box::new(|cc| Ok(Box::new(app::EditorApp::new(cc)))),
    )
}

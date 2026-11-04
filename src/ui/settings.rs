//! The settings window.

use crate::core::plugins::{OptionKind, Plugin};
use crate::core::settings::Settings;
use crate::core::tools;
use egui::{Color32, RichText};
use std::path::Path;

const OK: Color32 = Color32::from_rgb(120, 190, 120);
const PROBLEM: Color32 = Color32::from_rgb(230, 80, 70);
/// How many words of a program's version line are worth showing.
const VERSION_WORDS: usize = 3;
const FIELD_WIDTH: f32 = 260.0;

#[derive(Default)]
pub struct SettingsWindow {
    open: bool,
    /// What each program answers, from the last time they were asked.
    answers: Option<[Result<String, String>; 2]>,
}

fn ask(program: &str) -> Result<String, String> {
    match tools::check(program) {
        Ok(version) => Ok(version.split_whitespace().take(VERSION_WORDS).collect::<Vec<_>>().join(" ")),
        Err(error) => Err(format!("{error:#}")),
    }
}

impl SettingsWindow {
    pub fn open(&mut self) {
        self.open = true;
        self.answers = None;
    }

    /// Returns true when the user wants to choose another workspace.
    pub fn show(&mut self, ctx: &egui::Context, settings: &mut Settings, workspace: Option<&Path>, plugins: &[Plugin]) -> bool {
        if !self.open {
            return false;
        }
        let answers = self.answers.get_or_insert_with(|| [ask(tools::FFMPEG), ask(tools::FFPROBE)]);
        let mut change_workspace = false;
        let mut ask_again = false;
        let mut open = self.open;
        egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.heading("Projects folder");
            ui.horizontal(|ui| {
                match workspace {
                    Some(path) => ui.label(path.display().to_string()),
                    None => ui.label(RichText::new("Not chosen yet").weak()),
                };
                change_workspace = ui.button("Change…").clicked();
            });
            ui.label(RichText::new("A project that is open stays where it is. New ones go to the new folder.").weak().small());
            ui.separator();

            ui.heading("Programs");
            ui.label(RichText::new("Leave a line empty if the program is on the PATH.").weak().small());
            egui::Grid::new("programs").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                let lines = [(tools::FFMPEG, &mut settings.ffmpeg), (tools::FFPROBE, &mut settings.ffprobe)];
                for ((name, path), answer) in lines.into_iter().zip(answers.iter()) {
                    ui.label(name);
                    let edit = ui.add_sized([FIELD_WIDTH, 20.0], egui::TextEdit::singleline(path).hint_text("From the PATH"));
                    ask_again |= edit.lost_focus();
                    match answer {
                        Ok(version) => ui.colored_label(OK, version),
                        Err(_) => ui.colored_label(PROBLEM, "Not found"),
                    }
                    .on_hover_text(answer.as_ref().map_or_else(String::clone, String::clone));
                    ui.end_row();
                }
            });

            for plugin in plugins.iter().filter(|plugin| !plugin.options.is_empty()) {
                ui.separator();
                ui.heading(&plugin.name);
                let values = settings.plugin_options.entry(plugin.id.clone()).or_default();
                egui::Grid::new(("plugin_options", &plugin.id)).num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
                    for option in &plugin.options {
                        ui.label(&option.label);
                        let value = values.entry(option.key.clone()).or_default();
                        ui.add_sized([FIELD_WIDTH, 20.0], egui::TextEdit::singleline(value).hint_text(&option.hint)).on_hover_text(&option.hint);
                        if option.kind == OptionKind::File && ui.button("…").clicked() {
                            if let Some(path) = rfd::FileDialog::new().set_title(&option.label).pick_file() {
                                *value = path.display().to_string();
                            }
                        }
                        ui.end_row();
                    }
                });
            }
        });
        self.open = open;
        if ask_again {
            settings.apply_programs();
            self.answers = None;
        }
        change_workspace
    }
}

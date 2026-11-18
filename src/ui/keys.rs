//! What the keys and the mouse do, in one window: opened by the Keys button in the top bar, or F1.
//! Arrows are written as words: the fonts have no arrow characters.

/// Each group: its title and the lines in it, what to press and what it does.
const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Playing",
        &[
            ("Space", "Play or pause"),
            ("K / L", "Pause / play"),
            ("Left / Right", "One frame back / forward"),
            ("Shift+Left / Shift+Right", "One second back / forward"),
            ("Up / Down", "To the start / the end of the export range"),
            ("Home / End", "To the start / the end of the clips"),
        ],
    ),
    (
        "Export range",
        &[("I", "Start it at the playhead"), ("O", "End it at the playhead")],
    ),
    (
        "Clips",
        &[
            ("[ / ]", "Trim the selected clip's start / end to the playhead"),
            ("Ctrl+B", "Cut the selected clip at the playhead"),
            ("F", "Freeze frame: hold the frame at the playhead for 2 s; the rest of the clip and the clips after it move later"),
            ("S", "Slow motion: play a second of the recording from the playhead half as fast; the rest of the clip and the clips after it move later"),
            ("Ctrl+D", "Duplicate the selected clips, right after them, as far apart as they are"),
            ("Ctrl+C / Ctrl+V", "Copy the selected clips / paste them at the playhead, each onto its track; where that has no room then, onto the nearest track above that has"),
            ("Ctrl+Shift+V", "Paste them where the mouse pointer is, onto the track under it"),
            ("Ctrl+X", "Copy and delete: the selected clips go to be pasted"),
            ("Delete", "Delete the selected clips"),
            ("No key for now", "Delete them and move the clips after each on its track back by its length: it is in a clip's menu"),
            ("Ctrl+A / Ctrl+Shift+A", "Select all the clips / none"),
            ("Ctrl+Z", "Undo"),
            ("Ctrl+Y, Ctrl+Shift+Z", "Redo"),
        ],
    ),
    (
        "Tools",
        &[("V or Esc", "Select: pick, move and trim clips"), ("B", "Blade: a click cuts a clip, with Shift every clip there")],
    ),
    (
        "Mouse on the timeline",
        &[
            ("Click on the ruler", "Take the playhead there"),
            ("Click on a clip", "Select it"),
            ("Ctrl+click on a clip", "Add it to the selected clips, or take it out"),
            (
                "Shift+click on a clip",
                "Select the clips from the one clicked last to this one: on one track those between them, on several those of the tracks between them that lie between the start of the first and the end of the last",
            ),
            ("Ctrl+Shift+click", "The same, besides those selected"),
            ("Drag a clip", "Move it, also onto another track of its kind; the clips chained to it go along. Several selected move together in time"),
            ("Drag an edge of the selected clip", "Trim it"),
            ("Drag the dots on its top edge", "Change the length of its fade"),
            (
                "Drag the dots on its bottom edge",
                "A frame it holds: the left ones hold another frame, the right ones hold it longer or shorter. A part in slow motion: they move where it starts and ends in the recording",
            ),
            ("Right click on a clip", "Its menu: detach its sound, chain it to another clip, fades, color, speed, a cut there, a freeze frame, slow motion, delete"),
            ("Right click on a frame held or a part in slow motion", "Remove freeze or Remove slow motion, as well as the clip's menu"),
            ("Double click on a clip", "Unfold or fold its track: effects and sound"),
            ("Drag the ruler or an empty place", "Move along the time, and over the tracks up and down too"),
            ("Drag with the right or middle button", "The same, wherever it starts"),
            ("Wheel", "Zoom; over the track names or the scroll bar, scroll the tracks"),
            ("Drag the dots of a track", "Move the track up or down"),
            ("Drag a source from Sources", "Put it on the timeline there"),
        ],
    ),
    (
        "Mouse on the pictures",
        &[
            ("Drag in Output", "Move the selected clip's picture; the wheel zooms it"),
            ("Drag inside the frame in the middle", "Move the frame (Alt: no snapping)"),
            ("Drag a side of the frame", "Resize it (Ctrl: the opposite side stays)"),
            ("Drag an arrow at a corner", "Rotate (Shift: 15 degree steps)"),
        ],
    ),
    ("Window", &[("F1", "This list"), ("Drag the dots above the timeline", "Make it taller or lower"), ("Drag the dots above Clip", "Share the right column between Output and Clip")]),
];

#[derive(Default)]
pub struct KeysWindow {
    open: bool,
}

impl KeysWindow {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let mut open = self.open;
        egui::Window::new("Keys").open(&mut open).collapsible(false).resizable(false).default_width(560.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() * 0.75).show(ui, |ui| {
                for (n, (title, lines)) in GROUPS.iter().enumerate() {
                    if n > 0 {
                        ui.add_space(6.0);
                    }
                    ui.strong(*title);
                    egui::Grid::new(("keys", n)).num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                        for (keys, what) in lines.iter() {
                            ui.label(egui::RichText::new(*keys).monospace());
                            ui.label(*what);
                            ui.end_row();
                        }
                    });
                }
            });
        });
        self.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_has_no_arrow_characters() {
        for (title, lines) in GROUPS {
            for (keys, what) in lines.iter() {
                let text = format!("{title} {keys} {what}");
                assert!(!text.chars().any(|c| ('\u{2190}'..='\u{21FF}').contains(&c)), "{text}");
            }
        }
    }
}

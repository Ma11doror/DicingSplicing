pub mod editor;
pub mod export;
pub mod flash;
pub mod framing;
pub mod picture;
pub mod plugins;
pub mod settings;
pub mod sources;
pub mod start;
pub mod timeline;
pub mod viewer;

use std::path::PathBuf;

pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "webm", "avi", "m4v"];

/// Asks for a video file.
pub fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Choose a video")
        .add_filter("Video", VIDEO_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_file()
}

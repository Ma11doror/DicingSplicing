pub mod audio;
pub mod cache;
pub mod frames;
pub mod geometry;
pub mod color;
pub mod library;
pub mod player;
pub mod probe;
pub mod render;
pub mod stretch;
pub mod timecode;
pub mod waveform;

use crate::core::edit::SourceId;
use crate::media::probe::MediaInfo;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Wakes the UI when background work has something new to show.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// The ffmpeg filters that lay an image's colours over black, so that what is transparent in it is
/// black, like the bars. Without them ffmpeg keeps whatever colour a transparent pixel has.
pub const OVER_BLACK: &str = "format=rgba,premultiply=inplace=1";

/// A source's file and what is in it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub info: MediaInfo,
}

/// The files of a project's sources that are there and could be read.
pub type SourceFiles = BTreeMap<SourceId, SourceFile>;

pub mod audio;
pub mod cache;
pub mod frames;
pub mod geometry;
pub mod library;
pub mod player;
pub mod probe;
pub mod render;
pub mod timecode;

use crate::core::edit::SourceId;
use crate::media::probe::MediaInfo;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Wakes the UI when background work has something new to show.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// A source's file and what is in it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub info: MediaInfo,
}

/// The files of a project's sources that are there and could be read.
pub type SourceFiles = BTreeMap<SourceId, SourceFile>;

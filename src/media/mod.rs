pub mod audio;
pub mod cache;
pub mod frames;
pub mod geometry;
pub mod player;
pub mod probe;
pub mod timecode;

use std::sync::Arc;

/// Wakes the UI when background work has something new to show.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

//! The media of every source of a project: what is in each file, its frames prepared for the
//! editor and its sound. Files are looked at in the background as soon as they are added, and their
//! frames prepared one file at a time: two preparations at once would only slow each other down.

use crate::core::edit::SourceId;
use crate::media::cache::{CacheStatus, MediaCache};
use crate::media::probe::{self, MediaInfo};
use crate::media::{Notify, SourceFile, SourceFiles};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

/// Where a source is, as the user sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceState {
    /// Its file is being looked at.
    Opening,
    /// Its frames wait for another file's to be prepared.
    Waiting,
    /// Its frames are being prepared, this far.
    Preparing(f32),
    Ready,
    /// There is no file where the project says.
    Missing,
    Failed(String),
}

enum Item {
    Probing(Receiver<Result<Probed>>),
    Waiting(MediaInfo),
    Starting(MediaInfo, Receiver<Result<MediaCache>>),
    Cache(MediaCache),
    Missing,
    Failed(String),
}

/// What looking at a file found: what is in it, and its frames if they were prepared before.
struct Probed {
    info: MediaInfo,
    cache: Option<MediaCache>,
}

struct Entry {
    path: PathBuf,
    item: Item,
}

pub struct Library {
    /// Where the prepared frames go.
    root: PathBuf,
    notify: Notify,
    entries: BTreeMap<SourceId, Entry>,
}

impl Library {
    pub fn new(root: PathBuf, notify: Notify) -> Self {
        Self { root, notify, entries: BTreeMap::new() }
    }

    /// Starts looking at the file of `id`. Whatever was known of it before is forgotten.
    pub fn add(&mut self, id: SourceId, path: &Path) {
        let item = if path.is_file() {
            let (sender, receiver) = mpsc::channel();
            let (root, file, notify) = (self.root.clone(), path.to_owned(), self.notify.clone());
            thread::spawn(move || {
                let probed = probe::probe(&file).map(|info| Probed { cache: MediaCache::existing(&root, &file, &info), info });
                let _ = sender.send(probed);
                notify();
            });
            Item::Probing(receiver)
        } else {
            Item::Missing
        };
        self.entries.insert(id, Entry { path: path.to_owned(), item });
    }

    /// Forgets a source, and stops preparing its frames.
    pub fn remove(&mut self, id: SourceId) {
        self.entries.remove(&id);
    }

    /// Takes in what the background work has found, and starts preparing the next file's frames
    /// when nothing is being prepared. Returns the sources whose contents became known.
    pub fn poll(&mut self) -> Vec<(SourceId, MediaInfo)> {
        let mut known = Vec::new();
        for (&id, entry) in &mut self.entries {
            match &entry.item {
                Item::Probing(receiver) => match receiver.try_recv() {
                    Ok(Ok(Probed { info, cache })) => {
                        known.push((id, info.clone()));
                        entry.item = cache.map_or(Item::Waiting(info), Item::Cache);
                    }
                    Ok(Err(error)) => entry.item = Item::Failed(format!("{error:#}")),
                    Err(TryRecvError::Disconnected) => entry.item = Item::Failed("the file could not be read".to_owned()),
                    Err(TryRecvError::Empty) => {}
                },
                Item::Starting(_, receiver) => match receiver.try_recv() {
                    Ok(Ok(cache)) => entry.item = Item::Cache(cache),
                    Ok(Err(error)) => entry.item = Item::Failed(format!("{error:#}")),
                    Err(TryRecvError::Disconnected) => entry.item = Item::Failed("preparing the frames did not start".to_owned()),
                    Err(TryRecvError::Empty) => {}
                },
                _ => {}
            }
        }

        let busy = self.entries.values().any(|entry| match &entry.item {
            Item::Starting(..) => true,
            Item::Cache(cache) => matches!(cache.status(), CacheStatus::Building(_)),
            _ => false,
        });
        let next = self.entries.values_mut().find(|entry| matches!(entry.item, Item::Waiting(_)));
        if let (false, Some(entry)) = (busy, next) {
            if let Item::Waiting(info) = &entry.item {
                let info = info.clone();
                let (sender, receiver) = mpsc::channel();
                let (root, file, notify, build_info) = (self.root.clone(), entry.path.clone(), self.notify.clone(), info.clone());
                thread::spawn(move || {
                    let _ = sender.send(MediaCache::build_in(&root, &file, build_info, notify.clone()));
                    notify();
                });
                entry.item = Item::Starting(info, receiver);
            }
        }
        known
    }

    pub fn state(&self, id: SourceId) -> SourceState {
        match self.entries.get(&id).map(|entry| &entry.item) {
            None | Some(Item::Probing(_)) => SourceState::Opening,
            Some(Item::Waiting(_)) => SourceState::Waiting,
            Some(Item::Starting(..)) => SourceState::Preparing(0.0),
            Some(Item::Cache(cache)) => match cache.status() {
                CacheStatus::Ready => SourceState::Ready,
                CacheStatus::Building(progress) => SourceState::Preparing(progress),
                CacheStatus::Failed(error) => SourceState::Failed(error),
            },
            Some(Item::Missing) => SourceState::Missing,
            Some(Item::Failed(error)) => SourceState::Failed(error.clone()),
        }
    }

    /// What is in the file, once it has been looked at.
    pub fn info(&self, id: SourceId) -> Option<&MediaInfo> {
        match &self.entries.get(&id)?.item {
            Item::Waiting(info) | Item::Starting(info, _) => Some(info),
            Item::Cache(cache) => Some(&cache.info),
            _ => None,
        }
    }

    /// The frames and sound prepared for the editor, complete or still growing.
    pub fn cache(&self, id: SourceId) -> Option<&MediaCache> {
        match &self.entries.get(&id)?.item {
            Item::Cache(cache) => Some(cache),
            _ => None,
        }
    }

    /// The files that can be rendered from: those that are there and could be read.
    pub fn files(&self) -> SourceFiles {
        self.entries
            .iter()
            .filter_map(|(&id, entry)| Some((id, SourceFile { path: entry.path.clone(), info: self.info(id)?.clone() })))
            .collect()
    }

    /// How many files still have their frames to be prepared, and how far the one being prepared is.
    pub fn preparing(&self) -> Option<(usize, f32)> {
        let mut count = 0;
        let mut progress = 0.0;
        for &id in self.entries.keys() {
            match self.state(id) {
                SourceState::Waiting => count += 1,
                SourceState::Preparing(p) => {
                    count += 1;
                    progress = p;
                }
                _ => {}
            }
        }
        (count > 0).then_some((count, progress))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools;
    use std::fs;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn make_video(path: &Path, seconds: u32) {
        let picture = format!("testsrc=size=160x120:rate=25:duration={seconds}");
        let status = tools::command(tools::FFMPEG)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &picture, "-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Polls until `done` says so, and returns every state that was seen on the way.
    fn poll_until(library: &mut Library, done: impl Fn(&Library) -> bool) -> Vec<(SourceState, SourceState)> {
        let started = Instant::now();
        let mut seen = Vec::new();
        while !done(library) {
            library.poll();
            let states = (library.state(0), library.state(1));
            if seen.last() != Some(&states) {
                seen.push(states);
            }
            assert!(started.elapsed() < Duration::from_secs(60), "stuck at {seen:?}");
            thread::sleep(Duration::from_millis(5));
        }
        seen
    }

    #[test]
    fn prepares_one_file_at_a_time_and_reuses_what_was_prepared() {
        if tools::check(tools::FFMPEG).is_err() || tools::check(tools::FFPROBE).is_err() {
            eprintln!("ffmpeg/ffprobe not found, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("simple_editor_library_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
        make_video(&a, 2);
        make_video(&b, 1);
        let notify: Notify = Arc::new(|| {});
        let both_ready = |library: &Library| library.state(0) == SourceState::Ready && library.state(1) == SourceState::Ready;

        let mut library = Library::new(dir.join("cache"), notify.clone());
        library.add(0, &a);
        library.add(1, &b);
        library.add(2, &dir.join("gone.mp4"));
        assert_eq!(library.state(2), SourceState::Missing);
        let seen = poll_until(&mut library, both_ready);
        let preparing = |state: &SourceState| matches!(state, SourceState::Preparing(_));
        assert!(!seen.iter().any(|(a, b)| preparing(a) && preparing(b)), "both at once: {seen:?}");
        // Whichever was read first is prepared first; the other one waits for it.
        let waiting = |state: &SourceState| *state == SourceState::Waiting;
        assert!(seen.iter().any(|(a, b)| (preparing(a) && waiting(b)) || (preparing(b) && waiting(a))), "{seen:?}");
        assert_eq!(library.info(1).map(|info| info.duration.round()), Some(1.0));
        assert_eq!(library.files().keys().copied().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(library.preparing(), None);
        assert!(library.cache(0).unwrap().frame_count() >= 50);
        drop(library);

        // Opened again: the frames are there, nothing waits.
        let mut library = Library::new(dir.join("cache"), notify);
        library.add(0, &a);
        library.add(1, &b);
        let seen = poll_until(&mut library, both_ready);
        assert!(!seen.iter().any(|(a, b)| *a == SourceState::Waiting || *b == SourceState::Waiting), "{seen:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}

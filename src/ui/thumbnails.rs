//! Small pictures of sources, for the thumbnails of the Sources list: a frame from the editor's
//! cache of the source, or one that ffmpeg takes while the cache has not got that far yet. They are
//! made in the background, one at a time. The frame under the pointer goes first, and a frame asked
//! for before it, if it is not made yet, is not made at all.

use crate::core::edit::SourceId;
use crate::core::tools;
use crate::media::cache::MediaCache;
use crate::media::probe::MediaInfo;
use crate::media::{Notify, OVER_BLACK};
use egui::{ColorImage, TextureHandle, TextureId, TextureOptions, Vec2};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// Where in a video its thumbnail is taken from, as a part of its length: the very first frame is
/// often black, as many videos fade in from it.
pub const POSTER_AT: f64 = 0.1;

/// A picture that can be drawn, and its size in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    pub texture: TextureId,
    pub size: Vec2,
}

impl Shown {
    fn of(texture: &TextureHandle) -> Self {
        Self { texture: texture.id(), size: texture.size_vec2() }
    }
}

/// Where the JPEG of a frame comes from.
#[derive(Debug, Clone, PartialEq)]
enum Origin {
    /// The bytes of a frame in the editor's cache of the source.
    Cache { frames: PathBuf, offset: u64, len: u32 },
    /// ffmpeg takes the frame at `at` seconds from the file itself; of an `image`, its picture, with
    /// what is transparent in it black.
    Ffmpeg { file: PathBuf, at: f64, image: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum What {
    /// The picture a thumbnail shows when the pointer is not over it.
    Poster,
    /// A frame of the cache, by its number, for the thumbnail under the pointer.
    Frame(usize),
}

struct Job {
    id: SourceId,
    /// The source's file when it was asked for: a source can be pointed to another file meanwhile.
    path: PathBuf,
    what: What,
    origin: Origin,
    /// The picture fits in this many pixels.
    max: [u32; 2],
}

struct Made {
    id: SourceId,
    path: PathBuf,
    what: What,
    image: Result<ColorImage, String>,
}

#[derive(Default)]
struct Queue {
    posters: VecDeque<Job>,
    /// Only the last frame asked for: the pointer has moved on from the others.
    frame: Option<Job>,
    made: Vec<Made>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

enum Poster {
    Asked(PathBuf),
    Ready(PathBuf, TextureHandle),
    Failed(PathBuf),
}

impl Poster {
    fn path(&self) -> &Path {
        match self {
            Self::Asked(path) | Self::Ready(path, _) | Self::Failed(path) => path,
        }
    }
}

/// The thumbnails of a project's sources. Dropping this stops the work in the background.
pub struct Thumbnails {
    shared: Arc<Shared>,
    posters: HashMap<SourceId, Poster>,
    /// The frame drawn for the thumbnail under the pointer: of which source, which frame.
    frame: Option<(SourceId, usize, TextureHandle)>,
    /// The frame asked for last.
    wanted: Option<(SourceId, usize)>,
}

impl Thumbnails {
    pub fn new(notify: Notify) -> Self {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        thread::spawn(move || work(&worker, &notify));
        Self { shared, posters: HashMap::new(), frame: None, wanted: None }
    }

    /// Turns what has been made into textures.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let made = std::mem::take(&mut self.shared.queue.lock().unwrap().made);
        for Made { id, path, what, image } in made {
            match what {
                What::Poster => {
                    // Asked for another file since, or forgotten: this one is of no use.
                    if !matches!(self.posters.get(&id), Some(Poster::Asked(asked)) if *asked == path) {
                        continue;
                    }
                    let poster = match image {
                        Ok(image) => Poster::Ready(path, ctx.load_texture(format!("source poster {id}"), image, TextureOptions::LINEAR)),
                        Err(_) => Poster::Failed(path),
                    };
                    self.posters.insert(id, poster);
                }
                What::Frame(number) => {
                    let Ok(image) = image else { continue };
                    match &mut self.frame {
                        Some((shown_id, shown_number, texture)) => {
                            texture.set(image, TextureOptions::LINEAR);
                            (*shown_id, *shown_number) = (id, number);
                        }
                        None => self.frame = Some((id, number, ctx.load_texture("source frame under the pointer", image, TextureOptions::LINEAR))),
                    }
                }
            }
        }
    }

    /// The source's thumbnail, asked for if it isn't there yet. None for a source of sound only,
    /// one not yet read, and one whose picture couldn't be made.
    pub fn poster(&mut self, id: SourceId, path: &Path, info: Option<&MediaInfo>, cache: Option<&MediaCache>, max: [u32; 2]) -> Option<Shown> {
        match self.posters.get(&id) {
            Some(Poster::Ready(made_of, texture)) if made_of == path => return Some(Shown::of(texture)),
            Some(poster) if poster.path() == path => return None,
            _ => {}
        }
        let info = info.filter(|info| info.has_video())?;
        let origin = cache
            .and_then(|cache| cached(cache, poster_frame(cache)))
            .unwrap_or_else(|| Origin::Ffmpeg { file: path.to_owned(), at: info.duration * POSTER_AT, image: info.image });
        self.posters.insert(id, Poster::Asked(path.to_owned()));
        let job = Job { id, path: path.to_owned(), what: What::Poster, origin, max };
        self.shared.queue.lock().unwrap().posters.push_back(job);
        self.shared.wake.notify_one();
        None
    }

    /// The frame at `fraction` of the video, while the pointer moves over its thumbnail. Only from
    /// the editor's cache: where the cache has not got to yet, None, and the poster stays.
    pub fn frame(&mut self, id: SourceId, path: &Path, fraction: f32, cache: Option<&MediaCache>, max: [u32; 2]) -> Option<Shown> {
        let cache = cache?;
        let number = frame_at(fraction, cache.frame_count());
        let origin = cached(cache, number)?;
        if let Some((shown_id, shown_number, texture)) = &self.frame {
            if (*shown_id, *shown_number) == (id, number) {
                return Some(Shown::of(texture));
            }
        }
        if self.wanted != Some((id, number)) {
            self.wanted = Some((id, number));
            self.shared.queue.lock().unwrap().frame = Some(Job { id, path: path.to_owned(), what: What::Frame(number), origin, max });
            self.shared.wake.notify_one();
        }
        // Until the new one comes, the frame of this source drawn last stays: going back to the
        // poster for a moment would flicker.
        self.frame.as_ref().filter(|(shown_id, _, _)| *shown_id == id).map(|(_, _, texture)| Shown::of(texture))
    }

    /// Forgets the sources that are no longer in the project.
    pub fn keep(&mut self, keep: impl Fn(SourceId) -> bool) {
        self.posters.retain(|id, _| keep(*id));
        if self.frame.as_ref().is_some_and(|(id, _, _)| !keep(*id)) {
            self.frame = None;
            self.wanted = None;
        }
    }
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.shared.queue.lock() {
            queue.closed = true;
        }
        self.shared.wake.notify_all();
    }
}

/// The frame a poster is taken from.
fn poster_frame(cache: &MediaCache) -> usize {
    let frame = (cache.info.duration * POSTER_AT * cache.format.fps.as_f64()).floor() as usize;
    frame.min(cache.frame_count().saturating_sub(1))
}

/// The frame under the pointer at `fraction` of a thumbnail's width, of `count` frames.
fn frame_at(fraction: f32, count: usize) -> usize {
    ((fraction.clamp(0.0, 1.0) as f64 * count as f64).floor() as usize).min(count.saturating_sub(1))
}

/// Where the cache keeps this frame, if it has it yet.
fn cached(cache: &MediaCache, frame: usize) -> Option<Origin> {
    let (offset, len) = cache.frames.get(frame)?;
    Some(Origin::Cache { frames: cache.frames_path(), offset, len })
}

fn work(shared: &Shared, notify: &Notify) {
    loop {
        let job = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.frame.take().or_else(|| queue.posters.pop_front()) {
                    break job;
                }
                queue = shared.wake.wait(queue).unwrap();
            }
        };
        let image = make(&job.origin, job.max);
        shared.queue.lock().unwrap().made.push(Made { id: job.id, path: job.path, what: job.what, image });
        notify();
    }
}

/// The picture, scaled down to fit in `max`.
fn make(origin: &Origin, max: [u32; 2]) -> Result<ColorImage, String> {
    let jpeg = match origin {
        Origin::Cache { frames, offset, len } => read_at(frames, *offset, *len).map_err(|error| format!("can't read {}: {error}", frames.display()))?,
        Origin::Ffmpeg { file, at, image } => grab(file, *at, *image, max)?,
    };
    let picture = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).map_err(|error| error.to_string())?;
    let small = picture.thumbnail(max[0].max(1), max[1].max(1)).to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], small.as_raw()))
}

fn read_at(path: &Path, offset: u64, len: u32) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; len as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// One frame of the file, as a JPEG that fits in `max`; of an `image`, its picture over black.
fn grab(file: &Path, at: f64, image: bool, max: [u32; 2]) -> Result<Vec<u8>, String> {
    let flat = if image { format!("{OVER_BLACK},") } else { String::new() };
    let scale = format!("{flat}scale={}:{}:force_original_aspect_ratio=decrease:force_divisible_by=2", max[0].max(2), max[1].max(2));
    let output = tools::command(tools::FFMPEG)
        .args(["-v", "error", "-ss", &format!("{:.3}", at.max(0.0)), "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-vf", &scale, "-f", "image2pipe", "-c:v", "mjpeg", "-q:v", "4", "-"])
        .output()
        .map_err(|error| format!("ffmpeg doesn't start: {error}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!("ffmpeg took no frame: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_runs_through_all_the_frames() {
        assert_eq!(frame_at(0.0, 300), 0);
        assert_eq!(frame_at(0.5, 300), 150);
        assert_eq!(frame_at(0.999, 300), 299);
        // The right edge itself is the last frame, not one past it.
        assert_eq!(frame_at(1.0, 300), 299);
        assert_eq!(frame_at(-0.2, 300), 0);
        assert_eq!(frame_at(1.5, 300), 299);
        assert_eq!(frame_at(0.5, 0), 0);
    }

    /// A small JPEG, made by the same library that reads them.
    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let picture = image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 40]));
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut bytes).encode_image(&picture).unwrap();
        bytes
    }

    #[test]
    fn a_frame_of_the_cache_is_read_from_its_place_and_scaled_down() {
        let dir = std::env::temp_dir().join(format!("dicing_splicing_thumbnails_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (first, second) = (jpeg(64, 36), jpeg(960, 540));
        let frames = dir.join("frames.mjpeg");
        std::fs::write(&frames, [first.as_slice(), second.as_slice()].concat()).unwrap();

        let origin = Origin::Cache { frames: frames.clone(), offset: first.len() as u64, len: second.len() as u32 };
        let image = make(&origin, [288, 162]).unwrap();
        assert_eq!(image.size, [288, 162], "it keeps its shape and fits");
        let tall = Origin::Cache { frames: frames.clone(), offset: 0, len: first.len() as u32 };
        assert_eq!(make(&tall, [32, 32]).unwrap().size, [32, 18]);

        let past_the_end = Origin::Cache { frames, offset: 1 << 30, len: 10 };
        assert!(make(&past_the_end, [288, 162]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

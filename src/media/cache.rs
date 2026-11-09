//! Editor cache of a source file: every frame as a JPEG in one file plus an index, and the whole
//! audio track as PCM. It is built once with ffmpeg; after that seeking and scrubbing need no
//! ffmpeg at all.

use crate::core::tools;
use crate::media::probe::{MediaInfo, Rational};
use crate::media::Notify;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread;

/// Sample rate of the cached audio (interleaved stereo s16).
pub const AUDIO_RATE: u32 = 48_000;

/// Decoded audio of a source, available once the cache has it. Empty for silent sources.
pub type Pcm = Arc<OnceLock<Arc<Vec<i16>>>>;

/// Bump when the cache layout changes: older caches are rebuilt.
const VERSION: u32 = 1;
/// Frames are scaled down so that their shorter side is at most this.
const FRAME_SHORT_SIDE: u32 = 540;
/// Sources above this frame rate are cached at half (or a quarter...) of it.
const FRAME_MAX_FPS: f64 = 30.5;
/// ffmpeg `-q:v` for the cached JPEGs: 2 is best, 31 is worst.
const JPEG_QUALITY: &str = "4";

const META_FILE: &str = "meta.json";
const FRAMES_FILE: &str = "frames.mjpeg";
const INDEX_FILE: &str = "frames.idx";
const AUDIO_FILE: &str = "audio.pcm";

/// Size and rate of the cached frames.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameFormat {
    pub fps: Rational,
    pub width: u32,
    pub height: u32,
}

impl FrameFormat {
    fn for_source(info: &MediaInfo) -> Self {
        let mut fps = info.fps;
        while fps.as_f64() > FRAME_MAX_FPS {
            fps.den *= 2;
        }
        let scale = (FRAME_SHORT_SIDE as f64 / info.width.min(info.height) as f64).min(1.0);
        let even = |v: u32| (((v as f64 * scale) / 2.0).round() as u32 * 2).max(2);
        Self { fps, width: even(info.width), height: even(info.height) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    version: u32,
    source: PathBuf,
    info: MediaInfo,
    format: FrameFormat,
    frames_complete: bool,
    audio_complete: bool,
}

/// Byte ranges of the frames in `frames.mjpeg`. Grows while the cache is being built.
#[derive(Default)]
pub struct FrameIndex(RwLock<Vec<(u64, u32)>>);

impl FrameIndex {
    pub fn get(&self, frame: usize) -> Option<(u64, u32)> {
        self.0.read().unwrap().get(frame).copied()
    }

    pub fn len(&self) -> usize {
        self.0.read().unwrap().len()
    }

    fn push(&self, offset: u64, len: u32) {
        self.0.write().unwrap().push((offset, len));
    }
}

pub enum CacheStatus {
    Ready,
    Building(f32),
    Failed(String),
}

pub struct MediaCache {
    pub source: PathBuf,
    pub dir: PathBuf,
    pub info: MediaInfo,
    pub format: FrameFormat,
    pub frames: Arc<FrameIndex>,
    pub audio: Pcm,
    build: Option<Build>,
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

struct Build {
    shared: Arc<BuildShared>,
    children: Vec<ChildSlot>,
}

struct BuildShared {
    dir: PathBuf,
    cancelled: AtomicBool,
    state: Mutex<BuildState>,
    notify: Notify,
}

struct BuildState {
    meta: Meta,
    error: Option<String>,
}

impl MediaCache {
    /// Opens the cache for `source` in the folder `root`, starting a background build if there is
    /// no complete one.
    #[cfg(test)]
    pub fn open_in(root: &Path, source: &Path, notify: Notify) -> Result<Self> {
        let info = crate::media::probe::probe(source)?;
        match Self::existing(root, source, &info) {
            Some(cache) => Ok(cache),
            None => Self::build_in(root, source, info, notify),
        }
    }

    /// The complete cache of `source` in the folder `root`, if there is one.
    pub fn existing(root: &Path, source: &Path, info: &MediaInfo) -> Option<Self> {
        Self::load(source, &root.join(cache_key(source).ok()?), info)
    }

    /// Starts building the cache of `source` in the folder `root` in the background.
    pub fn build_in(root: &Path, source: &Path, info: MediaInfo, notify: Notify) -> Result<Self> {
        Self::build(source, root.join(cache_key(source)?), info, notify)
    }

    pub fn frames_path(&self) -> PathBuf {
        self.dir.join(FRAMES_FILE)
    }

    pub fn status(&self) -> CacheStatus {
        let Some(build) = &self.build else { return CacheStatus::Ready };
        let state = build.shared.state.lock().unwrap();
        if let Some(error) = &state.error {
            return CacheStatus::Failed(error.clone());
        }
        if state.meta.frames_complete && state.meta.audio_complete {
            return CacheStatus::Ready;
        }
        let progress = self.frames.len() as f32 / self.expected_frames() as f32;
        CacheStatus::Building(progress.min(0.99))
    }

    /// Number of frames: exact once they are all cached, estimated from the duration before that.
    pub fn frame_count(&self) -> usize {
        let complete = self.build.as_ref().is_none_or(|b| b.shared.state.lock().unwrap().meta.frames_complete);
        if complete {
            self.frames.len().max(1)
        } else {
            self.expected_frames()
        }
    }

    fn expected_frames(&self) -> usize {
        ((self.info.duration * self.format.fps.as_f64()).ceil() as usize).max(1)
    }

    /// A complete cache from an earlier run, if there is one for exactly this source.
    fn load(source: &Path, dir: &Path, info: &MediaInfo) -> Option<Self> {
        let meta: Meta = serde_json::from_slice(&fs::read(dir.join(META_FILE)).ok()?).ok()?;
        if meta.version != VERSION || !meta.frames_complete || !meta.audio_complete || &meta.info != info {
            return None;
        }
        let entries = read_index(&dir.join(INDEX_FILE)).ok()?;
        let end = entries.last().map(|&(offset, len)| offset + len as u64)?;
        if fs::metadata(dir.join(FRAMES_FILE)).ok()?.len() != end {
            return None;
        }
        let samples = if info.has_audio { pcm_from_bytes(&fs::read(dir.join(AUDIO_FILE)).ok()?) } else { Vec::new() };
        Some(Self {
            source: source.to_path_buf(),
            dir: dir.to_path_buf(),
            info: info.clone(),
            format: meta.format,
            frames: Arc::new(FrameIndex(RwLock::new(entries))),
            audio: Arc::new(OnceLock::from(Arc::new(samples))),
            build: None,
        })
    }

    fn build(source: &Path, dir: PathBuf, info: MediaInfo, notify: Notify) -> Result<Self> {
        // Whatever is there is stale or incomplete.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).with_context(|| format!("can't create the cache folder {}", dir.display()))?;
        let format = FrameFormat::for_source(&info);
        let meta = Meta {
            version: VERSION,
            source: source.to_path_buf(),
            info: info.clone(),
            format,
            frames_complete: false,
            audio_complete: !info.has_audio,
        };
        write_meta(&dir, &meta)?;

        let shared = Arc::new(BuildShared {
            dir: dir.clone(),
            cancelled: AtomicBool::new(false),
            state: Mutex::new(BuildState { meta, error: None }),
            notify,
        });
        // Created before the processes start, so that dropping it on an error stops them.
        let mut cache = Self {
            source: source.to_path_buf(),
            dir,
            info,
            format,
            frames: Arc::default(),
            audio: Arc::default(),
            build: Some(Build { shared: shared.clone(), children: Vec::new() }),
        };

        let filter = format!(
            "fps={}/{}:start_time=0,scale={}:{}:flags=bicubic",
            format.fps.num, format.fps.den, format.width, format.height
        );
        #[rustfmt::skip]
        let (child, stdout, stderr) = cache.spawn_ffmpeg(&[
            "-map", "0:V:0", "-an", "-sn", "-dn",
            "-vf", &filter,
            "-c:v", "mjpeg", "-q:v", JPEG_QUALITY, "-pix_fmt", "yuvj420p",
            "-f", "image2pipe", "pipe:1",
        ])?;
        let (job, frames) = (shared.clone(), cache.frames.clone());
        thread::spawn(move || {
            run_job(child, stdout, stderr, &job, |out| write_frames(out, &job, &frames), |meta| {
                meta.frames_complete = true
            })
        });

        if cache.info.has_audio {
            // first_pts=0 pads with silence if the audio starts later than the video.
            let filter = format!("aresample={AUDIO_RATE}:async=1:first_pts=0");
            #[rustfmt::skip]
            let (child, stdout, stderr) = cache.spawn_ffmpeg(&[
                "-map", "0:a:0", "-vn", "-sn", "-dn",
                "-af", &filter, "-ac", "2",
                "-c:a", "pcm_s16le", "-f", "s16le", "pipe:1",
            ])?;
            let (job, audio) = (shared, cache.audio.clone());
            thread::spawn(move || {
                run_job(child, stdout, stderr, &job, |out| read_audio(out, &job, &audio), |meta| {
                    meta.audio_complete = true
                })
            });
        } else {
            let _ = cache.audio.set(Arc::new(Vec::new()));
        }
        Ok(cache)
    }

    fn spawn_ffmpeg(&mut self, args: &[&str]) -> Result<(ChildSlot, ChildStdout, ChildStderr)> {
        let mut child = tools::command(tools::FFMPEG)
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&self.source)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start ffmpeg")?;
        let stdout = child.stdout.take().context("ffmpeg has no stdout")?;
        let stderr = child.stderr.take().context("ffmpeg has no stderr")?;
        let slot = Arc::new(Mutex::new(Some(child)));
        if let Some(build) = &mut self.build {
            build.children.push(slot.clone());
        }
        Ok((slot, stdout, stderr))
    }
}

impl Drop for MediaCache {
    fn drop(&mut self) {
        let Some(build) = &self.build else { return };
        // The readers stop at the next chunk and close the pipe, so ffmpeg exits even when
        // `kill` only reaches a launcher shim (e.g. Chocolatey's) and not ffmpeg itself.
        build.shared.cancelled.store(true, Ordering::Relaxed);
        for slot in &build.children {
            if let Some(child) = slot.lock().unwrap().as_mut() {
                let _ = child.kill();
            }
        }
    }
}

/// Runs one ffmpeg job to the end and records the outcome in the build state.
fn run_job(
    child: ChildSlot,
    stdout: ChildStdout,
    stderr: ChildStderr,
    shared: &BuildShared,
    work: impl FnOnce(ChildStdout) -> Result<()>,
    mark_complete: fn(&mut Meta),
) {
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = BufReader::new(stderr).read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let result = work(stdout);
    let status = child.lock().unwrap().take().map(|mut child| child.wait());
    let errors = errors.join().unwrap_or_default();
    if shared.cancelled.load(Ordering::Relaxed) {
        return;
    }

    let result = result.and_then(|()| match status {
        Some(Ok(status)) if status.success() => Ok(()),
        Some(Ok(_)) => Err(anyhow!("ffmpeg failed: {}", errors.lines().last().unwrap_or("no details"))),
        Some(Err(e)) => Err(e.into()),
        None => Err(anyhow!("ffmpeg was stopped")),
    });
    let mut state = shared.state.lock().unwrap();
    let result = result.and_then(|()| {
        mark_complete(&mut state.meta);
        write_meta(&shared.dir, &state.meta)
    });
    if let Err(e) = result {
        state.error.get_or_insert(format!("{e:#}"));
    }
    drop(state);
    (shared.notify)();
}

fn write_frames(out: ChildStdout, shared: &BuildShared, index: &FrameIndex) -> Result<()> {
    let mut file = File::create(shared.dir.join(FRAMES_FILE))?;
    let mut reader = JpegReader(BufReader::with_capacity(1 << 20, out));
    let mut jpeg = Vec::new();
    let mut offset = 0u64;
    while reader.next(&mut jpeg)? {
        if shared.cancelled.load(Ordering::Relaxed) {
            return Ok(());
        }
        file.write_all(&jpeg)?;
        index.push(offset, jpeg.len() as u32);
        offset += jpeg.len() as u64;
        if index.len() % 10 == 1 {
            (shared.notify)();
        }
    }
    write_index(&shared.dir.join(INDEX_FILE), &index.0.read().unwrap())
}

fn read_audio(mut out: ChildStdout, shared: &BuildShared, audio: &Pcm) -> Result<()> {
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        if shared.cancelled.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = out.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    fs::write(shared.dir.join(AUDIO_FILE), &bytes)?;
    let _ = audio.set(Arc::new(pcm_from_bytes(&bytes)));
    Ok(())
}

fn pcm_from_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

fn write_meta(dir: &Path, meta: &Meta) -> Result<()> {
    fs::write(dir.join(META_FILE), serde_json::to_vec_pretty(meta)?)?;
    Ok(())
}

fn write_index(path: &Path, entries: &[(u64, u32)]) -> Result<()> {
    let mut bytes = Vec::with_capacity(entries.len() * 12);
    for (offset, len) in entries {
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&len.to_le_bytes());
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn read_index(path: &Path) -> Result<Vec<(u64, u32)>> {
    let bytes = fs::read(path)?;
    if bytes.len() % 12 != 0 {
        bail!("corrupt frame index");
    }
    Ok(bytes
        .chunks_exact(12)
        .map(|c| {
            let offset = u64::from_le_bytes(c[..8].try_into().unwrap());
            let len = u32::from_le_bytes(c[8..].try_into().unwrap());
            (offset, len)
        })
        .collect())
}

/// Until projects live in a workspace, caches go to the user's local app data.
/// Cache folder name for a local file; it changes when the file is replaced or modified.
fn cache_key(source: &Path) -> Result<String> {
    let path = fs::canonicalize(source).with_context(|| format!("can't open {}", source.display()))?;
    let meta = fs::metadata(&path)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let path_text = path.to_string_lossy();
    let bytes = path_text.bytes().chain(meta.len().to_le_bytes()).chain(modified.to_le_bytes());
    for byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Ok(format!("file-{hash:016x}"))
}

/// Splits ffmpeg's `image2pipe` MJPEG output (JPEG images back to back) into single images.
struct JpegReader<R>(R);

impl<R: BufRead> JpegReader<R> {
    /// Reads the next image into `out`. Returns false at the end of the stream.
    fn next(&mut self, out: &mut Vec<u8>) -> Result<bool> {
        out.clear();
        if self.0.fill_buf()?.is_empty() {
            return Ok(false);
        }
        if self.byte()? != 0xFF || self.byte()? != 0xD8 {
            bail!("broken frame stream: no JPEG start marker");
        }
        out.extend_from_slice(&[0xFF, 0xD8]);
        let mut next = None;
        loop {
            let marker = match next.take() {
                Some(marker) => marker,
                None => self.marker(out)?,
            };
            match marker {
                // End of image.
                0xD9 => return Ok(true),
                // Markers without a payload.
                0x01 | 0xD0..=0xD7 => {}
                // Start of scan: a header, then entropy-coded data up to the next marker.
                0xDA => {
                    self.segment(out)?;
                    next = Some(self.scan(out)?);
                }
                _ => self.segment(out)?,
            }
        }
    }

    fn byte(&mut self) -> Result<u8> {
        let Some(&byte) = self.0.fill_buf()?.first() else {
            bail!("frame stream ended in the middle of an image");
        };
        self.0.consume(1);
        Ok(byte)
    }

    /// Reads a marker (0xFF, optional fill bytes, the code) and returns its code.
    fn marker(&mut self, out: &mut Vec<u8>) -> Result<u8> {
        if self.byte()? != 0xFF {
            bail!("broken frame stream: expected a JPEG marker");
        }
        let mut code = self.byte()?;
        while code == 0xFF {
            code = self.byte()?;
        }
        out.extend_from_slice(&[0xFF, code]);
        Ok(code)
    }

    /// Reads a segment: a big-endian length that counts itself, then the payload.
    fn segment(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let len_bytes = [self.byte()?, self.byte()?];
        let len = u16::from_be_bytes(len_bytes) as usize;
        if len < 2 {
            bail!("broken frame stream: bad JPEG segment length");
        }
        out.extend_from_slice(&len_bytes);
        let start = out.len();
        out.resize(start + len - 2, 0);
        self.0.read_exact(&mut out[start..])?;
        Ok(())
    }

    /// Reads entropy-coded data up to the next marker and returns that marker's code.
    fn scan(&mut self, out: &mut Vec<u8>) -> Result<u8> {
        loop {
            let buf = self.0.fill_buf()?;
            if buf.is_empty() {
                bail!("frame stream ended in the middle of an image");
            }
            match buf.iter().position(|&b| b == 0xFF) {
                None => {
                    let n = buf.len();
                    out.extend_from_slice(buf);
                    self.0.consume(n);
                }
                Some(i) => {
                    out.extend_from_slice(&buf[..i]);
                    self.0.consume(i + 1);
                    let mut code = self.byte()?;
                    while code == 0xFF {
                        code = self.byte()?;
                    }
                    out.extend_from_slice(&[0xFF, code]);
                    // Stuffed zero bytes and restart markers are part of the data.
                    if code != 0x00 && !(0xD0..=0xD7).contains(&code) {
                        return Ok(code);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn jpeg(seed: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, seed]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img).unwrap();
        out
    }

    #[test]
    fn jpeg_reader_splits_back_to_back_images() {
        let (a, b) = (jpeg(1), jpeg(200));
        let mut reader = JpegReader(std::io::Cursor::new([a.clone(), b.clone()].concat()));
        let mut out = Vec::new();
        assert!(reader.next(&mut out).unwrap());
        assert_eq!(out, a);
        assert!(reader.next(&mut out).unwrap());
        assert_eq!(out, b);
        assert!(!reader.next(&mut out).unwrap());
    }

    #[test]
    fn jpeg_reader_rejects_a_truncated_image() {
        let a = jpeg(1);
        let mut reader = JpegReader(std::io::Cursor::new(a[..a.len() / 2].to_vec()));
        assert!(reader.next(&mut Vec::new()).is_err());
    }

    #[test]
    fn frame_format_limits_size_and_rate() {
        let info = |w, h, num, den| MediaInfo { duration: 1.0, width: w, height: h, fps: Rational { num, den }, has_audio: false };
        let f = FrameFormat::for_source(&info(1920, 1080, 60000, 1001));
        assert_eq!((f.width, f.height, f.fps), (960, 540, Rational { num: 60000, den: 2002 }));
        let f = FrameFormat::for_source(&info(1080, 1920, 25, 1));
        assert_eq!((f.width, f.height, f.fps), (540, 960, Rational { num: 25, den: 1 }));
        let f = FrameFormat::for_source(&info(320, 240, 30, 1));
        assert_eq!((f.width, f.height), (320, 240));
    }

    /// Builds a cache from a generated clip with the real ffmpeg, then reopens it.
    #[test]
    fn builds_and_reuses_the_cache() {
        if tools::check(tools::FFMPEG).is_err() || tools::check(tools::FFPROBE).is_err() {
            eprintln!("ffmpeg/ffprobe not found, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("dicing_splicing_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mp4");
        #[rustfmt::skip]
        let status = tools::command(tools::FFMPEG)
            .args([
                "-v", "error",
                "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=2",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());

        let root = dir.join("cache");
        let notify: Notify = Arc::new(|| {});
        let cache = MediaCache::open_in(&root, &source, notify.clone()).unwrap();
        assert!(cache.build.is_some());
        let started = Instant::now();
        loop {
            match cache.status() {
                CacheStatus::Ready => break,
                CacheStatus::Failed(e) => panic!("cache build failed: {e}"),
                CacheStatus::Building(_) => {
                    assert!(started.elapsed() < Duration::from_secs(60), "cache build timed out");
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
        assert_eq!(cache.format, FrameFormat { fps: Rational { num: 25, den: 1 }, width: 320, height: 240 });
        assert_eq!(cache.frame_count(), 50);
        let samples = cache.audio.get().unwrap().len() as i64;
        assert!((samples - 2 * 2 * AUDIO_RATE as i64).abs() < 2 * AUDIO_RATE as i64 / 10, "{samples} samples");
        let (offset, len) = cache.frames.get(10).unwrap();
        let bytes = fs::read(cache.frames_path()).unwrap();
        let frame = image::load_from_memory(&bytes[offset as usize..][..len as usize]).unwrap();
        assert_eq!((frame.width(), frame.height()), (320, 240));
        drop(cache);

        let cache = MediaCache::open_in(&root, &source, notify).unwrap();
        assert!(cache.build.is_none(), "the second open must reuse the cache");
        assert_eq!(cache.frame_count(), 50);
        drop(cache);
        let _ = fs::remove_dir_all(&dir);
    }
}

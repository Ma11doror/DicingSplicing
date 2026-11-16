//! The sound of clips that play faster or slower, for the preview. It is made the way the export
//! makes it, by ffmpeg's `atempo`, which keeps the pitch: in the background, from the source's sound
//! in the editor's cache. Until it is there the clip is silent, unless a part made earlier covers
//! the new one: a clip trimmed shorter keeps its sound meanwhile.

use crate::core::edit::SourceId;
use crate::core::tools;
use crate::media::cache::{Pcm, AUDIO_RATE};
use crate::media::render::tempo;
use crate::media::Notify;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// How long a part has to be wanted before it is made: a speed or an edge being dragged asks for a
/// new one on every frame.
const SETTLE_SECONDS: f64 = 0.3;
/// How many frames of sound go to ffmpeg at a time.
const CHUNK_FRAMES: usize = 1 << 15;

/// A part of a source's sound at a speed: what a clip plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Part {
    source: SourceId,
    /// Where it starts and ends in the source, in frames of the cached sound.
    from: i64,
    to: i64,
    /// The bits of the speed: the same number is the same speed.
    speed: u64,
}

impl Part {
    /// The source's sound from `from` to `to` seconds, played `speed` times as fast.
    pub fn new(source: SourceId, from: f64, to: f64, speed: f64) -> Self {
        let frame = |t: f64| (t * AUDIO_RATE as f64).round() as i64;
        Self { source, from: frame(from), to: frame(to), speed: speed.to_bits() }
    }

    fn speed(&self) -> f64 {
        f64::from_bits(self.speed)
    }

    /// Whether this part has all of `other`, at the same speed.
    fn covers(&self, other: &Part) -> bool {
        self.source == other.source && self.speed == other.speed && self.from <= other.from && self.to >= other.to
    }
}

struct Entry {
    /// When it was first wanted, in the UI's seconds.
    since: f64,
    sound: Pcm,
    job: Option<Job>,
}

/// The parts that are made, or being made.
pub struct Stretcher {
    parts: HashMap<Part, Entry>,
    notify: Notify,
}

impl Stretcher {
    pub fn new(notify: Notify) -> Self {
        Self { parts: HashMap::new(), notify }
    }

    /// Keeps the parts in `wanted`, each with the whole sound of its source, and the made parts that
    /// cover them while their own are not there; the others go, and what is being made for them
    /// stops. A part wanted for a moment starts being made, once its source's sound is there.
    /// Returns how many seconds from `now` to call again, while a part waits for that moment.
    pub fn keep(&mut self, wanted: &[(Part, Pcm)], now: f64) -> Option<f64> {
        let mut used: HashSet<Part> = HashSet::new();
        for (part, _) in wanted {
            used.insert(*part);
            if !self.ready(part) {
                used.extend(self.covering(part));
            }
        }
        self.parts.retain(|part, _| used.contains(part));
        let mut wait: Option<f64> = None;
        for (part, source) in wanted {
            let entry = self.parts.entry(*part).or_insert_with(|| Entry { since: now, sound: Pcm::default(), job: None });
            if entry.job.is_some() || entry.sound.get().is_some() {
                continue;
            }
            // Its source's sound is still being read from the file: the library wakes the UI then.
            let Some(samples) = source.get() else { continue };
            let left = SETTLE_SECONDS - (now - entry.since);
            if left > 0.0 {
                wait = Some(wait.map_or(left, |wait| wait.min(left)));
                continue;
            }
            entry.job = Some(Job::start(*part, samples.clone(), entry.sound.clone(), self.notify.clone()));
        }
        wait
    }

    /// What plays `part`: a sound, and where in it the part starts, in seconds. Its own, once it is
    /// made; until then a made one that covers it, or else its own, which is silent until it is made.
    pub fn sound(&self, part: &Part) -> Option<(Pcm, f64)> {
        if !self.ready(part) {
            if let Some(cover) = self.covering(part) {
                let skipped = (part.from - cover.from) as f64 / AUDIO_RATE as f64;
                return Some((self.parts[&cover].sound.clone(), skipped / part.speed()));
            }
        }
        self.parts.get(part).map(|entry| (entry.sound.clone(), 0.0))
    }

    fn ready(&self, part: &Part) -> bool {
        self.parts.get(part).is_some_and(|entry| entry.sound.get().is_some())
    }

    /// A made part, other than `part` itself, that has all of it.
    fn covering(&self, part: &Part) -> Option<Part> {
        self.parts.iter().find(|(other, entry)| *other != part && other.covers(part) && entry.sound.get().is_some()).map(|(other, _)| *other)
    }
}

/// Making one part: ffmpeg gets the source's sound from one thread and gives the part to another.
/// Dropping it stops ffmpeg.
struct Job {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
}

impl Job {
    fn start(part: Part, source: Arc<Vec<i16>>, into: Pcm, notify: Notify) -> Self {
        let (filters, held) = tempo(part.speed());
        let cancelled = Arc::new(AtomicBool::new(false));
        #[rustfmt::skip]
        let spawned = tools::command(tools::FFMPEG)
            .args([
                "-v", "error", "-f", "s16le", "-ar", "48000", "-ac", "2", "-i", "pipe:0",
                "-af", &filters, "-f", "s16le", "-ar", "48000", "-ac", "2", "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            // Without ffmpeg there is nothing to hear, and nothing to try again.
            Err(_) => {
                let _ = into.set(Arc::new(Vec::new()));
                return Self { child: Arc::default(), cancelled };
            }
        };
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        let child = Arc::new(Mutex::new(Some(child)));
        // atempo gives what it hears ahead of time by as much as it holds back: it hears that much
        // before the part, so that what it gives starts where the part does.
        let first = part.from - (held * AUDIO_RATE as f64).round() as i64;
        if let Some(stdin) = stdin {
            let cancelled = cancelled.clone();
            thread::spawn(move || write_frames(stdin, &source, first, part.to, &cancelled));
        }
        if let Some(stdout) = stdout {
            let (cancelled, child) = (cancelled.clone(), child.clone());
            thread::spawn(move || {
                let samples = read_samples(stdout, &cancelled);
                if let Some(mut child) = child.lock().unwrap().take() {
                    let _ = child.wait();
                }
                if let Some(samples) = samples {
                    let _ = into.set(Arc::new(samples));
                    notify();
                }
            });
        }
        Self { child, cancelled }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            tools::kill_tree(&mut child);
            let _ = child.wait();
        }
    }
}

/// Writes the stereo frames `first..end` of `source` to ffmpeg: silence where the source has none.
fn write_frames(mut stdin: ChildStdin, source: &[i16], first: i64, end: i64, cancelled: &AtomicBool) {
    let count = (source.len() / 2) as i64;
    let mut bytes = Vec::with_capacity(CHUNK_FRAMES * 4);
    let mut frame = first;
    while frame < end {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        bytes.clear();
        let until = (frame + CHUNK_FRAMES as i64).min(end);
        for at in frame..until {
            let (left, right) = if (0..count).contains(&at) { (source[at as usize * 2], source[at as usize * 2 + 1]) } else { (0, 0) };
            bytes.extend_from_slice(&left.to_le_bytes());
            bytes.extend_from_slice(&right.to_le_bytes());
        }
        if stdin.write_all(&bytes).is_err() {
            return;
        }
        frame = until;
    }
}

/// All that ffmpeg gives, as samples; None if it was stopped.
fn read_samples(mut stdout: ChildStdout, cancelled: &AtomicBool) -> Option<Vec<i16>> {
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
    Some(bytes.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    fn have_ffmpeg() -> bool {
        let found = tools::check(tools::FFMPEG).is_ok();
        if !found {
            eprintln!("ffmpeg not found, skipping");
        }
        found
    }

    /// `seconds` of stereo sound, a beep of 1 kHz for 20 ms at the start of every second.
    fn beeps(seconds: usize) -> Pcm {
        let rate = AUDIO_RATE as usize;
        let mut samples = Vec::with_capacity(seconds * rate * 2);
        for i in 0..seconds * rate {
            let t = i as f64 / rate as f64;
            let value = if t.fract() < 0.02 { (0.9 * 32767.0 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin()) as i16 } else { 0 };
            samples.extend([value, value]);
        }
        Arc::new(OnceLock::from(Arc::new(samples)))
    }

    /// The seconds of each beep in `samples`.
    fn beeps_in(samples: &[i16]) -> Vec<f64> {
        let mut found: Vec<f64> = Vec::new();
        for (i, pair) in samples.chunks_exact(2).enumerate() {
            let t = i as f64 / AUDIO_RATE as f64;
            if pair[0].unsigned_abs() > 12000 && found.last().is_none_or(|last| t - last > 0.2) {
                found.push(t);
            }
        }
        found
    }

    /// Keeps `wanted` from the UI's second `now` on, until `part` is made.
    fn wait_for(stretcher: &mut Stretcher, wanted: &[(Part, Pcm)], part: &Part, mut now: f64) -> Arc<Vec<i16>> {
        let started = Instant::now();
        loop {
            stretcher.keep(wanted, now);
            if let Some(samples) = stretcher.parts.get(part).and_then(|entry| entry.sound.get().cloned()) {
                return samples;
            }
            assert!(started.elapsed() < Duration::from_secs(30), "the part was not made");
            std::thread::sleep(Duration::from_millis(20));
            now += 0.05;
        }
    }

    #[test]
    fn a_part_is_made_after_a_moment_and_plays_its_source_at_its_speed() {
        if !have_ffmpeg() {
            return;
        }
        let source = beeps(12);
        let mut stretcher = Stretcher::new(Arc::new(|| {}));
        // Seconds 1.5 to 9.5 of the source, twice as fast: 4 s, a beep every half second from 0.25 s.
        let part = Part::new(0, 1.5, 9.5, 2.0);
        let wanted = [(part, source.clone())];
        assert_eq!(stretcher.keep(&wanted, 0.0), Some(SETTLE_SECONDS), "it waits a moment first");
        assert!(stretcher.sound(&part).is_some_and(|(sound, _)| sound.get().is_none()), "silent until it is made");
        let samples = wait_for(&mut stretcher, &wanted, &part, 0.0);
        let length = samples.len() as f64 / 2.0 / AUDIO_RATE as f64;
        assert!((length - 4.0).abs() < 0.03, "{length} s");
        let found = beeps_in(&samples);
        assert_eq!(found.len(), 8, "{found:?}");
        for (n, at) in found.iter().enumerate() {
            assert!((at - (0.25 + n as f64 * 0.5)).abs() < 0.02, "beep {n} at {at:.4} s: {found:?}");
        }
    }

    #[test]
    fn a_shorter_part_plays_from_a_made_one_that_covers_it_until_its_own_is_there() {
        if !have_ffmpeg() {
            return;
        }
        let source = beeps(12);
        let mut stretcher = Stretcher::new(Arc::new(|| {}));
        let whole = Part::new(0, 2.0, 10.0, 2.0);
        wait_for(&mut stretcher, &[(whole, source.clone())], &whole, 0.0);
        // Trimmed to start at 4 s: one second into what was made, at twice the speed.
        let trimmed = Part::new(0, 4.0, 10.0, 2.0);
        let wanted = [(trimmed, source.clone())];
        stretcher.keep(&wanted, 100.0);
        let (sound, start) = stretcher.sound(&trimmed).unwrap();
        assert!(sound.get().is_some() && (start - 1.0).abs() < 1e-9, "from the whole one, 1 s in: {start}");
        // Its own one, once made; the whole one goes.
        wait_for(&mut stretcher, &wanted, &trimmed, 100.0);
        assert_eq!(stretcher.sound(&trimmed).unwrap().1, 0.0);
        stretcher.keep(&wanted, 200.0);
        assert_eq!(stretcher.parts.len(), 1);
        // Another speed is another part; nothing wanted, nothing kept.
        assert!(!Part::new(0, 2.0, 10.0, 3.0).covers(&trimmed));
        stretcher.keep(&[], 300.0);
        assert!(stretcher.parts.is_empty());
    }
}

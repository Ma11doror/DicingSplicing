//! Audio output through cpal. The stream position is the playback clock; video follows it.

use crate::core::edit::{Curve, Edge};
use crate::media::cache::{Pcm, AUDIO_RATE};
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::sync::OnceLock;

/// What to play: a range of the timeline, optionally looped, with fades at its edges.
#[derive(Clone, Default)]
pub struct PlayRange {
    pub start: f64,
    pub end: f64,
    pub looping: bool,
    /// Fade-in and fade-out lengths, when the range is the export range.
    pub fades: Option<(f64, f64)>,
    /// The sounds on the timeline: they play together, and where there is none it is silent.
    pub sounds: Arc<[Sound]>,
}

/// A clip's sound: where it is on the timeline, and the decoded source it comes from.
#[derive(Clone)]
pub struct Sound {
    pub segment: Segment,
    pub pcm: Pcm,
}

/// Timeline `start..end` plays the source from `source_start` on, multiplied by `gain`, rising
/// over its first `fade_in` seconds and falling over its last `fade_out`, each by its curve.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub source_start: f64,
    pub gain: f32,
    pub fade_in: f64,
    pub fade_out: f64,
    pub curves: (Curve, Curve),
}

impl Segment {
    /// Source time at timeline time `t`, if the segment covers it.
    pub fn source_time(&self, t: f64) -> Option<f64> {
        (t >= self.start && t < self.end).then_some(t - self.start + self.source_start)
    }

    /// What the segment's sound is multiplied by at `t`: its gain and its fades.
    pub fn level(&self, t: f64) -> f32 {
        let mut fade = 1.0f64;
        if self.fade_in > 0.0 {
            fade = fade.min(self.curves.0.level(Edge::In, (t - self.start) / self.fade_in));
        }
        if self.fade_out > 0.0 {
            fade = fade.min(self.curves.1.level(Edge::Out, (t - (self.end - self.fade_out)) / self.fade_out));
        }
        self.gain * fade.clamp(0.0, 1.0) as f32
    }
}

impl PlayRange {
    /// Linear fade gain at `t`, matching ffmpeg's default `afade` curve.
    pub fn gain(&self, t: f64) -> f32 {
        let Some((fade_in, fade_out)) = self.fades else { return 1.0 };
        let mut gain = 1.0f64;
        if fade_in > 0.0 {
            gain = gain.min((t - self.start) / fade_in);
        }
        if fade_out > 0.0 {
            gain = gain.min((self.end - t) / fade_out);
        }
        gain.clamp(0.0, 1.0) as f32
    }

    /// The sounds at `t` added together, before the fades.
    pub fn mix(&self, t: f64) -> (f32, f32) {
        let mut sum = (0.0, 0.0);
        for sound in self.sounds.iter() {
            // A source whose sound is still being decoded is silent until it is there.
            if let (Some(pcm), Some(source)) = (sound.pcm.get(), sound.segment.source_time(t)) {
                let (left, right) = sample_at(pcm, source * AUDIO_RATE as f64);
                let level = sound.segment.level(t);
                sum.0 += left * level;
                sum.1 += right * level;
            }
        }
        sum
    }

    /// Position `elapsed` seconds after starting at `from`, or None once playback has run out.
    pub fn advance(&self, from: f64, elapsed: f64) -> Option<f64> {
        let t = from + elapsed;
        if t < self.end {
            Some(t)
        } else if self.looping && self.end > self.start {
            Some(self.start + (t - self.end) % (self.end - self.start))
        } else {
            None
        }
    }
}

struct Shared {
    range: Mutex<PlayRange>,
    playing: AtomicBool,
    /// Seconds, as f64 bits. Written only by the audio callback.
    position: AtomicU64,
    seek_to: AtomicU64,
    seek_pending: AtomicBool,
}

pub struct AudioOutput {
    shared: Arc<Shared>,
    _stream: cpal::Stream,
}

impl AudioOutput {
    pub fn new() -> Result<Self> {
        let device = cpal::default_host().default_output_device().context("no audio output device")?;
        let config = device.default_output_config().context("can't read the audio output config")?;
        let shared = Arc::new(Shared {
            range: Mutex::new(PlayRange::default()),
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0f64.to_bits()),
            seek_to: AtomicU64::new(0f64.to_bits()),
            seek_pending: AtomicBool::new(false),
        });
        let format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let stream = match format {
            SampleFormat::F32 => build_stream::<f32>(&device, config, shared.clone()),
            SampleFormat::I16 => build_stream::<i16>(&device, config, shared.clone()),
            SampleFormat::U16 => build_stream::<u16>(&device, config, shared.clone()),
            SampleFormat::I32 => build_stream::<i32>(&device, config, shared.clone()),
            other => bail!("unsupported audio sample format {other}"),
        }?;
        stream.play().context("can't start the audio output")?;
        Ok(Self { shared, _stream: stream })
    }

    pub fn play(&self, from: f64, range: PlayRange) {
        *self.shared.range.lock().unwrap() = range;
        self.shared.seek_to.store(from.to_bits(), Ordering::Release);
        self.shared.seek_pending.store(true, Ordering::Release);
        self.shared.playing.store(true, Ordering::Release);
    }

    /// Changes the range without seeking, e.g. when the export range is edited during playback.
    pub fn set_range(&self, range: PlayRange) {
        *self.shared.range.lock().unwrap() = range;
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Release);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn position(&self) -> f64 {
        let bits = if self.shared.seek_pending.load(Ordering::Acquire) {
            self.shared.seek_to.load(Ordering::Acquire)
        } else {
            self.shared.position.load(Ordering::Acquire)
        };
        f64::from_bits(bits)
    }
}

fn build_stream<T>(device: &cpal::Device, config: cpal::StreamConfig, shared: Arc<Shared>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let rate = AUDIO_RATE as f64;
    // Cached frames per output frame: a simple linear resampler covers devices not at 48 kHz.
    let step = rate / config.sample_rate as f64;
    let mut range = PlayRange::default();
    let mut pos = 0.0f64;
    let silence = T::from_sample(0.0f32);

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            // Never block the audio thread: if the UI holds the lock, keep the last range.
            if let Ok(r) = shared.range.try_lock() {
                range = r.clone();
            }
            if shared.seek_pending.swap(false, Ordering::AcqRel) {
                pos = f64::from_bits(shared.seek_to.load(Ordering::Acquire)) * rate;
            }
            let mut playing = shared.playing.load(Ordering::Acquire);
            for frame in data.chunks_mut(channels) {
                if playing {
                    let mut t = pos / rate;
                    if t >= range.end {
                        if range.looping && range.end > range.start {
                            t = range.start;
                            pos = t * rate;
                        } else {
                            playing = false;
                            shared.playing.store(false, Ordering::Release);
                        }
                    }
                    if playing {
                        let (left, right) = range.mix(t);
                        // Clips together, or a volume above 100%, can go past full scale.
                        let gain = range.gain(t);
                        write_frame(frame, (left * gain).clamp(-1.0, 1.0), (right * gain).clamp(-1.0, 1.0), silence);
                        pos += step;
                        continue;
                    }
                }
                frame.fill(silence);
            }
            shared.position.store((pos / rate).to_bits(), Ordering::Release);
        },
        |err| eprintln!("audio output error: {err}"),
        None,
    )?;
    Ok(stream)
}

/// Stereo sample at a fractional frame position, linearly interpolated.
fn sample_at(pcm: &[i16], pos: f64) -> (f32, f32) {
    let get = |frame: usize| match pcm.get(frame * 2..frame * 2 + 2) {
        Some(&[l, r]) => (l as f32 / 32768.0, r as f32 / 32768.0),
        _ => (0.0, 0.0),
    };
    let i = pos as usize;
    let frac = (pos - i as f64) as f32;
    let (a, b) = (get(i), get(i + 1));
    (a.0 + (b.0 - a.0) * frac, a.1 + (b.1 - a.1) * frac)
}

fn write_frame<T: SizedSample + FromSample<f32>>(frame: &mut [T], left: f32, right: f32, silence: T) {
    match frame {
        [mono] => *mono = T::from_sample((left + right) * 0.5),
        [l, r, rest @ ..] => {
            *l = T::from_sample(left);
            *r = T::from_sample(right);
            rest.fill(silence);
        }
        [] => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_are_linear_at_the_range_edges() {
        let range = PlayRange { start: 10.0, end: 40.0, fades: Some((1.0, 2.0)), ..PlayRange::default() };
        assert_eq!(range.gain(10.0), 0.0);
        assert_eq!(range.gain(10.5), 0.5);
        assert_eq!(range.gain(20.0), 1.0);
        assert_eq!(range.gain(39.0), 0.5);
        assert_eq!(PlayRange { fades: None, ..range }.gain(10.0), 1.0);
    }

    #[test]
    fn advance_loops_or_stops() {
        let range = PlayRange { start: 10.0, end: 20.0, looping: true, ..PlayRange::default() };
        assert_eq!(range.advance(15.0, 2.0), Some(17.0));
        assert_eq!(range.advance(15.0, 7.0), Some(12.0));
        assert_eq!(PlayRange { looping: false, ..range }.advance(15.0, 7.0), None);
    }

    #[test]
    fn segment_maps_to_source_time() {
        let sound = Segment { start: 5.0, end: 15.0, source_start: 2.0, gain: 1.0, ..Segment::default() };
        assert_eq!(sound.source_time(5.0), Some(2.0));
        assert_eq!(sound.source_time(10.5), Some(7.5));
        assert_eq!(sound.source_time(4.9), None);
        assert_eq!(sound.source_time(15.0), None);
    }

    #[test]
    fn time_before_the_zero_of_the_timeline_plays_like_any_other() {
        // A clip moved to -5..5 s, playing its source from 2 s on.
        let sound = Segment { start: -5.0, end: 5.0, source_start: 2.0, gain: 1.0, ..Segment::default() };
        assert_eq!(sound.source_time(-5.0), Some(2.0));
        assert_eq!(sound.source_time(0.0), Some(7.0));
        assert_eq!(sound.source_time(-5.1), None);

        let range = PlayRange { start: -8.0, end: -2.0, looping: true, fades: Some((1.0, 2.0)), ..PlayRange::default() };
        assert_eq!(range.gain(-8.0), 0.0);
        assert_eq!(range.gain(-7.5), 0.5);
        assert_eq!(range.gain(-3.0), 0.5);
        assert_eq!(range.advance(-4.0, 1.0), Some(-3.0));
        assert_eq!(range.advance(-4.0, 3.0), Some(-7.0));
    }

    #[test]
    fn sounds_of_clips_play_together() {
        let pcm = |level: i16| -> Pcm { Arc::new(OnceLock::from(Arc::new(vec![level; 2 * AUDIO_RATE as usize * 20]))) };
        // One clip at 0..10 s at half volume, another at 5..15 s; a third whose sound is not decoded yet.
        let sounds: Vec<Sound> = vec![
            Sound { segment: Segment { start: 0.0, end: 10.0, source_start: 0.0, gain: 0.5, ..Segment::default() }, pcm: pcm(8192) },
            Sound { segment: Segment { start: 5.0, end: 15.0, source_start: 3.0, gain: 1.0, ..Segment::default() }, pcm: pcm(4096) },
            Sound { segment: Segment { start: 0.0, end: 15.0, source_start: 0.0, gain: 1.0, ..Segment::default() }, pcm: Arc::new(OnceLock::new()) },
        ];
        let range = PlayRange { start: 0.0, end: 20.0, sounds: sounds.into(), ..PlayRange::default() };
        assert_eq!(range.mix(2.0), (0.125, 0.125));
        assert_eq!(range.mix(7.0), (0.25, 0.25));
        assert_eq!(range.mix(12.0), (0.125, 0.125));
        assert_eq!(range.mix(16.0), (0.0, 0.0));
        assert_eq!(PlayRange::default().mix(1.0), (0.0, 0.0));
    }

    #[test]
    fn a_clip_fades_in_and_out_on_its_own() {
        let pcm: Pcm = Arc::new(OnceLock::from(Arc::new(vec![16384i16; 2 * AUDIO_RATE as usize * 20])));
        // A clip at 2..12 s at full volume, rising over 2 s and falling over 4 s.
        let segment = Segment { start: 2.0, end: 12.0, source_start: 0.0, gain: 1.0, fade_in: 2.0, fade_out: 4.0, ..Segment::default() };
        let range = PlayRange { start: 0.0, end: 20.0, sounds: vec![Sound { segment, pcm }].into(), ..PlayRange::default() };
        assert_eq!(range.mix(2.0), (0.0, 0.0));
        assert_eq!(range.mix(3.0), (0.25, 0.25));
        assert_eq!(range.mix(6.0), (0.5, 0.5));
        assert_eq!(range.mix(10.0), (0.25, 0.25));
        assert_eq!(Segment { gain: 0.5, ..segment }.level(3.0), 0.25);
        // By curves: an ease in rises slowly, an ease out falls fast and lingers.
        let eased = Segment { curves: (Curve::EaseIn, Curve::EaseOut), ..segment };
        assert_eq!((eased.level(3.0), eased.level(10.0)), (0.25, 0.25));
        assert_eq!((eased.level(2.5), eased.level(8.5)), (0.0625, 0.765625));
    }

    #[test]
    fn interpolates_between_frames() {
        let pcm = [0, 0, 16384, -16384];
        assert_eq!(sample_at(&pcm, 0.5), (0.25, -0.25));
        assert_eq!(sample_at(&pcm, 5.0), (0.0, 0.0));
    }
}

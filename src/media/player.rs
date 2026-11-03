//! Playback control on top of the audio output. Without an audio device it runs on the system clock.

use crate::media::audio::{AudioOutput, PlayRange};
use crate::media::cache::Pcm;
use std::time::Instant;

pub struct Player {
    audio: Option<AudioOutput>,
    pub audio_error: Option<String>,
    clock: Option<Clock>,
}

struct Clock {
    started: Instant,
    from: f64,
    range: PlayRange,
}

impl Player {
    pub fn new(pcm: Pcm) -> Self {
        match AudioOutput::new(pcm) {
            Ok(audio) => Self { audio: Some(audio), audio_error: None, clock: None },
            Err(e) => Self { audio: None, audio_error: Some(format!("{e:#}")), clock: None },
        }
    }

    pub fn play(&mut self, from: f64, range: PlayRange) {
        match &self.audio {
            Some(audio) => audio.play(from, range),
            None => self.clock = Some(Clock { started: Instant::now(), from, range }),
        }
    }

    pub fn set_range(&mut self, range: PlayRange) {
        if let Some(audio) = &self.audio {
            audio.set_range(range);
        }
        if let Some(clock) = &mut self.clock {
            clock.range = range;
        }
    }

    pub fn pause(&mut self) {
        if let Some(audio) = &self.audio {
            audio.pause();
        }
        self.clock = None;
    }

    /// Current position and whether playback is still going (it stops by itself at the end).
    pub fn poll(&mut self) -> (f64, bool) {
        if let Some(audio) = &self.audio {
            return (audio.position(), audio.is_playing());
        }
        let Some(clock) = &self.clock else { return (0.0, false) };
        match clock.range.advance(clock.from, clock.started.elapsed().as_secs_f64()) {
            Some(t) => (t, true),
            None => {
                let end = clock.range.end;
                self.clock = None;
                (end, false)
            }
        }
    }
}

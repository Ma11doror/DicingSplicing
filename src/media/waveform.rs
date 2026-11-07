//! How loud a source's sound is along its length: the graph of a clip's sound on the timeline.
//! Made once per source from the decoded sound the editor already has, at a fixed step; drawing at
//! any zoom takes the loudest step under each pixel.

use crate::media::cache::AUDIO_RATE;

/// Sound frames (a sample for each channel) a step covers: 10 ms.
const STEP_FRAMES: usize = AUDIO_RATE as usize / 100;
const CHANNELS: usize = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    /// The loudest sample of each step, from 0 to 1.
    levels: Vec<f32>,
}

impl Peaks {
    /// From interleaved stereo samples at the cache's rate.
    pub fn from_pcm(pcm: &[i16]) -> Self {
        let levels = pcm
            .chunks(STEP_FRAMES * CHANNELS)
            .map(|step| step.iter().map(|sample| (*sample as f32).abs()).fold(0.0, f32::max) / 32768.0)
            .collect();
        Self { levels }
    }

    /// The loudest level between source times `from` and `to`, in seconds; at least one step is
    /// looked at, so that a zoom where a pixel is shorter than a step still shows something.
    pub fn level(&self, from: f64, to: f64) -> f32 {
        let step = |t: f64| (t.max(0.0) * AUDIO_RATE as f64 / STEP_FRAMES as f64) as usize;
        let first = step(from).min(self.levels.len());
        let last = step(to).max(first + 1).min(self.levels.len());
        self.levels[first..last].iter().copied().fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of silence, then a second at half level, then a click at full level at 2.5 s.
    fn sound() -> Vec<i16> {
        let frames = 3 * AUDIO_RATE as usize;
        let mut pcm = vec![0i16; frames * CHANNELS];
        for frame in AUDIO_RATE as usize..2 * AUDIO_RATE as usize {
            pcm[frame * CHANNELS] = if frame % 2 == 0 { 16384 } else { -16384 };
        }
        let click = 5 * AUDIO_RATE as usize / 2;
        pcm[click * CHANNELS + 1] = -32768;
        pcm
    }

    #[test]
    fn steps_keep_the_loudest_sample_of_either_channel() {
        let peaks = Peaks::from_pcm(&sound());
        assert_eq!(peaks.levels.len(), 300);
        assert_eq!(peaks.level(0.0, 1.0), 0.0);
        assert_eq!(peaks.level(1.2, 1.8), 0.5);
        assert_eq!(peaks.level(0.0, 3.0), 1.0);
        // The click is in one step of 10 ms: a narrow look still finds it, one beside it does not.
        assert_eq!(peaks.level(2.5, 2.5001), 1.0);
        assert_eq!(peaks.level(2.52, 2.53), 0.0);
    }

    #[test]
    fn looking_outside_the_sound_finds_silence() {
        let peaks = Peaks::from_pcm(&sound());
        assert_eq!(peaks.level(5.0, 6.0), 0.0);
        assert_eq!(peaks.level(-2.0, -1.0), 0.0);
        assert_eq!(Peaks::from_pcm(&[]).level(0.0, 1.0), 0.0);
    }
}

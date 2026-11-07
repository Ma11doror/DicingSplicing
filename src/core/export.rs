//! Export settings: how the result is encoded, and presets that set some or all of them.

use crate::core::preset::{Output, PresetId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    WebM,
}

impl Container {
    pub const ALL: [Self; 4] = [Self::Mp4, Self::Mov, Self::Mkv, Self::WebM];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mov => "MOV",
            Self::Mkv => "MKV",
            Self::WebM => "WebM",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::WebM => "webm",
        }
    }

    pub fn allows_video(self, codec: VideoCodec) -> bool {
        match self {
            Self::Mp4 | Self::Mov => codec != VideoCodec::Vp9,
            Self::Mkv => true,
            Self::WebM => codec == VideoCodec::Vp9,
        }
    }

    pub fn allows_audio(self, codec: AudioCodec) -> bool {
        match self {
            Self::Mp4 | Self::Mov => codec != AudioCodec::Opus,
            Self::Mkv => true,
            Self::WebM => codec != AudioCodec::Aac,
        }
    }

    /// The file's index can go to its start, so that playback begins before the whole file arrives.
    pub fn has_faststart(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    H265,
    Vp9,
}

impl VideoCodec {
    pub const ALL: [Self; 3] = [Self::H264, Self::H265, Self::Vp9];

    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::Vp9 => "VP9",
        }
    }

    /// The ffmpeg encoder.
    pub fn encoder(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::Vp9 => "libvpx-vp9",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Profile {
    Baseline,
    Main,
    High,
}

impl H264Profile {
    pub const ALL: [Self; 3] = [Self::Baseline, Self::Main, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Baseline => "Baseline",
            Self::Main => "Main",
            Self::High => "High",
        }
    }

    pub fn arg(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Main => "main",
            Self::High => "high",
        }
    }
}

/// How long the encoder may take: slower gives a smaller file of the same quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Speed {
    Ultrafast,
    Veryfast,
    Fast,
    Medium,
    Slow,
    Veryslow,
}

impl Speed {
    pub const ALL: [Self; 6] = [Self::Ultrafast, Self::Veryfast, Self::Fast, Self::Medium, Self::Slow, Self::Veryslow];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ultrafast => "Ultrafast",
            Self::Veryfast => "Very fast",
            Self::Fast => "Fast",
            Self::Medium => "Medium",
            Self::Slow => "Slow",
            Self::Veryslow => "Very slow",
        }
    }

    /// The `-preset` of x264 and x265.
    pub fn x26x_preset(self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Veryfast => "veryfast",
            Self::Fast => "fast",
            Self::Medium => "medium",
            Self::Slow => "slow",
            Self::Veryslow => "veryslow",
        }
    }

    /// The `-cpu-used` of VP9.
    pub fn vp9_cpu_used(self) -> u8 {
        match self {
            Self::Ultrafast => 8,
            Self::Veryfast => 6,
            Self::Fast => 4,
            Self::Medium => 3,
            Self::Slow => 2,
            Self::Veryslow => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rate {
    /// Constant quality (CRF): lower is better, the size of the file varies.
    Quality(u8),
    /// Average bitrate in kbit/s: the size of the file is known, the quality varies.
    Bitrate(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSize {
    /// What the main frame asks for.
    FromFrame,
    /// The long side in pixels, the proportions of the main frame.
    LongSide(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum FrameRate {
    Source,
    Fixed(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    Aac,
    Opus,
    None,
}

impl AudioCodec {
    pub const ALL: [Self; 3] = [Self::Aac, Self::Opus, Self::None];

    pub fn label(self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Opus => "Opus",
            Self::None => "No sound",
        }
    }

    /// The ffmpeg encoder, if there is sound at all.
    pub fn encoder(self) -> Option<&'static str> {
        match self {
            Self::Aac => Some("aac"),
            Self::Opus => Some("libopus"),
            Self::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    pub container: Container,
    pub video_codec: VideoCodec,
    pub profile: H264Profile,
    pub speed: Speed,
    pub rate: Rate,
    pub size: OutputSize,
    pub frame_rate: FrameRate,
    pub audio_codec: AudioCodec,
    pub audio_kbps: u32,
    /// Fade-in and fade-out of the sound at the edges of the export range, in seconds.
    pub fades: (f64, f64),
    /// The sound is brought to one loudness, as the sites that play videos do: quiet sources and
    /// loud ones come out alike.
    #[serde(default)]
    pub even_loudness: bool,
}

/// The loudness of `ExportSettings::even_loudness`: what YouTube and most sites play at, in LUFS.
pub const EVEN_LOUDNESS: f64 = -14.0;

/// The starting point is the format the editor was made for: the settings of tgCircleGen.
impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            container: Container::Mp4,
            video_codec: VideoCodec::H264,
            profile: H264Profile::Baseline,
            speed: Speed::Medium,
            rate: Rate::Quality(23),
            size: OutputSize::FromFrame,
            frame_rate: FrameRate::Fixed(30.0),
            audio_codec: AudioCodec::Aac,
            audio_kbps: 128,
            fades: (1.0, 1.0),
            even_loudness: false,
        }
    }
}

impl ExportSettings {
    /// Output size in pixels.
    pub fn pixels(&self, output: Output) -> [u32; 2] {
        match self.size {
            OutputSize::FromFrame => output.pixels(),
            OutputSize::LongSide(long_side) => output.ratio().pixels(long_side),
        }
    }

    /// The same settings with codecs that the container can hold.
    pub fn normalized(mut self) -> Self {
        if !self.container.allows_video(self.video_codec) {
            self.video_codec = VideoCodec::ALL.into_iter().find(|c| self.container.allows_video(*c)).unwrap_or(VideoCodec::H264);
        }
        if !self.container.allows_audio(self.audio_codec) {
            self.audio_codec = AudioCodec::ALL.into_iter().find(|c| self.container.allows_audio(*c)).unwrap_or(AudioCodec::None);
        }
        self
    }

    /// The settings that differ from `other`.
    pub fn differences(&self, other: &Self) -> Vec<Setting> {
        let checks = [
            (Setting::Format, self.container != other.container),
            (Setting::VideoCodec, self.video_codec != other.video_codec),
            (Setting::Profile, self.profile != other.profile),
            (Setting::Speed, self.speed != other.speed),
            (Setting::Quality, self.rate != other.rate),
            (Setting::Size, self.size != other.size),
            (Setting::FrameRate, self.frame_rate != other.frame_rate),
            (Setting::AudioCodec, self.audio_codec != other.audio_codec),
            (Setting::AudioBitrate, self.audio_kbps != other.audio_kbps),
            (Setting::Fades, self.fades != other.fades),
            (Setting::Loudness, self.even_loudness != other.even_loudness),
        ];
        checks.into_iter().filter(|(_, differs)| *differs).map(|(setting, _)| setting).collect()
    }
}

/// One line of the detailed settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Frame,
    Format,
    VideoCodec,
    Profile,
    Speed,
    Quality,
    Size,
    FrameRate,
    AudioCodec,
    AudioBitrate,
    Fades,
    Loudness,
}

impl Setting {
    pub fn label(self) -> &'static str {
        match self {
            Self::Frame => "Frame",
            Self::Format => "Format",
            Self::VideoCodec => "Video codec",
            Self::Profile => "Profile",
            Self::Speed => "Speed",
            Self::Quality => "Quality",
            Self::Size => "Size",
            Self::FrameRate => "Frame rate",
            Self::AudioCodec => "Audio codec",
            Self::AudioBitrate => "Audio bitrate",
            Self::Fades => "Sound fades",
            Self::Loudness => "Loudness",
        }
    }
}

/// A named group of settings. What it leaves as `None` stays as it is when the preset is applied.
#[derive(Debug, Clone, Copy)]
pub struct ExportPreset {
    pub group: &'static str,
    pub name: &'static str,
    pub frame: Option<PresetId>,
    pub container: Option<Container>,
    pub video_codec: Option<VideoCodec>,
    pub profile: Option<H264Profile>,
    pub speed: Option<Speed>,
    pub rate: Option<Rate>,
    pub size: Option<OutputSize>,
    pub frame_rate: Option<FrameRate>,
    pub audio_codec: Option<AudioCodec>,
    pub audio_kbps: Option<u32>,
    pub fades: Option<(f64, f64)>,
}

const NOTHING: ExportPreset = ExportPreset {
    group: "",
    name: "",
    frame: None,
    container: None,
    video_codec: None,
    profile: None,
    speed: None,
    rate: None,
    size: None,
    frame_rate: None,
    audio_codec: None,
    audio_kbps: None,
    fades: None,
};

/// Everything a finished video for some place needs. `Default` must stay the same as the first one.
const fn everything(name: &'static str, frame: PresetId, profile: H264Profile, crf: u8, fps: FrameRate, audio_kbps: u32, fade: f64) -> ExportPreset {
    ExportPreset {
        group: "Everything",
        name,
        frame: Some(frame),
        container: Some(Container::Mp4),
        video_codec: Some(VideoCodec::H264),
        profile: Some(profile),
        speed: Some(Speed::Medium),
        rate: Some(Rate::Quality(crf)),
        size: Some(OutputSize::FromFrame),
        frame_rate: Some(fps),
        audio_codec: Some(AudioCodec::Aac),
        audio_kbps: Some(audio_kbps),
        fades: Some((fade, fade)),
    }
}

const fn size(name: &'static str, long_side: u32) -> ExportPreset {
    ExportPreset { group: "Size", name, size: Some(OutputSize::LongSide(long_side)), ..NOTHING }
}

const fn quality(name: &'static str, crf: u8, speed: Speed) -> ExportPreset {
    ExportPreset { group: "Quality", name, rate: Some(Rate::Quality(crf)), speed: Some(speed), ..NOTHING }
}

const fn frame_rate(name: &'static str, fps: FrameRate) -> ExportPreset {
    ExportPreset { group: "Frame rate", name, frame_rate: Some(fps), ..NOTHING }
}

const fn format(name: &'static str, container: Container, video: VideoCodec, audio: AudioCodec) -> ExportPreset {
    ExportPreset { group: "Format", name, container: Some(container), video_codec: Some(video), audio_codec: Some(audio), ..NOTHING }
}

/// In the order they are listed.
pub const PRESET_GROUPS: [&str; 6] = ["Everything", "Size", "Quality", "Frame rate", "Format", "Sound"];

pub const EXPORT_PRESETS: [ExportPreset; 18] = [
    everything("Telegram circle", PresetId::TelegramCircle, H264Profile::Baseline, 23, FrameRate::Fixed(30.0), 128, 1.0),
    everything("Reels, Shorts, TikTok", PresetId::Vertical, H264Profile::High, 20, FrameRate::Fixed(30.0), 192, 0.0),
    everything("YouTube", PresetId::Landscape, H264Profile::High, 18, FrameRate::Source, 256, 0.0),
    everything("Instagram post", PresetId::Post, H264Profile::High, 20, FrameRate::Fixed(30.0), 192, 0.0),
    size("From the frame", 0),
    size("720p", 1280),
    size("1080p", 1920),
    size("1440p", 2560),
    size("4K", 3840),
    quality("Best", 18, Speed::Slow),
    quality("Good", 23, Speed::Medium),
    quality("Small file", 28, Speed::Medium),
    frame_rate("As source", FrameRate::Source),
    frame_rate("30 fps", FrameRate::Fixed(30.0)),
    frame_rate("60 fps", FrameRate::Fixed(60.0)),
    format("MP4, H.264", Container::Mp4, VideoCodec::H264, AudioCodec::Aac),
    format("WebM, VP9", Container::WebM, VideoCodec::Vp9, AudioCodec::Opus),
    ExportPreset { group: "Sound", name: "No sound", audio_codec: Some(AudioCodec::None), ..NOTHING },
];

impl ExportPreset {
    /// The lines of the detailed settings that this preset sets.
    pub fn settings(&self) -> Vec<Setting> {
        let checks = [
            (Setting::Frame, self.frame.is_some()),
            (Setting::Format, self.container.is_some()),
            (Setting::VideoCodec, self.video_codec.is_some()),
            (Setting::Profile, self.profile.is_some()),
            (Setting::Speed, self.speed.is_some()),
            (Setting::Quality, self.rate.is_some()),
            (Setting::Size, self.size.is_some()),
            (Setting::FrameRate, self.frame_rate.is_some()),
            (Setting::AudioCodec, self.audio_codec.is_some()),
            (Setting::AudioBitrate, self.audio_kbps.is_some()),
            (Setting::Fades, self.fades.is_some()),
        ];
        checks.into_iter().filter(|(_, set)| *set).map(|(setting, _)| setting).collect()
    }

    /// `size` keeps a long side of 0 for "from the frame" to stay a `const` table entry.
    fn own_size(&self) -> Option<OutputSize> {
        self.size.map(|size| if size == OutputSize::LongSide(0) { OutputSize::FromFrame } else { size })
    }

    /// Applies the preset. Returns the lines to highlight: what the preset sets, and any other line
    /// that had to change for the result to make sense.
    pub fn apply(&self, output: &mut Output, settings: &mut ExportSettings) -> Vec<Setting> {
        if let Some(frame) = self.frame {
            output.preset = frame;
        }
        let raw = ExportSettings {
            container: self.container.unwrap_or(settings.container),
            video_codec: self.video_codec.unwrap_or(settings.video_codec),
            profile: self.profile.unwrap_or(settings.profile),
            speed: self.speed.unwrap_or(settings.speed),
            rate: self.rate.unwrap_or(settings.rate),
            size: self.own_size().unwrap_or(settings.size),
            frame_rate: self.frame_rate.unwrap_or(settings.frame_rate),
            audio_codec: self.audio_codec.unwrap_or(settings.audio_codec),
            audio_kbps: self.audio_kbps.unwrap_or(settings.audio_kbps),
            fades: self.fades.unwrap_or(settings.fades),
            // Presets don't say: it is the user's own choice.
            even_loudness: settings.even_loudness,
        };
        *settings = raw.normalized();
        let mut lines = self.settings();
        for line in raw.differences(settings) {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        lines
    }

    /// The current settings are what this preset sets.
    pub fn is_applied(&self, output: Output, settings: &ExportSettings) -> bool {
        fn same<T: PartialEq>(wanted: Option<T>, current: T) -> bool {
            wanted.is_none_or(|wanted| wanted == current)
        }
        same(self.frame, output.preset)
            && same(self.container, settings.container)
            && same(self.video_codec, settings.video_codec)
            && same(self.profile, settings.profile)
            && same(self.speed, settings.speed)
            && same(self.rate, settings.rate)
            && same(self.own_size(), settings.size)
            && same(self.frame_rate, settings.frame_rate)
            && same(self.audio_codec, settings.audio_codec)
            && same(self.audio_kbps, settings.audio_kbps)
            && same(self.fades, settings.fades)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str) -> &'static ExportPreset {
        EXPORT_PRESETS.iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn the_defaults_are_the_telegram_preset() {
        assert!(preset("Telegram circle").is_applied(Output::default(), &ExportSettings::default()));
    }

    #[test]
    fn a_preset_changes_only_what_it_sets() {
        let (mut output, mut settings) = (Output::default(), ExportSettings::default());
        let lines = preset("720p").apply(&mut output, &mut settings);
        assert_eq!(lines, [Setting::Size]);
        assert_eq!(settings, ExportSettings { size: OutputSize::LongSide(1280), ..ExportSettings::default() });
        assert_eq!(output, Output::default());

        let lines = preset("Best").apply(&mut output, &mut settings);
        assert_eq!(lines, [Setting::Speed, Setting::Quality]);
        assert_eq!(settings.size, OutputSize::LongSide(1280));
    }

    #[test]
    fn a_full_preset_sets_everything() {
        let (mut output, mut settings) = (Output::default(), ExportSettings::default());
        let lines = preset("Reels, Shorts, TikTok").apply(&mut output, &mut settings);
        assert_eq!(lines.len(), 11);
        assert_eq!(output.preset, PresetId::Vertical);
        assert_eq!(settings.pixels(output), [1080, 1920]);
        assert_eq!(settings.fades, (0.0, 0.0));
        for p in EXPORT_PRESETS.iter().filter(|p| p.group == "Everything") {
            assert_eq!(p.settings().len(), 11, "{}", p.name);
        }
    }

    #[test]
    fn the_size_preset_can_go_back_to_the_frame() {
        let (mut output, mut settings) = (Output::default(), ExportSettings::default());
        preset("1080p").apply(&mut output, &mut settings);
        assert_eq!(settings.pixels(output), [1920, 1920]);
        preset("From the frame").apply(&mut output, &mut settings);
        assert_eq!(settings.pixels(output), [400, 400]);
        assert!(preset("From the frame").is_applied(output, &settings));
    }

    #[test]
    fn presets_are_applied_only_while_the_settings_match() {
        let (mut output, mut settings) = (Output::default(), ExportSettings::default());
        assert!(preset("Good").is_applied(output, &settings));
        assert!(!preset("Best").is_applied(output, &settings));

        preset("Best").apply(&mut output, &mut settings);
        assert!(preset("Best").is_applied(output, &settings));
        // Telegram sets the quality too, so it no longer matches; a preset for something else still does.
        assert!(!preset("Telegram circle").is_applied(output, &settings));
        assert!(preset("MP4, H.264").is_applied(output, &settings));

        settings.rate = Rate::Quality(19);
        assert!(!preset("Best").is_applied(output, &settings));
    }

    #[test]
    fn codecs_follow_the_container() {
        let webm = ExportSettings { container: Container::WebM, ..ExportSettings::default() }.normalized();
        assert_eq!((webm.video_codec, webm.audio_codec), (VideoCodec::Vp9, AudioCodec::Opus));

        // Having no sound suits every container.
        let silent = ExportSettings { container: Container::WebM, audio_codec: AudioCodec::None, ..ExportSettings::default() };
        assert_eq!(silent.normalized().audio_codec, AudioCodec::None);

        // Going back, the codec changes only if it has to.
        let mkv = ExportSettings { container: Container::Mkv, ..webm }.normalized();
        assert_eq!((mkv.video_codec, mkv.audio_codec), (VideoCodec::Vp9, AudioCodec::Opus));
        let mp4 = ExportSettings { container: Container::Mp4, ..webm }.normalized();
        assert_eq!((mp4.video_codec, mp4.audio_codec), (VideoCodec::H264, AudioCodec::Aac));
    }

    #[test]
    fn a_preset_highlights_lines_it_forced_to_change() {
        let (mut output, mut settings) = (Output::default(), ExportSettings::default());
        let format_only = ExportPreset { container: Some(Container::WebM), ..NOTHING };
        let lines = format_only.apply(&mut output, &mut settings);
        assert_eq!(lines, [Setting::Format, Setting::VideoCodec, Setting::AudioCodec]);
    }
}

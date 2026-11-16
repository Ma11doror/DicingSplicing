//! The edit: clips of sources placed on tracks, and the range of the timeline that is exported.
//!
//! All times here are timeline seconds unless a name says `source`. They can be negative: the
//! user sees time counted from the export start, so the timeline's own zero means nothing to them.
//! A clip plays its source from `source_in` to `source_out`, starting at `start`.

use crate::core::export::ExportSettings;
use crate::core::preset::Output;
use crate::media::geometry::Transform;
use crate::media::probe::MediaInfo;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Closer than this, two times are the same one: a saved file may not bring back the last digit.
const SAME_TIME: f64 = 1e-6;
/// How much a source's length may differ from what the project remembers: rounding, not a new file.
const SOURCE_DURATION_ROUNDING: f64 = 1e-3;

/// A time range, start inclusive, end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    /// From the start of the earlier span to the end of the later one.
    pub fn union(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    /// Drags the start edge; the end stays.
    pub fn resize_start(&mut self, t: f64, min_len: f64) {
        self.start = t.min(self.end - min_len);
    }

    /// Drags the end edge; the start stays.
    pub fn resize_end(&mut self, t: f64, min_len: f64) {
        self.end = t.max(self.start + min_len);
    }

    /// Puts the start at `t`; the end follows only if it would be too close.
    pub fn set_start(&mut self, t: f64, min_len: f64) {
        self.start = t;
        self.end = self.end.max(t + min_len);
    }

    /// Puts the end at `t`; the start follows only if it would be too close.
    pub fn set_end(&mut self, t: f64, min_len: f64) {
        self.end = t;
        self.start = self.start.min(t - min_len);
    }

    /// Moves the whole span, keeping its length.
    pub fn move_to(&mut self, start: f64) {
        self.end = start + self.len();
        self.start = start;
    }
}

/// Which of a project's sources something comes from.
pub type SourceId = u32;

/// How long a fade is when it is added, like the sound fades of the export.
pub const DEFAULT_FADE: f64 = 1.0;
/// How long an image lasts when it is put on the timeline.
pub const IMAGE_LENGTH: f64 = 5.0;
/// How long a frame is held when a freeze frame is made.
pub const FREEZE_LENGTH: f64 = 2.0;

/// An edge of a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    In,
    Out,
}

/// The clip comes in (at its start) or goes out (at its end) gradually: its picture from or into
/// what is under it, its sound from or into silence.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fade {
    pub edge: Edge,
    pub seconds: f64,
    pub picture: bool,
    pub sound: bool,
    /// How it goes over its length, for the picture and the sound alike.
    #[serde(default)]
    pub curve: Curve,
}

/// How a fade goes over its length. The names are about time, as in animation: an ease in starts
/// slowly and an ease out ends slowly, whether the clip comes in or goes out. Strong ones are
/// cubic, the others quadratic, and the ease in-out a half of a sine wave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Curve {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    StrongEaseIn,
    StrongEaseOut,
    StrongEaseInOut,
}

impl Curve {
    pub const ALL: [Curve; 7] =
        [Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::EaseInOut, Curve::StrongEaseIn, Curve::StrongEaseOut, Curve::StrongEaseInOut];

    pub fn name(self) -> &'static str {
        match self {
            Curve::Linear => "Linear",
            Curve::EaseIn => "Ease in",
            Curve::EaseOut => "Ease out",
            Curve::EaseInOut => "Ease in-out",
            Curve::StrongEaseIn => "Strong ease in",
            Curve::StrongEaseOut => "Strong ease out",
            Curve::StrongEaseInOut => "Strong ease in-out",
        }
    }

    /// How far the change has got when `x` of the fade's length has gone by: both from 0 to 1.
    pub fn ease(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Curve::Linear => x,
            Curve::EaseIn => x * x,
            Curve::EaseOut => 1.0 - (1.0 - x).powi(2),
            Curve::EaseInOut => (1.0 - (std::f64::consts::PI * x).cos()) / 2.0,
            Curve::StrongEaseIn => x.powi(3),
            Curve::StrongEaseOut => 1.0 - (1.0 - x).powi(3),
            Curve::StrongEaseInOut if x < 0.5 => 4.0 * x.powi(3),
            Curve::StrongEaseInOut => 1.0 - 4.0 * (1.0 - x).powi(3),
        }
    }

    /// How much of the clip shows, or sounds, when `x` of a fade at `edge` has gone by.
    pub fn level(self, edge: Edge, x: f64) -> f64 {
        match edge {
            Edge::In => self.ease(x),
            Edge::Out => 1.0 - self.ease(x),
        }
    }
}

/// How much the colour of temperature 100 moves red up and blue down.
const TEMPERATURE_STRENGTH: f64 = 0.15;

/// A clip's colour: how bright, how contrasty, how colourful and how warm. It is worked out on red,
/// green and blue: each is multiplied (the exposure and the temperature), spread around the middle
/// (the contrast), and mixed with the pixel's brightness (the saturation).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Color {
    /// In stops: +1 is twice as bright, -1 half.
    pub exposure: f64,
    /// -100 is flat gray, 0 leaves it, 100 is twice the contrast.
    pub contrast: f64,
    /// Percent: 0 is black and white, 100 leaves it, 200 is twice as colourful.
    pub saturation: f64,
    /// -100 is colder (bluer), 100 warmer (redder).
    pub temperature: f64,
}

impl Default for Color {
    fn default() -> Self {
        Self { exposure: 0.0, contrast: 0.0, saturation: 100.0, temperature: 0.0 }
    }
}

impl Color {
    pub const EXPOSURE: std::ops::RangeInclusive<f64> = -2.0..=2.0;
    pub const CONTRAST: std::ops::RangeInclusive<f64> = -100.0..=100.0;
    pub const SATURATION: std::ops::RangeInclusive<f64> = 0.0..=200.0;
    pub const TEMPERATURE: std::ops::RangeInclusive<f64> = -100.0..=100.0;

    /// It changes nothing.
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    /// What red, green and blue are multiplied by before the contrast.
    pub fn gains(&self) -> [f64; 3] {
        let light = 2f64.powf(self.exposure);
        let warm = self.temperature / 100.0 * TEMPERATURE_STRENGTH;
        [light * (1.0 + warm), light, light * (1.0 - warm)]
    }

    /// How far from the middle a value goes for each step it was from it.
    pub fn contrast_factor(&self) -> f64 {
        1.0 + self.contrast / 100.0
    }

    pub fn saturation_factor(&self) -> f64 {
        self.saturation / 100.0
    }
}

/// How fast a clip plays its source: 2 is twice as fast, so half as long on the timeline, and 0.5
/// half as fast. Its sound keeps its pitch, like a video sped up on YouTube.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Speed {
    pub factor: f64,
}

impl Speed {
    pub const MIN: f64 = 0.25;
    pub const MAX: f64 = 16.0;

    /// The factor as the clip plays it: within the limits, and 1 for what is no speed at all, like
    /// 0 or a negative number written into a project file.
    pub fn played(self) -> f64 {
        playable(self.factor)
    }
}

/// A speed as it is written for people: ×2, ×0.5, ×1.25.
pub fn speed_text(factor: f64) -> String {
    let digits = format!("{factor:.2}");
    format!("×{}", digits.trim_end_matches('0').trim_end_matches('.'))
}

/// A speed factor within the limits; 1 for what is no speed.
fn playable(factor: f64) -> f64 {
    if factor.is_finite() && factor > 0.0 {
        factor.clamp(Speed::MIN, Speed::MAX)
    } else {
        1.0
    }
}

/// A frame of the clip's source held on screen: when the clip gets to the moment `at` of its source,
/// it stays there for `seconds`, without sound, and then plays on from there. The clip is that much
/// longer. Its fades stay in the clip's own time and go on through it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Freeze {
    /// The moment of the source whose frame is held, in source seconds.
    pub at: f64,
    /// How long it is held, in seconds of the timeline.
    pub seconds: f64,
}

/// Something done to a clip. Effects apply in the order of the clip's list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Effect {
    Fade(Fade),
    Color(Color),
    Speed(Speed),
    Freeze(Freeze),
}

impl Effect {
    fn as_fade(&self) -> Option<&Fade> {
        match self {
            Effect::Fade(fade) => Some(fade),
            _ => None,
        }
    }

    fn as_speed(&self) -> Option<&Speed> {
        match self {
            Effect::Speed(speed) => Some(speed),
            _ => None,
        }
    }

    pub fn as_freeze(&self) -> Option<&Freeze> {
        match self {
            Effect::Freeze(freeze) => Some(freeze),
            _ => None,
        }
    }

    fn is_fade_at(&self, edge: Edge) -> bool {
        self.as_fade().is_some_and(|fade| fade.edge == edge)
    }
}

/// A frame a clip holds, where it is on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hold {
    /// Which of the clip's effects it is: its Freeze.
    pub effect: usize,
    /// The moment of the source whose frame is held.
    pub at: f64,
    pub span: Span,
}

/// A part of a clip's time in which its source goes on at one rate: as fast as the clip plays it,
/// or not at all while a frame is held.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    /// Where it is on the timeline.
    pub span: Span,
    /// The source time at its start.
    pub source: f64,
    /// How many seconds of the source go by in a second of the timeline: 0 while a frame is held.
    pub rate: f64,
}

impl Run {
    pub fn is_held(&self) -> bool {
        self.rate == 0.0
    }

    /// Source time at timeline time `t`, also outside the run.
    pub fn source_time(&self, t: f64) -> f64 {
        self.source + (t - self.span.start) * self.rate
    }
}

/// A piece of a source on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    #[serde(default)]
    pub source: SourceId,
    /// Where the clip starts on the timeline.
    pub start: f64,
    /// The part of the source that plays, in source seconds.
    pub source_in: f64,
    pub source_out: f64,
    pub source_duration: f64,
    pub transform: Transform,
    /// Sound level: 1 leaves it as it is.
    #[serde(default = "full_volume")]
    pub volume: f64,
    #[serde(default)]
    pub muted: bool,
    /// What is done to it, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    /// Its sound was made a clip of its own, on an audio track: this one is only seen.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sound_detached: bool,
    /// Clips with the same link are chained to each other: moved in time, they go together. A clip
    /// and the sound detached from it are, until one of them is unchained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkId>,
    /// It is a still: it shows the frame of its source at this source time all along, an image or a
    /// freeze frame, and has no sound. Its `source_in` and `source_out` count its own time then, from
    /// 0: they say how long it lasts, which its source does not limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still: Option<f64>,
}

/// What chained clips have in common.
pub type LinkId = u32;

fn full_volume() -> f64 {
    1.0
}

impl Clip {
    /// The whole first source, from the start of the timeline.
    #[cfg(test)]
    pub fn new(name: String, source_duration: f64) -> Self {
        Self::of(0, name, source_duration)
    }

    /// The whole of `source`, from the start of the timeline.
    pub fn of(source: SourceId, name: String, source_duration: f64) -> Self {
        Self {
            name,
            source,
            start: 0.0,
            source_in: 0.0,
            source_out: source_duration,
            source_duration,
            transform: Transform::default(),
            volume: full_volume(),
            muted: false,
            effects: Vec::new(),
            sound_detached: false,
            link: None,
            still: None,
        }
    }

    /// The picture of an image `source`, for `IMAGE_LENGTH` seconds from the start of the timeline.
    pub fn image(source: SourceId, name: String) -> Self {
        Self { still: Some(0.0), source_out: IMAGE_LENGTH, ..Self::of(source, name, 0.0) }
    }

    /// All of `source`, with `info` in it, from the start of the timeline: a recording as long as it
    /// is, an image for `IMAGE_LENGTH` seconds.
    pub fn whole(source: SourceId, name: String, info: &MediaInfo) -> Self {
        if info.image {
            Self::image(source, name)
        } else {
            Self::of(source, name, info.duration)
        }
    }

    /// Whether it shows an image: a still of a source with no length of its own.
    pub fn is_image(&self) -> bool {
        self.still.is_some() && self.source_duration <= 0.0
    }

    /// The source turned out to have another length: its file was replaced. The trim stays as far as
    /// it can; if nothing of it is left, the clip takes the whole source. A still keeps its length,
    /// and its frame stays within the source; a clip whose file is an image now, with no length,
    /// becomes a still of it, as long as it was.
    pub fn refit_source(&mut self, duration: f64) {
        if self.still.is_none() && duration <= 0.0 {
            let length = self.span().len();
            (self.still, self.source_in, self.source_out) = (Some(0.0), 0.0, length);
        }
        self.source_duration = duration;
        if let Some(frame) = &mut self.still {
            *frame = frame.min(duration).max(0.0);
            return;
        }
        if self.source_in >= duration {
            self.source_in = 0.0;
            self.source_out = duration;
        }
        self.source_out = self.source_out.min(duration);
        // A frame held that is not in what it plays any more goes.
        let (from, to) = (self.source_in, self.source_out);
        self.effects.retain(|effect| effect.as_freeze().is_none_or(|freeze| freeze.at >= from - SAME_TIME && freeze.at <= to + SAME_TIME));
    }

    /// What the clip's sound is multiplied by.
    pub fn gain(&self) -> f64 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    /// How fast it plays its source: 1 without a Speed effect, and for a still, which shows one
    /// frame however fast.
    pub fn speed(&self) -> f64 {
        if self.still.is_some() {
            return 1.0;
        }
        self.speed_effect().map_or(1.0, |speed| speed.played())
    }

    /// Its Speed effect, if it has one.
    pub fn speed_effect(&self) -> Option<&Speed> {
        self.effects.iter().find_map(Effect::as_speed)
    }

    /// Adds a Speed effect that changes nothing yet, unless there is one or the clip is a still.
    /// Returns whether it was added. Its length stays: another speed is set by `Sequence::set_speed`,
    /// which moves the clips after it.
    pub fn add_speed(&mut self) -> bool {
        if self.speed_effect().is_some() || self.still.is_some() {
            return false;
        }
        self.effects.push(Effect::Speed(Speed { factor: 1.0 }));
        true
    }

    /// Plays its source `factor` times as fast, by its Speed effect, added if there is none. It
    /// keeps its start, and its end follows.
    fn set_speed(&mut self, factor: f64) {
        let factor = playable(factor);
        match self.effects.iter_mut().find_map(|effect| match effect {
            Effect::Speed(speed) => Some(speed),
            _ => None,
        }) {
            Some(speed) => speed.factor = factor,
            None => self.effects.push(Effect::Speed(Speed { factor })),
        }
    }

    fn remove_speed(&mut self) {
        self.effects.retain(|effect| effect.as_speed().is_none());
    }

    pub fn end(&self) -> f64 {
        self.start + (self.source_out - self.source_in) / self.speed() + self.held()
    }

    pub fn span(&self) -> Span {
        Span { start: self.start, end: self.end() }
    }

    /// Its Freeze effects that hold a frame of what it plays, for some time, with their places in its
    /// list of effects. A still holds its one frame anyway.
    fn freezes(&self) -> impl Iterator<Item = (usize, &Freeze)> {
        let (from, to, moving) = (self.source_in, self.source_out, self.still.is_none());
        self.effects.iter().enumerate().filter_map(|(index, effect)| Some((index, effect.as_freeze()?))).filter(move |(_, freeze)| {
            moving && freeze.seconds.is_finite() && freeze.seconds > 0.0 && freeze.at >= from - SAME_TIME && freeze.at <= to + SAME_TIME
        })
    }

    /// How long it holds frames, all of them together.
    fn held(&self) -> f64 {
        self.freezes().map(|(_, freeze)| freeze.seconds).sum()
    }

    /// The frames it holds, in the order they come, each where it is on the timeline.
    pub fn holds(&self) -> Vec<Hold> {
        let mut freezes: Vec<(usize, Freeze)> = self.freezes().map(|(effect, freeze)| (effect, *freeze)).collect();
        freezes.sort_by(|a, b| a.1.at.total_cmp(&b.1.at).then(a.0.cmp(&b.0)));
        let speed = self.speed();
        let mut held = 0.0;
        freezes
            .into_iter()
            .map(|(effect, freeze)| {
                let at = clamp(freeze.at, self.source_in, self.source_out);
                let start = self.start + (at - self.source_in) / speed + held;
                held += freeze.seconds;
                Hold { effect, at, span: Span { start, end: start + freeze.seconds } }
            })
            .collect()
    }

    /// Its time from its start to its end in runs, one after another: where it plays its source and
    /// where it holds a frame.
    pub fn runs(&self) -> Vec<Run> {
        let speed = self.speed();
        let mut runs = Vec::new();
        let (mut t, mut source) = (self.start, self.source_in);
        for hold in self.holds() {
            if hold.span.start > t + SAME_TIME {
                runs.push(Run { span: Span { start: t, end: hold.span.start }, source, rate: speed });
            }
            runs.push(Run { span: hold.span, source: hold.at, rate: 0.0 });
            (t, source) = (hold.span.end, hold.at);
        }
        let end = self.end();
        if end > t + SAME_TIME || runs.is_empty() {
            runs.push(Run { span: Span { start: t, end }, source, rate: speed });
        }
        runs
    }

    /// The frame it holds at timeline time `t`, if it holds one then.
    pub fn hold_at(&self, t: f64) -> Option<Hold> {
        self.holds().into_iter().find(|hold| hold.span.contains(t))
    }

    /// Where the whole source would be if it were not trimmed. A still is all there is of itself.
    pub fn source_span(&self) -> Span {
        if self.still.is_some() {
            return self.span();
        }
        let offset = self.offset();
        Span { start: offset, end: offset + self.source_duration / self.speed() + self.held() }
    }

    /// Source time at timeline time `t`, also outside the trimmed part: while a frame is held, the
    /// moment of that frame. A still's own time.
    pub fn source_time(&self, t: f64) -> f64 {
        let (mut time, mut source) = (self.start, self.source_in);
        for hold in self.holds() {
            if t < hold.span.start {
                break;
            }
            if t < hold.span.end {
                return hold.at;
            }
            (time, source) = (hold.span.end, hold.at);
        }
        source + (t - time) * self.speed()
    }

    /// The source time of the frame seen at timeline time `t`: a still shows the same one all along.
    pub fn picture_time(&self, t: f64) -> f64 {
        self.still.unwrap_or_else(|| self.source_time(t))
    }

    /// Timeline time at which the source is at `source` seconds: the other way round from
    /// `source_time`. A frame held is first seen where it starts to be held.
    pub fn time_of(&self, source: f64) -> f64 {
        let (mut time, mut from) = (self.start, self.source_in);
        for hold in self.holds() {
            if source <= hold.at + SAME_TIME {
                break;
            }
            (time, from) = (hold.span.end, hold.at);
        }
        time + (source - from) / self.speed()
    }

    /// The moment of its source that its Freeze `effect` would hold to be held from timeline time
    /// `t` on: what it shows at `t` without that hold.
    pub fn moment_without(&self, effect: usize, t: f64) -> f64 {
        let mut without = self.clone();
        without.remove_effects(&[effect]);
        without.source_time(t).clamp(self.source_in.min(self.source_out), self.source_out)
    }

    /// Holds the frame at the moment `at` of its source `seconds` longer: one it holds already, or a
    /// new one.
    fn add_freeze(&mut self, at: f64, seconds: f64) {
        match self.freeze_at_mut(at) {
            Some(freeze) => freeze.seconds += seconds,
            None => self.effects.push(Effect::Freeze(Freeze { at, seconds })),
        }
    }

    /// Its Freeze of the moment `at` of its source, if it holds that frame.
    fn freeze_at_mut(&mut self, at: f64) -> Option<&mut Freeze> {
        let held: Vec<usize> = self.freezes().filter(|(_, freeze)| (freeze.at - at).abs() < SAME_TIME).map(|(effect, _)| effect).collect();
        let effect = *held.first()?;
        match &mut self.effects[effect] {
            Effect::Freeze(freeze) => Some(freeze),
            _ => None,
        }
    }

    /// Takes the effects at `indices` of its list off.
    fn remove_effects(&mut self, indices: &[usize]) {
        let mut index = 0;
        self.effects.retain(|_| {
            index += 1;
            !indices.contains(&(index - 1))
        });
    }

    /// Takes what of its holds is before timeline time `t` off: the ones that end by then go, and
    /// one that goes on through it is held from `t` on only.
    fn cut_holds_before(&mut self, t: f64) {
        let mut gone = Vec::new();
        for hold in self.holds() {
            if hold.span.end <= t + SAME_TIME {
                gone.push(hold.effect);
            } else if let (true, Effect::Freeze(freeze)) = (hold.span.start < t, &mut self.effects[hold.effect]) {
                freeze.seconds = hold.span.end - t;
            }
        }
        self.remove_effects(&gone);
    }

    /// Takes what of its holds is after timeline time `t` off, see `cut_holds_before`.
    fn cut_holds_after(&mut self, t: f64) {
        let mut gone = Vec::new();
        for hold in self.holds() {
            if hold.span.start >= t - SAME_TIME {
                gone.push(hold.effect);
            } else if let (true, Effect::Freeze(freeze)) = (hold.span.end > t, &mut self.effects[hold.effect]) {
                freeze.seconds = t - hold.span.start;
            }
        }
        self.remove_effects(&gone);
    }

    /// Timeline time of the source's start.
    fn offset(&self) -> f64 {
        self.time_of(0.0)
    }

    /// The fade at `edge`, if the clip has one.
    pub fn fade(&self, edge: Edge) -> Option<&Fade> {
        self.effects.iter().filter_map(Effect::as_fade).find(|fade| fade.edge == edge)
    }

    fn fade_mut(&mut self, edge: Edge) -> Option<&mut Fade> {
        self.effects.iter_mut().find_map(|effect| match effect {
            Effect::Fade(fade) if fade.edge == edge => Some(fade),
            _ => None,
        })
    }

    /// The clip's colour, if it has its own.
    pub fn color(&self) -> Option<&Color> {
        self.effects.iter().find_map(|effect| match effect {
            Effect::Color(color) => Some(color),
            _ => None,
        })
    }

    /// The colour the clip is seen in, if it is not as filmed.
    pub fn graded(&self) -> Option<Color> {
        self.color().copied().filter(|color| !color.is_neutral())
    }

    /// Adds a colour correction that changes nothing yet, unless there is one. Returns whether it was
    /// added.
    pub fn add_color(&mut self) -> bool {
        if self.color().is_some() {
            return false;
        }
        self.effects.push(Effect::Color(Color::default()));
        true
    }

    pub fn remove_color(&mut self) {
        self.effects.retain(|effect| !matches!(effect, Effect::Color(_)));
    }

    /// The lengths of the fade in and the fade out as they play: together no longer than the clip.
    pub fn fade_lengths(&self) -> (f64, f64) {
        let len = self.span().len();
        let fade_in = self.fade(Edge::In).map_or(0.0, |fade| fade.seconds.max(0.0)).min(len);
        let fade_out = self.fade(Edge::Out).map_or(0.0, |fade| fade.seconds.max(0.0)).min(len - fade_in);
        (fade_in, fade_out)
    }

    /// How much of the clip's picture shows at timeline time `t`, from 0 to 1, by its fades' curves.
    pub fn opacity(&self, t: f64) -> f64 {
        self.fade_level(t, |fade| fade.picture)
    }

    /// What the clip's fades multiply its sound by at `t`, besides its volume.
    pub fn sound_fade(&self, t: f64) -> f64 {
        self.fade_level(t, |fade| fade.sound)
    }

    fn fade_level(&self, t: f64, applies: impl Fn(&Fade) -> bool) -> f64 {
        let (fade_in, fade_out) = self.fade_lengths();
        let (local, len) = (t - self.start, self.span().len());
        let mut level = 1.0f64;
        if let Some(fade) = self.fade(Edge::In).filter(|fade| fade_in > 0.0 && applies(fade)) {
            level = level.min(fade.curve.level(Edge::In, local / fade_in));
        }
        if let Some(fade) = self.fade(Edge::Out).filter(|fade| fade_out > 0.0 && applies(fade)) {
            level = level.min(fade.curve.level(Edge::Out, (local - (len - fade_out)) / fade_out));
        }
        level.clamp(0.0, 1.0)
    }

    /// The curve of the fade at `edge`; Linear if there is none.
    pub fn fade_curve(&self, edge: Edge) -> Curve {
        self.fade(edge).map_or(Curve::Linear, |fade| fade.curve)
    }

    /// Makes the fade at `edge` go by `curve`, if the clip has one.
    pub fn set_fade_curve(&mut self, edge: Edge, curve: Curve) {
        if let Some(fade) = self.fade_mut(edge) {
            fade.curve = curve;
        }
    }

    /// Adds a fade of the picture and the sound at `edge`, unless there is one already. Returns
    /// whether it was added.
    pub fn add_fade(&mut self, edge: Edge) -> bool {
        if self.fade(edge).is_some() {
            return false;
        }
        self.effects.push(Effect::Fade(Fade { edge, seconds: DEFAULT_FADE, picture: true, sound: true, curve: Curve::Linear }));
        self.fit_fades();
        true
    }

    /// Takes the fade at `edge` off, if there is one.
    pub fn remove_fade(&mut self, edge: Edge) {
        self.effects.retain(|effect| !effect.is_fade_at(edge));
    }

    /// Sets the length of the fade at `edge`, as long as the clip and the other fade leave room for.
    pub fn set_fade_length(&mut self, edge: Edge, seconds: f64) {
        let (fade_in, fade_out) = self.fade_lengths();
        let room = self.span().len() - if edge == Edge::In { fade_out } else { fade_in };
        if let Some(fade) = self.fade_mut(edge) {
            fade.seconds = seconds.clamp(0.0, room.max(0.0));
        }
    }

    /// Keeps the fades within the clip, after it has been trimmed shorter.
    pub fn fit_fades(&mut self) {
        let (fade_in, fade_out) = self.fade_lengths();
        for (edge, seconds) in [(Edge::In, fade_in), (Edge::Out, fade_out)] {
            if let Some(fade) = self.fade_mut(edge) {
                fade.seconds = seconds;
            }
        }
    }

    /// Moves the start edge to `t`, not before `lo`; the picture stays where it is on the timeline.
    /// A frame held before `t` goes, and one held through it is held from `t` on. A still has no
    /// source to run out of: only `lo` stops it.
    fn trim_start(&mut self, t: f64, lo: f64, min_len: f64) {
        if self.still.is_some() {
            let end = self.end();
            self.start = clamp(t, lo, end - min_len);
            self.source_in = 0.0;
            self.source_out = end - self.start;
        } else {
            let start = clamp(t, lo.max(self.offset()), self.end() - min_len);
            if start <= self.start {
                self.source_in = (self.source_in - (self.start - start) * self.speed()).max(0.0);
            } else {
                let source_in = self.source_time(start);
                self.cut_holds_before(start);
                self.source_in = source_in;
            }
            self.start = start;
        }
        self.fit_fades();
    }

    /// Moves the end edge to `t`, not after `hi`. A frame held after `t` goes, and one held through it
    /// is held up to `t`. A still goes as far as `hi`.
    fn trim_end(&mut self, t: f64, hi: f64, min_len: f64) {
        if self.still.is_some() {
            let end = clamp(t, self.start + min_len, hi);
            self.source_out = self.source_in + (end - self.start);
        } else {
            let (old, speed) = (self.end(), self.speed());
            // Where its source runs out.
            let last = old + (self.source_duration - self.source_out) / speed;
            let end = clamp(t, self.start + min_len, hi.min(last));
            if end >= old {
                self.source_out = (self.source_out + (end - old) * speed).min(self.source_duration);
            } else {
                let source_out = self.source_time(end);
                self.cut_holds_after(end);
                self.source_out = source_out;
            }
        }
        self.fit_fades();
    }

    /// A still's own time counts from 0 again, after a cut has taken its first part.
    fn settle_still(&mut self) {
        if self.still.is_some() {
            self.source_out -= self.source_in;
            self.source_in = 0.0;
        }
    }
}

/// Which track something is about. Unlike its place among the tracks, this does not change.
pub type TrackId = u32;

/// What a track carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    /// The picture of its clips, and their sound unless it was detached.
    #[default]
    Video,
    /// Only the sound of its clips.
    Audio,
}

impl TrackKind {
    fn is_video(&self) -> bool {
        *self == TrackKind::Video
    }
}

/// A layer of the picture, or of sound only. Its clips are sorted by start and do not overlap.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    #[serde(default)]
    pub id: TrackId,
    #[serde(default, skip_serializing_if = "TrackKind::is_video")]
    pub kind: TrackKind,
    /// Its picture is left out: what is under it shows.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// Its sound is left out.
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    pub clips: Vec<Clip>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Track {
    pub fn clip_at(&self, t: f64) -> Option<usize> {
        self.clips.iter().position(|c| c.span().contains(t))
    }

    /// Trims a clip's start, up to the end of the clip before it.
    pub fn trim_start(&mut self, index: usize, t: f64, min_len: f64) {
        let lo = self.free_before(index);
        self.clips[index].trim_start(t, lo, min_len);
    }

    /// Trims a clip's end, up to the start of the clip after it.
    pub fn trim_end(&mut self, index: usize, t: f64, min_len: f64) {
        let hi = self.free_after(index);
        self.clips[index].trim_end(t, hi, min_len);
    }

    /// Puts a clip's start at `t`, between its neighbours. What it plays does not change. The
    /// sequence moves clips with what is chained to them: see `Sequence::move_clip`.
    #[cfg(test)]
    pub fn move_clip(&mut self, index: usize, t: f64) {
        let len = self.clips[index].span().len();
        self.clips[index].start = clamp(t, self.free_before(index), self.free_after(index) - len);
    }

    /// The free time around `t`, if no clip is there.
    pub fn gap_at(&self, t: f64) -> Option<Span> {
        if self.clip_at(t).is_some() {
            return None;
        }
        let start = self.clips.iter().map(Clip::end).filter(|&end| end <= t).fold(f64::NEG_INFINITY, f64::max);
        let end = self.clips.iter().map(|c| c.start).filter(|&start| start > t).fold(f64::INFINITY, f64::min);
        Some(Span { start, end })
    }

    /// Puts a clip in its place by time. It must not overlap the others.
    fn insert(&mut self, clip: Clip) -> usize {
        let index = self.clips.partition_point(|c| c.start < clip.start);
        self.clips.insert(index, clip);
        index
    }

    /// Where the clip before this one ends.
    fn free_before(&self, index: usize) -> f64 {
        index.checked_sub(1).map_or(f64::NEG_INFINITY, |i| self.clips[i].end())
    }

    /// Where the clip after this one starts.
    fn free_after(&self, index: usize) -> f64 {
        self.clips.get(index + 1).map_or(f64::INFINITY, |c| c.start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipId {
    pub track: usize,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    /// Bottom layer first.
    pub tracks: Vec<Track>,
    /// What goes into the output.
    pub export: Span,
    /// The format of the output.
    pub output: Output,
    /// How the output is encoded.
    pub settings: ExportSettings,
}

impl Sequence {
    /// One track with one clip; everything of it is exported.
    pub fn new(clip: Clip) -> Self {
        let export = clip.span();
        let track = Track { clips: vec![clip], ..Track::default() };
        Self { tracks: vec![track], export, output: Output::default(), settings: ExportSettings::default() }
    }

    pub fn clip(&self, id: ClipId) -> &Clip {
        &self.tracks[id.track].clips[id.index]
    }

    pub fn clip_mut(&mut self, id: ClipId) -> &mut Clip {
        &mut self.tracks[id.track].clips[id.index]
    }

    pub fn clips(&self) -> impl Iterator<Item = (ClipId, &Clip)> {
        self.tracks.iter().enumerate().flat_map(|(track, t)| {
            t.clips.iter().enumerate().map(move |(index, clip)| (ClipId { track, index }, clip))
        })
    }

    /// Whether `id` is a clip of this sequence.
    pub fn has(&self, id: ClipId) -> bool {
        self.tracks.get(id.track).is_some_and(|track| id.index < track.clips.len())
    }

    /// The clip whose picture is seen at `t`: the topmost one on a video track that is not hidden. It
    /// covers the whole frame, bars included, so nothing under it shows.
    pub fn shown_at(&self, t: f64) -> Option<ClipId> {
        let mut visible = self.tracks.iter().enumerate().rev().filter(|(_, track)| track.kind == TrackKind::Video && !track.hidden);
        visible.find_map(|(track, clips)| Some(ClipId { track, index: clips.clip_at(t)? }))
    }

    /// The clip seen at `t` under the tracks from `track` up: what a clip on `track` shows through
    /// while it fades.
    pub fn shown_under(&self, t: f64, track: usize) -> Option<ClipId> {
        let below = self.tracks[..track.min(self.tracks.len())].iter().enumerate().rev();
        let mut visible = below.filter(|(_, track)| track.kind == TrackKind::Video && !track.hidden);
        visible.find_map(|(track, clips)| Some(ClipId { track, index: clips.clip_at(t)? }))
    }

    /// Whether the sound of a clip is heard: on a track that is not muted, not muted itself, and on
    /// an audio track, or on a video track if its sound was not detached. A still has none.
    pub fn sounds(&self, id: ClipId) -> bool {
        let (track, clip) = (&self.tracks[id.track], self.clip(id));
        !track.muted && clip.gain() > 0.0 && (track.kind == TrackKind::Audio || !clip.sound_detached) && clip.still.is_none()
    }

    /// The video tracks' clips: what can be seen.
    fn seen_clips(&self) -> impl Iterator<Item = (ClipId, &Clip)> {
        self.clips().filter(|(id, _)| self.tracks[id.track].kind == TrackKind::Video && !self.tracks[id.track].hidden)
    }

    /// What the picture is made of in `range`: pieces of it, each with the clip that is seen there.
    /// Pieces that follow each other with the same clip are one.
    pub fn picture(&self, range: Span) -> Vec<(Span, Option<ClipId>)> {
        let mut cuts = vec![range.start, range.end];
        for (_, clip) in self.seen_clips() {
            cuts.extend([clip.start, clip.end()].into_iter().filter(|&t| t > range.start && t < range.end));
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() < SAME_TIME);
        let mut pieces: Vec<(Span, Option<ClipId>)> = Vec::new();
        for pair in cuts.windows(2) {
            let span = Span { start: pair[0], end: pair[1] };
            let shown = self.shown_at((span.start + span.end) / 2.0);
            match pieces.last_mut() {
                Some((last, clip)) if *clip == shown => last.end = span.end,
                _ => pieces.push((span, shown)),
            }
        }
        pieces
    }

    /// The source seen longest in the export range: what the result is mostly made of.
    pub fn main_source(&self) -> Option<SourceId> {
        let mut seen: Vec<(SourceId, f64)> = Vec::new();
        for (span, id) in self.picture(self.export) {
            let Some(id) = id else { continue };
            let source = self.clip(id).source;
            match seen.iter_mut().find(|(known, _)| *known == source) {
                Some((_, time)) => *time += span.len(),
                None => seen.push((source, span.len())),
            }
        }
        seen.into_iter().reduce(|best, next| if next.1 > best.1 { next } else { best }).map(|(source, _)| source)
    }

    /// How many clips play `source`.
    pub fn uses(&self, source: SourceId) -> usize {
        self.clips().filter(|(_, clip)| clip.source == source).count()
    }

    /// The source turned out to have another length: see `Clip::refit_source`. Returns whether a
    /// clip changed.
    pub fn refit_source(&mut self, source: SourceId, duration: f64) -> bool {
        let mut changed = false;
        for clip in self.tracks.iter_mut().flat_map(|track| &mut track.clips).filter(|clip| clip.source == source) {
            if (clip.source_duration - duration).abs() > SOURCE_DURATION_ROUNDING {
                clip.refit_source(duration);
                changed = true;
            }
        }
        changed
    }

    /// Tracks that came without ids of their own, from a file of the first version, get them.
    pub fn number_tracks(&mut self) {
        let mut next = self.next_track_id();
        let mut seen = BTreeSet::new();
        for track in &mut self.tracks {
            if !seen.insert(track.id) {
                track.id = next;
                seen.insert(next);
                next += 1;
            }
        }
    }

    fn next_track_id(&self) -> TrackId {
        self.tracks.iter().map(|track| track.id + 1).max().unwrap_or(0)
    }

    /// Puts a clip on a new track of `kind` under all the others, right after the last clip: there
    /// nothing covers it. An export range that fitted the clips, or had none to fit, takes it in.
    pub fn add_below(&mut self, mut clip: Clip, kind: TrackKind) -> ClipId {
        clip.start = self.content().map_or(self.export.start, |content| content.end);
        let track = self.add_track(0, kind);
        self.place_clip(Some(track), clip).unwrap_or(ClipId { track, index: 0 })
    }

    /// Puts a clip on `track`, as near its start as the free time around its middle allows, or with
    /// no track given, on a new track under all the others. None if it does not fit on the track.
    /// An export range that fitted the clips, or had none to fit, takes it in.
    pub fn place_clip(&mut self, track: Option<usize>, mut clip: Clip) -> Option<ClipId> {
        let fit = self.export_fits() || self.content().is_none();
        let id = match track {
            Some(track) => {
                let len = clip.span().len();
                let gap = self.tracks.get(track)?.gap_at(clip.start + len / 2.0)?;
                if gap.len() + SAME_TIME < len {
                    return None;
                }
                clip.start = clamp(clip.start, gap.start, gap.end - len);
                ClipId { track, index: self.tracks[track].insert(clip) }
            }
            None => {
                let id = self.next_track_id();
                self.tracks.insert(0, Track { id, clips: vec![clip], ..Track::default() });
                ClipId { track: 0, index: 0 }
            }
        };
        if fit {
            self.fit_export();
        }
        Some(id)
    }

    /// Whether `len` seconds from `start` are free on `track`.
    fn free_on(&self, track: usize, start: f64, len: f64) -> bool {
        let gap = self.tracks.get(track).and_then(|t| t.gap_at(start + len / 2.0));
        gap.is_some_and(|gap| gap.start <= start + SAME_TIME && gap.end >= start + len - SAME_TIME)
    }

    /// Puts `clip` at `t` exactly: on `track` if it is of `kind` and free there, else on a new track
    /// of `kind` right above it, or above all the others with no track given. A copy is chained to
    /// nothing. Returns where it went.
    pub fn paste(&mut self, mut clip: Clip, t: f64, track: Option<usize>, kind: TrackKind) -> ClipId {
        clip.start = t;
        clip.link = None;
        let len = clip.span().len();
        let target = match track {
            Some(track) if self.tracks.get(track).is_some_and(|it| it.kind == kind) && self.free_on(track, t, len) => track,
            Some(track) => self.add_track(track + 1, kind),
            None => self.add_track(self.tracks.len(), kind),
        };
        self.place_clip(Some(target), clip).unwrap_or(ClipId { track: target, index: 0 })
    }

    /// A copy of a clip, with its effects, right after it. Returns the copy.
    pub fn duplicate(&mut self, id: ClipId) -> ClipId {
        let clip = self.clip(id).clone();
        let end = clip.end();
        let kind = self.tracks[id.track].kind;
        self.paste(clip, end, Some(id.track), kind)
    }

    /// Removes a clip, and moves the clips after it on its track back by its length: no gap is left
    /// where it was. Each of them takes the clips chained to it along, while all of them have room;
    /// the first that has none stays, and so do the ones after it. Other tracks stay as they are.
    pub fn ripple_delete(&mut self, id: ClipId) {
        let len = self.clip(id).span().len();
        let fitted = self.export_fits();
        self.tracks[id.track].clips.remove(id.index);
        let mut moved: Vec<ClipId> = Vec::new();
        for index in id.index..self.tracks[id.track].clips.len() {
            let next = ClipId { track: id.track, index };
            if moved.contains(&next) {
                continue;
            }
            let group: Vec<ClipId> = self.group_of(next).into_iter().filter(|member| !moved.contains(member)).collect();
            if self.room_for(&group, -len) > -len + SAME_TIME {
                break;
            }
            self.shift(&group, -len);
            moved.extend(group);
        }
        self.tidy_links();
        if fitted {
            self.fit_export();
        }
    }

    /// Puts `clip` on `track` where its clip at `index` starts, right after its last clip when
    /// `index` is how many it has, or at the start of the export range when it has none. The clips
    /// from `index` on move later by the new clip's length, each with the clips chained to it. None,
    /// with nothing changed, if one of them can't go that far: a clip chained to it has no room on
    /// its own track. An export range that fitted the clips, or had none to fit, takes it in.
    pub fn ripple_insert(&mut self, track: usize, index: usize, clip: Clip) -> Option<ClipId> {
        let fit = self.export_fits() || self.content().is_none();
        let mut next = self.clone();
        let id = next.insert_shifting(track, index, clip)?;
        if fit {
            next.fit_export();
        }
        *self = next;
        Some(id)
    }

    /// Takes the clip at `from` on `track` out, closing the gap it leaves, and puts it back as the
    /// clip at `to`, moving the ones from there on later: clips that went one after another still
    /// do, in the new order. None, with nothing changed, if the clip is chained to another, or the
    /// clips after either place can't move with the clips chained to them.
    pub fn ripple_move(&mut self, track: usize, from: usize, to: usize) -> Option<ClipId> {
        let count = self.tracks.get(track)?.clips.len();
        if from >= count || to >= count || self.is_chained(ClipId { track, index: from }) {
            return None;
        }
        let fit = self.export_fits();
        let mut next = self.clone();
        let clip = next.tracks[track].clips.remove(from);
        if !next.ripple_shift(track, from, -clip.span().len()) {
            return None;
        }
        let id = next.insert_shifting(track, to, clip)?;
        if fit {
            next.fit_export();
        }
        *self = next;
        Some(id)
    }

    /// Makes a clip play another part of its source, `part` in source seconds. It keeps its start,
    /// and the clips after it on its track move by as much as it got longer or shorter, each with
    /// the clips chained to it; its fades shorten to fit it. False, with nothing changed, if the
    /// part is not in the source, the clip is chained to another or is a still, which plays no part
    /// of its source, or the clips after it can't move that far.
    pub fn ripple_retime(&mut self, id: ClipId, part: Span) -> bool {
        if !self.has(id) || self.is_chained(id) || self.clip(id).still.is_some() {
            return false;
        }
        let clip = self.clip(id);
        if part.start < 0.0 || part.len() < SAME_TIME || part.end > clip.source_duration + SAME_TIME {
            return false;
        }
        // The frames it holds stay held, those in the new part.
        let mut changed = clip.clone();
        changed.source_in = part.start;
        changed.source_out = part.end.min(clip.source_duration);
        changed.effects.retain(|effect| effect.as_freeze().is_none_or(|freeze| freeze.at >= part.start - SAME_TIME && freeze.at <= part.end + SAME_TIME));
        changed.fit_fades();
        let delta = changed.span().len() - clip.span().len();
        let fit = self.export_fits();
        let mut next = self.clone();
        let later = |sequence: &mut Sequence| sequence.ripple_shift(id.track, id.index + 1, delta);
        // A clip that grows needs the room first; one that shrinks leaves it first.
        if delta > 0.0 && !later(&mut next) {
            return false;
        }
        *next.clip_mut(id) = changed;
        if delta <= 0.0 && !later(&mut next) {
            return false;
        }
        if fit {
            next.fit_export();
        }
        *self = next;
        true
    }

    /// Makes clip `id` play its source `factor` times as fast, by its Speed effect, added if it has
    /// none: see `resize` for the clips after it. False, with nothing changed, if they can't move
    /// that far, or the clip is a still, which shows one frame however fast.
    pub fn set_speed(&mut self, id: ClipId, factor: f64) -> bool {
        self.has(id) && self.clip(id).still.is_none() && self.resize(id, |clip| clip.set_speed(factor))
    }

    /// Makes still clip `id` last `seconds`: see `resize` for the clips after it. False, with nothing
    /// changed, if it is no still, `seconds` is no length, or the clips after it can't move that far.
    pub fn set_length(&mut self, id: ClipId, seconds: f64) -> bool {
        let still = self.has(id) && self.clip(id).still.is_some();
        still && seconds.is_finite() && seconds > 0.0 && self.resize(id, |clip| clip.source_out = clip.source_in + seconds)
    }

    /// Takes clip `id`'s Speed effect off: it plays its source as it is again, see `set_speed`.
    pub fn remove_speed(&mut self, id: ClipId) -> bool {
        self.resize(id, Clip::remove_speed)
    }

    /// Makes `change` to clip `id`, which may make it longer or shorter. It keeps its start, its fades
    /// shorten to fit it, and the clips after it on its track move by as much as it got longer or
    /// shorter, each with the clips chained to it, and so do the clips chained to `id` that start
    /// where it ends or later. `id` and the rest of what is chained to it stay, and so do the other
    /// tracks. An export range that fitted the clips goes on fitting them. False, with nothing
    /// changed, if what has to move can't go that far: a clip chained to one of them has no room on
    /// its own track.
    fn resize(&mut self, id: ClipId, change: impl FnOnce(&mut Clip)) -> bool {
        if !self.has(id) {
            return false;
        }
        let mut changed = self.clip(id).clone();
        change(&mut changed);
        let end = self.clip(id).end();
        self.ripple(vec![(id, changed)], end)
    }

    /// Puts the `changed` clips in place of the clips they were, each of them as much longer or
    /// shorter as the others, from timeline time `from` on: their fades shorten to fit them, the
    /// clips after each of them on its track move by as much, each with the clips chained to it, and
    /// so do the clips chained to them that start at `from` or later. The rest of what is chained to
    /// them stays, and so do the other tracks. An export range that fitted the clips goes on fitting
    /// them. False, with nothing changed, if they did not change by as much, or what has to move
    /// can't go that far: a clip chained to one of them has no room on its own track.
    fn ripple(&mut self, mut changed: Vec<(ClipId, Clip)>, from: f64) -> bool {
        if changed.is_empty() || changed.iter().any(|(id, _)| !self.has(*id)) {
            return false;
        }
        let mut deltas = Vec::new();
        for (id, clip) in &mut changed {
            clip.fit_fades();
            deltas.push(clip.span().len() - self.clip(*id).span().len());
        }
        let delta = deltas[0];
        if deltas.iter().any(|other| (other - delta).abs() > SAME_TIME) {
            return false;
        }
        let ids: Vec<ClipId> = changed.iter().map(|(id, _)| *id).collect();
        let moving = self.after(&ids, from);
        let fit = self.export_fits();
        let mut next = self.clone();
        let room = |sequence: &Sequence| (sequence.room_for(&moving, delta) - delta).abs() <= SAME_TIME;
        // A clip that grows needs the room first; one that shrinks leaves it first.
        if delta > 0.0 {
            if !room(&next) {
                return false;
            }
            next.shift(&moving, delta);
        }
        for (id, clip) in changed {
            *next.clip_mut(id) = clip;
        }
        if delta <= 0.0 {
            if !room(&next) {
                return false;
            }
            next.shift(&moving, delta);
        }
        if fit {
            next.fit_export();
        }
        *self = next;
        true
    }

    /// What moves when clips `ids` get longer or shorter from timeline time `from` on: the clips after
    /// each of them on its track, each with the clips chained to it, and of the clips chained to
    /// them, those that start at `from` or later. Not `ids`, nor what is chained to them and starts
    /// before `from`.
    fn after(&self, ids: &[ClipId], from: f64) -> Vec<ClipId> {
        let mut own: Vec<ClipId> = Vec::new();
        for &id in ids {
            for member in self.group_of(id) {
                if !own.contains(&member) {
                    own.push(member);
                }
            }
        }
        let mut moving: Vec<ClipId> = own.iter().copied().filter(|member| !ids.contains(member) && self.clip(*member).start >= from - SAME_TIME).collect();
        for &id in ids {
            for index in id.index + 1..self.tracks[id.track].clips.len() {
                let next = ClipId { track: id.track, index };
                for member in self.group_of(next) {
                    if (member == next || !own.contains(&member)) && !ids.contains(&member) && !moving.contains(&member) {
                        moving.push(member);
                    }
                }
            }
        }
        moving
    }

    /// The other clips chained to `id` that show the moment `at` of its source when it does, at its
    /// speed: the sound detached from it, say. A frame it holds, they hold too.
    fn partners(&self, id: ClipId, at: f64) -> Vec<ClipId> {
        let clip = self.clip(id);
        let when = clip.time_of(at);
        let same = |other: &Clip| {
            other.source == clip.source
                && other.still.is_none()
                && (other.speed() - clip.speed()).abs() < SAME_TIME
                && at >= other.source_in - SAME_TIME
                && at <= other.source_out + SAME_TIME
                && (other.time_of(at) - when).abs() < SAME_TIME
        };
        self.group_of(id).into_iter().filter(|&other| other != id && same(self.clip(other))).collect()
    }

    /// Clip `id` and its partners that hold the frame its Freeze `effect` holds, each changed by
    /// `change` with the moment of that frame; and where that hold ends. None if it has no such
    /// Freeze.
    fn changed_holds(&self, id: ClipId, effect: usize, change: impl Fn(&mut Clip, f64)) -> Option<(Vec<(ClipId, Clip)>, Span)> {
        let hold = self.clip(id).holds().into_iter().find(|hold| hold.effect == effect)?;
        let mut changed = Vec::new();
        for member in std::iter::once(id).chain(self.partners(id, hold.at)) {
            let mut clip = self.clip(member).clone();
            if member != id && clip.freeze_at_mut(hold.at).is_none() {
                continue;
            }
            change(&mut clip, hold.at);
            changed.push((member, clip));
        }
        Some((changed, hold.span))
    }

    /// Holds the frame clip `id` shows at `t` for `length` seconds: when the clip gets to that moment
    /// of its source, it stays there, without sound, and then plays on from there. A frame it holds
    /// already is held that much longer. The other clips chained to it that show the same moment at
    /// the same time, like the sound detached from it, hold it too, in silence. They get longer, the
    /// clips after them on their tracks move later, each with the clips chained to it, and so do the
    /// clips chained to them from the hold on: see `ripple`. False, with nothing changed, if the clip
    /// is a still, is not there at `t`, or what has to move can't go that far.
    pub fn freeze(&mut self, id: ClipId, t: f64, length: f64) -> bool {
        if !(self.has(id) && length.is_finite() && length > 0.0) {
            return false;
        }
        let clip = self.clip(id);
        if clip.still.is_some() || !clip.span().contains(t) {
            return false;
        }
        let at = clip.source_time(t);
        // A frame held already is held longer from where it was held up to.
        let from = clip.holds().into_iter().find(|hold| (hold.at - at).abs() < SAME_TIME).map_or_else(|| clip.time_of(at), |hold| hold.span.end);
        let changed = std::iter::once(id)
            .chain(self.partners(id, at))
            .map(|member| {
                let mut clip = self.clip(member).clone();
                clip.add_freeze(at, length);
                (member, clip)
            })
            .collect();
        self.ripple(changed, from)
    }

    /// Makes clip `id`'s Freeze `effect` hold `seconds`, and the same hold of its partners as much
    /// longer or shorter: see `freeze`. False, with nothing changed, if it has no such Freeze,
    /// `seconds` is no length, or what has to move can't go that far.
    pub fn set_freeze_length(&mut self, id: ClipId, effect: usize, seconds: f64) -> bool {
        let Some(held) = self.clip(id).effects.get(effect).and_then(Effect::as_freeze).map(|freeze| freeze.seconds) else {
            return false;
        };
        if !(seconds.is_finite() && seconds > 0.0) {
            return false;
        }
        let delta = seconds - held;
        let longer = |clip: &mut Clip, at: f64| {
            if let Some(freeze) = clip.freeze_at_mut(at) {
                freeze.seconds = (freeze.seconds + delta).max(SAME_TIME);
            }
        };
        let Some((changed, hold)) = self.changed_holds(id, effect, longer) else {
            return false;
        };
        self.ripple(changed, hold.end)
    }

    /// Takes clip `id`'s Freeze `effect` off, and the same hold of its partners: they get shorter,
    /// see `freeze`. False, with nothing changed, if it has no such Freeze, or what has to move can't
    /// go that far.
    pub fn remove_freeze(&mut self, id: ClipId, effect: usize) -> bool {
        let gone = |clip: &mut Clip, at: f64| {
            let held: Vec<usize> = clip.freezes().filter(|(_, freeze)| (freeze.at - at).abs() < SAME_TIME).map(|(effect, _)| effect).take(1).collect();
            clip.remove_effects(&held);
        };
        match self.changed_holds(id, effect, gone) {
            Some((changed, hold)) => self.ripple(changed, hold.end),
            // One that holds nothing just goes.
            None if self.clip(id).effects.get(effect).is_some_and(|effect| effect.as_freeze().is_some()) => {
                self.clip_mut(id).remove_effects(&[effect]);
                true
            }
            None => false,
        }
    }

    /// Makes clip `id`'s Freeze `effect` hold the frame of the moment `at` of its source instead, as
    /// long as before, and the same hold of its partners: the hold moves to where the clip gets to
    /// that moment. `at` stays within what all of them play. Nothing else moves: the clip is as long
    /// as it was. False if it has no such Freeze.
    pub fn move_freeze(&mut self, id: ClipId, effect: usize, at: f64) -> bool {
        let Some(hold) = self.clip(id).holds().into_iter().find(|hold| hold.effect == effect) else {
            return false;
        };
        let mut members = vec![(id, effect)];
        for member in self.partners(id, hold.at) {
            let clip = self.clip(member);
            if let Some((index, _)) = clip.freezes().find(|(_, freeze)| (freeze.at - hold.at).abs() < SAME_TIME) {
                members.push((member, index));
            }
        }
        let lo = members.iter().map(|(member, _)| self.clip(*member).source_in).fold(f64::NEG_INFINITY, f64::max);
        let hi = members.iter().map(|(member, _)| self.clip(*member).source_out).fold(f64::INFINITY, f64::min);
        let at = clamp(at, lo, hi);
        for (member, index) in members {
            if let Effect::Freeze(freeze) = &mut self.clip_mut(member).effects[index] {
                freeze.at = at;
            }
        }
        true
    }

    /// Removes a clip. Its track stays, even when it is left empty.
    pub fn remove_clip(&mut self, id: ClipId) {
        let fitted = self.export_fits();
        self.tracks[id.track].clips.remove(id.index);
        self.tidy_links();
        if fitted {
            self.fit_export();
        }
    }

    /// Removes a track with its clips.
    pub fn remove_track(&mut self, track: usize) {
        let fitted = self.export_fits();
        self.tracks.remove(track);
        self.tidy_links();
        if fitted {
            self.fit_export();
        }
    }

    /// Puts an empty track of `kind` at `index` among the tracks, 0 being the bottom one. Returns
    /// where it is.
    pub fn add_track(&mut self, index: usize, kind: TrackKind) -> usize {
        let index = index.min(self.tracks.len());
        let id = self.next_track_id();
        self.tracks.insert(index, Track { id, kind, ..Track::default() });
        index
    }

    /// The clips chained to `id`, itself among them.
    pub fn group_of(&self, id: ClipId) -> Vec<ClipId> {
        match self.clip(id).link {
            Some(link) => self.clips().filter(|(_, clip)| clip.link == Some(link)).map(|(member, _)| member).collect(),
            None => vec![id],
        }
    }

    /// Whether a clip is chained to another one.
    pub fn is_chained(&self, id: ClipId) -> bool {
        self.group_of(id).len() > 1
    }

    /// How far, of `wanted` seconds, every clip of `group` can move in time: the clips that are not
    /// in it stay where they are, and none may be passed.
    fn room_for(&self, group: &[ClipId], wanted: f64) -> f64 {
        let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
        for &id in group {
            let span = self.clip(id).span();
            for (index, other) in self.tracks[id.track].clips.iter().enumerate() {
                if group.contains(&ClipId { track: id.track, index }) {
                    continue;
                }
                if other.end() <= span.start + SAME_TIME {
                    lo = lo.max(other.end() - span.start);
                } else if other.start >= span.end - SAME_TIME {
                    hi = hi.min(other.start - span.end);
                }
            }
        }
        if lo > hi {
            0.0
        } else {
            clamp(wanted, lo, hi)
        }
    }

    /// Moves every clip of `group` by `delta` seconds. They keep their places among the others.
    fn shift(&mut self, group: &[ClipId], delta: f64) {
        for &id in group {
            self.clip_mut(id).start += delta;
        }
    }

    /// `ripple_insert` without the export range. When it fails, some clips may have moved: it is
    /// done on a copy.
    fn insert_shifting(&mut self, track: usize, index: usize, mut clip: Clip) -> Option<ClipId> {
        let clips = &self.tracks.get(track)?.clips;
        if index > clips.len() {
            return None;
        }
        clip.start = match clips.get(index) {
            Some(next) => next.start,
            None => clips.last().map_or(self.export.start, Clip::end),
        };
        clip.link = None;
        if !self.ripple_shift(track, index, clip.span().len()) {
            return None;
        }
        Some(ClipId { track, index: self.tracks[track].insert(clip) })
    }

    /// Moves the clips of `track` from `index` on by `delta` seconds, each with the clips chained to
    /// it: the last one first when they go later, the first one first when they go earlier, so that
    /// each goes into room the others have left. Whether all of them could go the whole way; when
    /// not, some may have moved: it is done on a copy.
    fn ripple_shift(&mut self, track: usize, index: usize, delta: f64) -> bool {
        let count = self.tracks[track].clips.len();
        let order: Vec<usize> = if delta > 0.0 { (index..count).rev().collect() } else { (index..count).collect() };
        let mut moved: Vec<ClipId> = Vec::new();
        for index in order {
            let id = ClipId { track, index };
            if moved.contains(&id) {
                continue;
            }
            let group: Vec<ClipId> = self.group_of(id).into_iter().filter(|member| !moved.contains(member)).collect();
            if (self.room_for(&group, delta) - delta).abs() > SAME_TIME {
                return false;
            }
            self.shift(&group, delta);
            moved.extend(group);
        }
        true
    }

    fn next_link(&self) -> LinkId {
        self.clips().filter_map(|(_, clip)| clip.link).max().map_or(0, |link| link + 1)
    }

    /// A clip left alone in its group is chained to nothing.
    fn tidy_links(&mut self) {
        let mut counts: BTreeMap<LinkId, usize> = BTreeMap::new();
        for link in self.clips().filter_map(|(_, clip)| clip.link) {
            *counts.entry(link).or_default() += 1;
        }
        for clip in self.tracks.iter_mut().flat_map(|track| &mut track.clips) {
            if clip.link.is_some_and(|link| counts[&link] < 2) {
                clip.link = None;
            }
        }
    }

    /// Chains `id`, with whatever it is chained to, to `to` and whatever that is chained to.
    pub fn chain(&mut self, id: ClipId, to: ClipId) {
        if id == to {
            return;
        }
        let link = match self.clip(to).link {
            Some(link) => link,
            None => {
                let link = self.next_link();
                self.clip_mut(to).link = Some(link);
                link
            }
        };
        for member in self.group_of(id) {
            self.clip_mut(member).link = Some(link);
        }
    }

    /// The clip is chained to nothing any more; the others of its group stay together.
    pub fn unchain(&mut self, id: ClipId) {
        self.clip_mut(id).link = None;
        self.tidy_links();
    }

    /// Makes the sound of a clip on a video track a clip of its own, on an audio track right under
    /// it: the one there if it is free at that time, else a new one. The two are chained. The sound's
    /// part of the fades goes with the sound; the colour stays with the picture; both keep the speed
    /// and the frames held, so that they stay together. Returns where the picture and the sound are
    /// now, or None if the clip has no sound of its own to detach, like a still.
    pub fn detach_sound(&mut self, id: ClipId) -> Option<(ClipId, ClipId)> {
        if self.tracks[id.track].kind != TrackKind::Video || self.clip(id).sound_detached || self.clip(id).still.is_some() {
            return None;
        }
        let link = self.clip(id).link.unwrap_or_else(|| self.next_link());
        let picture = self.clip_mut(id);
        picture.link = Some(link);
        picture.sound_detached = true;
        let mut sound = picture.clone();
        sound.sound_detached = false;
        sound.transform = Transform::default();
        sound.effects = picture
            .effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Fade(fade) if fade.sound => Some(Effect::Fade(Fade { picture: false, ..*fade })),
                Effect::Speed(speed) => Some(Effect::Speed(*speed)),
                // Silent while the picture holds a frame.
                Effect::Freeze(freeze) => Some(Effect::Freeze(*freeze)),
                _ => None,
            })
            .collect();
        picture.effects.retain(|effect| !matches!(effect, Effect::Fade(fade) if !fade.picture));
        for effect in &mut picture.effects {
            if let Effect::Fade(fade) = effect {
                fade.sound = false;
            }
        }
        let span = sound.span();
        let below = id.track.checked_sub(1).filter(|&track| self.tracks[track].kind == TrackKind::Audio && self.free_on(track, span.start, span.len()));
        let (track, picture) = match below {
            Some(track) => (track, id),
            None => (self.add_track(id.track, TrackKind::Audio), ClipId { track: id.track + 1, ..id }),
        };
        let index = self.tracks[track].insert(sound);
        Some((picture, ClipId { track, index }))
    }

    /// Moves a track to `index`; the others keep their order.
    pub fn move_track_to(&mut self, track: usize, index: usize) {
        if track < self.tracks.len() {
            let moved = self.tracks.remove(track);
            self.tracks.insert(index.min(self.tracks.len()), moved);
        }
    }

    /// Swaps a track with the one above it (`up`) or under it. Returns where it is now.
    pub fn move_track(&mut self, track: usize, up: bool) -> usize {
        let other = if up { track + 1 } else { track.wrapping_sub(1) };
        if other < self.tracks.len() {
            self.tracks.swap(track, other);
            other
        } else {
            track
        }
    }

    /// Moves a clip to another track of the same kind, its start as near `start` as the free time
    /// there allows: the free time where the clip's middle would be. The clips chained to it follow
    /// it in time, on their own tracks. None if the clip does not fit there, or they can't follow.
    pub fn move_clip_to_track(&mut self, id: ClipId, track: usize, start: f64) -> Option<ClipId> {
        if track == id.track || track >= self.tracks.len() || self.tracks[track].kind != self.tracks[id.track].kind {
            return None;
        }
        let len = self.clip(id).span().len();
        let gap = self.tracks[track].gap_at(start + len / 2.0)?;
        if gap.len() + SAME_TIME < len {
            return None;
        }
        let start = clamp(start, gap.start, gap.end - len);
        let delta = start - self.clip(id).start;
        let others: Vec<ClipId> = self.group_of(id).into_iter().filter(|&member| member != id).collect();
        if (self.room_for(&others, delta) - delta).abs() > SAME_TIME {
            return None;
        }
        self.shift(&others, delta);
        let mut clip = self.tracks[id.track].clips.remove(id.index);
        clip.start = start;
        let index = self.tracks[track].insert(clip);
        Some(ClipId { track, index })
    }

    /// From the start of the first clip to the end of the last one.
    pub fn content(&self) -> Option<Span> {
        self.clips().map(|(_, c)| c.span()).reduce(Span::union)
    }

    /// Where there is anything to see: the export range and all the sources, trimmed parts included,
    /// so that every trim can be undone by dragging.
    pub fn reach(&self) -> Span {
        self.clips().map(|(_, c)| c.source_span()).fold(self.export, Span::union)
    }

    /// The parts of the export range where no clip is seen: black in the result.
    pub fn gaps(&self) -> Vec<Span> {
        self.picture(self.export).into_iter().filter(|(_, shown)| shown.is_none()).map(|(span, _)| span).collect()
    }

    /// Whether the export range is exactly what the clips cover.
    pub fn export_fits(&self) -> bool {
        let same = |a: f64, b: f64| (a - b).abs() < SAME_TIME;
        self.content().is_some_and(|c| same(c.start, self.export.start) && same(c.end, self.export.end))
    }

    /// Makes the export range exactly what the clips cover.
    pub fn fit_export(&mut self) {
        if let Some(content) = self.content() {
            self.export = content;
        }
    }

    /// Trims a clip's start. An export range that fitted the clips goes on fitting them, otherwise
    /// the trim would leave black in the result that nobody asked for.
    pub fn trim_start(&mut self, id: ClipId, t: f64, min_len: f64) {
        let fitted = self.export_fits();
        self.tracks[id.track].trim_start(id.index, t, min_len);
        if fitted {
            self.fit_export();
        }
    }

    /// Trims a clip's end, see `trim_start`.
    pub fn trim_end(&mut self, id: ClipId, t: f64, min_len: f64) {
        let fitted = self.export_fits();
        self.tracks[id.track].trim_end(id.index, t, min_len);
        if fitted {
            self.fit_export();
        }
    }

    /// Cuts a clip in two at `t`. It keeps what is before the cut; a new clip right after it plays
    /// the rest, with the same framing and sound. A frame held through the cut is held in both, each
    /// for its part of the time. From then on they are two clips like any others. None if `t` is
    /// closer than `min_len` to an edge. Returns the part after the cut.
    pub fn split(&mut self, id: ClipId, t: f64, min_len: f64) -> Option<ClipId> {
        let clip = self.clip(id);
        if t - clip.start < min_len - SAME_TIME || clip.end() - t < min_len - SAME_TIME {
            return None;
        }
        let cut = clip.source_time(t);
        // The fade in stays with the part before the cut, the fade out goes with the part after it.
        // Both keep the colour.
        let mut after = clip.clone();
        after.cut_holds_before(t);
        (after.start, after.source_in) = (t, cut);
        after.settle_still();
        after.effects.retain(|effect| !effect.is_fade_at(Edge::In));
        after.fit_fades();
        let before = self.clip_mut(id);
        before.cut_holds_after(t);
        before.source_out = cut;
        before.effects.retain(|effect| !effect.is_fade_at(Edge::Out));
        before.fit_fades();
        self.tracks[id.track].clips.insert(id.index + 1, after);
        Some(ClipId { track: id.track, index: id.index + 1 })
    }

    /// Cuts every clip that is under `t`, on every track. Returns the parts after the cuts.
    pub fn split_all(&mut self, t: f64, min_len: f64) -> Vec<ClipId> {
        let under: Vec<ClipId> = self.clips().filter(|(_, clip)| clip.span().contains(t)).map(|(id, _)| id).collect();
        // One clip per track at most is under `t`, so the cuts do not move each other's ids.
        under.into_iter().filter_map(|id| self.split(id, t, min_len)).collect()
    }

    /// Puts a clip's start at `t`, as far as the clips around it let it go; the clips chained to it
    /// go along, as far as theirs let them. The export range stays: this is how a clip gets black
    /// around it.
    pub fn move_clip(&mut self, id: ClipId, t: f64) {
        let group = self.group_of(id);
        let delta = self.room_for(&group, t - self.clip(id).start);
        self.shift(&group, delta);
    }
}

/// Like `f64::clamp`, but never panics: if the bounds cross, `hi` wins.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: f64 = 1.0;

    fn span(start: f64, end: f64) -> Span {
        Span { start, end }
    }

    fn track_of(clips: Vec<Clip>) -> Track {
        Track { clips, ..Track::default() }
    }

    /// A 30 s source trimmed to 2..12 s, placed at 10 s on the timeline.
    fn placed_clip() -> Clip {
        Clip { start: 10.0, source_in: 2.0, source_out: 12.0, ..Clip::new("a".into(), 30.0) }
    }

    #[test]
    fn resize_keeps_the_other_edge() {
        let mut s = span(20.0, 50.0);
        s.resize_start(60.0, MIN);
        assert_eq!(s, span(49.0, 50.0));
        // Time has no limits on either side.
        s.resize_start(-5.0, MIN);
        assert_eq!(s, span(-5.0, 50.0));
        s.resize_end(500.0, MIN);
        assert_eq!(s, span(-5.0, 500.0));
        s.resize_end(-30.0, MIN);
        assert_eq!(s, span(-5.0, -4.0));
    }

    #[test]
    fn set_moves_the_other_edge_only_when_needed() {
        let mut s = span(20.0, 50.0);
        s.set_start(30.0, MIN);
        assert_eq!(s, span(30.0, 50.0));
        s.set_start(70.0, MIN);
        assert_eq!(s, span(70.0, 71.0));
        s.set_end(40.0, MIN);
        assert_eq!(s, span(39.0, 40.0));
        s.set_end(500.0, MIN);
        assert_eq!(s, span(39.0, 500.0));
        s.set_start(-10.0, MIN);
        assert_eq!(s, span(-10.0, 500.0));
    }

    #[test]
    fn move_keeps_the_length() {
        let mut s = span(20.0, 50.0);
        s.move_to(180.0);
        assert_eq!(s, span(180.0, 210.0));
        s.move_to(-3.0);
        assert_eq!(s, span(-3.0, 27.0));
    }

    #[test]
    fn maps_timeline_to_source_time() {
        let clip = placed_clip();
        assert_eq!(clip.span(), span(10.0, 20.0));
        assert_eq!(clip.source_span(), span(8.0, 38.0));
        assert_eq!(clip.source_time(10.0), 2.0);
        assert_eq!(clip.source_time(19.5), 11.5);
    }

    #[test]
    fn trimming_keeps_the_picture_in_place() {
        let mut track = track_of(vec![placed_clip()]);
        track.trim_start(0, 15.0, 0.5);
        let clip = &track.clips[0];
        assert_eq!((clip.start, clip.source_in, clip.end()), (15.0, 7.0, 20.0));
        assert_eq!(clip.source_time(16.0), 8.0);

        // Back out as far as the source goes, and no further.
        track.trim_start(0, 0.0, 0.5);
        assert_eq!((track.clips[0].start, track.clips[0].source_in), (8.0, 0.0));
        track.trim_end(0, 100.0, 0.5);
        assert_eq!(track.clips[0].span(), span(8.0, 38.0));

        // Edges keep the minimum length between them.
        track.trim_end(0, 0.0, 0.5);
        assert_eq!(track.clips[0].span(), span(8.0, 8.5));
        track.trim_start(0, 50.0, 0.5);
        assert_eq!(track.clips[0].span(), span(8.0, 8.5));
    }

    #[test]
    fn trimming_stops_at_the_source_and_at_neighbours() {
        // The source starts 4 s before the zero of the timeline, and that is as far as it goes.
        let mut track = track_of(vec![Clip { start: 1.0, source_in: 5.0, ..Clip::new("a".into(), 30.0) }]);
        track.trim_start(0, -10.0, 0.5);
        assert_eq!((track.clips[0].start, track.clips[0].source_in), (-4.0, 0.0));

        let mut track = track_of(vec![Clip::new("a".into(), 10.0), placed_clip()]);
        track.clips[0].source_out = 5.0;
        track.trim_end(0, 30.0, 0.5);
        assert_eq!(track.clips[0].end(), 10.0);
        track.trim_start(1, 0.0, 0.5);
        assert_eq!(track.clips[1].start, 10.0);
    }

    #[test]
    fn moving_a_clip_keeps_what_it_plays() {
        let mut track = track_of(vec![placed_clip()]);
        for t in [14.0, -25.0, 500.0] {
            track.move_clip(0, t);
            let clip = &track.clips[0];
            assert_eq!((clip.start, clip.source_in, clip.source_out), (t, 2.0, 12.0));
            assert_eq!(clip.span(), span(t, t + 10.0));
            assert_eq!(clip.source_span(), span(t - 2.0, t + 28.0));
            assert_eq!(clip.source_time(t + 1.0), 3.0);
        }
    }

    #[test]
    fn a_clip_does_not_move_onto_its_neighbours() {
        let neighbours = || {
            track_of(vec![
                Clip { start: 0.0, source_out: 5.0, ..Clip::new("a".into(), 30.0) },
                Clip { start: 10.0, source_out: 5.0, ..Clip::new("b".into(), 30.0) },
                Clip { start: 30.0, source_out: 5.0, ..Clip::new("c".into(), 30.0) },
            ])
        };
        let mut track = neighbours();
        track.move_clip(1, 2.0);
        assert_eq!(track.clips[1].start, 5.0);
        track.move_clip(1, 28.0);
        assert_eq!(track.clips[1].start, 25.0);
        track.move_clip(1, 12.0);
        assert_eq!(track.clips[1].start, 12.0);

        // The first and the last one are free on their outer side.
        track.move_clip(0, -100.0);
        assert_eq!(track.clips[0].start, -100.0);
        track.move_clip(2, 900.0);
        assert_eq!(track.clips[2].start, 900.0);
    }

    #[test]
    fn a_trim_takes_a_fitting_export_range_along() {
        let id = ClipId { track: 0, index: 0 };
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        assert!(sequence.export_fits());
        sequence.trim_start(id, 3.0, 0.5);
        sequence.trim_end(id, 8.0, 0.5);
        assert_eq!(sequence.export, span(3.0, 8.0));
        assert!(sequence.export_fits());

        // Set by hand, it stays. Back on the edges of the clip, it follows again.
        sequence.export = span(1.0, 8.0);
        assert!(!sequence.export_fits());
        sequence.trim_end(id, 9.0, 0.5);
        assert_eq!(sequence.export, span(1.0, 8.0));
        sequence.fit_export();
        assert_eq!(sequence.export, span(3.0, 9.0));
        sequence.trim_start(id, 4.0, 0.5);
        assert_eq!(sequence.export, span(4.0, 9.0));
    }

    #[test]
    fn a_move_leaves_the_export_range_where_it_is() {
        let id = ClipId { track: 0, index: 0 };
        let mut sequence = Sequence::new(Clip::new("a".into(), 12.0));
        sequence.move_clip(id, 2.0);
        assert_eq!(sequence.export, span(0.0, 12.0));
        assert_eq!(sequence.clip(id).span(), span(2.0, 14.0));
        assert!(!sequence.export_fits());
        // And the trim after it does not fit it either: there is black now, and it was asked for.
        sequence.trim_end(id, 10.0, 0.5);
        assert_eq!(sequence.export, span(0.0, 12.0));
    }

    #[test]
    fn the_export_range_also_fits_clips_of_several_tracks() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks.push(track_of(vec![Clip { start: 5.0, source_out: 10.0, ..Clip::new("b".into(), 30.0) }]));
        assert!(!sequence.export_fits());
        sequence.fit_export();
        assert_eq!(sequence.export, span(0.0, 15.0));

        // A trim that does not change the outer edges leaves them where they are.
        sequence.trim_end(ClipId { track: 0, index: 0 }, 6.0, 0.5);
        assert_eq!(sequence.export, span(0.0, 15.0));
        sequence.trim_end(ClipId { track: 1, index: 0 }, 12.0, 0.5);
        assert_eq!(sequence.export, span(0.0, 12.0));
    }

    #[test]
    fn a_saved_time_may_differ_in_the_last_digit() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        sequence.export.end += 1e-9;
        assert!(sequence.export_fits());
        sequence.export.end += 0.01;
        assert!(!sequence.export_fits());
    }

    #[test]
    fn nothing_to_fit_without_clips() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        sequence.tracks.clear();
        assert!(!sequence.export_fits());
        sequence.fit_export();
        assert_eq!(sequence.export, span(0.0, 30.0));
    }

    #[test]
    fn finds_the_black_in_the_export_range() {
        let id = ClipId { track: 0, index: 0 };
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        assert!(sequence.gaps().is_empty());

        sequence.export = span(-2.0, 13.0);
        assert_eq!(sequence.gaps(), [span(-2.0, 0.0), span(10.0, 13.0)]);

        // Out of the range altogether: all of it is black.
        sequence.move_clip(id, 20.0);
        assert_eq!(sequence.gaps(), [span(-2.0, 13.0)]);
        sequence.move_clip(id, -50.0);
        assert_eq!(sequence.gaps(), [span(-2.0, 13.0)]);

        // Clips of two tracks cover the range together, whichever is on top.
        sequence.move_clip(id, -2.0);
        sequence.tracks.push(track_of(vec![Clip { start: 6.0, source_out: 4.0, ..Clip::new("b".into(), 30.0) }]));
        assert_eq!(sequence.gaps(), [span(10.0, 13.0)]);
        sequence.move_clip(ClipId { track: 1, index: 0 }, 9.0);
        assert_eq!(sequence.gaps(), [span(8.0, 9.0)]);
    }

    #[test]
    fn a_replaced_source_keeps_what_it_can_of_the_trim() {
        let mut longer = placed_clip();
        longer.refit_source(60.0);
        assert_eq!((longer.source_in, longer.source_out, longer.source_duration), (2.0, 12.0, 60.0));

        let mut shorter = placed_clip();
        shorter.refit_source(5.0);
        assert_eq!((shorter.source_in, shorter.source_out), (2.0, 5.0));

        // The whole trim is past the end of the new file.
        let mut gone = placed_clip();
        gone.refit_source(1.0);
        assert_eq!((gone.source_in, gone.source_out), (0.0, 1.0));
    }

    #[test]
    fn finds_the_topmost_clip_and_the_extent() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        assert_eq!(sequence.export, span(0.0, 30.0));
        sequence.tracks.push(track_of(vec![placed_clip()]));
        let top = ClipId { track: 1, index: 0 };
        assert_eq!(sequence.shown_at(5.0), Some(ClipId { track: 0, index: 0 }));
        assert_eq!(sequence.shown_at(15.0), Some(top));
        assert_eq!(sequence.shown_at(30.0), None);
        assert_eq!(sequence.content(), Some(span(0.0, 30.0)));
        // The second clip's source reaches 38 s even though the clip ends at 20 s.
        assert_eq!(sequence.reach(), span(0.0, 38.0));

        sequence.trim_end(ClipId { track: 0, index: 0 }, 12.0, 0.5);
        assert_eq!(sequence.content(), Some(span(0.0, 20.0)));
        assert_eq!(sequence.shown_at(25.0), None);

        // The reach goes wherever the clips and the export range go.
        sequence.move_clip(top, -40.0);
        assert_eq!(sequence.reach().start, -42.0);
        sequence.export = span(-50.0, 100.0);
        assert_eq!(sequence.reach(), span(-50.0, 100.0));
    }

    #[test]
    fn a_new_source_goes_under_everything_after_the_last_clip() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        let first = ClipId { track: 0, index: 0 };
        sequence.move_clip(first, 2.0);
        sequence.fit_export();

        let id = sequence.add_below(Clip::of(1, "b".into(), 5.0), TrackKind::Video);
        assert_eq!(id, ClipId { track: 0, index: 0 });
        assert_eq!(sequence.tracks.len(), 2);
        assert_eq!((sequence.clip(id).source, sequence.clip(id).span()), (1, span(12.0, 17.0)));
        assert_eq!(sequence.clip(ClipId { track: 1, index: 0 }).name, "a");
        assert_ne!(sequence.tracks[0].id, sequence.tracks[1].id);
        // The range fitted the clips, so it takes the new one in.
        assert_eq!(sequence.export, span(2.0, 17.0));

        // Set by hand, it stays.
        sequence.export = span(0.0, 5.0);
        sequence.add_below(Clip::of(2, "c".into(), 3.0), TrackKind::Video);
        assert_eq!(sequence.export, span(0.0, 5.0));
        assert_eq!(sequence.clip(ClipId { track: 0, index: 0 }).span(), span(17.0, 20.0));
        let ids: BTreeSet<_> = sequence.tracks.iter().map(|track| track.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn a_dropped_clip_goes_where_there_is_room() {
        // 0..10 s and 20..30 s on the track.
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks[0].clips.push(Clip { start: 20.0, ..Clip::new("b".into(), 10.0) });
        sequence.export = span(0.0, 5.0);
        let dropped = |start: f64, len: f64| Clip { start, ..Clip::of(1, "c".into(), len) };

        // Its middle over the gap: it goes in, as near as it can be.
        let id = sequence.place_clip(Some(0), dropped(13.0, 6.0)).unwrap();
        assert_eq!((id, sequence.clip(id).span()), (ClipId { track: 0, index: 1 }, span(13.0, 19.0)));
        let id = sequence.place_clip(Some(0), dropped(9.0, 3.0));
        assert_eq!(id.map(|id| sequence.clip(id).span()), Some(span(10.0, 13.0)));
        // No room, or no such track: nothing changes.
        assert_eq!(sequence.place_clip(Some(0), dropped(19.5, 2.0)), None);
        assert_eq!(sequence.place_clip(Some(3), dropped(50.0, 2.0)), None);
        assert_eq!(sequence.tracks.len(), 1);
        // Without a track: a new one under the others, where it was dropped. The range set by hand stays.
        let id = sequence.place_clip(None, dropped(2.0, 4.0)).unwrap();
        assert_eq!((id, sequence.clip(id).span()), (ClipId { track: 0, index: 0 }, span(2.0, 6.0)));
        assert_eq!(sequence.tracks.len(), 2);
        assert_eq!(sequence.export, span(0.0, 5.0));
    }

    #[test]
    fn the_first_clip_starts_the_file() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks.clear();
        sequence.export = span(3.0, 8.0);
        let id = sequence.add_below(Clip::of(4, "b".into(), 6.0), TrackKind::Video);
        assert_eq!(sequence.clip(id).span(), span(3.0, 9.0));
        assert_eq!(sequence.export, span(3.0, 9.0));
    }

    #[test]
    fn removing_a_clip_leaves_its_track() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.add_below(Clip::of(1, "b".into(), 5.0), TrackKind::Video);
        sequence.remove_clip(ClipId { track: 0, index: 0 });
        assert_eq!(sequence.tracks.len(), 2);
        assert!(sequence.tracks[0].clips.is_empty());
        // The range fitted both clips, it fits what is left.
        assert_eq!(sequence.export, span(0.0, 10.0));
        assert!(!sequence.has(ClipId { track: 0, index: 0 }));
        assert!(sequence.has(ClipId { track: 1, index: 0 }));

        sequence.remove_track(0);
        assert_eq!(sequence.tracks.len(), 1);
        sequence.remove_track(0);
        assert!(sequence.tracks.is_empty());
        assert_eq!(sequence.export, span(0.0, 10.0));
    }

    #[test]
    fn tracks_trade_places() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.add_below(Clip::of(1, "b".into(), 5.0), TrackKind::Video);
        let (bottom, top) = (sequence.tracks[0].id, sequence.tracks[1].id);
        assert_eq!(sequence.move_track(0, true), 1);
        assert_eq!((sequence.tracks[0].id, sequence.tracks[1].id), (top, bottom));
        // There is nothing above the top one, nor under the bottom one.
        assert_eq!(sequence.move_track(1, true), 1);
        assert_eq!(sequence.move_track(0, false), 0);
        assert_eq!((sequence.tracks[0].id, sequence.tracks[1].id), (top, bottom));
    }

    #[test]
    fn a_cut_clip_is_two_clips_that_play_the_same() {
        // Source 2..12 s at 10..20 s, framed and turned, quieter.
        let mut clip = placed_clip();
        clip.transform = Transform { cx: 0.3, angle_deg: 20.0, ..Transform::default() };
        clip.volume = 0.5;
        let mut sequence = Sequence::new(clip.clone());
        sequence.tracks[0].clips.push(Clip { start: 30.0, ..Clip::new("b".into(), 5.0) });
        let export = sequence.export;
        let first = ClipId { track: 0, index: 0 };

        let second = sequence.split(first, 14.0, 0.5).unwrap();
        assert_eq!(second, ClipId { track: 0, index: 1 });
        let (a, b) = (sequence.clip(first).clone(), sequence.clip(second).clone());
        assert_eq!((a.span(), a.source_in, a.source_out), (span(10.0, 14.0), 2.0, 6.0));
        assert_eq!((b.span(), b.source_in, b.source_out), (span(14.0, 20.0), 6.0, 12.0));
        // The same picture at every moment, the same framing and sound.
        for t in [10.0, 13.9, 14.0, 19.0] {
            let part = if t < 14.0 { &a } else { &b };
            assert_eq!(part.source_time(t), clip.source_time(t));
        }
        assert_eq!((b.transform, b.volume, b.source, b.name.as_str()), (clip.transform, 0.5, 0, "a"));
        assert_eq!(sequence.export, export);
        assert_eq!(sequence.clip(ClipId { track: 0, index: 2 }).name, "b");

        // From now on each is its own: turning one leaves the other.
        sequence.clip_mut(second).transform.angle_deg = -45.0;
        assert_eq!(sequence.clip(first).transform.angle_deg, 20.0);
        // Not closer than the minimum to an edge.
        assert_eq!(sequence.split(first, 10.2, 0.5), None);
        assert_eq!(sequence.split(first, 13.7, 0.5), None);
        assert_eq!(sequence.tracks[0].clips.len(), 3);
    }

    #[test]
    fn fades_rise_and_fall_straight_and_fit_the_clip() {
        // A clip 10..20 s with a fade in of 2 s, picture and sound, and a fade out of 4 s, picture only.
        let mut clip = placed_clip();
        assert_eq!((clip.opacity(10.0), clip.sound_fade(19.9)), (1.0, 1.0));
        assert!(clip.add_fade(Edge::In));
        assert!(!clip.add_fade(Edge::In), "one fade in only");
        clip.set_fade_length(Edge::In, 2.0);
        clip.add_fade(Edge::Out);
        clip.set_fade_length(Edge::Out, 4.0);
        if let Some(fade) = clip.fade_mut(Edge::Out) {
            fade.sound = false;
        }
        assert_eq!(clip.fade_lengths(), (2.0, 4.0));
        assert_eq!((clip.opacity(10.0), clip.opacity(11.0), clip.opacity(12.0), clip.opacity(15.0)), (0.0, 0.5, 1.0, 1.0));
        assert_eq!((clip.opacity(18.0), clip.opacity(20.0)), (0.5, 0.0));
        assert_eq!((clip.sound_fade(11.0), clip.sound_fade(18.0)), (0.5, 1.0));
        // Not longer than what the other fade leaves.
        clip.set_fade_length(Edge::In, 9.0);
        assert_eq!(clip.fade_lengths(), (6.0, 4.0));

        // Trimmed shorter, the fades shrink to fit: the fade in keeps what it can.
        let mut track = track_of(vec![clip]);
        track.trim_end(0, 15.0, 0.5);
        assert_eq!(track.clips[0].fade_lengths(), (5.0, 0.0));
        assert_eq!(track.clips[0].fade(Edge::Out).map(|fade| fade.seconds), Some(0.0));
    }

    #[test]
    fn fades_go_by_their_curves() {
        for curve in Curve::ALL {
            // From nothing to all, without going back or beyond.
            assert_eq!((curve.ease(0.0), curve.ease(1.0)), (0.0, 1.0), "{curve:?}");
            assert_eq!((curve.ease(-1.0), curve.ease(2.0)), (0.0, 1.0), "{curve:?}");
            let steps: Vec<f64> = (0..=100).map(|i| curve.ease(i as f64 / 100.0)).collect();
            assert!(steps.windows(2).all(|pair| pair[1] >= pair[0]), "{curve:?}");
            // A fade out is the fade in turned upside down.
            assert_eq!(curve.level(Edge::Out, 0.3), 1.0 - curve.level(Edge::In, 0.3));
        }
        let at = |curve: Curve, x| (curve.ease(x) * 1000.0).round() / 1000.0;
        // Ease in starts slowly, ease out ends slowly; strong ones more so.
        assert_eq!((at(Curve::Linear, 0.25), at(Curve::EaseIn, 0.25), at(Curve::StrongEaseIn, 0.25)), (0.25, 0.063, 0.016));
        assert_eq!((at(Curve::EaseOut, 0.75), at(Curve::StrongEaseOut, 0.75)), (0.938, 0.984));
        assert_eq!((at(Curve::EaseInOut, 0.25), at(Curve::EaseInOut, 0.5), at(Curve::StrongEaseInOut, 0.25)), (0.146, 0.5, 0.063));
        assert_eq!(at(Curve::StrongEaseInOut, 0.5), 0.5);

        // A clip 10..20 s fading in over 2 s with an ease in, out over 4 s with an ease out.
        let mut clip = placed_clip();
        clip.add_fade(Edge::In);
        clip.set_fade_length(Edge::In, 2.0);
        clip.add_fade(Edge::Out);
        clip.set_fade_length(Edge::Out, 4.0);
        assert_eq!(clip.fade_curve(Edge::Out), Curve::Linear, "a new fade is linear");
        clip.set_fade_curve(Edge::In, Curve::EaseIn);
        clip.set_fade_curve(Edge::Out, Curve::EaseOut);
        assert_eq!((clip.opacity(10.0), clip.opacity(11.0), clip.opacity(12.0)), (0.0, 0.25, 1.0));
        assert_eq!((clip.opacity(16.0), clip.opacity(18.0), clip.opacity(20.0)), (1.0, 0.25, 0.0));
        assert_eq!(clip.sound_fade(11.0), 0.25, "the sound goes by the same curve");
    }

    #[test]
    fn a_clip_has_one_colour_that_both_parts_of_a_cut_keep() {
        let mut clip = placed_clip();
        assert_eq!((clip.color(), clip.graded()), (None, None));
        assert!(clip.add_color());
        assert!(!clip.add_color(), "one colour only");
        assert_eq!(clip.graded(), None, "a new one changes nothing");
        clip.add_fade(Edge::In);
        clip.add_fade(Edge::Out);
        if let Some(Effect::Color(color)) = clip.effects.iter_mut().find(|effect| matches!(effect, Effect::Color(_))) {
            color.exposure = 0.5;
        }
        assert_eq!(clip.graded().map(|color| color.exposure), Some(0.5));
        assert!(clip.fade(Edge::In).is_some() && clip.fade(Edge::Out).is_some(), "fades are found among other effects");

        let mut sequence = Sequence::new(clip);
        let first = ClipId { track: 0, index: 0 };
        let second = sequence.split(first, 14.0, 0.5).unwrap();
        for id in [first, second] {
            assert_eq!(sequence.clip(id).graded().map(|color| color.exposure), Some(0.5));
        }
        assert!(sequence.clip(first).fade(Edge::Out).is_none() && sequence.clip(second).fade(Edge::In).is_none());
        sequence.clip_mut(second).remove_color();
        assert_eq!(sequence.clip(second).color(), None);
        assert!(sequence.clip(second).fade(Edge::Out).is_some(), "only the colour went");

        // Saved and read back; a colour saved with fewer values gets the rest as they leave it.
        let color: Color = serde_json::from_str(r#"{"exposure": 1.0}"#).unwrap();
        assert_eq!(color, Color { exposure: 1.0, ..Color::default() });
        let effect: Effect = serde_json::from_str(&serde_json::to_string(&Effect::Color(color)).unwrap()).unwrap();
        assert_eq!(effect, Effect::Color(color));
        // Warmer: more red, less blue.
        let [r, g, b] = Color { temperature: 100.0, ..Color::default() }.gains();
        assert!(r > 1.0 && g == 1.0 && b < 1.0);
    }

    #[test]
    fn a_copy_goes_after_its_clip_or_onto_a_new_track_above() {
        // Track 0: a at 0..10 and b at 10..14; track 1: c at 30..34.
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks[0].clips.push(Clip { start: 10.0, ..Clip::of(1, "b".into(), 4.0) });
        sequence.tracks.push(track_of(vec![Clip { start: 30.0, ..Clip::of(2, "c".into(), 4.0) }]));
        let (a, b, c) = (ClipId { track: 0, index: 0 }, ClipId { track: 0, index: 1 }, ClipId { track: 1, index: 0 });
        sequence.clip_mut(c).add_fade(Edge::In);

        // Room after c: the copy is right there, with its effects.
        let copy = sequence.duplicate(c);
        assert_eq!(copy, ClipId { track: 1, index: 1 });
        assert_eq!(sequence.clip(copy).span(), span(34.0, 38.0));
        assert!(sequence.clip(copy).fade(Edge::In).is_some());
        // b follows a at once: a's copy goes onto a new track between tracks 0 and 1.
        let copy = sequence.duplicate(a);
        assert_eq!((copy, sequence.tracks.len()), (ClipId { track: 1, index: 0 }, 3));
        assert_eq!(sequence.clip(copy).span(), span(10.0, 20.0));
        assert_eq!(sequence.clip(ClipId { track: 2, index: 0 }).name, "c", "the track above moved up");

        // Pasted at 50 s on track 0: free there. At 5 s on track 0: a is there, so above it.
        let pasted = sequence.paste(sequence.clip(b).clone(), 50.0, Some(0), TrackKind::Video);
        assert_eq!((pasted, sequence.clip(pasted).span()), (ClipId { track: 0, index: 2 }, span(50.0, 54.0)));
        let pasted = sequence.paste(sequence.clip(b).clone(), 5.0, Some(0), TrackKind::Video);
        assert_eq!(pasted.track, 1);
        assert_eq!(sequence.clip(pasted).span(), span(5.0, 9.0), "exactly where it was asked to go");
    }

    #[test]
    fn a_ripple_delete_closes_the_gap_on_its_track_only() {
        // Track 0: a 0..10, b 10..14, c 20..24; track 1: d 12..16.
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks[0].clips.push(Clip { start: 10.0, ..Clip::of(1, "b".into(), 4.0) });
        sequence.tracks[0].clips.push(Clip { start: 20.0, ..Clip::of(2, "c".into(), 4.0) });
        sequence.tracks.push(track_of(vec![Clip { start: 12.0, ..Clip::of(3, "d".into(), 4.0) }]));
        sequence.fit_export();
        sequence.ripple_delete(ClipId { track: 0, index: 1 });
        let starts: Vec<f64> = sequence.tracks[0].clips.iter().map(|clip| clip.start).collect();
        assert_eq!(starts, [0.0, 16.0], "c moved back by b's 4 s");
        assert_eq!(sequence.tracks[1].clips[0].start, 12.0);
        assert_eq!(sequence.export, span(0.0, 20.0), "a fitted export range follows");
    }

    #[test]
    fn a_fade_saved_before_curves_is_linear() {
        let fade: Fade = serde_json::from_str(r#"{"edge": "Out", "seconds": 2.0, "picture": true, "sound": false}"#).unwrap();
        assert_eq!(fade.curve, Curve::Linear);
        let mut saved = Fade { curve: Curve::StrongEaseInOut, ..fade };
        saved = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(saved.curve, Curve::StrongEaseInOut);
    }

    #[test]
    fn a_cut_keeps_the_fade_in_before_it_and_the_fade_out_after_it() {
        let mut clip = placed_clip();
        clip.add_fade(Edge::In);
        clip.add_fade(Edge::Out);
        let mut sequence = Sequence::new(clip);
        let first = ClipId { track: 0, index: 0 };
        let second = sequence.split(first, 14.0, 0.5).unwrap();
        let edges = |id| sequence.clip(id).effects.iter().filter_map(Effect::as_fade).map(|fade| fade.edge).collect::<Vec<_>>();
        assert_eq!((edges(first), edges(second)), (vec![Edge::In], vec![Edge::Out]));
        assert_eq!(sequence.clip(second).opacity(20.0), 0.0);
        assert_eq!(sequence.clip(first).opacity(14.0 - 1e-9), 1.0);
    }

    #[test]
    fn a_fading_clip_shows_what_is_under_it() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks.push(track_of(vec![Clip { start: 2.0, ..Clip::of(1, "b".into(), 4.0) }]));
        sequence.tracks.push(track_of(vec![Clip { start: 3.0, ..Clip::of(2, "c".into(), 1.0) }]));
        assert_eq!(sequence.shown_under(3.5, 2), Some(ClipId { track: 1, index: 0 }));
        assert_eq!(sequence.shown_under(3.5, 1), Some(ClipId { track: 0, index: 0 }));
        assert_eq!(sequence.shown_under(3.5, 0), None);
        sequence.tracks[1].hidden = true;
        assert_eq!(sequence.shown_under(3.5, 2), Some(ClipId { track: 0, index: 0 }));
    }

    #[test]
    fn a_cut_through_all_tracks_cuts_what_is_under_it() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks.push(track_of(vec![Clip { start: 4.0, ..Clip::of(1, "b".into(), 4.0) }]));
        sequence.tracks.push(track_of(vec![Clip { start: 20.0, ..Clip::of(2, "c".into(), 4.0) }]));
        let parts = sequence.split_all(6.0, 0.1);
        assert_eq!(parts, [ClipId { track: 0, index: 1 }, ClipId { track: 1, index: 1 }]);
        assert_eq!(sequence.tracks.iter().map(|track| track.clips.len()).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(sequence.clip(parts[1]).span(), span(6.0, 8.0));
    }

    #[test]
    fn empty_tracks_go_anywhere_and_tracks_move_to_any_place() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        assert_eq!(sequence.add_track(0, TrackKind::Video), 0);
        assert_eq!(sequence.add_track(99, TrackKind::Video), 2);
        let ids: Vec<TrackId> = sequence.tracks.iter().map(|track| track.id).collect();
        assert_eq!(ids, [1, 0, 2]);
        assert!(sequence.tracks[0].clips.is_empty() && sequence.tracks[2].clips.is_empty());
        // Empty tracks change nothing of the result.
        assert_eq!(sequence.export, span(0.0, 10.0));
        assert!(sequence.export_fits());

        sequence.move_track_to(2, 0);
        assert_eq!(sequence.tracks.iter().map(|track| track.id).collect::<Vec<_>>(), [2, 1, 0]);
        sequence.move_track_to(0, 2);
        assert_eq!(sequence.tracks.iter().map(|track| track.id).collect::<Vec<_>>(), [1, 0, 2]);
        sequence.move_track_to(1, 1);
        assert_eq!(sequence.tracks.iter().map(|track| track.id).collect::<Vec<_>>(), [1, 0, 2]);
    }

    #[test]
    fn a_clip_moves_to_another_track_only_where_it_fits() {
        // Below: 0..10 and 20..30. Above: a 6 s clip.
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks[0].clips.push(Clip { start: 20.0, ..Clip::new("b".into(), 10.0) });
        sequence.tracks.push(track_of(vec![Clip { start: 40.0, ..Clip::of(1, "c".into(), 6.0) }]));
        let above = ClipId { track: 1, index: 0 };

        // Its middle over the gap: it goes in, as near as it can be.
        let id = sequence.move_clip_to_track(above, 0, 9.0).unwrap();
        assert_eq!(id, ClipId { track: 0, index: 1 });
        assert_eq!(sequence.clip(id).span(), span(10.0, 16.0));
        assert_eq!(sequence.clip(id).name, "c");
        assert!(sequence.tracks[1].clips.is_empty());
        assert_eq!(sequence.tracks[0].clips.iter().map(|c| c.start).collect::<Vec<_>>(), [0.0, 10.0, 20.0]);

        // Over a clip, or into a gap shorter than itself: it stays.
        sequence.tracks[0].clips[2].start = 14.0;
        let back = sequence.move_clip_to_track(id, 1, 40.0).unwrap();
        assert_eq!(sequence.move_clip_to_track(back, 0, 2.0), None);
        assert_eq!(sequence.move_clip_to_track(back, 0, 9.0), None);
        assert_eq!(sequence.move_clip_to_track(back, 1, 9.0), None);
        assert_eq!(sequence.clip(back).span(), span(40.0, 46.0));
        // After the last clip there is all the time there is.
        let id = sequence.move_clip_to_track(back, 0, 100.0).unwrap();
        assert_eq!((id, sequence.clip(id).start), (ClipId { track: 0, index: 2 }, 100.0));
    }

    #[test]
    fn the_topmost_visible_clip_is_seen() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks.push(track_of(vec![Clip { start: 4.0, ..Clip::of(1, "b".into(), 2.0) }]));
        let (below, above) = (ClipId { track: 0, index: 0 }, ClipId { track: 1, index: 0 });
        sequence.export = span(-1.0, 12.0);
        assert_eq!(sequence.shown_at(5.0), Some(above));
        assert_eq!(
            sequence.picture(sequence.export),
            [(span(-1.0, 0.0), None), (span(0.0, 4.0), Some(below)), (span(4.0, 6.0), Some(above)), (span(6.0, 10.0), Some(below)), (span(10.0, 12.0), None)]
        );

        // A hidden track shows what is under it, and where there is nothing it is black.
        sequence.tracks[1].hidden = true;
        assert_eq!(sequence.shown_at(5.0), Some(below));
        assert_eq!(sequence.picture(span(0.0, 10.0)), [(span(0.0, 10.0), Some(below))]);
        sequence.tracks[0].hidden = true;
        sequence.tracks[1].hidden = false;
        assert_eq!(sequence.gaps(), [span(-1.0, 4.0), span(6.0, 12.0)]);
    }

    #[test]
    fn the_main_source_is_the_one_seen_longest() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        assert_eq!(sequence.main_source(), Some(0));
        // Two clips of source 1 cover 7 s of the 10 s of source 0.
        sequence.tracks.push(track_of(vec![Clip { start: 1.0, ..Clip::of(1, "b".into(), 4.0) }, Clip { start: 6.0, ..Clip::of(1, "c".into(), 3.0) }]));
        assert_eq!(sequence.main_source(), Some(1));
        sequence.tracks[1].hidden = true;
        assert_eq!(sequence.main_source(), Some(0));
        sequence.export = span(20.0, 30.0);
        assert_eq!(sequence.main_source(), None);
    }

    #[test]
    fn tracks_of_an_old_file_get_ids_of_their_own() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.tracks.push(track_of(Vec::new()));
        sequence.tracks.push(track_of(Vec::new()));
        sequence.number_tracks();
        assert_eq!(sequence.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1, 2]);
        // Numbered already: nothing changes.
        sequence.tracks.swap(0, 2);
        sequence.number_tracks();
        assert_eq!(sequence.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 1, 0]);
    }

    #[test]
    fn a_source_is_refitted_wherever_it_plays() {
        let mut sequence = Sequence::new(placed_clip());
        sequence.add_below(Clip::of(1, "b".into(), 5.0), TrackKind::Video);
        sequence.tracks[0].clips.push(Clip { start: 50.0, ..placed_clip() });
        assert_eq!((sequence.uses(0), sequence.uses(1), sequence.uses(2)), (2, 1, 0));
        assert!(!sequence.refit_source(0, 30.0 + 1e-4));
        assert!(sequence.refit_source(0, 6.0));
        let outs: Vec<f64> = sequence.clips().filter(|(_, c)| c.source == 0).map(|(_, c)| c.source_out).collect();
        assert_eq!(outs, [6.0, 6.0]);
        assert_eq!(sequence.clip(ClipId { track: 0, index: 0 }).source_out, 5.0);
    }

    /// A video clip 10..20 s on track 0 with a fade in of 2 s and a fade out of 1 s, picture and
    /// sound, coloured; its sound detached onto an audio track under it.
    fn detached() -> (Sequence, ClipId, ClipId) {
        let mut clip = placed_clip();
        clip.add_fade(Edge::In);
        clip.set_fade_length(Edge::In, 2.0);
        clip.add_fade(Edge::Out);
        clip.add_color();
        let mut sequence = Sequence::new(clip);
        let (picture, sound) = sequence.detach_sound(ClipId { track: 0, index: 0 }).unwrap();
        (sequence, picture, sound)
    }

    #[test]
    fn detached_sound_is_a_clip_of_its_own_on_an_audio_track_under_its_picture() {
        let (sequence, picture, sound) = detached();
        // A new audio track went under the video one, which moved up.
        assert_eq!((picture, sound), (ClipId { track: 1, index: 0 }, ClipId { track: 0, index: 0 }));
        assert_eq!(sequence.tracks.iter().map(|track| track.kind).collect::<Vec<_>>(), [TrackKind::Audio, TrackKind::Video]);
        let (p, s) = (sequence.clip(picture), sequence.clip(sound));
        assert_eq!((p.span(), s.span()), (span(10.0, 20.0), span(10.0, 20.0)));
        assert_eq!((p.source_in, s.source_in), (2.0, 2.0));
        // The picture is seen and not heard; the sound is heard and not seen.
        assert!(p.sound_detached && !s.sound_detached);
        assert!(!sequence.sounds(picture) && sequence.sounds(sound));
        assert_eq!(sequence.shown_at(15.0), Some(picture));
        assert_eq!(sequence.picture(span(0.0, 30.0)).iter().filter_map(|(_, id)| *id).collect::<Vec<_>>(), [picture]);
        // The fades went apart: the picture's stay, the sound's went with the sound. The colour stayed.
        let fades = |clip: &Clip| clip.effects.iter().filter_map(Effect::as_fade).map(|fade| (fade.edge, fade.picture, fade.sound)).collect::<Vec<_>>();
        assert_eq!(fades(p), [(Edge::In, true, false), (Edge::Out, true, false)]);
        assert_eq!(fades(s), [(Edge::In, false, true), (Edge::Out, false, true)]);
        assert_eq!(s.fade(Edge::In).map(|fade| fade.seconds), Some(2.0));
        assert!(p.color().is_some() && s.color().is_none());
        // Chained; and there is nothing more to detach.
        assert_eq!(sequence.group_of(picture), [sound, picture]);
        let mut again = sequence.clone();
        assert_eq!(again.detach_sound(picture), None);
        assert_eq!(again.detach_sound(sound), None, "sound has no sound of its own to detach");
    }

    #[test]
    fn a_free_audio_track_right_under_takes_the_sound() {
        let (mut sequence, picture, _) = detached();
        // Another video clip at 30..40 s on the video track: its sound goes onto the same audio track.
        let other = sequence.place_clip(Some(picture.track), Clip { start: 30.0, ..Clip::new("b".into(), 10.0) }).unwrap();
        let (other, sound) = sequence.detach_sound(other).unwrap();
        assert_eq!(sequence.tracks.len(), 2);
        assert_eq!((other.track, sound), (1, ClipId { track: 0, index: 1 }));
        // Two groups, one for each.
        assert_eq!(sequence.group_of(other), [sound, other]);
    }

    #[test]
    fn chained_clips_move_together_as_far_as_all_of_them_can() {
        let (mut sequence, picture, sound) = detached();
        sequence.move_clip(picture, 14.0);
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start), (14.0, 14.0));
        // Shifted on its own, then moved by the sound: the distance stays.
        sequence.unchain(sound);
        sequence.move_clip(sound, 15.0);
        sequence.chain(sound, picture);
        sequence.move_clip(sound, 16.0);
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start), (15.0, 16.0));
        // A clip at 0..10 s on the audio track: the sound stops at 10 s, and the picture with it.
        sequence.place_clip(Some(0), Clip::new("c".into(), 10.0));
        let sound = ClipId { track: 0, index: 1 };
        sequence.move_clip(picture, 0.0);
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start), (9.0, 10.0));
        // Onto another video track: only the picture changes track, both go in time.
        let top = sequence.add_track(2, TrackKind::Video);
        let moved = sequence.move_clip_to_track(picture, top, 20.0).unwrap();
        assert_eq!((moved.track, sequence.clip(moved).start, sequence.clip(sound).start), (2, 20.0, 21.0));
        // Never onto a track of the other kind.
        assert_eq!(sequence.move_clip_to_track(moved, 0, 40.0), None);
        // Where the sound can't follow, the picture does not go.
        sequence.place_clip(Some(0), Clip { start: 31.0, ..Clip::new("d".into(), 10.0) });
        let lower = sequence.add_track(1, TrackKind::Video);
        let moved = ClipId { track: 3, index: 0 };
        assert_eq!(sequence.move_clip_to_track(moved, lower, 25.0), None);
    }

    #[test]
    fn both_parts_of_a_cut_stay_chained_and_a_copy_is_chained_to_nothing() {
        let (mut sequence, picture, sound) = detached();
        let after = sequence.split(picture, 15.0, 0.5).unwrap();
        assert_eq!(sequence.group_of(picture).len(), 3);
        sequence.move_clip(after, 17.0);
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start, sequence.clip(after).start), (12.0, 12.0, 17.0));
        let copy = sequence.duplicate(sound);
        assert_eq!(sequence.tracks[copy.track].kind, TrackKind::Audio);
        assert!(!sequence.is_chained(copy));
        // A copy of sound pasted onto a video track goes onto a new audio track above it instead.
        let pasted = sequence.paste(sequence.clip(sound).clone(), 40.0, Some(picture.track), TrackKind::Audio);
        assert_eq!(sequence.tracks[pasted.track].kind, TrackKind::Audio);
        assert_eq!(pasted.track, picture.track + 1);
    }

    #[test]
    fn closing_a_gap_takes_chained_clips_along_while_they_have_room() {
        // Video track: a 0..5, then the detached clip 10..20; audio track: its sound 10..20.
        let (mut sequence, picture, sound) = detached();
        sequence.place_clip(Some(picture.track), Clip::new("a".into(), 5.0));
        let first = ClipId { track: 1, index: 0 };
        sequence.ripple_delete(first);
        let picture = ClipId { track: 1, index: 0 };
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start), (5.0, 5.0));
        // Something in the sound's way: nothing moves.
        let mut blocked = sequence.clone();
        blocked.place_clip(Some(0), Clip { start: 2.0, ..Clip::new("e".into(), 3.0) });
        blocked.place_clip(Some(1), Clip::new("f".into(), 3.0));
        let (f, picture) = (ClipId { track: 1, index: 0 }, ClipId { track: 1, index: 1 });
        blocked.ripple_delete(f);
        let picture = ClipId { track: 1, index: picture.index - 1 };
        assert_eq!(blocked.clip(picture).start, 5.0, "the sound had no room to go back: it stays, and so does the picture");
    }

    #[test]
    fn chaining_joins_groups_and_unchaining_leaves_the_rest_together() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        for start in [20.0, 40.0] {
            sequence.place_clip(Some(0), Clip { start, ..Clip::new("b".into(), 5.0) });
        }
        let [a, b, c] = [0, 1, 2].map(|index| ClipId { track: 0, index });
        sequence.chain(a, b);
        sequence.chain(c, a);
        assert_eq!(sequence.group_of(b), [a, b, c]);
        sequence.unchain(a);
        assert!(!sequence.is_chained(a));
        assert_eq!(sequence.group_of(b), [b, c]);
        // A group left with one clip is no group.
        sequence.unchain(b);
        assert!(!sequence.is_chained(c) && sequence.clip(c).link.is_none());
        sequence.chain(a, b);
        sequence.remove_clip(a);
        assert!(sequence.clips().all(|(_, clip)| clip.link.is_none()));
    }

    #[test]
    fn audio_tracks_and_detached_sound_are_saved_and_older_files_read() {
        let (sequence, _, _) = detached();
        let text = serde_json::to_string(&sequence).unwrap();
        assert_eq!(serde_json::from_str::<Sequence>(&text).unwrap(), sequence);
        assert!(text.contains(r#""kind":"Audio""#) && !text.contains(r#""kind":"Video""#), "a video track says nothing of its kind");
        // A track and a clip from before: a video track, sound in its clip, chained to nothing.
        let old: Track = serde_json::from_str(r#"{"id": 3, "clips": [{"name": "a", "start": 0.0, "source_in": 0.0, "source_out": 1.0, "source_duration": 1.0, "transform": {"cx": 0.5, "cy": 0.5, "zoom": 1.0, "angle_deg": 0.0, "mirror": false, "bars": false}}]}"#).unwrap();
        assert_eq!(old.kind, TrackKind::Video);
        assert!(!old.clips[0].sound_detached && old.clips[0].link.is_none());
    }

    /// One video track with a 0..10, b 10..14 and c 14..20, one after another; the export range fits.
    fn three_in_a_row() -> Sequence {
        let mut sequence = Sequence::new(Clip { source_out: 10.0, ..Clip::new("a".into(), 30.0) });
        sequence.tracks[0].clips.push(Clip { start: 10.0, ..Clip::of(1, "b".into(), 4.0) });
        sequence.tracks[0].clips.push(Clip { start: 14.0, ..Clip::of(2, "c".into(), 6.0) });
        sequence.fit_export();
        sequence
    }

    fn names_and_starts(sequence: &Sequence, track: usize) -> Vec<(&str, f64)> {
        sequence.tracks[track].clips.iter().map(|clip| (clip.name.as_str(), clip.start)).collect()
    }

    #[test]
    fn an_insert_moves_the_clips_from_its_place_on_by_its_length() {
        let mut sequence = three_in_a_row();
        // Another video track: its clip stays where it is.
        sequence.tracks.push(track_of(vec![Clip { start: 12.0, ..Clip::of(3, "d".into(), 4.0) }]));
        let id = sequence.ripple_insert(0, 1, Clip::of(4, "n".into(), 5.0)).unwrap();
        assert_eq!(id, ClipId { track: 0, index: 1 });
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("n", 10.0), ("b", 15.0), ("c", 19.0)]);
        assert_eq!(sequence.tracks[1].clips[0].start, 12.0);
        assert_eq!(sequence.export, span(0.0, 25.0), "a fitted export range takes it in");
        // As many as there are: after the last one.
        let last = sequence.ripple_insert(0, 4, Clip::of(5, "z".into(), 2.0)).unwrap();
        assert_eq!(sequence.clip(last).span(), span(25.0, 27.0));
        assert_eq!(sequence.ripple_insert(0, 9, Clip::of(5, "x".into(), 2.0)), None);
        assert_eq!(sequence.ripple_insert(7, 0, Clip::of(5, "x".into(), 2.0)), None, "no such track");
    }

    #[test]
    fn an_insert_on_an_empty_track_goes_to_the_start_of_the_export_range() {
        let mut sequence = three_in_a_row();
        let empty = sequence.add_track(1, TrackKind::Video);
        let id = sequence.ripple_insert(empty, 0, Clip::of(4, "n".into(), 5.0)).unwrap();
        assert_eq!(sequence.clip(id).span(), span(0.0, 5.0));
    }

    #[test]
    fn an_insert_takes_chained_clips_along_or_changes_nothing() {
        // The picture 10..20 on the video track, its sound 10..20 on the audio track under it.
        let (mut sequence, picture, sound) = detached();
        let mut blocked = sequence.clone();
        let id = sequence.ripple_insert(picture.track, 0, Clip::of(1, "n".into(), 5.0)).unwrap();
        assert_eq!(sequence.clip(id).span(), span(10.0, 15.0));
        let picture = ClipId { track: picture.track, index: 1 };
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start), (15.0, 15.0));
        // Something in the sound's way: nothing changes at all.
        blocked.place_clip(Some(sound.track), Clip { start: 22.0, ..Clip::of(2, "m".into(), 3.0) });
        let before = blocked.clone();
        assert_eq!(blocked.ripple_insert(picture.track, 0, Clip::of(1, "n".into(), 5.0)), None);
        assert_eq!(blocked, before);
    }

    #[test]
    fn a_moved_clip_takes_its_new_place_and_all_stay_one_after_another() {
        let mut sequence = three_in_a_row();
        assert_eq!(sequence.ripple_move(0, 0, 2), Some(ClipId { track: 0, index: 2 }));
        assert_eq!(names_and_starts(&sequence, 0), [("b", 0.0), ("c", 4.0), ("a", 10.0)]);
        assert_eq!(sequence.export, span(0.0, 20.0));
        sequence.ripple_move(0, 2, 0).unwrap();
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 10.0), ("c", 14.0)]);
        sequence.ripple_move(0, 1, 1).unwrap();
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 10.0), ("c", 14.0)], "its own place: as it was");
        assert_eq!(sequence.ripple_move(0, 3, 0), None);
        assert_eq!(sequence.ripple_move(0, 0, 3), None);
        let (mut detached, picture, _) = detached();
        assert_eq!(detached.ripple_move(picture.track, picture.index, 0), None, "chained: it would leave its sound");
    }

    #[test]
    fn a_retimed_clip_keeps_its_start_and_the_ones_after_it_follow() {
        let mut sequence = three_in_a_row();
        let a = ClipId { track: 0, index: 0 };
        assert!(sequence.ripple_retime(a, span(5.0, 20.0)));
        assert_eq!((sequence.clip(a).source_in, sequence.clip(a).source_out), (5.0, 20.0));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 15.0), ("c", 19.0)]);
        assert_eq!(sequence.export, span(0.0, 25.0));
        // Shorter than its fade: the fade shortens too.
        sequence.clip_mut(a).add_fade(Edge::Out);
        sequence.clip_mut(a).set_fade_length(Edge::Out, 5.0);
        assert!(sequence.ripple_retime(a, span(2.0, 4.0)));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 2.0), ("c", 6.0)]);
        assert!(sequence.clip(a).fade_lengths().1 <= 2.0);
        assert_eq!(sequence.export, span(0.0, 12.0));
        let before = sequence.clone();
        assert!(!sequence.ripple_retime(a, span(25.0, 40.0)), "past the end of its 30 s source");
        assert!(!sequence.ripple_retime(a, span(4.0, 4.0)));
        assert_eq!(sequence, before);
        let (mut detached, picture, _) = detached();
        assert!(!detached.ripple_retime(picture, span(2.0, 8.0)), "chained");
    }

    #[test]
    fn a_faster_clip_is_shorter_and_plays_more_of_its_source_each_second() {
        // Source 2..12 s at 10 s on the timeline, twice as fast: 5 s long.
        let mut clip = placed_clip();
        clip.set_speed(2.0);
        assert_eq!(clip.speed(), 2.0);
        assert_eq!(clip.span(), span(10.0, 15.0));
        assert_eq!((clip.source_time(10.0), clip.source_time(12.5)), (2.0, 7.0));
        assert_eq!(clip.time_of(7.0), 12.5);
        // The whole 30 s source would take 15 s, starting 1 s before the clip.
        assert_eq!(clip.source_span(), span(9.0, 24.0));
        clip.set_speed(0.5);
        assert_eq!(clip.span(), span(10.0, 30.0));
        assert_eq!(clip.source_time(14.0), 4.0);
        assert_eq!(clip.effects.len(), 1, "one Speed effect");
        // What a project file may say that is no speed, or too much of one.
        for (factor, played) in [(0.0, 1.0), (-3.0, 1.0), (f64::NAN, 1.0), (f64::INFINITY, 1.0), (0.1, 0.25), (100.0, 16.0)] {
            assert_eq!(Speed { factor }.played(), played, "{factor}");
        }
        assert_eq!(placed_clip().speed(), 1.0);
    }

    #[test]
    fn a_faster_clip_is_trimmed_in_timeline_time_and_keeps_its_picture_in_place() {
        let mut clip = placed_clip();
        clip.set_speed(2.0);
        let mut track = track_of(vec![clip]);
        track.trim_start(0, 12.0, 0.5);
        let clip = &track.clips[0];
        assert_eq!((clip.start, clip.source_in, clip.end()), (12.0, 6.0, 15.0));
        assert_eq!(clip.source_time(13.0), 8.0);
        // Out to the ends of the source, which takes half its length at this speed.
        track.trim_start(0, 0.0, 0.5);
        assert_eq!((track.clips[0].start, track.clips[0].source_in), (9.0, 0.0));
        track.trim_end(0, 100.0, 0.5);
        assert_eq!((track.clips[0].span(), track.clips[0].source_out), (span(9.0, 24.0), 30.0));
        track.trim_end(0, 0.0, 0.5);
        assert_eq!((track.clips[0].span(), track.clips[0].source_out), (span(9.0, 9.5), 1.0));
    }

    #[test]
    fn both_parts_of_a_cut_keep_the_speed_and_play_on_where_the_first_one_stops() {
        let mut clip = placed_clip();
        clip.set_speed(2.0);
        let mut sequence = Sequence::new(clip);
        let first = ClipId { track: 0, index: 0 };
        let second = sequence.split(first, 12.5, MIN).unwrap();
        let (a, b) = (sequence.clip(first), sequence.clip(second));
        assert_eq!((a.span(), a.source_in, a.source_out, a.speed()), (span(10.0, 12.5), 2.0, 7.0, 2.0));
        assert_eq!((b.span(), b.source_in, b.source_out, b.speed()), (span(12.5, 15.0), 7.0, 12.0, 2.0));
    }

    #[test]
    fn a_new_speed_moves_the_clips_after_it_on_its_track() {
        let mut sequence = three_in_a_row();
        // Another video track: its clip stays where it is.
        sequence.tracks.push(track_of(vec![Clip { start: 1.0, ..Clip::of(3, "d".into(), 2.0) }]));
        let a = ClipId { track: 0, index: 0 };
        assert!(sequence.clip_mut(a).add_speed());
        assert!(!sequence.clip_mut(a).add_speed(), "one is enough");
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 10.0), ("c", 14.0)], "at ×1 nothing changes");
        assert!(sequence.set_speed(a, 2.0));
        assert_eq!(sequence.clip(a).span(), span(0.0, 5.0));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 5.0), ("c", 9.0)]);
        assert_eq!(sequence.tracks[1].clips[0].start, 1.0);
        assert_eq!(sequence.export, span(0.0, 15.0), "a fitted export range goes on fitting");
        assert!(sequence.set_speed(a, 0.5));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 20.0), ("c", 24.0)]);
        assert_eq!(sequence.export, span(0.0, 30.0));
        assert!(sequence.remove_speed(a));
        assert_eq!(sequence.clip(a).speed_effect(), None);
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 10.0), ("c", 14.0)]);
        // Shorter than its fade: the fade shortens too.
        sequence.clip_mut(a).add_fade(Edge::Out);
        sequence.clip_mut(a).set_fade_length(Edge::Out, 8.0);
        assert!(sequence.set_speed(a, 4.0));
        assert_eq!(sequence.clip(a).span(), span(0.0, 2.5));
        assert!(sequence.clip(a).fade_lengths().1 <= 2.5);
        // Another part of the source, at that speed.
        assert!(sequence.ripple_retime(a, span(0.0, 20.0)));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 5.0), ("c", 9.0)]);
    }

    #[test]
    fn a_new_speed_takes_chained_clips_along_or_changes_nothing() {
        // The picture 10..20 on the video track, its sound 10..20 on the audio track under it, and
        // another clip 0..10 before the picture.
        let (mut sequence, picture, sound) = detached();
        let before_it = sequence.place_clip(Some(picture.track), Clip::of(1, "x".into(), 10.0)).unwrap();
        let picture = ClipId { track: picture.track, index: 1 };
        let mut blocked = sequence.clone();
        assert!(sequence.set_speed(before_it, 0.5));
        assert_eq!(sequence.clip(before_it).span(), span(0.0, 20.0));
        assert_eq!((sequence.clip(picture).start, sequence.clip(sound).start), (20.0, 20.0));
        // Something in the sound's way: nothing changes at all.
        blocked.place_clip(Some(sound.track), Clip { start: 22.0, ..Clip::of(2, "m".into(), 3.0) });
        let before = blocked.clone();
        assert!(!blocked.set_speed(before_it, 0.5));
        assert_eq!(blocked, before);
        // A clip's own sound is a clip of its own: its speed is set apart, so it stays.
        let (mut sequence, picture, sound) = detached();
        assert!(sequence.set_speed(picture, 2.0));
        assert_eq!((sequence.clip(picture).span(), sequence.clip(sound).span()), (span(10.0, 15.0), span(10.0, 20.0)));
    }

    #[test]
    fn detached_sound_keeps_the_speed_of_its_picture() {
        let mut clip = placed_clip();
        clip.set_speed(2.0);
        clip.add_color();
        let mut sequence = Sequence::new(clip);
        let (picture, sound) = sequence.detach_sound(ClipId { track: 0, index: 0 }).unwrap();
        assert_eq!((sequence.clip(picture).speed(), sequence.clip(sound).speed()), (2.0, 2.0));
        assert_eq!(sequence.clip(sound).span(), sequence.clip(picture).span());
        assert_eq!(sequence.clip(sound).color(), None, "the colour stays with the picture");
    }

    #[test]
    fn the_speed_is_saved_as_an_effect_and_older_files_play_as_they_are() {
        let mut clip = placed_clip();
        clip.set_speed(2.0);
        let json = serde_json::to_value(&clip).unwrap();
        assert_eq!(json["effects"], serde_json::json!([{ "kind": "Speed", "factor": 2.0 }]));
        let back: Clip = serde_json::from_value(json.clone()).unwrap();
        assert_eq!((back.speed(), back.span()), (2.0, span(10.0, 15.0)));
        let mut written = json;
        written["effects"][0]["factor"] = 0.into();
        assert_eq!(serde_json::from_value::<Clip>(written).unwrap().speed(), 1.0, "no speed at all plays as it is");
        let old: Clip = serde_json::from_value(serde_json::to_value(placed_clip()).unwrap()).unwrap();
        assert_eq!((old.speed(), old.span()), (1.0, span(10.0, 20.0)));
    }

    fn image_info() -> MediaInfo {
        MediaInfo { duration: 0.0, width: 1920, height: 1080, fps: crate::media::probe::Rational { num: 25, den: 1 }, has_audio: false, image: true }
    }

    #[test]
    fn an_image_shows_its_one_picture_for_as_long_as_it_lasts() {
        let image = Clip::whole(4, "logo".into(), &image_info());
        assert_eq!((image.source, image.span(), image.still), (4, span(0.0, IMAGE_LENGTH), Some(0.0)));
        assert!(image.is_image());
        assert_eq!((image.picture_time(0.0), image.picture_time(4.9), image.picture_time(100.0)), (0.0, 0.0, 0.0));
        assert_eq!(image.source_span(), image.span(), "nothing of it is left out: no ghost");
        // A recording is still all of it, and shows its frames one after another.
        let video = MediaInfo { duration: 30.0, image: false, has_audio: true, ..image_info() };
        let whole = Clip::whole(1, "a".into(), &video);
        assert_eq!((whole.span(), whole.still, whole.picture_time(2.5)), (span(0.0, 30.0), None, 2.5));
        assert!(!whole.is_image());
        // However fast: one picture.
        let mut sped = image.clone();
        assert!(!sped.add_speed());
        sped.effects.push(Effect::Speed(Speed { factor: 2.0 }));
        assert_eq!((sped.speed(), sped.span()), (1.0, span(0.0, IMAGE_LENGTH)), "a speed written into its file changes nothing");
        let mut sequence = Sequence::new(image);
        assert!(!sequence.set_speed(ClipId { track: 0, index: 0 }, 2.0));
    }

    #[test]
    fn an_image_is_trimmed_as_far_as_its_neighbours_let_it() {
        // A video 0..3, the image 5..10, a video 12..20.
        let image = Clip { start: 5.0, ..Clip::image(1, "logo".into()) };
        let mut track = track_of(vec![Clip { source_out: 3.0, ..Clip::new("a".into(), 30.0) }, image, Clip { start: 12.0, source_out: 8.0, ..Clip::of(2, "b".into(), 30.0) }]);
        track.trim_start(1, 1.0, 0.5);
        assert_eq!((track.clips[1].span(), track.clips[1].source_in, track.clips[1].source_out), (span(3.0, 10.0), 0.0, 7.0));
        track.trim_end(1, 15.0, 0.5);
        assert_eq!(track.clips[1].span(), span(3.0, 12.0));
        track.trim_end(1, 2.0, 0.5);
        assert_eq!(track.clips[1].span(), span(3.0, 3.5));
        track.trim_start(1, 9.0, 0.5);
        assert_eq!((track.clips[1].span(), track.clips[1].source_in), (span(3.0, 3.5), 0.0), "kept a minimal length");
        // Alone, it goes as far as it is dragged.
        let mut alone = track_of(vec![Clip { start: 5.0, ..Clip::image(1, "logo".into()) }]);
        alone.trim_end(0, 1000.0, 0.5);
        alone.trim_start(0, -50.0, 0.5);
        assert_eq!(alone.clips[0].span(), span(-50.0, 1000.0));
        assert_eq!(alone.clips[0].picture_time(500.0), 0.0);
    }

    #[test]
    fn a_cut_image_is_two_and_a_new_length_moves_the_clips_after_it() {
        // The image 0..5 with fades, a video 5..9 after it.
        let mut image = Clip::image(1, "logo".into());
        image.add_fade(Edge::In);
        image.add_fade(Edge::Out);
        let mut sequence = Sequence::new(image);
        sequence.tracks[0].clips.push(Clip { start: 5.0, source_out: 4.0, ..Clip::of(2, "b".into(), 30.0) });
        sequence.fit_export();
        let first = ClipId { track: 0, index: 0 };
        let second = sequence.split(first, 2.0, MIN).unwrap();
        let (a, b) = (sequence.clip(first), sequence.clip(second));
        assert_eq!((a.span(), a.source_in, a.source_out, a.still), (span(0.0, 2.0), 0.0, 2.0, Some(0.0)));
        assert_eq!((b.span(), b.source_in, b.source_out, b.still), (span(2.0, 5.0), 0.0, 3.0, Some(0.0)));
        assert!(a.fade(Edge::In).is_some() && a.fade(Edge::Out).is_none() && b.fade(Edge::Out).is_some());
        assert!(sequence.set_length(first, 4.0));
        assert_eq!(names_and_starts(&sequence, 0), [("logo", 0.0), ("logo", 4.0), ("b", 7.0)]);
        assert_eq!(sequence.export, span(0.0, 11.0), "a fitted export range goes on fitting");
        assert!(sequence.set_length(first, 1.0));
        assert_eq!(names_and_starts(&sequence, 0), [("logo", 0.0), ("logo", 1.0), ("b", 4.0)]);
        let before = sequence.clone();
        assert!(!sequence.set_length(first, 0.0), "no length");
        assert!(!sequence.set_length(first, f64::NAN));
        assert!(!sequence.set_length(ClipId { track: 0, index: 2 }, 3.0), "a recording is trimmed instead");
        assert_eq!(sequence, before);
    }

    /// The Freeze effects of a clip.
    fn freezes_of(clip: &Clip) -> Vec<Freeze> {
        clip.effects.iter().filter_map(Effect::as_freeze).copied().collect()
    }

    fn hold_spans(clip: &Clip) -> Vec<Span> {
        clip.holds().iter().map(|hold| hold.span).collect()
    }

    #[test]
    fn a_freeze_holds_the_frame_at_its_time_and_the_clip_plays_on_from_it_later() {
        // a 0..10, b 10..14 and c 14..20 on one track; another track's clip stays where it is.
        let mut sequence = three_in_a_row();
        sequence.tracks.push(track_of(vec![Clip { start: 12.0, ..Clip::of(3, "d".into(), 2.0) }]));
        let a = ClipId { track: 0, index: 0 };
        let clip = sequence.clip_mut(a);
        clip.add_fade(Edge::In);
        clip.add_fade(Edge::Out);
        clip.add_color();
        assert!(sequence.freeze(a, 4.0, FREEZE_LENGTH));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 12.0), ("c", 16.0)], "one clip, 2 s longer");
        let clip = sequence.clip(a);
        assert_eq!(clip.span(), span(0.0, 12.0));
        assert_eq!((clip.source_in, clip.source_out), (0.0, 10.0), "it plays the same part of its source");
        assert_eq!(freezes_of(clip), [Freeze { at: 4.0, seconds: 2.0 }]);
        assert_eq!(hold_spans(clip), [span(4.0, 6.0)]);
        let times: Vec<f64> = [3.0, 4.0, 5.9, 6.0, 7.5, 12.0].map(|t| clip.source_time(t)).to_vec();
        assert_eq!(times, [3.0, 4.0, 4.0, 4.0, 5.5, 10.0], "up to it, held, then on from it");
        assert_eq!((clip.picture_time(5.0), clip.time_of(4.0), clip.time_of(5.0)), (4.0, 4.0, 7.0));
        let runs = clip.runs();
        assert_eq!(
            runs.iter().map(|run| (run.span, run.source, run.rate)).collect::<Vec<_>>(),
            [(span(0.0, 4.0), 0.0, 1.0), (span(4.0, 6.0), 4.0, 0.0), (span(6.0, 12.0), 4.0, 1.0)]
        );
        assert_eq!(clip.hold_at(5.0).map(|hold| hold.at), Some(4.0));
        assert_eq!(clip.hold_at(6.0), None);
        // The fades are where they were, in the clip's own time; the colour is as it was.
        assert_eq!(clip.fade_lengths(), (1.0, 1.0));
        assert!(clip.opacity(11.5) < 1.0 && clip.opacity(10.5) == 1.0 && clip.color().is_some());
        assert_eq!(clip.source_span(), span(0.0, 32.0), "its whole source, with the frame held");
        assert_eq!(sequence.tracks[1].clips[0].start, 12.0, "other tracks stay");
        assert_eq!(sequence.export, span(0.0, 22.0), "a fitted export range goes on fitting");

        // Longer: what comes after it moves later.
        let effect = sequence.clip(a).holds()[0].effect;
        assert!(sequence.set_freeze_length(a, effect, 3.0));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 13.0), ("c", 17.0)]);
        // Again on the frame held: held that much longer, still one hold.
        assert!(sequence.freeze(a, 5.0, FREEZE_LENGTH));
        assert_eq!(freezes_of(sequence.clip(a)), [Freeze { at: 4.0, seconds: 5.0 }]);
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 15.0), ("c", 19.0)]);
        // Another frame, as long: the hold moves to where the clip gets to it, nothing else moves.
        assert!(sequence.move_freeze(a, effect, 8.0));
        let clip = sequence.clip(a);
        assert_eq!((hold_spans(clip), clip.span(), clip.source_time(14.0)), (vec![span(8.0, 13.0)], span(0.0, 15.0), 9.0));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 15.0), ("c", 19.0)]);
        assert!(sequence.move_freeze(a, effect, 40.0));
        assert_eq!(freezes_of(sequence.clip(a))[0].at, 10.0, "not past what it plays");
        // Off: all as it was.
        assert!(sequence.remove_freeze(a, effect));
        assert_eq!(names_and_starts(&sequence, 0), [("a", 0.0), ("b", 10.0), ("c", 14.0)]);
        assert!(sequence.clip(a).holds().is_empty() && sequence.clip(a).color().is_some());

        // Faster: 1 s into it at ×2 is 2 s into its source.
        let mut fast = three_in_a_row();
        assert!(fast.set_speed(a, 2.0));
        assert!(fast.freeze(a, 1.0, FREEZE_LENGTH));
        let clip = fast.clip(a);
        assert_eq!((clip.span(), hold_spans(clip), clip.source_time(4.0)), (span(0.0, 7.0), vec![span(1.0, 3.0)], 4.0));
        // A new speed: the frame is held as long, and the clip plays at the new speed around it.
        assert!(fast.set_speed(a, 1.0));
        let clip = fast.clip(a);
        assert_eq!((clip.span(), hold_spans(clip)), (span(0.0, 12.0), vec![span(2.0, 4.0)]));

        // What holds no frame, or not then: nothing changes.
        let before = sequence.clone();
        assert!(!sequence.freeze(a, 10.5, FREEZE_LENGTH), "not there then");
        assert!(!sequence.freeze(a, 5.0, 0.0), "no length");
        assert!(!sequence.set_freeze_length(a, effect, 1.0), "it has no such hold");
        assert!(!sequence.move_freeze(a, 0, 1.0), "a fade is no hold");
        assert!(!sequence.remove_freeze(a, 0));
        assert_eq!(sequence, before);
        let logo = sequence.paste(Clip::image(1, "logo".into()), 30.0, Some(0), TrackKind::Video);
        let with_logo = sequence.clone();
        assert!(!sequence.freeze(logo, 31.0, FREEZE_LENGTH), "a still holds its one frame anyway");
        assert_eq!(sequence, with_logo);
    }

    #[test]
    fn trimming_and_cutting_a_clip_take_what_it_holds_with_the_time() {
        // A 30 s source, 0..10 of it from 0, holding its frame at 4 s for 2 s: 0..12.
        let held = || {
            let mut sequence = Sequence::new(Clip { source_out: 10.0, ..Clip::new("a".into(), 30.0) });
            assert!(sequence.freeze(ClipId { track: 0, index: 0 }, 4.0, FREEZE_LENGTH));
            sequence
        };
        let a = ClipId { track: 0, index: 0 };
        // The start into the hold: the rest of it is held, the picture stays where it was.
        let mut sequence = held();
        sequence.trim_start(a, 5.0, MIN);
        let clip = sequence.clip(a);
        assert_eq!((clip.span(), clip.source_in, hold_spans(clip)), (span(5.0, 12.0), 4.0, vec![span(5.0, 6.0)]));
        assert_eq!((clip.source_time(5.5), clip.source_time(6.5)), (4.0, 4.5));
        // Past it: it goes.
        sequence.trim_start(a, 7.0, MIN);
        let clip = sequence.clip(a);
        assert_eq!((clip.span(), clip.source_in, clip.effects.len()), (span(7.0, 12.0), 5.0, 0));
        // Back out: what comes back is the recording.
        sequence.trim_start(a, 2.0, MIN);
        assert_eq!((sequence.clip(a).span(), sequence.clip(a).source_in), (span(2.0, 12.0), 0.0));

        // The end into the hold, past it, and out again.
        let mut sequence = held();
        sequence.trim_end(a, 5.0, MIN);
        let clip = sequence.clip(a);
        assert_eq!((clip.span(), clip.source_out, hold_spans(clip)), (span(0.0, 5.0), 4.0, vec![span(4.0, 5.0)]));
        sequence.trim_end(a, 8.0, MIN);
        let clip = sequence.clip(a);
        assert_eq!((clip.span(), clip.source_out, hold_spans(clip)), (span(0.0, 8.0), 7.0, vec![span(4.0, 5.0)]));
        sequence.trim_end(a, 3.5, MIN);
        let clip = sequence.clip(a);
        assert_eq!((clip.span(), clip.source_out, clip.effects.len()), (span(0.0, 3.5), 3.5, 0));
        // Out as far as the source goes: the hold makes the clip that much longer.
        let mut sequence = held();
        sequence.trim_end(a, 100.0, MIN);
        assert_eq!((sequence.clip(a).span(), sequence.clip(a).source_out), (span(0.0, 32.0), 30.0));

        // A cut in the hold: each part holds the frame for its part of the time.
        let mut sequence = held();
        let after = sequence.split(a, 5.0, MIN).unwrap();
        let (first, second) = (sequence.clip(a), sequence.clip(after));
        assert_eq!((first.span(), first.source_out, hold_spans(first)), (span(0.0, 5.0), 4.0, vec![span(4.0, 5.0)]));
        assert_eq!((second.span(), second.source_in, hold_spans(second)), (span(5.0, 12.0), 4.0, vec![span(5.0, 6.0)]));
        assert_eq!((second.source_time(5.5), second.source_time(7.0)), (4.0, 5.0));
        // After it, and where it starts: the hold goes with the part it is in.
        for (t, first_holds, second_holds) in [(8.0, vec![span(4.0, 6.0)], vec![]), (4.0, vec![], vec![span(4.0, 6.0)])] {
            let mut sequence = held();
            let after = sequence.split(a, t, MIN).unwrap();
            assert_eq!((hold_spans(sequence.clip(a)), hold_spans(sequence.clip(after))), (first_holds, second_holds), "a cut at {t}");
            assert_eq!(sequence.clip(after).span(), span(t, 12.0));
        }

        // Another part of its source: what it holds of it stays held.
        let mut sequence = held();
        assert!(sequence.ripple_retime(a, span(3.0, 8.0)));
        assert_eq!((sequence.clip(a).span(), hold_spans(sequence.clip(a))), (span(0.0, 7.0), vec![span(1.0, 3.0)]));
        assert!(sequence.ripple_retime(a, span(5.0, 8.0)));
        assert_eq!((sequence.clip(a).span(), sequence.clip(a).effects.len()), (span(0.0, 3.0), 0));
        // Its file turned out shorter than the moment held: the hold goes.
        let mut sequence = held();
        sequence.refit_source(0, 3.0);
        assert!(sequence.clip(a).effects.is_empty());
    }

    #[test]
    fn the_sound_detached_from_a_clip_holds_its_frames_with_it_in_silence() {
        // The picture 10..20 on the video track, its sound 10..20 on the audio track under it, both
        // from 2 s of their source.
        let (mut sequence, picture, sound) = detached();
        assert!(sequence.freeze(picture, 14.0, FREEZE_LENGTH));
        for id in [picture, sound] {
            let clip = sequence.clip(id);
            assert_eq!((clip.span(), hold_spans(clip), freezes_of(clip)), (span(10.0, 22.0), vec![span(14.0, 16.0)], vec![Freeze { at: 6.0, seconds: 2.0 }]));
        }
        // Longer from either of them, another frame, off: both alike.
        let effect = sequence.clip(sound).holds()[0].effect;
        assert!(sequence.set_freeze_length(sound, effect, 3.0));
        assert_eq!((sequence.clip(picture).span(), sequence.clip(sound).span()), (span(10.0, 23.0), span(10.0, 23.0)));
        let effect = sequence.clip(picture).holds()[0].effect;
        assert!(sequence.move_freeze(picture, effect, 3.0));
        assert_eq!((hold_spans(sequence.clip(picture)), hold_spans(sequence.clip(sound))), (vec![span(11.0, 14.0)], vec![span(11.0, 14.0)]));
        assert!(sequence.remove_freeze(picture, effect));
        assert_eq!((sequence.clip(picture).span(), sequence.clip(sound).span()), (span(10.0, 20.0), span(10.0, 20.0)));
        assert!(sequence.clip(sound).effects.iter().all(|effect| effect.as_freeze().is_none()));

        // Detached after: the sound holds the frames the picture holds.
        let mut clip = placed_clip();
        clip.add_freeze(6.0, 2.0);
        let mut sequence = Sequence::new(clip);
        let (picture, sound) = sequence.detach_sound(ClipId { track: 0, index: 0 }).unwrap();
        assert_eq!(hold_spans(sequence.clip(sound)), [span(14.0, 16.0)]);
        assert_eq!(hold_spans(sequence.clip(picture)), [span(14.0, 16.0)]);

        // Cut in parts, all of them chained, as the editor leaves them: the parts the frame is in hold
        // it, the parts after them move later, those before stay. What is chained to them from the
        // hold on moves too; what starts before it stays.
        let (mut sequence, picture, sound) = detached();
        assert_eq!(sequence.split_all(15.0, MIN).len(), 2);
        let early = sequence.paste(Clip::image(1, "early".into()), 11.0, None, TrackKind::Video);
        let late = sequence.paste(Clip { source_out: 1.0, ..Clip::image(1, "late".into()) }, 17.0, Some(early.track), TrackKind::Video);
        assert_eq!(late.track, early.track);
        sequence.chain(early, picture);
        sequence.chain(late, picture);
        assert_eq!(sequence.group_of(picture).len(), 6);
        assert!(sequence.freeze(picture, 12.0, FREEZE_LENGTH));
        let spans = |sequence: &Sequence, track: usize| sequence.tracks[track].clips.iter().map(Clip::span).collect::<Vec<_>>();
        assert_eq!(spans(&sequence, picture.track), [span(10.0, 17.0), span(17.0, 22.0)]);
        assert_eq!(spans(&sequence, sound.track), [span(10.0, 17.0), span(17.0, 22.0)]);
        assert_eq!(spans(&sequence, early.track), [span(11.0, 16.0), span(19.0, 20.0)]);
        assert_eq!(hold_spans(sequence.clip(sound)), [span(12.0, 14.0)]);
        assert!(sequence.clip(ClipId { track: sound.track, index: 1 }).holds().is_empty());

        // Something in the way of what has to move: nothing changes.
        let (mut blocked, picture, sound) = detached();
        let music = blocked.place_clip(Some(sound.track), Clip { start: 21.0, ..Clip::of(2, "music".into(), 3.0) }).unwrap();
        let wall = blocked.add_track(blocked.tracks.len(), TrackKind::Video);
        let tied = blocked.place_clip(Some(wall), Clip { start: 30.0, ..Clip::of(3, "tied".into(), 1.0) }).unwrap();
        blocked.place_clip(Some(wall), Clip { start: 31.0, ..Clip::of(3, "wall".into(), 1.0) }).unwrap();
        blocked.chain(tied, music);
        let before = blocked.clone();
        assert!(!blocked.freeze(picture, 14.0, FREEZE_LENGTH));
        assert_eq!(blocked, before);
        // Without the wall, the music after the sound moves later with what is chained to it.
        blocked.remove_clip(ClipId { track: wall, index: 1 });
        assert!(blocked.freeze(picture, 14.0, FREEZE_LENGTH));
        assert_eq!((blocked.clip(music).start, blocked.clip(tied).start), (23.0, 32.0));
    }

    #[test]
    fn a_freeze_is_saved_as_an_effect() {
        let mut clip = Clip::new("a".into(), 30.0);
        clip.add_freeze(4.5, 2.0);
        let text = serde_json::to_string(&clip.effects).unwrap();
        assert_eq!(text, r#"[{"kind":"Freeze","at":4.5,"seconds":2.0}]"#);
        assert_eq!(serde_json::from_str::<Vec<Effect>>(&text).unwrap(), clip.effects);
    }

    #[test]
    fn a_still_has_no_sound_and_no_part_of_a_recording() {
        let mut sequence = Sequence::new(Clip::image(1, "logo".into()));
        let id = ClipId { track: 0, index: 0 };
        assert!(!sequence.sounds(id));
        assert_eq!(sequence.detach_sound(id), None);
        assert!(!sequence.ripple_retime(id, span(0.0, 2.0)));
        assert_eq!(sequence.tracks.len(), 1);
    }

    #[test]
    fn a_still_is_saved_as_such_and_a_recording_that_became_an_image_is_shown_as_one() {
        let json = serde_json::to_value(Clip::image(1, "logo".into())).unwrap();
        assert_eq!((json["still"].as_f64(), json["source_out"].as_f64()), (Some(0.0), Some(IMAGE_LENGTH)));
        let back: Clip = serde_json::from_value(json).unwrap();
        assert_eq!(back, Clip::image(1, "logo".into()));
        assert_eq!(serde_json::to_value(placed_clip()).unwrap().get("still"), None, "a recording's clip says nothing of it");
        // Its file was pointed to an image: the clip shows it, as long as it was.
        let mut clip = placed_clip();
        clip.set_speed(2.0);
        clip.refit_source(0.0);
        assert_eq!((clip.span(), clip.still, clip.is_image()), (span(10.0, 15.0), Some(0.0), true));
        let mut sequence = Sequence::new(Clip::image(1, "logo".into()));
        assert!(!sequence.refit_source(1, 0.0), "an image stays as it is");
    }
}

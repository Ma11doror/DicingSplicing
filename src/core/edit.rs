//! The edit: clips of sources placed on tracks, and the range of the timeline that is exported.
//!
//! All times here are timeline seconds unless a name says `source`. They can be negative: the
//! user sees time counted from the export start, so the timeline's own zero means nothing to them.
//! A clip plays its source from `source_in` to `source_out`, starting at `start`.

use crate::core::export::ExportSettings;
use crate::core::preset::Output;
use crate::media::geometry::Transform;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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

/// Something done to a clip. Effects apply in the order of the clip's list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Effect {
    Fade(Fade),
    Color(Color),
}

impl Effect {
    fn as_fade(&self) -> Option<&Fade> {
        match self {
            Effect::Fade(fade) => Some(fade),
            _ => None,
        }
    }

    fn is_fade_at(&self, edge: Edge) -> bool {
        self.as_fade().is_some_and(|fade| fade.edge == edge)
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
}

fn full_volume() -> f64 {
    1.0
}

impl Clip {
    /// The whole first source, from the start of the timeline.
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
        }
    }

    /// The source turned out to have another length: its file was replaced. The trim stays as far as
    /// it can; if nothing of it is left, the clip takes the whole source.
    pub fn refit_source(&mut self, duration: f64) {
        self.source_duration = duration;
        if self.source_in >= duration {
            self.source_in = 0.0;
            self.source_out = duration;
        }
        self.source_out = self.source_out.min(duration);
    }

    /// What the clip's sound is multiplied by.
    pub fn gain(&self) -> f64 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    pub fn end(&self) -> f64 {
        self.start + self.source_out - self.source_in
    }

    pub fn span(&self) -> Span {
        Span { start: self.start, end: self.end() }
    }

    /// Where the whole source would be if it were not trimmed.
    pub fn source_span(&self) -> Span {
        let offset = self.offset();
        Span { start: offset, end: offset + self.source_duration }
    }

    /// Source time at timeline time `t`, also outside the trimmed part.
    pub fn source_time(&self, t: f64) -> f64 {
        t - self.offset()
    }

    /// Timeline time of the source's start.
    fn offset(&self) -> f64 {
        self.start - self.source_in
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
    fn trim_start(&mut self, t: f64, lo: f64, min_len: f64) {
        let offset = self.offset();
        self.start = clamp(t, lo.max(offset), self.end() - min_len);
        self.source_in = self.start - offset;
        self.fit_fades();
    }

    /// Moves the end edge to `t`, not after `hi`.
    fn trim_end(&mut self, t: f64, hi: f64, min_len: f64) {
        let offset = self.offset();
        let end = clamp(t, self.start + min_len, hi.min(offset + self.source_duration));
        self.source_out = end - offset;
        self.fit_fades();
    }
}

/// Which track something is about. Unlike its place among the tracks, this does not change.
pub type TrackId = u32;

/// A layer of the picture. Its clips are sorted by start and do not overlap.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    #[serde(default)]
    pub id: TrackId,
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

    /// Puts a clip's start at `t`, between its neighbours. What it plays does not change.
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

    /// The clip whose picture is seen at `t`: the topmost one on a track that is not hidden. It covers
    /// the whole frame, bars included, so nothing under it shows.
    pub fn shown_at(&self, t: f64) -> Option<ClipId> {
        let mut visible = self.tracks.iter().enumerate().rev().filter(|(_, track)| !track.hidden);
        visible.find_map(|(track, clips)| Some(ClipId { track, index: clips.clip_at(t)? }))
    }

    /// The clip seen at `t` under the tracks from `track` up: what a clip on `track` shows through
    /// while it fades.
    pub fn shown_under(&self, t: f64, track: usize) -> Option<ClipId> {
        let mut visible = self.tracks[..track.min(self.tracks.len())].iter().enumerate().rev().filter(|(_, track)| !track.hidden);
        visible.find_map(|(track, clips)| Some(ClipId { track, index: clips.clip_at(t)? }))
    }

    /// What the picture is made of in `range`: pieces of it, each with the clip that is seen there.
    /// Pieces that follow each other with the same clip are one.
    pub fn picture(&self, range: Span) -> Vec<(Span, Option<ClipId>)> {
        let mut cuts = vec![range.start, range.end];
        for (_, clip) in self.clips().filter(|(id, _)| !self.tracks[id.track].hidden) {
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

    /// Puts a clip on a new track under all the others, right after the last clip: there nothing
    /// covers it. An export range that fitted the clips, or had none to fit, takes it in.
    pub fn add_below(&mut self, mut clip: Clip) -> ClipId {
        clip.start = self.content().map_or(self.export.start, |content| content.end);
        self.place_clip(None, clip).unwrap_or(ClipId { track: 0, index: 0 })
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

    /// Puts `clip` at `t` exactly: on `track` if it is free there, else on a new track right above
    /// it, or above all the others with no track given. Returns where it went.
    pub fn paste(&mut self, mut clip: Clip, t: f64, track: Option<usize>) -> ClipId {
        clip.start = t;
        let len = clip.span().len();
        let target = match track {
            Some(track) if self.free_on(track, t, len) => track,
            Some(track) => self.add_track(track + 1),
            None => self.add_track(self.tracks.len()),
        };
        self.place_clip(Some(target), clip).unwrap_or(ClipId { track: target, index: 0 })
    }

    /// A copy of a clip, with its effects, right after it. Returns the copy.
    pub fn duplicate(&mut self, id: ClipId) -> ClipId {
        let clip = self.clip(id).clone();
        let end = clip.end();
        self.paste(clip, end, Some(id.track))
    }

    /// Removes a clip, and moves the clips after it on its track back by its length: no gap is left
    /// where it was. Other tracks stay as they are.
    pub fn ripple_delete(&mut self, id: ClipId) {
        let span = self.clip(id).span();
        let fitted = self.export_fits();
        let track = &mut self.tracks[id.track];
        track.clips.remove(id.index);
        for clip in track.clips.iter_mut().filter(|clip| clip.start >= span.end - SAME_TIME) {
            clip.start -= span.len();
        }
        if fitted {
            self.fit_export();
        }
    }

    /// Removes a clip. Its track stays, even when it is left empty.
    pub fn remove_clip(&mut self, id: ClipId) {
        let fitted = self.export_fits();
        self.tracks[id.track].clips.remove(id.index);
        if fitted {
            self.fit_export();
        }
    }

    /// Removes a track with its clips.
    pub fn remove_track(&mut self, track: usize) {
        let fitted = self.export_fits();
        self.tracks.remove(track);
        if fitted {
            self.fit_export();
        }
    }

    /// Puts an empty track at `index` among the tracks, 0 being the bottom one. Returns where it is.
    pub fn add_track(&mut self, index: usize) -> usize {
        let index = index.min(self.tracks.len());
        let id = self.next_track_id();
        self.tracks.insert(index, Track { id, ..Track::default() });
        index
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

    /// Moves a clip to another track, its start as near `start` as the free time there allows: the
    /// free time where the clip's middle would be. None if the clip does not fit into it.
    pub fn move_clip_to_track(&mut self, id: ClipId, track: usize, start: f64) -> Option<ClipId> {
        if track == id.track || track >= self.tracks.len() {
            return None;
        }
        let len = self.clip(id).span().len();
        let gap = self.tracks[track].gap_at(start + len / 2.0)?;
        if gap.len() + SAME_TIME < len {
            return None;
        }
        let mut clip = self.tracks[id.track].clips.remove(id.index);
        clip.start = clamp(start, gap.start, gap.end - len);
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
    /// the rest, with the same framing and sound. From then on they are two clips like any others.
    /// None if `t` is closer than `min_len` to an edge. Returns the part after the cut.
    pub fn split(&mut self, id: ClipId, t: f64, min_len: f64) -> Option<ClipId> {
        let clip = self.clip(id);
        if t - clip.start < min_len - SAME_TIME || clip.end() - t < min_len - SAME_TIME {
            return None;
        }
        let cut = clip.source_time(t);
        // The fade in stays with the part before the cut, the fade out goes with the part after it.
        // Both keep the colour.
        let mut after = Clip { start: t, source_in: cut, ..clip.clone() };
        after.effects.retain(|effect| !effect.is_fade_at(Edge::In));
        after.fit_fades();
        let before = self.clip_mut(id);
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

    /// Puts a clip's start at `t`. The export range stays: this is how a clip gets black around it.
    pub fn move_clip(&mut self, id: ClipId, t: f64) {
        self.tracks[id.track].move_clip(id.index, t);
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

        let id = sequence.add_below(Clip::of(1, "b".into(), 5.0));
        assert_eq!(id, ClipId { track: 0, index: 0 });
        assert_eq!(sequence.tracks.len(), 2);
        assert_eq!((sequence.clip(id).source, sequence.clip(id).span()), (1, span(12.0, 17.0)));
        assert_eq!(sequence.clip(ClipId { track: 1, index: 0 }).name, "a");
        assert_ne!(sequence.tracks[0].id, sequence.tracks[1].id);
        // The range fitted the clips, so it takes the new one in.
        assert_eq!(sequence.export, span(2.0, 17.0));

        // Set by hand, it stays.
        sequence.export = span(0.0, 5.0);
        sequence.add_below(Clip::of(2, "c".into(), 3.0));
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
        let id = sequence.add_below(Clip::of(4, "b".into(), 6.0));
        assert_eq!(sequence.clip(id).span(), span(3.0, 9.0));
        assert_eq!(sequence.export, span(3.0, 9.0));
    }

    #[test]
    fn removing_a_clip_leaves_its_track() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 10.0));
        sequence.add_below(Clip::of(1, "b".into(), 5.0));
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
        sequence.add_below(Clip::of(1, "b".into(), 5.0));
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
        let pasted = sequence.paste(sequence.clip(b).clone(), 50.0, Some(0));
        assert_eq!((pasted, sequence.clip(pasted).span()), (ClipId { track: 0, index: 2 }, span(50.0, 54.0)));
        let pasted = sequence.paste(sequence.clip(b).clone(), 5.0, Some(0));
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
        assert_eq!(sequence.add_track(0), 0);
        assert_eq!(sequence.add_track(99), 2);
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
        sequence.add_below(Clip::of(1, "b".into(), 5.0));
        sequence.tracks[0].clips.push(Clip { start: 50.0, ..placed_clip() });
        assert_eq!((sequence.uses(0), sequence.uses(1), sequence.uses(2)), (2, 1, 0));
        assert!(!sequence.refit_source(0, 30.0 + 1e-4));
        assert!(sequence.refit_source(0, 6.0));
        let outs: Vec<f64> = sequence.clips().filter(|(_, c)| c.source == 0).map(|(_, c)| c.source_out).collect();
        assert_eq!(outs, [6.0, 6.0]);
        assert_eq!(sequence.clip(ClipId { track: 0, index: 0 }).source_out, 5.0);
    }
}

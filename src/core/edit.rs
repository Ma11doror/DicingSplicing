//! The edit: clips of sources placed on tracks, and the range of the timeline that is exported.
//!
//! All times here are timeline seconds unless a name says `source`. The timeline starts at 0;
//! a clip plays its source from `source_in` to `source_out`, starting at `start`.

use crate::core::export::ExportSettings;
use crate::core::preset::Output;
use crate::media::geometry::Transform;
use serde::{Deserialize, Serialize};

/// A time range, start inclusive, end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

/// Where the edges of a span may go: within `lo..=hi`, at least `min_len` apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub lo: f64,
    pub hi: f64,
    pub min_len: f64,
}

impl Span {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    /// Drags the start edge; the end stays.
    pub fn resize_start(&mut self, t: f64, b: Bounds) {
        self.start = clamp(t, b.lo, self.end - b.min_len);
    }

    /// Drags the end edge; the start stays.
    pub fn resize_end(&mut self, t: f64, b: Bounds) {
        self.end = clamp(t, self.start + b.min_len, b.hi);
    }

    /// Puts the start at `t`; the end follows only if it would be too close.
    pub fn set_start(&mut self, t: f64, b: Bounds) {
        self.start = clamp(t, b.lo, b.hi - b.min_len);
        self.end = clamp(self.end, self.start + b.min_len, b.hi);
    }

    /// Puts the end at `t`; the start follows only if it would be too close.
    pub fn set_end(&mut self, t: f64, b: Bounds) {
        self.end = clamp(t, b.lo + b.min_len, b.hi);
        self.start = clamp(self.start, b.lo, self.end - b.min_len);
    }

    /// Moves the whole span, keeping its length.
    pub fn move_to(&mut self, start: f64, b: Bounds) {
        let len = self.len();
        self.start = clamp(start, b.lo, b.hi - len);
        self.end = self.start + len;
    }
}

/// A piece of a source on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
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
}

fn full_volume() -> f64 {
    1.0
}

impl Clip {
    /// The whole source, from the start of the timeline.
    pub fn new(name: String, source_duration: f64) -> Self {
        Self {
            name,
            start: 0.0,
            source_in: 0.0,
            source_out: source_duration,
            source_duration,
            transform: Transform::default(),
            volume: full_volume(),
            muted: false,
        }
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

    /// Moves the start edge to `t`, not before `lo`; the picture stays where it is on the timeline.
    fn trim_start(&mut self, t: f64, lo: f64, min_len: f64) {
        let offset = self.offset();
        self.start = clamp(t, lo.max(offset), self.end() - min_len);
        self.source_in = self.start - offset;
    }

    /// Moves the end edge to `t`, not after `hi`.
    fn trim_end(&mut self, t: f64, hi: f64, min_len: f64) {
        let offset = self.offset();
        let end = clamp(t, self.start + min_len, hi.min(offset + self.source_duration));
        self.source_out = end - offset;
    }
}

/// A layer of the picture. Its clips are sorted by start and do not overlap.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn clip_at(&self, t: f64) -> Option<usize> {
        self.clips.iter().position(|c| c.span().contains(t))
    }

    /// Trims a clip's start, up to the end of the clip before it.
    pub fn trim_start(&mut self, index: usize, t: f64, min_len: f64) {
        let lo = index.checked_sub(1).map_or(0.0, |i| self.clips[i].end());
        self.clips[index].trim_start(t, lo, min_len);
    }

    /// Trims a clip's end, up to the start of the clip after it.
    pub fn trim_end(&mut self, index: usize, t: f64, min_len: f64) {
        let hi = self.clips.get(index + 1).map_or(f64::INFINITY, |c| c.start);
        self.clips[index].trim_end(t, hi, min_len);
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
        Self { tracks: vec![Track { clips: vec![clip] }], export, output: Output::default(), settings: ExportSettings::default() }
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

    /// The topmost clip at `t`.
    pub fn clip_at(&self, t: f64) -> Option<ClipId> {
        self.tracks.iter().enumerate().rev().find_map(|(track, clips)| Some(ClipId { track, index: clips.clip_at(t)? }))
    }

    /// From the start of the first clip to the end of the last one.
    pub fn content(&self) -> Option<Span> {
        self.clips().map(|(_, c)| c.span()).reduce(|a, b| Span { start: a.start.min(b.start), end: a.end.max(b.end) })
    }

    /// How far the timeline goes: to the end of the furthest source, trimmed parts included, so
    /// that every trim can be undone by dragging, or of the export range.
    pub fn end(&self) -> f64 {
        self.clips().map(|(_, c)| c.source_span().end).fold(self.export.end, f64::max)
    }

    /// Where the export range may go.
    pub fn export_bounds(&self, min_len: f64) -> Bounds {
        Bounds { lo: 0.0, hi: self.end(), min_len }
    }

    pub fn trim_start(&mut self, id: ClipId, t: f64, min_len: f64) {
        self.tracks[id.track].trim_start(id.index, t, min_len);
    }

    pub fn trim_end(&mut self, id: ClipId, t: f64, min_len: f64) {
        self.tracks[id.track].trim_end(id.index, t, min_len);
    }
}

/// Like `f64::clamp`, but never panics: if the bounds cross, `hi` wins.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Bounds = Bounds { lo: 0.0, hi: 200.0, min_len: 1.0 };

    fn span(start: f64, end: f64) -> Span {
        Span { start, end }
    }

    /// A 30 s source trimmed to 2..12 s, placed at 10 s on the timeline.
    fn placed_clip() -> Clip {
        Clip { start: 10.0, source_in: 2.0, source_out: 12.0, ..Clip::new("a".into(), 30.0) }
    }

    #[test]
    fn resize_keeps_the_other_edge() {
        let mut s = span(20.0, 50.0);
        s.resize_start(60.0, B);
        assert_eq!(s, span(49.0, 50.0));
        s.resize_start(-5.0, B);
        assert_eq!(s, span(0.0, 50.0));
        s.resize_end(500.0, B);
        assert_eq!(s, span(0.0, 200.0));
        s.resize_end(-3.0, B);
        assert_eq!(s, span(0.0, 1.0));
    }

    #[test]
    fn set_moves_the_other_edge_only_when_needed() {
        let mut s = span(20.0, 50.0);
        s.set_start(30.0, B);
        assert_eq!(s, span(30.0, 50.0));
        s.set_start(70.0, B);
        assert_eq!(s, span(70.0, 71.0));
        s.set_end(40.0, B);
        assert_eq!(s, span(39.0, 40.0));
        s.set_end(500.0, B);
        assert_eq!(s, span(39.0, 200.0));
        s.set_start(500.0, B);
        assert_eq!(s, span(199.0, 200.0));
    }

    #[test]
    fn move_keeps_the_length() {
        let mut s = span(20.0, 50.0);
        s.move_to(180.0, B);
        assert_eq!(s, span(170.0, 200.0));
        s.move_to(-3.0, B);
        assert_eq!(s, span(0.0, 30.0));
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
        let mut track = Track { clips: vec![placed_clip()] };
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
    fn trimming_stops_at_the_timeline_start_and_at_neighbours() {
        // Source time 0 would be before the timeline start.
        let mut track = Track { clips: vec![Clip { start: 1.0, source_in: 5.0, ..Clip::new("a".into(), 30.0) }] };
        track.trim_start(0, -10.0, 0.5);
        assert_eq!((track.clips[0].start, track.clips[0].source_in), (0.0, 4.0));

        let mut track = Track { clips: vec![Clip::new("a".into(), 10.0), placed_clip()] };
        track.clips[0].source_out = 5.0;
        track.trim_end(0, 30.0, 0.5);
        assert_eq!(track.clips[0].end(), 10.0);
        track.trim_start(1, 0.0, 0.5);
        assert_eq!(track.clips[1].start, 10.0);
    }

    #[test]
    fn finds_the_topmost_clip_and_the_extent() {
        let mut sequence = Sequence::new(Clip::new("a".into(), 30.0));
        assert_eq!(sequence.export, span(0.0, 30.0));
        sequence.tracks.push(Track { clips: vec![placed_clip()] });
        let top = ClipId { track: 1, index: 0 };
        assert_eq!(sequence.clip_at(5.0), Some(ClipId { track: 0, index: 0 }));
        assert_eq!(sequence.clip_at(15.0), Some(top));
        assert_eq!(sequence.clip_at(30.0), None);
        assert_eq!(sequence.content(), Some(span(0.0, 30.0)));
        // The second clip's source reaches 38 s even though the clip ends at 20 s.
        assert_eq!(sequence.end(), 38.0);

        sequence.trim_end(ClipId { track: 0, index: 0 }, 12.0, 0.5);
        assert_eq!(sequence.content(), Some(span(0.0, 20.0)));
        assert_eq!(sequence.clip_at(25.0), None);
    }
}

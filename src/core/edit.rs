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

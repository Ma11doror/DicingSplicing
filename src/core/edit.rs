//! The edit: clips of sources placed on tracks, and the range of the timeline that is exported.
//!
//! All times here are timeline seconds unless a name says `source`. They can be negative: the
//! user sees time counted from the export start, so the timeline's own zero means nothing to them.
//! A clip plays its source from `source_in` to `source_out`, starting at `start`.

use crate::core::export::ExportSettings;
use crate::core::preset::Output;
use crate::media::geometry::Transform;
use serde::{Deserialize, Serialize};

/// Closer than this, two times are the same one: a saved file may not bring back the last digit.
const SAME_TIME: f64 = 1e-6;

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
    /// The whole source, from the start of the timeline.
    pub fn new(name: String, source_duration: f64) -> Self {
        Self {
            name,
            source: 0,
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
        self.clips().map(|(_, c)| c.span()).reduce(Span::union)
    }

    /// Where there is anything to see: the export range and all the sources, trimmed parts included,
    /// so that every trim can be undone by dragging.
    pub fn reach(&self) -> Span {
        self.clips().map(|(_, c)| c.source_span()).fold(self.export, Span::union)
    }

    /// The parts of the export range that no clip covers: black and silence in the result.
    pub fn gaps(&self) -> Vec<Span> {
        let mut spans: Vec<Span> = self.clips().map(|(_, c)| c.span()).collect();
        spans.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut gaps = Vec::new();
        let mut at = self.export.start;
        for span in spans {
            if span.start >= self.export.end {
                break;
            }
            if span.start > at {
                gaps.push(Span { start: at, end: span.start });
            }
            at = at.max(span.end);
        }
        if at < self.export.end {
            gaps.push(Span { start: at, end: self.export.end });
        }
        gaps
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
    fn trimming_stops_at_the_source_and_at_neighbours() {
        // The source starts 4 s before the zero of the timeline, and that is as far as it goes.
        let mut track = Track { clips: vec![Clip { start: 1.0, source_in: 5.0, ..Clip::new("a".into(), 30.0) }] };
        track.trim_start(0, -10.0, 0.5);
        assert_eq!((track.clips[0].start, track.clips[0].source_in), (-4.0, 0.0));

        let mut track = Track { clips: vec![Clip::new("a".into(), 10.0), placed_clip()] };
        track.clips[0].source_out = 5.0;
        track.trim_end(0, 30.0, 0.5);
        assert_eq!(track.clips[0].end(), 10.0);
        track.trim_start(1, 0.0, 0.5);
        assert_eq!(track.clips[1].start, 10.0);
    }

    #[test]
    fn moving_a_clip_keeps_what_it_plays() {
        let mut track = Track { clips: vec![placed_clip()] };
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
        let neighbours = || Track {
            clips: vec![
                Clip { start: 0.0, source_out: 5.0, ..Clip::new("a".into(), 30.0) },
                Clip { start: 10.0, source_out: 5.0, ..Clip::new("b".into(), 30.0) },
                Clip { start: 30.0, source_out: 5.0, ..Clip::new("c".into(), 30.0) },
            ],
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
        sequence.tracks.push(Track { clips: vec![Clip { start: 5.0, source_out: 10.0, ..Clip::new("b".into(), 30.0) }] });
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
        sequence.tracks.push(Track { clips: vec![Clip { start: 6.0, source_out: 4.0, ..Clip::new("b".into(), 30.0) }] });
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
        sequence.tracks.push(Track { clips: vec![placed_clip()] });
        let top = ClipId { track: 1, index: 0 };
        assert_eq!(sequence.clip_at(5.0), Some(ClipId { track: 0, index: 0 }));
        assert_eq!(sequence.clip_at(15.0), Some(top));
        assert_eq!(sequence.clip_at(30.0), None);
        assert_eq!(sequence.content(), Some(span(0.0, 30.0)));
        // The second clip's source reaches 38 s even though the clip ends at 20 s.
        assert_eq!(sequence.reach(), span(0.0, 38.0));

        sequence.trim_end(ClipId { track: 0, index: 0 }, 12.0, 0.5);
        assert_eq!(sequence.content(), Some(span(0.0, 20.0)));
        assert_eq!(sequence.clip_at(25.0), None);

        // The reach goes wherever the clips and the export range go.
        sequence.move_clip(top, -40.0);
        assert_eq!(sequence.reach().start, -42.0);
        sequence.export = span(-50.0, 100.0);
        assert_eq!(sequence.reach(), span(-50.0, 100.0));
    }
}

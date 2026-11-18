//! Which clips are picked, as in a file manager: one, or several. Kept by the clips' keys, so that it
//! stays with them wherever an edit moves them, onto another track too, and back with an undo.

use crate::core::edit::{ClipId, ClipKey, Sequence};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    /// The picked clips, in the order they were picked: the last one is the main one. It has the
    /// handles on the timeline, and the Clip tab and the middle show it.
    picked: Vec<ClipKey>,
    /// The clip last clicked without Shift: a Shift+click picks from there.
    anchor: Option<ClipKey>,
}

impl Selection {
    /// The picked clips that are there, in the order they were picked: the main one last.
    pub fn ids(&self, sequence: &Sequence) -> Vec<ClipId> {
        sequence.find_all(&self.picked)
    }

    /// The main one: the last picked that is there.
    pub fn main(&self, sequence: &Sequence) -> Option<ClipId> {
        self.ids(sequence).pop()
    }

    /// The picked clips that are there, other than the main one.
    pub fn others(&self, sequence: &Sequence) -> Vec<ClipId> {
        let mut ids = self.ids(sequence);
        ids.pop();
        ids
    }

    /// Whether clip `id` is picked.
    pub fn contains(&self, sequence: &Sequence, id: ClipId) -> bool {
        sequence.has(id) && self.picked.contains(&sequence.clip(id).key())
    }

    /// Picks clip `id` alone, or nothing: a click.
    pub fn pick(&mut self, sequence: &Sequence, id: Option<ClipId>) {
        let key = id.filter(|&id| sequence.has(id)).map(|id| sequence.clip(id).key());
        self.picked = key.into_iter().collect();
        self.anchor = key;
    }

    /// Picks clips `ids`, the last of them the main one, as a range starting from the first.
    pub fn set(&mut self, sequence: &Sequence, ids: &[ClipId]) {
        self.picked = ids.iter().filter(|&&id| sequence.has(id)).map(|&id| sequence.clip(id).key()).collect();
        self.anchor = self.picked.first().copied();
    }

    /// Picks clip `id` besides the others, as the main one, or leaves it out if it was picked: a
    /// Ctrl+click. A Shift+click goes from it then.
    pub fn toggle(&mut self, sequence: &Sequence, id: ClipId) {
        if !sequence.has(id) {
            return;
        }
        let key = sequence.clip(id).key();
        match self.picked.iter().position(|&picked| picked == key) {
            Some(at) => {
                self.picked.remove(at);
            }
            None => self.picked.push(key),
        }
        self.anchor = Some(key);
    }

    /// Makes clip `id` the main one: with the others when it is picked, alone when it is not. What a
    /// press on a clip to drag it does, or to open its menu.
    pub fn focus(&mut self, sequence: &Sequence, id: ClipId) {
        if !sequence.has(id) {
            return;
        }
        let key = sequence.clip(id).key();
        match self.picked.iter().position(|&picked| picked == key) {
            Some(at) => {
                self.picked.remove(at);
                self.picked.push(key);
            }
            None => self.pick(sequence, Some(id)),
        }
    }

    /// Picks the clips from the one last clicked without Shift to clip `id`, see `Sequence::range`,
    /// `id` the main one: a Shift+click. With `add`, besides those picked already: a Ctrl+Shift+click.
    /// With nothing clicked before, from the main one, or `id` alone.
    pub fn extend_to(&mut self, sequence: &Sequence, id: ClipId, add: bool) {
        if !sequence.has(id) {
            return;
        }
        let from = self.anchor.and_then(|key| sequence.find(key)).or_else(|| self.main(sequence)).unwrap_or(id);
        let key = sequence.clip(id).key();
        let mut picked = if add { self.picked.clone() } else { Vec::new() };
        for other in sequence.range(from, id) {
            let other = sequence.clip(other).key();
            if other != key && !picked.contains(&other) {
                picked.push(other);
            }
        }
        picked.retain(|&other| other != key);
        picked.push(key);
        self.picked = picked;
        self.anchor = Some(sequence.clip(from).key());
    }

    /// Picks every clip: the main one stays so, if there is one.
    pub fn all(&mut self, sequence: &Sequence) {
        let main = self.main(sequence).map(|id| sequence.clip(id).key());
        let mut picked: Vec<ClipKey> = sequence.clips().map(|(_, clip)| clip.key()).filter(|&key| Some(key) != main).collect();
        picked.extend(main);
        self.anchor = self.anchor.or(picked.first().copied());
        self.picked = picked;
    }

    /// Picks nothing.
    pub fn clear(&mut self) {
        self.picked.clear();
        self.anchor = None;
    }

    /// Forgets the clips that are not there any more: removed, or gone with an undo.
    pub fn tidy(&mut self, sequence: &Sequence) {
        let there: Vec<ClipKey> = self.ids(sequence).iter().map(|&id| sequence.clip(id).key()).collect();
        self.picked = there;
        if self.anchor.is_some_and(|key| sequence.find(key).is_none()) {
            self.anchor = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::{Clip, Identity, TrackKind};

    /// Video 1 (track 1) with a 0..10, b 10..20, c 20..30; Audio 1 (track 0) with m 5..25 and n 25..28.
    fn two_tracks() -> Sequence {
        let mut sequence = Sequence::new(Clip { start: 5.0, ..Clip::new("m".into(), 20.0) });
        sequence.tracks[0].kind = TrackKind::Audio;
        sequence.tracks[0].clips.push(Clip { start: 25.0, ..Clip::new("n".into(), 3.0) });
        let video = sequence.add_track(1, TrackKind::Video);
        for (name, start) in [("a", 0.0), ("b", 10.0), ("c", 20.0)] {
            sequence.place_clip(Some(video), Clip { start, ..Clip::new(name.into(), 10.0) }).unwrap();
        }
        sequence
    }

    fn names(sequence: &Sequence, ids: &[ClipId]) -> Vec<String> {
        ids.iter().map(|&id| sequence.clip(id).name.clone()).collect()
    }

    fn id(sequence: &Sequence, name: &str) -> ClipId {
        sequence.clips().find(|(_, clip)| clip.name == name).unwrap().0
    }

    #[test]
    fn a_shift_click_picks_the_clips_between_on_one_track_and_those_in_the_time_between_on_several() {
        let sequence = two_tracks();
        let [a, c, m, n] = ["a", "c", "m", "n"].map(|name| id(&sequence, name));
        assert_eq!(names(&sequence, &sequence.range(a, c)), ["a", "b", "c"]);
        assert_eq!(names(&sequence, &sequence.range(c, a)), ["a", "b", "c"], "either way");
        // From a (0..10) on Video 1 to m (5..25) on Audio 1: what lies in 0..25 on both.
        assert_eq!(names(&sequence, &sequence.range(a, m)), ["m", "a", "b"]);
        // From n (25..28) to b (10..20): 10..28.
        assert_eq!(names(&sequence, &sequence.range(n, b_of(&sequence))), ["n", "b"]);

        let mut selection = Selection::default();
        selection.pick(&sequence, Some(a));
        selection.extend_to(&sequence, c, false);
        assert_eq!((names(&sequence, &selection.ids(&sequence)), selection.main(&sequence)), (vec!["a".into(), "b".into(), "c".into()], Some(c)));
        // Again from a, the anchor: to b only.
        selection.extend_to(&sequence, b_of(&sequence), false);
        assert_eq!(names(&sequence, &selection.ids(&sequence)), ["a", "b"]);
        // With Ctrl too: besides those picked.
        selection.toggle(&sequence, n);
        selection.extend_to(&sequence, m, true);
        assert_eq!(names(&sequence, &selection.ids(&sequence)), ["a", "b", "n", "m"]);
        assert_eq!(selection.main(&sequence), Some(m));
    }

    fn b_of(sequence: &Sequence) -> ClipId {
        id(sequence, "b")
    }

    #[test]
    fn clicks_with_ctrl_add_and_take_out_and_the_main_one_is_the_last_picked() {
        let sequence = two_tracks();
        let [a, b, m] = ["a", "b", "m"].map(|name| id(&sequence, name));
        let mut selection = Selection::default();
        selection.pick(&sequence, Some(a));
        selection.toggle(&sequence, m);
        selection.toggle(&sequence, b);
        assert_eq!((selection.ids(&sequence), selection.main(&sequence)), (vec![a, m, b], Some(b)));
        assert_eq!(selection.others(&sequence), [a, m]);
        // Out again: the one picked before it is the main one.
        selection.toggle(&sequence, b);
        assert_eq!((selection.main(&sequence), selection.contains(&sequence, b)), (Some(m), false));
        // A press on a picked one makes it the main one and keeps the others; on another, it alone.
        selection.focus(&sequence, a);
        assert_eq!(selection.ids(&sequence), [m, a]);
        selection.focus(&sequence, b);
        assert_eq!(selection.ids(&sequence), [b]);
        // All, the main one staying so; then none.
        selection.all(&sequence);
        assert_eq!((selection.ids(&sequence).len(), selection.main(&sequence)), (5, Some(b)));
        selection.clear();
        assert_eq!(selection.main(&sequence), None);
        // A click on nothing picks nothing.
        selection.pick(&sequence, Some(a));
        selection.pick(&sequence, None);
        assert!(selection.ids(&sequence).is_empty());
    }

    #[test]
    fn the_selection_stays_with_its_clips_wherever_edits_move_them() {
        let mut sequence = two_tracks();
        let [a, c, n] = ["a", "c", "n"].map(|name| id(&sequence, name));
        let mut selection = Selection::default();
        selection.set(&sequence, &[c, n, a]);
        let before = sequence.clone();
        // A track under them all, a clip before c on its track, a cut of a: the same clips.
        sequence.add_track(0, TrackKind::Video);
        let b = id(&sequence, "b");
        sequence.remove_clip(b);
        let a = id(&sequence, "a");
        sequence.split(a, 4.0, 1.0).unwrap();
        assert_eq!(names(&sequence, &selection.ids(&sequence)), ["c", "n", "a"], "the part after the cut is another clip");
        // Moved onto another track: still picked.
        let empty = sequence.add_track(sequence.tracks.len(), TrackKind::Video);
        let c = id(&sequence, "c");
        sequence.move_clip_to_track(c, empty, 20.0).unwrap();
        assert_eq!(selection.main(&sequence), Some(id(&sequence, "a")));
        assert!(selection.contains(&sequence, id(&sequence, "c")));
        // An undo brings them back as they were: the same clips again.
        sequence = before.clone();
        assert_eq!(names(&sequence, &selection.ids(&sequence)), ["c", "n", "a"]);
        // Removed: forgotten.
        sequence.remove_clip(id(&sequence, "n"));
        selection.tidy(&sequence);
        assert_eq!(names(&sequence, &selection.ids(&sequence)), ["c", "a"]);
        // A key is no part of what a clip is: another one, and the same clip all the same.
        let clip = sequence.clip(id(&sequence, "c"));
        let other = Clip { identity: Identity::default(), ..clip.clone() };
        assert!(other.key() != clip.key() && &other == clip);
    }
}

//! Undo and redo: snapshots of the edit, taken when it settles after a change.

const LIMIT: usize = 200;
/// A series of changes with shorter pauses, like turning the wheel, is one step.
const QUIET_SECONDS: f64 = 0.4;

pub struct History<T> {
    undo: Vec<T>,
    redo: Vec<T>,
    /// The state the last step ended with.
    saved: T,
    last_seen: T,
    changed_at: f64,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new(state: T) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), saved: state.clone(), last_seen: state, changed_at: 0.0 }
    }

    /// Call on every frame; `busy` means a gesture is still going on. Returns true while a change
    /// waits to become a step, so that the caller keeps frames coming.
    pub fn track(&mut self, state: &T, now: f64, busy: bool) -> bool {
        if *state != self.last_seen {
            self.last_seen = state.clone();
            self.changed_at = now;
        }
        if *state == self.saved {
            return false;
        }
        if busy || now - self.changed_at < QUIET_SECONDS {
            return true;
        }
        self.commit(state);
        false
    }

    fn commit(&mut self, state: &T) {
        if *state == self.saved {
            return;
        }
        self.undo.push(std::mem::replace(&mut self.saved, state.clone()));
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn can_undo(&self, state: &T) -> bool {
        !self.undo.is_empty() || *state != self.saved
    }

    pub fn can_redo(&self, state: &T) -> bool {
        !self.redo.is_empty() && *state == self.saved
    }

    /// Returns false if there is nothing to undo.
    pub fn undo(&mut self, state: &mut T) -> bool {
        self.commit(state);
        let Some(previous) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.saved, previous));
        self.restore(state);
        true
    }

    /// Returns false if there is nothing to redo: a change made after an undo drops the redo steps.
    pub fn redo(&mut self, state: &mut T) -> bool {
        self.commit(state);
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.saved, next));
        self.restore(state);
        true
    }

    /// Changes every state it keeps, for a fact that is not an edit: say, a source file turned out
    /// shorter. Undo then can't bring back what no longer fits.
    pub fn amend(&mut self, change: impl Fn(&mut T)) {
        for state in self.undo.iter_mut().chain(&mut self.redo) {
            change(state);
        }
        change(&mut self.saved);
        change(&mut self.last_seen);
    }

    fn restore(&mut self, state: &mut T) {
        *state = self.saved.clone();
        self.last_seen = self.saved.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A history that has seen `steps` settled changes, and the time after them.
    fn with_steps(steps: &[i32]) -> (History<i32>, i32, f64) {
        let mut history = History::new(0);
        let mut now = 0.0;
        for &step in steps {
            history.track(&step, now, false);
            now += 1.0;
            history.track(&step, now, false);
        }
        (history, *steps.last().unwrap_or(&0), now)
    }

    #[test]
    fn undoes_and_redoes_steps() {
        let (mut history, mut state, _) = with_steps(&[1, 2]);
        assert!(history.undo(&mut state));
        assert_eq!(state, 1);
        assert!(history.undo(&mut state));
        assert_eq!(state, 0);
        assert!(!history.undo(&mut state));
        assert!(history.redo(&mut state));
        assert!(history.redo(&mut state));
        assert_eq!(state, 2);
        assert!(!history.redo(&mut state));
    }

    #[test]
    fn a_step_brings_back_the_clip_and_the_export_range_together() {
        use crate::core::edit::{Clip, ClipId, Sequence};
        let id = ClipId { track: 0, index: 0 };
        let first = Sequence::new(Clip::new("a".into(), 30.0));
        let mut history = History::new(first.clone());
        let mut state = first.clone();

        // The trim takes the export range along, the move leaves it.
        state.trim_end(id, 8.0, 0.1);
        history.track(&state, 0.0, false);
        history.track(&state, 1.0, false);
        let trimmed = state.clone();
        state.move_clip(id, -4.0);
        history.track(&state, 2.0, false);
        history.track(&state, 3.0, false);
        let moved = state.clone();

        assert!(history.undo(&mut state));
        assert_eq!(state, trimmed);
        assert!(history.undo(&mut state));
        assert_eq!(state, first);
        assert!(state.export_fits());
        assert!(history.redo(&mut state));
        assert!(history.redo(&mut state));
        assert_eq!(state, moved);
    }

    #[test]
    fn a_quick_series_of_changes_is_one_step() {
        let mut history = History::new(0);
        for (i, now) in [0.0, 0.1, 0.2, 0.3].into_iter().enumerate() {
            assert!(history.track(&(i as i32 + 1), now, false));
        }
        assert!(!history.track(&4, 1.0, false));
        let mut state = 4;
        assert!(history.undo(&mut state));
        assert_eq!(state, 0);
    }

    #[test]
    fn waits_for_the_gesture_to_end() {
        let mut history = History::new(0);
        assert!(history.track(&5, 0.0, true));
        assert!(history.track(&5, 10.0, true));
        assert!(!history.track(&5, 10.0, false));
        let mut state = 5;
        assert!(history.undo(&mut state));
        assert_eq!(state, 0);
    }

    #[test]
    fn undo_takes_a_change_that_has_not_settled_yet() {
        let (mut history, _, now) = with_steps(&[1]);
        let mut state = 2;
        assert!(history.track(&state, now, false));
        assert!(history.can_undo(&state));
        assert!(history.undo(&mut state));
        assert_eq!(state, 1);
        assert!(history.redo(&mut state));
        assert_eq!(state, 2);
    }

    #[test]
    fn a_new_change_drops_the_redo_steps() {
        let (mut history, mut state, now) = with_steps(&[1, 2]);
        history.undo(&mut state);
        assert!(history.can_redo(&state));
        state = 7;
        assert!(!history.can_redo(&state));
        history.track(&state, now, false);
        history.track(&state, now + 1.0, false);
        assert!(!history.redo(&mut state));
        assert_eq!(state, 7);
    }

    #[test]
    fn keeps_a_limited_number_of_steps() {
        let steps: Vec<i32> = (1..=LIMIT as i32 + 5).collect();
        let (mut history, mut state, _) = with_steps(&steps);
        let mut undone = 0;
        while history.undo(&mut state) {
            undone += 1;
        }
        assert_eq!(undone, LIMIT);
        assert_eq!(state, 5);
    }

    #[test]
    fn an_amendment_is_not_a_step_and_reaches_every_step() {
        let (mut history, mut state, now) = with_steps(&[1, 2]);
        let add_ten = |value: &mut i32| *value += 10;
        history.amend(add_ten);
        add_ten(&mut state);
        assert!(!history.track(&state, now + 1.0, false));
        assert!(history.undo(&mut state));
        assert_eq!(state, 11);
        assert!(history.undo(&mut state));
        assert_eq!(state, 10);
        assert!(history.redo(&mut state));
        assert!(history.redo(&mut state));
        assert_eq!(state, 12);
    }

    #[test]
    fn going_back_to_the_saved_state_is_not_a_step() {
        let mut history = History::new(0);
        assert!(history.track(&1, 0.0, false));
        assert!(!history.track(&0, 0.1, false));
        let mut state = 0;
        assert!(!history.undo(&mut state));
    }
}

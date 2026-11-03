//! General undo/redo. Every timeline change becomes a step once the gesture
//! that made it ends, whichever path made it: pointer edits, Details, the
//! console, chat or local intents. A drag, a slider move or a burst of
//! typing is one step, not one per frame.

use openreel_engine::{ClipId, Timeline};

/// Steps kept. Each holds a timeline clone (clips are small; no media).
const LIMIT: usize = 200;
/// Typing into a focused field settles into a step after this pause.
const SETTLE_SECONDS: f64 = 0.75;

#[derive(Clone)]
struct State {
    timeline: Timeline,
    selected: Option<ClipId>,
}

pub(super) struct History {
    committed: State,
    /// The last differing timeline seen, and when it last changed.
    latest: Option<(Timeline, f64)>,
    undo: Vec<State>,
    redo: Vec<State>,
}

impl History {
    pub(super) fn new(timeline: &Timeline) -> Self {
        Self {
            committed: State {
                timeline: timeline.clone(),
                selected: None,
            },
            latest: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Forget every step; the current timeline becomes the baseline.
    pub(super) fn reset(&mut self, timeline: &Timeline) {
        *self = Self::new(timeline);
    }

    /// Call once per frame. `pointer_down` and `typing` hold a change open;
    /// typing settles after a pause. Returns whether a change is still open.
    pub(super) fn track(
        &mut self,
        timeline: &Timeline,
        selected: Option<ClipId>,
        now: f64,
        pointer_down: bool,
        typing: bool,
    ) -> bool {
        if *timeline == self.committed.timeline {
            self.latest = None;
            self.committed.selected = selected;
            return false;
        }
        match &mut self.latest {
            Some((latest, at)) if latest == timeline => {
                if pointer_down || (typing && now - *at < SETTLE_SECONDS) {
                    return true;
                }
            }
            _ => {
                self.latest = Some((timeline.clone(), now));
                if pointer_down || typing {
                    return true;
                }
            }
        }
        self.commit(timeline, selected);
        false
    }

    fn commit(&mut self, timeline: &Timeline, selected: Option<ClipId>) {
        let before = std::mem::replace(
            &mut self.committed,
            State {
                timeline: timeline.clone(),
                selected,
            },
        );
        self.undo.push(before);
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.latest = None;
    }

    /// Step back. An open change is committed first so it is what undoes.
    pub(super) fn undo(&mut self, timeline: &mut Timeline, selected: &mut Option<ClipId>) -> bool {
        if *timeline != self.committed.timeline {
            self.commit(timeline, *selected);
        }
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.committed, previous);
        self.redo.push(current);
        self.apply(timeline, selected);
        true
    }

    pub(super) fn redo(&mut self, timeline: &mut Timeline, selected: &mut Option<ClipId>) -> bool {
        if *timeline != self.committed.timeline {
            // A new edit since the undo replaced the redo branch.
            self.commit(timeline, *selected);
            return false;
        }
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.committed, next);
        self.undo.push(current);
        self.apply(timeline, selected);
        true
    }

    fn apply(&mut self, timeline: &mut Timeline, selected: &mut Option<ClipId>) {
        *timeline = self.committed.timeline.clone();
        *selected = self
            .committed
            .selected
            .filter(|id| timeline.clip(*id).is_some());
        self.latest = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openreel_engine::ClipKind;

    fn timeline_with(starts: &[f64]) -> Timeline {
        let mut timeline = Timeline::new(30.0);
        for start in starts {
            timeline.add_clip("t", ClipKind::Title, 0, *start, 1.0, 0.0);
        }
        timeline
    }

    #[test]
    fn a_drag_is_one_step_and_redo_survives_until_a_new_edit() {
        let mut timeline = timeline_with(&[0.0]);
        let mut history = History::new(&timeline);
        let id = timeline.clips[0].id;
        let mut selected = Some(id);
        assert!(!history.track(&timeline, selected, 0.0, false, false));
        // A drag moves the clip on several frames while the pointer is down.
        for (frame, start) in [1.0, 2.0, 3.0].into_iter().enumerate() {
            timeline.clips[0].start = start;
            assert!(history.track(&timeline, selected, frame as f64 * 0.016, true, false));
        }
        assert!(!history.track(&timeline, selected, 0.1, false, false));
        timeline.clips[0].start = 5.0;
        history.track(&timeline, selected, 0.2, false, false);

        assert!(history.undo(&mut timeline, &mut selected));
        assert_eq!(
            timeline.clips[0].start, 3.0,
            "undo one release, not one frame"
        );
        assert!(history.undo(&mut timeline, &mut selected));
        assert_eq!(timeline.clips[0].start, 0.0);
        assert_eq!(selected, Some(id));
        assert!(!history.undo(&mut timeline, &mut selected));

        assert!(history.redo(&mut timeline, &mut selected));
        assert_eq!(timeline.clips[0].start, 3.0);
        timeline.clips[0].start = 9.0;
        assert!(
            !history.redo(&mut timeline, &mut selected),
            "a new edit drops redo"
        );
        assert_eq!(timeline.clips[0].start, 9.0);
        assert!(history.undo(&mut timeline, &mut selected));
        assert_eq!(timeline.clips[0].start, 3.0);
    }

    #[test]
    fn typing_settles_into_one_step_and_open_edits_undo_first() {
        let mut timeline = timeline_with(&[0.0]);
        let mut history = History::new(&timeline);
        let mut selected = None;
        for (index, name) in ["a", "ab", "abc"].into_iter().enumerate() {
            timeline.clips[0].name = name.to_owned();
            assert!(history.track(&timeline, None, index as f64 * 0.2, false, true));
        }
        assert!(
            history.track(&timeline, None, 0.9, false, true),
            "still typing"
        );
        assert!(!history.track(&timeline, None, 1.2, false, true), "paused");
        timeline.clips[0].name = "abcd".to_owned();
        // Undo before the next pause still steps back over the open edit.
        assert!(history.undo(&mut timeline, &mut selected));
        assert_eq!(timeline.clips[0].name, "abc");
        assert!(history.undo(&mut timeline, &mut selected));
        assert_eq!(timeline.clips[0].name, "t");
    }

    #[test]
    fn a_deleted_selection_is_not_restored_onto_a_missing_clip() {
        let mut timeline = timeline_with(&[0.0, 2.0]);
        let mut history = History::new(&timeline);
        let kept = timeline.clips[0].id;
        timeline.add_clip("new", ClipKind::Title, 0, 4.0, 1.0, 0.0);
        let added = timeline.clips[2].id;
        history.track(&timeline, Some(added), 0.0, false, false);
        let mut selected = Some(added);
        history.undo(&mut timeline, &mut selected);
        assert_eq!(timeline.clips.len(), 2);
        assert_eq!(selected, None, "the added clip is gone");
        history.redo(&mut timeline, &mut selected);
        assert_eq!(selected, Some(added));
        assert!(timeline.clip(kept).is_some());
    }
}

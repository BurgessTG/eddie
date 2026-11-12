//! Lanes: titles, video and sound each have their own, shown in that
//! order from the top. A clip's `track` counts within its kind's lanes, so
//! a lane is a kind and a number, and a clip never shares one with a clip
//! of another kind.

use super::Timeline;
use crate::{Clip, ClipId, ClipKind};

/// The kinds in the order their lanes are shown, top to bottom.
pub const LANE_KINDS: [ClipKind; 3] = [ClipKind::Title, ClipKind::Video, ClipKind::Audio];

/// How many lanes each kind has. Every kind has at least one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lanes {
    pub titles: usize,
    pub video: usize,
    pub audio: usize,
}

impl Lanes {
    pub fn count(&self, kind: ClipKind) -> usize {
        match kind {
            ClipKind::Title => self.titles,
            ClipKind::Video => self.video,
            ClipKind::Audio => self.audio,
        }
    }

    fn count_mut(&mut self, kind: ClipKind) -> &mut usize {
        match kind {
            ClipKind::Title => &mut self.titles,
            ClipKind::Video => &mut self.video,
            ClipKind::Audio => &mut self.audio,
        }
    }

    /// Every lane, top to bottom.
    pub fn len(&self) -> usize {
        self.titles + self.video + self.audio
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Clip {
    /// Whether two clips sit on one lane: the same kind and number.
    pub fn same_lane(&self, other: &Clip) -> bool {
        self.kind == other.kind && self.track == other.track
    }
}

impl Timeline {
    /// The row lane `track` of `kind` is drawn on, counted from the top.
    pub fn row(&self, kind: ClipKind, track: usize) -> usize {
        LANE_KINDS
            .iter()
            .take_while(|k| **k != kind)
            .map(|k| self.lanes.count(*k))
            .sum::<usize>()
            + track
    }

    /// The lane drawn on `row`, as its kind and number.
    pub fn lane_at_row(&self, row: usize) -> Option<(ClipKind, usize)> {
        let mut first = 0;
        for kind in LANE_KINDS {
            let count = self.lanes.count(kind);
            if row < first + count {
                return Some((kind, row - first));
            }
            first += count;
        }
        None
    }

    /// "Video 2": what a lane's header says.
    pub fn lane_label(kind: ClipKind, track: usize) -> String {
        let name = match kind {
            ClipKind::Title => "Titles",
            ClipKind::Video => "Video",
            ClipKind::Audio => "Audio",
        };
        format!("{name} {}", track + 1)
    }

    /// Add a lane of `kind`, above its others when `first` (clips of that
    /// kind move one lane down) or below them. Returns its number.
    pub fn add_lane(&mut self, kind: ClipKind, first: bool) -> usize {
        let count = self.lanes.count_mut(kind);
        *count += 1;
        if !first {
            return *count - 1;
        }
        for clip in self.clips.iter_mut().filter(|clip| clip.kind == kind) {
            clip.track += 1;
        }
        0
    }

    /// Grow `kind`'s lanes until lane `track` exists.
    pub fn grow_lanes(&mut self, kind: ClipKind, track: usize) {
        let count = self.lanes.count_mut(kind);
        *count = (*count).max(track + 1);
    }

    /// The first lane of `kind` with nothing on it from `start` to `end`.
    pub fn first_free_lane(&self, kind: ClipKind, start: f64, end: f64) -> Option<usize> {
        let taken = |track: usize| {
            self.clips.iter().any(|clip| {
                clip.kind == kind && clip.track == track && clip.start < end && start < clip.end()
            })
        };
        (0..self.lanes.count(kind)).find(|track| !taken(*track))
    }

    /// The first lane of `kind` with nothing on it from `start` to `end`,
    /// or a new one below the others.
    pub fn free_lane(&mut self, kind: ClipKind, start: f64, end: f64) -> usize {
        match self.first_free_lane(kind, start, end) {
            Some(track) => track,
            None => self.add_lane(kind, false),
        }
    }

    /// Where a clip of `kind` dropped on `row` lands: that lane when it is
    /// one of `kind`'s, else the first free from `start` to `end`. `None`
    /// means a new lane below the others.
    pub fn drop_lane(&self, kind: ClipKind, row: usize, start: f64, end: f64) -> Option<usize> {
        match self.lane_at_row(row) {
            Some((lane_kind, track)) if lane_kind == kind => Some(track),
            _ => self.first_free_lane(kind, start, end),
        }
    }

    /// Move a clip to another lane of its kind. False, and nothing moves,
    /// when its kind has no such lane.
    pub fn set_clip_track(&mut self, id: ClipId, track: usize) -> bool {
        let Some(kind) = self.clip(id).map(|clip| clip.kind) else {
            return false;
        };
        if track >= self.lanes.count(kind) {
            return false;
        }
        if let Some(clip) = self.clip_mut(id) {
            clip.track = track;
        }
        true
    }

    /// Every kind has a lane, and a lane for each of its clips. A timeline
    /// from a saved project or a test may have neither.
    pub fn ensure_lanes(&mut self) {
        for kind in LANE_KINDS {
            let needed = self
                .clips
                .iter()
                .filter(|clip| clip.kind == kind)
                .map(|clip| clip.track + 1)
                .max()
                .unwrap_or(1);
            self.grow_lanes(kind, needed - 1);
        }
    }

    /// Remove the lanes nothing is on, keeping one of each kind that had
    /// one. Clips keep their lanes' order, so the video that shows where two
    /// overlap still shows. Also reads a version 1 project, whose lanes held
    /// any kind and whose `track` counted every lane (`ensure_lanes` then
    /// adds the kinds it lacks).
    pub fn drop_empty_lanes(&mut self) {
        let had = self.lanes;
        for kind in LANE_KINDS {
            let mut used: Vec<usize> = self
                .clips
                .iter()
                .filter(|clip| clip.kind == kind)
                .map(|clip| clip.track)
                .collect();
            used.sort_unstable();
            used.dedup();
            for clip in self.clips.iter_mut().filter(|clip| clip.kind == kind) {
                clip.track = used.binary_search(&clip.track).unwrap_or(0);
            }
            *self.lanes.count_mut(kind) = used.len().max(had.count(kind).min(1));
        }
    }
}

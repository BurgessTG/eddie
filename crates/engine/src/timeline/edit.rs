//! Moving, trimming, splitting and closing up clips.

use super::{Timeline, EDGE_EPSILON};
use crate::{Clip, ClipId, ClipKind};

impl Timeline {
    /// Move a clip. Start is clamped to >= 0 and snapped to a frame.
    /// A linked detach partner moves by the same amount.
    pub fn move_clip(&mut self, id: ClipId, new_start: f64) {
        let Some(original) = self.clip(id).cloned() else {
            return;
        };
        if !new_start.is_finite() {
            return;
        }
        let snapped = self.snap(new_start.max(0.0));
        let delta = snapped - original.start;
        if let Some(clip) = self.clip_mut(id) {
            clip.start = snapped;
        }
        if delta != 0.0 {
            if let Some(linked) = self.linked_id(id) {
                if let Some(companion) = self.clip(linked).cloned() {
                    let companion_start = self.snap((companion.start + delta).max(0.0));
                    if let Some(clip) = self.clip_mut(linked) {
                        clip.start = companion_start;
                    }
                }
            }
        }
    }

    /// Swap two non-overlapping clips on one lane, including their detached
    /// partners, atomically. Adjacent clips keep their occupied outer span
    /// and gap; non-adjacent clips exchange start positions only if they fit.
    /// This reorders existing intervals without rounding source durations or
    /// their derived boundary. Ordinary free moves still use `move_clip`.
    /// Any collision on any affected lane leaves the whole timeline unchanged.
    pub fn swap_clips(&mut self, first: ClipId, second: ClipId) -> bool {
        let (Some(first), Some(second)) = (self.clip(first), self.clip(second)) else {
            return false;
        };
        if first.id == second.id || !first.same_lane(second) {
            return false;
        }
        let (left, right) = if first.start <= second.start {
            (first, second)
        } else {
            (second, first)
        };
        const EPSILON: f64 = 1e-9;
        if left.end() > right.start + EPSILON {
            return false;
        }
        let adjacent = !self.clips.iter().any(|clip| {
            clip.id != left.id
                && clip.id != right.id
                && clip.same_lane(left)
                && clip.start < right.start
                && clip.end() > left.end() + EPSILON
        });
        let left_start = if adjacent {
            right.end() - left.duration
        } else {
            right.start
        };
        let mut placements = Vec::with_capacity(4);
        for (clip, new_start) in [(left, left_start), (right, left.start)] {
            placements.push((clip.id, new_start));
            if let Some(companion) = self.linked_id(clip.id).and_then(|id| self.clip(id)) {
                // Swapping members of the same linked group would move one
                // clip twice and cannot preserve their relative placement.
                if companion.id == left.id || companion.id == right.id {
                    return false;
                }
                placements.push((companion.id, companion.start + (new_start - clip.start)));
            }
        }
        for (index, &(id, start)) in placements.iter().enumerate() {
            if !start.is_finite()
                || start < 0.0
                || placements[..index].iter().any(|(other, _)| *other == id)
            {
                return false;
            }
            let clip = self.clip(id).unwrap();
            let blocked = self.clips.iter().any(|other| {
                if other.id == id || !other.same_lane(clip) {
                    return false;
                }
                let other_start = placements
                    .iter()
                    .find(|(moved, _)| *moved == other.id)
                    .map_or(other.start, |(_, new_start)| *new_start);
                start < other_start + other.duration - EPSILON
                    && other_start < start + clip.duration - EPSILON
            });
            if blocked {
                return false;
            }
        }
        for (id, start) in placements {
            self.clip_mut(id).unwrap().start = start;
        }
        true
    }

    fn trim_left_one(&mut self, id: ClipId, new_start: f64) {
        let frame = 1.0 / self.fps;
        let Some(clip) = self.clip(id).cloned() else {
            return;
        };
        if !new_start.is_finite() {
            return;
        }
        // Generated clips have no media to run out of: a title's edge can
        // move earlier, and its source clock goes negative so word timings
        // stay where they were.
        let generated = clip.kind == ClipKind::Title;
        let min_start = if generated {
            0.0
        } else {
            (clip.start - clip.source_in).max(0.0)
        };
        let max_start = clip.end() - frame;
        // The request is a bound the caller may have clamped to a neighbour
        // or the media: the frame grid never moves the edge before it.
        let snapped = self.snap_at_or_after(new_start);
        // Clamp after snapping, so an off-grid media start stays exact.
        let desired = snapped.clamp(min_start, max_start.max(min_start));
        if new_start <= clip.start + EDGE_EPSILON && desired > clip.start {
            // Extending must not shorten a clip whose edge is off the grid.
            return;
        }
        let delta = desired - clip.start;
        if let Some(clip) = self.clip_mut(id) {
            clip.start = desired;
            clip.source_in += delta;
            if !generated {
                clip.source_in = clip.source_in.max(0.0);
            }
            clip.duration = (clip.duration - delta).max(frame);
        }
    }

    /// Trim the left edge to `new_start`, keeping the end fixed. The edge
    /// lands on the frame grid at or after `new_start`, never before it.
    /// `source_in` moves with the edge and clamps at 0 (start of media).
    /// A linked detach partner receives the same edge movement.
    pub fn trim_left(&mut self, id: ClipId, new_start: f64) {
        let Some(original) = self.clip(id).cloned() else {
            return;
        };
        self.trim_left_one(id, new_start);
        let Some(updated) = self.clip(id).cloned() else {
            return;
        };
        let delta = updated.start - original.start;
        if delta != 0.0 {
            if let Some(linked) = self.linked_id(id) {
                if let Some(companion) = self.clip(linked).cloned() {
                    self.trim_left_one(linked, companion.start + delta);
                }
            }
        }
    }

    fn trim_right_one(&mut self, id: ClipId, new_end: f64) {
        if !new_end.is_finite() {
            return;
        }
        let frame = 1.0 / self.fps;
        // The caller clamped `new_end` to the media or the next clip, so the
        // frame grid never moves the edge past it.
        let snapped = self.snap_at_or_before(new_end);
        if let Some(clip) = self.clip_mut(id) {
            if new_end >= clip.end() - EDGE_EPSILON && snapped < clip.end() {
                // Extending must not shorten a clip whose edge is off the grid.
                return;
            }
            clip.duration = (snapped - clip.start).max(frame);
        }
    }

    /// Trim the right edge to `new_end`: the frame at or before it, never
    /// after. The caller clamps against the source media duration and the
    /// next clip; the model only enforces one frame minimum.
    /// A linked detach partner receives the same edge movement.
    pub fn trim_right(&mut self, id: ClipId, new_end: f64) {
        let Some(original) = self.clip(id).cloned() else {
            return;
        };
        self.trim_right_one(id, new_end);
        let Some(updated) = self.clip(id).cloned() else {
            return;
        };
        let delta = updated.end() - original.end();
        if delta != 0.0 {
            if let Some(linked) = self.linked_id(id) {
                if let Some(companion) = self.clip(linked).cloned() {
                    self.trim_right_one(linked, companion.end() + delta);
                }
            }
        }
    }

    fn split_one(&mut self, id: ClipId, time: f64) -> Option<ClipId> {
        let at = self.snap(time);
        let clip = self.clip(id)?.clone();
        let frame = 1.0 / self.fps;
        if at <= clip.start + frame / 2.0 || at >= clip.end() - frame / 2.0 {
            return None;
        }
        let left_duration = at - clip.start;
        if let Some(left) = self.clip_mut(id) {
            left.duration = left_duration;
            // The head fade stays left, the tail fade moves right.
            left.fade_out = 0.0;
        }
        let right = self.add_clip_inner(
            &clip.name,
            clip.kind,
            clip.track,
            at,
            clip.end() - at,
            clip.source_in + left_duration,
            clip.source.as_deref(),
        );
        if let Some(right_clip) = self.clip_mut(right) {
            // A title extended earlier has a negative source clock; adding
            // a clip clamps it, which would shift the word timing.
            right_clip.source_in = clip.source_in + left_duration;
            right_clip.opacity = clip.opacity;
            right_clip.volume = clip.volume;
            right_clip.embedded_audio = clip.embedded_audio;
            right_clip.fade_out = clip.fade_out;
            right_clip.linked_clip = None;
            right_clip.title_style = clip.title_style;
            right_clip.duck = clip.duck;
            right_clip.clean_voice = clip.clean_voice;
            right_clip.smooth_fades = clip.smooth_fades;
            right_clip.words = clip.words.clone();
        }
        Some(right)
    }

    /// Split a clip at `time`. Returns the new (right-hand) clip id.
    /// No-op (`None`) if the point is not strictly inside the clip.
    /// A linked detach partner splits at the same timeline point
    /// and the two right-hand pieces stay linked to each other.
    pub fn split_at(&mut self, id: ClipId, time: f64) -> Option<ClipId> {
        let linked = self.linked_id(id);
        let right = self.split_one(id, time)?;
        if let Some(companion) = linked {
            if let Some(right_companion) = self.split_one(companion, time) {
                self.link_pair(right, right_companion);
            }
        }
        Some(right)
    }

    /// Open `seconds` of empty time at `at`: every clip that crosses the
    /// point splits there (linked pairs together, titles keep their text),
    /// and everything from the point on, on every lane, moves later by the
    /// same amount, so pictures, sound and captions stay in sync.
    pub fn insert_time(&mut self, at: f64, seconds: f64) {
        let at = self.snap(at.max(0.0));
        let seconds = self.snap(seconds);
        if seconds <= 0.0 {
            return;
        }
        let crossing: Vec<ClipId> = self
            .clips
            .iter()
            .filter(|clip| clip.start < at && at < clip.end())
            .map(|clip| clip.id)
            .collect();
        for id in crossing {
            // A linked partner may already have split with its pair.
            if self.clip(id).is_some_and(|clip| at < clip.end()) {
                self.split_at(id, at);
            }
        }
        let frame = 1.0 / self.fps;
        for clip in &mut self.clips {
            if clip.start >= at - frame / 2.0 {
                clip.start += seconds;
            }
        }
    }

    /// Magnetic main-track behavior: shift clips on `track` left so no
    /// gaps remain between them. The first clip keeps its start.
    /// Remove timeline time that no video or audio clip covers, after the
    /// first such clip. Every clip after a gap, on every lane, moves left by
    /// the same amount, so titles, captions and linked or layered media stay
    /// in sync with the pictures they sit over. A title inside a removed gap
    /// moves to the gap's start; a title that would then share its lane's
    /// time with another clip moves to a free lane instead.
    pub fn close_gaps(&mut self) {
        let mut covered: Vec<(f64, f64)> = self
            .clips
            .iter()
            .filter(|c| c.kind != ClipKind::Title)
            .map(|c| (c.start, c.end()))
            .collect();
        covered.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps: Vec<(f64, f64)> = Vec::new();
        let mut reach: Option<f64> = None;
        for (start, end) in covered {
            match reach {
                Some(at) if start > at => {
                    gaps.push((at, start));
                    reach = Some(end);
                }
                Some(at) => reach = Some(at.max(end)),
                None => reach = Some(end),
            }
        }
        for clip in &mut self.clips {
            let shift: f64 = gaps
                .iter()
                .map(|&(a, b)| (clip.start.min(b) - a).max(0.0))
                .sum();
            clip.start -= shift;
        }
        // Media keeps its spacing, so only a title can now sit on another
        // title: one from a removed gap, or one overhanging it. The later
        // title keeps its time and moves to a title lane free there.
        let mut titles: Vec<(f64, ClipId)> = self
            .clips
            .iter()
            .filter(|c| c.kind == ClipKind::Title)
            .map(|c| (c.start, c.id))
            .collect();
        titles.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (start, id) in titles {
            let Some(title) = self.clip(id).cloned() else {
                continue;
            };
            let covers = |c: &Clip, track: usize| {
                c.id != id
                    && c.kind == ClipKind::Title
                    && c.track == track
                    && c.start < title.end() - EDGE_EPSILON
                    && title.start < c.end() - EDGE_EPSILON
            };
            let stays = |c: &Clip| (c.start, c.id) < (start, id);
            if !self
                .clips
                .iter()
                .any(|c| covers(c, title.track) && stays(c))
            {
                continue;
            }
            let free =
                (0..self.lanes.titles).find(|&track| !self.clips.iter().any(|c| covers(c, track)));
            let track = free.unwrap_or_else(|| self.add_lane(ClipKind::Title, false));
            if let Some(clip) = self.clip_mut(id) {
                clip.track = track;
            }
        }
    }
}

//! Moving, trimming, splitting and closing up clips.

use super::{Timeline, EDGE_EPSILON};
use crate::{Clip, ClipId, ClipKind, MAX_SPEED, MIN_SPEED};

/// What [`Timeline::ripple_delete`] took with the clip and left in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ripple {
    /// Titles that lay wholly over the deleted picture.
    pub titles: usize,
    /// Lanes with clips after the gap that kept their place, because a
    /// clip on them runs into the deleted time.
    pub lanes_kept: usize,
}

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
        // stay where they were. A still has no source range to run out of.
        let generated = clip.kind == ClipKind::Title;
        let min_start = if generated || clip.still() {
            0.0
        } else {
            clip.time_at(0.0).max(0.0)
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
            clip.keyframes.shift(-delta);
            if !clip.still() {
                clip.source_in += delta * clip.speed;
            }
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
            clip.source_at(at),
            clip.source.as_deref(),
        );
        let mut keyframes = clip.keyframes.clone();
        keyframes.shift(-left_duration);
        if let Some(right_clip) = self.clip_mut(right) {
            // Everything else carries over: look, sound, speed, words and
            // keys, which stay on the moments they were on. A
            // title extended earlier keeps its negative source clock, which
            // adding a clip would clamp and so shift its word timing.
            *right_clip = Clip {
                keyframes,
                id: right,
                start: at,
                duration: clip.end() - at,
                source_in: clip.source_at(at),
                fade_in: 0.0,
                linked_clip: None,
                // The cut it leaves has no transition.
                transition: None,
                ..clip.clone()
            };
        }
        Some(right)
    }

    /// Play a media clip at `speed` source seconds per second. It keeps its
    /// source range, so its length becomes that range at the new speed; a
    /// linked partner changes with it. False, and nothing changes, for a
    /// title, a still, a clip without a source or a speed outside
    /// [`MIN_SPEED`]..=[`MAX_SPEED`].
    pub fn set_speed(&mut self, id: ClipId, speed: f64) -> bool {
        let media = self.clip(id).is_some_and(|clip| {
            clip.kind != ClipKind::Title && clip.source.is_some() && !clip.still()
        });
        if !media || !(MIN_SPEED..=MAX_SPEED).contains(&speed) {
            return false;
        }
        for id in [Some(id), self.linked_id(id)].into_iter().flatten() {
            if let Some(clip) = self.clip_mut(id) {
                clip.duration = clip.duration * clip.speed / speed;
                clip.keyframes.stretch(clip.speed / speed);
                clip.speed = speed;
            }
        }
        true
    }

    /// How long a clip may last from its start before it runs into the next
    /// clip on its lane, or its linked partner into one on its own lane.
    pub fn room_after_start(&self, id: ClipId) -> f64 {
        let pair = [Some(id), self.linked_id(id)];
        pair.iter()
            .flatten()
            .filter_map(|&id| self.clip(id))
            .flat_map(|clip| {
                self.clips
                    .iter()
                    .filter(move |other| {
                        other.same_lane(clip)
                            && !pair.contains(&Some(other.id))
                            && other.start >= clip.start - EDGE_EPSILON
                    })
                    .map(move |other| other.start - clip.start)
            })
            .fold(f64::INFINITY, f64::min)
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
        self.push_later(at, seconds);
    }

    /// Move every clip that starts at `at` or later, on every lane, later
    /// by `seconds`. Clips that run across `at` stay where they are.
    pub fn push_later(&mut self, at: f64, seconds: f64) {
        let frame = 1.0 / self.fps;
        for clip in &mut self.clips {
            if clip.start >= at - frame / 2.0 {
                clip.start += seconds;
            }
        }
    }

    /// Hold the picture video clip `id` shows at `time` for `seconds`, as
    /// CapCut's Freeze does: the clip and its linked sound split at that
    /// frame, a freeze frame of it fills the new time on the clip's lane,
    /// and every clip that starts from there on, on every lane, moves later
    /// by `seconds` so pictures, sound and titles after it stay together.
    /// Clips on other lanes that run across the point stay whole, so a
    /// music bed plays on under the hold. Returns the freeze frame. `None`,
    /// and nothing changes, unless the clip is video from a file, not
    /// already a still, and `time` is inside it, more than half a frame
    /// before its end.
    pub fn freeze_frame(&mut self, id: ClipId, time: f64, seconds: f64) -> Option<ClipId> {
        let clip = self.clip(id)?.clone();
        let frame = 1.0 / self.fps;
        if clip.kind != ClipKind::Video
            || clip.source.is_none()
            || clip.still()
            || !clip.contains(time)
            || !seconds.is_finite()
        {
            return None;
        }
        // The frame on screen at `time` starts at the grid line before it.
        let mut at = self.snap_at_or_before(time).max(clip.start);
        if at > clip.end() - frame / 2.0 {
            return None;
        }
        if at > clip.start + frame / 2.0 {
            self.split_at(id, at)?;
        } else {
            at = clip.start;
        }
        let picture = clip.source_at(at);
        // The held picture keeps the place and opacity it had there, still.
        let (transform, opacity) = (clip.transform_at(at), clip.opacity_at(at));
        let seconds = self.snap(seconds).max(frame);
        self.push_later(at, seconds);
        let freeze = self.next_id;
        self.next_id += 1;
        self.clips.push(Clip {
            id: freeze,
            start: at,
            duration: seconds,
            source_in: picture,
            embedded_audio: false,
            fade_in: 0.0,
            fade_out: 0.0,
            linked_clip: None,
            duck: false,
            clean_voice: false,
            smooth_fades: false,
            speed: 1.0,
            freeze: true,
            transition: None,
            transform,
            opacity,
            keyframes: crate::Keyframes::default(),
            ..clip
        });
        Some(freeze)
    }

    /// Delete clip `id` with its linked partner and close the time they
    /// leave: on every lane, clips that start at or after its end move left
    /// by its length, so pictures, sound and titles after it stay together.
    /// A deleted picture takes the titles lying wholly over its time with
    /// it and cuts that time out of titles across its edges, unless other
    /// video still shows there. A lane where a clip that stays still runs
    /// into that time, such as a music bed, keeps its place. `None` when
    /// there is no clip `id`.
    pub fn ripple_delete(&mut self, id: ClipId) -> Option<Ripple> {
        let clip = self.clip(id)?.clone();
        let (start, end) = (clip.start, clip.end());
        let slack = 0.5 / self.fps;
        let mut gone: Vec<ClipId> = [Some(id), self.linked_id(id)]
            .into_iter()
            .flatten()
            .collect();
        // Overlaps under half a frame come from off-grid media edges.
        let shown_elsewhere = self.clips.iter().any(|other| {
            other.kind == ClipKind::Video
                && !gone.contains(&other.id)
                && other.start < end - slack
                && start < other.end() - slack
        });
        let partners = gone.len();
        let closes_titles = clip.kind == ClipKind::Video && !shown_elsewhere;
        if closes_titles {
            gone.extend(
                self.clips
                    .iter()
                    .filter(|title| {
                        title.kind == ClipKind::Title
                            && title.start >= start - slack
                            && title.end() <= end + slack
                    })
                    .map(|title| title.id),
            );
        }
        let titles = gone.len() - partners;
        for id in gone {
            self.remove_clip(id);
        }
        let later = |other: &Clip| other.start >= end - slack;
        let crosses = |other: &Clip| !later(other) && other.end() > start + slack;
        let mut held: Vec<(ClipKind, usize)> = self
            .clips
            .iter()
            .filter(|other| crosses(other) && !(closes_titles && other.kind == ClipKind::Title))
            .map(|other| (other.kind, other.track))
            .collect();
        held.sort_by_key(|&(kind, track)| self.row(kind, track));
        held.dedup();
        for other in &mut self.clips {
            if closes_titles && other.kind == ClipKind::Title && crosses(other) {
                // A title across an edge of the deleted picture loses the
                // part over it, so the captions after it stay in step.
                let before = (start - other.start).max(0.0);
                let after = (other.end() - end).max(0.0);
                if other.start > start {
                    other.source_in += end - other.start;
                    other.start = start;
                }
                other.duration = before + after;
            } else if later(other) && !held.contains(&(other.kind, other.track)) {
                other.start = (other.start - (end - start)).max(0.0);
            }
        }
        let lanes_kept = held
            .iter()
            .filter(|lane| {
                self.clips
                    .iter()
                    .any(|other| later(other) && (other.kind, other.track) == **lane)
            })
            .count();
        Some(Ripple { titles, lanes_kept })
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

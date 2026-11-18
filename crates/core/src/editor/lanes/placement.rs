//! Where a moved clip lands: its neighbours on the lane, a stop flush
//! against one it runs into, and a swap with one it mostly covers.

use openreel_engine::{Clip, ClipId, Timeline};

/// End of the nearest same-track clip ending at or before `orig_start`.
pub(super) fn prev_end(timeline: &Timeline, clip: &Clip, orig_start: f64) -> f64 {
    timeline
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.same_lane(clip) && c.end() <= orig_start + 1e-6)
        .map(Clip::end)
        .fold(0.0, f64::max)
}

/// Start of the nearest same-track clip starting at or after `from`.
pub(super) fn next_start(timeline: &Timeline, clip: &Clip, from: f64) -> Option<f64> {
    timeline
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.same_lane(clip) && c.start >= from - 1e-6)
        .map(|c| c.start)
        .min_by(|a, b| a.total_cmp(b))
}

fn lane_overlaps(timeline: &Timeline, clip: &Clip) -> Vec<ClipId> {
    timeline
        .clips
        .iter()
        .filter(|c| {
            c.id != clip.id
                && c.same_lane(clip)
                && c.start < clip.end() - 1e-9
                && clip.start < c.end() - 1e-9
        })
        .map(|c| c.id)
        .collect()
}

pub(super) fn linked_lane_clear(timeline: &Timeline, id: ClipId) -> bool {
    timeline
        .clip(id)
        .and_then(|clip| clip.linked_clip)
        .and_then(|linked| timeline.clip(linked))
        .is_none_or(|linked| {
            // The dragged clip can be temporarily on its partner's lane
            // before a drop creates a new lane for it.
            lane_overlaps(timeline, linked)
                .into_iter()
                .all(|other| other == id)
        })
}

/// Put a clip's start exactly at `start` and move its linked partner by
/// the same amount. A magnet target or a neighbour's edge is already where
/// the clip must meet it: rounding the start to the frame grid would leave
/// up to half a frame between them, or across the edge, which a drop then
/// read as landing on the neighbour.
pub(super) fn place_exact(timeline: &mut Timeline, id: ClipId, start: f64) {
    let Some(clip) = timeline.clip_mut(id) else {
        return;
    };
    let delta = start - clip.start;
    clip.start = start;
    let linked = clip.linked_clip;
    if let Some(partner) = linked.and_then(|linked| timeline.clip_mut(linked)) {
        partner.start += delta;
    }
}

/// Whether a dragged clip covers enough of `other` to mean "put me in
/// its place": half the shorter of the two. Less is a collision.
fn covers_for_swap(clip: &Clip, other: &Clip) -> bool {
    let overlap = clip.end().min(other.end()) - clip.start.max(other.start);
    overlap >= clip.duration.min(other.duration) / 2.0 - 1e-9
}

/// A dragged clip that runs a little way into one clip on its lane stops
/// flush against it, on the side the clip came from. False, and nothing
/// moved, when there is no such collision or the flush place is taken.
pub(super) fn stop_against(timeline: &mut Timeline, id: ClipId) -> bool {
    let Some(clip) = timeline.clip(id).cloned() else {
        return false;
    };
    let [other] = lane_overlaps(timeline, &clip)[..] else {
        return false;
    };
    let Some(other) = timeline.clip(other).cloned() else {
        return false;
    };
    if covers_for_swap(&clip, &other) {
        return false;
    }
    let start = if clip.start + clip.duration / 2.0 < other.start + other.duration / 2.0 {
        other.start - clip.duration
    } else {
        other.end()
    };
    if start < 0.0 {
        return false;
    }
    place_exact(timeline, id, start);
    let placed = timeline.clip(id).cloned();
    if placed.is_some_and(|placed| lane_overlaps(timeline, &placed).is_empty()) {
        return true;
    }
    place_exact(timeline, id, clip.start);
    false
}

pub(super) fn restore_move(
    timeline: &mut Timeline,
    id: ClipId,
    home_start: f64,
    home_track: usize,
    home_linked: Option<(ClipId, f64)>,
) {
    // Rollback is not a new frame-grid edit. Imported starts can be
    // fractional, so calling move_clip(home_start) would change them.
    if let Some(clip) = timeline.clip_mut(id) {
        clip.start = home_start;
        clip.track = home_track;
    }
    if let Some((linked, start)) = home_linked {
        if let Some(clip) = timeline.clip_mut(linked) {
            clip.start = start;
        }
    }
}

pub(super) fn resolve_move_drop(
    timeline: &mut Timeline,
    id: ClipId,
    home_start: f64,
    home_track: usize,
    home_linked: Option<(ClipId, f64)>,
    drop_time: f64,
) {
    let Some(clip) = timeline.clip(id).cloned() else {
        return;
    };
    let overlaps = lane_overlaps(timeline, &clip);
    if overlaps.is_empty() && linked_lane_clear(timeline, id) {
        return;
    }
    if stop_against(timeline, id) && linked_lane_clear(timeline, id) {
        return;
    }
    // Attempt a swap from the original positions, not the overlapping
    // drag preview. The engine validates all linked placements before
    // mutating anything; rejection leaves the restored home untouched.
    restore_move(timeline, id, home_start, home_track, home_linked);
    if clip.track != home_track {
        return;
    }
    let partner = if let [other] = overlaps[..] {
        timeline
            .clip(other)
            .filter(|other| covers_for_swap(&clip, other))
            .map(|other| other.id)
    } else {
        // Centering a long clip on a short neighbor can make the drag
        // preview extend over a third clip. The pointer disambiguates
        // only an immediate neighbor, never a multi-clip ripple.
        overlaps.into_iter().find(|other| {
            let partner = timeline.clip(*other).unwrap();
            if !partner.contains(drop_time) {
                return false;
            }
            let left_end = if home_start < partner.start {
                home_start + clip.duration
            } else {
                partner.end()
            };
            let right_start = home_start.max(partner.start);
            !timeline.clips.iter().any(|between| {
                between.id != id
                    && between.id != partner.id
                    && between.kind == clip.kind
                    && between.track == home_track
                    && between.start < right_start - 1e-9
                    && between.end() > left_end + 1e-9
            })
        })
    };
    if let Some(partner) = partner {
        timeline.swap_clips(id, partner);
    }
}

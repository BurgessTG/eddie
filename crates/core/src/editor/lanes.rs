//! The timeline lanes as a window draws them: geometry, hit-testing and
//! pointer gestures, independent of the window toolkit. Pointer placement
//! uses the engine's project frame grid; reorders and rollbacks retain
//! exact existing intervals.

use openreel_engine::{Clip, ClipId, Timeline};

use super::viewport::TimelineViewport;

/// Track headers left of the lanes.
pub const HEADER_W: f32 = 144.0;
pub const RULER_H: f32 = 26.0;
pub const TRACK_H: f32 = 46.0;
pub const TRACK_GAP: f32 = 4.0;
/// Width of the trim handle at each clip edge.
pub const EDGE_PX: f32 = 9.0;
/// The part of a clip that always moves it; handles shrink first.
pub const MOVE_BODY_PX: f32 = 12.0;
/// Held this close to either end of the lanes, a drag scrolls them.
const AUTOSCROLL_MARGIN: f32 = 36.0;
const AUTOSCROLL_SPEED: f32 = 14.0;

/// `[x, y]` in the window's points.
pub type Point = [f32; 2];

/// A box in the window's points. For the whole view it includes the track
/// headers on the left and the ruler on top.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl ViewRect {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    pub fn contains(&self, [x, y]: Point) -> bool {
        (self.left..=self.right).contains(&x) && (self.top..=self.bottom).contains(&y)
    }

    /// Where time zero sits when nothing is scrolled.
    pub fn content_left(&self) -> f32 {
        self.left + HEADER_W
    }

    pub fn lanes_top(&self) -> f32 {
        self.top + RULER_H
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipHit {
    Body,
    Left,
    Right,
}

#[derive(Clone, Copy)]
enum DragMode {
    Move {
        grab_offset: f64,
        home_start: f64,
        home_track: usize,
        home_linked: Option<(ClipId, f64)>,
    },
    TrimLeft {
        orig_start: f64,
        press_time: f64,
    },
    TrimRight {
        orig_end: f64,
        press_time: f64,
    },
}

/// One filmstrip fragment: destination offset/width inside the clip
/// rect, the horizontal UV span of its thumbnail, and the source
/// time at its center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilmstripTile {
    pub x: f32,
    pub w: f32,
    pub uv: (f32, f32),
    pub center_source: f64,
}

/// Lay out filmstrip fragments on a grid anchored to source time
/// zero, not to the clip's left edge. Tile width in seconds comes
/// from the zoom (`tile_w / px_per_sec`), so zooming changes frame
/// density, while trimming slides the clip border across fragments
/// that stay put: an edge tile is UV-cropped, never re-phased.
pub fn filmstrip_tiles(
    source_in: f64,
    duration: f64,
    px_per_sec: f32,
    tile_w: f32,
) -> Vec<FilmstripTile> {
    if px_per_sec <= 0.0 || tile_w <= 0.0 || duration <= 0.0 {
        return Vec::new();
    }
    let tile_sec = (tile_w / px_per_sec) as f64;
    let clip_w = (duration * px_per_sec as f64) as f32;
    let first_k = (source_in / tile_sec).floor() as i64;
    let mut tiles = Vec::new();
    let mut k = first_k;
    loop {
        let tile_source_start = k as f64 * tile_sec;
        let x = ((tile_source_start - source_in) * px_per_sec as f64) as f32;
        if x >= clip_w {
            break;
        }
        let visible_left = x.max(0.0);
        let visible_right = (x + tile_w).min(clip_w);
        if visible_right > visible_left {
            tiles.push(FilmstripTile {
                x: visible_left,
                w: visible_right - visible_left,
                uv: ((visible_left - x) / tile_w, (visible_right - x) / tile_w),
                center_source: tile_source_start + tile_sec / 2.0,
            });
        }
        k += 1;
    }
    tiles
}

/// An absent target must not compete with a real target as a zero-distance snap.
fn snap_target(time: f64, candidates: &[f64], threshold: f64) -> Option<f64> {
    candidates
        .iter()
        .copied()
        .filter(|c| (c - time).abs() <= threshold)
        .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
}

/// Snap `time` to the nearest candidate within `threshold` seconds.
pub fn snap_to(time: f64, candidates: &[f64], threshold: f64) -> f64 {
    snap_target(time, candidates, threshold).unwrap_or(time)
}

/// Seconds between labelled ruler ticks: the first step at least 72
/// points wide. Three minor ticks sit between each pair.
pub fn ruler_step(px_per_sec: f32) -> f64 {
    const STEPS: [f64; 13] = [
        0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
    ];
    // Past an hour the step doubles: a long timeline zoomed out to fit
    // still draws a handful of ticks, not thousands.
    let hours = (1..=30).map(|doublings| 3600.0 * f64::from(1_u32 << doublings));
    STEPS
        .into_iter()
        .chain(hours)
        .find(|step| *step as f32 * px_per_sec >= 72.0)
        .unwrap_or(3600.0 * f64::from(1_u32 << 30))
}

/// Ruler label: `M:SS`, or `H:MM:SS` from an hour on, with hundredths
/// only when the step needs them.
pub fn ruler_label(t: f64, step: f64) -> String {
    let whole_minutes = (t / 60.0).floor() as u64;
    let seconds = t - whole_minutes as f64 * 60.0;
    let minutes = match (whole_minutes / 60, whole_minutes % 60) {
        (0, minutes) => minutes.to_string(),
        (hours, minutes) => format!("{hours}:{minutes:02}"),
    };
    if step < 1.0 {
        let text = format!("{minutes}:{seconds:05.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        format!("{minutes}:{:02}", seconds.round() as u64)
    }
}

pub fn x_at(port: &TimelineViewport, content_left: f32, time: f64) -> f32 {
    content_left + time as f32 * port.px_per_sec - port.scroll_x
}

pub fn time_at(port: &TimelineViewport, content_left: f32, x: f32) -> f64 {
    ((x - content_left + port.scroll_x) / port.px_per_sec) as f64
}

/// The lane at height `y`, counting on past the last one; `None` over the
/// ruler.
pub fn track_at(rect: ViewRect, y: f32) -> Option<usize> {
    let lanes_top = rect.lanes_top();
    if y < lanes_top {
        return None;
    }
    Some(((y - lanes_top) / (TRACK_H + TRACK_GAP)) as usize)
}

/// A lane's box right of the headers. Works one past the last lane too,
/// where a drop appends one.
pub fn lane_rect(rect: ViewRect, track: usize) -> ViewRect {
    let top = rect.lanes_top() + track as f32 * (TRACK_H + TRACK_GAP);
    ViewRect {
        left: rect.content_left(),
        top,
        right: rect.right.max(rect.content_left()),
        bottom: top + TRACK_H,
    }
}

pub fn clip_rect(port: &TimelineViewport, rect: ViewRect, clip: &Clip) -> ViewRect {
    let content_left = rect.content_left();
    let top = rect.lanes_top() + clip.track as f32 * (TRACK_H + TRACK_GAP) + 3.0;
    ViewRect {
        left: x_at(port, content_left, clip.start),
        top,
        right: x_at(port, content_left, clip.end()),
        bottom: top + TRACK_H - 6.0,
    }
}

/// The lane and time under a point, for something dropped there from
/// outside. `None` outside the view, over the headers or over the ruler.
pub fn drop_target(port: &TimelineViewport, rect: ViewRect, pos: Point) -> Option<(usize, f64)> {
    if !rect.contains(pos) || pos[0] < rect.content_left() {
        return None;
    }
    let track = track_at(rect, pos[1])?;
    let time = time_at(port, rect.content_left(), pos[0]).max(0.0);
    Some((track, time))
}

/// Paint, hover and press classification share the same geometry. Handles
/// shrink before taking away the move body; tiny clips are entirely body
/// until zoomed in. An adjacent clip's outside handle cannot steal it.
pub fn hit_clip<'a>(
    port: &TimelineViewport,
    timeline: &'a Timeline,
    rect: ViewRect,
    pos: Point,
) -> Option<(&'a Clip, ClipHit)> {
    let [x, y] = pos;
    let track = track_at(rect, y)?;
    let mut outside_edge: Option<(f32, &Clip, ClipHit)> = None;
    for clip in timeline.clips.iter().filter(|c| c.track == track) {
        let bounds = clip_rect(port, rect, clip);
        if !(bounds.top..=bounds.bottom).contains(&y) {
            continue;
        }
        let handle = EDGE_PX.min(((bounds.width() - MOVE_BODY_PX) * 0.5).max(0.0));
        let left_dist = (x - bounds.left).abs();
        let right_dist = (x - bounds.right).abs();
        if x >= bounds.left && x < bounds.right {
            let hit = if handle > 0.0 && left_dist <= handle {
                ClipHit::Left
            } else if handle > 0.0 && right_dist <= handle {
                ClipHit::Right
            } else {
                ClipHit::Body
            };
            return Some((clip, hit));
        }
        for (distance, hit) in [(left_dist, ClipHit::Left), (right_dist, ClipHit::Right)] {
            if handle > 0.0
                && distance <= handle
                && outside_edge.is_none_or(|(best, _, _)| distance < best)
            {
                outside_edge = Some((distance, clip, hit));
            }
        }
    }
    outside_edge.map(|(_, clip, hit)| (clip, hit))
}

/// The clip under a point the pointer rests on, for hover looks and the
/// cursor. `None` outside the view and deep in the header gutter.
pub fn hover_clip<'a>(
    port: &TimelineViewport,
    timeline: &'a Timeline,
    rect: ViewRect,
    pos: Point,
) -> Option<(&'a Clip, ClipHit)> {
    if !rect.contains(pos) || pos[0] < rect.content_left() - EDGE_PX {
        return None;
    }
    hit_clip(port, timeline, rect, pos)
}

/// Keep the scroll inside what the timeline holds: its end can come back
/// to the middle of the lanes, no further.
pub fn clamp_scroll(port: &mut TimelineViewport, end: f64, lanes_width: f32) {
    let max_scroll = (end as f32 * port.px_per_sec - lanes_width * 0.5).max(0.0);
    port.scroll_x = port.scroll_x.min(max_scroll);
}

/// A plain wheel or two-finger scroll over the lanes: both axes scroll
/// along time. Positive deltas move content right, as window toolkits
/// report them.
pub fn scroll_by(port: &mut TimelineViewport, dx: f32, dy: f32) {
    port.scroll_x = (port.scroll_x - dy - dx).max(0.0);
}

/// Zoom by `factor`, keeping the time under window x `pointer_x` where it
/// is. `end` is the timeline's, which sets how far out it goes.
pub fn zoom_about(
    port: &mut TimelineViewport,
    content_left: f32,
    pointer_x: f32,
    factor: f32,
    end: f64,
) {
    let anchor_time = time_at(port, content_left, pointer_x);
    port.set_zoom(port.px_per_sec * factor, end);
    port.scroll_x = (anchor_time as f32 * port.px_per_sec - (pointer_x - content_left)).max(0.0);
}

/// End of the nearest same-track clip ending at or before `orig_start`.
fn prev_end(timeline: &Timeline, clip: &Clip, orig_start: f64) -> f64 {
    timeline
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.track == clip.track && c.end() <= orig_start + 1e-6)
        .map(Clip::end)
        .fold(0.0, f64::max)
}

/// Start of the nearest same-track clip starting at or after `from`.
fn next_start(timeline: &Timeline, clip: &Clip, from: f64) -> Option<f64> {
    timeline
        .clips
        .iter()
        .filter(|c| c.id != clip.id && c.track == clip.track && c.start >= from - 1e-6)
        .map(|c| c.start)
        .min_by(|a, b| a.total_cmp(b))
}

fn lane_overlaps(timeline: &Timeline, clip: &Clip) -> Vec<ClipId> {
    timeline
        .clips
        .iter()
        .filter(|c| {
            c.id != clip.id
                && c.track == clip.track
                && c.start < clip.end() - 1e-9
                && clip.start < c.end() - 1e-9
        })
        .map(|c| c.id)
        .collect()
}

fn linked_lane_clear(timeline: &Timeline, id: ClipId) -> bool {
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

fn restore_move(
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

fn resolve_move_drop(
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
    // Attempt a swap from the original positions, not the overlapping
    // drag preview. The engine validates all linked placements before
    // mutating anything; rejection leaves the restored home untouched.
    restore_move(timeline, id, home_start, home_track, home_linked);
    if clip.track != home_track {
        return;
    }
    let partner = if overlaps.len() == 1 {
        Some(overlaps[0])
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

/// What a gesture reads and changes.
pub struct Scene<'a> {
    pub timeline: &'a mut Timeline,
    pub playhead: &'a mut f64,
    pub selected: &'a mut Option<ClipId>,
    pub port: &'a mut TimelineViewport,
    /// The whole view, headers and ruler included.
    pub rect: ViewRect,
}

/// One held pointer over the timeline: a ruler scrub, or a clip being
/// moved or trimmed. The window decides what is a click and what a drag
/// and reports each; everything a gesture means is decided here.
#[derive(Default)]
pub struct Gesture {
    drag: Option<(ClipId, DragMode)>,
    seeking: bool,
}

impl Gesture {
    /// True while the ruler is being scrubbed or a clip dragged.
    pub fn is_interacting(&self) -> bool {
        self.seeking || self.drag.is_some()
    }

    /// The clip being moved or trimmed.
    pub fn dragged_clip(&self) -> Option<ClipId> {
        self.drag.map(|(id, _)| id)
    }

    /// Whether a press there starts anything. Presses deep in the
    /// track-header gutter do not, but the strip of gutter next to time
    /// zero does: a full-length clip's left edge lives there.
    pub fn accepts(rect: ViewRect, press: Point) -> bool {
        press[0] >= rect.content_left() - EDGE_PX
    }

    /// A press released where it began. On the ruler it moves the
    /// playhead to `pos`; on the lanes it parks the playhead at the press
    /// and selects the clip there, or clears the selection.
    pub fn click(&mut self, scene: &mut Scene<'_>, press: Point, pos: Point) {
        let content_left = scene.rect.content_left();
        if press[1] < scene.rect.lanes_top() {
            let time = time_at(scene.port, content_left, pos[0]).max(0.0);
            *scene.playhead = scene.timeline.snap(time);
            return;
        }
        let press_time = time_at(scene.port, content_left, press[0]).max(0.0);
        *scene.playhead = scene.timeline.snap(press_time);
        *scene.selected =
            hit_clip(scene.port, scene.timeline, scene.rect, press).map(|(clip, _)| clip.id);
    }

    /// The pointer left its press point with the button held. What it
    /// does is classified from where the press began, not from where the
    /// pointer is once the window calls it a drag: using the current
    /// position misclassifies most edge grabs as moves. A drag keeps the
    /// playhead where it is instead of snapping short clips to their own
    /// grab point, especially on a purely vertical move.
    pub fn begin(&mut self, scene: &mut Scene<'_>, press: Point, pos: Point) {
        let content_left = scene.rect.content_left();
        if press[1] < scene.rect.lanes_top() {
            self.seeking = true;
            let time = time_at(scene.port, content_left, pos[0]).max(0.0);
            *scene.playhead = scene.timeline.snap(time);
            return;
        }
        let press_time = time_at(scene.port, content_left, press[0]).max(0.0);
        let Some((clip, hit)) = hit_clip(scene.port, scene.timeline, scene.rect, press) else {
            return;
        };
        let mode = match hit {
            ClipHit::Body => DragMode::Move {
                grab_offset: press_time - clip.start,
                home_start: clip.start,
                home_track: clip.track,
                home_linked: clip
                    .linked_clip
                    .and_then(|id| scene.timeline.clip(id))
                    .map(|linked| (linked.id, linked.start)),
            },
            ClipHit::Left => DragMode::TrimLeft {
                orig_start: clip.start,
                press_time,
            },
            ClipHit::Right => DragMode::TrimRight {
                orig_end: clip.end(),
                press_time,
            },
        };
        *scene.selected = Some(clip.id);
        self.drag = Some((clip.id, mode));
    }

    /// Scroll while a gesture is held near either end of the lanes, so
    /// the far edge of a long clip can be reached without letting go.
    /// True when it scrolled: call again next frame, pointer still or not.
    pub fn autoscroll(&self, port: &mut TimelineViewport, rect: ViewRect, pos: Point) -> bool {
        if !self.is_interacting() {
            return false;
        }
        if pos[0] > rect.right - AUTOSCROLL_MARGIN {
            port.scroll_x += AUTOSCROLL_SPEED;
            true
        } else if pos[0] < rect.content_left() + AUTOSCROLL_MARGIN && pos[0] >= rect.left {
            port.scroll_x = (port.scroll_x - AUTOSCROLL_SPEED).max(0.0);
            true
        } else {
            false
        }
    }

    /// Follow the held pointer to `pos`. `source_duration` gives a media
    /// file's length, which a right trim cannot pass.
    pub fn drag(
        &mut self,
        scene: &mut Scene<'_>,
        pos: Point,
        source_duration: &dyn Fn(&str) -> Option<f64>,
    ) {
        let time = time_at(scene.port, scene.rect.content_left(), pos[0]).max(0.0);
        let timeline = &mut *scene.timeline;
        if self.seeking {
            *scene.playhead = timeline.snap(time);
            return;
        }
        let Some((id, mode)) = self.drag else {
            return;
        };
        // Snap targets (magnet on): timeline start, playhead, other
        // clip edges. Magnet off means free placement on frame grid.
        let threshold = 8.0 / scene.port.px_per_sec as f64;
        let mut candidates: Vec<f64> = Vec::new();
        if scene.port.magnet {
            candidates.extend([0.0, *scene.playhead]);
            let linked = timeline.clip(id).and_then(|c| c.linked_clip);
            for clip in timeline
                .clips
                .iter()
                .filter(|c| c.id != id && Some(c.id) != linked)
            {
                candidates.push(clip.start);
                candidates.push(clip.end());
            }
        }
        let Some(clip) = timeline.clip(id).cloned() else {
            return;
        };
        match mode {
            DragMode::Move { grab_offset, .. } => {
                // Vertical lane change within existing lanes;
                // growth past the top or bottom lane resolves
                // on drop (finish). Horizontal overlap is
                // allowed mid-drag so the clip can be carried
                // over a neighbor; finish swaps or
                // returns it home, never leaving an overlap.
                if let Some(target) =
                    track_at(scene.rect, pos[1]).filter(|t| *t < timeline.track_labels.len())
                {
                    if target != clip.track {
                        timeline.set_clip_track(id, target);
                    }
                }
                let raw = time - grab_offset;
                let by_start = snap_target(raw, &candidates, threshold);
                let by_end = snap_target(raw + clip.duration, &candidates, threshold)
                    .map(|end| end - clip.duration);
                let snapped = [by_start, by_end]
                    .into_iter()
                    .flatten()
                    .min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs()))
                    .unwrap_or(raw);
                timeline.move_clip(id, snapped);
            }
            DragMode::TrimLeft {
                orig_start,
                press_time,
            } => {
                // Move the edge by the drag delta. Setting the edge
                // to the pointer time would instantly cut away the
                // grab offset inside the edge handle.
                let desired = orig_start + (time - press_time);
                // The pointer takes the nearest frame; only a bound
                // (the previous clip) makes the model round one way.
                let snapped = timeline.snap(snap_to(desired, &candidates, threshold));
                let prev_end = prev_end(timeline, &clip, orig_start);
                timeline.trim_left(id, snapped.max(prev_end));
            }
            DragMode::TrimRight {
                orig_end,
                press_time,
            } => {
                let desired = orig_end + (time - press_time);
                let mut new_end = timeline.snap(snap_to(desired, &candidates, threshold));
                if let Some(duration) = clip.source.as_deref().and_then(source_duration) {
                    let max_end = clip.start + (duration - clip.source_in).max(0.0);
                    new_end = new_end.min(max_end);
                }
                if let Some(next) = next_start(timeline, &clip, clip.start) {
                    new_end = new_end.min(next);
                }
                timeline.trim_right(id, new_end);
            }
        }
    }

    /// End the gesture at `drop_pos`; `None` (the pointer was lost or the
    /// gesture cancelled) returns a moved clip home. The magnet only
    /// snaps while dragging; gaps are never closed here. A Move dropped
    /// above the first lane or below the last grows a new lane there for
    /// the clip. A Move dropped onto one neighbor on its home lane swaps
    /// positions with it. A wider preview may overlap more clips when the
    /// pointer clearly targets an adjacent neighbor; only that pair
    /// reorders. Ambiguous or colliding drops return home without moving
    /// other clips.
    pub fn finish(&mut self, scene: &mut Scene<'_>, drop_pos: Option<Point>) {
        let timeline = &mut *scene.timeline;
        let rect = scene.rect;
        if let Some((
            id,
            DragMode::Move {
                home_start,
                home_track,
                home_linked,
                ..
            },
        )) = self.drag
        {
            let lanes_top = rect.lanes_top();
            let lanes_bottom =
                lanes_top + timeline.track_labels.len() as f32 * (TRACK_H + TRACK_GAP) - TRACK_GAP;
            // Well past the edge counts as aiming for a new lane, so
            // slight pointer drift on a sideways drag of an edge-lane
            // clip does not grow one by accident. The ruler strip is
            // the only room above the first lane, hence the smaller
            // top threshold.
            let above = drop_pos.is_some_and(|pos| pos[1] < lanes_top - RULER_H * 0.5);
            let below = drop_pos.is_some_and(|pos| pos[1] > lanes_bottom + TRACK_H * 0.5);
            if drop_pos.is_none() || ((above || below) && !linked_lane_clear(timeline, id)) {
                restore_move(timeline, id, home_start, home_track, home_linked);
            } else if above {
                timeline.insert_track_at(0);
                timeline.set_clip_track(id, 0);
            } else if below {
                let track = timeline.append_track();
                timeline.set_clip_track(id, track);
            } else if let Some(pos) = drop_pos {
                resolve_move_drop(
                    timeline,
                    id,
                    home_start,
                    home_track,
                    home_linked,
                    time_at(scene.port, rect.content_left(), pos[0]),
                );
            }
        }
        self.drag = None;
        self.seeking = false;
    }
}

#[cfg(test)]
mod tests;

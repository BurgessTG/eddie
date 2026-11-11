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
mod tests {
    use openreel_engine::ClipKind;

    use super::*;

    const VIEW: ViewRect = ViewRect {
        left: 0.0,
        top: 0.0,
        right: 1200.0,
        bottom: 400.0,
    };

    struct Stage {
        timeline: Timeline,
        playhead: f64,
        selected: Option<ClipId>,
        port: TimelineViewport,
        gesture: Gesture,
    }

    impl Stage {
        fn new(timeline: Timeline) -> Self {
            Self {
                timeline,
                playhead: 0.0,
                selected: None,
                port: TimelineViewport::default(),
                gesture: Gesture::default(),
            }
        }

        fn scene(&mut self) -> (Scene<'_>, &mut Gesture) {
            (
                Scene {
                    timeline: &mut self.timeline,
                    playhead: &mut self.playhead,
                    selected: &mut self.selected,
                    port: &mut self.port,
                    rect: VIEW,
                },
                &mut self.gesture,
            )
        }

        fn point(&self, time: f64, track: usize) -> Point {
            [
                x_at(&self.port, VIEW.content_left(), time),
                VIEW.lanes_top() + track as f32 * (TRACK_H + TRACK_GAP) + TRACK_H / 2.0,
            ]
        }

        fn x(&self, time: f64) -> f32 {
            x_at(&self.port, VIEW.content_left(), time)
        }

        fn center(&self, id: ClipId) -> Point {
            let clip = self.timeline.clip(id).unwrap();
            self.point(clip.start + clip.duration / 2.0, clip.track)
        }

        /// A press at `from` released at `to`; the selection it leaves.
        fn drag(&mut self, from: Point, to: Point) -> Option<ClipId> {
            self.drag_through(from, &[to]);
            self.selected
        }

        /// A press that visits each point of `path` and is released at
        /// the last, as a window reports it: the gesture begins at the
        /// first move, and the release repeats the final position.
        fn drag_through(&mut self, press: Point, path: &[Point]) {
            let (mut scene, gesture) = self.scene();
            gesture.begin(&mut scene, press, path[0]);
            for &pos in path {
                gesture.drag(&mut scene, pos, &|_| None);
            }
            let last = path[path.len() - 1];
            gesture.drag(&mut scene, last, &|_| None);
            gesture.finish(&mut scene, Some(last));
            assert!(!gesture.is_interacting());
        }

        fn click(&mut self, at: Point) {
            let (mut scene, gesture) = self.scene();
            gesture.click(&mut scene, at, at);
        }
    }

    #[test]
    fn snaps_to_nearest_candidate_within_threshold() {
        let candidates = [0.0, 2.0, 5.0];
        assert_eq!(snap_to(1.9, &candidates, 0.25), 2.0);
        assert_eq!(snap_to(5.2, &candidates, 0.25), 5.0);
    }

    #[test]
    fn filmstrip_tiles_anchor_to_source_time_not_clip_edge() {
        // 48px/s, 72px tiles -> one fragment per 1.5s of source.
        let tiles = filmstrip_tiles(3.5, 6.0, 48.0, 72.0);
        // First fragment covers source 3.0..4.5: only its right part
        // is inside the clip, UV-cropped instead of re-phased.
        let first = tiles[0];
        assert_eq!(first.x, 0.0);
        assert!((first.w - 48.0).abs() < 0.01, "w {}", first.w);
        assert!((first.uv.0 - 1.0 / 3.0).abs() < 0.01, "uv0 {}", first.uv.0);
        assert!((first.uv.1 - 1.0).abs() < 0.01);
        assert!((first.center_source - 3.75).abs() < 1e-9);
        // Fragments are equally spaced and cover the clip width.
        for pair in tiles.windows(2) {
            assert!((pair[1].x - (pair[0].x + pair[0].w)).abs() < 0.01);
        }
        let last = tiles[tiles.len() - 1];
        assert!((last.x + last.w - 288.0).abs() < 0.01);

        // Trimming the head by 0.5s must not re-phase later
        // fragments: the fragment covering source 6.75 keeps its
        // center and UV span, only its clip-local x shifts.
        let trimmed = filmstrip_tiles(4.0, 5.5, 48.0, 72.0);
        let before = tiles
            .iter()
            .find(|t| (t.center_source - 6.75).abs() < 1e-9)
            .expect("fragment at 6.75 before trim");
        let after = trimmed
            .iter()
            .find(|t| (t.center_source - 6.75).abs() < 1e-9)
            .expect("fragment at 6.75 after trim");
        assert_eq!(before.uv, after.uv);
        assert!((before.x - after.x - 24.0).abs() < 0.01);
    }

    #[test]
    fn does_not_snap_beyond_threshold() {
        let candidates = [0.0, 2.0, 5.0];
        assert_eq!(snap_to(3.0, &candidates, 0.25), 3.0);
    }

    #[test]
    fn a_click_parks_the_playhead_and_selects_what_is_under_it() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 3.0, 0.0, "a");
        let mut stage = Stage::new(timeline);
        let on_clip = stage.point(3.0, 0);
        let (mut scene, gesture) = stage.scene();
        gesture.click(&mut scene, on_clip, on_clip);
        assert_eq!(stage.selected, Some(id));
        assert_eq!(stage.playhead, 3.0);
        assert_eq!(stage.timeline.clip(id).unwrap().start, 2.0);

        let empty = stage.point(9.0, 0);
        let (mut scene, gesture) = stage.scene();
        gesture.click(&mut scene, empty, empty);
        assert_eq!(stage.selected, None);
        assert_eq!(stage.playhead, 9.0);
    }

    #[test]
    fn a_ruler_drag_scrubs_and_leaves_clips_and_selection_alone() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("a", ClipKind::Video, 0, 0.0, 3.0, 0.0, "a");
        let mut stage = Stage::new(timeline);
        let before = stage.timeline.clone();
        let x = |stage: &Stage, time| x_at(&stage.port, VIEW.content_left(), time);
        let (press, to) = ([x(&stage, 1.0), 8.0], [x(&stage, 2.5), 120.0]);
        let (mut scene, gesture) = stage.scene();
        gesture.begin(&mut scene, press, press);
        assert!(gesture.is_interacting());
        assert_eq!(gesture.dragged_clip(), None);
        gesture.drag(&mut scene, to, &|_| None);
        gesture.finish(&mut scene, Some(to));
        assert_eq!(stage.playhead, 2.5);
        assert_eq!(stage.selected, None);
        assert_eq!(stage.timeline, before);
        assert!(stage.timeline.clip(id).is_some());
    }

    #[test]
    fn a_body_drag_moves_and_an_edge_drag_trims_from_where_the_press_began() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 4.0, 1.0, "a");
        let mut stage = Stage::new(timeline);
        stage.port.magnet = false;

        // The press sits inside the right handle; the pointer has already
        // travelled into the body when the drag is recognised.
        let press = [stage.point(6.0, 0)[0] - 3.0, stage.point(6.0, 0)[1]];
        let to = [press[0] - 48.0, press[1]];
        stage.drag(press, to);
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!((clip.start, clip.duration, clip.source_in), (2.0, 3.0, 1.0));

        let from = stage.point(3.5, 0);
        stage.drag(from, [from[0] + 96.0, from[1]]);
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!((clip.start, clip.duration, clip.source_in), (4.0, 3.0, 1.0));
        assert_eq!(stage.selected, Some(id));
        assert_eq!(stage.playhead, 0.0, "a drag does not move the playhead");
    }

    #[test]
    fn a_right_trim_stops_at_the_end_of_the_source() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("a", ClipKind::Video, 0, 0.0, 2.0, 1.0, "a.mp4");
        let mut stage = Stage::new(timeline);
        stage.port.magnet = false;
        let press = [stage.point(2.0, 0)[0] - 2.0, stage.point(2.0, 0)[1]];
        let to = [press[0] + 480.0, press[1]];
        let (mut scene, gesture) = stage.scene();
        gesture.begin(&mut scene, press, to);
        gesture.drag(&mut scene, to, &|path| (path == "a.mp4").then_some(4.0));
        gesture.finish(&mut scene, Some(to));
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!((clip.start, clip.duration), (0.0, 3.0));
    }

    #[test]
    fn a_lost_pointer_returns_a_moved_clip_to_its_exact_home() {
        let mut timeline = Timeline::empty_project();
        // An imported start off the frame grid must come back unrounded.
        let id = timeline.add_media_clip("a", ClipKind::Video, 0, 1.2345, 2.0, 0.0, "a");
        let mut stage = Stage::new(timeline);
        let before = stage.timeline.clone();
        let from = stage.point(2.0, 0);
        let to = [from[0] + 200.0, from[1]];
        let (mut scene, gesture) = stage.scene();
        gesture.begin(&mut scene, from, to);
        gesture.drag(&mut scene, to, &|_| None);
        assert_ne!(scene.timeline.clip(id).unwrap().start, 1.2345);
        gesture.finish(&mut scene, None);
        assert_eq!(stage.timeline, before);
    }

    #[test]
    fn a_drop_past_the_last_lane_grows_one_and_a_held_edge_scrolls() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("a", ClipKind::Video, 0, 0.0, 2.0, 0.0, "a");
        let lanes = timeline.track_labels.len();
        let mut stage = Stage::new(timeline);
        let from = stage.point(1.0, 0);
        let below = [from[0], stage.point(0.0, lanes)[1] + TRACK_H];
        let (mut scene, gesture) = stage.scene();
        assert!(!gesture.autoscroll(scene.port, VIEW, [VIEW.right - 4.0, from[1]]));
        gesture.begin(&mut scene, from, below);
        assert!(gesture.autoscroll(scene.port, VIEW, [VIEW.right - 4.0, from[1]]));
        assert_eq!(scene.port.scroll_x, 14.0);
        assert!(gesture.autoscroll(scene.port, VIEW, [VIEW.content_left() + 4.0, from[1]]));
        assert_eq!(scene.port.scroll_x, 0.0);
        gesture.drag(&mut scene, below, &|_| None);
        gesture.finish(&mut scene, Some(below));
        assert_eq!(stage.timeline.track_labels.len(), lanes + 1);
        assert_eq!(stage.timeline.clip(id).unwrap().track, lanes);
    }

    #[test]
    fn the_ruler_keeps_its_labels_apart_however_far_out() {
        // From the closest zoom to a 1,000,000 s timeline fitted in 40 px.
        for px_per_sec in [600.0, 48.0, 4.0, 0.25, 0.01, 0.000_04_f32] {
            let step = ruler_step(px_per_sec);
            let apart = step as f32 * px_per_sec;
            assert!(apart >= 72.0, "{px_per_sec} px/s: labels {apart} px apart");
        }
        assert_eq!(ruler_step(48.0), 2.0);
        assert_eq!(ruler_step(0.25), 300.0);
        assert_eq!(ruler_step(0.01), 7200.0);

        assert_eq!(ruler_label(0.5, 0.25), "0:00.5");
        assert_eq!(ruler_label(75.0, 5.0), "1:15");
        assert_eq!(ruler_label(3725.0, 5.0), "1:02:05");
        assert_eq!(ruler_label(7200.0, 3600.0), "2:00:00");
    }

    #[test]
    fn wheel_scrolls_along_time_and_zoom_keeps_the_time_under_the_pointer() {
        let mut port = TimelineViewport::default();
        scroll_by(&mut port, 0.0, -120.0);
        assert_eq!(port.scroll_x, 120.0);
        scroll_by(&mut port, 500.0, 0.0);
        assert_eq!(port.scroll_x, 0.0);

        let content_left = VIEW.content_left();
        port.scroll_x = 96.0;
        let pointer_x = content_left + 240.0;
        let under = time_at(&port, content_left, pointer_x);
        zoom_about(&mut port, content_left, pointer_x, 2.0, 10.0);
        assert_eq!(port.px_per_sec, 96.0);
        assert!((time_at(&port, content_left, pointer_x) - under).abs() < 1e-4);

        // The end of a 10 s timeline can come back to mid-lanes, no further.
        port.scroll_x = 5000.0;
        clamp_scroll(&mut port, 10.0, 800.0);
        assert_eq!(port.scroll_x, 10.0 * 96.0 - 400.0);
    }

    #[test]
    fn drops_from_outside_land_on_lanes_only() {
        let port = TimelineViewport::default();
        let lane_y = VIEW.lanes_top() + TRACK_H + TRACK_GAP + 5.0;
        assert_eq!(
            drop_target(&port, VIEW, [VIEW.content_left() + 96.0, lane_y]),
            Some((1, 2.0))
        );
        assert_eq!(drop_target(&port, VIEW, [20.0, lane_y]), None);
        assert_eq!(
            drop_target(&port, VIEW, [VIEW.content_left() + 96.0, 10.0]),
            None
        );
        assert_eq!(drop_target(&port, VIEW, [1300.0, lane_y]), None);
    }

    /// One clip on the second lane, 2 s to 8 s, from 3 s into a 20 s source.
    fn clip_fixture() -> (Stage, ClipId) {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip(
            "clip.mp4",
            ClipKind::Video,
            1,
            2.0,
            6.0,
            3.0,
            "/media/full.mp4",
        );
        (Stage::new(timeline), id)
    }

    /// Two clips on the second lane that meet at 8 s.
    fn touching_fixture() -> (Stage, ClipId, ClipId) {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 1, 2.0, 6.0, 0.0, "/media/a.mp4");
        let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 1, 8.0, 6.0, 0.0, "/media/a.mp4");
        let mut stage = Stage::new(timeline);
        // Keep the snap out of a test of which clip a press takes.
        stage.port.magnet = false;
        (stage, a, b)
    }

    #[test]
    fn a_short_clip_is_all_move_body_and_the_gap_between_lanes_is_nothing() {
        let mut timeline = Timeline::empty_project();
        let short = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.2, 0.157, "scene");
        timeline.append_track();
        let mut stage = Stage::new(timeline);
        let before = stage.timeline.clone();
        let hover = |stage: &Stage, pos| {
            hover_clip(&stage.port, &stage.timeline, VIEW, pos).map(|(clip, hit)| (clip.id, hit))
        };
        assert_eq!(
            hover(&stage, stage.center(short)),
            Some((short, ClipHit::Body))
        );

        // One point below the painted clip, still above the next lane.
        let gap = [stage.center(short)[0], VIEW.lanes_top() + TRACK_H + 1.0];
        assert_eq!(hover(&stage, gap), None);
        stage.selected = Some(short);
        stage.click(gap);
        assert_eq!(stage.selected, None);
        assert_eq!(stage.timeline, before);
    }

    #[test]
    fn a_release_uses_where_the_pointer_ended_not_where_it_last_paused() {
        let (mut stage, id) = clip_fixture();
        stage.port.magnet = false;
        stage.drag_through([384.0, 99.0], &[[432.0, 99.0], [480.0, 49.0]]);
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!(
            (clip.track, clip.start, clip.duration, clip.source_in),
            (0, 4.0, 6.0, 3.0)
        );

        for (press_x, step, expected) in [
            (246.0, 24.0, (3.0, 8.0, 4.0)),
            (522.0, -24.0, (2.0, 7.0, 3.0)),
        ] {
            let (mut stage, id) = clip_fixture();
            stage.port.magnet = false;
            stage.drag_through(
                [press_x, 99.0],
                &[[press_x + step, 99.0], [press_x + 2.0 * step, 99.0]],
            );
            let clip = stage.timeline.clip(id).unwrap();
            assert_eq!((clip.start, clip.end(), clip.source_in), expected);
        }
    }

    /// A full-length clip flush at zero: its start edge is grabbed from a
    /// few points inside the header gutter, and that must still trim.
    #[test]
    fn a_clip_at_time_zero_trims_from_a_press_just_inside_the_header() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("full.mp4", ClipKind::Video, 1, 0.0, 20.0, 0.0, "full");
        let mut stage = Stage::new(timeline);
        let press = [VIEW.content_left() - 3.0, 99.0];
        assert!(Gesture::accepts(VIEW, press));
        assert!(!Gesture::accepts(VIEW, [VIEW.content_left() - 40.0, 99.0]));
        stage.drag_through(press, &[[170.0, 99.0], [210.0, 99.0]]);
        let clip = stage.timeline.clip(id).unwrap();
        assert!(clip.start > 0.5, "the trim never began: {}", clip.start);
        assert!((clip.end() - 20.0).abs() < 1e-6);
        assert!((clip.source_in - clip.start).abs() < 1e-9);
    }

    /// Grabbing six points inside the left edge and dragging 24 moves the
    /// edge by the drag (0.5 s), not to the pointer.
    #[test]
    fn a_left_trim_moves_by_the_drag_and_keeps_the_end_fixed() {
        let (mut stage, id) = clip_fixture();
        let (mut scene, gesture) = stage.scene();
        gesture.begin(&mut scene, [246.0, 99.0], [270.0, 99.0]);
        gesture.drag(&mut scene, [270.0, 99.0], &|_| None);
        assert!(gesture.is_interacting(), "a trim stays active while held");
        gesture.finish(&mut scene, Some([270.0, 99.0]));
        assert!(!gesture.is_interacting());
        let clip = stage.timeline.clip(id).unwrap();
        assert!((clip.start - 2.5).abs() < 0.02, "start {}", clip.start);
        assert!((clip.end() - 8.0).abs() < 1e-6);
        assert!((clip.source_in - 3.5).abs() < 0.02);

        // After another clip, with the magnet on: the release once rippled
        // the trimmed clip back, taking the trim off its end as well.
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 1, 0.0, 10.0, 0.0, "a");
        let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 1, 12.0, 10.0, 0.0, "b");
        let mut stage = Stage::new(timeline);
        let edge = stage.point(12.0, 1);
        stage.drag(edge, [edge[0] + 144.0, edge[1]]);
        let a = stage.timeline.clip(a).unwrap();
        assert_eq!((a.start, a.end()), (0.0, 10.0));
        let b = stage.timeline.clip(b).unwrap();
        assert!((b.start - 15.0).abs() < 0.02, "start {}", b.start);
        assert!((b.end() - 22.0).abs() < 1e-6, "end {}", b.end());
        assert!((b.source_in - 3.0).abs() < 0.02);
    }

    /// Two clips share the edge at 8 s. A press just right of it trims the
    /// right clip; the first-match hit test once always took the left one.
    #[test]
    fn touching_clips_each_trim_from_their_own_side_of_the_shared_edge() {
        let (mut stage, a, b) = touching_fixture();
        let edge = stage.point(8.0, 1);
        stage.drag([edge[0] + 5.0, edge[1]], [edge[0] + 53.0, edge[1]]);
        assert_eq!(stage.selected, Some(b));
        let (left, right) = (
            stage.timeline.clip(a).unwrap(),
            stage.timeline.clip(b).unwrap(),
        );
        assert_eq!((left.start, left.end()), (2.0, 8.0));
        assert!((right.start - 9.0).abs() < 0.02, "start {}", right.start);
        assert!((right.end() - 14.0).abs() < 1e-6);
        assert!((right.source_in - 1.0).abs() < 0.02);

        let (mut stage, a, b) = touching_fixture();
        stage.drag([edge[0] - 5.0, edge[1]], [edge[0] - 53.0, edge[1]]);
        assert_eq!(stage.selected, Some(a));
        let (left, right) = (
            stage.timeline.clip(a).unwrap(),
            stage.timeline.clip(b).unwrap(),
        );
        assert!((left.end() - 7.0).abs() < 0.02, "end {}", left.end());
        assert_eq!((left.start, right.start, right.end()), (2.0, 8.0, 14.0));
    }

    #[test]
    fn a_drop_on_a_neighbour_swaps_places_and_a_drop_across_two_returns_home() {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 0, 2.0, 6.0, 0.0, "a");
        let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 0, 10.0, 6.0, 0.0, "b");
        let mut stage = Stage::new(timeline);
        // Held at 5 s, dropped with the pointer at 13 s, inside the other.
        stage.drag(stage.point(5.0, 0), stage.point(13.0, 0));
        assert!((stage.timeline.clip(a).unwrap().start - 10.0).abs() < 0.02);
        assert!((stage.timeline.clip(b).unwrap().start - 2.0).abs() < 0.02);

        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a.mp4", ClipKind::Video, 0, 2.0, 6.0, 0.0, "a");
        let b = timeline.add_media_clip("b.mp4", ClipKind::Video, 0, 10.0, 2.0, 0.0, "b");
        let c = timeline.add_media_clip("c.mp4", ClipKind::Video, 0, 12.0, 2.0, 0.0, "c");
        let mut stage = Stage::new(timeline);
        // Dropped from 9 s it would cover both others: no single swap.
        stage.drag(stage.point(5.0, 0), stage.point(12.0, 0));
        assert!((stage.timeline.clip(a).unwrap().start - 2.0).abs() < 0.02);
        assert_eq!(stage.timeline.clip(b).unwrap().start, 10.0);
        assert_eq!(stage.timeline.clip(c).unwrap().start, 12.0);
    }

    #[test]
    fn a_vertical_drag_changes_lane_and_one_above_the_first_adds_a_lane_on_top() {
        let (mut stage, id) = clip_fixture();
        stage.drag(stage.point(5.0, 1), stage.point(5.0, 0));
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!(clip.track, 0);
        assert!((clip.start - 2.0).abs() < 0.02, "start {}", clip.start);

        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("clip.mp4", ClipKind::Video, 0, 2.0, 6.0, 0.0, "a");
        let other = timeline.add_media_clip("other.mp4", ClipKind::Video, 0, 10.0, 2.0, 0.0, "a");
        let mut stage = Stage::new(timeline);
        // Dropped in the ruler, the only room above the first lane.
        stage.drag(stage.point(5.0, 0), [stage.x(5.0), 10.0]);
        assert_eq!(stage.timeline.track_labels.len(), 2);
        assert_eq!(stage.timeline.clip(id).unwrap().track, 0);
        assert_eq!(stage.timeline.clip(other).unwrap().track, 1);
    }

    #[test]
    fn a_drop_below_the_view_still_adds_a_lane_and_keeps_the_sideways_move() {
        let (mut stage, id) = clip_fixture();
        stage.port.magnet = false;
        let outside = [480.0, VIEW.bottom + 20.0];
        let (mut scene, gesture) = stage.scene();
        gesture.begin(&mut scene, [384.0, 99.0], [432.0, 99.0]);
        gesture.drag(&mut scene, [432.0, 99.0], &|_| None);
        gesture.drag(&mut scene, outside, &|_| None);
        assert!(gesture.is_interacting(), "a move follows the pointer out");
        assert_eq!(scene.timeline.track_labels.len(), 2);
        gesture.finish(&mut scene, Some(outside));
        assert!(!gesture.is_interacting());
        assert_eq!(stage.selected, Some(id));
        assert_eq!(stage.timeline.track_labels.len(), 3);
        let clip = stage.timeline.clip(id).unwrap();
        assert_eq!(
            (clip.track, clip.start, clip.duration, clip.source_in),
            (2, 4.0, 6.0, 3.0)
        );
    }

    #[test]
    fn a_click_on_a_trim_handle_selects_without_trimming_and_a_ruler_click_keeps_the_selection() {
        // The body, then three points inside each handle.
        for (x, time) in [(384.0, 5.0), (246.0, 2.125), (522.0, 7.875)] {
            let (mut stage, id) = clip_fixture();
            let before = stage.timeline.clone();
            stage.click([x, 99.0]);
            assert_eq!(stage.selected, Some(id));
            assert_eq!(stage.playhead, stage.timeline.snap(time));
            assert_eq!(stage.timeline, before, "x={x}");
            assert!(!stage.gesture.is_interacting());
        }

        let (mut stage, id) = clip_fixture();
        stage.selected = Some(id);
        stage.click([432.0, 10.0]);
        assert_eq!(stage.playhead, 6.0);
        assert_eq!(stage.selected, Some(id));
        assert!(!stage.gesture.is_interacting());
    }

    #[test]
    fn move_magnet_uses_start_target_when_end_has_no_target() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
        let target = timeline.add_media_clip("target", ClipKind::Video, 1, 8.0, 2.0, 0.0, "target");
        let target_before = timeline.clip(target).unwrap().clone();
        let mut stage = Stage::new(timeline);
        let from = stage.center(id);
        let to = [stage.x(7.9 + 0.5), from[1]];
        stage.drag(from, to);
        assert_eq!(stage.timeline.clip(id).unwrap().start, 8.0);
        assert_eq!(stage.timeline.clip(target).unwrap(), &target_before);
    }

    #[test]
    fn move_magnet_uses_end_target_when_start_has_no_target() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
        timeline.add_media_clip("target", ClipKind::Video, 1, 8.0, 2.0, 0.0, "target");
        let mut stage = Stage::new(timeline);
        let from = stage.center(id);
        let to = [stage.x(6.9 + 0.5), from[1]];
        stage.drag(from, to);
        assert_eq!(stage.timeline.clip(id).unwrap().end(), 8.0);
    }

    #[test]
    fn move_magnet_respects_toggle_threshold_and_nearest_edge() {
        for (magnet, raw, expected) in [
            (true, 7.9, 238.0 / 30.0),
            (false, 7.9, 7.9),
            (true, 7.7, 7.7),
        ] {
            let mut timeline = Timeline::empty_project();
            let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
            timeline.add_media_clip(
                "target",
                ClipKind::Video,
                1,
                8.0,
                28.0 / 30.0,
                0.0,
                "target",
            );
            let mut stage = Stage::new(timeline);
            stage.port = TimelineViewport {
                magnet,
                ..Default::default()
            };
            let from = stage.center(id);
            let to = [stage.x(raw + 0.5), from[1]];
            stage.drag(from, to);
            assert!(
                (stage.timeline.clip(id).unwrap().start - expected).abs() < 1e-9,
                "magnet={magnet} raw={raw}"
            );
        }
    }

    #[test]
    fn short_clip_center_moves_without_trimming() {
        for duration in [1.0 / 120.0, 0.1, 0.2, 0.375] {
            let mut timeline = Timeline::empty_project();
            let id =
                timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, duration, 0.157, "scene");
            let mut stage = Stage::new(timeline);
            stage.port = TimelineViewport {
                magnet: false,
                ..Default::default()
            };
            let from = stage.center(id);
            let selected = stage.drag(from, [from[0] + 48.0, from[1]]);
            let clip = stage.timeline.clip(id).unwrap();
            assert_eq!(selected, Some(id));
            assert_eq!(
                (clip.start, clip.duration, clip.source_in),
                (3.0, duration, 0.157),
                "duration={duration}"
            );
        }
    }

    #[test]
    fn short_clip_center_moves_to_existing_lane_without_trimming() {
        for duration in [1.0 / 120.0, 0.1, 0.2, 0.375] {
            let mut timeline = Timeline::empty_project();
            let id =
                timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, duration, 0.157, "scene");
            timeline.append_track();
            let mut stage = Stage::new(timeline);
            let from = stage.center(id);
            stage.drag(from, [from[0], from[1] + 50.0]);
            let clip = stage.timeline.clip(id).unwrap();
            assert_eq!(
                (clip.track, clip.start, clip.duration, clip.source_in),
                (1, 2.0, duration, 0.157),
                "duration={duration}"
            );
        }
    }

    #[test]
    fn short_clip_center_is_not_stolen_by_neighbor_edge() {
        let mut timeline = Timeline::empty_project();
        let left = timeline.add_media_clip("left", ClipKind::Video, 0, 0.0, 2.0, 0.0, "left");
        let id = timeline.add_media_clip(
            "insert",
            ClipKind::Video,
            0,
            2.0,
            1.0 / 120.0,
            0.157,
            "insert",
        );
        let right = timeline.add_media_clip(
            "right",
            ClipKind::Video,
            0,
            2.0 + 1.0 / 120.0,
            2.0,
            0.0,
            "right",
        );
        timeline.append_track();
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(id);
        assert_eq!(stage.drag(from, [from[0], from[1] + 50.0]), Some(id));
        assert_eq!(stage.timeline.clip(id).unwrap().track, 1);
        assert_eq!(stage.timeline.clip(id).unwrap().duration, 1.0 / 120.0);
        for other in [left, right] {
            assert_eq!(stage.timeline.clip(other), before.clip(other));
        }
    }

    #[test]
    fn short_clip_edges_still_trim_when_zoomed_in() {
        for left in [true, false] {
            let mut timeline = Timeline::empty_project();
            let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 1.0, 0.2, 0.157, "scene");
            let mut stage = Stage::new(timeline);
            stage.port = TimelineViewport {
                px_per_sec: 300.0,
                magnet: false,
                ..Default::default()
            };
            let edge = if left { 1.0 } else { 1.2 };
            let from = [stage.x(edge) + if left { 3.0 } else { -3.0 }, 49.0];
            let to = [from[0] + if left { 10.0 } else { -10.0 }, from[1]];
            stage.drag(from, to);
            let clip = stage.timeline.clip(id).unwrap();
            let expected_start = if left { 31.0 / 30.0 } else { 1.0 };
            let expected_end = if left { 1.2 } else { 35.0 / 30.0 };
            assert!((clip.start - expected_start).abs() < 1e-9);
            assert!((clip.end() - expected_end).abs() < 1e-9);
            assert!((clip.source_in - (0.157 + expected_start - 1.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn move_drop_adjacent_unequal_clips_preserves_outer_span() {
        for (left_duration, right_duration, drag_left) in [
            (0.2, 0.4, true),
            (0.2, 0.4, false),
            (0.4, 0.2, true),
            (0.4, 0.2, false),
        ] {
            let mut timeline = Timeline::empty_project();
            let left = timeline.add_media_clip(
                "left",
                ClipKind::Video,
                0,
                2.0,
                left_duration,
                0.157,
                "left",
            );
            let right = timeline.add_media_clip(
                "right",
                ClipKind::Video,
                0,
                2.0 + left_duration,
                right_duration,
                1.237,
                "right",
            );
            let tail = timeline.add_media_clip(
                "tail",
                ClipKind::Video,
                0,
                2.0 + left_duration + right_duration,
                0.5,
                4.0,
                "tail",
            );
            let before = timeline.clone();
            let mut stage = Stage::new(timeline);
            stage.port = TimelineViewport {
                px_per_sec: 240.0,
                magnet: false,
                ..Default::default()
            };
            let id = if drag_left { left } else { right };
            let from = stage.center(id);
            let raw = if drag_left { 2.0 + right_duration } else { 2.0 };
            let to = [
                stage.x(raw + stage.timeline.clip(id).unwrap().duration / 2.0),
                from[1],
            ];
            stage.drag(from, to);
            assert!((stage.timeline.clip(left).unwrap().start - (2.0 + right_duration)).abs() < 1e-9, "left_duration={left_duration} right_duration={right_duration} drag_left={drag_left}");
            assert_eq!(stage.timeline.clip(right).unwrap().start, 2.0);
            for id in [left, right] {
                let clip = stage.timeline.clip(id).unwrap();
                let original = before.clip(id).unwrap();
                assert_eq!(
                    (clip.duration, clip.source_in),
                    (original.duration, original.source_in)
                );
            }
            assert_eq!(stage.timeline.clip(tail), before.clip(tail));
        }
    }

    #[test]
    fn move_drop_adjacent_with_gap_preserves_gap_and_neighbors() {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 0.4, 0.0, "a");
        let b = timeline.add_media_clip("b", ClipKind::Video, 0, 2.6, 0.2, 0.0, "b");
        let tail = timeline.add_media_clip("tail", ClipKind::Video, 0, 2.8, 1.0, 0.0, "tail");
        let tail_before = timeline.clip(tail).unwrap().clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            px_per_sec: 120.0,
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(a);
        let to = [stage.x(2.6), from[1]];
        stage.drag(from, to);
        assert!((stage.timeline.clip(a).unwrap().start - 2.4).abs() < 1e-9);
        assert!((stage.timeline.clip(a).unwrap().end() - 2.8).abs() < 1e-9);
        assert_eq!(stage.timeline.clip(b).unwrap().start, 2.0);
        assert!(
            (stage.timeline.clip(a).unwrap().start - stage.timeline.clip(b).unwrap().end() - 0.2)
                .abs()
                < 1e-9
        );
        assert_eq!(stage.timeline.clip(tail).unwrap(), &tail_before);
    }

    #[test]
    fn move_drop_blocked_on_another_lane_reverts_exact_fractional_start() {
        for linked in [false, true] {
            let mut timeline = Timeline::empty_project();
            let id =
                timeline.add_media_clip("scene", ClipKind::Video, 0, 2.157, 0.418, 0.157, "scene");
            if linked {
                timeline.detach_audio(id).unwrap();
            }
            timeline.add_media_clip("blocker", ClipKind::Video, 2, 5.0, 2.0, 0.0, "blocker");
            let before = timeline.clone();
            let mut stage = Stage::new(timeline);
            stage.port = TimelineViewport {
                px_per_sec: 120.0,
                magnet: false,
                ..Default::default()
            };
            let from = stage.center(id);
            let to = [stage.x(5.0 + 0.209), 149.0];
            stage.drag(from, to);
            assert_eq!(
                stage.timeline, before,
                "rejected drop must restore exact placement (linked={linked})"
            );
        }
    }

    #[test]
    fn move_drop_with_only_linked_audio_collision_reverts() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 1.0, 0.157, "scene");
        timeline.detach_audio(id).unwrap();
        timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(id);
        stage.drag(from, [from[0] + 96.0, from[1]]);
        assert_eq!(stage.timeline, before);
    }

    #[test]
    fn move_drop_linked_swap_checks_both_linked_lanes_atomically() {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 1.0, 0.157, "a");
        let b = timeline.add_media_clip("b", ClipKind::Video, 0, 4.0, 1.0, 1.237, "b");
        timeline.detach_audio(a).unwrap();
        let b_audio = timeline.detach_audio(b).unwrap();
        timeline.set_clip_track(b_audio, 2);
        timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(a);
        stage.drag(from, [from[0] + 96.0, from[1]]);
        assert_eq!(
            stage.timeline, before,
            "a blocked companion must reject the entire swap"
        );
    }

    #[test]
    fn move_drop_linked_swap_keeps_all_four_ranges() {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 0.2, 0.157, "a");
        let b = timeline.add_media_clip("b", ClipKind::Video, 0, 2.2, 0.4, 1.237, "b");
        let a_audio = timeline.detach_audio(a).unwrap();
        let b_audio = timeline.detach_audio(b).unwrap();
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            px_per_sec: 240.0,
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(a);
        let to = [stage.x(2.5), from[1]];
        stage.drag(from, to);
        for (id, expected_start) in [(a, 2.4), (a_audio, 2.4), (b, 2.0), (b_audio, 2.0)] {
            let clip = stage.timeline.clip(id).unwrap();
            let original = before.clip(id).unwrap();
            assert!((clip.start - expected_start).abs() < 1e-9);
            assert_eq!(
                (clip.duration, clip.source_in, clip.track, clip.linked_clip),
                (
                    original.duration,
                    original.source_in,
                    original.track,
                    original.linked_clip
                )
            );
        }
    }

    #[test]
    fn move_magnet_does_not_snap_to_linked_partner() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 2.0, 0.157, "scene");
        let audio = timeline.detach_audio(id).unwrap();
        let mut stage = Stage::new(timeline);
        let from = stage.center(id);
        stage.drag(from, [from[0] + 7.2, from[1]]);
        assert_eq!(stage.timeline.clip(id).unwrap().start, 65.0 / 30.0);
        assert_eq!(
            stage.timeline.clip(audio).unwrap().start,
            stage.timeline.clip(id).unwrap().start
        );
    }

    #[test]
    fn move_drop_new_lane_with_linked_collision_reverts_without_growing() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 1.0, 0.157, "scene");
        timeline.detach_audio(id).unwrap();
        timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
        let before = timeline.clone();
        let mut stage = Stage::new(timeline);
        stage.port = TimelineViewport {
            magnet: false,
            ..Default::default()
        };
        let from = stage.center(id);
        let to = [from[0] + 96.0, 170.0];
        stage.drag(from, to);
        assert_eq!(stage.timeline, before);
    }

    #[test]
    fn dense_short_timeline_default_zoom_reorders_only_the_adjacent_pair() {
        for magnet in [false, true] {
            for drag_left in [false, true] {
                for (left_duration, right_duration) in [(0.2, 0.4), (0.4, 0.2)] {
                    let mut timeline = Timeline::empty_project();
                    for i in 0..40 {
                        timeline.add_media_clip(
                            "prefix",
                            ClipKind::Video,
                            0,
                            i as f64 * 0.1,
                            0.1,
                            i as f64 * 0.1,
                            "scene",
                        );
                    }
                    let left = timeline.add_media_clip(
                        "left",
                        ClipKind::Video,
                        0,
                        4.0,
                        left_duration,
                        0.157,
                        "left",
                    );
                    let right = timeline.add_media_clip(
                        "right",
                        ClipKind::Video,
                        0,
                        4.0 + left_duration,
                        right_duration,
                        1.237,
                        "right",
                    );
                    for i in 0..40 {
                        timeline.add_media_clip(
                            "tail",
                            ClipKind::Video,
                            0,
                            4.6 + i as f64 * 0.1,
                            0.1,
                            i as f64 * 0.1,
                            "scene",
                        );
                    }
                    let before = timeline.clone();
                    let mut stage = Stage::new(timeline);
                    stage.port = TimelineViewport {
                        magnet,
                        ..Default::default()
                    };
                    let id = if drag_left { left } else { right };
                    let from = stage.center(id);
                    let raw = if drag_left { 4.0 + right_duration } else { 4.0 };
                    let to = [
                        stage.x(raw + stage.timeline.clip(id).unwrap().duration / 2.0),
                        from[1],
                    ];
                    assert_eq!(stage.drag(from, to), Some(id));
                    assert!(
                        (stage.timeline.clip(left).unwrap().start - (4.0 + right_duration)).abs()
                            < 1e-9
                    );
                    assert_eq!(stage.timeline.clip(right).unwrap().start, 4.0);
                    assert_eq!(stage.timeline.clips.len(), 82);
                    for clip in &stage.timeline.clips {
                        let mut expected = before.clip(clip.id).unwrap().clone();
                        if clip.id == left || clip.id == right {
                            expected.start = clip.start;
                        }
                        assert_eq!(clip, &expected, "no ripple, trim or source change");
                    }
                }
            }
        }
    }

    #[test]
    fn unequal_adjacent_center_drop_does_not_collide_with_unrelated_neighbors() {
        for magnet in [false, true] {
            for drag_left in [false, true] {
                let (left_duration, right_duration) =
                    if drag_left { (0.6, 0.2) } else { (0.2, 0.6) };
                let mut timeline = Timeline::empty_project();
                timeline.add_media_clip("prefix", ClipKind::Video, 0, 1.5, 0.5, 0.0, "prefix");
                let left = timeline.add_media_clip(
                    "left",
                    ClipKind::Video,
                    0,
                    2.0,
                    left_duration,
                    0.157,
                    "left",
                );
                let right = timeline.add_media_clip(
                    "right",
                    ClipKind::Video,
                    0,
                    2.0 + left_duration,
                    right_duration,
                    1.237,
                    "right",
                );
                timeline.add_media_clip("tail", ClipKind::Video, 0, 2.8, 0.5, 0.0, "tail");
                let before = timeline.clone();
                let mut stage = Stage::new(timeline);
                stage.port = TimelineViewport {
                    magnet,
                    ..Default::default()
                };
                let (dragged, target) = if drag_left {
                    (left, right)
                } else {
                    (right, left)
                };
                let from = stage.center(dragged);
                let to = stage.center(target);
                stage.drag(from, to);
                assert!(
                    (stage.timeline.clip(left).unwrap().start - (2.0 + right_duration)).abs()
                        < 1e-9,
                    "magnet={magnet} drag_left={drag_left}"
                );
                assert_eq!(stage.timeline.clip(right).unwrap().start, 2.0);
                for clip in &stage.timeline.clips {
                    let mut expected = before.clip(clip.id).unwrap().clone();
                    if clip.id == left || clip.id == right {
                        expected.start = clip.start;
                    }
                    assert_eq!(clip, &expected);
                }
            }
        }
    }

    #[test]
    fn short_clips_can_grow_lanes_regardless_of_kind() {
        for kind in [ClipKind::Video, ClipKind::Audio, ClipKind::Title] {
            for above in [false, true] {
                let mut timeline = Timeline::empty_project();
                let id = timeline.add_clip("short", kind, 0, 2.0, 0.2, 0.157);
                let other = timeline.add_clip("other", ClipKind::Video, 0, 4.0, 0.4, 1.237);
                let mut stage = Stage::new(timeline);
                let from = stage.center(id);
                let to = [from[0], if above { 10.0 } else { 120.0 }];
                stage.drag(from, to);
                let clip = stage.timeline.clip(id).unwrap();
                assert_eq!(stage.timeline.track_labels.len(), 2);
                assert_eq!(clip.track, if above { 0 } else { 1 });
                assert_eq!(
                    (clip.start, clip.duration, clip.source_in),
                    (2.0, 0.2, 0.157)
                );
                assert_eq!(
                    stage.timeline.clip(other).unwrap().track,
                    if above { 1 } else { 0 }
                );
                assert_eq!(stage.timeline.clip(other).unwrap().start, 4.0);
            }
        }
    }

    #[test]
    fn linked_short_clip_lane_change_preserves_sound_and_rejects_its_partner_lane() {
        for target in [1, 2] {
            let mut timeline = Timeline::empty_project();
            let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.1, 0.157, "scene");
            let audio = timeline.detach_audio(id).unwrap();
            timeline.append_track();
            let before = timeline.clone();
            let mut stage = Stage::new(timeline);
            let from = stage.center(id);
            stage.drag(from, [from[0], from[1] + target as f32 * 50.0]);
            let mut expected = before;
            if target == 2 {
                expected.clip_mut(id).unwrap().track = 2;
            }
            assert_eq!(stage.timeline, expected);
            assert_eq!(
                stage.timeline.clip(id).unwrap().start,
                stage.timeline.clip(audio).unwrap().start
            );
        }
    }
}

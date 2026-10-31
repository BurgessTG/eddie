//! Custom-painted timeline: ruler, lanes, clips, playhead.
//!
//! Geometry and hit-testing live here. Pointer placement uses the engine's
//! project frame grid; reorders and rollbacks retain exact existing intervals.

use std::collections::HashMap;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, Vec2,
};
use openreel_engine::{format_timecode, Clip, ClipId, ClipKind, Timeline};

use crate::theme;

const HEADER_W: f32 = 144.0;
const RULER_H: f32 = 26.0;
const TRACK_H: f32 = 46.0;
const TRACK_GAP: f32 = 4.0;
const EDGE_PX: f32 = 9.0;
const MOVE_BODY_PX: f32 = 12.0;

/// Per-source visual data supplied by the app for one frame of painting.
pub struct ClipVisual<'a> {
    pub duration: f64,
    pub peaks: &'a [f32],
    pub thumbs: &'a [(f64, TextureHandle)],
}

pub type VisualMap<'a> = HashMap<&'a str, ClipVisual<'a>>;

#[derive(Clone, Copy)]
enum ClipHit {
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
struct FilmstripTile {
    x: f32,
    w: f32,
    uv: (f32, f32),
    center_source: f64,
}

/// Lay out filmstrip fragments on a grid anchored to source time
/// zero, not to the clip's left edge. Tile width in seconds comes
/// from the zoom (`tile_w / px_per_sec`), so zooming changes frame
/// density, while trimming slides the clip border across fragments
/// that stay put: an edge tile is UV-cropped, never re-phased.
fn filmstrip_tiles(
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
fn snap_to(time: f64, candidates: &[f64], threshold: f64) -> f64 {
    snap_target(time, candidates, threshold).unwrap_or(time)
}

pub struct TimelineView {
    pub px_per_sec: f32,
    pub scroll_x: f32,
    pub magnet: bool,
    drag: Option<(ClipId, DragMode)>,
    press_origin: Option<Pos2>,
    last_drag_pos: Option<Pos2>,
    seeking: bool,
    last_rect: Option<Rect>,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            px_per_sec: 48.0,
            scroll_x: 0.0,
            magnet: true,
            drag: None,
            press_origin: None,
            last_drag_pos: None,
            seeking: false,
            last_rect: None,
        }
    }
}

impl TimelineView {
    fn x_at(&self, content_left: f32, time: f64) -> f32 {
        content_left + time as f32 * self.px_per_sec - self.scroll_x
    }

    fn time_at(&self, content_left: f32, x: f32) -> f64 {
        ((x - content_left + self.scroll_x) / self.px_per_sec) as f64
    }

    /// True while the user is scrubbing the ruler or dragging a clip.
    pub fn is_interacting(&self) -> bool {
        self.seeking || self.drag.is_some()
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        timeline: &mut Timeline,
        playhead: &mut f64,
        selected: &mut Option<ClipId>,
        visuals: &VisualMap<'_>,
    ) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.last_rect = Some(rect);
        let content_left = rect.left() + HEADER_W;
        let content = Rect::from_min_max(
            Pos2::new(content_left, rect.top()),
            Pos2::new(rect.right(), rect.bottom()),
        );

        let max_scroll = (timeline.end() as f32 * self.px_per_sec - content.width() * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.min(max_scroll);
        self.handle_scroll(ui, &response, content_left);
        self.handle_pointer(
            &response,
            timeline,
            playhead,
            selected,
            visuals,
            content_left,
            rect,
        );
        self.paint(ui, timeline, *playhead, *selected, visuals, rect, content);
        response.on_hover_text(
            "Click to move the playhead. Drag clips to move, drag edges to trim. S splits at the playhead. Ctrl+scroll zooms.",
        );
    }

    /// Map a screen position to `(track, time)` for drag-and-drop from the
    /// media bin. Uses the most recent frame's geometry.
    pub fn drop_target(&self, pos: Pos2) -> Option<(usize, f64)> {
        let rect = self.last_rect?;
        if !rect.contains(pos) || pos.x < rect.left() + HEADER_W {
            return None;
        }
        let track = self.track_at(rect, pos.y)?;
        let time = self.time_at(rect.left() + HEADER_W, pos.x).max(0.0);
        Some((track, time))
    }

    /// Content rect of a lane from the most recent frame's geometry,
    /// for highlighting a pending drop lane. Works one past the last
    /// lane too (the append zone).
    pub fn lane_rect(&self, track: usize) -> Option<Rect> {
        let rect = self.last_rect?;
        let y = rect.top() + RULER_H + track as f32 * (TRACK_H + TRACK_GAP);
        Some(Rect::from_min_size(
            Pos2::new(rect.left() + HEADER_W, y),
            Vec2::new((rect.right() - rect.left() - HEADER_W).max(0.0), TRACK_H),
        ))
    }

    fn handle_scroll(&mut self, ui: &mut Ui, response: &egui::Response, content_left: f32) {
        if !response.hovered() {
            return;
        }
        let (delta, ctrl, pointer_x) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.modifiers.ctrl || i.modifiers.mac_cmd,
                i.pointer.hover_pos().map(|p| p.x).unwrap_or(content_left),
            )
        });
        if ctrl && delta.y != 0.0 {
            // Zoom around the cursor: keep the time under it stationary.
            let anchor_time = self.time_at(content_left, pointer_x);
            let factor = (delta.y * 0.002).exp();
            self.px_per_sec = (self.px_per_sec * factor).clamp(4.0, 600.0);
            self.scroll_x =
                (anchor_time as f32 * self.px_per_sec - (pointer_x - content_left)).max(0.0);
        } else {
            self.scroll_x = (self.scroll_x - delta.y - delta.x).max(0.0);
        }
    }

    fn track_at(&self, rect: Rect, y: f32) -> Option<usize> {
        let lanes_top = rect.top() + RULER_H;
        if y < lanes_top {
            return None;
        }
        Some(((y - lanes_top) / (TRACK_H + TRACK_GAP)) as usize)
    }

    fn clip_rect(&self, rect: Rect, clip: &Clip) -> Rect {
        let content_left = rect.left() + HEADER_W;
        let y = rect.top() + RULER_H + clip.track as f32 * (TRACK_H + TRACK_GAP) + 3.0;
        Rect::from_min_max(
            Pos2::new(self.x_at(content_left, clip.start), y),
            Pos2::new(self.x_at(content_left, clip.end()), y + TRACK_H - 6.0),
        )
    }

    /// Paint, hover and press classification share the same geometry. Handles
    /// shrink before taking away the move body; tiny clips are entirely body
    /// until zoomed in. An adjacent clip's outside handle cannot steal it.
    fn hit_clip<'a>(
        &self,
        timeline: &'a Timeline,
        rect: Rect,
        pos: Pos2,
    ) -> Option<(&'a Clip, ClipHit)> {
        let track = self.track_at(rect, pos.y)?;
        let mut outside_edge: Option<(f32, &Clip, ClipHit)> = None;
        for clip in timeline.clips.iter().filter(|c| c.track == track) {
            let bounds = self.clip_rect(rect, clip);
            if !bounds.y_range().contains(pos.y) {
                continue;
            }
            let handle = EDGE_PX.min(((bounds.width() - MOVE_BODY_PX) * 0.5).max(0.0));
            let left_dist = (pos.x - bounds.left()).abs();
            let right_dist = (pos.x - bounds.right()).abs();
            if pos.x >= bounds.left() && pos.x < bounds.right() {
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

    fn handle_pointer(
        &mut self,
        response: &egui::Response,
        timeline: &mut Timeline,
        playhead: &mut f64,
        selected: &mut Option<ClipId>,
        visuals: &VisualMap<'_>,
        content_left: f32,
        rect: Rect,
    ) {
        let (pressed, released, decidedly_dragging, origin, pointer_pos) =
            response.ctx.input(|i| {
                (
                    i.pointer.primary_pressed(),
                    i.pointer.primary_released(),
                    i.pointer.is_decidedly_dragging(),
                    i.pointer.press_origin(),
                    i.pointer.interact_pos(),
                )
            });
        if pressed {
            self.press_origin = response
                .is_pointer_button_down_on()
                .then_some(origin)
                .flatten();
        }
        let saved_press = self.press_origin;
        if released {
            self.press_origin = None;
        }
        // egui drops its potential drag and press origin on release. A quick
        // gesture can cross the drag threshold in that same frame, before it
        // has ever reported drag_started. Only promote a press we owned.
        let release_drag = released
            && decidedly_dragging
            && saved_press.is_some()
            && self.drag.is_none()
            && !self.seeking;
        let drag_started = response.drag_started() || release_drag;
        let drag_stopped = response.drag_stopped() || release_drag;
        let Some(pos) = response
            .interact_pointer_pos()
            .or_else(|| release_drag.then_some(pointer_pos).flatten())
        else {
            if drag_stopped {
                let last = self.last_drag_pos;
                self.finish_drag(timeline, rect, last);
            }
            return;
        };
        if self.seeking || self.drag.is_some() {
            self.last_drag_pos = Some(pos);
        }
        // Classify from where the press began, not where the pointer is
        // when egui crosses its drag threshold (~6px later). Using the
        // current position misclassifies most edge grabs as moves.
        let press_pos = saved_press.or(origin).unwrap_or(pos);
        // Ignore presses deep in the track-header gutter, but allow the
        // strip of gutter next to t=0 where a full-length clip's left edge
        // lives. While an interaction is live, always keep tracking.
        if press_pos.x < content_left - EDGE_PX && !self.seeking && self.drag.is_none() {
            return;
        }

        // Auto-scroll while dragging near the viewport edges so the far
        // edge of a long clip can be reached without letting go.
        if (self.seeking || self.drag.is_some()) && !drag_stopped {
            const MARGIN: f32 = 36.0;
            const SPEED: f32 = 14.0;
            if pos.x > rect.right() - MARGIN {
                self.scroll_x += SPEED;
                response.ctx.request_repaint();
            } else if pos.x < content_left + MARGIN && pos.x >= rect.left() {
                self.scroll_x = (self.scroll_x - SPEED).max(0.0);
                response.ctx.request_repaint();
            }
        }

        let time = self.time_at(content_left, pos.x).max(0.0);
        let press_time = self.time_at(content_left, press_pos.x).max(0.0);

        // Clicks arrive on release without a later drag_stopped event.
        // Only real drags may retain interaction state.
        if drag_started || response.clicked() {
            if press_pos.y < rect.top() + RULER_H {
                self.seeking = drag_started;
                *playhead = timeline.snap(time);
            } else {
                // A click parks the playhead. A drag keeps the existing
                // playhead target instead of snapping short clips to their
                // own grab point, especially on a purely vertical move.
                if response.clicked() {
                    *playhead = timeline.snap(press_time);
                }
                if let Some((clip, hit)) = self.hit_clip(timeline, rect, press_pos) {
                    let mode = match hit {
                        ClipHit::Body => DragMode::Move {
                            grab_offset: press_time - clip.start,
                            home_start: clip.start,
                            home_track: clip.track,
                            home_linked: clip
                                .linked_clip
                                .and_then(|id| timeline.clip(id))
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
                    *selected = Some(clip.id);
                    self.drag = drag_started.then_some((clip.id, mode));
                } else if response.clicked() {
                    *selected = None;
                }
            }
        }

        // egui does not report `dragged` on the release frame, which can
        // carry a new pointer position (including a different target lane).
        if response.dragged() || drag_stopped {
            self.last_drag_pos = Some(pos);
            if self.seeking {
                *playhead = timeline.snap(time);
            } else if let Some((id, mode)) = self.drag {
                // Snap targets (magnet on): timeline start, playhead, other
                // clip edges. Magnet off means free placement on frame grid.
                let threshold = 8.0 / self.px_per_sec as f64;
                let mut candidates: Vec<f64> = Vec::new();
                if self.magnet {
                    candidates.extend([0.0, *playhead]);
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
                let clip_snapshot = timeline.clip(id).cloned();
                match (mode, clip_snapshot) {
                    (DragMode::Move { grab_offset, .. }, Some(clip)) => {
                        // Vertical lane change within existing lanes;
                        // growth past the top or bottom lane resolves
                        // on drop (finish_drag). Horizontal overlap is
                        // allowed mid-drag so the clip can be carried
                        // over a neighbor; finish_drag swaps or
                        // returns it home, never leaving an overlap.
                        if let Some(target) = self
                            .track_at(rect, pos.y)
                            .filter(|t| *t < timeline.track_labels.len())
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
                    (
                        DragMode::TrimLeft {
                            orig_start,
                            press_time,
                        },
                        Some(clip),
                    ) => {
                        // Move the edge by the drag delta. Setting the edge
                        // to the pointer time would instantly cut away the
                        // grab offset inside the edge handle.
                        let desired = orig_start + (time - press_time);
                        let snapped = snap_to(desired, &candidates, threshold);
                        let prev_end = Self::prev_end(timeline, &clip, orig_start);
                        timeline.trim_left(id, snapped.max(prev_end));
                    }
                    (
                        DragMode::TrimRight {
                            orig_end,
                            press_time,
                        },
                        Some(clip),
                    ) => {
                        let desired = orig_end + (time - press_time);
                        let snapped = snap_to(desired, &candidates, threshold);
                        let mut new_end = snapped;
                        if let Some(visual) = clip
                            .source
                            .as_ref()
                            .and_then(|src| visuals.get(src.as_str()))
                        {
                            let max_end = clip.start + (visual.duration - clip.source_in).max(0.0);
                            new_end = new_end.min(max_end);
                        }
                        if let Some(next) = Self::next_start(timeline, &clip, clip.start) {
                            new_end = new_end.min(next);
                        }
                        timeline.trim_right(id, new_end);
                    }
                    _ => {}
                }
            }
        }

        if drag_stopped {
            self.finish_drag(timeline, rect, Some(pos));
        }
    }

    /// End a drag. The magnet only snaps while dragging; gaps are
    /// never closed here. A Move dropped above the first lane or
    /// below the last grows a new lane there for the clip. A Move
    /// dropped onto one neighbor on its home lane swaps positions
    /// with it. A wider preview may overlap more clips when the pointer
    /// clearly targets an adjacent neighbor; only that pair reorders.
    /// Ambiguous or colliding drops return home without moving other clips.
    fn finish_drag(&mut self, timeline: &mut Timeline, rect: Rect, drop_pos: Option<Pos2>) {
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
            let lanes_top = rect.top() + RULER_H;
            let lanes_bottom =
                lanes_top + timeline.track_labels.len() as f32 * (TRACK_H + TRACK_GAP) - TRACK_GAP;
            // Well past the edge counts as aiming for a new lane, so
            // slight pointer drift on a sideways drag of an edge-lane
            // clip does not grow one by accident. The ruler strip is
            // the only room above the first lane, hence the smaller
            // top threshold.
            let above = drop_pos.is_some_and(|pos| pos.y < lanes_top - RULER_H * 0.5);
            let below = drop_pos.is_some_and(|pos| pos.y > lanes_bottom + TRACK_H * 0.5);
            if drop_pos.is_none() || ((above || below) && !Self::linked_lane_clear(timeline, id)) {
                Self::restore_move(timeline, id, home_start, home_track, home_linked);
            } else if above {
                timeline.insert_track_at(0);
                timeline.set_clip_track(id, 0);
            } else if below {
                let track = timeline.append_track();
                timeline.set_clip_track(id, track);
            } else if let Some(pos) = drop_pos {
                Self::resolve_move_drop(
                    timeline,
                    id,
                    home_start,
                    home_track,
                    home_linked,
                    self.time_at(rect.left() + HEADER_W, pos.x),
                );
            }
        }
        self.drag = None;
        self.press_origin = None;
        self.last_drag_pos = None;
        self.seeking = false;
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
                Self::lane_overlaps(timeline, linked)
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
        let overlaps = Self::lane_overlaps(timeline, &clip);
        if overlaps.is_empty() && Self::linked_lane_clear(timeline, id) {
            return;
        }
        // Attempt a swap from the original positions, not the overlapping
        // drag preview. The engine validates all linked placements before
        // mutating anything; rejection leaves the restored home untouched.
        Self::restore_move(timeline, id, home_start, home_track, home_linked);
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

    fn paint(
        &self,
        ui: &mut Ui,
        timeline: &Timeline,
        playhead: f64,
        selected: Option<ClipId>,
        visuals: &VisualMap<'_>,
        rect: Rect,
        content: Rect,
    ) {
        let painter = ui.painter().clone();
        let clipped = painter.with_clip_rect(content);
        let content_left = content.left();
        let lanes_top = rect.top() + RULER_H;

        // Lanes + headers.
        for track in 0..timeline.track_labels.len() {
            let y = lanes_top + track as f32 * (TRACK_H + TRACK_GAP);
            let lane = Rect::from_min_size(
                Pos2::new(content_left, y),
                Vec2::new(content.width(), TRACK_H),
            );
            clipped.rect_filled(lane, theme::R_SM, theme::BACKGROUND);

            let header = Rect::from_min_size(
                Pos2::new(rect.left(), y),
                Vec2::new(HEADER_W - 8.0, TRACK_H),
            );
            painter.rect_filled(header, theme::R_SM, theme::CARD);
            // Lane kind stripe: the colour of the clips it mostly holds.
            let kind_color = timeline
                .clips
                .iter()
                .find(|clip| clip.track == track)
                .map(|clip| match clip.kind {
                    ClipKind::Video => theme::CLIP_VIDEO,
                    ClipKind::Audio => theme::CLIP_AUDIO,
                    ClipKind::Title => theme::CLIP_TITLE,
                })
                .unwrap_or(theme::BORDER_STRONG);
            painter.rect_filled(
                Rect::from_min_size(header.left_top(), Vec2::new(3.0, TRACK_H)),
                egui::CornerRadius {
                    nw: theme::R_SM,
                    sw: theme::R_SM,
                    ne: 0,
                    se: 0,
                },
                kind_color.gamma_multiply(1.5),
            );
            painter.text(
                Pos2::new(rect.left() + 14.0, y + TRACK_H / 2.0),
                Align2::LEFT_CENTER,
                &timeline.track_labels[track],
                FontId::proportional(12.0),
                theme::MUTED_FG,
            );
        }

        // Ruler.
        self.paint_ruler(&clipped, rect, content_left);

        // Hovered clip brightens so the track feels responsive pre-click.
        let hovered = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| rect.contains(*p) && p.x >= content_left - EDGE_PX)
            .and_then(|p| self.hit_clip(timeline, rect, p));
        let hovered_clip = hovered.map(|(clip, _)| clip.id);
        if let Some((_, hit)) = hovered {
            ui.ctx().set_cursor_icon(match hit {
                ClipHit::Body => egui::CursorIcon::Grab,
                ClipHit::Left | ClipHit::Right => egui::CursorIcon::ResizeHorizontal,
            });
        }

        // Clips.
        for clip in &timeline.clips {
            let clip_rect = self.clip_rect(rect, clip);
            if clip_rect.right() < content.left() || clip_rect.left() > content.right() {
                continue;
            }
            let fill = match clip.kind {
                ClipKind::Video => theme::CLIP_VIDEO,
                ClipKind::Audio => theme::CLIP_AUDIO,
                ClipKind::Title => theme::CLIP_TITLE,
            };
            let is_selected = selected == Some(clip.id);
            let is_hovered = hovered_clip == Some(clip.id);
            let fill = if is_hovered && !is_selected {
                fill.gamma_multiply(1.35)
            } else {
                fill
            };
            let stroke = if is_selected {
                Stroke::new(2.0_f32, theme::BRAND)
            } else if is_hovered {
                Stroke::new(1.0_f32, Color32::from_white_alpha(110))
            } else {
                // At rest clips separate by fill alone, no hard outline.
                Stroke::new(1.0_f32, Color32::from_black_alpha(80))
            };
            clipped.rect(
                clip_rect,
                theme::R_SM,
                fill,
                stroke,
                egui::StrokeKind::Outside,
            );

            let visual = clip
                .source
                .as_ref()
                .and_then(|src| visuals.get(src.as_str()));
            let has_filmstrip = visual.map(|v| !v.thumbs.is_empty()).unwrap_or(false);
            if clip.kind == ClipKind::Audio {
                if let Some(visual) = visual {
                    self.paint_waveform(&clipped, clip_rect, clip, visual);
                }
            } else if clip.kind == ClipKind::Video {
                if let Some(visual) = visual {
                    self.paint_filmstrip(&clipped, clip_rect, clip, visual);
                }
            }
            if clip_rect.width() > 48.0 && clip.kind != ClipKind::Audio {
                let max_chars = (clip_rect.width() / 6.2) as usize;
                let label: String = clip.name.chars().take(max_chars.max(1)).collect();
                if has_filmstrip {
                    let bg = Rect::from_min_size(
                        clip_rect.left_top(),
                        Vec2::new(
                            (label.len() as f32 * 6.2 + 14.0).min(clip_rect.width()),
                            20.0,
                        ),
                    );
                    clipped.rect_filled(bg, theme::R_SM, Color32::from_black_alpha(150));
                }
                clipped.text(
                    clip_rect.left_top() + Vec2::new(8.0, 4.0),
                    Align2::LEFT_TOP,
                    label,
                    FontId::proportional(11.5),
                    Color32::from_white_alpha(235),
                );
            }
        }

        // Live readout while moving/trimming a clip.
        if let Some((id, _)) = self.drag {
            if let (Some(clip), Some(pos)) =
                (timeline.clip(id), ui.input(|i| i.pointer.interact_pos()))
            {
                let label = format!(
                    "{} → {}  ·  src {}",
                    format_timecode(clip.start, timeline.fps),
                    format_timecode(clip.end(), timeline.fps),
                    format_timecode(clip.source_in, timeline.fps),
                );
                let box_rect = Rect::from_min_size(
                    Pos2::new(pos.x + 14.0, pos.y - 30.0),
                    Vec2::new(label.len() as f32 * 6.6 + 16.0, 20.0),
                );
                painter.rect_filled(box_rect, 6, Color32::from_black_alpha(200));
                painter.text(
                    box_rect.left_top() + Vec2::new(8.0, 4.0),
                    Align2::LEFT_TOP,
                    label,
                    FontId::monospace(10.5),
                    theme::FOREGROUND,
                );
            }
        }

        // Playhead.
        let px = self.x_at(content_left, playhead);
        if content.x_range().contains(px) {
            clipped.line_segment(
                [Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())],
                Stroke::new(1.5_f32, theme::PLAYHEAD),
            );
            let handle = Rect::from_center_size(
                Pos2::new(px, rect.top() + RULER_H / 2.0),
                Vec2::new(11.0, RULER_H - 6.0),
            );
            clipped.rect_filled(handle, 3, theme::PLAYHEAD);
        }
    }

    fn paint_ruler(&self, painter: &egui::Painter, rect: Rect, content_left: f32) {
        let steps = [0.25_f64, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0];
        let step = steps
            .iter()
            .copied()
            .find(|s| *s as f32 * self.px_per_sec >= 72.0)
            .unwrap_or(60.0);
        let first = (self.time_at(content_left, content_left) / step).floor() * step;
        let mut t = first;
        while self.x_at(content_left, t) <= rect.right() {
            let x = self.x_at(content_left, t);
            painter.line_segment(
                [
                    Pos2::new(x, rect.top() + RULER_H - 8.0),
                    Pos2::new(x, rect.top() + RULER_H),
                ],
                Stroke::new(1.0_f32, theme::BORDER_STRONG),
            );
            if t >= 0.0 {
                painter.text(
                    Pos2::new(x + 4.0, rect.top() + 4.0),
                    Align2::LEFT_TOP,
                    format!("{:02}:{:02}", (t / 60.0) as u64, (t % 60.0) as u64),
                    FontId::monospace(10.5),
                    theme::MUTED_FG,
                );
            }
            t += step;
        }
        painter.line_segment(
            [
                Pos2::new(content_left, rect.top() + RULER_H),
                Pos2::new(rect.right(), rect.top() + RULER_H),
            ],
            Stroke::new(1.0_f32, theme::BORDER),
        );
    }

    /// Real peak waveform, mapped through the clip's source window so
    /// trimming and splitting show the correct part of the audio.
    fn paint_waveform(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        clip: &Clip,
        visual: &ClipVisual<'_>,
    ) {
        if visual.peaks.is_empty() || visual.duration <= 0.0 {
            return;
        }
        let peaks_per_sec = visual.peaks.len() as f64 / visual.duration;
        let mid = rect.center().y;
        let max_amp = rect.height() / 2.0 - 3.0;
        let mut x = rect.left() + 2.0;
        while x < rect.right() - 2.0 {
            let source_time = clip.source_in + (x - rect.left()) as f64 / self.px_per_sec as f64;
            let idx = (source_time * peaks_per_sec) as usize;
            if let Some(peak) = visual.peaks.get(idx) {
                let amp = (peak * max_amp).max(1.0);
                painter.line_segment(
                    [Pos2::new(x, mid - amp), Pos2::new(x, mid + amp)],
                    Stroke::new(2.0_f32, Color32::from_white_alpha(110)),
                );
            }
            x += 4.0;
        }
    }

    /// Tiled thumbnails chosen by the source time under each tile, so the
    /// strip tracks trims like the waveform does.
    fn paint_filmstrip(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        clip: &Clip,
        visual: &ClipVisual<'_>,
    ) {
        if visual.thumbs.is_empty() {
            return;
        }
        let first_size = visual.thumbs[0].1.size_vec2();
        let aspect = if first_size.y > 0.0 {
            first_size.x / first_size.y
        } else {
            16.0 / 9.0
        };
        let tile_w = (rect.height() * aspect).max(12.0);
        let tint = Color32::from_white_alpha((clip.opacity.clamp(0.0, 1.0) * 230.0) as u8);
        for tile in filmstrip_tiles(clip.source_in, clip.duration, self.px_per_sec, tile_w) {
            let dest = Rect::from_min_size(
                Pos2::new(rect.left() + tile.x, rect.top()),
                Vec2::new(tile.w, rect.height()),
            );
            let idx = visual
                .thumbs
                .partition_point(|(t, _)| *t <= tile.center_source)
                .saturating_sub(1);
            let uv = Rect::from_min_max(Pos2::new(tile.uv.0, 0.0), Pos2::new(tile.uv.1, 1.0));
            painter.image(visual.thumbs[idx].1.id(), dest, uv, tint);
            // Fragment separators so frames read as discrete frames.
            if tile.x > 0.5 {
                painter.line_segment(
                    [
                        Pos2::new(dest.left(), dest.top()),
                        Pos2::new(dest.left(), dest.bottom()),
                    ],
                    Stroke::new(1.0_f32, Color32::from_black_alpha(110)),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn run_frames(
        ctx: &egui::Context,
        view: &mut TimelineView,
        timeline: &mut Timeline,
        playhead: &mut f64,
        selected: &mut Option<ClipId>,
        visuals: &VisualMap<'_>,
        frames: &[Vec<egui::Event>],
    ) -> egui::CursorIcon {
        let mut cursor = egui::CursorIcon::Default;
        for events in frames {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 400.0))),
                // A test can inspect a held gesture between frame batches.
                time: Some(ctx.input(|i| i.time) + 0.016),
                events: events.clone(),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        view.show(ui, timeline, playhead, selected, visuals);
                    });
            });
            cursor = output.platform_output.cursor_icon;
        }
        cursor
    }

    fn pointer_button(x: f32, y: f32, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: Pos2::new(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn clip_center(view: &TimelineView, timeline: &Timeline, id: ClipId) -> Pos2 {
        let clip = timeline.clip(id).unwrap();
        Pos2::new(
            view.x_at(HEADER_W, clip.start + clip.duration / 2.0),
            RULER_H + clip.track as f32 * (TRACK_H + TRACK_GAP) + TRACK_H / 2.0,
        )
    }

    fn pointer_drag(
        view: &mut TimelineView,
        timeline: &mut Timeline,
        from: Pos2,
        to: Pos2,
    ) -> Option<ClipId> {
        let ctx = egui::Context::default();
        let mut playhead = 0.0;
        let mut selected = None;
        run_frames(
            &ctx,
            view,
            timeline,
            &mut playhead,
            &mut selected,
            &VisualMap::new(),
            &[
                vec![],
                vec![pointer_button(from.x, from.y, true)],
                vec![egui::Event::PointerMoved(to)],
                vec![pointer_button(to.x, to.y, false)],
            ],
        );
        assert!(!view.is_interacting(), "release must end the drag");
        assert!(view.last_drag_pos.is_none());
        selected
    }

    #[test]
    fn move_magnet_uses_start_target_when_end_has_no_target() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
        let target = timeline.add_media_clip("target", ClipKind::Video, 1, 8.0, 2.0, 0.0, "target");
        let target_before = timeline.clip(target).unwrap().clone();
        let mut view = TimelineView::default();
        let from = clip_center(&view, &timeline, id);
        let to = Pos2::new(view.x_at(HEADER_W, 7.9 + 0.5), from.y);
        pointer_drag(&mut view, &mut timeline, from, to);
        assert_eq!(timeline.clip(id).unwrap().start, 8.0);
        assert_eq!(timeline.clip(target).unwrap(), &target_before);
    }

    #[test]
    fn move_magnet_uses_end_target_when_start_has_no_target() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("move", ClipKind::Video, 0, 2.0, 1.0, 0.2, "move");
        timeline.add_media_clip("target", ClipKind::Video, 1, 8.0, 2.0, 0.0, "target");
        let mut view = TimelineView::default();
        let from = clip_center(&view, &timeline, id);
        let to = Pos2::new(view.x_at(HEADER_W, 6.9 + 0.5), from.y);
        pointer_drag(&mut view, &mut timeline, from, to);
        assert_eq!(timeline.clip(id).unwrap().end(), 8.0);
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
            let mut view = TimelineView {
                magnet,
                ..Default::default()
            };
            let from = clip_center(&view, &timeline, id);
            let to = Pos2::new(view.x_at(HEADER_W, raw + 0.5), from.y);
            pointer_drag(&mut view, &mut timeline, from, to);
            assert!(
                (timeline.clip(id).unwrap().start - expected).abs() < 1e-9,
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
            let mut view = TimelineView {
                magnet: false,
                ..Default::default()
            };
            let from = clip_center(&view, &timeline, id);
            let selected =
                pointer_drag(&mut view, &mut timeline, from, from + Vec2::new(48.0, 0.0));
            let clip = timeline.clip(id).unwrap();
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
            let mut view = TimelineView::default();
            let from = clip_center(&view, &timeline, id);
            pointer_drag(&mut view, &mut timeline, from, from + Vec2::new(0.0, 50.0));
            let clip = timeline.clip(id).unwrap();
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
        let mut view = TimelineView {
            magnet: false,
            ..Default::default()
        };
        let from = clip_center(&view, &timeline, id);
        assert_eq!(
            pointer_drag(&mut view, &mut timeline, from, from + Vec2::new(0.0, 50.0)),
            Some(id)
        );
        assert_eq!(timeline.clip(id).unwrap().track, 1);
        assert_eq!(timeline.clip(id).unwrap().duration, 1.0 / 120.0);
        for other in [left, right] {
            assert_eq!(timeline.clip(other), before.clip(other));
        }
    }

    #[test]
    fn short_clip_hover_cursor_matches_move_body() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.2, 0.157, "scene");
        let mut view = TimelineView::default();
        let pos = clip_center(&view, &timeline, id);
        let cursor = run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut 0.0,
            &mut None,
            &VisualMap::new(),
            &[vec![], vec![egui::Event::PointerMoved(pos)]],
        );
        assert_eq!(cursor, egui::CursorIcon::Grab);
    }

    #[test]
    fn short_clip_edges_still_trim_when_zoomed_in() {
        for left in [true, false] {
            let mut timeline = Timeline::empty_project();
            let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 1.0, 0.2, 0.157, "scene");
            let mut view = TimelineView {
                px_per_sec: 300.0,
                magnet: false,
                ..Default::default()
            };
            let edge = if left { 1.0 } else { 1.2 };
            let from = Pos2::new(
                view.x_at(HEADER_W, edge) + if left { 3.0 } else { -3.0 },
                49.0,
            );
            let to = from + Vec2::new(if left { 10.0 } else { -10.0 }, 0.0);
            pointer_drag(&mut view, &mut timeline, from, to);
            let clip = timeline.clip(id).unwrap();
            let expected_start = if left { 31.0 / 30.0 } else { 1.0 };
            let expected_end = if left { 1.2 } else { 35.0 / 30.0 };
            assert!((clip.start - expected_start).abs() < 1e-9);
            assert!((clip.end() - expected_end).abs() < 1e-9);
            assert!((clip.source_in - (0.157 + expected_start - 1.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn move_release_uses_final_pointer_position() {
        for pointer_gone in [false, true] {
            let ctx = egui::Context::default();
            let (mut timeline, id) = clip_fixture();
            let mut view = TimelineView {
                magnet: false,
                ..Default::default()
            };
            let mut playhead = 0.0;
            let mut selected = None;
            let mut release = vec![
                egui::Event::PointerMoved(Pos2::new(480.0, 49.0)),
                pointer_button(480.0, 49.0, false),
            ];
            if pointer_gone {
                release.push(egui::Event::PointerGone);
            }
            run_frames(
                &ctx,
                &mut view,
                &mut timeline,
                &mut playhead,
                &mut selected,
                &VisualMap::new(),
                &[
                    vec![],
                    vec![pointer_button(384.0, 99.0, true)],
                    vec![egui::Event::PointerMoved(Pos2::new(432.0, 99.0))],
                    release,
                ],
            );
            let clip = timeline.clip(id).unwrap();
            assert_eq!(
                (clip.track, clip.start, clip.duration, clip.source_in),
                (0, 4.0, 6.0, 3.0),
                "pointer_gone={pointer_gone}"
            );
            assert!(!view.is_interacting());
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
            let mut view = TimelineView {
                px_per_sec: 240.0,
                magnet: false,
                ..Default::default()
            };
            let id = if drag_left { left } else { right };
            let from = clip_center(&view, &timeline, id);
            let raw = if drag_left { 2.0 + right_duration } else { 2.0 };
            let to = Pos2::new(
                view.x_at(HEADER_W, raw + timeline.clip(id).unwrap().duration / 2.0),
                from.y,
            );
            pointer_drag(&mut view, &mut timeline, from, to);
            assert!((timeline.clip(left).unwrap().start - (2.0 + right_duration)).abs() < 1e-9, "left_duration={left_duration} right_duration={right_duration} drag_left={drag_left}");
            assert_eq!(timeline.clip(right).unwrap().start, 2.0);
            for id in [left, right] {
                let clip = timeline.clip(id).unwrap();
                let original = before.clip(id).unwrap();
                assert_eq!(
                    (clip.duration, clip.source_in),
                    (original.duration, original.source_in)
                );
            }
            assert_eq!(timeline.clip(tail), before.clip(tail));
        }
    }

    #[test]
    fn move_drop_adjacent_with_gap_preserves_gap_and_neighbors() {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip("a", ClipKind::Video, 0, 2.0, 0.4, 0.0, "a");
        let b = timeline.add_media_clip("b", ClipKind::Video, 0, 2.6, 0.2, 0.0, "b");
        let tail = timeline.add_media_clip("tail", ClipKind::Video, 0, 2.8, 1.0, 0.0, "tail");
        let tail_before = timeline.clip(tail).unwrap().clone();
        let mut view = TimelineView {
            px_per_sec: 120.0,
            magnet: false,
            ..Default::default()
        };
        let from = clip_center(&view, &timeline, a);
        let to = Pos2::new(view.x_at(HEADER_W, 2.6), from.y);
        pointer_drag(&mut view, &mut timeline, from, to);
        assert!((timeline.clip(a).unwrap().start - 2.4).abs() < 1e-9);
        assert!((timeline.clip(a).unwrap().end() - 2.8).abs() < 1e-9);
        assert_eq!(timeline.clip(b).unwrap().start, 2.0);
        assert!(
            (timeline.clip(a).unwrap().start - timeline.clip(b).unwrap().end() - 0.2).abs() < 1e-9
        );
        assert_eq!(timeline.clip(tail).unwrap(), &tail_before);
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
            let mut view = TimelineView {
                px_per_sec: 120.0,
                magnet: false,
                ..Default::default()
            };
            let from = clip_center(&view, &timeline, id);
            let to = Pos2::new(view.x_at(HEADER_W, 5.0 + 0.209), 149.0);
            pointer_drag(&mut view, &mut timeline, from, to);
            assert_eq!(
                timeline, before,
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
        let mut view = TimelineView {
            magnet: false,
            ..Default::default()
        };
        let from = clip_center(&view, &timeline, id);
        pointer_drag(&mut view, &mut timeline, from, from + Vec2::new(96.0, 0.0));
        assert_eq!(timeline, before);
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
        let mut view = TimelineView {
            magnet: false,
            ..Default::default()
        };
        let from = clip_center(&view, &timeline, a);
        pointer_drag(&mut view, &mut timeline, from, from + Vec2::new(96.0, 0.0));
        assert_eq!(
            timeline, before,
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
        let mut view = TimelineView {
            px_per_sec: 240.0,
            magnet: false,
            ..Default::default()
        };
        let from = clip_center(&view, &timeline, a);
        let to = Pos2::new(view.x_at(HEADER_W, 2.5), from.y);
        pointer_drag(&mut view, &mut timeline, from, to);
        for (id, expected_start) in [(a, 2.4), (a_audio, 2.4), (b, 2.0), (b_audio, 2.0)] {
            let clip = timeline.clip(id).unwrap();
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
        let mut view = TimelineView::default();
        let from = clip_center(&view, &timeline, id);
        pointer_drag(&mut view, &mut timeline, from, from + Vec2::new(7.2, 0.0));
        assert_eq!(timeline.clip(id).unwrap().start, 65.0 / 30.0);
        assert_eq!(
            timeline.clip(audio).unwrap().start,
            timeline.clip(id).unwrap().start
        );
    }

    #[test]
    fn move_drop_new_lane_with_linked_collision_reverts_without_growing() {
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 1.0, 0.157, "scene");
        timeline.detach_audio(id).unwrap();
        timeline.add_media_clip("blocker", ClipKind::Audio, 1, 4.0, 1.0, 0.0, "blocker");
        let before = timeline.clone();
        let mut view = TimelineView {
            magnet: false,
            ..Default::default()
        };
        let from = clip_center(&view, &timeline, id);
        let to = Pos2::new(from.x + 96.0, 170.0);
        pointer_drag(&mut view, &mut timeline, from, to);
        assert_eq!(timeline, before);
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
                    let mut view = TimelineView {
                        magnet,
                        ..Default::default()
                    };
                    let id = if drag_left { left } else { right };
                    let from = clip_center(&view, &timeline, id);
                    let raw = if drag_left { 4.0 + right_duration } else { 4.0 };
                    let to = Pos2::new(
                        view.x_at(HEADER_W, raw + timeline.clip(id).unwrap().duration / 2.0),
                        from.y,
                    );
                    assert_eq!(pointer_drag(&mut view, &mut timeline, from, to), Some(id));
                    assert!(
                        (timeline.clip(left).unwrap().start - (4.0 + right_duration)).abs() < 1e-9
                    );
                    assert_eq!(timeline.clip(right).unwrap().start, 4.0);
                    assert_eq!(timeline.clips.len(), 82);
                    for clip in &timeline.clips {
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
                let mut view = TimelineView {
                    magnet,
                    ..Default::default()
                };
                let (dragged, target) = if drag_left {
                    (left, right)
                } else {
                    (right, left)
                };
                let from = clip_center(&view, &timeline, dragged);
                let to = clip_center(&view, &timeline, target);
                pointer_drag(&mut view, &mut timeline, from, to);
                assert!(
                    (timeline.clip(left).unwrap().start - (2.0 + right_duration)).abs() < 1e-9,
                    "magnet={magnet} drag_left={drag_left}"
                );
                assert_eq!(timeline.clip(right).unwrap().start, 2.0);
                for clip in &timeline.clips {
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
                let mut view = TimelineView::default();
                let from = clip_center(&view, &timeline, id);
                let to = Pos2::new(from.x, if above { 10.0 } else { 120.0 });
                pointer_drag(&mut view, &mut timeline, from, to);
                let clip = timeline.clip(id).unwrap();
                assert_eq!(timeline.track_labels.len(), 2);
                assert_eq!(clip.track, if above { 0 } else { 1 });
                assert_eq!(
                    (clip.start, clip.duration, clip.source_in),
                    (2.0, 0.2, 0.157)
                );
                assert_eq!(
                    timeline.clip(other).unwrap().track,
                    if above { 1 } else { 0 }
                );
                assert_eq!(timeline.clip(other).unwrap().start, 4.0);
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
            let mut view = TimelineView::default();
            let from = clip_center(&view, &timeline, id);
            pointer_drag(
                &mut view,
                &mut timeline,
                from,
                from + Vec2::new(0.0, target as f32 * 50.0),
            );
            let mut expected = before;
            if target == 2 {
                expected.clip_mut(id).unwrap().track = 2;
            }
            assert_eq!(timeline, expected);
            assert_eq!(
                timeline.clip(id).unwrap().start,
                timeline.clip(audio).unwrap().start
            );
        }
    }

    #[test]
    fn hover_and_press_ignore_the_gap_outside_painted_clip_height() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 1.0, 0.157, "scene");
        let before = timeline.clone();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = Some(id);
        let pos = Pos2::new(264.0, 73.0);
        let cursor = run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &VisualMap::new(),
            &[
                vec![],
                vec![egui::Event::PointerMoved(pos)],
                vec![pointer_button(pos.x, pos.y, true)],
                vec![pointer_button(pos.x, pos.y, false)],
            ],
        );
        assert_eq!(cursor, egui::CursorIcon::Default);
        assert_eq!(selected, None);
        assert_eq!(timeline, before);
        assert!(!view.is_interacting());
    }

    #[test]
    fn trim_release_uses_final_pointer_position() {
        for left in [false, true] {
            let ctx = egui::Context::default();
            let (mut timeline, id) = clip_fixture();
            let mut view = TimelineView {
                magnet: false,
                ..Default::default()
            };
            let (press_x, delta) = if left { (246.0, 24.0) } else { (522.0, -24.0) };
            run_frames(
                &ctx,
                &mut view,
                &mut timeline,
                &mut 0.0,
                &mut None,
                &VisualMap::new(),
                &[
                    vec![],
                    vec![pointer_button(press_x, 99.0, true)],
                    vec![egui::Event::PointerMoved(Pos2::new(press_x + delta, 99.0))],
                    vec![
                        egui::Event::PointerMoved(Pos2::new(press_x + 2.0 * delta, 99.0)),
                        pointer_button(press_x + 2.0 * delta, 99.0, false),
                    ],
                ],
            );
            let clip = timeline.clip(id).unwrap();
            assert_eq!(
                (clip.start, clip.end(), clip.source_in),
                if left {
                    (3.0, 8.0, 4.0)
                } else {
                    (2.0, 7.0, 3.0)
                }
            );
            assert!(!view.is_interacting());
        }
    }

    #[test]
    fn pointer_drag_started_outside_timeline_does_not_move_a_clip() {
        let ctx = egui::Context::default();
        let (mut timeline, id) = clip_fixture();
        let before = timeline.clone();
        let mut view = TimelineView::default();
        let mut selected = Some(id);
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut 0.0,
            &mut selected,
            &VisualMap::new(),
            &[
                vec![],
                vec![pointer_button(384.0, 420.0, true)],
                vec![
                    egui::Event::PointerMoved(Pos2::new(432.0, 99.0)),
                    pointer_button(432.0, 99.0, false),
                ],
            ],
        );
        assert_eq!(timeline, before);
        assert_eq!(selected, Some(id));
        assert!(!view.is_interacting());
        assert!(view.press_origin.is_none());
    }

    #[test]
    fn short_clip_drag_can_cross_threshold_on_release_frame() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let id = timeline.add_media_clip("scene", ClipKind::Video, 0, 2.0, 0.2, 0.157, "scene");
        timeline.append_track();
        let mut view = TimelineView::default();
        let from = clip_center(&view, &timeline, id);
        let to = from + Vec2::new(48.0, 50.0);
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut 0.0,
            &mut None,
            &VisualMap::new(),
            &[
                vec![],
                vec![pointer_button(from.x, from.y, true)],
                vec![
                    egui::Event::PointerMoved(to),
                    pointer_button(to.x, to.y, false),
                ],
            ],
        );
        let clip = timeline.clip(id).unwrap();
        assert_eq!(
            (clip.track, clip.start, clip.duration, clip.source_in),
            (1, 3.0, 0.2, 0.157)
        );
        assert!(!view.is_interacting());
    }

    /// Full-length clip flush at t=0: grabbing its start edge from a few
    /// pixels inside the header gutter must still begin a trim.
    #[test]
    fn trim_left_of_full_length_clip_can_start_at_timeline_origin() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let clip_id = timeline.add_media_clip(
            "full.mp4",
            ClipKind::Video,
            1,
            0.0,
            20.0,
            0.0,
            "/media/full.mp4",
        );
        let visuals: VisualMap<'_> = [(
            "/media/full.mp4",
            ClipVisual {
                duration: 20.0,
                peaks: &[],
                thumbs: &[],
            },
        )]
        .into();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0; // V1 lane centre for a frameless 1200x400 panel.
        let frames = vec![
            vec![],
            vec![pointer_button(141.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(170.0, y))],
            vec![egui::Event::PointerMoved(Pos2::new(210.0, y))],
            vec![pointer_button(210.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        let clip = timeline.clip(clip_id).unwrap();
        assert!(
            clip.start > 0.5,
            "start edge trim never began: start={} source_in={}",
            clip.start,
            clip.source_in
        );
        assert!((clip.end() - 20.0).abs() < 1e-6);
    }

    #[test]
    fn trim_right_uses_press_origin_not_drag_threshold_position() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let clip_id = timeline.add_media_clip(
            "clip.mp4",
            ClipKind::Video,
            1,
            2.0,
            4.0,
            0.0,
            "/media/full.mp4",
        );
        let visuals: VisualMap<'_> = [(
            "/media/full.mp4",
            ClipVisual {
                duration: 20.0,
                peaks: &[],
                thumbs: &[],
            },
        )]
        .into();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Clip spans x=240..432 at 48px/s. Press 3px inside its right edge,
        // then drag well inside the clip: this must trim, not move.
        let frames = vec![
            vec![],
            vec![pointer_button(429.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(390.0, y))],
            vec![egui::Event::PointerMoved(Pos2::new(340.0, y))],
            vec![pointer_button(340.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        let clip = timeline.clip(clip_id).unwrap();
        assert_eq!(clip.start, 2.0, "right-edge trim must not move the clip");
        assert!(clip.end() < 5.0, "end did not trim: {}", clip.end());
    }

    fn clip_fixture() -> (Timeline, ClipId) {
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
        (timeline, id)
    }

    fn full_visuals() -> VisualMap<'static> {
        [(
            "/media/full.mp4",
            ClipVisual {
                duration: 20.0,
                peaks: &[],
                thumbs: &[],
            },
        )]
        .into()
    }

    /// Grabbing 6px inside the left edge and dragging 24px must move the
    /// edge by the drag delta (0.5s), not jump it to the pointer time.
    #[test]
    fn trim_left_respects_grab_offset() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Left edge at x=240 (t=2s). Press at 246, drag to 270.
        let frames = vec![
            vec![],
            vec![pointer_button(246.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(270.0, y))],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert!(
            view.is_interacting(),
            "left trim must stay active while held"
        );
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &[vec![pointer_button(270.0, y, false)]],
        );
        assert!(!view.is_interacting(), "left trim must end on release");
        let clip = timeline.clip(clip_id).unwrap();
        assert!(
            (clip.start - 2.5).abs() < 0.02,
            "expected start 2.5 from delta drag, got {}",
            clip.start
        );
        assert!((clip.end() - 8.0).abs() < 1e-6);
        assert!((clip.source_in - 3.5).abs() < 0.02);
    }

    /// Left-trimming a clip that follows another clip must keep the
    /// trimmed clip's end fixed (the engine trim contract). Regression:
    /// on drag release, the magnet's close_gaps rippled the trimmed
    /// clip back to the previous clip's end, so the trimmed amount
    /// came off the end as well.
    #[test]
    fn trim_left_after_another_clip_keeps_end_fixed() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let a_id =
            timeline.add_media_clip("a.mp4", ClipKind::Video, 1, 0.0, 10.0, 0.0, "/media/a.mp4");
        let b_id =
            timeline.add_media_clip("b.mp4", ClipKind::Video, 1, 12.0, 10.0, 0.0, "/media/b.mp4");
        let visuals: VisualMap<'_> = [
            (
                "/media/a.mp4",
                ClipVisual {
                    duration: 30.0,
                    peaks: &[],
                    thumbs: &[],
                },
            ),
            (
                "/media/b.mp4",
                ClipVisual {
                    duration: 30.0,
                    peaks: &[],
                    thumbs: &[],
                },
            ),
        ]
        .into();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // B spans t=12..22, so its left edge is x=720 (144 + 12*48).
        // Drag the edge 144px right (+3s) and release.
        let frames = vec![
            vec![],
            vec![pointer_button(720.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(864.0, y))],
            vec![pointer_button(864.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        let a = timeline.clip(a_id).unwrap();
        assert_eq!(a.start, 0.0);
        assert!((a.end() - 10.0).abs() < 1e-6);
        let b = timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 15.0).abs() < 0.02,
            "expected start 15 after +3s left trim, got {}",
            b.start
        );
        assert!(
            (b.end() - 22.0).abs() < 1e-6,
            "left trim must keep the end fixed, got {}",
            b.end()
        );
        assert!((b.source_in - 3.0).abs() < 0.02);
    }

    #[test]
    fn trim_right_respects_grab_offset() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Right edge at x=528 (t=8s). Press at 522, drag to 498 (-0.5s).
        let frames = vec![
            vec![],
            vec![pointer_button(522.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(498.0, y))],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert!(
            view.is_interacting(),
            "right trim must stay active while held"
        );
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &[vec![pointer_button(498.0, y, false)]],
        );
        assert!(!view.is_interacting(), "right trim must end on release");
        let clip = timeline.clip(clip_id).unwrap();
        assert_eq!(clip.start, 2.0);
        assert!(
            (clip.end() - 7.5).abs() < 0.02,
            "expected end 7.5 from delta drag, got {}",
            clip.end()
        );
    }

    fn touching_fixture() -> (Timeline, ClipId, ClipId) {
        let mut timeline = Timeline::empty_project();
        let a = timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            1,
            2.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        let b = timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            1,
            8.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        (timeline, a, b)
    }

    /// Regression: two clips touching at t=8 share an edge at x=528.
    /// Pressing just right of the edge must trim the right clip's
    /// left edge. The old first-match hit test always grabbed the
    /// left clip, so the right clip could not be trimmed at all.
    #[test]
    fn touching_clips_right_clip_left_edge_trims() {
        let ctx = egui::Context::default();
        let (mut timeline, a_id, b_id) = touching_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        view.magnet = false; // isolate hit-testing from ripple-on-release
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Press 5px right of the shared edge, drag +48px (+1s).
        let frames = vec![
            vec![],
            vec![pointer_button(533.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(581.0, y))],
            vec![pointer_button(581.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(selected, Some(b_id));
        let a = timeline.clip(a_id).unwrap();
        assert!(
            (a.start - 2.0).abs() < 1e-6 && (a.end() - 8.0).abs() < 1e-6,
            "left clip must not change, got {}..{}",
            a.start,
            a.end()
        );
        let b = timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 9.0).abs() < 0.02,
            "right clip left edge should trim to 9, got {}",
            b.start
        );
        assert!((b.end() - 14.0).abs() < 1e-6, "end fixed, got {}", b.end());
        assert!((b.source_in - 1.0).abs() < 0.02);
    }

    /// Pressing just left of a shared edge trims the left clip's
    /// right edge and leaves the right clip alone.
    #[test]
    fn touching_clips_left_clip_right_edge_trims() {
        let ctx = egui::Context::default();
        let (mut timeline, a_id, b_id) = touching_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        view.magnet = false; // isolate hit-testing from ripple-on-release
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Press 5px left of the shared edge, drag -48px (-1s).
        let frames = vec![
            vec![],
            vec![pointer_button(523.0, y, true)],
            vec![egui::Event::PointerMoved(Pos2::new(475.0, y))],
            vec![pointer_button(475.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(selected, Some(a_id));
        let a = timeline.clip(a_id).unwrap();
        assert!(
            (a.end() - 7.0).abs() < 0.02,
            "left clip right edge should trim to 7, got {}",
            a.end()
        );
        let b = timeline.clip(b_id).unwrap();
        assert!(
            (b.start - 8.0).abs() < 1e-6 && (b.end() - 14.0).abs() < 1e-6,
            "right clip must not change, got {}..{}",
            b.start,
            b.end()
        );
    }

    /// Dropping a clip onto another clip on the same lane swaps
    /// their positions instead of overlapping or refusing to move.
    #[test]
    fn move_drop_onto_neighbor_swaps_positions() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let a_id = timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            0,
            2.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        let b_id = timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            0,
            10.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        // Grab A mid-clip (t=5, x=384) and drop it on B (pointer t=13, x=768).
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, 49.0, true)],
            vec![egui::Event::PointerMoved(Pos2::new(768.0, 49.0))],
            vec![pointer_button(768.0, 49.0, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        let a = timeline.clip(a_id).unwrap();
        let b = timeline.clip(b_id).unwrap();
        assert!(
            (a.start - 10.0).abs() < 0.02,
            "dragged clip takes the drop spot, got {}",
            a.start
        );
        assert!(
            (b.start - 2.0).abs() < 0.02,
            "displaced clip takes the dragged home spot, got {}",
            b.start
        );
    }

    /// A non-adjacent target inside a multi-clip overlap is ambiguous,
    /// so the dragged clip returns home and nobody overlaps.
    #[test]
    fn move_drop_overlapping_two_clips_returns_home() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let a_id = timeline.add_media_clip(
            "a.mp4",
            ClipKind::Video,
            0,
            2.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        let b_id = timeline.add_media_clip(
            "b.mp4",
            ClipKind::Video,
            0,
            10.0,
            2.0,
            0.0,
            "/media/full.mp4",
        );
        let c_id = timeline.add_media_clip(
            "c.mp4",
            ClipKind::Video,
            0,
            12.0,
            2.0,
            0.0,
            "/media/full.mp4",
        );
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        // Drop A so it spans both B and C (pointer t=12, x=720 -> start 9).
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, 49.0, true)],
            vec![egui::Event::PointerMoved(Pos2::new(720.0, 49.0))],
            vec![pointer_button(720.0, 49.0, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        let a = timeline.clip(a_id).unwrap();
        assert!(
            (a.start - 2.0).abs() < 0.02,
            "ambiguous drop returns home, got {}",
            a.start
        );
        assert_eq!(timeline.clip(b_id).unwrap().start, 10.0);
        assert_eq!(timeline.clip(c_id).unwrap().start, 12.0);
    }

    #[test]
    fn move_drag_changes_lane() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        // Clip sits on track 1 (row center y=99). Drag up to track 0 (y=49).
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, 99.0, true)],
            vec![egui::Event::PointerMoved(Pos2::new(384.0, 49.0))],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert!(view.is_interacting(), "move must stay active while held");
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &[vec![pointer_button(384.0, 49.0, false)]],
        );
        assert!(!view.is_interacting(), "move must end on release");
        let clip = timeline.clip(clip_id).unwrap();
        assert_eq!(clip.track, 0, "clip should move up a lane");
        assert!(
            (clip.start - 2.0).abs() < 0.02,
            "start should not drift, got {}",
            clip.start
        );
    }

    #[test]
    fn move_drag_below_last_lane_appends_track() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let clip_id = timeline.add_media_clip(
            "clip.mp4",
            ClipKind::Video,
            0,
            2.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        assert_eq!(timeline.track_labels.len(), 1);
        // Drag from the only lane (y=49) to below it (y=120) and drop.
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, 49.0, true)],
            vec![egui::Event::PointerMoved(Pos2::new(384.0, 120.0))],
            vec![pointer_button(384.0, 120.0, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(timeline.track_labels.len(), 2);
        assert_eq!(timeline.clip(clip_id).unwrap().track, 1);
    }

    #[test]
    fn move_drag_above_first_lane_inserts_track_on_top() {
        let ctx = egui::Context::default();
        let mut timeline = Timeline::empty_project();
        let clip_id = timeline.add_media_clip(
            "clip.mp4",
            ClipKind::Video,
            0,
            2.0,
            6.0,
            0.0,
            "/media/full.mp4",
        );
        let other_id = timeline.add_media_clip(
            "other.mp4",
            ClipKind::Video,
            0,
            10.0,
            2.0,
            0.0,
            "/media/full.mp4",
        );
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        // Drag from the only lane (y=49) up into the ruler (y=10) and drop.
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, 49.0, true)],
            vec![egui::Event::PointerMoved(Pos2::new(384.0, 10.0))],
            vec![pointer_button(384.0, 10.0, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(timeline.track_labels.len(), 2);
        assert_eq!(timeline.clip(clip_id).unwrap().track, 0);
        assert_eq!(timeline.clip(other_id).unwrap().track, 1);
    }

    #[test]
    fn clip_click_moves_playhead_and_releases_interaction() {
        // Click the body and both trim handles without moving the pointer.
        for (x, time) in [(384.0, 5.0), (246.0, 2.125), (522.0, 7.875)] {
            let ctx = egui::Context::default();
            let (mut timeline, clip_id) = clip_fixture();
            let visuals = full_visuals();
            let mut view = TimelineView::default();
            let mut playhead = 0.0;
            let mut selected = None;
            let frames = vec![
                vec![],
                vec![pointer_button(x, 99.0, true)],
                vec![pointer_button(x, 99.0, false)],
            ];
            run_frames(
                &ctx,
                &mut view,
                &mut timeline,
                &mut playhead,
                &mut selected,
                &visuals,
                &frames,
            );
            assert_eq!(selected, Some(clip_id));
            assert_eq!(playhead, timeline.snap(time));
            let clip = timeline.clip(clip_id).unwrap();
            assert_eq!(
                (clip.start, clip.duration, clip.source_in, clip.track),
                (2.0, 6.0, 3.0, 1),
                "a plain click must not move or trim the clip"
            );
            assert!(
                !view.is_interacting(),
                "released clip click at x={x} must not leave a move or trim active"
            );
        }
    }

    #[test]
    fn ruler_click_moves_playhead_and_releases_interaction() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = Some(clip_id);
        let frames = vec![
            vec![],
            vec![pointer_button(432.0, 10.0, true)],
            vec![pointer_button(432.0, 10.0, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(playhead, 6.0);
        assert_eq!(selected, Some(clip_id), "ruler clicks preserve selection");
        assert!(
            !view.is_interacting(),
            "a released ruler click must not leave seeking active"
        );
    }

    /// Clicking empty space after selecting a clip must park the playhead,
    /// clear the selection, and leave transport free of any interaction.
    #[test]
    fn empty_lane_click_moves_playhead_and_clears_selection() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Click inside the clip at t=5 (x=384).
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, y, true)],
            vec![pointer_button(384.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(selected, Some(clip_id));
        assert!(
            (playhead - 5.0).abs() < 0.02,
            "playhead should jump to click, got {playhead}"
        );
        // Click empty lane at t=10 (x=624): playhead follows, selection clears.
        let frames = vec![
            vec![pointer_button(624.0, y, true)],
            vec![pointer_button(624.0, y, false)],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert_eq!(selected, None);
        assert!(
            (playhead - 10.0).abs() < 0.02,
            "playhead should jump to empty click, got {playhead}"
        );
        assert!(
            !view.is_interacting(),
            "an empty-lane click must not retain the preceding clip interaction"
        );
    }

    #[test]
    fn ruler_drag_seeks_until_release() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        let mut playhead = 0.0;
        let mut selected = Some(clip_id);
        let frames = vec![
            vec![],
            vec![pointer_button(384.0, 10.0, true)],
            vec![egui::Event::PointerMoved(Pos2::new(432.0, 10.0))],
        ];
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &frames,
        );
        assert!(
            view.is_interacting(),
            "ruler drag must stay active while held"
        );
        assert_eq!(playhead, 6.0);
        run_frames(
            &ctx,
            &mut view,
            &mut timeline,
            &mut playhead,
            &mut selected,
            &visuals,
            &[
                vec![egui::Event::PointerMoved(Pos2::new(528.0, 10.0))],
                vec![pointer_button(528.0, 10.0, false)],
            ],
        );
        assert_eq!(playhead, 8.0);
        assert_eq!(selected, Some(clip_id));
        assert!(!view.is_interacting(), "ruler drag must end on release");
    }

    #[test]
    fn move_drag_released_outside_finishes_and_clears_interaction() {
        for pointer_gone in [false, true] {
            let ctx = egui::Context::default();
            let (mut timeline, clip_id) = clip_fixture();
            let visuals = full_visuals();
            let mut view = TimelineView::default();
            let mut playhead = 0.0;
            let mut selected = None;
            let outside = Pos2::new(480.0, 420.0);
            let frames = vec![
                vec![],
                vec![pointer_button(384.0, 99.0, true)],
                vec![egui::Event::PointerMoved(Pos2::new(432.0, 99.0))],
                vec![egui::Event::PointerMoved(outside)],
            ];
            run_frames(
                &ctx,
                &mut view,
                &mut timeline,
                &mut playhead,
                &mut selected,
                &visuals,
                &frames,
            );
            assert!(!view.last_rect.unwrap().contains(outside));
            assert!(view.is_interacting(), "move must track outside while held");
            assert_eq!(timeline.track_labels.len(), 2);
            let mut release = vec![pointer_button(outside.x, outside.y, false)];
            if pointer_gone {
                release.push(egui::Event::PointerGone);
            }
            run_frames(
                &ctx,
                &mut view,
                &mut timeline,
                &mut playhead,
                &mut selected,
                &visuals,
                &[release],
            );
            assert!(
                !view.is_interacting(),
                "outside release must end the move (pointer_gone={pointer_gone})"
            );
            assert!(view.last_drag_pos.is_none());
            assert_eq!(selected, Some(clip_id));
            assert_eq!(timeline.track_labels.len(), 3);
            let clip = timeline.clip(clip_id).unwrap();
            assert_eq!(clip.track, 2, "outside drop must still append a lane");
            assert_eq!((clip.start, clip.duration, clip.source_in), (4.0, 6.0, 3.0));
        }
    }
}

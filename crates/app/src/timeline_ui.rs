//! Custom-painted timeline: ruler, lanes, clips, playhead.
//!
//! Geometry and hit-testing live here; all edits go through
//! `openreel_engine::Timeline` so the model stays frame-snapped.

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

/// Per-source visual data supplied by the app for one frame of painting.
pub struct ClipVisual<'a> {
    pub duration: f64,
    pub peaks: &'a [f32],
    pub thumbs: &'a [(f64, TextureHandle)],
}

pub type VisualMap<'a> = HashMap<&'a str, ClipVisual<'a>>;

#[derive(Clone, Copy)]
enum DragMode {
    Move {
        grab_offset: f64,
        home_start: f64,
        home_track: usize,
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

/// Snap `time` to the nearest candidate within `threshold` seconds.
fn snap_to(time: f64, candidates: &[f64], threshold: f64) -> f64 {
    candidates
        .iter()
        .copied()
        .filter(|c| (c - time).abs() <= threshold)
        .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
        .unwrap_or(time)
}

pub struct TimelineView {
    pub px_per_sec: f32,
    pub scroll_x: f32,
    pub magnet: bool,
    drag: Option<(ClipId, DragMode)>,
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
        let Some(pos) = response.interact_pointer_pos() else {
            if response.drag_stopped() {
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
        let press_pos = response
            .ctx
            .input(|i| i.pointer.press_origin())
            .unwrap_or(pos);
        // Ignore presses deep in the track-header gutter, but allow the
        // strip of gutter next to t=0 where a full-length clip's left edge
        // lives. While an interaction is live, always keep tracking.
        if press_pos.x < content_left - EDGE_PX && !self.seeking && self.drag.is_none() {
            return;
        }

        // Auto-scroll while dragging near the viewport edges so the far
        // edge of a long clip can be reached without letting go.
        if self.seeking || self.drag.is_some() {
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

        if response.drag_started() || response.clicked() {
            if press_pos.y < rect.top() + RULER_H {
                self.seeking = true;
                *playhead = timeline.snap(time);
            } else if let Some(track) = self.track_at(rect, press_pos.y) {
                // Clicking anywhere in a lane parks the playhead there;
                // split and transport act on the playhead from there.
                *playhead = timeline.snap(press_time);
                // Nearest edge wins, so the shared edge between two
                // touching clips can trim either side: a press left of
                // the edge grabs the left clip's right edge, a press
                // right of it grabs the right clip's left edge. First
                // match in the clip vec made one side ungrabbable.
                let mut best_edge: Option<(f32, u8, ClipId, DragMode)> = None;
                let mut containing: Option<(ClipId, f64, usize)> = None;
                for c in timeline.clips.iter().filter(|c| c.track == track) {
                    let left_x = self.x_at(content_left, c.start);
                    let right_x = self.x_at(content_left, c.end());
                    let edges = [
                        (
                            left_x,
                            DragMode::TrimLeft {
                                orig_start: c.start,
                                press_time,
                            },
                            press_pos.x >= left_x,
                        ),
                        (
                            right_x,
                            DragMode::TrimRight {
                                orig_end: c.end(),
                                press_time,
                            },
                            press_pos.x <= right_x,
                        ),
                    ];
                    for (x, mode, on_owning_side) in edges {
                        let dist = (press_pos.x - x).abs();
                        if dist <= EDGE_PX {
                            // At equal distance (a shared edge), prefer
                            // the edge whose clip the press is on.
                            let penalty = u8::from(!on_owning_side);
                            let replace = match best_edge {
                                None => true,
                                Some((d, p, _, _)) => dist < d || (dist == d && penalty < p),
                            };
                            if replace {
                                best_edge = Some((dist, penalty, c.id, mode));
                            }
                        }
                    }
                    if press_pos.x >= left_x && press_pos.x <= right_x {
                        containing = Some((c.id, c.start, c.track));
                    }
                }
                if let Some((_, _, id, mode)) = best_edge {
                    *selected = Some(id);
                    self.drag = Some((id, mode));
                } else if let Some((id, start, track)) = containing {
                    *selected = Some(id);
                    self.drag = Some((
                        id,
                        DragMode::Move {
                            grab_offset: press_time - start,
                            home_start: start,
                            home_track: track,
                        },
                    ));
                } else if response.clicked() {
                    *selected = None;
                }
            }
        }

        if response.dragged() {
            if self.seeking {
                *playhead = timeline.snap(time);
            } else if let Some((id, mode)) = self.drag {
                // Snap targets (magnet on): timeline start, playhead, other
                // clip edges. Magnet off means free placement on frame grid.
                let threshold = 8.0 / self.px_per_sec as f64;
                let mut candidates: Vec<f64> = Vec::new();
                if self.magnet {
                    candidates.extend([0.0, *playhead]);
                    for clip in timeline.clips.iter().filter(|c| c.id != id) {
                        candidates.push(clip.start);
                        candidates.push(clip.end());
                    }
                }
                let clip_snapshot = timeline.clip(id).cloned();
                match (mode, clip_snapshot) {
                    (DragMode::Move { grab_offset, .. }, Some(mut clip)) => {
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
                                if let Some(fresh) = timeline.clip(id).cloned() {
                                    clip = fresh;
                                }
                            }
                        }
                        let raw = time - grab_offset;
                        let by_start = snap_to(raw, &candidates, threshold);
                        let by_end =
                            snap_to(raw + clip.duration, &candidates, threshold) - clip.duration;
                        let snapped = if (by_start - raw).abs() <= (by_end - raw).abs() {
                            by_start
                        } else {
                            by_end
                        };
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

        if response.drag_stopped() {
            self.finish_drag(timeline, rect, Some(pos));
        }
    }

    /// End a drag. The magnet only snaps while dragging; gaps are
    /// never closed here. A Move dropped above the first lane or
    /// below the last grows a new lane there for the clip. A Move
    /// dropped onto one neighbor on its home lane swaps positions
    /// with it; any other overlap returns the clip home, so clips
    /// never overlap on a lane.
    fn finish_drag(&mut self, timeline: &mut Timeline, rect: Rect, drop_pos: Option<Pos2>) {
        if let (
            Some((
                id,
                DragMode::Move {
                    home_start,
                    home_track,
                    ..
                },
            )),
            Some(pos),
        ) = (self.drag, drop_pos)
        {
            let lanes_top = rect.top() + RULER_H;
            let lanes_bottom =
                lanes_top + timeline.track_labels.len() as f32 * (TRACK_H + TRACK_GAP) - TRACK_GAP;
            // Well past the edge counts as aiming for a new lane, so
            // slight pointer drift on a sideways drag of an edge-lane
            // clip does not grow one by accident. The ruler strip is
            // the only room above the first lane, hence the smaller
            // top threshold.
            if pos.y < lanes_top - RULER_H * 0.5 {
                timeline.insert_track_at(0);
                timeline.set_clip_track(id, 0);
            } else if pos.y > lanes_bottom + TRACK_H * 0.5 {
                let track = timeline.append_track();
                timeline.set_clip_track(id, track);
            } else {
                Self::resolve_move_drop(timeline, id, home_start, home_track);
            }
        }
        self.drag = None;
        self.last_drag_pos = None;
        self.seeking = false;
    }

    fn lane_overlaps(timeline: &Timeline, clip: &Clip) -> Vec<Clip> {
        timeline
            .clips
            .iter()
            .filter(|c| {
                c.id != clip.id
                    && c.track == clip.track
                    && c.start < clip.end()
                    && clip.start < c.end()
            })
            .cloned()
            .collect()
    }

    fn resolve_move_drop(timeline: &mut Timeline, id: ClipId, home_start: f64, home_track: usize) {
        let Some(clip) = timeline.clip(id).cloned() else {
            return;
        };
        let overlaps = Self::lane_overlaps(timeline, &clip);
        if overlaps.is_empty() {
            return;
        }
        let mut swapped = false;
        if overlaps.len() == 1 && clip.track == home_track {
            let partner = &overlaps[0];
            timeline.move_clip(id, partner.start);
            timeline.move_clip(partner.id, home_start);
            let clean = timeline
                .clip(id)
                .map(|c| Self::lane_overlaps(timeline, c).is_empty())
                .unwrap_or(false)
                && timeline
                    .clip(partner.id)
                    .map(|c| Self::lane_overlaps(timeline, c).is_empty())
                    .unwrap_or(false);
            if clean {
                swapped = true;
            } else {
                timeline.move_clip(partner.id, partner.start);
            }
        }
        if !swapped {
            timeline.set_clip_track(id, home_track);
            timeline.move_clip(id, home_start);
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
            let fill = if track % 2 == 0 {
                theme::CARD
            } else {
                theme::BACKGROUND
            };
            clipped.rect_filled(lane, 0, fill);

            let header =
                Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(HEADER_W, TRACK_H));
            painter.rect_filled(header, 0, theme::CARD);
            painter.text(
                Pos2::new(rect.left() + 12.0, y + TRACK_H / 2.0),
                Align2::LEFT_CENTER,
                &timeline.track_labels[track],
                FontId::proportional(12.0),
                theme::MUTED_FG,
            );
        }

        // Ruler.
        self.paint_ruler(&clipped, rect, content_left);

        // Hovered clip brightens so the track feels responsive pre-click.
        let hovered_clip = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| content.contains(*p))
            .and_then(|p| {
                let track = self.track_at(rect, p.y)?;
                let time = self.time_at(content_left, p.x);
                timeline
                    .clips
                    .iter()
                    .find(|c| c.track == track && c.contains(time))
                    .map(|c| c.id)
            });
        if let (Some(id), Some(pos)) = (hovered_clip, ui.input(|i| i.pointer.hover_pos())) {
            let near_edge = timeline.clip(id).map(|c| {
                (pos.x - self.x_at(content_left, c.start)).abs() <= EDGE_PX
                    || (pos.x - self.x_at(content_left, c.end())).abs() <= EDGE_PX
            }) == Some(true);
            ui.ctx().set_cursor_icon(if near_edge {
                egui::CursorIcon::ResizeHorizontal
            } else {
                egui::CursorIcon::Grab
            });
        }

        // Clips.
        for clip in &timeline.clips {
            let y = lanes_top + clip.track as f32 * (TRACK_H + TRACK_GAP) + 3.0;
            let clip_rect = Rect::from_min_max(
                Pos2::new(self.x_at(content_left, clip.start), y),
                Pos2::new(self.x_at(content_left, clip.end()), y + TRACK_H - 6.0),
            );
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
                Stroke::new(2.0_f32, theme::RING)
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
    ) {
        for (i, events) in frames.iter().enumerate() {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 400.0))),
                time: Some(i as f64 * 0.016),
                events: events.clone(),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        view.show(ui, timeline, playhead, selected, visuals);
                    });
            });
        }
    }

    fn pointer_button(x: f32, y: f32, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: Pos2::new(x, y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
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
            vec![pointer_button(270.0, y, false)],
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
            vec![pointer_button(498.0, y, false)],
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

    /// A drop overlapping more than one clip cannot swap cleanly,
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
            vec![pointer_button(384.0, 49.0, false)],
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

    /// Clicking in a lane moves the playhead to the click (split and
    /// transport act on the playhead from there) as well as selecting.
    #[test]
    fn lane_click_moves_playhead() {
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
    }
}

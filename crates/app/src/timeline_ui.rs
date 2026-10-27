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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TimelineTool {
    Select,
    Split,
}

#[derive(Clone, Copy)]
enum DragMode {
    Move { grab_offset: f64, orig_start: f64 },
    TrimLeft { orig_start: f64, press_time: f64 },
    TrimRight { orig_end: f64, press_time: f64 },
}

const MAIN_TRACK: usize = 1;

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
    pub tool: TimelineTool,
    pub magnet: bool,
    drag: Option<(ClipId, DragMode)>,
    seeking: bool,
    last_rect: Option<Rect>,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self {
            px_per_sec: 48.0,
            scroll_x: 0.0,
            tool: TimelineTool::Select,
            magnet: true,
            drag: None,
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
            "Click the ruler to seek. Drag clips to move, drag edges to trim. Ctrl+scroll zooms.",
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
                self.drag = None;
                self.seeking = false;
            }
            return;
        };
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
            } else if self.tool == TimelineTool::Split && response.clicked() {
                if let Some(track) = self.track_at(rect, press_pos.y) {
                    if let Some(id) = timeline
                        .clips
                        .iter()
                        .find(|c| c.track == track && c.contains(press_time))
                        .map(|c| c.id)
                    {
                        if let Some(right) = timeline.split_at(id, press_time) {
                            *selected = Some(right);
                            if self.magnet {
                                timeline.close_gaps(MAIN_TRACK);
                            }
                        } else {
                            *selected = Some(id);
                        }
                    }
                }
            } else if let Some(track) = self.track_at(rect, press_pos.y) {
                // Hit-test in pixels so edges can be grabbed from just
                // outside the clip, not only from inside it.
                let hit = timeline.clips.iter().find_map(|c| {
                    if c.track != track {
                        return None;
                    }
                    let left_x = self.x_at(content_left, c.start);
                    let right_x = self.x_at(content_left, c.end());
                    (press_pos.x >= left_x - EDGE_PX && press_pos.x <= right_x + EDGE_PX)
                        .then_some((c.id, c.start, c.end(), left_x, right_x))
                });
                match hit {
                    Some((id, start, end, left_x, right_x)) => {
                        *selected = Some(id);
                        let mode = if press_pos.x <= left_x + EDGE_PX {
                            DragMode::TrimLeft {
                                orig_start: start,
                                press_time,
                            }
                        } else if press_pos.x >= right_x - EDGE_PX {
                            DragMode::TrimRight {
                                orig_end: end,
                                press_time,
                            }
                        } else {
                            DragMode::Move {
                                grab_offset: press_time - start,
                                orig_start: start,
                            }
                        };
                        self.drag = Some((id, mode));
                    }
                    None if response.clicked() => *selected = None,
                    None => {}
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
                    (
                        DragMode::Move {
                            grab_offset,
                            orig_start,
                        },
                        Some(clip),
                    ) => {
                        let raw = time - grab_offset;
                        let by_start = snap_to(raw, &candidates, threshold);
                        let by_end =
                            snap_to(raw + clip.duration, &candidates, threshold) - clip.duration;
                        let snapped = if (by_start - raw).abs() <= (by_end - raw).abs() {
                            by_start
                        } else {
                            by_end
                        };
                        let prev_end = Self::prev_end(timeline, &clip, orig_start);
                        let next_start =
                            Self::next_start(timeline, &clip, orig_start + clip.duration);
                        let mut clamped = snapped.max(prev_end);
                        if let Some(next) = next_start {
                            clamped = clamped.min((next - clip.duration).max(prev_end));
                        }
                        timeline.move_clip(id, clamped);
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
            if self.magnet {
                if let Some((id, mode)) = self.drag {
                    // A left trim deliberately opens a gap, and the engine
                    // contract keeps the clip's end fixed; rippling the
                    // track closed here would shift that end left by the
                    // trimmed amount. Other drags keep the magnetic close.
                    let keep_gap = matches!(mode, DragMode::TrimLeft { .. });
                    if !keep_gap && timeline.clip(id).map(|c| c.track) == Some(MAIN_TRACK) {
                        timeline.close_gaps(MAIN_TRACK);
                    }
                }
            }
            self.drag = None;
            self.seeking = false;
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
            if self.tool == TimelineTool::Split {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if let Some(clip) = timeline.clip(id) {
                    let y = lanes_top + clip.track as f32 * (TRACK_H + TRACK_GAP);
                    clipped.line_segment(
                        [Pos2::new(pos.x, y), Pos2::new(pos.x, y + TRACK_H)],
                        Stroke::new(1.5_f32, theme::FOREGROUND),
                    );
                }
            } else {
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
        let mut x = rect.left();
        while x < rect.right() {
            let visible_w = tile_w.min(rect.right() - x);
            let tile = Rect::from_min_size(
                Pos2::new(x, rect.top()),
                Vec2::new(visible_w, rect.height()),
            );
            let center_source =
                clip.source_in + ((x + tile_w / 2.0 - rect.left()) / self.px_per_sec) as f64;
            let idx = visual
                .thumbs
                .partition_point(|(t, _)| *t <= center_source)
                .saturating_sub(1);
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(visible_w / tile_w, 1.0));
            painter.image(visual.thumbs[idx].1.id(), tile, uv, tint);
            x += tile_w;
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

    #[test]
    fn split_tool_cuts_clip_at_clicked_time() {
        let ctx = egui::Context::default();
        let (mut timeline, clip_id) = clip_fixture();
        let visuals = full_visuals();
        let mut view = TimelineView::default();
        view.tool = TimelineTool::Split;
        let mut playhead = 0.0;
        let mut selected = None;
        let y = 99.0;
        // Click at t=5s: x = 144 + 5*48 = 384.
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
        assert_eq!(timeline.clips.len(), 2);
        let left = timeline.clip(clip_id).unwrap();
        assert!((left.end() - 5.0).abs() < 0.02, "left end {}", left.end());
        let right_id = selected.expect("right half selected");
        let right = timeline.clip(right_id).unwrap();
        assert!((right.start - 5.0).abs() < 0.02);
        assert!((right.source_in - 6.0).abs() < 0.02);
    }
}

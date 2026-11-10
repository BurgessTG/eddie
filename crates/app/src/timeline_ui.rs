//! Custom-painted timeline: ruler, lanes, clips, playhead.
//!
//! Geometry, hit-testing and what each gesture means live in
//! `eddie_core::editor::lanes`; this file reads egui's pointer and paints.

use std::collections::HashMap;

use eddie_core::editor::lanes::{
    self, filmstrip_tiles, ruler_label, ruler_step, ClipHit, Gesture, Scene, ViewRect, EDGE_PX,
    HEADER_W, RULER_H, TRACK_GAP, TRACK_H,
};
use eddie_core::editor::viewport::TimelineViewport;
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Ui, Vec2,
};
use openreel_engine::{format_timecode, Clip, ClipId, ClipKind, Timeline};

use crate::theme;

/// Per-source visual data supplied by the app for one frame of painting.
pub struct ClipVisual<'a> {
    pub duration: f64,
    pub peaks: &'a [f32],
    pub thumbs: &'a [(f64, TextureHandle)],
}

pub type VisualMap<'a> = HashMap<&'a str, ClipVisual<'a>>;

fn view_rect(rect: Rect) -> ViewRect {
    ViewRect {
        left: rect.left(),
        top: rect.top(),
        right: rect.right(),
        bottom: rect.bottom(),
    }
}

fn egui_rect(rect: ViewRect) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect.left, rect.top),
        Pos2::new(rect.right, rect.bottom),
    )
}

#[derive(Default)]
pub struct TimelineView {
    /// The editor's viewport as drawn: the app copies it in before `show`
    /// and back out after, so drops from the bin hit-test against the
    /// frame on screen.
    pub port: TimelineViewport,
    gesture: Gesture,
    press_origin: Option<Pos2>,
    last_drag_pos: Option<Pos2>,
    last_rect: Option<Rect>,
}

impl TimelineView {
    fn x_at(&self, content_left: f32, time: f64) -> f32 {
        lanes::x_at(&self.port, content_left, time)
    }

    fn time_at(&self, content_left: f32, x: f32) -> f64 {
        lanes::time_at(&self.port, content_left, x)
    }

    /// True while the user is scrubbing the ruler or dragging a clip.
    pub fn is_interacting(&self) -> bool {
        self.gesture.is_interacting()
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

        self.port.width = Some(content.width());
        lanes::clamp_scroll(&mut self.port, timeline.end(), content.width());
        self.handle_scroll(ui, &response, content_left);
        self.handle_pointer(&response, timeline, playhead, selected, visuals, rect);
        self.port.interacting = self.is_interacting();
        self.paint(ui, timeline, *playhead, *selected, visuals, rect, content);
        response.on_hover_text(
            "Click to move the playhead. Drag clips to move, drag edges to trim. Ctrl+scroll zooms. F1 lists shortcuts.",
        );
    }

    /// Map a screen position to `(track, time)` for drag-and-drop from the
    /// media bin. Uses the most recent frame's geometry.
    pub fn drop_target(&self, pos: Pos2) -> Option<(usize, f64)> {
        lanes::drop_target(&self.port, view_rect(self.last_rect?), [pos.x, pos.y])
    }

    /// Content rect of a lane from the most recent frame's geometry,
    /// for highlighting a pending drop lane. Works one past the last
    /// lane too (the append zone).
    pub fn lane_rect(&self, track: usize) -> Option<Rect> {
        Some(egui_rect(lanes::lane_rect(
            view_rect(self.last_rect?),
            track,
        )))
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
            lanes::zoom_about(
                &mut self.port,
                content_left,
                pointer_x,
                (delta.y * 0.002).exp(),
            );
        } else {
            lanes::scroll_by(&mut self.port, delta.x, delta.y);
        }
    }

    fn clip_rect(&self, rect: Rect, clip: &Clip) -> Rect {
        egui_rect(lanes::clip_rect(&self.port, view_rect(rect), clip))
    }

    fn hit_clip<'a>(
        &self,
        timeline: &'a Timeline,
        rect: Rect,
        pos: Pos2,
    ) -> Option<(&'a Clip, ClipHit)> {
        lanes::hit_clip(&self.port, timeline, view_rect(rect), [pos.x, pos.y])
    }

    fn handle_pointer(
        &mut self,
        response: &egui::Response,
        timeline: &mut Timeline,
        playhead: &mut f64,
        selected: &mut Option<ClipId>,
        visuals: &VisualMap<'_>,
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
            && !self.gesture.is_interacting();
        let drag_started = response.drag_started() || release_drag;
        let drag_stopped = response.drag_stopped() || release_drag;
        let view = view_rect(rect);
        let point = |pos: Pos2| [pos.x, pos.y];
        let Some(pos) = response
            .interact_pointer_pos()
            .or_else(|| release_drag.then_some(pointer_pos).flatten())
        else {
            if drag_stopped {
                let last = self.last_drag_pos.map(point);
                self.finish_drag(timeline, playhead, selected, view, last);
            }
            return;
        };
        if self.gesture.is_interacting() {
            self.last_drag_pos = Some(pos);
        }
        // Classify from where the press began, not where the pointer is
        // when egui crosses its drag threshold (~6px later).
        let press_pos = saved_press.or(origin).unwrap_or(pos);
        // While an interaction is live, always keep tracking.
        if !Gesture::accepts(view, point(press_pos)) && !self.gesture.is_interacting() {
            return;
        }

        if !drag_stopped && self.gesture.autoscroll(&mut self.port, view, point(pos)) {
            response.ctx.request_repaint();
        }

        let mut scene = Scene {
            timeline,
            playhead,
            selected,
            port: &mut self.port,
            rect: view,
        };
        // Clicks arrive on release without a later drag_stopped event.
        // Only real drags may retain interaction state.
        if drag_started {
            self.gesture.begin(&mut scene, point(press_pos), point(pos));
        }
        if response.clicked() {
            self.gesture.click(&mut scene, point(press_pos), point(pos));
        }

        // egui does not report `dragged` on the release frame, which can
        // carry a new pointer position (including a different target lane).
        if response.dragged() || drag_stopped {
            self.last_drag_pos = Some(pos);
            self.gesture.drag(&mut scene, point(pos), &|source| {
                visuals.get(source).map(|visual| visual.duration)
            });
        }

        if drag_stopped {
            self.finish_drag(timeline, playhead, selected, view, Some(point(pos)));
        }
    }

    fn finish_drag(
        &mut self,
        timeline: &mut Timeline,
        playhead: &mut f64,
        selected: &mut Option<ClipId>,
        rect: ViewRect,
        drop_pos: Option<lanes::Point>,
    ) {
        let mut scene = Scene {
            timeline,
            playhead,
            selected,
            port: &mut self.port,
            rect,
        };
        self.gesture.finish(&mut scene, drop_pos);
        self.press_origin = None;
        self.last_drag_pos = None;
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
            // Lanes are kind-agnostic; the header shows the kind that
            // covers the most time there, and stays neutral when empty.
            let mut cover = [0.0_f64; 3];
            for clip in timeline.clips.iter().filter(|c| c.track == track) {
                cover[kind_index(clip.kind)] += clip.duration;
            }
            let kind = (cover.iter().any(|c| *c > 0.0)).then(|| {
                [ClipKind::Video, ClipKind::Audio, ClipKind::Title]
                    .into_iter()
                    .max_by(|a, b| cover[kind_index(*a)].total_cmp(&cover[kind_index(*b)]))
                    .unwrap_or(ClipKind::Video)
            });
            let accent = kind.map_or(theme::BORDER_STRONG, |k| kind_color(k).gamma_multiply(1.8));
            painter.rect_filled(
                Rect::from_min_size(
                    header.left_top() + Vec2::new(0.0, 10.0),
                    Vec2::new(3.0, TRACK_H - 20.0),
                ),
                2,
                accent,
            );
            let icon_center = Pos2::new(header.left() + 22.0, header.center().y);
            paint_kind_glyph(
                &painter,
                icon_center,
                kind,
                if kind.is_some() {
                    theme::MUTED_FG
                } else {
                    theme::SUBTLE_FG
                },
            );
            painter.text(
                Pos2::new(header.left() + 38.0, header.center().y),
                Align2::LEFT_CENTER,
                &timeline.track_labels[track],
                FontId::proportional(12.0),
                if kind.is_some() {
                    theme::FOREGROUND
                } else {
                    theme::SUBTLE_FG
                },
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
            let base = kind_color(clip.kind);
            let is_selected = selected == Some(clip.id);
            let is_hovered = hovered_clip == Some(clip.id);
            let fill = if is_hovered && !is_selected {
                base.gamma_multiply(1.3)
            } else {
                base
            };
            clipped.rect_filled(clip_rect, theme::R_SM, fill);

            let visual = clip
                .source
                .as_ref()
                .and_then(|src| visuals.get(src.as_str()));
            match (clip.kind, visual) {
                (ClipKind::Audio, Some(visual)) => {
                    self.paint_waveform(&clipped, clip_rect, clip, visual)
                }
                (ClipKind::Video, Some(visual)) => {
                    self.paint_filmstrip(&clipped, clip_rect, clip, visual)
                }
                _ => {}
            }
            // A lighter top edge gives the body depth without an outline.
            clipped.line_segment(
                [
                    clip_rect.left_top() + Vec2::new(4.0, 0.5),
                    clip_rect.right_top() + Vec2::new(-4.0, 0.5),
                ],
                Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
            );

            if clip_rect.width() > 40.0 {
                let link_w = if clip.linked_clip.is_some() {
                    16.0
                } else {
                    0.0
                };
                let font = FontId::proportional(11.0);
                let max_w = clip_rect.width() - 14.0 - link_w;
                let galley = ui.fonts(|f| {
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        clip.name.clone(),
                        font.clone(),
                        Color32::from_white_alpha(235),
                    );
                    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(8.0));
                    f.layout_job(job)
                });
                let chip = Rect::from_min_size(
                    clip_rect.left_top() + Vec2::new(3.0, 3.0),
                    Vec2::new(galley.size().x + 10.0 + link_w, 16.0),
                );
                // Media carries pictures/peaks under the name, so it gets
                // a dark chip; flat title bodies read without one.
                if clip.kind != ClipKind::Title {
                    clipped.rect_filled(chip, 4, Color32::from_black_alpha(140));
                }
                clipped.galley(
                    chip.left_center() + Vec2::new(5.0, -galley.size().y / 2.0),
                    galley,
                    Color32::from_white_alpha(235),
                );
                if link_w > 0.0 {
                    paint_link_glyph(
                        &clipped,
                        Pos2::new(chip.right() - 10.0, chip.center().y),
                        Color32::from_white_alpha(200),
                    );
                }
            }

            if is_selected {
                clipped.rect_stroke(
                    clip_rect,
                    theme::R_SM,
                    Stroke::new(2.0_f32, theme::BRAND),
                    egui::StrokeKind::Inside,
                );
            } else if is_hovered {
                clipped.rect_stroke(
                    clip_rect,
                    theme::R_SM,
                    Stroke::new(1.0_f32, Color32::from_white_alpha(90)),
                    egui::StrokeKind::Inside,
                );
            }
            // Trim affordance: a grip on the hovered edge.
            if let Some((hover, hit @ (ClipHit::Left | ClipHit::Right))) = hovered {
                if hover.id == clip.id {
                    let x = match hit {
                        ClipHit::Left => clip_rect.left() + 3.5,
                        _ => clip_rect.right() - 3.5,
                    };
                    clipped.line_segment(
                        [
                            Pos2::new(x, clip_rect.center().y - 8.0),
                            Pos2::new(x, clip_rect.center().y + 8.0),
                        ],
                        Stroke::new(2.5_f32, theme::FOREGROUND),
                    );
                }
            }
        }

        // Live readout while moving/trimming a clip.
        if let Some(id) = self.gesture.dragged_clip() {
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
            // Pentagon head pointing at the lanes.
            let top = rect.top() + RULER_H - 15.0;
            clipped.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(px - 5.5, top),
                    Pos2::new(px + 5.5, top),
                    Pos2::new(px + 5.5, top + 8.0),
                    Pos2::new(px, top + 14.0),
                    Pos2::new(px - 5.5, top + 8.0),
                ],
                theme::PLAYHEAD,
                Stroke::NONE,
            ));
        }
    }

    fn paint_ruler(&self, painter: &egui::Painter, rect: Rect, content_left: f32) {
        let step = ruler_step(self.port.px_per_sec);
        let first = (self.time_at(content_left, content_left) / step).floor() * step;
        let minor = step / 4.0;
        let bottom = rect.top() + RULER_H;
        let mut i = 0;
        loop {
            let t = first + i as f64 * minor;
            let x = self.x_at(content_left, t);
            if x > rect.right() {
                break;
            }
            let major = i % 4 == 0;
            painter.line_segment(
                [
                    Pos2::new(x, bottom - if major { 9.0 } else { 4.0 }),
                    Pos2::new(x, bottom),
                ],
                Stroke::new(
                    1.0_f32,
                    if major {
                        theme::BORDER_STRONG
                    } else {
                        theme::BORDER
                    },
                ),
            );
            if major && t >= 0.0 {
                painter.text(
                    Pos2::new(x + 6.0, rect.top() + 3.0),
                    Align2::LEFT_TOP,
                    ruler_label(t, step),
                    FontId::proportional(10.5),
                    theme::SUBTLE_FG,
                );
            }
            i += 1;
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
        // Below the name chip, so the label never sits on the peaks.
        let mid = rect.center().y + 5.0;
        let max_amp = rect.height() / 2.0 - 8.0;
        let color = theme::CLIP_AUDIO.gamma_multiply(2.6);
        let mut x = rect.left() + 2.0;
        while x < rect.right() - 2.0 {
            let source_time =
                clip.source_in + (x - rect.left()) as f64 / self.port.px_per_sec as f64;
            let idx = (source_time * peaks_per_sec) as usize;
            if let Some(peak) = visual.peaks.get(idx) {
                // Square-root scale so quiet dialogue stays visible
                // beside loud music, as in common editor waveforms.
                let amp = (peak.clamp(0.0, 1.0).sqrt() * max_amp).max(1.0);
                painter.line_segment(
                    [Pos2::new(x, mid - amp), Pos2::new(x, mid + amp)],
                    Stroke::new(2.0_f32, color),
                );
            }
            x += 3.0;
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
        for tile in filmstrip_tiles(clip.source_in, clip.duration, self.port.px_per_sec, tile_w) {
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

fn kind_index(kind: ClipKind) -> usize {
    match kind {
        ClipKind::Video => 0,
        ClipKind::Audio => 1,
        ClipKind::Title => 2,
    }
}

fn kind_color(kind: ClipKind) -> Color32 {
    match kind {
        ClipKind::Video => theme::CLIP_VIDEO,
        ClipKind::Audio => theme::CLIP_AUDIO,
        ClipKind::Title => theme::CLIP_TITLE,
    }
}

/// Small header glyph: film frame, level bars, a T, or an empty dot.
fn paint_kind_glyph(painter: &egui::Painter, c: Pos2, kind: Option<ClipKind>, fg: Color32) {
    let line = Stroke::new(1.4_f32, fg);
    match kind {
        Some(ClipKind::Video) => {
            let body = Rect::from_center_size(c, Vec2::new(13.0, 10.0));
            painter.rect_stroke(body, 2, line, egui::StrokeKind::Inside);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + Vec2::new(-1.5, -2.5),
                    c + Vec2::new(2.5, 0.0),
                    c + Vec2::new(-1.5, 2.5),
                ],
                fg,
                Stroke::NONE,
            ));
        }
        Some(ClipKind::Audio) => {
            for (i, h) in [4.0_f32, 9.0, 12.0, 7.0, 3.0].into_iter().enumerate() {
                let x = c.x - 6.0 + i as f32 * 3.0;
                painter.line_segment(
                    [Pos2::new(x, c.y - h / 2.0), Pos2::new(x, c.y + h / 2.0)],
                    line,
                );
            }
        }
        Some(ClipKind::Title) => {
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "T",
                FontId::proportional(13.0),
                fg,
            );
        }
        None => {
            painter.circle_stroke(c, 3.5, line);
        }
    }
}

/// Two interlocked rings marking a linked picture/sound pair.
fn paint_link_glyph(painter: &egui::Painter, c: Pos2, fg: Color32) {
    let stroke = Stroke::new(1.3_f32, fg);
    for dx in [-2.5_f32, 2.5] {
        painter.rect_stroke(
            Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(7.0, 5.0)),
            2,
            stroke,
            egui::StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                port: TimelineViewport {
                    magnet,
                    ..Default::default()
                },
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
                port: TimelineViewport {
                    magnet: false,
                    ..Default::default()
                },
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
            port: TimelineViewport {
                magnet: false,
                ..Default::default()
            },
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
                port: TimelineViewport {
                    px_per_sec: 300.0,
                    magnet: false,
                    ..Default::default()
                },
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
                port: TimelineViewport {
                    magnet: false,
                    ..Default::default()
                },
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
                port: TimelineViewport {
                    px_per_sec: 240.0,
                    magnet: false,
                    ..Default::default()
                },
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
            port: TimelineViewport {
                px_per_sec: 120.0,
                magnet: false,
                ..Default::default()
            },
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
                port: TimelineViewport {
                    px_per_sec: 120.0,
                    magnet: false,
                    ..Default::default()
                },
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
            port: TimelineViewport {
                magnet: false,
                ..Default::default()
            },
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
            port: TimelineViewport {
                magnet: false,
                ..Default::default()
            },
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
            port: TimelineViewport {
                px_per_sec: 240.0,
                magnet: false,
                ..Default::default()
            },
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
            port: TimelineViewport {
                magnet: false,
                ..Default::default()
            },
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
                        port: TimelineViewport {
                            magnet,
                            ..Default::default()
                        },
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
                    port: TimelineViewport {
                        magnet,
                        ..Default::default()
                    },
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
                port: TimelineViewport {
                    magnet: false,
                    ..Default::default()
                },
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
        view.port.magnet = false; // isolate hit-testing from ripple-on-release
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
        view.port.magnet = false; // isolate hit-testing from ripple-on-release
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

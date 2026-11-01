//! Program monitor: displays the real decoded frame texture for the video
//! clip under the playhead, a waveform view for audio-only moments, and
//! title overlays composited like the export will render them.

use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, TextureHandle, Ui, Vec2,
};
use openreel_engine::{ClipKind, Timeline};

use crate::theme;

/// Fit a rect of `aspect` (w/h) inside `area`, centered, with margin.
fn fit(area: Rect, aspect: f32) -> Rect {
    let size = area.size() - Vec2::splat(24.0);
    let w = size.x.min(size.y * aspect).max(0.0);
    Rect::from_center_size(area.center(), Vec2::new(w, w / aspect))
}

pub fn paint(
    ui: &mut Ui,
    area: Rect,
    timeline: &Timeline,
    playhead: f64,
    texture: Option<&TextureHandle>,
    audio_peaks: Option<(&[f32], f64)>,
    canvas: (u32, u32),
    has_media: bool,
) {
    let video = timeline.active_video(playhead);
    let audio = timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Audio && c.contains(playhead))
        .min_by_key(|c| c.track);

    // The export canvas, so pillarboxing and titles preview as exported.
    let frame = fit(area, canvas.0 as f32 / canvas.1.max(1) as f32);
    let painter = ui.painter().with_clip_rect(frame);
    painter.rect_filled(frame, CornerRadius::same(theme::R_LG), Color32::BLACK);

    match (video, texture) {
        (Some(clip), Some(texture)) => {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            let tint = Color32::from_white_alpha((clip.opacity.clamp(0.0, 1.0) * 255.0) as u8);
            let size = texture.size_vec2();
            let scale = (frame.width() / size.x).min(frame.height() / size.y);
            let picture = Rect::from_center_size(frame.center(), size * scale);
            // A picture that fills the canvas takes its rounded corners.
            let fills = (picture.width() - frame.width()).abs() < 1.5
                && (picture.height() - frame.height()).abs() < 1.5;
            let radius = if fills { theme::R_LG } else { 0 };
            painter.add(
                eframe::egui::epaint::RectShape::filled(picture, radius, tint)
                    .with_texture(texture.id(), uv),
            );
        }
        (Some(_), None) => {
            painter.text(
                frame.center(),
                Align2::CENTER_CENTER,
                "Loading frame…",
                FontId::proportional(14.0),
                theme::MUTED_FG,
            );
        }
        (None, _) => {
            if let (Some(clip), Some((peaks, duration))) = (audio, audio_peaks) {
                paint_audio_waveform(&painter, frame, clip, peaks, duration, playhead);
            } else {
                let message = if timeline.clips.is_empty() && has_media {
                    "Drag media onto the timeline"
                } else if timeline.clips.is_empty() {
                    "Import media to start"
                } else {
                    "No clip at playhead"
                };
                painter.text(
                    frame.center(),
                    Align2::CENTER_CENTER,
                    message,
                    FontId::proportional(14.0),
                    theme::MUTED_FG,
                );
            }
        }
    }

    // Title/caption overlay from export's own table, scaled from canvas
    // pixels to the monitor so it previews at its exported size and place.
    if let Some(title) = timeline.active_title(playhead) {
        let alpha = title.opacity.clamp(0.0, 1.0);
        let look = openreel_media::title_look(title.title_style);
        let scale = frame.height() / canvas.1.max(1) as f32;
        let (r, g, b) = look.color;
        let color = Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.0) as u8);
        let galley = painter.layout_no_wrap(
            title.name.clone(),
            FontId::proportional((look.font_size as f32 * scale).max(1.0)),
            color,
        );
        let size = galley.size();
        let x = match look.left {
            Some(left) => frame.left() + left as f32 * scale,
            None => frame.center().x - size.x / 2.0,
        };
        let top_left = Pos2::new(x, frame.bottom() - look.bottom as f32 * scale);
        let border = look.border as f32 * scale;
        match look.box_alpha {
            Some(box_alpha) => {
                painter.rect_filled(
                    Rect::from_min_size(top_left, size).expand(border),
                    0,
                    Color32::from_black_alpha((box_alpha * alpha * 255.0) as u8),
                );
            }
            None => {
                let outline = Color32::from_black_alpha((0.8 * alpha * 255.0) as u8);
                for offset in [
                    Vec2::new(-border, 0.0),
                    Vec2::new(border, 0.0),
                    Vec2::new(0.0, -border),
                    Vec2::new(0.0, border),
                ] {
                    painter.galley_with_override_text_color(
                        top_left + offset,
                        galley.clone(),
                        outline,
                    );
                }
            }
        }
        painter.galley(top_left, galley, color);
    }
}

fn paint_audio_waveform(
    painter: &eframe::egui::Painter,
    frame: Rect,
    clip: &openreel_engine::Clip,
    peaks: &[f32],
    duration: f64,
    playhead: f64,
) {
    if peaks.is_empty() || duration <= 0.0 || clip.duration <= 0.0 {
        return;
    }
    let peaks_per_sec = peaks.len() as f64 / duration;
    let mid = frame.center().y;
    let max_amp = frame.height() * 0.32;
    let inner = frame.shrink2(Vec2::new(24.0, 30.0));
    let mut x = inner.left();
    while x < inner.right() {
        let frac = (x - inner.left()) / inner.width();
        let source_time = clip.source_in + frac as f64 * clip.duration;
        if let Some(peak) = peaks.get((source_time * peaks_per_sec) as usize) {
            let amp = (peak * max_amp).max(1.0);
            painter.line_segment(
                [Pos2::new(x, mid - amp), Pos2::new(x, mid + amp)],
                Stroke::new(2.0_f32, Color32::from_rgb(0x4C, 0x9A, 0x68)),
            );
        }
        x += 4.0;
    }
    let progress = ((playhead - clip.start) / clip.duration).clamp(0.0, 1.0) as f32;
    let px = inner.left() + inner.width() * progress;
    painter.line_segment(
        [Pos2::new(px, inner.top()), Pos2::new(px, inner.bottom())],
        Stroke::new(1.5_f32, theme::PLAYHEAD),
    );
}

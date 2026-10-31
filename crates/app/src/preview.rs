//! Program monitor: displays the real decoded frame texture for the video
//! clip under the playhead, a waveform view for audio-only moments, and
//! title overlays composited like the export will render them.

use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, TextureHandle, Ui, Vec2,
};
use openreel_engine::{format_timecode, ClipKind, Timeline, TitleStyle};

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
) {
    let video = timeline.active_video(playhead);
    let audio = timeline
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Audio && c.contains(playhead))
        .min_by_key(|c| c.track);

    let aspect = texture
        .map(|t| {
            let s = t.size_vec2();
            if s.y > 0.0 {
                s.x / s.y
            } else {
                16.0 / 9.0
            }
        })
        .unwrap_or(16.0 / 9.0);
    let frame = fit(area, aspect);
    let painter = ui.painter().with_clip_rect(frame);
    painter.rect(
        frame,
        CornerRadius::same(theme::R_LG),
        Color32::BLACK,
        Stroke::new(1.0_f32, theme::BORDER_STRONG),
        eframe::egui::StrokeKind::Outside,
    );

    match (video, texture) {
        (Some(clip), Some(texture)) => {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            let tint = Color32::from_white_alpha((clip.opacity.clamp(0.0, 1.0) * 255.0) as u8);
            painter.image(texture.id(), frame, uv, tint);
            let local = clip.source_in + (playhead - clip.start);
            painter.text(
                frame.left_top() + Vec2::new(12.0, 8.0),
                Align2::LEFT_TOP,
                format!(
                    "{}  ·  src {}",
                    clip.name,
                    format_timecode(local, timeline.fps)
                ),
                FontId::monospace(11.0),
                Color32::from_white_alpha(170),
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
                painter.text(
                    frame.left_top() + Vec2::new(12.0, 8.0),
                    Align2::LEFT_TOP,
                    format!("{}  ·  audio only", clip.name),
                    FontId::monospace(11.0),
                    Color32::from_white_alpha(170),
                );
            } else {
                let message = if timeline.clips.is_empty() {
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

    // Title/caption overlay, positioned like export for its style.
    if let Some(title) = timeline.active_title(playhead) {
        let alpha = (title.opacity.clamp(0.0, 1.0) * 255.0) as u8;
        match title.title_style {
            TitleStyle::LowerThird => {
                let card = Rect::from_min_size(
                    frame.left_bottom() + Vec2::new(28.0, -72.0),
                    Vec2::new((frame.width() - 56.0).clamp(120.0, 420.0), 48.0),
                );
                painter.rect_filled(
                    card,
                    6,
                    Color32::from_rgba_unmultiplied(0, 0, 0, (alpha as f32 * 0.55) as u8),
                );
                painter.text(
                    card.left_center() + Vec2::new(14.0, 0.0),
                    Align2::LEFT_CENTER,
                    &title.name,
                    FontId::proportional(17.0),
                    Color32::from_white_alpha(alpha),
                );
            }
            TitleStyle::Caption | TitleStyle::PopCaption => {
                let pop = title.title_style == TitleStyle::PopCaption;
                let font_size = if pop { 21.0 } else { 17.0 };
                let estimated_width = title.name.chars().count() as f32 * font_size * 0.56 + 28.0;
                let width = estimated_width.clamp(140.0, (frame.width() - 40.0).max(140.0));
                let card = Rect::from_center_size(
                    Pos2::new(frame.center().x, frame.bottom() - 38.0),
                    Vec2::new(width, 42.0),
                );
                painter.rect_filled(
                    card,
                    6,
                    Color32::from_rgba_unmultiplied(
                        0,
                        0,
                        0,
                        (alpha as f32 * if pop { 0.75 } else { 0.65 }) as u8,
                    ),
                );
                let color = if pop {
                    Color32::from_rgba_unmultiplied(255, 214, 64, alpha)
                } else {
                    Color32::from_white_alpha(alpha)
                };
                painter.text(
                    card.center(),
                    Align2::CENTER_CENTER,
                    &title.name,
                    FontId::proportional(font_size),
                    color,
                );
            }
            TitleStyle::MinimalCaption => {
                let center = Pos2::new(frame.center().x, frame.bottom() - 34.0);
                painter.text(
                    center + Vec2::new(1.0, 1.0),
                    Align2::CENTER_CENTER,
                    &title.name,
                    FontId::proportional(16.0),
                    Color32::from_rgba_unmultiplied(0, 0, 0, alpha),
                );
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    &title.name,
                    FontId::proportional(16.0),
                    Color32::from_white_alpha(alpha),
                );
            }
        }
    }

    painter.text(
        frame.right_bottom() + Vec2::new(-12.0, -10.0),
        Align2::RIGHT_BOTTOM,
        format_timecode(playhead, timeline.fps),
        FontId::monospace(11.0),
        Color32::from_white_alpha(130),
    );
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

//! Voiceover takes on the lanes: the one recording, growing from where it
//! started, and finished ones until their import puts the clip there. Each
//! is drawn on the audio lane its clip will land on, which may be a new
//! lane past the last.

use eddie_core::Editor;
use eddie_core::editor::lanes::{self, TRACK_H, ViewRect};
use openreel_engine::ClipKind;
use openreel_media::WAVEFORM_PEAKS_PER_SEC;
use rune::{El, LineCap, Shape, el, row, text};

use super::{black, white};
use crate::theme;

/// A recording take's body: the destructive red, dimmed as clip bodies are.
const RECORDING: &str = "#5B2328";

/// Where each take shows: its start, length and the lane it lands on.
fn takes(editor: &Editor) -> Vec<(f64, f64, usize)> {
    let timeline = &editor.timeline;
    let lane = |start: f64, seconds: f64| {
        timeline
            .first_free_lane(ClipKind::Audio, start, start + seconds)
            .unwrap_or(timeline.lanes.audio)
    };
    let recording = editor
        .voiceover()
        .and_then(|voiceover| voiceover.recording())
        .map(|(start, take)| (start, take.seconds()));
    editor
        .pending_takes()
        .iter()
        .map(|take| (take.start, take.seconds - take.source_in))
        .chain(recording)
        .map(|(start, seconds)| (start, seconds, lane(start, seconds)))
        .collect()
}

/// A take needs an audio lane past the last one.
pub(super) fn extra_lane(editor: &Editor) -> bool {
    let audio = editor.timeline.lanes.audio;
    takes(editor).iter().any(|&(.., lane)| lane == audio)
}

/// Every take's element, in the lanes' points.
pub(super) fn elements(editor: &Editor, view: ViewRect, lanes_w: f32) -> Vec<El> {
    let timeline = &editor.timeline;
    let row = |lane| timeline.row(ClipKind::Audio, 0) + lane;
    let mut out = Vec::new();
    let pending = editor.pending_takes();
    let shown = takes(editor);
    for (take, &(start, seconds, lane)) in pending.iter().zip(&shown) {
        // A take that started before 0 lands from its source in.
        let skip = (take.source_in * f64::from(WAVEFORM_PEAKS_PER_SEC)) as usize;
        let peaks = take.peaks.get(skip..).unwrap_or(&[]);
        let label = (take.name.as_str(), false);
        out.extend(element(
            editor,
            view,
            lanes_w,
            (start, seconds, row(lane)),
            peaks,
            label,
        ));
    }
    if let (Some((_, take)), Some(&(start, seconds, lane))) = (
        editor
            .voiceover()
            .and_then(|voiceover| voiceover.recording()),
        shown.get(pending.len()),
    ) {
        let peaks = take.peaks();
        let at = (start, seconds, row(lane));
        out.extend(element(
            editor,
            view,
            lanes_w,
            at,
            &peaks,
            ("Recording", true),
        ));
    }
    out
}

/// One take from `start` for `seconds` on row `line`, its waveform drawn from
/// `peaks` (from its first sample) and named `label`; red while it records.
fn element(
    editor: &Editor,
    view: ViewRect,
    lanes_w: f32,
    (start, seconds, line): (f64, f64, usize),
    peaks: &[f32],
    (label, recording): (&str, bool),
) -> Option<El> {
    let port = &editor.viewport;
    let left = lanes::x_at(port, 0.0, start);
    let right = lanes::x_at(port, 0.0, start + seconds).max(left + 2.0);
    if right < 0.0 || left > lanes_w {
        return None;
    }
    let top = lanes::lane_rect(view, line).top + 3.0;
    let (from, to) = (left.max(-2.0), right.min(lanes_w + 2.0));
    let (w, h) = (to - from, TRACK_H - 6.0);
    let c = theme::c();
    let fill = if recording {
        theme::hex(RECORDING)
    } else {
        c.clip_audio
    };
    // Bars every 3 points, as clips draw their peaks.
    let mid = h / 2.0 + 5.0;
    let max_amp = h / 2.0 - 8.0;
    let color = if recording {
        c.destructive_fg
    } else {
        theme::hex("#72DA9C")
    };
    let mut art = Shape::new().view_box(w, h);
    let first = (from - left).max(0.0);
    let mut x = 2.0 + (first / 3.0).floor() * 3.0;
    while x < right - left - 1.0 && left + x - from <= w {
        let at = f64::from(x / port.px_per_sec) * f64::from(WAVEFORM_PEAKS_PER_SEC);
        if let Some(peak) = peaks.get(at as usize) {
            let amp = (peak.clamp(0.0, 1.0).sqrt() * max_amp).max(1.0);
            let bar = left + x - from;
            art = art.line(
                [bar, mid - amp],
                [bar, mid + amp],
                2.0,
                LineCap::Butt,
                color,
            );
        }
        x += 3.0;
    }
    let key = if recording {
        "take-recording".to_owned()
    } else {
        format!("take-{label}")
    };
    let mut body = el()
        .key(key)
        .absolute(from, top)
        .size(w, h)
        .radius(theme::R_SM)
        .fill(fill)
        .clip_rounded(theme::R_SM)
        .child(el().absolute(0.0, 0.0).size(w, h).shape(art));
    if right - left > 40.0 {
        let mut chip = row()
            .absolute(left - from + 3.0, 3.0)
            .h(16.0)
            .px(5.0)
            .gap(4.0)
            .radius(4.0)
            .center_y()
            .fill(black(140));
        if recording {
            chip = chip.child(el().size(6.0, 6.0).radius(3.0).fill(c.destructive));
        }
        body = body.child(
            chip.child(
                text(label.to_owned())
                    .font(theme::sized(11.0))
                    .color(white(235))
                    .single_line()
                    .truncate()
                    .max_w((right - left - 24.0).max(8.0)),
            ),
        );
    }
    Some(body)
}
